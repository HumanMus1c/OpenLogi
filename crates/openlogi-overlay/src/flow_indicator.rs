//! The floating Flow Indicator: a small card listing the devices the agent has
//! detected and their battery readings.
//!
//! The window is a non-activating pop-up owned by this process, so it survives
//! the settings window closing. Rows come from the same `Observer::state`
//! snapshot the GUI reads, settings from the shared `config.toml` (polled by
//! mtime), and the position from an overlay-owned file beside it.

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::SystemTime;

#[cfg(not(target_os = "windows"))]
use gpui::WindowDecorations;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, AppContext as _, Bounds, Context, FontWeight, Hsla, InteractiveElement as _, IntoElement,
    ParentElement, Point, Render, SharedString, Size, Styled, Window, WindowAppearance,
    WindowBackgroundAppearance, WindowBounds, WindowControlArea, WindowHandle, WindowKind,
    WindowOptions, div, hsla, point, px, rgb, size, svg,
};
#[cfg(target_os = "linux")]
use gpui::{Div, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent};
use openlogi_core::config::{
    AppSettings, Config, ConfigFile, FlowIndicatorDisplayMode, battery_devices_named,
};
use openlogi_core::device::{
    BatteryColor, BatteryDevice, BatteryInfo, BatteryTone, BatteryValue, DeviceInventory,
    DeviceKind, battery_devices, flow_indicator_devices,
};
use openlogi_ipc::Observation;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

/// Card width at 100% scale.
const CARD_WIDTH: f32 = 300.0;
const PADDING_X: f32 = 16.0;
const PADDING_Y: f32 = 14.0;
const BORDER: f32 = 1.0;
const RADIUS: f32 = 12.0;
/// One row: the line box, the 9 px gap, and the 6 px bar.
///
/// The line box is pinned because GPUI's default is 1.618x the font size: the
/// 16 px reading would render in a 26 px line and the card's height arithmetic
/// would come up short, clipping the last row at the window's bottom edge.
const LINE_HEIGHT: f32 = 26.0;
const LINE_GAP: f32 = 9.0;
const BAR_HEIGHT: f32 = 6.0;
const ROW_HEIGHT: f32 = LINE_HEIGHT + LINE_GAP + BAR_HEIGHT;
const ROW_GAP: f32 = 16.0;
const ICON_SIZE: f32 = 18.0;
const ICON_LANE: f32 = 22.0;
const MARGIN: f32 = 24.0;

const BOLT_ICON: &str = "action-icons/bolt.svg";
const MOUSE_ICON: &str = "action-icons/mouse.svg";
const KEYBOARD_ICON: &str = "action-icons/keyboard.svg";
const GENERIC_ICON: &str = "action-icons/circle-dot.svg";
/// WS_EX_TOPMOST's documented value; avoiding a lossy integer cast from the
/// windows-sys `u32` constant.
#[cfg(target_os = "windows")]
const TOPMOST_BIT: i32 = 0x0000_0008;

/// The settings the widget reads from `[app_settings]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Settings {
    pub(crate) enabled: bool,
    pub(crate) show_offline: bool,
    pub(crate) max_devices: usize,
    pub(crate) display_mode: FlowIndicatorDisplayMode,
}

impl Default for Settings {
    fn default() -> Self {
        Self::from(&AppSettings::default())
    }
}

impl From<&AppSettings> for Settings {
    fn from(settings: &AppSettings) -> Self {
        Self {
            enabled: settings.flow_indicator,
            show_offline: settings.flow_indicator_show_offline,
            // The core owner clamps a hand-edited value; the settings UI only
            // writes the valid range.
            max_devices: usize::from(settings.flow_indicator_row_limit()),
            display_mode: settings.flow_indicator_display_mode,
        }
    }
}

/// Read the widget settings and the device names from the shared config. A
/// missing or invalid file keeps the caller's last-good values.
pub(crate) fn load_settings() -> Option<(Settings, Config)> {
    match ConfigFile::load_or_default() {
        Ok((config, _)) => Some((Settings::from(&config.app_settings), config)),
        Err(error) => {
            warn!(%error, "Flow Indicator: keeping the last-good config");
            None
        }
    }
}

/// The config file's modification time, for the one-second live-settings poll.
pub(crate) fn config_stamp() -> Option<SystemTime> {
    let path = openlogi_core::paths::config_path().ok()?;
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
}

/// The saved indicator origin, in logical window coordinates.
#[derive(Debug, Serialize, Deserialize)]
struct SavedPosition {
    x: f32,
    y: f32,
}

