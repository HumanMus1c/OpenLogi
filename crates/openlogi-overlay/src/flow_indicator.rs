//! The floating Flow Indicator: a small card listing the devices the agent has
//! detected and their battery readings.
//!
//! The window is a non-activating pop-up owned by this process, so it survives
//! the settings window closing. Rows come from the same `Observer::state`
//! snapshot the GUI reads, and settings from the shared `config.toml` (polled
//! by mtime).

use std::time::SystemTime;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, AppContext as _, Bounds, Context, FontWeight, Hsla, IntoElement, ParentElement, Point,
    Render, SharedString, Size, Styled, Subscription, Window, WindowAppearance,
    WindowBackgroundAppearance, WindowBounds, WindowHandle, WindowKind, WindowOptions, div, point,
    px, rgb, size, svg,
};
use openlogi_core::config::{AppSettings, ConfigFile, FlowIndicatorDisplayMode};
use openlogi_core::device::{
    BatteryDevice, BatteryInfo, BatteryTone, BatteryValue, DeviceKind, battery_devices,
    flow_indicator_devices,
};
use openlogi_ipc::Observation;
use openlogi_ui::color::neutral;
use tracing::{debug, warn};

/// Card width at 100% scale.
const CARD_WIDTH: f32 = 300.0;
const PADDING_X: f32 = 16.0;
const PADDING_Y: f32 = 14.0;
const RADIUS: f32 = 12.0;
/// One row: the 22 px line, the 9 px gap, and the 6 px bar.
const ROW_HEIGHT: f32 = 37.0;
const ROW_GAP: f32 = 16.0;
const LINE_GAP: f32 = 9.0;
const BAR_HEIGHT: f32 = 6.0;
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

