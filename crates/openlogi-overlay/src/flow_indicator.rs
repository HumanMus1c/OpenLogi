//! The floating Flow Indicator: a small card listing the devices the agent has
//! detected and their battery readings.
//!
//! The window is a non-activating pop-up owned by this process, so it survives
//! the settings window closing. Rows come from the same `Observer::state`
//! snapshot the GUI reads, and settings from the shared `config.toml` (polled
//! by mtime). The pin toggle keeps the card on top and is remembered in an
//! overlay-owned file beside the config.

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::SystemTime;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, AppContext as _, Bounds, ClickEvent, Context, FontWeight, Hsla, InteractiveElement as _,
    IntoElement, ParentElement, Point, Render, SharedString, Size, StatefulInteractiveElement as _,
    Styled, Subscription, Window, WindowAppearance, WindowBackgroundAppearance, WindowBounds,
    WindowHandle, WindowKind, WindowOptions, div, point, px, rgb, size, svg,
};
use openlogi_core::config::{
    AppSettings, Appearance, Config, ConfigFile, FlowIndicatorDisplayMode, battery_devices_named,
};
use openlogi_core::device::{
    BatteryDevice, BatteryInfo, BatteryTone, BatteryValue, DeviceInventory, DeviceKind,
    battery_devices, flow_indicator_devices,
};
use openlogi_ipc::Observation;
use openlogi_ui::action_icons::PIN_ICON;
use openlogi_ui::color::neutral;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tracing::{debug, warn};

/// Card width at 100% scale.
const CARD_WIDTH: f32 = 300.0;
const PADDING_X: f32 = 16.0;
const PADDING_Y: f32 = 14.0;
const RADIUS: f32 = 12.0;
/// The header's pin lane, and the gap to the first row.
const HEADER_LANE: f32 = 20.0;
const HEADER_GAP: f32 = 6.0;
const PIN_ICON_SIZE: f32 = 14.0;
/// The line box is pinned so the card's height arithmetic matches the layout:
/// GPUI's default line height otherwise makes the last row overflow.
const LINE_HEIGHT: f32 = 26.0;
const LINE_GAP: f32 = 9.0;
const BAR_HEIGHT: f32 = 6.0;
const ROW_HEIGHT: f32 = LINE_HEIGHT + LINE_GAP + BAR_HEIGHT;
const ROW_GAP: f32 = 16.0;
const ICON_SIZE: f32 = 18.0;
const ICON_LANE: f32 = 22.0;
const ROW_INNER_GAP: f32 = 8.0;
const BOLT_SIZE: f32 = 16.0;
const NAME_TEXT_SIZE: f32 = 16.0;
const VALUE_TEXT_SIZE: f32 = 16.0;
/// A minimum trailing lane for the reading, so the name's truncation point
/// stays stable as `9%`, `99%` and `100%` change width.
const VALUE_LANE: f32 = 48.0;
/// Transparent room around the visible card so its box shadow is drawn by the
/// window instead of being clipped at its edge.
const SHADOW_MARGIN: f32 = 24.0;

const BOLT_ICON: &str = "action-icons/bolt.svg";
const MOUSE_ICON: &str = "action-icons/mouse.svg";
const KEYBOARD_ICON: &str = "action-icons/keyboard.svg";
const HEADSET_ICON: &str = "action-icons/headset.svg";
const GAMEPAD_ICON: &str = "action-icons/gamepad-2.svg";
const CAMERA_ICON: &str = "action-icons/camera.svg";
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
    pub(crate) appearance: Appearance,
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
            appearance: settings.appearance,
        }
    }
}

/// Read the widget settings and the config its rows take product names from.
/// A missing or invalid file keeps the caller's last-good settings.
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

/// The overlay-owned pin state. It lives in the same file the fork uses for
/// the card placement, so an existing `pinned` value carries over; the extra
/// placement keys are ignored here and dropped when the state is rewritten.
#[derive(Debug, Default, Serialize, Deserialize)]
struct SavedState {
    #[serde(default)]
    pinned: bool,
}