fn position_path() -> Option<PathBuf> {
    openlogi_core::paths::config_dir()
        .ok()
        .map(|dir| dir.join("flow-indicator.toml"))
}

fn load_position() -> Option<Point<gpui::Pixels>> {
    let body = std::fs::read_to_string(position_path()?).ok()?;
    let saved: SavedPosition = toml::from_str(&body).ok()?;
    Some(point(px(saved.x), px(saved.y)))
}

fn save_position(origin: Point<gpui::Pixels>) {
    let Some(path) = position_path() else {
        return;
    };
    // A fresh Linux install has no config directory until the agent writes
    // its config; the overlay can run against a mock before that ever
    // happened, so the write creates its parent instead of failing silently.
    if let Some(parent) = path.parent()
        && let Err(error) = std::fs::create_dir_all(parent)
    {
        warn!(%error, "Flow Indicator: could not create its config directory");
        return;
    }
    let Ok(body) = toml::to_string(&SavedPosition {
        x: f32::from(origin.x),
        y: f32::from(origin.y),
    }) else {
        return;
    };
    let written = atomic_write_file::AtomicWriteFile::options()
        .open(&path)
        .and_then(|mut file| {
            std::io::Write::write_all(&mut file, body.as_bytes())?;
            file.commit()
        });
    if let Err(error) = written {
        warn!(%error, "Flow Indicator: could not save its position");
    }
}

fn card_size(rows: usize) -> Size<gpui::Pixels> {
    let count = u16::try_from(rows).unwrap_or(u16::MAX);
    let rows_height = f32::from(count) * ROW_HEIGHT;
    let gaps = f32::from(count.saturating_sub(1)) * ROW_GAP;
    size(
        px(CARD_WIDTH),
        px(BORDER * 2.0 + PADDING_Y * 2.0 + rows_height + gaps),
    )
}

/// The bottom-right corner of `area`, inside the screen margin.
fn bottom_right(area: Bounds<gpui::Pixels>, card: Size<gpui::Pixels>) -> Point<gpui::Pixels> {
    let desired = point(
        area.origin.x + area.size.width - card.width - px(MARGIN),
        area.origin.y + area.size.height - card.height - px(MARGIN),
    );
    clamp_origin(area, card, desired)
}

fn clamp_origin(
    area: Bounds<gpui::Pixels>,
    card: Size<gpui::Pixels>,
    origin: Point<gpui::Pixels>,
) -> Point<gpui::Pixels> {
    let max = point(
        (area.right() - card.width).max(area.origin.x),
        (area.bottom() - card.height).max(area.origin.y),
    );
    origin.clamp(&area.origin, &max)
}

fn resolve_origin(
    cx: &App,
    remembered: Option<Point<gpui::Pixels>>,
    card: Size<gpui::Pixels>,
) -> Point<gpui::Pixels> {
    let area = cx.primary_display().map(|display| display.visible_bounds());
    match (remembered.or_else(load_position), area) {
        (Some(origin), Some(area)) => clamp_origin(area, card, origin),
        (Some(origin), None) => origin,
        (None, Some(area)) => bottom_right(area, card),
        (None, None) => point(px(0.0), px(0.0)),
    }
}

/// One rendered device row.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Row {
    name: SharedString,
    value: SharedString,
    fill: Option<u8>,
    color: BatteryColor,
    charging: bool,
    online: bool,
    kind: DeviceKind,
}

impl Row {
    fn from(device: &BatteryDevice) -> Self {
        let battery = device.battery.as_ref();
        let tone = battery.map_or(BatteryTone::Normal, BatteryInfo::tone);
        let color = battery.map_or(BatteryColor::Neutral, BatteryInfo::color);
        let mut value = battery.map_or_else(|| "–".to_owned(), value_text);
        let mut fill = battery.and_then(BatteryInfo::bar_percentage);
        if tone == BatteryTone::Error {
            // The settings app names the fault instead of a number that may be
            // stale; the red tone carries the rest.
            value = rust_i18n::t!("device.battery_error").to_string();
            fill = None;
        }
        let name = device
            .name
            .clone()
            .unwrap_or_else(|| rust_i18n::t!(device.kind.translation_key()).to_string());
        Self {
            name: name.into(),
            value: value.into(),
            fill,
            color,
            charging: tone == BatteryTone::Charging,
            online: device.online,
            kind: device.kind,
        }
    }
}

fn value_text(battery: &BatteryInfo) -> String {
    match battery.value() {
        BatteryValue::Percent(percentage) => format!("{percentage}%"),
        value => value
            .translation_key()
            .map_or_else(|| "–".to_owned(), |key| rust_i18n::t!(key).to_string()),
    }
}