/// Read the widget settings from the shared config. A missing or invalid file
/// keeps the caller's last-good settings.
pub(crate) fn load_settings() -> Option<Settings> {
    match ConfigFile::load_or_default() {
        Ok((config, _)) => Some(Settings::from(&config.app_settings)),
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

fn card_size(rows: usize) -> Size<gpui::Pixels> {
    let count = u16::try_from(rows).unwrap_or(u16::MAX);
    let rows_height = f32::from(count) * ROW_HEIGHT;
    let gaps = f32::from(count.saturating_sub(1)) * ROW_GAP;
    size(px(CARD_WIDTH), px(PADDING_Y * 2.0 + rows_height + gaps))
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

fn resolve_origin(cx: &App, card: Size<gpui::Pixels>) -> Point<gpui::Pixels> {
    cx.primary_display().map_or_else(
        || point(px(0.0), px(0.0)),
        |display| bottom_right(display.visible_bounds(), card),
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
        GENERIC_ICON
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
                card: neutral(0.10, 0.94),
                text: rgb(0xF5_F5_F7).into(),
                muted: rgb(0xA1_A1_A6).into(),
                track: neutral(1.0, 0.12),
                border: neutral(1.0, 0.10),
            }
        } else {
            Self {
                card: neutral(1.0, 0.96),
                text: rgb(0x1D_1D_1F).into(),
                muted: rgb(0x6E_6E_73).into(),
                track: neutral(0.0, 0.08),
                border: neutral(0.0, 0.12),
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

/// What the widget should show: disabled, empty, or a row set.
struct FlowIndicatorView {
    rows: Vec<Row>,
    /// Repaints when the platform's light/dark appearance changes.
    appearance_subscription: Option<Subscription>,
}

impl FlowIndicatorView {
    fn new(rows: Vec<Row>) -> Self {
        Self {
            rows,
            appearance_subscription: None,
        }
    }

    fn observe_appearance(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.appearance_subscription =
            Some(cx.observe_window_appearance(window, |_, _, cx| cx.notify()));
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
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0))
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
                    .when(row.online && row.charging(), |line| {
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
            .border_1()
            .border_color(pal.border)
            .shadow_lg()
            .children(self.rows.iter().map(|row| Self::row_view(row, pal)))
    }
}

impl Render for FlowIndicatorView {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let pal = Palette::for_appearance(window.appearance());
        div().size_full().child(self.card(&pal))
    }
}

/// What the widget task receives: a new agent snapshot or new config.
pub(crate) enum Command {
    Observation(Box<Observation>),
    Settings(Settings),
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
    devices: Vec<BatteryDevice>,
    window: Option<WindowHandle<FlowIndicatorView>>,
    window_mode: Option<FlowIndicatorDisplayMode>,
}

impl FlowIndicator {
    pub(crate) fn new(settings: Settings) -> Self {
        Self {
            settings,
            devices: Vec::new(),
            window: None,
            window_mode: None,
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
            Command::Settings(settings) => settings.enabled,
            Command::Observation(_) => self.settings.enabled,
        };
        enabled && self.window.is_none()
    }

    pub(crate) fn handle(&mut self, command: Command, cx: &mut App) {
        match command {
            Command::Observation(observation) => {
                self.devices = battery_devices(&observation.snapshot.inventory);
                self.sync(cx);
            }
            Command::Settings(settings) => {
                self.settings = settings;
                self.sync(cx);
            }
        }
    }

    fn rows(&self) -> Vec<Row> {
        if !self.settings.enabled {
            return Vec::new();
        }
        flow_indicator_devices(
            &self.devices,
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
        self.close(cx);
        self.open(rows, cx);
    }

    fn close(&mut self, cx: &mut App) {
        if let Some(handle) = self.window.take() {
            let _ = handle.update(cx, |_, window, _| window.remove_window());
        }
        self.window_mode = None;
    }

    fn open(&mut self, rows: Vec<Row>, cx: &mut App) {
        let card = card_size(rows.len());
        let origin = resolve_origin(cx, card);
        let options = window_options(self.settings.display_mode, Bounds::new(origin, card));
        let opened = cx.open_window(options, |_, cx| cx.new(|_| FlowIndicatorView::new(rows)));
        let handle = match opened {
            Ok(handle) => handle,
            Err(error) => {
                warn!(%error, "Flow Indicator: could not open its window");
                return;
            }
        };
        let mode = self.settings.display_mode;
        if let Err(error) = handle.update(cx, |view, window, cx| {
            apply_display_mode(window, mode);
            place_card(window);
            view.observe_appearance(window, cx);
        }) {
            debug!(%error, "Flow Indicator: could not configure its window");
        }
        self.window = Some(handle);
        self.window_mode = Some(mode);
    }
}

fn window_options(_mode: FlowIndicatorDisplayMode, bounds: Bounds<gpui::Pixels>) -> WindowOptions {
    // Windows has no non-topmost toolwindow kind in GPUI. A PopUp is the
    // taskbar-less/Alt-Tab-less window; `apply_display_mode` strips its
    // TOPMOST bit when the user chose Normal. Other platforms map the mode
    // through the window kind directly.
    #[cfg(target_os = "windows")]
    let kind = WindowKind::PopUp;
    #[cfg(not(target_os = "windows"))]
    let kind = match _mode {
        FlowIndicatorDisplayMode::Normal => WindowKind::Normal,
        FlowIndicatorDisplayMode::Topmost => WindowKind::PopUp,
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

/// Place the card in the primary monitor's work area after GPUI created it.
///
/// GPUI's display bounds on Windows are physical pixels while the window
/// bounds the card is opened with are logical pixels, so the logical
/// bottom-right origin is wrong at any scale other than 100%. Working from
/// the real window rect and the monitor's work area keeps the card on screen
/// at every scale.
#[cfg(target_os = "windows")]
#[expect(
    unsafe_code,
    reason = "place the live GPUI window with Win32's physical monitor work area"
)]
#[expect(
    clippy::cast_possible_truncation,
    reason = "a work-area coordinate and the card margin are whole physical pixels"
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
    let margin = (MARGIN * window.scale_factor()).round() as i32;
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
        let x = (info.rcWork.right - (rect.right - rect.left) - margin).max(info.rcWork.left);
        let y = (info.rcWork.bottom - (rect.bottom - rect.top) - margin).max(info.rcWork.top);
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
    use super::{Command, FlowIndicator, Settings, bottom_right, card_size};
    use gpui::{Bounds, point, px, size};

    #[test]
    fn the_card_asks_for_a_display_when_a_command_may_open_it() {
        let indicator = FlowIndicator::new(Settings::default());
        assert!(!indicator.needs_display(&Command::Settings(Settings::default())));

        let enabling = Command::Settings(Settings {
            enabled: true,
            ..Settings::default()
        });
        assert!(
            indicator.needs_display(&enabling),
            "turning the card on must connect before its window opens"
        );
    }

    #[test]
    fn the_card_height_follows_the_row_count() {
        assert_eq!(f32::from(card_size(1).height), 28.0 + 37.0);
        assert_eq!(
            f32::from(card_size(3).height),
            28.0 + 3.0 * 37.0 + 2.0 * 16.0
        );
        assert_eq!(
            f32::from(card_size(5).height),
            28.0 + 5.0 * 37.0 + 4.0 * 16.0
        );
    }

    #[test]
    fn the_card_starts_in_the_bottom_right_corner() {
        let area = Bounds::new(point(px(0.0), px(0.0)), size(px(1920.0), px(1080.0)));
        let card = size(px(300.0), px(171.0));

        assert_eq!(
            bottom_right(area, card),
            point(px(1920.0 - 300.0 - 24.0), px(1080.0 - 171.0 - 24.0))
        );
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
        let window = cx.add_window(|_, _| FlowIndicatorView::new(rows));
        cx.run_until_parked();
        let shown = window
            .read_with(cx, |view, _| view.rows.len())
            .expect("window is live");
        assert_eq!(shown, 1);
    }
}