fn state_path() -> Option<PathBuf> {
    openlogi_core::paths::config_dir()
        .ok()
        .map(|dir| dir.join("flow-indicator.toml"))
}

pub(crate) fn load_pinned() -> bool {
    let Some(path) = state_path() else {
        return false;
    };
    let Ok(body) = std::fs::read_to_string(&path) else {
        return false;
    };
    match toml::from_str::<SavedState>(&body) {
        Ok(saved) => saved.pinned,
        Err(error) => {
            warn!(%error, "Flow Indicator: ignoring an unreadable pin state");
            false
        }
    }
}

fn save_pinned(pinned: bool) {
    let Some(path) = state_path() else {
        return;
    };
    if let Some(parent) = path.parent()
        && let Err(error) = std::fs::create_dir_all(parent)
    {
        warn!(%error, "Flow Indicator: could not create its config directory");
        return;
    }
    let Ok(body) = toml::to_string(&SavedState { pinned }) else {
        return;
    };
    let written = atomic_write_file::AtomicWriteFile::options()
        .open(&path)
        .and_then(|mut file| {
            std::io::Write::write_all(&mut file, body.as_bytes())?;
            file.commit()
        });
    if let Err(error) = written {
        warn!(%error, "Flow Indicator: could not save its pin state");
    }
}

fn card_size(rows: usize) -> Size<gpui::Pixels> {
    let count = u16::try_from(rows).unwrap_or(u16::MAX);
    let rows_height = f32::from(count) * ROW_HEIGHT;
    let gaps = f32::from(count.saturating_sub(1)) * ROW_GAP;
    size(
        px(CARD_WIDTH),
        px(PADDING_Y * 2.0 + HEADER_LANE + HEADER_GAP + rows_height + gaps),
    )
}

fn window_size(card: Size<gpui::Pixels>) -> Size<gpui::Pixels> {
    size(
        px(f32::from(card.width) + SHADOW_MARGIN * 2.0),
        px(f32::from(card.height) + SHADOW_MARGIN * 2.0),
    )
}

/// Place the window so the visible card sits in the bottom-right corner with a
/// `SHADOW_MARGIN` screen margin. The window's own right and bottom edges sit
/// on the work-area edge, leaving the shadow room inside the window.
fn window_origin(area: Bounds<gpui::Pixels>, window: Size<gpui::Pixels>) -> Point<gpui::Pixels> {
    point(
        (area.right() - window.width).max(area.origin.x),
        (area.bottom() - window.height).max(area.origin.y),
    )
}

fn resolve_origin(cx: &App, card: Size<gpui::Pixels>) -> Point<gpui::Pixels> {
    let window = window_size(card);
    cx.primary_display().map_or_else(
        || point(px(0.0), px(0.0)),
        |display| window_origin(display.visible_bounds(), window),
    )
}

/// One rendered device row.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Row {
    name: SharedString,
    value: SharedString,
    fill: Option<u8>,
    tone: BatteryTone,
    online: bool,
    kind: DeviceKind,
}

impl Row {
    fn from(device: &BatteryDevice) -> Self {
        let battery = device.battery.as_ref();
        let tone = battery.map_or(BatteryTone::Normal, BatteryInfo::tone);
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
            tone,
            online: device.online,
            kind: device.kind,
        }
    }

    fn charging(&self) -> bool {
        self.tone == BatteryTone::Charging
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
    if matches!(
        kind,
        DeviceKind::Mouse | DeviceKind::Trackball | DeviceKind::Touchpad | DeviceKind::Tablet
    ) {
        MOUSE_ICON
    } else if kind.is_keyboard_family() {
        KEYBOARD_ICON
    } else {
        match kind {
            DeviceKind::Headset => HEADSET_ICON,
            DeviceKind::Gamepad | DeviceKind::Joystick => GAMEPAD_ICON,
            DeviceKind::Camera => CAMERA_ICON,
            _ => GENERIC_ICON,
        }
    }
}