fn kind_icon(kind: DeviceKind) -> &'static str {
    match kind {
        DeviceKind::Mouse | DeviceKind::Trackball | DeviceKind::Touchpad | DeviceKind::Tablet => {
            MOUSE_ICON
        }
        DeviceKind::Keyboard | DeviceKind::Numpad => KEYBOARD_ICON,
        _ => GENERIC_ICON,
    }
}

struct Palette {
    card: Hsla,
    text: Hsla,
    muted: Hsla,
    track: Hsla,
    border: Hsla,
}

impl Palette {
    fn for_appearance(appearance: WindowAppearance) -> Self {
        if matches!(
            appearance,
            WindowAppearance::Dark | WindowAppearance::VibrantDark
        ) {
            Self {
                card: hsla(0.0, 0.0, 0.10, 0.94),
                text: rgb(0xF5_F5_F7).into(),
                muted: rgb(0xA1_A1_A6).into(),
                track: hsla(0.0, 0.0, 1.0, 0.12),
                border: hsla(0.0, 0.0, 1.0, 0.10),
            }
        } else {
            Self {
                card: hsla(0.0, 0.0, 1.0, 0.96),
                text: rgb(0x1D_1D_1F).into(),
                muted: rgb(0x6E_6E_73).into(),
                track: hsla(0.0, 0.0, 0.0, 0.08),
                border: hsla(0.0, 0.0, 0.0, 0.12),
            }
        }
    }
}

fn color_paint(color: BatteryColor, pal: &Palette) -> Hsla {
    match color {
        BatteryColor::Green => rgb(0x22_C5_5E).into(),
        BatteryColor::Amber => rgb(0xEA_B3_08).into(),
        BatteryColor::Red => rgb(0xEF_44_44).into(),
        BatteryColor::Neutral => pal.text,
    }
}

/// What the widget should show: disabled, empty, or a row set.
struct FlowIndicatorView {
    rows: Vec<Row>,
    /// The card's X11 window, when this session is X11.
    #[cfg(target_os = "linux")]
    x11: Option<crate::platform::x11::CardWindow>,
    /// A move this process performs itself: an override-redirect card has no
    /// window manager to move it.
    #[cfg(target_os = "linux")]
    manual_drag: Option<ManualDrag>,
    /// A position the X11 window manager has not put the card at yet.
    #[cfg(target_os = "linux")]
    pending_place: Option<PendingPlace>,
}

#[cfg(target_os = "linux")]
#[derive(Clone, Copy)]
struct ManualDrag {
    /// Where the pointer was when the card was pressed, in root coordinates.
    press_root: Point<gpui::Pixels>,
    /// Where the card was when it was pressed.
    origin: Point<gpui::Pixels>,
}

#[cfg(target_os = "linux")]
#[derive(Clone, Copy)]
struct PendingPlace {
    origin: Point<gpui::Pixels>,
    attempts: u8,
}

impl FlowIndicatorView {
    fn new(rows: Vec<Row>) -> Self {
        Self {
            rows,
            #[cfg(target_os = "linux")]
            x11: None,
            #[cfg(target_os = "linux")]
            manual_drag: None,
            #[cfg(target_os = "linux")]
            pending_place: None,
        }
    }

    fn row_view(row: &Row, pal: &Palette) -> impl IntoElement + use<> {
        let color = color_paint(row.color, pal);
        let inner_width = CARD_WIDTH - PADDING_X * 2.0;
        let fill_width = f32::from(row.fill.unwrap_or(0)) / 100.0 * inner_width;
        div()
            .flex()
            .flex_col()
            .gap(px(LINE_GAP))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0))
                    .h(px(LINE_HEIGHT))
                    .line_height(px(LINE_HEIGHT))
                    .child(
                        div().w(px(ICON_LANE)).h(px(ICON_LANE)).flex_none().child(
                            svg()
                                .path(kind_icon(row.kind))
                                .size(px(ICON_SIZE))
                                .text_color(pal.muted),
                        ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(13.0))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(pal.text)
                            .child(row.name.clone()),
                    )
                    .when(row.online && row.charging, |line| {
                        line.child(
                            svg()
                                .path(BOLT_ICON)
                                .size(px(12.0))
                                .flex_none()
                                .text_color(color),
                        )
                    })
                    .when(!row.online, |line| {
                        line.child(
                            div()
                                .text_size(px(10.0))
                                .text_color(pal.muted)
                                .child(rust_i18n::t!("device.offline").to_string()),
                        )
                    })
                    .child(
                        div()
                            .text_size(px(16.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(color)
                            .child(row.value.clone()),
                    ),
            )
            .child(
                div()
                    .h(px(BAR_HEIGHT))
                    .w_full()
                    .rounded_full()
                    .bg(pal.track)
                    .when(row.fill.is_some(), |track| {
                        track.child(div().h_full().w(px(fill_width)).rounded_full().bg(color))
                    }),
            )
    }

    fn card(&self, pal: &Palette) -> impl IntoElement + use<> {
        div()
            .w(px(CARD_WIDTH))
            .flex()
            .flex_col()
            .gap(px(ROW_GAP))
            .px(px(PADDING_X))
            .py(px(PADDING_Y))
            .rounded(px(RADIUS))
            .bg(pal.card)
            .border(px(BORDER))
            .border_color(pal.border)
            .shadow_lg()
            .window_control_area(WindowControlArea::Drag)
            .debug_selector(|| "flow-indicator-card".into())
            .children(self.rows.iter().map(|row| Self::row_view(row, pal)))
    }
}

impl Render for FlowIndicatorView {
    #[cfg_attr(
        not(target_os = "linux"),
        expect(unused_variables, reason = "the card's drag listeners are Linux-only")
    )]
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = Palette::for_appearance(window.appearance());
        let root = div().size_full().child(self.card(&pal));
        #[cfg(target_os = "linux")]
        let root = Self::with_drag(root, cx);
        root
    }
}

#[cfg(target_os = "linux")]
impl FlowIndicatorView {
    /// Linux delivers no window-control hit test, so the card's `Drag` area is
    /// inert and a move has to start from these listeners.
    fn with_drag(root: Div, cx: &mut Context<Self>) -> Div {
        root.on_mouse_down(MouseButton::Left, cx.listener(Self::begin_drag))
            .on_mouse_move(cx.listener(Self::drag_to_pointer))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::end_drag))
    }

    /// A press starts the move. Wayland hands it to the compositor; on X11
    /// this process moves the window itself, in both modes: GPUI's
    /// `start_window_move` sends `_NET_WM_MOVERESIZE` with button 0, and
    /// xfwm4 drives its move loop from that button number.
    fn begin_drag(&mut self, event: &MouseDownEvent, window: &mut Window, _cx: &mut Context<Self>) {
        self.pending_place = None;
        self.manual_drag = None;
        if self.x11.is_some() {
            let origin = window.bounds().origin;
            self.manual_drag = Some(ManualDrag {
                press_root: origin + event.position,
                origin,
            });
        } else {
            window.start_window_move();
        }
    }

    fn drag_to_pointer(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(drag) = self.manual_drag else {
            return;
        };
        if event.pressed_button != Some(MouseButton::Left) {
            self.manual_drag = None;
            return;
        }
        let Some(x11) = self.x11.as_ref() else {
            return;
        };
        let scale = window.scale_factor();
        let Some(root) = x11.pointer_root(scale) else {
            return;
        };
        let desired = point(
            drag.origin.x + root.x - drag.press_root.x,
            drag.origin.y + root.y - drag.press_root.y,
        );
        let card = window.bounds().size;
        let origin = cx.primary_display().map_or(desired, |display| {
            clamp_origin(display.visible_bounds(), card, desired)
        });
        x11.move_to(origin, scale);
    }

    fn end_drag(&mut self, _event: &MouseUpEvent, _window: &mut Window, _cx: &mut Context<Self>) {
        self.manual_drag = None;
    }

    /// Retry a placement the window manager has not made: the first request
    /// can reach it before it manages the freshly created window.
    fn settle_place(&mut self, window: &mut Window) {
        const ATTEMPTS: u8 = 3;
        let Some(pending) = self.pending_place else {
            return;
        };
        let origin = window.bounds().origin;
        let at_target = (origin.x - pending.origin.x).abs() <= px(1.0)
            && (origin.y - pending.origin.y).abs() <= px(1.0);
        if at_target || pending.attempts + 1 >= ATTEMPTS {
            self.pending_place = None;
            return;
        }
        if let Some(x11) = self.x11.as_ref() {
            x11.move_to(pending.origin, window.scale_factor());
        }
        self.pending_place = Some(PendingPlace {
            origin: pending.origin,
            attempts: pending.attempts + 1,
        });
    }

    /// Take the window's X11 handle and put the card at `origin`: a window
    /// manager places a managed window itself, so the request is retried until
    /// it sticks.
    fn place_on_x11(&mut self, window: &mut Window, origin: Point<gpui::Pixels>) {
        self.x11 = crate::platform::x11::CardWindow::new(window);
        let Some(x11) = self.x11.as_ref() else {
            return;
        };
        x11.move_to(origin, window.scale_factor());
        if !x11.is_override_redirect() {
            self.pending_place = Some(PendingPlace {
                origin,
                attempts: 0,
            });
        }
    }
}