struct Palette {
    card: Hsla,
    text: Hsla,
    muted: Hsla,
    track: Hsla,
    border: Hsla,
    accent: Hsla,
}

impl Palette {
    fn for_dark(dark: bool) -> Self {
        let accent = rgb(0x3B_82_F6).into();
        if dark {
            Self {
                card: neutral(0.10, 0.98),
                text: rgb(0xF5_F5_F7).into(),
                muted: rgb(0xA1_A1_A6).into(),
                track: neutral(1.0, 0.12),
                border: neutral(1.0, 0.10),
                accent,
            }
        } else {
            Self {
                card: neutral(1.0, 0.98),
                text: rgb(0x1D_1D_1F).into(),
                muted: rgb(0x6E_6E_73).into(),
                track: neutral(0.0, 0.08),
                border: neutral(0.0, 0.12),
                accent,
            }
        }
    }
}

fn tone_color(tone: BatteryTone, pal: &Palette) -> Hsla {
    match tone {
        BatteryTone::Charging | BatteryTone::Full => rgb(0x22_C5_5E).into(),
        BatteryTone::Low => rgb(0xEA_B3_08).into(),
        BatteryTone::Critical | BatteryTone::Error => rgb(0xEF_44_44).into(),
        BatteryTone::Normal => pal.text,
    }
}

/// What the widget should show: a row set, the pin toggle, and the theme.
struct FlowIndicatorView {
    rows: Vec<Row>,
    pinned: Rc<Cell<bool>>,
    appearance: Appearance,
    /// Repaints when the platform's light/dark appearance changes.
    appearance_subscription: Option<Subscription>,
    command_tx: mpsc::UnboundedSender<Command>,
}

impl FlowIndicatorView {
    fn new(
        rows: Vec<Row>,
        pinned: Rc<Cell<bool>>,
        appearance: Appearance,
        command_tx: mpsc::UnboundedSender<Command>,
    ) -> Self {
        Self {
            rows,
            pinned,
            appearance,
            appearance_subscription: None,
            command_tx,
        }
    }

    fn observe_appearance(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.appearance_subscription =
            Some(cx.observe_window_appearance(window, |_, _, cx| cx.notify()));
    }

    fn dark_mode(&self, window: &Window) -> bool {
        if self.pinned.get() {
            return true;
        }
        match self.appearance {
            Appearance::System => matches!(
                window.appearance(),
                WindowAppearance::Dark | WindowAppearance::VibrantDark
            ),
            Appearance::Light => false,
            Appearance::Dark => true,
        }
    }