impl FlowIndicatorView {
    /// A freshly opened card: name it, and on X11 hand the window to the seam
    /// that restores its position.
    #[cfg(target_os = "linux")]
    fn on_opened(&mut self, window: &mut Window, origin: Point<gpui::Pixels>) {
        // The task list names the window after this; without it the card
        // surfaces as "Untitled window".
        window.set_window_title("OpenLogi - Flow Indicator");
        self.place_on_x11(window, origin);
    }

    #[cfg(not(target_os = "linux"))]
    #[expect(
        clippy::unused_self,
        reason = "the Linux version hands the window to the X11 seam"
    )]
    fn on_opened(&mut self, _window: &mut Window, _origin: Point<gpui::Pixels>) {}
}

/// What the widget task receives: a new agent snapshot, new config, or the
/// once-a-second position tick.
pub(crate) enum Command {
    Observation(Box<Observation>),
    Settings(Box<(Settings, Config)>),
    Tick,
}

/// The widget's window lifecycle: open it when there is something to show,
/// update it in place for percentage changes, and reopen it when its height or
/// display mode changes.
///
/// This type is owned by one GPUI task, not an entity: opening a window inside
/// an entity update re-enters GPUI's app borrow. The task hands each command
/// through `App::update`, the same context the notice windows open from.
pub(crate) struct FlowIndicator {
    settings: Settings,
    /// The config that names the rows; `None` until the first successful read.
    config: Option<Config>,
    inventory: Vec<DeviceInventory>,
    window: Option<WindowHandle<FlowIndicatorView>>,
    window_mode: Option<FlowIndicatorDisplayMode>,
    /// Whether the open window is a Wayland layer surface, which has no
    /// position the compositor reports or restores.
    layer_shell: bool,
    last_saved_origin: Option<Point<gpui::Pixels>>,
    /// Set when the user closed the card window: observations must not bring
    /// it back until the settings change.
    dismissed: Rc<Cell<bool>>,
}

impl FlowIndicator {
    pub(crate) fn new(settings: Settings, config: Option<Config>) -> Self {
        Self {
            settings,
            config,
            inventory: Vec::new(),
            window: None,
            window_mode: None,
            layer_shell: false,
            last_saved_origin: None,
            dismissed: Rc::new(Cell::new(false)),
        }
    }

    pub(crate) fn handle(&mut self, command: Command, cx: &mut App) {
        match command {
            Command::Observation(observation) => {
                self.inventory.clone_from(&observation.snapshot.inventory);
                self.sync(cx);
            }
            Command::Settings(snapshot) => {
                let (settings, config) = *snapshot;
                if settings != self.settings {
                    // A settings change is the user asking for the card again;
                    // retire a window they closed first so it cannot pass for
                    // the live one.
                    self.refresh_dismissed(cx);
                    self.dismissed.set(false);
                }
                self.settings = settings;
                self.config = Some(config);
                self.sync(cx);
            }
            Command::Tick => {
                self.refresh_dismissed(cx);
                #[cfg(target_os = "linux")]
                self.settle_place(cx);
                self.persist_position(cx);
            }
        }
    }

    /// Persist where the user dragged the card. Called once a second with the
    /// config poll: the OS owns the drag, so there is no drag-end event to
    /// hang the write on and a per-frame write would be wasteful.
    fn persist_position(&mut self, cx: &mut App) {
        // A layer surface is placed by its anchors, not by a position.
        if self.layer_shell {
            return;
        }
        // A card the window manager has not placed yet must not overwrite the
        // position it is being placed at.
        #[cfg(target_os = "linux")]
        if self.placement_pending(cx) {
            return;
        }
        let origin = self.window.as_ref().and_then(|handle| {
            handle
                .update(cx, |_, window, _| window.bounds().origin)
                .ok()
        });
        let Some(origin) = origin else {
            return;
        };
        if self.last_saved_origin == Some(origin) {
            return;
        }
        save_position(origin);
        self.last_saved_origin = Some(origin);
    }

    /// Whether the window manager has yet to put the card where it was asked.
    #[cfg(target_os = "linux")]
    fn placement_pending(&self, cx: &mut App) -> bool {
        self.window
            .as_ref()
            .and_then(|handle| {
                handle
                    .update(cx, |view, _, _| view.pending_place.is_some())
                    .ok()
            })
            .unwrap_or(false)
    }

    #[cfg(target_os = "linux")]
    fn settle_place(&mut self, cx: &mut App) {
        let Some(handle) = self.window.as_ref() else {
            return;
        };
        let _ = handle.update(cx, |view, window, _| view.settle_place(window));
    }

    fn rows(&self) -> Vec<Row> {
        if !self.settings.enabled {
            return Vec::new();
        }
        let devices = self.config.as_ref().map_or_else(
            || battery_devices(&self.inventory),
            |config| battery_devices_named(&self.inventory, config),
        );
        flow_indicator_devices(
            &devices,
            self.settings.show_offline,
            self.settings.max_devices,
        )
        .into_iter()
        .map(Row::from)
        .collect()
    }

    /// Retire a card the user closed from its task-list button.
    ///
    /// X11 destroys the window then, but GPUI keeps the removed window (and
    /// this view) in its map — the close callback never lands — so the only
    /// evidence is the window no longer answering on the X11 connection.
    #[cfg(target_os = "linux")]
    fn refresh_dismissed(&mut self, cx: &mut App) {
        let Some(handle) = self.window.as_ref() else {
            return;
        };
        let closed = handle
            .update(cx, |view, window, _| {
                let gone = view.x11.as_ref().is_some_and(|x11| !x11.is_alive());
                if gone {
                    window.remove_window();
                }
                gone
            })
            .unwrap_or(false);
        if closed {
            self.window = None;
            self.window_mode = None;
            self.layer_shell = false;
            self.dismissed.set(true);
        }
    }

    #[cfg(not(target_os = "linux"))]
    #[expect(
        clippy::unused_self,
        reason = "only the X11 window can be closed behind the overlay's back"
    )]
    fn refresh_dismissed(&mut self, _cx: &mut App) {}

    fn sync(&mut self, cx: &mut App) {
        self.refresh_dismissed(cx);
        if self.dismissed.get() {
            self.close(cx);
            return;
        }
        let rows = self.rows();
        if !self.settings.enabled || rows.is_empty() {
            self.close(cx);
            return;
        }
        if let Some(handle) = self.window.as_ref() {
            let same = self.window_mode == Some(self.settings.display_mode)
                && handle
                    .update(cx, |view, _, _| view.rows.len() == rows.len())
                    .unwrap_or(false);
            if same {
                let _ = handle.update(cx, |view, _, cx| {
                    view.rows = rows;
                    cx.notify();
                });
                return;
            }
        }
        let origin = self.window.as_ref().and_then(|handle| {
            handle
                .update(cx, |_, window, _| window.bounds().origin)
                .ok()
        });
        self.close(cx);
        self.open(&rows, origin, cx);
    }

    fn close(&mut self, cx: &mut App) {
        if let Some(handle) = self.window.take() {
            if !self.layer_shell
                && let Ok(origin) = handle.update(cx, |_, window, _| window.bounds().origin)
                && self.last_saved_origin != Some(origin)
            {
                save_position(origin);
                self.last_saved_origin = Some(origin);
            }
            let _ = handle.update(cx, |_, window, _| window.remove_window());
        }
        self.window_mode = None;
        self.layer_shell = false;
    }

    fn open(&mut self, rows: &[Row], remembered: Option<Point<gpui::Pixels>>, cx: &mut App) {
        let card = card_size(rows.len());
        let origin = resolve_origin(cx, remembered, card);
        let bounds = Bounds::new(origin, card);
        // A Wayland compositor places ordinary windows itself, so ask for a
        // layer-shell surface first (borderless, anchored bottom-right). An
        // X11 session, or a compositor without wlr-layer-shell, gets the
        // ordinary window.
        let attempts = window_attempts(self.settings.display_mode, bounds);
        let mut layer_shell = false;
        let opened = crate::platform::open_first(
            attempts.into_iter().map(|options| vec![options]).collect(),
            |options| {
                layer_shell = is_layer_shell(&options.kind);
                cx.open_window(options, |_, cx| {
                    cx.new(|_| FlowIndicatorView::new(rows.to_vec()))
                })
            },
        );
        let mode = self.settings.display_mode;
        let Some(handle) = opened else {
            return;
        };
        let configured = handle.update(cx, |view, window, _| {
            apply_display_mode(window, mode);
            view.on_opened(window, origin);
        });
        if let Err(error) = configured {
            debug!(%error, "Flow Indicator: could not configure its window");
        }
        self.window = Some(handle);
        self.window_mode = Some(mode);
        self.layer_shell = layer_shell;
    }
}