    fn header(&self, pal: &Palette, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let pinned = self.pinned.get();
        div()
            .h(px(HEADER_LANE))
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(pal.muted)
                    .child(if pinned {
                        rust_i18n::t!("flow_indicator.unpin_card").to_string()
                    } else {
                        rust_i18n::t!("flow_indicator.pin_card").to_string()
                    }),
            )
            .child(
                div()
                    .id("flow-indicator-pin")
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(px(HEADER_LANE))
                    .rounded(px(6.0))
                    .cursor_pointer()
                    .hover(|style| style.bg(pal.track))
                    .on_click(cx.listener(Self::toggle_pin))
                    .child(
                        svg()
                            .path(PIN_ICON)
                            .size(px(PIN_ICON_SIZE))
                            .text_color(if pinned { pal.accent } else { pal.muted }),
                    ),
            )
    }

    fn row_view(row: &Row, pal: &Palette) -> impl IntoElement + use<> {
        let color = tone_color(row.tone, pal);
        let inner_width = CARD_WIDTH - PADDING_X * 2.0;
        let fill_width = f32::from(row.fill.unwrap_or(0)) / 100.0 * inner_width;
        div()
            .flex()
            .flex_col()
            .gap(px(LINE_GAP))
            .child(
                div()
                    .h(px(LINE_HEIGHT))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(ROW_INNER_GAP))
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
                            .text_size(px(NAME_TEXT_SIZE))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(pal.text)
                            .child(row.name.clone()),
                    )
                    .when(row.online && row.charging(), |line| {
                        line.child(
                            svg()
                                .path(BOLT_ICON)
                                .size(px(BOLT_SIZE))
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
                            .w(px(VALUE_LANE))
                            .flex_none()
                            .flex()
                            .justify_end()
                            .text_size(px(VALUE_TEXT_SIZE))
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

    fn toggle_pin(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        let next = !self.pinned.get();
        self.pinned.set(next);
        cx.notify();
        let _ = self.command_tx.send(Command::Pin(next));
    }

    fn card(&self, pal: &Palette, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        div()
            .w(px(CARD_WIDTH))
            .flex()
            .flex_col()
            .gap(px(HEADER_GAP))
            .px(px(PADDING_X))
            .py(px(PADDING_Y))
            .rounded(px(RADIUS))
            .bg(pal.card)
            .border_1()
            .border_color(if self.pinned.get() {
                pal.accent
            } else {
                pal.border
            })
            .shadow_lg()
            .child(self.header(pal, cx))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(ROW_GAP))
                    .children(self.rows.iter().map(|row| Self::row_view(row, pal))),
            )
    }
}

impl Render for FlowIndicatorView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = Palette::for_dark(self.dark_mode(window));
        div()
            .size_full()
            .p(px(SHADOW_MARGIN))
            .child(self.card(&pal, cx))
    }
}

/// What the widget task receives: a new agent snapshot, new config, or a pin
/// toggle from the card's own button.
pub(crate) enum Command {
    Observation(Box<Observation>),
    Settings {
        settings: Settings,
        config: Option<Box<Config>>,
    },
    Pin(bool),
}

/// The widget's window lifecycle: open it when there is something to show,
/// update it in place for percentage changes, and reopen it when its height,
/// display mode, or pin state changes.
///
/// This type is owned by one GPUI task, not an entity: opening a window inside
/// an entity update re-enters GPUI's app borrow. The task hands each command
/// through `App::update`, the same context the notice windows open from.
pub(crate) struct FlowIndicator {
    settings: Settings,
    config: Option<Config>,
    inventory: Vec<DeviceInventory>,
    pinned: Rc<Cell<bool>>,
    window: Option<WindowHandle<FlowIndicatorView>>,
    window_mode: Option<FlowIndicatorDisplayMode>,
    window_pinned: Option<bool>,
    command_tx: mpsc::UnboundedSender<Command>,
}

impl FlowIndicator {
    pub(crate) fn new(
        settings: Settings,
        config: Option<Config>,
        pinned: Rc<Cell<bool>>,
        command_tx: mpsc::UnboundedSender<Command>,
    ) -> Self {
        Self {
            settings,
            config,
            inventory: Vec::new(),
            pinned,
            window: None,
            window_mode: None,
            window_pinned: None,
            command_tx,
        }
    }

    /// Whether the next command may have to open the card window.
    ///
    /// Windows starts the overlay headless, and GPUI refuses a window while
    /// headless (see [`crate::windowing::connect`]) — so the caller must
    /// connect to the display before feeding the owner a command that may
    /// open the card. A settings command is checked against the settings it
    /// carries, because the toggle that enables the card arrives that way.
    pub(crate) fn needs_display(&self, command: &Command) -> bool {
        let enabled = match command {
            Command::Settings { settings, .. } => settings.enabled,
            Command::Observation(_) => self.settings.enabled,
            Command::Pin(_) => false,
        };
        enabled && self.window.is_none()
    }