#[cfg(target_os = "windows")]
fn window_options(_mode: FlowIndicatorDisplayMode, bounds: Bounds<gpui::Pixels>) -> WindowOptions {
    // Windows has no non-topmost toolwindow kind in GPUI. A PopUp is the
    // taskbar-less/Alt-Tab-less window; `apply_display_mode` strips its
    // TOPMOST bit when the user chose Normal.
    WindowOptions {
        titlebar: None,
        focus: false,
        show: true,
        kind: WindowKind::PopUp,
        is_movable: true,
        is_resizable: false,
        is_minimizable: false,
        window_background: WindowBackgroundAppearance::Transparent,
        app_id: Some("openlogi-flow-indicator".to_string()),
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        ..WindowOptions::default()
    }
}

#[cfg(not(target_os = "windows"))]
fn window_options(mode: FlowIndicatorDisplayMode, bounds: Bounds<gpui::Pixels>) -> WindowOptions {
    // Other platforms map the mode through the window kind directly: a
    // Normal window can be covered, a PopUp floats above them.
    let kind = match mode {
        FlowIndicatorDisplayMode::Normal => WindowKind::Normal,
        FlowIndicatorDisplayMode::Topmost => WindowKind::PopUp,
    };
    WindowOptions {
        titlebar: None,
        focus: false,
        show: true,
        kind,
        is_movable: true,
        is_resizable: false,
        is_minimizable: false,
        // The card draws its own chrome; a compositor that decorates
        // server-side (KWin, labwc) must leave it borderless. X11 honours
        // this through `_MOTIF_WM_HINTS`, Wayland through xdg-decoration.
        window_decorations: Some(WindowDecorations::Client),
        window_background: WindowBackgroundAppearance::Transparent,
        app_id: Some("openlogi-flow-indicator".to_string()),
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        ..WindowOptions::default()
    }
}

/// Window options to try in order for the card.
fn window_attempts(
    mode: FlowIndicatorDisplayMode,
    bounds: Bounds<gpui::Pixels>,
) -> Vec<WindowOptions> {
    #[cfg(target_os = "linux")]
    if let Some(options) = layer_shell_window_options(mode, bounds) {
        return vec![options, window_options(mode, bounds)];
    }
    vec![window_options(mode, bounds)]
}

/// The card as a Wayland layer surface: borderless, anchored bottom-right of
/// the output rather than placed by the compositor.
#[cfg(target_os = "linux")]
fn layer_shell_window_options(
    mode: FlowIndicatorDisplayMode,
    bounds: Bounds<gpui::Pixels>,
) -> Option<WindowOptions> {
    use gpui::layer_shell::{Anchor, KeyboardInteractivity, Layer, LayerShellOptions};

    if gpui::guess_compositor() != "Wayland" || crate::platform::layer_shell_refused() {
        return None;
    }
    // Normal must stay coverable by other windows, which is the bottom layer;
    // topmost sits above them in the top layer.
    let layer = match mode {
        FlowIndicatorDisplayMode::Normal => Layer::Bottom,
        FlowIndicatorDisplayMode::Topmost => Layer::Top,
    };
    Some(WindowOptions {
        kind: WindowKind::LayerShell(LayerShellOptions {
            namespace: "openlogi-flow-indicator".to_string(),
            layer,
            anchor: Anchor::BOTTOM | Anchor::RIGHT,
            margin: Some((px(0.0), px(MARGIN), px(MARGIN), px(0.0))),
            keyboard_interactivity: KeyboardInteractivity::None,
            ..LayerShellOptions::default()
        }),
        titlebar: None,
        focus: false,
        show: true,
        window_background: WindowBackgroundAppearance::Transparent,
        app_id: Some("openlogi-flow-indicator".to_string()),
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        ..WindowOptions::default()
    })
}

#[cfg(target_os = "linux")]
fn is_layer_shell(kind: &WindowKind) -> bool {
    matches!(kind, WindowKind::LayerShell(_))
}

#[cfg(not(target_os = "linux"))]
const fn is_layer_shell(_kind: &WindowKind) -> bool {
    false
}