    pub(crate) fn handle(&mut self, command: Command, cx: &mut App) {
        match command {
            Command::Observation(observation) => {
                self.inventory = observation.snapshot.inventory;
                self.sync(cx);
            }
            Command::Settings { settings, config } => {
                self.settings = settings;
                self.config = config.map(|config| *config);
                self.sync(cx);
            }
            Command::Pin(pinned) => {
                self.pinned.set(pinned);
                save_pinned(pinned);
                self.sync(cx);
            }
        }
    }

    fn rows(&self) -> Vec<Row> {
        if !self.settings.enabled {
            return Vec::new();
        }
        let devices = match self.config.as_ref() {
            Some(config) => battery_devices_named(&self.inventory, config),
            None => battery_devices(&self.inventory),
        };
        flow_indicator_devices(
            &devices,
            self.settings.show_offline,
            self.settings.max_devices,
        )
        .into_iter()
        .map(Row::from)
        .collect()
    }

    fn sync(&mut self, cx: &mut App) {
        let rows = self.rows();
        if !self.settings.enabled || rows.is_empty() {
            self.close(cx);
            return;
        }
        let pinned = self.pinned.get();
        let mode = self.settings.display_mode;
        let appearance = self.settings.appearance;
        if let Some(handle) = self.window.as_ref() {
            let same_mode = self.window_mode == Some(mode);
            let same_pinned = self.window_pinned == Some(pinned);
            let same_rows = handle
                .update(cx, |view, _, _| view.rows.len() == rows.len())
                .unwrap_or(false);
            if same_mode && same_rows {
                let _ = handle.update(cx, |view, window, cx| {
                    view.rows.clone_from(&rows);
                    view.appearance = appearance;
                    if !same_pinned {
                        apply_window_mode(window, mode, pinned);
                    }
                    cx.notify();
                });
                if same_pinned {
                    return;
                }
                #[cfg(target_os = "windows")]
                {
                    self.window_pinned = Some(pinned);
                    return;
                }
            }
        }
        self.close(cx);
        self.open(rows, cx);
    }

    fn close(&mut self, cx: &mut App) {
        if let Some(handle) = self.window.take() {
            let _ = handle.update(cx, |_, window, _| window.remove_window());
        }
        self.window_mode = None;
        self.window_pinned = None;
    }

    fn open(&mut self, rows: Vec<Row>, cx: &mut App) {
        let card = card_size(rows.len());
        let window = window_size(card);
        let origin = resolve_origin(cx, card);
        let pinned = self.pinned.get();
        let options = window_options(
            self.settings.display_mode,
            pinned,
            Bounds::new(origin, window),
        );
        let appearance = self.settings.appearance;
        let command_tx = self.command_tx.clone();
        let pin_state = Rc::clone(&self.pinned);
        let opened = cx.open_window(options, |_, cx| {
            cx.new(|_| FlowIndicatorView::new(rows, pin_state, appearance, command_tx))
        });
        let handle = match opened {
            Ok(handle) => handle,
            Err(error) => {
                warn!(%error, "Flow Indicator: could not open its window");
                return;
            }
        };
        let mode = self.settings.display_mode;
        if let Err(error) = handle.update(cx, |view, window, cx| {
            apply_window_mode(window, mode, pinned);
            place_card(window);
            view.observe_appearance(window, cx);
        }) {
            debug!(%error, "Flow Indicator: could not configure its window");
        }
        self.window = Some(handle);
        self.window_mode = Some(mode);
        self.window_pinned = Some(pinned);
    }
}

fn window_options(
    _mode: FlowIndicatorDisplayMode,
    _pinned: bool,
    bounds: Bounds<gpui::Pixels>,
) -> WindowOptions {
    // Windows has no non-topmost toolwindow kind in GPUI. A PopUp is the
    // taskbar-less/Alt-Tab-less window; `apply_window_mode` sets or strips its
    // TOPMOST bit. Other platforms map the mode through the window kind.
    #[cfg(target_os = "windows")]
    let kind = WindowKind::PopUp;
    #[cfg(not(target_os = "windows"))]
    let kind = if _pinned || _mode == FlowIndicatorDisplayMode::Topmost {
        WindowKind::PopUp
    } else {
        WindowKind::Normal
    };
    WindowOptions {
        titlebar: None,
        focus: false,
        show: true,
        kind,
        is_movable: false,
        is_resizable: false,
        is_minimizable: false,
        window_background: WindowBackgroundAppearance::Transparent,
        app_id: Some("openlogi-flow-indicator".to_string()),
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        ..WindowOptions::default()
    }
}

#[cfg(target_os = "windows")]
#[expect(
    unsafe_code,
    reason = "set or strip WS_EX_TOPMOST on the live GPUI window on its UI thread"
)]
fn apply_window_mode(window: &mut Window, mode: FlowIndicatorDisplayMode, pinned: bool) {
    use raw_window_handle::RawWindowHandle;
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GWL_EXSTYLE, GetWindowLongW, HWND_NOTOPMOST, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE,
        SWP_NOSIZE, SetWindowLongW, SetWindowPos,
    };

    let topmost = pinned || mode == FlowIndicatorDisplayMode::Topmost;
    let Ok(handle) = raw_window_handle::HasWindowHandle::window_handle(window) else {
        return;
    };
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return;
    };
    let hwnd = handle.hwnd.get() as HWND;
    // SAFETY: style reads/writes on the live HWND owned by `window`, on the
    // thread GPUI requires; the style value is a bit-set of documented flags.
    unsafe {
        let style = GetWindowLongW(hwnd, GWL_EXSTYLE);
        let style = if topmost {
            style | TOPMOST_BIT
        } else {
            style & !TOPMOST_BIT
        };
        SetWindowLongW(hwnd, GWL_EXSTYLE, style);
        SetWindowPos(
            hwnd,
            if topmost {
                HWND_TOPMOST
            } else {
                HWND_NOTOPMOST
            },
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        );
    }
}

#[cfg(not(target_os = "windows"))]
fn apply_window_mode(_window: &mut Window, _mode: FlowIndicatorDisplayMode, _pinned: bool) {}

/// Place the card in the primary monitor's work area after GPUI created it.
///
/// GPUI's display bounds on Windows are physical pixels while the window
/// bounds the card is opened with are logical pixels, so the logical
/// bottom-right origin is wrong at any scale other than 100%. Working from
/// the real window rect and the monitor's work area keeps the card on screen
/// at every scale. The window includes the shadow margin, so its bottom-right
/// edge lands on the work area's edge while the visible card stays one margin
/// inside it.
#[cfg(target_os = "windows")]
#[expect(
    unsafe_code,
    reason = "place the live GPUI window with Win32's physical monitor work area"
)]
#[expect(
    clippy::cast_possible_truncation,
    reason = "a work-area coordinate is a whole physical pixel"
)]
fn place_card(window: &mut Window) {
    use raw_window_handle::RawWindowHandle;
    use windows_sys::Win32::Foundation::{HWND, POINT, RECT};
    use windows_sys::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromPoint,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetWindowRect, SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER, SetWindowPos,
    };

    let Ok(handle) = raw_window_handle::HasWindowHandle::window_handle(window) else {
        return;
    };
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return;
    };
    let hwnd = handle.hwnd.get() as HWND;
    // SAFETY: reads the live HWND's rect and the primary monitor's work area,
    // then moves that same window without resizing or changing its z-order,
    // on GPUI's UI thread.
    unsafe {
        let monitor = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
        if monitor.is_null() {
            return;
        }
        let mut info: MONITORINFO = std::mem::zeroed();
        info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        if GetMonitorInfoW(monitor, &raw mut info) == 0 {
            return;
        }
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        if GetWindowRect(hwnd, &raw mut rect) == 0 {
            return;
        }
        let x = (info.rcWork.right - (rect.right - rect.left)).max(info.rcWork.left);
        let y = (info.rcWork.bottom - (rect.bottom - rect.top)).max(info.rcWork.top);
        SetWindowPos(
            hwnd,
            std::ptr::null_mut(),
            x,
            y,
            0,
            0,
            SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOZORDER,
        );
    }
}