#[cfg(target_os = "windows")]
#[expect(
    unsafe_code,
    reason = "strip WS_EX_TOPMOST from the live GPUI window on its UI thread"
)]
fn apply_display_mode(window: &mut Window, mode: FlowIndicatorDisplayMode) {
    use raw_window_handle::RawWindowHandle;
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GWL_EXSTYLE, GetWindowLongW, HWND_NOTOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
        SetWindowLongW, SetWindowPos,
    };

    if mode == FlowIndicatorDisplayMode::Topmost {
        return;
    }
    let Ok(handle) = raw_window_handle::HasWindowHandle::window_handle(window) else {
        return;
    };
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return;
    };
    let hwnd = handle.hwnd.get() as HWND;
    // WS_EX_TOPMOST's documented value; keeping it here avoids a lossy
    // integer cast from the windows-sys `u32` constant.
    // SAFETY: style reads/writes on the live HWND owned by `window`, on the
    // thread GPUI requires; the style value is a bit-set of documented flags.
    unsafe {
        let style = GetWindowLongW(hwnd, GWL_EXSTYLE);
        SetWindowLongW(hwnd, GWL_EXSTYLE, style & !TOPMOST_BIT);
        SetWindowPos(
            hwnd,
            HWND_NOTOPMOST,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        );
    }
}

#[cfg(not(target_os = "windows"))]
fn apply_display_mode(_window: &mut Window, _mode: FlowIndicatorDisplayMode) {}

#[cfg(test)]
mod tests {
    use super::{SavedPosition, card_size, clamp_origin};
    use gpui::{Bounds, point, px, size};

    #[test]
    fn the_card_height_follows_the_row_count() {
        // Two 1 px borders, the 14 px padding top and bottom, 26+9+6 per row,
        // and the 16 px gap between rows.
        assert_eq!(f32::from(card_size(1).height), 2.0 + 28.0 + 41.0);
        assert_eq!(
            f32::from(card_size(3).height),
            2.0 + 28.0 + 3.0 * 41.0 + 2.0 * 16.0
        );
        assert_eq!(
            f32::from(card_size(5).height),
            2.0 + 28.0 + 5.0 * 41.0 + 4.0 * 16.0
        );
    }

    #[test]
    fn a_restored_position_is_clamped_to_the_screen() {
        let area = Bounds::new(point(px(0.0), px(0.0)), size(px(1920.0), px(1080.0)));
        let card = size(px(300.0), px(185.0));

        assert_eq!(
            clamp_origin(area, card, point(px(5000.0), px(5000.0))),
            point(px(1620.0), px(895.0))
        );
        assert_eq!(
            clamp_origin(area, card, point(px(-50.0), px(-50.0))),
            point(px(0.0), px(0.0))
        );
    }

    #[test]
    fn the_saved_position_roundtrips_through_toml() {
        let body = toml::to_string(&SavedPosition { x: 42.5, y: -7.25 }).expect("serialize");
        let saved: SavedPosition = toml::from_str(&body).expect("parse");
        assert_eq!((saved.x, saved.y), (42.5, -7.25));
    }

    #[gpui::test]
    fn the_card_window_renders_its_rows(cx: &mut gpui::TestAppContext) {
        use super::{FlowIndicatorView, Row};
        use openlogi_core::device::{BatteryColor, DeviceKind};

        let rows = vec![Row {
            name: "MX Master 3S".into(),
            value: "87%".into(),
            fill: Some(87),
            color: BatteryColor::Green,
            charging: false,
            online: true,
            kind: DeviceKind::Mouse,
        }];
        let window = cx.add_window(|_, _| FlowIndicatorView::new(rows));
        cx.run_until_parked();
        let shown = window
            .read_with(cx, |view, _| view.rows.len())
            .expect("window is live");
        assert_eq!(shown, 1);
    }

    /// The window is sized from `card_size`, so the laid-out card must be
    /// exactly that tall; anything taller is clipped at the window's edge,
    /// losing the bottom row's bar and the card's corners.
    #[gpui::test]
    fn the_card_fits_the_window_height_it_is_given(cx: &mut gpui::TestAppContext) {
        use super::{FlowIndicatorView, Row};
        use openlogi_core::device::{BatteryColor, DeviceKind};

        let rows = ["Direct Mouse", "Performance Mouse", "RGB Keyboard"]
            .into_iter()
            .map(|name| Row {
                name: name.into(),
                value: "55%".into(),
                fill: Some(55),
                color: BatteryColor::Amber,
                charging: false,
                online: true,
                kind: DeviceKind::Mouse,
            })
            .collect::<Vec<_>>();
        let window = card_size(rows.len());
        let (_view, cx) = cx.add_window_view(|_, _| FlowIndicatorView::new(rows));
        cx.simulate_resize(gpui::size(gpui::px(400.0), gpui::px(400.0)));
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let card = cx
            .debug_bounds("flow-indicator-card")
            .expect("the card renders");
        assert_eq!(card.size, window, "the card must fit its window");
    }
}