#[cfg(not(target_os = "windows"))]
fn place_card(_window: &mut Window) {}

#[cfg(test)]
mod tests {
    use super::{
        Appearance, Command, FlowIndicator, SavedState, Settings, card_size, window_origin,
        window_size,
    };
    use gpui::{Bounds, point, px, size};
    use std::cell::Cell;
    use std::rc::Rc;
    use tokio::sync::mpsc;

    fn settings(enabled: bool) -> Settings {
        Settings {
            enabled,
            ..Settings::default()
        }
    }

    #[test]
    fn the_card_asks_for_a_display_when_a_command_may_open_it() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let pinned = Rc::new(Cell::new(false));
        let indicator = FlowIndicator::new(Settings::default(), None, pinned, tx);
        let disabled = Command::Settings {
            settings: Settings::default(),
            config: None,
        };
        assert!(!indicator.needs_display(&disabled));

        let enabling = Command::Settings {
            settings: settings(true),
            config: None,
        };
        assert!(
            indicator.needs_display(&enabling),
            "turning the card on must connect before its window opens"
        );
    }

    #[test]
    fn the_card_height_follows_the_row_count() {
        assert_eq!(f32::from(card_size(1).height), 28.0 + 20.0 + 6.0 + 41.0);
        assert_eq!(
            f32::from(card_size(3).height),
            28.0 + 20.0 + 6.0 + 3.0 * 41.0 + 2.0 * 16.0
        );
        assert_eq!(
            f32::from(card_size(5).height),
            28.0 + 20.0 + 6.0 + 5.0 * 41.0 + 4.0 * 16.0
        );
    }

    #[test]
    fn the_window_leaves_room_for_the_shadow_and_the_card_corner() {
        let card = card_size(3);
        let window = window_size(card);
        assert_eq!(f32::from(window.width), f32::from(card.width) + 2.0 * 24.0);
        assert_eq!(
            f32::from(window.height),
            f32::from(card.height) + 2.0 * 24.0
        );

        let area = Bounds::new(point(px(0.0), px(0.0)), size(px(1920.0), px(1080.0)));
        assert_eq!(
            window_origin(area, window),
            point(
                px(1920.0 - f32::from(window.width)),
                px(1080.0 - f32::from(window.height))
            )
        );
    }

    #[test]
    fn the_pin_state_roundtrips_through_toml() {
        let body = toml::to_string(&SavedState { pinned: true }).expect("serialize");
        let saved: SavedState = toml::from_str(&body).expect("parse");
        assert!(saved.pinned);
    }

    #[gpui::test]
    fn the_card_window_renders_its_rows(cx: &mut gpui::TestAppContext) {
        use super::{FlowIndicatorView, Row};
        use openlogi_core::device::{BatteryTone, DeviceKind};

        let rows = vec![Row {
            name: "MX Master 3S".into(),
            value: "87%".into(),
            fill: Some(87),
            tone: BatteryTone::Normal,
            online: true,
            kind: DeviceKind::Mouse,
        }];
        let (tx, _rx) = mpsc::unbounded_channel();
        let pinned = Rc::new(Cell::new(false));
        let window =
            cx.add_window(|_, _| FlowIndicatorView::new(rows, pinned, Appearance::System, tx));
        cx.run_until_parked();
        let shown = window
            .read_with(cx, |view, _| view.rows.len())
            .expect("window is live");
        assert_eq!(shown, 1);
    }
}
