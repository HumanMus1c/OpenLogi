//! The floating Flow Indicator: a small card listing the devices the agent has
//! detected and their battery readings.
//!
//! The window is a non-activating pop-up owned by this process, so it survives
//! the settings window closing. Rows come from the same `Observer::state`
//! snapshot the GUI reads, settings from the shared `config.toml` (polled by
//! mtime), and the position from an overlay-owned file beside it.

use std::cell::Cell;
#[cfg(target_os = "windows")]
use std::collections::BTreeMap;
use std::collections::{HashMap, HashSet};
use std::hash::Hasher as _;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

#[cfg(target_os = "linux")]
use gpui::AnyWindowHandle;
#[cfg(target_os = "windows")]
use gpui::DevicePixels;
#[cfg(not(target_os = "windows"))]
use gpui::WindowDecorations;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, AppContext as _, Bounds, ClickEvent, Context, DisplayId, Div, FontWeight, Hsla,
    InteractiveElement as _, IntoElement, MouseExitEvent, MouseMoveEvent, ParentElement, Point,
    Render, Role, SharedString, Size, StatefulInteractiveElement as _, Styled, Subscription,
    TextRun, Toggled, TruncateFrom, Window, WindowAppearance, WindowBackgroundAppearance,
    WindowBounds, WindowControlArea, WindowHandle, WindowKind, WindowOptions, WindowTextSystem,
    div, font, img, point, px, rgb, size, svg, transparent_black,
};
#[cfg(target_os = "linux")]
use gpui::{MouseButton, MouseDownEvent, MouseUpEvent};
use openlogi_core::config::{
    AppSettings, Config, ConfigFile, FlowIndicatorDisplayMode, battery_devices_named,
};
use openlogi_core::device::{
    BatteryColor, BatteryDevice, BatteryInfo, BatteryTone, BatteryValue, DeviceInventory,
    DeviceKind, battery_devices, flow_indicator_rows,
};
use openlogi_ipc::Observation;
use openlogi_ui::action_icons::{PIN_ICON, icon_body};
use openlogi_ui::color::neutral;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::appearance::Appearance;

/// Card width at 100% scale.
const CARD_WIDTH: f32 = 300.0;
pub(crate) const PADDING_X: f32 = 16.0;
pub(crate) const PADDING_Y: f32 = 14.0;
const BORDER: f32 = 1.0;
const RADIUS: f32 = 12.0;
/// One row: the line box, the 9 px gap, and the 6 px bar.
///
/// The line box is pinned because GPUI's default is 1.618x the font size: the
/// 16 px reading would render in a 26 px line and the card's height arithmetic
/// would come up short, clipping the last row at the window's bottom edge.
const LINE_HEIGHT: f32 = 26.0;
/// The reading's font. Its hairline and shadow copies must wear it exactly:
/// another size or weight shapes a second line and rasterizes a second set of
/// glyphs, and the decoration stops landing on the reading's own edge.
const VALUE_TEXT_SIZE: f32 = 16.0;
const VALUE_FONT_WEIGHT: FontWeight = FontWeight::SEMIBOLD;
/// A minimum trailing lane for the reading. It keeps the name's truncation
/// point stable as `9%`, `99%` and `100%` change width without changing the
/// reading's glyphs: the baked reading shadow carries the same proportional
/// digits, so a tabular-numeral font feature would separate them.
const VALUE_LANE: f32 = 48.0;
/// The device name's font. The baked name shadow shapes its baseline from the
/// same pair, so the glow stands on the line the name is painted on.
const NAME_TEXT_SIZE: f32 = 16.0;
const NAME_FONT_WEIGHT: FontWeight = FontWeight::MEDIUM;
const LINE_GAP: f32 = 9.0;
const BAR_HEIGHT: f32 = 6.0;
const ROW_HEIGHT: f32 = LINE_HEIGHT + LINE_GAP + BAR_HEIGHT;
const ROW_GAP: f32 = 16.0;
const ICON_SIZE: f32 = 18.0;
const ICON_LANE: f32 = 22.0;
/// The gap between a row's cells. The name cell's truncation width is derived
/// from the same value, so the painted text and its shadow stop together.
const ROW_INNER_GAP: f32 = 8.0;
/// The charging bolt's box. It wears the row's text scale, so the status
/// glyph does not shrink beside the word it stands in for.
const BOLT_SIZE: f32 = 16.0;
/// The pin toggle's lane above the rows: its hit area, and the gap to the
/// first row. The lane's right end is the button the click-through keeps
/// interactive on Windows and Linux; both derive its rectangle from these
/// constants.
pub(crate) const HEADER_LANE: f32 = 20.0;
const HEADER_GAP: f32 = 6.0;
const PIN_ICON_SIZE: f32 = 14.0;
/// The action hint that shares the header row with the pin: the pin glyph's
/// size, so the hint and the button read as one control.
const HEADER_HINT_TEXT_SIZE: f32 = PIN_ICON_SIZE;
pub(crate) const MARGIN: f32 = 24.0;
/// Transparent room around the visible card so GPUI's large box shadow is
/// drawn by the window instead of being clipped at its edge. The saved origin
/// stays the visible card's origin; the window origin is inset by this much.
pub(crate) const SHADOW_MARGIN: f32 = 24.0;

/// How long a dropped agent connection may stay away before the card marks
/// its readings stale. Long enough to ride out the supervisor's restart — a
/// marker that flashes on every restart teaches nothing — and short enough
/// that frozen readings never pass for live ones for long.
const STALE_AFTER: Duration = Duration::from_secs(5);

/// How long the Windows card keeps re-anchoring after a display topology
/// change before it trusts the window's position again. Windows finishes
/// moving windows for the new layout well inside this window.
#[cfg(target_os = "windows")]
const DISPLAY_SETTLE: Duration = Duration::from_secs(2);

/// How long a card whose GPUI scale disagrees with its monitor's DPI waits for
/// the native watch's re-sent `WM_DPICHANGED` before the owner reopens the
/// window. Reopening gives GPUI a fresh scale from the monitor it lands on.
#[cfg(target_os = "windows")]
const DPI_MISMATCH_GRACE: Duration = Duration::from_secs(2);

/// How long the old pinned card stays visible under its replacement. The
/// replacement is opened hidden while the companion extension moves it onto
/// the old card's frame; the swap itself must not open a visible hole.
#[cfg(target_os = "linux")]
const SWAP_OVERLAP: Duration = Duration::from_millis(1500);

/// The reading's shadow is its own baked asset, one per distinct reading
/// string. The card's shadow hashes its whole row set, so folding the digits
/// into it would mint an asset for every combination of values; keyed by the
/// string alone the set stays small — `0%`–`100%` plus the few localized
/// states — and that bounds the rasters GPUI keeps for good.
pub(crate) const VALUE_SHADOW_PREFIX: &str = "flow-value-shadow-";

/// Logical pixels of breathing room around the digits in the baked canvas,
/// wide enough for the blur's tail. The digits sit right-aligned this far from
/// the canvas edge, so placing the canvas this far past the reading's own edge
/// lines the two up.
const VALUE_SHADOW_PAD: f32 = 16.0;

/// The baked canvas, in logical pixels: tall enough for the line box plus the
/// blur, and wide enough for the longest localized reading.
const VALUE_SHADOW_CANVAS: (f32, f32) = (160.0, LINE_HEIGHT + 2.0 * VALUE_SHADOW_PAD);

/// The pinned card's decoration ink: `ring` is the edge the glyph copies
/// paint. The pinned card keeps one dark scheme whatever the OS theme is, so
/// the ink only has to sit on the opposite side of its own light text; a dark
/// edge around dark glyphs would read as a bolder, smeared weight instead of
/// a halo.
#[derive(Clone, Copy)]
struct Ink {
    ring: Hsla,
}

fn shadow_ink() -> Ink {
    Ink {
        ring: neutral(0.0, 0.4),
    }
}

/// The contact shadow's ink at `alpha`, taken from the hairline's own ink so
/// the two decorations cannot drift apart.
fn contact_ink(alpha: f32) -> Hsla {
    let mut ink = shadow_ink().ring;
    ink.a = alpha;
    ink
}

/// The pinned shadow's ink, baked into the document: the image path paints
/// the SVG's own colours, so this is the fill the alpha mask carries.
const SHADOW_INK: &str = "#050505";

/// The shadow's alpha ramp, sampled for `feComponentTransfer`. The single
/// blur's raw alpha peaks near 0.11 on thin strokes, so the low end stays
/// faint while the far tail is invisible, the mid field rises steeply, and
/// the core saturates. One continuous curve, not stacked blur scales.
const SHADOW_ALPHA: &str = "0 0.006063 0.028371 0.061318 0.102928 0.152122 0.208185 0.270595 0.338945 0.41291 0.492218 0.576639 0.665974 0.760049 0.858709 0.961817 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1";

/// The bar's alpha ramp: a solid capsule reaches a higher raw alpha than a
/// glyph stroke, so this curve saturates later and keeps the visible outside
/// edge strong while the tail still fades.
const SHADOW_ALPHA_BARS: &str = "0 0 0.010978 0.02861 0.047366 0.06684 0.086852 0.107295 0.1281 0.149218 0.170612 0.192254 0.21412 0.236191 0.258451 0.280887 0.303488 0.326242 0.349142 0.372179 0.395347 0.418639 0.44205 0.465573 0.489206 0.512942 0.536779 0.560713 0.584739 0.608856 0.63306 0.657349 0.681719 0.706169 0.730696 0.755298 0.779974 0.80472 0.829536 0.854419 0.879369 0.904383 0.92946 0.954599 0.979798 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1";

/// The baked documents carry only the ambient tail; the near field belongs to
/// the view's contact copies ([`CONTACT_KERNEL`]). Keeping the group opacity
/// below half is what stops the wide Gaussian from plating a solid shadow
/// behind the glyphs.
const SHADOW_OPACITY_NAMES: f32 = 0.6;
const VALUE_SHADOW_OPACITY: f32 = 0.5;

/// The progress bar's shadow is one static document served under this path
/// and drawn behind every track. It is deliberately not part of the card's
/// baked picture: that picture is scaled as a whole while the tracks are
/// rounded per row, which pulled the lower bars' shadows below their tracks.
/// A per-track image keeps the text's resvg falloff and shares the track's
/// own layout, at any scale.
pub(crate) const BAR_SHADOW_PATH: &str = "flow-bar-shadow.svg";
/// The room around the bar in the asset, wide enough for the far blur's tail.
const BAR_SHADOW_PAD: f32 = 32.0;
const BAR_SHADOW_NEAR_SIGMA: f32 = 2.5;
const BAR_SHADOW_FAR_SIGMA: f32 = 9.0;
const BAR_SHADOW_NEAR_OPACITY: f32 = 0.72;
const BAR_SHADOW_FAR_OPACITY: f32 = 0.4;

/// The hairline border's eight directions, in device pixels.
const RING_DIRECTIONS: [(f32, f32); 8] = [
    (1.0, 0.0),
    (-1.0, 0.0),
    (0.0, 1.0),
    (0.0, -1.0),
    (0.7, 0.7),
    (0.7, -0.7),
    (-0.7, 0.7),
    (-0.7, -0.7),
];

/// The contact shadow's kernel, in device pixels: copies of the glyph's own
/// shape in dense rings two to three pixels out. The one-pixel ring above is
/// the hairline border; starting the shadow outside it keeps the glyph's
/// edge crisp. The copies stop at three pixels because a sparse fourth or
/// fifth ring shows its separate stamps as scallops; the smooth ambient
/// Gaussian carries the field past this kernel. The alphas fall with the
/// radius, so the rings compose into a falloff instead of a second border.
/// Every offset is a whole device pixel; a fractional one gives each copy a
/// different subpixel phase and the composite edge breaks into burrs.
const CONTACT_KERNEL: [(f32, f32, f32); 16] = [
    (2.0, 0.0, 0.22),
    (-2.0, 0.0, 0.22),
    (0.0, 2.0, 0.22),
    (0.0, -2.0, 0.22),
    (1.0, 1.0, 0.14),
    (1.0, -1.0, 0.14),
    (-1.0, 1.0, 0.14),
    (-1.0, -1.0, 0.14),
    (3.0, 0.0, 0.16),
    (-3.0, 0.0, 0.16),
    (0.0, 3.0, 0.16),
    (0.0, -3.0, 0.16),
    (2.0, 2.0, 0.11),
    (2.0, -2.0, 0.11),
    (-2.0, 2.0, 0.11),
    (-2.0, -2.0, 0.11),
];

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
pub(crate) const TOPMOST_BIT: i32 = 0x0000_0008;

/// The settings the widget reads from `[app_settings]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Settings {
    pub(crate) enabled: bool,
    pub(crate) show_offline: bool,
    pub(crate) max_devices: usize,
    pub(crate) display_mode: FlowIndicatorDisplayMode,
    /// The settings app's light/dark preference; the regular card follows it
    /// (and the OS under `System`), while the pinned card stays dark.
    pub(crate) appearance: openlogi_core::config::Appearance,
    /// The panel's language setting, applied to the overlay's shared catalog
    /// so the card follows the panel instead of this process's system locale.
    pub(crate) language: Option<String>,
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
            language: settings.language.clone(),
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

/// The config file's modification time, for the live-settings poll.
pub(crate) fn config_stamp() -> Option<SystemTime> {
    let path = openlogi_core::paths::config_path().ok()?;
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
}

/// The saved indicator placement.
///
/// Linux and macOS keep GPUI's logical window coordinates. Windows stores
/// physical virtual-desktop pixels and the monitor they belong to: a logical
/// point is relative to the scale of the display that produced it, so the same
/// pair means a different physical place on a 100% and a 125% display.
#[derive(Debug, Serialize, Deserialize)]
struct SavedPosition {
    x: f32,
    y: f32,
    /// A file written before pinning existed keeps the card unpinned.
    #[serde(default)]
    pinned: bool,
    /// Windows: `x`/`y` are physical virtual-desktop pixels.
    #[serde(default, skip_serializing_if = "is_false")]
    physical: bool,
    /// Windows: the GDI device name of the monitor the card was saved on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    display: Option<String>,
    /// Windows: the spot the user last left on each monitor, so switching back
    /// to a display restores that display's own place instead of clamping the
    /// other display's point onto it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    positions: Vec<SavedSpot>,
}

/// One monitor's remembered physical point.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct SavedSpot {
    display: String,
    x: f32,
    y: f32,
}

#[cfg(target_os = "windows")]
#[expect(
    clippy::cast_possible_truncation,
    reason = "a saved screen coordinate is a whole physical pixel"
)]
fn saved_origin(spot: &SavedSpot) -> Point<DevicePixels> {
    point(
        DevicePixels(spot.x.round() as i32),
        DevicePixels(spot.y.round() as i32),
    )
}

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde calls the skip predicate with a reference"
)]
const fn is_false(value: &bool) -> bool {
    !*value
}

/// The Windows card's last place, in physical virtual-desktop pixels.
#[cfg(target_os = "windows")]
#[derive(Clone, Debug, PartialEq, Eq)]
struct PhysicalPlacement {
    origin: Point<DevicePixels>,
    /// The monitor's GDI device name, when the card was last saved there.
    display: Option<String>,
}

#[cfg(target_os = "windows")]
impl SavedPosition {
    /// The physical placement this file describes, and whether it needs a
    /// first-tick rewrite.
    ///
    /// A file written before the physical space existed carries GPUI logical
    /// coordinates. Treating them as physical keeps the card where it is seen
    /// today, which is the only ground truth a migration has; the next tick
    /// writes the real physical position and monitor.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "a saved screen coordinate is a whole physical pixel"
    )]
    fn physical_placement(&self) -> (PhysicalPlacement, bool) {
        (
            PhysicalPlacement {
                origin: point(
                    DevicePixels(self.x.round() as i32),
                    DevicePixels(self.y.round() as i32),
                ),
                display: self.display.clone(),
            },
            !self.physical,
        )
    }
}

fn position_path() -> Option<PathBuf> {
    openlogi_core::paths::config_dir()
        .ok()
        .map(|dir| dir.join("flow-indicator.toml"))
}

fn load_saved() -> Option<SavedPosition> {
    let body = std::fs::read_to_string(position_path()?).ok()?;
    toml::from_str(&body).ok()
}

#[cfg(not(target_os = "windows"))]
fn load_position() -> Option<Point<gpui::Pixels>> {
    let saved = load_saved()?;
    Some(point(px(saved.x), px(saved.y)))
}

/// Whether the last session left the card pinned as a transparent HUD.
pub(crate) fn load_pinned() -> bool {
    load_saved().is_some_and(|saved| saved.pinned)
}

fn write_saved(saved: &SavedPosition) {
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
    let Ok(body) = toml::to_string(saved) else {
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

#[cfg(not(target_os = "windows"))]
fn save_position(origin: Point<gpui::Pixels>, pinned: bool) {
    write_saved(&SavedPosition {
        x: f32::from(origin.x),
        y: f32::from(origin.y),
        pinned,
        physical: false,
        display: None,
        positions: Vec::new(),
    });
}

#[cfg(target_os = "windows")]
#[expect(
    clippy::cast_precision_loss,
    reason = "physical screen coordinates are small integers, exactly representable in f32"
)]
fn save_physical_position(
    origin: Point<DevicePixels>,
    display: Option<&str>,
    pinned: bool,
    positions: &BTreeMap<String, Point<DevicePixels>>,
) {
    let mut remembered: Vec<SavedSpot> = positions
        .iter()
        .map(|(display, origin)| SavedSpot {
            display: display.clone(),
            x: origin.x.0 as f32,
            y: origin.y.0 as f32,
        })
        .collect();
    if let Some(display) = display
        && !positions.contains_key(display)
    {
        remembered.push(SavedSpot {
            display: display.to_string(),
            x: origin.x.0 as f32,
            y: origin.y.0 as f32,
        });
    }
    write_saved(&SavedPosition {
        x: origin.x.0 as f32,
        y: origin.y.0 as f32,
        pinned,
        physical: true,
        display: display.map(str::to_owned),
        positions: remembered,
    });
}

fn card_size(rows: usize) -> Size<gpui::Pixels> {
    let count = u16::try_from(rows).unwrap_or(u16::MAX);
    let rows_height = f32::from(count) * ROW_HEIGHT;
    let gaps = f32::from(count.saturating_sub(1)) * ROW_GAP;
    size(
        px(CARD_WIDTH),
        px(BORDER * 2.0 + PADDING_Y * 2.0 + HEADER_LANE + HEADER_GAP + rows_height + gaps),
    )
}

/// The window around a card, including the transparent shadow margin.
fn window_size(rows: usize) -> Size<gpui::Pixels> {
    let card = card_size(rows);
    size(
        card.width + px(SHADOW_MARGIN * 2.0),
        card.height + px(SHADOW_MARGIN * 2.0),
    )
}

fn window_origin(visible: Point<gpui::Pixels>) -> Point<gpui::Pixels> {
    point(visible.x - px(SHADOW_MARGIN), visible.y - px(SHADOW_MARGIN))
}

fn visible_origin(window: Point<gpui::Pixels>) -> Point<gpui::Pixels> {
    point(window.x + px(SHADOW_MARGIN), window.y + px(SHADOW_MARGIN))
}

/// The pin button's rectangle in the card's own logical coordinates.
///
/// The hit target is exactly the painted button: the Linux click-through cuts
/// the card's input region down to this rectangle while pinned; Windows derives
/// the same rectangle from the same constants for its native watch. Keeping the
/// two equal means the pointer and its hover feedback match what is visible.
#[cfg(any(target_os = "linux", test))]
pub(crate) fn pin_button_bounds() -> Bounds<gpui::Pixels> {
    Bounds::new(
        point(px(CARD_WIDTH - PADDING_X - HEADER_LANE), px(PADDING_Y)),
        size(px(HEADER_LANE), px(HEADER_LANE)),
    )
}

/// The pin button in window coordinates, including the shadow margin. Native
/// click-through and the Linux input region work in window coordinates.
#[cfg(any(target_os = "linux", test))]
pub(crate) fn pin_input_bounds() -> Bounds<gpui::Pixels> {
    let pin = pin_button_bounds();
    Bounds::new(
        point(
            pin.origin.x + px(SHADOW_MARGIN),
            pin.origin.y + px(SHADOW_MARGIN),
        ),
        pin.size,
    )
}

/// The visible card in window coordinates: the only interactive part while
/// regular. Linux applies it as the surface's input region, so the shadow
/// halo passes pointer input through.
#[cfg(any(target_os = "linux", test))]
pub(crate) fn card_input_bounds(rows: usize) -> Bounds<gpui::Pixels> {
    Bounds::new(point(px(SHADOW_MARGIN), px(SHADOW_MARGIN)), card_size(rows))
}

/// The rectangle the card keeps interactive, or `None` while it paints
/// nothing. A pin-swap replacement waits hidden above the old card, and a
/// pin-shaped hole there would swallow the clicks meant for the card still
/// on screen until the replacement is revealed.
#[cfg(any(target_os = "linux", test))]
fn click_through_region(visible: bool, pinned: bool, rows: usize) -> Option<Bounds<gpui::Pixels>> {
    if !visible {
        return None;
    }
    Some(if pinned {
        pin_input_bounds()
    } else {
        card_input_bounds(rows)
    })
}

/// Whether the card still has to be shown and needs a window created for it.
///
/// Windows starts the overlay headless (#2339) and GPUI refuses a window in
/// that mode, so this is the caller's cue to connect to the display before
/// feeding the owner a command that may open the card. A card with no
/// observation yet also asks: the first observation carries the inventory
/// that decides the rows, so waiting for them would make that first sync
/// open nothing.
#[expect(
    clippy::fn_params_excessive_bools,
    reason = "each flag is an independent view fact the caller already holds; the signature reads as its one-line predicate"
)]
const fn card_needs_display(
    has_window: bool,
    dismissed: bool,
    enabled: bool,
    row_count: usize,
    observed: bool,
) -> bool {
    enabled && !has_window && !dismissed && (row_count > 0 || !observed)
}

/// The bottom-right corner of `area`, inside the screen margin.
#[cfg(not(target_os = "windows"))]
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

/// The visible work areas GPUI reports, in display order.
fn display_areas(cx: &App) -> Vec<Bounds<gpui::Pixels>> {
    cx.displays()
        .iter()
        .map(|display| display.visible_bounds())
        .collect()
}

/// The display area that owns `point`, falling back to the primary display
/// when the point is in a gap or on a display that is no longer connected.
fn area_for_point(
    areas: &[Bounds<gpui::Pixels>],
    primary: Option<Bounds<gpui::Pixels>>,
    point: Point<gpui::Pixels>,
) -> Option<Bounds<gpui::Pixels>> {
    areas
        .iter()
        .find(|area| area.contains(&point))
        .copied()
        .or(primary)
}

/// Where the card should be after a row-count resize: a card sitting on the
/// bottom edge keeps that edge, and every card is kept inside its display.
fn anchored_origin(
    area: Option<Bounds<gpui::Pixels>>,
    old: Bounds<gpui::Pixels>,
    card: Size<gpui::Pixels>,
) -> Point<gpui::Pixels> {
    let Some(area) = area else {
        return old.origin;
    };
    let mut origin = old.origin;
    let old_bottom = old.origin.y + old.size.height;
    if old_bottom >= area.bottom() - px(MARGIN) {
        origin.y = old_bottom - card.height;
    }
    clamp_origin(area, card, origin)
}

/// The window origin a row-count resize should leave the card at, read from
/// the window's bounds before the resize. The returned point is the outer
/// window's, so the card's shadow margin stays around the same visible card.
fn resize_origin(
    cx: &mut App,
    handle: &WindowHandle<FlowIndicatorView>,
    card: Size<gpui::Pixels>,
) -> Option<Point<gpui::Pixels>> {
    let old = handle.update(cx, |_, window, _| window.bounds()).ok()?;
    let old_visible = Bounds::new(
        visible_origin(old.origin),
        size(
            (old.size.width - px(SHADOW_MARGIN * 2.0)).max(px(0.0)),
            (old.size.height - px(SHADOW_MARGIN * 2.0)).max(px(0.0)),
        ),
    );
    let area = area_for_point(
        &display_areas(cx),
        cx.primary_display().map(|display| display.visible_bounds()),
        old_visible.origin,
    );
    Some(window_origin(anchored_origin(area, old_visible, card)))
}

#[cfg(not(target_os = "windows"))]
fn resolve_origin(
    cx: &App,
    remembered: Option<Point<gpui::Pixels>>,
    card: Size<gpui::Pixels>,
) -> Point<gpui::Pixels> {
    let areas = display_areas(cx);
    let primary = cx.primary_display().map(|display| display.visible_bounds());
    match remembered.or_else(load_position) {
        Some(origin) => area_for_point(&areas, primary, origin)
            .map_or(origin, |area| clamp_origin(area, card, origin)),
        None => primary
            .or_else(|| areas.first().copied())
            .map_or_else(|| point(px(0.0), px(0.0)), |area| bottom_right(area, card)),
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
        kind if kind.is_keyboard_family() => KEYBOARD_ICON,
        DeviceKind::Headset => HEADSET_ICON,
        DeviceKind::Gamepad | DeviceKind::Joystick => GAMEPAD_ICON,
        DeviceKind::Camera => CAMERA_ICON,
        _ => GENERIC_ICON,
    }
}

/// The pinned card's bar track, measured from the regular card's empty track
/// (`#E1E1E1`) rather than its white-card theoretical value (`#EBEBEB`): the
/// pinned card has no white surface around the track, so the lighter value
/// reads as pure white against its shadow. It is opaque so the blurred shadow
/// behind the transparent card cannot show through the empty track.
const PINNED_TRACK: Hsla = neutral(0.882_352_94, 1.0);

struct Palette {
    card: Hsla,
    text: Hsla,
    muted: Hsla,
    track: Hsla,
    border: Hsla,
}

impl Palette {
    /// The pinned card is a transparent glyph HUD, so it keeps the dark scheme
    /// whatever the OS theme is; the regular card follows the settings app's
    /// theme so it sits with the panel it belongs to.
    fn for_dark(dark: bool) -> Self {
        if dark {
            Self {
                card: neutral(0.10, 0.94),
                text: rgb(0xD6_D6_DA).into(),
                muted: rgb(0x9A_9A_9E).into(),
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

    /// The pin button's hover/pressed chip: a point on the line between the
    /// card's surface and the glyph, opaque so the painted colour is exactly
    /// that point instead of being pulled back toward the card by alpha. Hover
    /// sits at the midpoint; pressed moves a little further toward the glyph.
    /// Both stay strictly between, so the chip can merge with neither the
    /// theme's surface nor the icon it surrounds.
    fn pin_chip(&self, pinned: bool, pressed: bool) -> Hsla {
        let glyph = if pinned { self.text } else { self.muted };
        let amount = if pressed { 0.62 } else { 0.50 };
        neutral(self.card.l + (glyph.l - self.card.l) * amount, 1.0)
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

/// A short muted label in the card's glyph language: eight device-pixel ring
/// copies in the pinned state, a plain label otherwise. These labels are
/// smaller than the row text, so they keep the hairline without the full
/// contact kernel; a four-pixel halo would be as tall as their cap height.
fn ringed_text(
    text: impl Into<SharedString>,
    size: f32,
    color: Hsla,
    pinned: bool,
    ink: Hsla,
    scale: f32,
) -> Div {
    let text = text.into();
    let mut label = div()
        .relative()
        .flex_none()
        .whitespace_nowrap()
        .text_size(px(size))
        .text_color(color);
    if pinned {
        label = label.children(RING_DIRECTIONS.map(|(dx, dy)| {
            div()
                .absolute()
                .left(px(dx / scale))
                .top(px(dy / scale))
                .whitespace_nowrap()
                .text_size(px(size))
                .text_color(ink)
                .child(text.clone())
        }));
    }
    label.child(text)
}

/// A row's status word (the offline tag) in the row's own type scale: the
/// name's size, weight, contact kernel and hairline, so every glyph inside a
/// row stands on one shadow language. Its ambient tail is baked into the
/// card's shadow document ([`shadow_svg`]), exactly like the name's.
fn status_text(
    text: impl Into<SharedString>,
    color: Hsla,
    pinned: bool,
    ink: Hsla,
    scale: f32,
) -> Div {
    let text = text.into();
    let mut label = div()
        .relative()
        .flex_none()
        .whitespace_nowrap()
        .text_size(px(NAME_TEXT_SIZE))
        .font_weight(NAME_FONT_WEIGHT)
        .text_color(color);
    if pinned {
        label = label.children(CONTACT_KERNEL.map(|(dx, dy, alpha)| {
            div()
                .absolute()
                .left(px(dx / scale))
                .top(px(dy / scale))
                .whitespace_nowrap()
                .text_size(px(NAME_TEXT_SIZE))
                .font_weight(NAME_FONT_WEIGHT)
                .text_color(contact_ink(alpha))
                .child(text.clone())
        }));
        label = label.children(RING_DIRECTIONS.map(|(dx, dy)| {
            div()
                .absolute()
                .left(px(dx / scale))
                .top(px(dy / scale))
                .whitespace_nowrap()
                .text_size(px(NAME_TEXT_SIZE))
                .font_weight(NAME_FONT_WEIGHT)
                .text_color(ink)
                .child(text.clone())
        }));
    }
    label.child(text)
}

/// The pin's hover hint in the pin button's own chip: the same 20 px lane,
/// 6 px radius, and [`Palette::pin_chip`] background as the button's hover
/// container, with the card's text colour at the pin glyph's size.
fn hint_capsule(text: impl Into<SharedString>, pinned: bool, pal: &Palette) -> Div {
    div()
        .flex_none()
        .h(px(HEADER_LANE))
        .px(px(8.0))
        .flex()
        .items_center()
        .rounded(px(6.0))
        .bg(pal.pin_chip(pinned, false))
        .text_size(px(HEADER_HINT_TEXT_SIZE))
        .font_weight(NAME_FONT_WEIGHT)
        .line_height(px(HEADER_LANE))
        .text_color(pal.text)
        .child(text.into())
}

/// The ambient tail of the pinned card's shadow, as a generated SVG asset:
/// the device names laid out where the card draws them, run through
/// `feGaussianBlur`. resvg rasterises the filter into the document's own
/// near-black ink, and the card paints it through the image path so GPUI
/// keeps that colour instead of tinting a monochrome mask — a true Gaussian
/// glow at any radius, where stamped glyph copies would show their spacing.
///
/// This document is no longer the whole shadow. At [`SHADOW_OPACITY_NAMES`]
/// it only carries the faint far field; the near field is the view's
/// [`CONTACT_KERNEL`] copies, which are stamped from GPUI's own glyphs and
/// therefore sit on the painted edge by construction. A second renderer's
/// silhouette cannot promise that, so it must not carry the contact.
///
/// The readings stay out of this document: its hash covers the whole row set,
/// so every combination of values would mint an asset. They get one baked
/// asset per reading string instead ([`VALUE_SHADOW_PREFIX`]). The pinned
/// card keeps one dark scheme, so both OS themes share one baked ink.
///
/// The names are text in this source form, and GPUI's SVG renderer builds the
/// whole system font database on the UI thread when it first paints a
/// `<text>`. The card bakes those names into paths on a background thread
/// instead ([`bake_shadow_svg`]), so the first pin pays neither the scan nor
/// the text layout.
pub(crate) const SHADOW_ASSET_PREFIX: &str = "flow-shadow-";

/// The baked shadow the asset source serves, keyed by the rows it describes.
#[derive(Default)]
struct ShadowAsset {
    /// The path the pinned card renders; `None` while no bake for the current
    /// rows has finished. Keeping it empty until the body exists means GPUI
    /// never caches a missing asset under a path we later reuse.
    path: Option<SharedString>,
    /// The baked SVG behind `path`.
    body: String,
    /// The rows hash the current bake belongs to.
    rows: Option<u64>,
}

static SHADOW_ASSET: Mutex<ShadowAsset> = Mutex::new(ShadowAsset {
    path: None,
    body: String::new(),
    rows: None,
});

/// The static bar shadow's document: the track's capsule on the same
/// two-scale resvg falloff as the text, with enough padding for the far
/// blur's tail. One document serves every track.
fn bar_shadow_source() -> &'static str {
    static SOURCE: OnceLock<String> = OnceLock::new();
    SOURCE.get_or_init(|| {
        let width = CARD_WIDTH - PADDING_X * 2.0;
        let view_width = width + BAR_SHADOW_PAD * 2.0;
        let view_height = BAR_HEIGHT + BAR_SHADOW_PAD * 2.0;
        let bar = format!(
            r#"<rect x="{}" y="{}" width="{}" height="{}" rx="{}"/>"#,
            BAR_SHADOW_PAD,
            BAR_SHADOW_PAD,
            width,
            BAR_HEIGHT,
            BAR_HEIGHT / 2.0
        );
        format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="{half_width}" height="{half_height}" viewBox="0 0 {view_width} {view_height}"><defs><filter id="near" filterUnits="userSpaceOnUse" x="0" y="0" width="{view_width}" height="{view_height}"><feGaussianBlur stdDeviation="{near_sigma}"/><feComponentTransfer><feFuncA type="table" tableValues="{alpha}"/></feComponentTransfer></filter><filter id="far" filterUnits="userSpaceOnUse" x="0" y="0" width="{view_width}" height="{view_height}"><feGaussianBlur stdDeviation="{far_sigma}"/><feComponentTransfer><feFuncA type="table" tableValues="{alpha}"/></feComponentTransfer></filter></defs><g filter="url(#far)" fill="{fill}" opacity="{far_opacity}">{bar}</g><g filter="url(#near)" fill="{fill}" opacity="{near_opacity}">{bar}</g></svg>"#,
            half_width = view_width / 2.0,
            half_height = view_height / 2.0,
            near_sigma = BAR_SHADOW_NEAR_SIGMA,
            far_sigma = BAR_SHADOW_FAR_SIGMA,
            alpha = SHADOW_ALPHA_BARS,
            fill = SHADOW_INK,
            far_opacity = BAR_SHADOW_FAR_OPACITY,
            near_opacity = BAR_SHADOW_NEAR_OPACITY,
            bar = bar,
        )
    })
}

/// Whether [`take_shadow_asset`] serves this path; the asset source asks
/// before handing anything to GPUI's own loader.
pub(crate) fn is_shadow_asset(path: &str) -> bool {
    path == BAR_SHADOW_PATH
        || path.starts_with(SHADOW_ASSET_PREFIX)
        || path.starts_with(VALUE_SHADOW_PREFIX)
}

/// Serve a generated shadow to the asset source for the path the view
/// announced — the whole card's document, one reading's, or the bar's.
pub(crate) fn take_shadow_asset(path: &str) -> Option<String> {
    if path == BAR_SHADOW_PATH {
        return Some(bar_shadow_source().to_owned());
    }
    if let Some(rest) = path.strip_prefix(VALUE_SHADOW_PREFIX) {
        let hash = u64::from_str_radix(rest.strip_suffix(".svg")?, 16).ok()?;
        return value_shadows().lock().ok()?.ready.get(&hash).cloned();
    }
    path.strip_prefix(SHADOW_ASSET_PREFIX)?;
    let asset = SHADOW_ASSET.lock().ok()?;
    (asset.path.as_deref() == Some(path)).then(|| asset.body.clone())
}

/// The path of the baked shadow, once it exists.
fn shadow_asset_path() -> Option<SharedString> {
    SHADOW_ASSET.lock().ok()?.path.clone()
}

/// Claim a bake for `rows`; false when those rows are already baked or being
/// baked.
fn claim_shadow_bake(rows: u64) -> bool {
    let Ok(mut asset) = SHADOW_ASSET.lock() else {
        return false;
    };
    if asset.rows == Some(rows) {
        return false;
    }
    asset.rows = Some(rows);
    asset.path = None;
    asset.body.clear();
    true
}

/// Let a failed bake be retried by the next row change or pin click.
fn fail_shadow_bake(rows: u64) {
    let Ok(mut asset) = SHADOW_ASSET.lock() else {
        return;
    };
    if asset.rows == Some(rows) {
        asset.rows = None;
    }
}

/// Publish a finished bake. A newer row set may have superseded it.
fn finish_shadow_bake(rows: u64, baked: String) {
    let Ok(mut asset) = SHADOW_ASSET.lock() else {
        return;
    };
    if asset.rows != Some(rows) {
        return;
    }
    asset.path = Some(SharedString::from(format!(
        "{SHADOW_ASSET_PREFIX}{rows:016x}.svg"
    )));
    asset.body = baked;
}

/// The system font database the bake resolves text with. Built once, on the
/// background thread; it never touches GPUI's own SVG renderer.
fn font_database() -> &'static Arc<usvg::fontdb::Database> {
    static DATABASE: OnceLock<Arc<usvg::fontdb::Database>> = OnceLock::new();
    DATABASE.get_or_init(|| {
        let mut database = usvg::fontdb::Database::new();
        database.load_system_fonts();
        Arc::new(database)
    })
}

/// Turn the generated shadow's `<text>` into paths so GPUI's SVG renderer
/// never has to resolve a font.
fn bake_shadow_svg(raw: &str) -> Result<String, String> {
    let default_selector = usvg::FontResolver::default_font_selector();
    let options = usvg::Options {
        fontdb: Arc::clone(font_database()),
        font_resolver: usvg::FontResolver {
            // The document names the family GPUI resolved ([`shadow_font_family`]),
            // but that is virtual on Windows and macOS and the chain then
            // falls back to Segoe UI, which only Windows has. Without this
            // last resort usvg drops the names on Linux and the glow loses
            // its text.
            select_font: Box::new(move |font, db| {
                if let Some(id) = default_selector(font, db) {
                    return Some(id);
                }
                let sans = usvg::fontdb::Query {
                    families: &[usvg::fontdb::Family::SansSerif],
                    ..Default::default()
                };
                db.query(&sans)
                    .or_else(|| db.faces().next().map(|face| face.id))
            }),
            select_fallback: usvg::FontResolver::default_fallback_selector(),
        },
        ..usvg::Options::default()
    };
    let tree = usvg::Tree::from_str(raw, &options).map_err(|error| error.to_string())?;
    // `WriteOptions` converts text to paths unless `preserve_text` is set.
    Ok(tree.to_string(&usvg::WriteOptions::default()))
}

fn escape_xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The baked reading shadows the asset source serves, and the bakes still in
/// flight. Keyed by the reading alone, so two rows showing the same value
/// share one raster.
#[derive(Default)]
struct ValueShadows {
    ready: HashMap<u64, String>,
    pending: HashSet<u64>,
}

static VALUE_SHADOWS: OnceLock<Mutex<ValueShadows>> = OnceLock::new();

fn value_shadows() -> &'static Mutex<ValueShadows> {
    VALUE_SHADOWS.get_or_init(|| Mutex::new(ValueShadows::default()))
}

/// The hash one reading's shadow asset is cached under.
fn value_shadow_hash(value: &str) -> u64 {
    let mut hasher = std::hash::DefaultHasher::new();
    hasher.write(value.as_bytes());
    hasher.finish()
}

fn value_shadow_asset_path(hash: u64) -> String {
    format!("{VALUE_SHADOW_PREFIX}{hash:016x}.svg")
}

/// The baked asset for one reading, once its bake has landed.
fn value_shadow_path(value: &str) -> Option<SharedString> {
    let hash = value_shadow_hash(value);
    value_shadows()
        .lock()
        .ok()?
        .ready
        .contains_key(&hash)
        .then(|| SharedString::from(value_shadow_asset_path(hash)))
}

/// The family the baked shadows name, from GPUI's own text system: the card
/// paints `.SystemUIFont`, which the host resolves to a concrete family (the
/// fallback stack's pick on Linux). A second font stack's `sans-serif` is not
/// that family — on a host without the named face it lands on an arbitrary
/// database entry, and its glyphs and metrics drift away from the text.
///
/// Windows and macOS resolve the request to a virtual name (`.SystemUIFont`,
/// `.AppleSystemUIFont`) that a font database cannot match; the document
/// keeps naming Segoe UI there, and only the baseline comes from GPUI.
fn shadow_font_family(text_system: &WindowTextSystem) -> SharedString {
    let font_id = text_system.resolve_font(&font(".SystemUIFont"));
    text_system
        .get_font_for_id(font_id)
        .map(|font| font.family)
        .filter(|family| !family.starts_with('.'))
        .unwrap_or_else(|| SharedString::from("Segoe UI"))
}

/// The weight a baked shadow must ask for to wear the face the card paints.
///
/// DirectWrite resolves the card's MEDIUM names against Segoe UI's static
/// family to Semibold (400 and 600 are equally close, and it takes the
/// heavier), while usvg's fontdb follows CSS and lands on Regular. A 500 bake
/// then shapes a narrower, thinner line that stands inside the painted glyphs
/// and stops short of the name's right end. The readings ask for SEMIBOLD
/// already, so only the names move.
fn shadow_weight(weight: FontWeight) -> f32 {
    #[cfg(target_os = "windows")]
    let weight = if weight == FontWeight::MEDIUM {
        FontWeight::SEMIBOLD
    } else {
        weight
    };
    weight.0
}

/// The paragraph separators GPUI's single-line shaper rejects.
fn is_line_break(c: char) -> bool {
    c.is_control() || c == '\u{2029}'
}

/// The card paints one line; control characters would make GPUI's shaper
/// reject the paragraph, so they become spaces before measuring or baking.
fn single_line_text(text: &str) -> String {
    if text.chars().any(is_line_break) {
        text.chars()
            .map(|c| if is_line_break(c) { ' ' } else { c })
            .collect()
    } else {
        text.to_owned()
    }
}

/// The width of `text` as GPUI shapes it, in logical pixels.
fn shaped_text_width(
    text_system: &WindowTextSystem,
    text: &str,
    size: f32,
    weight: FontWeight,
) -> f32 {
    let mut font = font(".SystemUIFont");
    font.weight = weight;
    let run = TextRun {
        len: text.len(),
        font,
        color: transparent_black(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let line = text_system.shape_line(text.into(), px(size), &[run], None);
    f32::from(line.width())
}

/// The name cell's width before truncation, mirroring the row's flex layout:
/// the icon lane, the gaps, any charging bolt or offline tag, and the reading's
/// own lane. The card is one fixed width, so this is the whole budget the name
/// has; the painted text and the baked shadow both truncate to it.
fn name_cell_width(text_system: &WindowTextSystem, row: &Row) -> f32 {
    let mut extras = 0.0;
    let mut extra_cells = 0.0;
    if row.online && row.charging {
        extras += BOLT_SIZE;
        extra_cells += 1.0;
    }
    if !row.online {
        extras += shaped_text_width(
            text_system,
            rust_i18n::t!("device.offline").as_ref(),
            NAME_TEXT_SIZE,
            NAME_FONT_WEIGHT,
        );
        extra_cells += 1.0;
    }
    let value = shaped_text_width(text_system, &row.value, VALUE_TEXT_SIZE, VALUE_FONT_WEIGHT)
        .max(VALUE_LANE);
    let gaps = ROW_INNER_GAP * (2.0 + extra_cells);
    CARD_WIDTH - PADDING_X * 2.0 - ICON_LANE - extras - value - gaps
}

/// The name exactly as the card paints it: the full string when it fits, or
/// GPUI's own end truncation (the same ellipsis the name cell would draw)
/// otherwise. The baked shadow uses this string, so its glyphs cannot outlive
/// the visible name.
fn painted_name(text_system: &WindowTextSystem, row: &Row) -> String {
    let text = single_line_text(&row.name);
    let width = name_cell_width(text_system, row);
    let mut font = font(".SystemUIFont");
    font.weight = NAME_FONT_WEIGHT;
    let run = TextRun {
        len: text.len(),
        font: font.clone(),
        color: transparent_black(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let shaped = text_system.shape_line(
        text.clone().into(),
        px(NAME_TEXT_SIZE),
        std::slice::from_ref(&run),
        None,
    );
    if f32::from(shaped.width()) <= width {
        return text;
    }
    // Half a pixel of headroom keeps a rounding difference from making the
    // cell truncate the already-truncated string a second time.
    let budget = (width - 0.5).max(0.0);
    let mut wrapper = text_system.line_wrapper(font, px(NAME_TEXT_SIZE));
    let (truncated, _) =
        wrapper.truncate_line(text.into(), px(budget), "…", &[run], TruncateFrom::End);
    truncated.to_string()
}

/// Where GPUI paints `text`'s baseline inside a [`LINE_HEIGHT`] line, measured
/// from the line box top.
///
/// Both the reading and its shadow must stand on this line. GPUI centres the
/// shaped ascent/descent pair; a baked `<text>` left to
/// `dominant-baseline="central"` instead centres whatever face its own font
/// database resolved, which is a few pixels off whenever the two disagree.
fn line_baseline(text_system: &WindowTextSystem, text: &str, size: f32, weight: FontWeight) -> f32 {
    let single_line = single_line_text(text);
    let mut font = font(".SystemUIFont");
    font.weight = weight;
    let run = TextRun {
        len: single_line.len(),
        font,
        color: transparent_black(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let line = text_system.shape_line(single_line.into(), px(size), &[run], None);
    let ascent = f32::from(line.ascent);
    let descent = f32::from(line.descent);
    (LINE_HEIGHT - ascent - descent) / 2.0 + ascent
}

/// The status glyphs one row paints beside its name, where their cells start:
/// the offline word and/or the charging bolt. They are `flex_none` cells
/// after the name, so their x is the name cell's right edge, and the name
/// cell's width is [`name_cell_width`]'s budget.
#[derive(Clone, Debug, PartialEq)]
struct StatusGlyph {
    /// The localized offline word; `None` for an online row.
    word: Option<String>,
    /// The word's left edge, in card coordinates.
    word_x: f32,
    /// [`line_baseline`]'s offset for the word, from the line box top.
    word_baseline: f32,
    /// The charging bolt's box left edge; `None` when the row paints no bolt.
    bolt_x: Option<f32>,
}

/// Lay out `row`'s status glyphs where the row paints them. Both the painted
/// decoration and the baked ambient read this one placement. `frozen` is the
/// card's stale state, which hides the charging bolt even on an online row.
fn status_glyph(text_system: &WindowTextSystem, row: &Row, frozen: bool) -> StatusGlyph {
    let word = (!row.online).then(|| rust_i18n::t!("device.offline").to_string());
    let word_x =
        PADDING_X + ICON_LANE + ROW_INNER_GAP + name_cell_width(text_system, row) + ROW_INNER_GAP;
    StatusGlyph {
        word_baseline: word.as_deref().map_or(0.0, |word| {
            line_baseline(text_system, word, NAME_TEXT_SIZE, NAME_FONT_WEIGHT)
        }),
        word,
        word_x,
        bolt_x: (row.online && row.charging && !frozen).then_some(word_x),
    }
}

/// The text-bearing source of one reading's ambient shadow. It paints at
/// [`VALUE_SHADOW_OPACITY`] because the near field is the view's
/// [`CONTACT_KERNEL`] copies; the far radius is smaller than the names'
/// because this canvas only has room for a tighter tail.
///
/// `baseline` is [`line_baseline`]'s offset from the line box top; the canvas
/// starts [`VALUE_SHADOW_PAD`] above that box, so the digits stand exactly on
/// the reading's baseline.
fn value_shadow_source(value: &str, baseline: f32, family: &str) -> String {
    let (width, height) = VALUE_SHADOW_CANVAS;
    let text = format!(
        r#"<text x="{}" y="{}" text-anchor="end" font-family="'{}', sans-serif" font-size="{}" font-weight="{}" xml:space="preserve">{}</text>"#,
        width - VALUE_SHADOW_PAD,
        VALUE_SHADOW_PAD + baseline,
        escape_xml(family),
        VALUE_TEXT_SIZE,
        shadow_weight(VALUE_FONT_WEIGHT),
        escape_xml(value),
    );
    // One blurred mask, shaped by a continuous alpha ramp. A single blur
    // keeps the falloff smooth; the transfer curve carries the density a
    // stack of blur scales used to approximate.
    format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{half_width}" height="{half_height}" viewBox="0 0 {width} {height}"><defs><filter id="shadow" filterUnits="userSpaceOnUse" x="-30" y="-30" width="{filter_width}" height="{filter_height}"><feGaussianBlur stdDeviation="8"/><feComponentTransfer><feFuncA type="table" tableValues="{alpha}"/></feComponentTransfer></filter></defs><g filter="url(#shadow)" fill="{fill}" opacity="{opacity}">{text}</g></svg>"#,
        fill = SHADOW_INK,
        alpha = SHADOW_ALPHA,
        opacity = VALUE_SHADOW_OPACITY,
        half_width = width / 2.0,
        half_height = height / 2.0,
        filter_width = width + 60.0,
        filter_height = height + 60.0,
    )
}

/// Claim a bake for one reading; false while it is already baked or baking.
fn claim_value_bake(hash: u64) -> bool {
    let Ok(mut shadows) = value_shadows().lock() else {
        return false;
    };
    !shadows.ready.contains_key(&hash) && shadows.pending.insert(hash)
}

/// Let a failed bake be retried by the next observation or pin click.
fn fail_value_bake(hash: u64) {
    if let Ok(mut shadows) = value_shadows().lock() {
        shadows.pending.remove(&hash);
    }
}

/// Publish a finished reading shadow.
fn finish_value_bake(hash: u64, baked: String) {
    let Ok(mut shadows) = value_shadows().lock() else {
        return;
    };
    shadows.pending.remove(&hash);
    shadows.ready.insert(hash, baked);
}

/// The hash the card's shadow raster is cached under: the row set the
/// document describes, with the names and status words exactly as they will
/// be painted, and the status cells' x — a wider reading lane moves them, so
/// the x belongs in the key even when the words do not change. Baselines and
/// the family are host constants, so they stay out of the key.
fn shadow_rows_hash(names: &[String], rows: &[Row], statuses: &[StatusGlyph]) -> u64 {
    let mut hasher = std::hash::DefaultHasher::new();
    for ((name, row), status) in names.iter().zip(rows).zip(statuses) {
        hasher.write(name.as_bytes());
        hasher.write(kind_icon(row.kind).as_bytes());
        hasher.write(&[u8::from(row.charging), u8::from(row.online)]);
        hasher.write(status.word.as_deref().unwrap_or_default().as_bytes());
        if status.word.is_some() || status.bolt_x.is_some() {
            hasher.write_u32((status.word_x * 8.0).round().to_bits());
        }
        hasher.write_u8(u8::from(status.bolt_x.is_some()));
    }
    hasher.finish()
}

/// Build the (text-bearing) ambient SVG for `rows`, with each name and status
/// glyph on the baseline and x the card paints it at (`baselines` and
/// `statuses`, aligned with `rows`). The near field is not here: the card
/// stamps [`CONTACT_KERNEL`] copies of its own glyphs, so this document only
/// has to carry the faint Gaussian tail.
///
/// The progress bars are deliberately absent. They carry a GPUI box shadow on
/// the track itself ([`BAR_SHADOW`]); a baked bar would be scaled with this
/// picture while the track is rounded per row, and at fractional scales the
/// lower bars' shadows drifted below their tracks.
#[expect(
    clippy::cast_precision_loss,
    reason = "row counts are single digits; the conversion is exact"
)]
fn shadow_svg(
    rows: &[Row],
    names: &[String],
    baselines: &[f32],
    statuses: &[StatusGlyph],
    family: &str,
) -> String {
    use std::fmt::Write as _;

    let mut texts = String::new();
    for (index, row) in rows.iter().enumerate() {
        let line_top = PADDING_Y + HEADER_LANE + HEADER_GAP + index as f32 * (ROW_HEIGHT + ROW_GAP);
        let name = escape_xml(&names[index]);
        let _ = write!(
            texts,
            r#"<text x="{}" y="{}" font-family="'{}', sans-serif" font-size="{}" font-weight="{}" xml:space="preserve">{}</text>"#,
            PADDING_X + ICON_LANE + ROW_INNER_GAP,
            line_top + baselines[index],
            escape_xml(family),
            NAME_TEXT_SIZE,
            shadow_weight(NAME_FONT_WEIGHT),
            name
        );
        // The kind glyph: its vendored body, scaled from its 24px view box
        // into the 18px lane and inheriting the stock stroke attributes.
        let icon = kind_icon(row.kind);
        if let Some(body) = icon_body(icon) {
            let _ = write!(
                texts,
                r#"<g transform="translate({} {}) scale(0.75)" stroke="black" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" fill="none">{}</g>"#,
                PADDING_X + (ICON_LANE - ICON_SIZE) / 2.0,
                line_top + (LINE_HEIGHT - ICON_SIZE) / 2.0,
                body
            );
        }
        let status = &statuses[index];
        // The offline word wears the name's face and baseline, so the two
        // glyphs of one row cannot drift apart in the same blur.
        if let Some(word) = &status.word {
            let _ = write!(
                texts,
                r#"<text x="{}" y="{}" font-family="'{}', sans-serif" font-size="{}" font-weight="{}" xml:space="preserve">{}</text>"#,
                status.word_x,
                line_top + status.word_baseline,
                escape_xml(family),
                NAME_TEXT_SIZE,
                shadow_weight(NAME_FONT_WEIGHT),
                escape_xml(word)
            );
        }
        // The bolt's file draws a 7..39 box on a 24 px canvas; mapping that
        // box into BOLT_SIZE keeps the baked outline on the painted glyph.
        if status.bolt_x.is_some()
            && let Some(body) = icon_body(BOLT_ICON)
        {
            let _ = write!(
                texts,
                r#"<g transform="translate({} {}) scale({}) translate(-7 -7)" fill="black">{}</g>"#,
                status.bolt_x.unwrap_or(0.0),
                line_top + (LINE_HEIGHT - BOLT_SIZE) / 2.0,
                BOLT_SIZE / 32.0,
                body
            );
        }
    }
    // One ambient mask for the row glyphs. The filter region is user-space so
    // a one-row card's short bounding box cannot cut the blur before the
    // viewport does.
    let height = f32::from(card_size(rows.len()).height);
    format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{half_width}" height="{half_height}" viewBox="0 0 {width} {height}"><defs><filter id="names" filterUnits="userSpaceOnUse" x="-48" y="-48" width="{names_width}" height="{names_height}"><feGaussianBlur stdDeviation="12"/><feComponentTransfer><feFuncA type="table" tableValues="{alpha}"/></feComponentTransfer></filter></defs><g filter="url(#names)" fill="{fill}" opacity="{names_opacity}">{names}</g></svg>"#,
        width = CARD_WIDTH,
        height = height,
        half_width = CARD_WIDTH / 2.0,
        half_height = height / 2.0,
        fill = SHADOW_INK,
        alpha = SHADOW_ALPHA,
        names_opacity = SHADOW_OPACITY_NAMES,
        names_width = CARD_WIDTH + 96.0,
        names_height = height + 96.0,
        names = texts,
    )
}

/// What the widget should show: disabled, empty, or a row set.
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent view flags: frozen readings, visibility, placement ownership, and pin hover"
)]
struct FlowIndicatorView {
    rows: Vec<Row>,
    /// Each row's name exactly as the card paints it: the full string when it
    /// fits, GPUI's ellipsis truncation otherwise. The visible text and the
    /// baked shadow read the same string, so the glow cannot outlive the
    /// painted name.
    painted_names: Vec<String>,
    /// Matching devices the card's row limit hid; shown as `+N` in the header.
    hidden: usize,
    /// The agent connection has been gone past the grace period: the readings
    /// are frozen and the card says so instead of presenting them as live.
    stale: bool,
    /// The desktop's mode, watched because the window's own report can be
    /// stuck at Light for a session started before the settings portal.
    appearance: Arc<Appearance>,
    /// Repaints on the platform's own appearance change; the watched mode
    /// repaints through the tick instead.
    appearance_subscription: Option<Subscription>,
    /// False while settings hide the card: render nothing. The window itself
    /// is shrunk to a transparent 1x1 on Wayland and hidden natively on
    /// Windows, so its placement survives the toggle.
    visible: bool,
    /// The panel's light/dark preference, watched so the regular card follows
    /// the settings app's theme over the OS's; `System` defers to the platform.
    appearance_preference: openlogi_core::config::Appearance,
    /// The pin toggle's state, shared with the owner: the view's button writes
    /// it, and a window reopen (a display-mode change) keeps the choice.
    pinned: Rc<Cell<bool>>,
    /// Whether the pointer is over the pin: the header shows the action label
    /// in the empty lane left of the pin, in the card's own text style.
    ///
    /// Tracked from the pin's own move/exit events rather than GPUI's
    /// `on_hover`: that listener re-checks itself at paint time against the
    /// last position sampled inside the window, so the frame drawn right
    /// after `MouseExited` hovers the pin again and the label never leaves.
    pin_hovered: bool,
    /// The settings' display mode — unpinning returns the card to it, and a
    /// settings mode of Topmost makes the pin toggle leave the bit alone.
    mode: FlowIndicatorDisplayMode,
    /// Whether this view owns the real placement file. GPUI tests construct
    /// the view without one, so a pin click there must not write the user's
    /// placement.
    saves_placement: bool,
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
    #[cfg(test)]
    fn new(
        rows: Vec<Row>,
        hidden: usize,
        appearance: Arc<Appearance>,
        pinned: Rc<Cell<bool>>,
    ) -> Self {
        Self::new_with_visible(rows, hidden, appearance, pinned, true)
    }

    /// A view that can start transparent: the pin swap opens the replacement
    /// hidden so the old card stays visible until the new one is in place.
    fn new_with_visible(
        rows: Vec<Row>,
        hidden: usize,
        appearance: Arc<Appearance>,
        pinned: Rc<Cell<bool>>,
        visible: bool,
    ) -> Self {
        Self {
            rows,
            painted_names: Vec::new(),
            hidden,
            stale: false,
            appearance,
            appearance_subscription: None,
            visible,
            appearance_preference: openlogi_core::config::Appearance::System,
            pinned,
            pin_hovered: false,
            mode: FlowIndicatorDisplayMode::Normal,
            saves_placement: false,
            #[cfg(target_os = "linux")]
            x11: None,
            #[cfg(target_os = "linux")]
            manual_drag: None,
            #[cfg(target_os = "linux")]
            pending_place: None,
        }
    }

    /// Repaint when the platform reports an appearance change (the live path
    /// on macOS and Windows).
    fn observe_appearance(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.appearance_subscription =
            Some(cx.observe_window_appearance(window, |_, _, cx| cx.notify()));
    }

    /// The watched mode wins once the desktop has reported one; until then
    /// (and on platforms without a watcher) the window's own appearance is
    /// the best answer available.
    fn dark_mode(&self, window: &Window) -> bool {
        Self::resolve_dark(
            self.appearance_preference,
            self.appearance.is_dark(),
            matches!(
                window.appearance(),
                WindowAppearance::Dark | WindowAppearance::VibrantDark
            ),
        )
    }

    /// The scheme decision, kept pure so the preference, the watched desktop
    /// mode and the window's own report can be exercised apart.
    fn resolve_dark(
        preference: openlogi_core::config::Appearance,
        watched: Option<bool>,
        window_dark: bool,
    ) -> bool {
        match preference {
            openlogi_core::config::Appearance::Light => false,
            openlogi_core::config::Appearance::Dark => true,
            openlogi_core::config::Appearance::System => watched.unwrap_or(window_dark),
        }
    }

    /// Bake the pinned card's shadow for the current rows off the UI thread.
    ///
    /// The generated SVG carries the names and status glyphs as `<text>`;
    /// GPUI's SVG renderer would build the system font database on the UI
    /// thread the first time it paints one, which is exactly the frame the pin
    /// click waits for. The pinned card renders its other decorations
    /// immediately and picks the shadow up when the bake lands.
    ///
    /// The family, each name's baseline, and the status cells' placement are
    /// read from the window's own text system before the bake leaves this
    /// thread: the document only carries glyphs, but where it stands them has
    /// to be GPUI's answer.
    fn refresh_shadow(&mut self, window: &Window, cx: &mut Context<Self>) {
        let painted = self
            .rows
            .iter()
            .map(|row| painted_name(window.text_system(), row))
            .collect::<Vec<_>>();
        self.painted_names.clone_from(&painted);
        let statuses = self
            .rows
            .iter()
            .map(|row| status_glyph(window.text_system(), row, self.stale))
            .collect::<Vec<_>>();
        let rows = shadow_rows_hash(&painted, &self.rows, &statuses);
        if !claim_shadow_bake(rows) {
            return;
        }
        let family = shadow_font_family(window.text_system());
        let baselines = painted
            .iter()
            .map(|name| line_baseline(window.text_system(), name, NAME_TEXT_SIZE, NAME_FONT_WEIGHT))
            .collect::<Vec<_>>();
        let raw = shadow_svg(&self.rows, &painted, &baselines, &statuses, &family);
        let bake = cx
            .background_executor()
            .spawn(async move { bake_shadow_svg(&raw) });
        cx.spawn(async move |view, cx| match bake.await {
            Ok(baked) => {
                finish_shadow_bake(rows, baked);
                view.update(cx, |_, cx| cx.notify()).ok();
            }
            Err(error) => {
                fail_shadow_bake(rows);
                warn!(%error, "Flow Indicator: could not bake its shadow");
            }
        })
        .detach();
    }

    /// Bake the shadow of every reading the pinned card shows. Keyed by the
    /// reading string, so a value is baked once and shared by every row; each
    /// bake stands on the baseline GPUI paints that reading on.
    fn refresh_value_shadows(&self, window: &Window, cx: &mut Context<Self>) {
        if !self.pinned.get() {
            return;
        }
        let family = shadow_font_family(window.text_system());
        for row in &self.rows {
            let hash = value_shadow_hash(&row.value);
            if !claim_value_bake(hash) {
                continue;
            }
            let baseline = line_baseline(
                window.text_system(),
                &row.value,
                VALUE_TEXT_SIZE,
                VALUE_FONT_WEIGHT,
            );
            let raw = value_shadow_source(&row.value, baseline, &family);
            let bake = cx
                .background_executor()
                .spawn(async move { bake_shadow_svg(&raw) });
            cx.spawn(async move |view, cx| match bake.await {
                Ok(baked) => {
                    finish_value_bake(hash, baked);
                    view.update(cx, |_, cx| cx.notify()).ok();
                }
                Err(error) => {
                    fail_value_bake(hash);
                    warn!(%error, "Flow Indicator: could not bake a reading shadow");
                }
            })
            .detach();
        }
    }

    fn row_view(
        row: &Row,
        name: &str,
        pal: &Palette,
        pinned: bool,
        scale: f32,
        frozen: bool,
    ) -> impl IntoElement + use<> {
        Self::row_view_with(row, name, pal, pinned, scale, true, frozen)
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one row's whole element tree reads top to bottom"
    )]
    fn row_view_with(
        row: &Row,
        name: &str,
        pal: &Palette,
        pinned: bool,
        scale: f32,
        value_shadow: bool,
        frozen: bool,
    ) -> impl IntoElement + use<> {
        // A sleeping device's last reading is context, not live charge: mute
        // the reading and its bar while the name stays identifiable. A stale
        // agent freezes every reading, so the whole card takes that language.
        let live = row.online && !frozen;
        let color = if live {
            color_paint(row.color, pal)
        } else {
            pal.muted
        };
        let ink = shadow_ink();
        let track_bg = if pinned { PINNED_TRACK } else { pal.track };
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
                    .gap(px(ROW_INNER_GAP))
                    .h(px(LINE_HEIGHT))
                    .line_height(px(LINE_HEIGHT))
                    .child({
                        // The glyph centres in its lane and in the line, so it
                        // sits on the same horizontal as the texts; the ring
                        // copies ride the glyph's own box.
                        let icon = kind_icon(row.kind);
                        let mut icon_box = div().relative().w(px(ICON_SIZE)).h(px(ICON_SIZE));
                        if pinned {
                            icon_box = icon_box.children(CONTACT_KERNEL.map(|(dx, dy, alpha)| {
                                svg()
                                    .path(icon)
                                    .size(px(ICON_SIZE))
                                    .text_color(contact_ink(alpha))
                                    .absolute()
                                    .left(px(dx / scale))
                                    .top(px(dy / scale))
                            }));
                            icon_box = icon_box.children(RING_DIRECTIONS.map(|(dx, dy)| {
                                svg()
                                    .path(icon)
                                    .size(px(ICON_SIZE))
                                    .text_color(ink.ring)
                                    .absolute()
                                    .left(px(dx / scale))
                                    .top(px(dy / scale))
                            }));
                        }
                        icon_box = icon_box
                            .child(svg().path(icon).size(px(ICON_SIZE)).text_color(pal.muted));
                        div()
                            .w(px(ICON_LANE))
                            .h(px(ICON_LANE))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(icon_box)
                    })
                    .child({
                        // Both decorations come from GPUI's own glyphs: the
                        // contact copies two to four device pixels out carry
                        // the shadow, and the one-pixel ring carries the
                        // hairline. Neither can drift from the text the way a
                        // second renderer's outline does.
                        let name = name.to_owned();
                        let mut cell = div().relative().flex_1().min_w_0().truncate();
                        if pinned {
                            cell = cell.children(CONTACT_KERNEL.map(|(dx, dy, alpha)| {
                                div()
                                    .absolute()
                                    .inset_0()
                                    .left(px(dx / scale))
                                    .top(px(dy / scale))
                                    .truncate()
                                    .text_size(px(NAME_TEXT_SIZE))
                                    .font_weight(NAME_FONT_WEIGHT)
                                    .text_color(contact_ink(alpha))
                                    .child(name.clone())
                            }));
                            cell = cell.children(RING_DIRECTIONS.map(|(dx, dy)| {
                                div()
                                    .absolute()
                                    .inset_0()
                                    .left(px(dx / scale))
                                    .top(px(dy / scale))
                                    .truncate()
                                    .text_size(px(NAME_TEXT_SIZE))
                                    .font_weight(NAME_FONT_WEIGHT)
                                    .text_color(ink.ring)
                                    .child(name.clone())
                            }));
                        }
                        cell.child(
                            div()
                                .truncate()
                                .text_size(px(NAME_TEXT_SIZE))
                                .font_weight(NAME_FONT_WEIGHT)
                                .text_color(pal.text)
                                .child(name),
                        )
                    })
                    .when(live && row.charging, |line| {
                        let mut bolt = div().relative().flex_none().size(px(BOLT_SIZE));
                        if pinned {
                            bolt = bolt.children(CONTACT_KERNEL.map(|(dx, dy, alpha)| {
                                svg()
                                    .path(BOLT_ICON)
                                    .size(px(BOLT_SIZE))
                                    .text_color(contact_ink(alpha))
                                    .absolute()
                                    .left(px(dx / scale))
                                    .top(px(dy / scale))
                            }));
                            bolt = bolt.children(RING_DIRECTIONS.map(|(dx, dy)| {
                                svg()
                                    .path(BOLT_ICON)
                                    .size(px(BOLT_SIZE))
                                    .text_color(ink.ring)
                                    .absolute()
                                    .left(px(dx / scale))
                                    .top(px(dy / scale))
                            }));
                        }
                        line.child(
                            bolt.child(svg().path(BOLT_ICON).size(px(BOLT_SIZE)).text_color(color)),
                        )
                    })
                    .when(!row.online, |line| {
                        line.child(status_text(
                            rust_i18n::t!("device.offline").to_string(),
                            pal.muted,
                            pinned,
                            ink.ring,
                            scale,
                        ))
                    })
                    .child({
                        let value = row.value.clone();
                        // The cell must not clip: the glow's canvas reaches
                        // past the reading. The reading keeps its `truncate`.
                        // The lane is a minimum, not a fixed width, so a long
                        // localized reading still takes the room it needs.
                        let mut cell = div().relative().flex().justify_end().min_w(px(VALUE_LANE));
                        if pinned {
                            // The baked asset is the ambient tail; the contact
                            // copies below are the near field, stamped from
                            // the reading's own glyphs.
                            if value_shadow && let Some(path) = value_shadow_path(&value) {
                                let (width, height) = VALUE_SHADOW_CANVAS;
                                cell = cell.child(
                                    img(path)
                                        .absolute()
                                        .right(px(-VALUE_SHADOW_PAD))
                                        .top(px(-(height - LINE_HEIGHT) / 2.0))
                                        .w(px(width))
                                        .h(px(height)),
                                );
                            }
                            cell = cell.children(CONTACT_KERNEL.map(|(dx, dy, alpha)| {
                                div()
                                    .absolute()
                                    .right(px(-dx / scale))
                                    .top(px(dy / scale))
                                    .whitespace_nowrap()
                                    .text_size(px(VALUE_TEXT_SIZE))
                                    .font_weight(VALUE_FONT_WEIGHT)
                                    .text_color(contact_ink(alpha))
                                    .child(value.clone())
                            }));
                            cell = cell.children(RING_DIRECTIONS.map(|(dx, dy)| {
                                // The hairline stays GPUI glyphs: it has to
                                // land on the reading's own edge.
                                div()
                                    .absolute()
                                    .right(px(-dx / scale))
                                    .top(px(dy / scale))
                                    .whitespace_nowrap()
                                    .text_size(px(VALUE_TEXT_SIZE))
                                    .font_weight(VALUE_FONT_WEIGHT)
                                    .text_color(ink.ring)
                                    .child(value.clone())
                            }));
                        }
                        cell.child(
                            div()
                                .truncate()
                                .text_size(px(VALUE_TEXT_SIZE))
                                .font_weight(VALUE_FONT_WEIGHT)
                                .text_color(color)
                                .child(value),
                        )
                    }),
            )
            .child({
                let mut track = div()
                    .relative()
                    .h(px(BAR_HEIGHT))
                    .w_full()
                    .rounded_full()
                    .bg(track_bg);
                if pinned {
                    // The track's white veil only has body on the dark card;
                    // over a light desktop the blurred shadow was the whole
                    // visible capsule. This hairline gives the bar the crisp
                    // edge the glyphs' ring copies keep in that state.
                    track = track.child(
                        div()
                            .absolute()
                            .inset(px(-1.0 / scale))
                            .rounded_full()
                            .border(px(1.0 / scale))
                            .border_color(ink.ring),
                    );
                }
                let track = track.when(row.fill.is_some(), |track| {
                    track.child(div().h_full().w(px(fill_width)).rounded_full().bg(color))
                });
                // The lane is the track's own layout cell. The bar's shadow
                // is a fixed asset placed behind the track in this cell, so
                // it shares the cell's y instead of drifting with the
                // full-card picture at fractional scales.
                let mut lane = div().relative().h(px(BAR_HEIGHT)).w_full();
                if pinned {
                    lane = lane.child(
                        img(BAR_SHADOW_PATH)
                            .absolute()
                            .left(px(-BAR_SHADOW_PAD))
                            .top(px(-BAR_SHADOW_PAD))
                            .w(px(inner_width + BAR_SHADOW_PAD * 2.0))
                            .h(px(BAR_HEIGHT + BAR_SHADOW_PAD * 2.0)),
                    );
                }
                lane.child(track)
            })
    }

    #[expect(
        clippy::too_many_lines,
        reason = "the card's shadow, header and rows read top to bottom"
    )]
    fn card(
        &self,
        pal: &Palette,
        pinned: bool,
        scale: f32,
        pointer_inside: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let ink = shadow_ink();
        let pin_hover = pal.pin_chip(pinned, false);
        let pin_pressed = pal.pin_chip(pinned, true);
        let pin_hovered = self.pin_hovered && pointer_inside;
        div()
            .w(px(CARD_WIDTH))
            .flex()
            .flex_col()
            .px(px(PADDING_X))
            .py(px(PADDING_Y))
            .rounded(px(RADIUS))
            .when(!pinned, |card| {
                card.bg(pal.card)
                    .border(px(BORDER))
                    .border_color(pal.border)
                    .shadow_lg()
                    // The pinned card is fixed by definition; while it can be
                    // dragged, the visible card is the platform's move handle —
                    // except the pin button, which occludes its own hitbox.
                    .window_control_area(WindowControlArea::Drag)
            })
            .debug_selector(|| "flow-indicator-card".into())
            .when(pinned, |card| {
                if let Some(path) = shadow_asset_path() {
                    let card_box = card_size(self.rows.len());
                    card.child(
                        img(path)
                            .absolute()
                            .inset_0()
                            .w(card_box.width)
                            .h(card_box.height),
                    )
                } else {
                    card
                }
            })
            .child({
                let pin_label = if pinned {
                    rust_i18n::t!("flow_indicator.unpin_card")
                } else {
                    rust_i18n::t!("flow_indicator.pin_card")
                };
                let pin = div()
                    .id("flow-indicator-pin")
                    .size(px(HEADER_LANE))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(6.0))
                    .occlude()
                    .cursor_pointer()
                    .role(Role::Button)
                    .accessibility_id("flow-indicator-pin")
                    .aria_label(pin_label.to_string())
                    .aria_toggled(if pinned {
                        Toggled::True
                    } else {
                        Toggled::False
                    })
                    .when(pin_hovered, |pin| pin.bg(pin_hover))
                    .active(|style| style.bg(pin_pressed))
                    .on_click(cx.listener(Self::toggle_pin))
                    .debug_selector(|| "flow-indicator-pin".into())
                    .on_mouse_move(cx.listener(|this, _: &MouseMoveEvent, _window, cx| {
                        this.set_pin_hovered(true, cx);
                    }))
                    .on_mouse_exit(cx.listener(|this, _: &MouseExitEvent, _window, cx| {
                        this.set_pin_hovered(false, cx);
                    }))
                    .child({
                        let mut icon = div().relative().size(px(PIN_ICON_SIZE));
                        if pinned {
                            icon = icon.children(CONTACT_KERNEL.map(|(dx, dy, alpha)| {
                                svg()
                                    .path(PIN_ICON)
                                    .size(px(PIN_ICON_SIZE))
                                    .text_color(contact_ink(alpha))
                                    .absolute()
                                    .left(px(dx / scale))
                                    .top(px(dy / scale))
                            }));
                            icon = icon.children(RING_DIRECTIONS.map(|(dx, dy)| {
                                svg()
                                    .path(PIN_ICON)
                                    .size(px(PIN_ICON_SIZE))
                                    .text_color(ink.ring)
                                    .absolute()
                                    .left(px(dx / scale))
                                    .top(px(dy / scale))
                            }));
                        }
                        icon.child(
                            svg()
                                .path(PIN_ICON)
                                .size(px(PIN_ICON_SIZE))
                                .text_color(if pinned { pal.text } else { pal.muted }),
                        )
                    });
                let mut header = div()
                    .h(px(HEADER_LANE))
                    .mb(px(HEADER_GAP))
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_end();
                if self.hidden > 0 {
                    // A `Role::Label` node's UIA name comes from its value,
                    // not its label, so both are set: the value names it for
                    // assistive technology and the label keeps other AccessKit
                    // adapters consistent.
                    let hidden_label =
                        rust_i18n::t!("flow_indicator.more_devices", count => self.hidden)
                            .to_string();
                    header = header.child(
                        ringed_text(
                            format!("+{}", self.hidden),
                            10.0,
                            pal.muted,
                            pinned,
                            ink.ring,
                            scale,
                        )
                        .id("flow-indicator-hidden")
                        .role(Role::Label)
                        .accessibility_id("flow-indicator-hidden")
                        .aria_label(hidden_label.clone())
                        .aria_value(hidden_label)
                        .debug_selector(|| "flow-indicator-hidden".into()),
                    );
                }
                if self.stale {
                    if self.hidden > 0 {
                        header = header.child(div().w(px(6.0)));
                    }
                    header = header.child(
                        ringed_text(
                            rust_i18n::t!("common.unavailable").to_string(),
                            10.0,
                            pal.muted,
                            pinned,
                            ink.ring,
                            scale,
                        )
                        .debug_selector(|| "flow-indicator-stale".into()),
                    );
                }
                header = header.child(div().flex_1());
                if pin_hovered {
                    let hint = if pinned {
                        rust_i18n::t!("flow_indicator.unpin_card")
                    } else {
                        rust_i18n::t!("flow_indicator.pin_card")
                    };
                    header = header
                        .child(
                            hint_capsule(hint.to_string(), pinned, pal)
                                .debug_selector(|| "flow-indicator-pin-hint".into()),
                        )
                        .child(div().w(px(HEADER_GAP)));
                }
                header.child(pin)
            })
            .child(div().flex().flex_col().gap(px(ROW_GAP)).children(
                self.rows.iter().enumerate().map(|(index, row)| {
                    let name = self
                        .painted_names
                        .get(index)
                        .map_or(row.name.as_str(), String::as_str);
                    Self::row_view(row, name, pal, pinned, scale, self.stale)
                }),
            ))
    }

    /// The pin's hover state drives the header hint; notify only on a change
    /// so the card does not repaint for every mouse move across the button.
    fn set_pin_hovered(&mut self, hovered: bool, cx: &mut Context<Self>) {
        if self.pin_hovered != hovered {
            self.pin_hovered = hovered;
            cx.notify();
        }
    }

    /// The pin button's handler. Windows and X11 flip the stacking live;
    /// Linux also sets the card's input region here, leaving only the pin
    /// button interactive. macOS and a Wayland layer surface carry the
    /// stacking in the window kind (or the layer), which only a new window
    /// has, so their owner tick notices the flip and reopens the card.
    fn toggle_pin(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.set_pinned(!self.pinned.get(), window, cx);
    }

    /// Apply a pinned state to the card: the pointer click and the agent's
    /// tray command both land here.
    pub(crate) fn set_pinned(&mut self, pinned: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.pinned.set(pinned);
        #[cfg(target_os = "windows")]
        {
            set_topmost(
                window,
                pinned || self.mode == FlowIndicatorDisplayMode::Topmost,
            );
            crate::platform::set_card_click_through(window, pinned);
        }
        #[cfg(target_os = "linux")]
        {
            self.apply_topmost(pinned || self.mode == FlowIndicatorDisplayMode::Topmost);
            self.apply_click_through(window, pinned);
            if pinned {
                self.nudge_wayland_window(window);
            }
        }
        if self.saves_placement {
            // The GNOME companion extension reads `pinned` to take keyboard
            // focus off the card; it must see the new state before the next
            // click, not on the once-a-second owner tick. On Windows the owner
            // writes the per-display positions a tick later: the view has no
            // access to that map, and writing here would drop the other
            // monitors' remembered spots.
            #[cfg(not(target_os = "windows"))]
            save_position(visible_origin(window.bounds().origin), pinned);
        }
        if pinned {
            // The regular card has already baked the shadow; this covers a pin
            // that arrives before the bake (or after a failed one).
            self.refresh_shadow(window, cx);
            self.refresh_value_shadows(window, cx);
        }
        info!(pinned, "Flow Indicator pin toggled");
        cx.notify();
    }
}

impl Render for FlowIndicatorView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.visible {
            return div().size_full();
        }
        let pinned = self.pinned.get();
        // The pinned card is always dark: its ring copies and blurred shadow
        // only have body on a dark surface. The regular card sits beside the
        // settings panel, so it follows that panel's theme.
        let pal = Palette::for_dark(pinned || self.dark_mode(window));
        // Windows reports a pointer leave only through its hover bit: it
        // dispatches no `MouseExited`. Linux and macOS send the exit event,
        // and macOS's bit means "key window", which a HUD does not hold.
        #[cfg(target_os = "windows")]
        let pointer_inside = window.is_window_hovered();
        #[cfg(not(target_os = "windows"))]
        let pointer_inside = true;
        // The padding is the room the card's shadow paints into. Only the
        // visible card is the move handle; the shadow halo is not interactive.
        let root = div()
            .size_full()
            .p(px(SHADOW_MARGIN))
            // The pin occludes this hitbox, so a move that reaches the card is
            // a move off the pin.
            .on_mouse_move(cx.listener(|this, _: &MouseMoveEvent, _window, cx| {
                this.set_pin_hovered(false, cx);
            }))
            .child(self.card(
                &pal,
                pinned,
                window.scale_factor(),
                pointer_inside,
                cx,
            ));
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
        if self.pinned.get() {
            return;
        }
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
        let areas = display_areas(cx);
        let primary = cx.primary_display().map(|display| display.visible_bounds());
        let origin = area_for_point(&areas, primary, root)
            .map_or(desired, |area| clamp_origin(area, card, desired));
        x11.move_to(origin, scale);
    }

    fn end_drag(&mut self, _event: &MouseUpEvent, _window: &mut Window, _cx: &mut Context<Self>) {
        self.manual_drag = None;
    }

    /// Keep a bottom-anchored card's bottom edge when its row count changes.
    /// A layer surface has no position to move; the compositor anchors it.
    #[cfg(target_os = "linux")]
    fn anchor_after_resize(&mut self, window: &Window, origin: Point<gpui::Pixels>) {
        if window.bounds().origin == origin {
            return;
        }
        if let Some(x11) = self.x11.as_ref() {
            x11.move_to(origin, window.scale_factor());
        }
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

    /// Apply the card's stacking mode. X11's `_NET_WM_STATE_ABOVE` is a live
    /// switch; a plain Wayland toplevel has no stacking to switch, so only X11
    /// does anything here.
    fn apply_topmost(&self, above: bool) {
        if let Some(x11) = self.x11.as_ref() {
            x11.set_above(above);
        }
    }

    /// Cut pointer input down to the visible card while regular, or to the
    /// pin button while pinned, so the shadow halo passes through. A
    /// replacement opened hidden for the pin swap takes no input at all: it
    /// sits above the old card, which stays on screen and keeps the pointer
    /// until the replacement is revealed.
    ///
    /// Wayland has no cross-process hit-test bit; a `wl_surface` input region
    /// is the native equivalent. X11 has no such GPUI API, so the card's own
    /// connection applies the Shape extension's input shape. A fresh window
    /// starts with the whole surface taking input.
    fn apply_click_through(&self, window: &Window, pinned: bool) {
        let region = click_through_region(self.visible, pinned, self.rows.len());
        if let Some(x11) = self.x11.as_ref() {
            // The hidden replacement is a Wayland-only state: the pin swap
            // never runs on X11, where the card is visible when it takes input.
            if let Some(region) = region {
                x11.set_input_region(window.scale_factor(), Some(region));
            }
        } else if gpui::guess_compositor() == "Wayland" {
            match region {
                Some(region) => window.set_input_region(Some(std::slice::from_ref(&region))),
                None => window.set_input_region(Some(&[])),
            }
        }
    }

    /// Mutter keeps the pointer focus on the card after its input region
    /// shrinks while the pointer is inside the old frame, so the next click
    /// still reaches the card until the pointer leaves and re-enters. A
    /// one-pixel window growth for one frame re-checks the actor under the
    /// pointer without touching the card: the extra pixel stays inside the
    /// transparent shadow margin.
    fn nudge_wayland_window(&self, window: &mut Window) {
        if self.x11.is_some() || gpui::guess_compositor() != "Wayland" {
            return;
        }
        let outer = window_size(self.rows.len());
        window.resize(size(outer.width + px(1.0), outer.height + px(1.0)));
        window.on_next_frame(move |window, _| window.resize(outer));
    }
}

impl FlowIndicatorView {
    /// A freshly opened card: name it, and on X11 hand the window to the seam
    /// that restores its position.
    #[cfg(target_os = "linux")]
    fn on_opened(&mut self, window: &mut Window, origin: Point<gpui::Pixels>) {
        // The task list names the window after this; without it the card
        // surfaces as "Untitled window".
        window.set_window_title(openlogi_core::brand::FLOW_INDICATOR_TITLE);
        self.place_on_x11(window, origin);
        // A card reopened while pinned (a layer-shell flip) needs the hole
        // again; a fresh window never inherits the old one's input region.
        self.apply_click_through(window, self.pinned.get());
    }

    /// A freshly opened card on Windows: drop the Win32 frame GPUI leaves on
    /// pop-ups (the ring and the notice take the same treatment through their
    /// placement), keep clicks from taking the foreground, then bring the
    /// window back to the card's size — losing the frame grows the client area
    /// by the old frame.
    ///
    /// The native placement uses the window's actual scale, not the monitor
    /// DPI: a virtual display can report a different effective DPI to
    /// `GetDpiForMonitor` than Windows gives the window, and sizing the clamp
    /// from the reported DPI leaves the card short of the edge it was placed
    /// against.
    #[cfg(target_os = "windows")]
    fn on_opened(
        &mut self,
        window: &mut Window,
        _origin: Point<gpui::Pixels>,
        placement: Option<&PhysicalPlacement>,
    ) {
        // The tray finds the card by this title to raise it with the GUI's
        // windows; the card is a tool window, so it stays out of the task list.
        window.set_window_title(openlogi_core::brand::FLOW_INDICATOR_TITLE);
        crate::platform::make_card_frameless(window);
        crate::platform::make_card_nonactivating(window);
        window.resize(window_size(self.rows.len()));
        let layout = crate::platform::monitor_layout();
        let saved = placement.map(|placement| (placement.origin, placement.display.as_deref()));
        if let Some(target) = crate::platform::card_target(
            &layout,
            saved,
            card_size(self.rows.len()),
            Some(window.scale_factor()),
        ) {
            let _ = crate::platform::place_card_visible(window, target.origin);
        }
        // The watch makes the shadow margin click-through in both states and
        // keeps the card interactive only over its visible area (or the pin
        // while pinned). A fresh window never inherits the old styles.
        crate::platform::set_card_click_through(window, self.pinned.get());
    }

    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    #[expect(
        clippy::unused_self,
        reason = "the Linux version hands the window to the X11 seam"
    )]
    fn on_opened(&mut self, _window: &mut Window, _origin: Point<gpui::Pixels>) {}
}

/// What the widget task receives: a new agent snapshot, new config, or the
/// once-a-second position tick.
pub(crate) enum Command {
    /// A fresh snapshot, or `None` when the connection that carried the last
    /// one dropped.
    Observation(Option<Box<Observation>>),
    Settings(Box<(Settings, Config)>),
    /// The agent's pinned state, owned there so the tray and the card agree.
    FlowPinned(bool),
    Tick,
}

/// The card's relationship to the agent connection, in the terms the card
/// paints: when the connection dropped, and whether the drop has outlived the
/// grace period so the frozen readings must be marked.
#[derive(Clone, Copy, Debug, Default)]
struct AgentLink {
    lost_since: Option<Instant>,
    stale: bool,
}

impl AgentLink {
    /// A snapshot arrived: the connection is alive and the readings are live.
    fn observed(&mut self) {
        self.lost_since = None;
        self.stale = false;
    }

    /// The connection dropped. Does not mark the card stale yet: a supervisor
    /// restart usually reconnects inside the grace period, and a marker that
    /// flashes for every restart teaches nothing.
    fn lost(&mut self, now: Instant) {
        self.lost_since.get_or_insert(now);
    }

    /// Fold the clock into the latch; `true` when the painted state changed.
    fn settle(&mut self, now: Instant) -> bool {
        let stale = self
            .lost_since
            .is_some_and(|since| now.duration_since(since) >= STALE_AFTER);
        let changed = stale != self.stale;
        self.stale = stale;
        changed
    }
}

/// The widget's window lifecycle: open it when there is something to show,
/// update it in place for percentage changes, and reopen it when its height or
/// display mode changes.
///
/// This type is owned by one GPUI task, not an entity: opening a window inside
/// an entity update re-enters GPUI's app borrow. The task hands each command
/// through `App::update`, the same context the notice windows open from.
#[expect(
    clippy::struct_excessive_bools,
    reason = "each bool is one lifecycle fact; the last mirrors the shared pin toggle"
)]
pub(crate) struct FlowIndicator {
    settings: Settings,
    /// The config that names the rows; `None` until the first successful read.
    config: Option<Config>,
    inventory: Vec<DeviceInventory>,
    /// The desktop's light/dark mode, shared with the view that paints it.
    appearance: Arc<Appearance>,
    /// The appearance revision the open card last painted.
    appearance_revision: u64,
    /// The window exists but settings hide the card: it is not closed, which
    /// is the only way a Wayland compositor keeps the placement it chose for
    /// it. Wayland hides it as a transparent 1x1, Windows natively.
    hidden: bool,
    window: Option<WindowHandle<FlowIndicatorView>>,
    /// The display mode the open window carries: the mode it was created for,
    /// or the one applied live on X11 (whose card never changes kind).
    window_mode: Option<FlowIndicatorDisplayMode>,
    /// Whether the open window is a Wayland layer surface, which has no
    /// position the compositor reports or restores.
    layer_shell: bool,
    /// The pinned card's predecessor, kept visible under the replacement
    /// until the extension has moved the new window onto it.
    #[cfg(target_os = "linux")]
    retiring: Option<(AnyWindowHandle, Instant)>,
    #[cfg(not(target_os = "windows"))]
    last_saved_origin: Option<Point<gpui::Pixels>>,
    /// The user's physical spot for the card, and the monitor it belongs to.
    #[cfg(target_os = "windows")]
    placement: Option<PhysicalPlacement>,
    /// The spot remembered for each monitor, so a display switch back to a
    /// previously used monitor restores that monitor's own place.
    #[cfg(target_os = "windows")]
    positions: BTreeMap<String, Point<DevicePixels>>,
    /// The monitor layout the last tick saw, to notice a topology change.
    #[cfg(target_os = "windows")]
    monitor_layout: Option<Vec<crate::platform::MonitorInfo>>,
    /// Until this instant the card is re-anchored every tick and its position
    /// is not persisted: Windows is still moving windows for the new topology.
    #[cfg(target_os = "windows")]
    placement_settle_until: Option<Instant>,
    /// A row-count change moved the card deliberately; the next tick should
    /// adopt and save that position.
    #[cfg(target_os = "windows")]
    placement_needs_save: bool,
    /// When the card's layout scale started disagreeing with its monitor.
    #[cfg(target_os = "windows")]
    dpi_mismatch_since: Option<Instant>,
    /// Reopens attempted for that mismatch, so a window that still disagrees
    /// after being reopened cannot loop forever.
    #[cfg(target_os = "windows")]
    dpi_reopens: u8,
    /// The layout scale the last tick saw. A scale change is not always a
    /// monitor topology change (virtual displays can report the same
    /// `GetDpiForMonitor` at several window scales), yet the card's visible
    /// origin moves with the shadow margin and must be re-anchored.
    #[cfg(target_os = "windows")]
    last_layout_dpi: u32,
    /// A display topology change settles, then the card is reopened once.
    ///
    /// A window can be created during a display transition with the old
    /// monitor's DPI; `GetDpiForWindow` then agrees with GPUI's scale, so the
    /// DPI mismatch watchdog cannot see that the window is still scaled for a
    /// display that is gone. A fresh window opened after the topology settles
    /// asks Windows for the remaining monitor's DPI.
    #[cfg(target_os = "windows")]
    topology_reopen_pending: bool,
    /// Set when the user closed the card window: observations must not bring
    /// it back until the settings change.
    dismissed: Rc<Cell<bool>>,
    /// The pin toggle the open card's view writes; the owner reads it so a
    /// reopen (display-mode change) keeps the choice, and so a flip can move
    /// the card between the platform's window kinds.
    pinned: Rc<Cell<bool>>,
    /// The pin state the last tick saw.
    pinned_seen: bool,
    /// The pin state the agent last reported, or `None` before the first sync.
    agent_pinned: Option<bool>,
    /// Where a pin toggled on the card itself is reported, so the tray's
    /// checkmark follows it.
    flow_pin_reports: Option<mpsc::UnboundedSender<bool>>,
    /// The agent connection's freshness: when it dropped, and whether the
    /// frozen readings are already marked stale.
    agent: AgentLink,
    /// Logged once, so a boot without a card shows whether observations ever
    /// reached this process.
    observed: bool,
}

impl FlowIndicator {
    pub(crate) fn new(
        settings: Settings,
        config: Option<Config>,
        appearance: Arc<Appearance>,
        pinned: bool,
        flow_pin_reports: mpsc::UnboundedSender<bool>,
    ) -> Self {
        let appearance_revision = appearance.revision();
        // A Windows file without the physical marker was written in GPUI's
        // logical coordinates: adopt its point as physical so the card stays
        // where it is seen today, then rewrite it from the first tick.
        #[cfg(target_os = "windows")]
        let (placement, placement_needs_save, positions) =
            load_saved().map_or((None, false, BTreeMap::new()), |saved| {
                let (placement, legacy) = saved.physical_placement();
                let mut positions: BTreeMap<String, Point<DevicePixels>> = saved
                    .positions
                    .iter()
                    .map(|spot| (spot.display.clone(), saved_origin(spot)))
                    .collect();
                if let Some(display) = &placement.display {
                    positions.entry(display.clone()).or_insert(placement.origin);
                }
                (Some(placement), legacy, positions)
            });
        Self {
            settings,
            config,
            inventory: Vec::new(),
            appearance,
            appearance_revision,
            hidden: false,
            window: None,
            window_mode: None,
            layer_shell: false,
            #[cfg(target_os = "linux")]
            retiring: None,
            #[cfg(not(target_os = "windows"))]
            last_saved_origin: None,
            #[cfg(target_os = "windows")]
            placement,
            #[cfg(target_os = "windows")]
            positions,
            #[cfg(target_os = "windows")]
            monitor_layout: None,
            #[cfg(target_os = "windows")]
            placement_settle_until: None,
            #[cfg(target_os = "windows")]
            placement_needs_save,
            #[cfg(target_os = "windows")]
            dpi_mismatch_since: None,
            #[cfg(target_os = "windows")]
            dpi_reopens: 0,
            #[cfg(target_os = "windows")]
            last_layout_dpi: 0,
            #[cfg(target_os = "windows")]
            topology_reopen_pending: false,
            dismissed: Rc::new(Cell::new(false)),
            pinned: Rc::new(Cell::new(pinned)),
            pinned_seen: pinned,
            agent_pinned: None,
            flow_pin_reports: Some(flow_pin_reports),
            agent: AgentLink::default(),
            observed: false,
        }
    }

    /// Whether handling the next command may have to open the card window:
    /// the card is wanted and no window exists to show it through.
    ///
    /// Windows starts the overlay headless, and GPUI refuses a window while
    /// headless (see [`crate::windowing::connect`]) — so a caller that feeds
    /// [`Self::handle`] must connect to the display first when this answers
    /// true, or the card silently never opens there.
    pub(crate) fn needs_display(&self) -> bool {
        card_needs_display(
            self.window.is_some(),
            self.dismissed.get(),
            self.settings.enabled,
            self.rows().0.len(),
            self.observed,
        )
    }

    pub(crate) fn handle(&mut self, command: Command, cx: &mut App) {
        match command {
            Command::Observation(observation) => {
                match observation {
                    Some(observation) => {
                        self.inventory.clone_from(&observation.snapshot.inventory);
                        if !self.observed {
                            self.observed = true;
                            info!("Flow Indicator received its first observation");
                        }
                        if self.agent.stale {
                            info!("Flow Indicator agent reconnected; readings are live again");
                        }
                        self.agent.observed();
                        self.sync(cx);
                    }
                    None => {
                        // The card keeps its last readings; the tick marks
                        // them stale once the drop outlives the grace period.
                        self.agent.lost(Instant::now());
                    }
                }
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
            Command::FlowPinned(pinned) => self.apply_agent_pin(pinned, cx),
            Command::Tick => {
                self.refresh_dismissed(cx);
                #[cfg(target_os = "linux")]
                self.settle_place(cx);
                self.follow_pin(cx);
                #[cfg(target_os = "linux")]
                self.settle_retired(cx);
                #[cfg(target_os = "windows")]
                self.settle_size(cx);
                #[cfg(target_os = "windows")]
                self.settle_display_change(cx);
                self.follow_appearance(cx);
                self.follow_staleness(cx);
                self.persist_position(cx);
            }
        }
    }

    /// Apply the agent's pinned state. The agent is the owner: a tray toggle
    /// made while the overlay was down still wins over the stale placement
    /// file, and the card persists the adopted value.
    fn apply_agent_pin(&mut self, pinned: bool, cx: &mut App) {
        self.agent_pinned = Some(pinned);
        if self.pinned.get() == pinned {
            return;
        }
        self.pinned.set(pinned);
        if let Some(handle) = self.window.as_ref() {
            let _ = handle.update(cx, |view, window, cx| view.set_pinned(pinned, window, cx));
        }
        // Persist and settle even when the card is hidden; the next open
        // already reads the shared flag.
        self.follow_pin(cx);
    }

    /// Tell the agent the card's current pin, so the tray's checkmark follows
    /// a click on the pin itself.
    fn report_pin(&self) {
        if let Some(sender) = &self.flow_pin_reports {
            let _ = sender.send(self.pinned.get());
        }
    }

    /// The mode the card should run in: pinning forces Topmost over the
    /// settings' choice, unpinning returns to it.
    fn effective_mode(&self) -> FlowIndicatorDisplayMode {
        if self.pinned.get() {
            FlowIndicatorDisplayMode::Topmost
        } else {
            self.settings.display_mode
        }
    }

    /// Re-assert the card's physical size after a DPI change.
    ///
    /// The tick is the only clock that also runs while the readings are
    /// unchanged; observations alone could leave the card clipped for minutes.
    #[cfg(target_os = "windows")]
    fn settle_size(&mut self, cx: &mut App) {
        let want_topmost = self.effective_mode() == FlowIndicatorDisplayMode::Topmost;
        let shown = !self.hidden;
        let (mismatch, layout_dpi) = {
            let Some(handle) = self.window.as_ref() else {
                self.dpi_mismatch_since = None;
                return;
            };
            let mut mismatch = false;
            let mut layout_dpi = 0;
            let _ = handle.update(cx, |view, window, _| {
                // The native watch needs the painted scale to place its pin
                // hit test; GPUI only changes it when a DPI message arrives.
                layout_dpi = crate::platform::set_card_layout_scale(window);
                mismatch = crate::platform::card_dpi_mismatch(window);
                let outer = window_size(view.rows.len());
                if view.visible && card_size_is_stale(window, outer) {
                    window.resize(outer);
                }
                // The style bit can survive a z-order reset while the window
                // sits in the normal band; only the z-order shows that. A
                // pinned card is raised back as soon as a normal window is
                // above it, not only when the bit is missing.
                if shown {
                    if want_topmost {
                        if crate::platform::card_needs_raise(window) {
                            set_topmost(window, true);
                            info!("Flow Indicator raised back into the topmost band");
                        }
                    } else if window_is_topmost(window) != Some(false) {
                        set_topmost(window, false);
                    }
                }
            });
            (mismatch, layout_dpi)
        };
        if self.last_layout_dpi != layout_dpi {
            let changed = self.last_layout_dpi != 0;
            self.last_layout_dpi = layout_dpi;
            if changed {
                // The visible origin moves with the shadow margin even when
                // the monitor list is unchanged (virtual displays can expose
                // one DPI while their windows get another); re-anchor so the
                // saved physical point stays the visible card's origin.
                self.placement_settle_until = Some(Instant::now() + DISPLAY_SETTLE);
            }
        }
        if !mismatch || self.hidden {
            self.dpi_mismatch_since = None;
            if !mismatch {
                self.dpi_reopens = 0;
            }
            return;
        }
        let since = *self.dpi_mismatch_since.get_or_insert_with(Instant::now);
        if since.elapsed() >= DPI_MISMATCH_GRACE {
            self.reopen_for_dpi(cx);
        }
    }

    /// Recreate the card when its GPUI scale still disagrees with the monitor
    /// after the watch has re-sent `WM_DPICHANGED`.
    ///
    /// A fresh window asks GPUI for the scale of the monitor it opens on, so
    /// this is the fallback that cannot leave the card stuck at the previous
    /// monitor's scale (and its pin unreachable).
    #[cfg(target_os = "windows")]
    fn reopen_for_dpi(&mut self, cx: &mut App) {
        if self.dpi_reopens >= 2 {
            warn!("Flow Indicator could not recover its monitor DPI by reopening the card");
            self.dpi_mismatch_since = None;
            return;
        }
        self.dpi_reopens += 1;
        info!("Flow Indicator reopening the card to apply its monitor DPI");
        self.close(cx);
        let (rows, hidden) = self.rows();
        self.open(&rows, hidden, None, true, cx);
        self.dpi_mismatch_since = None;
    }

    /// Reopen the card once after a display topology has settled.
    ///
    /// A window created while a display was still disappearing can keep the
    /// old monitor's DPI; its own DPI and GPUI's scale then agree, so the
    /// mismatch watchdog cannot see the stale scale. A fresh window created
    /// after the topology settles asks Windows for the remaining monitor's
    /// DPI.
    #[cfg(target_os = "windows")]
    fn reopen_for_topology(&mut self, cx: &mut App) {
        info!("Flow Indicator reopening the card after a display topology change");
        self.close(cx);
        let (rows, hidden) = self.rows();
        self.open(&rows, hidden, None, true, cx);
        self.dpi_mismatch_since = None;
        self.dpi_reopens = 0;
        self.last_layout_dpi = 0;
    }

    /// Put the card back where the user fixed it after a display topology
    /// change, and keep Windows from adopting its own repositioning.
    ///
    /// The saved point stays on its original monitor while that monitor is
    /// connected; without it the card clamps into the primary's work area.
    /// Re-anchoring runs every tick for [`DISPLAY_SETTLE`], because Windows
    /// finishes moving windows for a new topology over several hundred
    /// milliseconds and the first anchor can land before the last move.
    #[cfg(target_os = "windows")]
    fn settle_display_change(&mut self, cx: &mut App) {
        let layout = crate::platform::monitor_layout();
        let changed = crate::platform::topology_changed(self.monitor_layout.as_deref(), &layout);
        let had_layout = self.monitor_layout.is_some();
        self.monitor_layout = Some(layout.clone());
        if changed {
            if had_layout {
                info!(
                    displays = layout.len(),
                    "Flow Indicator display layout changed; re-anchoring the card"
                );
                self.topology_reopen_pending = true;
                self.dpi_reopens = 0;
            }
            self.placement_settle_until = Some(Instant::now() + DISPLAY_SETTLE);
        }
        let Some(deadline) = self.placement_settle_until else {
            return;
        };
        if Instant::now() >= deadline {
            self.placement_settle_until = None;
            if self.topology_reopen_pending {
                self.topology_reopen_pending = false;
                if !self.hidden {
                    self.reopen_for_topology(cx);
                }
            }
            return;
        }
        let Some(handle) = self.window.as_ref() else {
            return;
        };
        let Some(saved) = self.target_placement(&layout) else {
            return;
        };
        let _ = handle.update(cx, |view, window, _| {
            if let Some(target) = crate::platform::card_target(
                &layout,
                Some((saved.origin, saved.display.as_deref())),
                card_size(view.rows.len()),
                Some(window.scale_factor()),
            ) {
                let before =
                    crate::platform::card_physical_placement(window).map(|(origin, _)| origin);
                let _ = crate::platform::place_card_visible(window, target.origin);
                let after =
                    crate::platform::card_physical_placement(window).map(|(origin, _)| origin);
                if before != after {
                    info!(
                        ?target.origin,
                        scale = window.scale_factor(),
                        "Flow Indicator re-anchored the card"
                    );
                }
            }
        });
    }

    /// Notice a pin flip. Windows has already moved the live window (the
    /// view's handler flipped the bit); elsewhere the flip changes which
    /// window kind the card needs, and `sync` reopens it for that.
    fn follow_pin(&mut self, cx: &mut App) {
        if self.pinned_seen == self.pinned.get() {
            return;
        }
        #[cfg(target_os = "linux")]
        let was_pinned = self.pinned_seen;
        self.pinned_seen = self.pinned.get();
        if let Some(agent) = self.agent_pinned
            && agent != self.pinned_seen
        {
            self.agent_pinned = Some(self.pinned_seen);
            self.report_pin();
        }
        // Persist from the owner, not the view: the view's pin handler also
        // runs in GPUI tests, where writing the real placement file is noise.
        // On Windows a re-pin adopts where the card now is: the preceding drag
        // ran in the OS move loop, so no once-a-second tick saw it.
        #[cfg(target_os = "windows")]
        {
            let _ = self.remember_placement(cx, self.pinned_seen);
            if let Some(placement) = self.placement.clone() {
                save_physical_position(
                    placement.origin,
                    placement.display.as_deref(),
                    self.pinned_seen,
                    &self.positions,
                );
            }
        }
        #[cfg(not(target_os = "windows"))]
        if let Some(origin) = self.window.as_ref().and_then(|handle| {
            handle
                .update(cx, |_, window, _| window.bounds().origin)
                .ok()
                .map(visible_origin)
        }) {
            save_position(origin, self.pinned_seen);
            self.last_saved_origin = Some(origin);
        }
        #[cfg(target_os = "linux")]
        if self.pinned_seen
            && !was_pinned
            && self.retiring.is_none()
            && self.window.is_some()
            && !self.layer_shell
            && gpui::guess_compositor() == "Wayland"
        {
            // Mutter keeps the pointer focus on the old surface after the
            // input region shrinks; a replacement surface is the only reliable
            // reset. Keep this one visible until the new one is in place.
            //
            // One swap at a time: the single `retiring` slot holds the card
            // being replaced, so a second swap would strand that card on
            // screen. A pin that lands while the first one settles reuses the
            // live replacement and lets the pending retiree finish.
            self.swap_for_pin(cx);
            return;
        }
        self.sync(cx);
    }

    /// Open a pinned replacement above the old card instead of recreating the
    /// card in one step. The old card is already drawing the pinned HUD; the
    /// replacement waits hidden until the extension has moved it onto the
    /// same frame, then takes over without a visible hole.
    #[cfg(target_os = "linux")]
    fn swap_for_pin(&mut self, cx: &mut App) {
        let Some(old) = self.window.take() else {
            return;
        };
        let origin = old
            .update(cx, |_, window, _| window.bounds().origin)
            .ok()
            .map(visible_origin);
        let (rows, hidden) = self.rows();
        self.open(&rows, hidden, origin, false, cx);
        if self.window.is_none() {
            // The replacement never opened: keep the old card live.
            self.window = Some(old);
            return;
        }
        self.retiring = Some((*old, Instant::now() + SWAP_OVERLAP));
    }

    /// Finish a pin swap once the replacement has had time to be placed.
    #[cfg(target_os = "linux")]
    fn settle_retired(&mut self, cx: &mut App) {
        let Some((old, deadline)) = self.retiring else {
            return;
        };
        if Instant::now() < deadline {
            return;
        }
        self.retiring = None;
        let Some(handle) = self.window.as_ref() else {
            if let Some(old) = old.downcast::<FlowIndicatorView>() {
                let _ = old.update(cx, |_, window, _| window.remove_window());
            }
            return;
        };
        let _ = handle.update(cx, |view, window, cx| {
            view.visible = true;
            view.apply_click_through(window, view.pinned.get());
            // The replacement took no input while it waited, so Mutter never
            // gave it pointer focus. Re-pick the window under the pointer now
            // that its hole is back, instead of losing the first click after
            // the reveal.
            view.nudge_wayland_window(window);
            cx.notify();
            // Draw the replacement once before dropping the old window, so
            // the swap cannot open a one-frame hole.
            window.on_next_frame(move |_, cx| {
                if let Some(old) = old.downcast::<FlowIndicatorView>() {
                    let _ = old.update(cx, |_, window, _| window.remove_window());
                }
            });
        });
    }

    /// Repaint the open card when the watched mode flips. The platform's own
    /// appearance signal repaints through the view's subscription; this covers
    /// the mode the watcher owns (and a portal that was late to the bus).
    fn follow_appearance(&mut self, cx: &mut App) {
        let revision = self.appearance.revision();
        if revision == self.appearance_revision {
            return;
        }
        self.appearance_revision = revision;
        if let Some(handle) = self.window.as_ref() {
            let _ = handle.update(cx, |_, _, cx| cx.notify());
        }
    }

    /// Mark the card stale once a dropped agent connection has stayed away
    /// past the grace period, and clear the mark on the next snapshot.
    fn follow_staleness(&mut self, cx: &mut App) {
        if !self.agent.settle(Instant::now()) {
            return;
        }
        let stale = self.agent.stale;
        if stale {
            info!("Flow Indicator agent readings marked stale");
        }
        if let Some(handle) = self.window.as_ref() {
            let _ = handle.update(cx, |view, _, cx| {
                view.stale = stale;
                cx.notify();
            });
        }
    }

    /// Persist where the user dragged the card. Called once a second with the
    /// config poll: the OS owns the drag, so there is no drag-end event to
    /// hang the write on and a per-frame write would be wasteful.
    #[cfg(target_os = "windows")]
    fn persist_position(&mut self, cx: &mut App) {
        // Windows is still moving windows for a new display layout; the tick
        // re-anchors the card instead of adopting those moves.
        if self
            .placement_settle_until
            .is_some_and(|deadline| Instant::now() < deadline)
        {
            return;
        }
        if let Some(placement) = self.remember_placement(cx, false) {
            save_physical_position(
                placement.origin,
                placement.display.as_deref(),
                self.pinned.get(),
                &self.positions,
            );
        }
    }

    /// Adopt the card's current physical position when the user moved it (or
    /// a row-count change/legacy file asks for it).
    ///
    /// Windows' own repositioning during a topology change must not overwrite
    /// the spot the user fixed; the watch marks a move with the left button
    /// down, which is the only move that belongs to the user. `adopt_now` is
    /// for a re-pin: the user is fixing the card where they see it, including
    /// a drag that crossed monitors (which itself is not a reliable tick,
    /// because the move loop keeps the UI thread busy).
    #[cfg(target_os = "windows")]
    fn remember_placement(&mut self, cx: &mut App, adopt_now: bool) -> Option<PhysicalPlacement> {
        let user_moved = crate::platform::take_card_user_moved();
        let actual = self.window.as_ref().and_then(|handle| {
            handle
                .update(cx, |_, window, _| {
                    crate::platform::card_physical_placement(window)
                })
                .ok()
                .flatten()
        });
        let mut adopted = false;
        if let Some((origin, display)) = actual {
            // A saved monitor name of `None` is a legacy file: whatever display
            // currently holds the card is the one to record.
            let same_display = self.placement.as_ref().is_none_or(|placement| {
                placement
                    .display
                    .as_deref()
                    .is_none_or(|saved| saved == display)
            });
            if adopt_now
                || user_moved
                || self.placement.is_none()
                || (self.placement_needs_save && same_display)
            {
                self.positions.insert(display.clone(), origin);
                self.placement = Some(PhysicalPlacement {
                    origin,
                    display: Some(display),
                });
                adopted = true;
            }
            self.placement_needs_save = false;
        }
        if adopted {
            self.placement.clone()
        } else {
            None
        }
    }

    /// Persist where the user dragged the card. Called once a second with the
    /// config poll: the OS owns the drag, so there is no drag-end event to
    /// hang the write on and a per-frame write would be wasteful.
    #[cfg(not(target_os = "windows"))]
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
                .map(visible_origin)
        });
        let Some(origin) = origin else {
            return;
        };
        if self.last_saved_origin == Some(origin) {
            return;
        }
        save_position(origin, self.pinned.get());
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

    fn devices(&self) -> Vec<BatteryDevice> {
        self.config.as_ref().map_or_else(
            || battery_devices(&self.inventory),
            |config| battery_devices_named(&self.inventory, config),
        )
    }

    fn rows(&self) -> (Vec<Row>, usize) {
        if !self.settings.enabled {
            return (Vec::new(), 0);
        }
        let devices = self.devices();
        let selected = flow_indicator_rows(
            &devices,
            self.settings.show_offline,
            self.settings.max_devices,
        );
        (
            selected.rows.into_iter().map(Row::from).collect(),
            selected.hidden,
        )
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

    #[expect(
        clippy::too_many_lines,
        reason = "the card's in-place update reads top to bottom"
    )]
    fn sync(&mut self, cx: &mut App) {
        self.refresh_dismissed(cx);
        if self.dismissed.get() {
            self.close(cx);
            return;
        }
        let (rows, hidden) = self.rows();
        let show = self.settings.enabled && !rows.is_empty();
        if let Some(handle) = self.window.as_ref() {
            if !show {
                self.hide(cx);
                return;
            }
            // Windows and X11 carry the display mode live; macOS and a
            // Wayland layer surface need a new kind (or layer), which only a
            // new window has.
            #[cfg(not(target_os = "windows"))]
            let mode_changed = self.window_mode != Some(self.effective_mode());
            #[cfg(not(target_os = "windows"))]
            let kind_change_needs_new_window = {
                #[cfg(target_os = "linux")]
                {
                    self.layer_shell
                }
                #[cfg(not(target_os = "linux"))]
                {
                    true
                }
            };
            #[cfg(not(target_os = "windows"))]
            if mode_changed && kind_change_needs_new_window {
                let origin = handle
                    .update(cx, |_, window, _| window.bounds().origin)
                    .ok()
                    .map(visible_origin);
                self.close(cx);
                self.open(&rows, hidden, origin, true, cx);
                return;
            }
            // Record a live change now so the next tick does not repeat it.
            #[cfg(target_os = "linux")]
            if mode_changed {
                self.window_mode = Some(self.effective_mode());
            }
            // Update in place — a new window would lose the placement the
            // compositor chose for this one.
            let card = card_size(rows.len());
            let outer = window_size(rows.len());
            let desired = resize_origin(cx, handle, card);
            #[cfg(not(any(target_os = "windows", target_os = "linux")))]
            let _ = desired;
            let settings_mode = self.settings.display_mode;
            let preference = self.settings.appearance;
            #[cfg_attr(
                not(any(target_os = "windows", target_os = "linux")),
                expect(unused_variables, reason = "a live stacking bit is Windows/X11-only")
            )]
            let want_topmost = self.effective_mode() == FlowIndicatorDisplayMode::Topmost;
            let new_row_count = rows.len();
            let size_changed = handle
                .update(cx, |view, window, cx| {
                    let size_changed = view.rows.len() != new_row_count;
                    view.rows = rows;
                    view.hidden = hidden;
                    view.stale = self.agent.stale;
                    view.visible = true;
                    view.mode = settings_mode;
                    view.appearance_preference = preference;
                    view.refresh_shadow(window, cx);
                    view.refresh_value_shadows(window, cx);
                    // Rewrite the z-order only when the live bit disagrees: an
                    // already-topmost window must not hear about it every tick.
                    #[cfg(target_os = "windows")]
                    if window_is_topmost(window) != Some(want_topmost) {
                        set_topmost(window, want_topmost);
                    }
                    #[cfg(target_os = "linux")]
                    if mode_changed {
                        view.apply_topmost(want_topmost);
                    }
                    if window.bounds().size != outer {
                        window.resize(outer);
                    }
                    #[cfg(target_os = "linux")]
                    view.apply_click_through(window, view.pinned.get());
                    #[cfg(any(target_os = "windows", target_os = "linux"))]
                    if let Some(desired) = desired {
                        #[cfg(target_os = "windows")]
                        {
                            let current = window.bounds().origin;
                            if current != desired {
                                crate::platform::move_card_by(
                                    window,
                                    point(desired.x - current.x, desired.y - current.y),
                                );
                            }
                        }
                        #[cfg(target_os = "linux")]
                        view.anchor_after_resize(window, desired);
                    }
                    cx.notify();
                    size_changed
                })
                .unwrap_or(false);
            #[cfg(target_os = "windows")]
            if size_changed {
                // The row-count resize anchors the card's bottom edge itself;
                // remember that deliberate move on the next tick.
                self.placement_needs_save = true;
            }
            #[cfg(not(target_os = "windows"))]
            let _ = size_changed;
            if self.hidden {
                self.hidden = false;
                let _ = handle.update(cx, |_, window, _| crate::platform::show_card(window));
                info!("Flow Indicator card shown");
            }
            return;
        }
        if show {
            self.open(&rows, hidden, None, true, cx);
        }
    }

    /// Take the card off the screen: settings (or an empty row set) hide it.
    ///
    /// A kept window is what remembers the card's place, so it is hidden
    /// rather than closed. Wayland shrinks it to a transparent 1x1; Windows
    /// hides it natively, because shrinking cannot work there: GPUI creates
    /// pop-ups as `WS_OVERLAPPED`, so the window is clamped to its frame
    /// minimum and DWM keeps drawing the frame and its shadow around the 1x1
    /// client area.
    fn hide(&mut self, cx: &mut App) {
        #[cfg(target_os = "linux")]
        if let Some((old, _)) = self.retiring.take()
            && let Some(old) = old.downcast::<FlowIndicatorView>()
        {
            let _ = old.update(cx, |_, window, _| window.remove_window());
        }
        if self.hidden {
            return;
        }
        let Some(handle) = self.window.as_ref() else {
            return;
        };
        let _ = handle.update(cx, |view, window, cx| {
            view.visible = false;
            crate::platform::hide_card(window);
            cx.notify();
        });
        self.hidden = true;
        info!("Flow Indicator card hidden");
    }

    fn close(&mut self, cx: &mut App) {
        #[cfg(target_os = "linux")]
        if let Some((old, _)) = self.retiring.take()
            && let Some(old) = old.downcast::<FlowIndicatorView>()
        {
            let _ = old.update(cx, |_, window, _| window.remove_window());
        }
        #[cfg(target_os = "windows")]
        {
            let _ = self.remember_placement(cx, false);
            if let Some(placement) = self.placement.clone() {
                save_physical_position(
                    placement.origin,
                    placement.display.as_deref(),
                    self.pinned.get(),
                    &self.positions,
                );
            }
        }
        if let Some(handle) = self.window.take() {
            info!("Flow Indicator card closed");
            #[cfg(not(target_os = "windows"))]
            if !self.layer_shell
                && let Ok(origin) = handle.update(cx, |_, window, _| window.bounds().origin)
                && self.last_saved_origin != Some(visible_origin(origin))
            {
                let origin = visible_origin(origin);
                save_position(origin, self.pinned.get());
                self.last_saved_origin = Some(origin);
            }
            let _ = handle.update(cx, |_, window, _| window.remove_window());
        }
        self.window_mode = None;
        self.layer_shell = false;
        self.hidden = false;
    }

    /// The placement the card should use for `layout`.
    #[cfg(target_os = "windows")]
    fn target_placement(
        &self,
        layout: &[crate::platform::MonitorInfo],
    ) -> Option<PhysicalPlacement> {
        self.placement
            .as_ref()
            .map(|active| Self::target_placement_from(active, layout, &self.positions))
    }

    /// The active monitor's spot when it is present; otherwise the spot
    /// remembered for the primary display, so switching back to a monitor
    /// restores that monitor's own place instead of clamping the other
    /// display's point onto it.
    #[cfg(target_os = "windows")]
    fn target_placement_from(
        active: &PhysicalPlacement,
        layout: &[crate::platform::MonitorInfo],
        positions: &BTreeMap<String, Point<DevicePixels>>,
    ) -> PhysicalPlacement {
        if active
            .display
            .as_deref()
            .is_some_and(|device| layout.iter().any(|monitor| monitor.device == device))
        {
            return active.clone();
        }
        let primary = layout
            .iter()
            .find(|monitor| monitor.monitor.origin == point(DevicePixels(0), DevicePixels(0)))
            .or_else(|| layout.first());
        if let Some(primary) = primary
            && let Some(origin) = positions.get(&primary.device)
        {
            return PhysicalPlacement {
                origin: *origin,
                display: Some(primary.device.clone()),
            };
        }
        active.clone()
    }

    /// The Windows window bounds and monitor for a restored card.
    ///
    /// GPUI's logical point is the physical point divided by the target
    /// monitor's scale; asking for that monitor by id also makes GPUI validate
    /// the request against the right display instead of falling back to the
    /// primary's centre.
    #[cfg(target_os = "windows")]
    #[expect(
        clippy::cast_possible_truncation,
        reason = "a screen-sized physical coordinate divided by a display scale fits an f32 logical pixel"
    )]
    fn card_bounds(&self, rows: usize) -> (Bounds<gpui::Pixels>, Option<DisplayId>) {
        let layout = crate::platform::monitor_layout();
        let target_placement = self.target_placement(&layout);
        let saved = target_placement
            .as_ref()
            .map(|placement| (placement.origin, placement.display.as_deref()));
        let Some(target) = crate::platform::card_target(&layout, saved, card_size(rows), None)
        else {
            // No monitor reported (a very early boot): open at the origin and
            // let the first tick place the card once the layout is known.
            return (
                Bounds::new(point(px(0.0), px(0.0)), window_size(rows)),
                None,
            );
        };
        let scale = f64::from(target.scale);
        let visible = point(
            px((f64::from(target.origin.x.0) / scale) as f32),
            px((f64::from(target.origin.y.0) / scale) as f32),
        );
        (
            Bounds::new(window_origin(visible), window_size(rows)),
            Some(target.display_id),
        )
    }

    fn open(
        &mut self,
        rows: &[Row],
        hidden: usize,
        remembered: Option<Point<gpui::Pixels>>,
        visible: bool,
        cx: &mut App,
    ) {
        // Windows restores from physical pixels so a display switch cannot
        // reinterpret the saved point through another monitor's scale; the
        // other platforms keep GPUI's logical placement.
        #[cfg(target_os = "windows")]
        let (bounds, origin, display_id) = {
            let (bounds, display_id) = self.card_bounds(rows.len());
            (bounds, bounds.origin, display_id)
        };
        #[cfg(not(target_os = "windows"))]
        let (bounds, origin, display_id) = {
            let card = card_size(rows.len());
            let origin = window_origin(resolve_origin(cx, remembered, card));
            let bounds = Bounds::new(origin, window_size(rows.len()));
            (bounds, origin, None)
        };
        #[cfg(target_os = "windows")]
        let _ = remembered;
        // A Wayland compositor places ordinary windows itself, so ask for a
        // layer-shell surface first (borderless, anchored bottom-right). An
        // X11 session, or a compositor without wlr-layer-shell, gets the
        // ordinary window.
        let mode = self.effective_mode();
        let settings_mode = self.settings.display_mode;
        let preference = self.settings.appearance;
        let attempts = window_attempts(mode, bounds, display_id);
        let mut layer_shell = false;
        let appearance = Arc::clone(&self.appearance);
        let pinned = Rc::clone(&self.pinned);
        let opened = crate::platform::open_first(
            attempts.into_iter().map(|options| vec![options]).collect(),
            |options| {
                layer_shell = is_layer_shell(&options.kind);
                cx.open_window(options, |_, cx| {
                    cx.new(|_| {
                        FlowIndicatorView::new_with_visible(
                            rows.to_vec(),
                            hidden,
                            Arc::clone(&appearance),
                            Rc::clone(&pinned),
                            visible,
                        )
                    })
                })
            },
        );
        let Some(handle) = opened else {
            return;
        };
        #[cfg(target_os = "windows")]
        let placement = self.target_placement(&crate::platform::monitor_layout());
        let configured = handle.update(cx, |view, window, cx| {
            view.mode = settings_mode;
            view.appearance_preference = preference;
            view.stale = self.agent.stale;
            view.saves_placement = true;
            apply_display_mode(window, mode);
            view.observe_appearance(window, cx);
            view.refresh_shadow(window, cx);
            view.refresh_value_shadows(window, cx);
            #[cfg(target_os = "windows")]
            view.on_opened(window, origin, placement.as_ref());
            #[cfg(not(target_os = "windows"))]
            view.on_opened(window, origin);
            // X11's card is always a managed toplevel; its initial stacking is
            // the live EWMH state, not part of the window kind.
            #[cfg(target_os = "linux")]
            view.apply_topmost(mode == FlowIndicatorDisplayMode::Topmost);
        });
        if let Err(error) = configured {
            debug!(%error, "Flow Indicator: could not configure its window");
        }
        info!(
            rows = rows.len(),
            ?mode,
            layer_shell,
            "Flow Indicator card opened"
        );
        self.window = Some(handle);
        self.window_mode = Some(mode);
        self.layer_shell = layer_shell;
        #[cfg(target_os = "windows")]
        {
            // A window opened during a display transition can be moved by
            // Windows after `on_opened` placed it. Keep re-anchoring to the
            // saved spot for the same settling window as a topology change.
            self.placement_settle_until = Some(Instant::now() + DISPLAY_SETTLE);
            self.topology_reopen_pending = false;
        }
    }
}

#[cfg(target_os = "windows")]
fn window_options(
    _mode: FlowIndicatorDisplayMode,
    bounds: Bounds<gpui::Pixels>,
    display_id: Option<DisplayId>,
) -> WindowOptions {
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
        display_id,
        ..WindowOptions::default()
    }
}

/// macOS and the other non-Linux Unices carry the display mode in the window
/// kind: a Normal window can be covered, a PopUp floats above them.
#[cfg(not(any(target_os = "windows", target_os = "linux")))]
fn window_options(
    mode: FlowIndicatorDisplayMode,
    bounds: Bounds<gpui::Pixels>,
    display_id: Option<DisplayId>,
) -> WindowOptions {
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
        display_id,
        ..WindowOptions::default()
    }
}

/// Linux always opens the card as a managed toplevel: on X11 the stacking is
/// the live `_NET_WM_STATE_ABOVE` switch, and a plain Wayland toplevel has no
/// stacking to switch at all. Only a layer surface still encodes the mode in
/// its kind (the layer), and only that path reopens the card for a mode
/// change.
#[cfg(target_os = "linux")]
fn window_options(
    _mode: FlowIndicatorDisplayMode,
    bounds: Bounds<gpui::Pixels>,
    display_id: Option<DisplayId>,
) -> WindowOptions {
    WindowOptions {
        titlebar: None,
        focus: false,
        show: true,
        kind: WindowKind::Normal,
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
        display_id,
        ..WindowOptions::default()
    }
}

/// Window options to try in order for the card.
fn window_attempts(
    mode: FlowIndicatorDisplayMode,
    bounds: Bounds<gpui::Pixels>,
    display_id: Option<DisplayId>,
) -> Vec<WindowOptions> {
    #[cfg(target_os = "linux")]
    if let Some(options) = layer_shell_window_options(mode, bounds) {
        return vec![options, window_options(mode, bounds, display_id)];
    }
    vec![window_options(mode, bounds, display_id)]
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
    // The visible card sits SHADOW_MARGIN inside the surface; keep its own
    // screen margin at MARGIN rather than adding the two.
    let edge = (MARGIN - SHADOW_MARGIN).max(0.0);
    Some(WindowOptions {
        kind: WindowKind::LayerShell(LayerShellOptions {
            namespace: "openlogi-flow-indicator".to_string(),
            layer,
            anchor: Anchor::BOTTOM | Anchor::RIGHT,
            margin: Some((px(0.0), px(edge), px(edge), px(0.0))),
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

/// The card window's Win32 handle, for the style and z-order adjustments the
/// platform's window kinds cannot express.
#[cfg(target_os = "windows")]
fn card_hwnd(window: &Window) -> Option<windows_sys::Win32::Foundation::HWND> {
    use windows_sys::Win32::Foundation::HWND;

    let handle = raw_window_handle::HasWindowHandle::window_handle(window).ok()?;
    let raw_window_handle::RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return None;
    };
    Some(handle.hwnd.get() as HWND)
}

/// Whether the card window's native client area is not its design size at the
/// window's current scale.
///
/// GPUI updates only its scale factor when Windows reports a DPI change; the
/// window keeps the physical size it was created with, so after a display-scale
/// change or a move to another monitor the layout is larger than the window and
/// the right and bottom are clipped.
#[cfg(target_os = "windows")]
#[expect(
    unsafe_code,
    reason = "a client-area read on the live GPUI window's HWND, on its UI thread"
)]
fn card_size_is_stale(window: &Window, expected: Size<gpui::Pixels>) -> bool {
    use windows_sys::Win32::Foundation::RECT;
    use windows_sys::Win32::UI::WindowsAndMessaging::GetClientRect;

    let Some(hwnd) = card_hwnd(window) else {
        return false;
    };
    let mut rect = RECT::default();
    // SAFETY: a client-area read on the live GPUI window's HWND, on its UI thread.
    if unsafe { GetClientRect(hwnd, &raw mut rect) } == 0 {
        return false;
    }
    let client = size(
        DevicePixels(rect.right - rect.left),
        DevicePixels(rect.bottom - rect.top),
    );
    let expected = expected.to_device_pixels(window.scale_factor());
    // One device pixel of slack: a rounding difference must not turn the
    // every-second tick into a resize loop.
    (client.width.0 - expected.width.0).abs() > 1 || (client.height.0 - expected.height.0).abs() > 1
}

/// Whether the card's window currently carries the topmost bit.
#[cfg(target_os = "windows")]
#[expect(
    unsafe_code,
    reason = "an extended-style read on the live GPUI window on its UI thread"
)]
fn window_is_topmost(window: &Window) -> Option<bool> {
    let hwnd = card_hwnd(window)?;
    // SAFETY: a style read on the live HWND owned by `window`.
    let style = unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::GetWindowLongW(
            hwnd,
            windows_sys::Win32::UI::WindowsAndMessaging::GWL_EXSTYLE,
        )
    };
    Some(style & TOPMOST_BIT != 0)
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

/// Move the live card above (or back below) the other windows. `HWND_TOPMOST`
/// sets the `WS_EX_TOPMOST` bit itself, so no style edit rides along.
#[cfg(target_os = "windows")]
#[expect(
    unsafe_code,
    reason = "a documented z-order change on the live GPUI window, on its UI thread"
)]
fn set_topmost(window: &Window, topmost: bool) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        HWND_NOTOPMOST, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SetWindowPos,
    };

    let Some(hwnd) = card_hwnd(window) else {
        return;
    };
    // SAFETY: a documented z-order change on the live HWND owned by `window`,
    // on the thread GPUI requires.
    unsafe {
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

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;
    use std::sync::Arc;

    use super::{
        SavedPosition, anchored_origin, area_for_point, card_input_bounds, card_needs_display,
        card_size, clamp_origin, click_through_region, pin_input_bounds, window_size,
    };
    use gpui::{Bounds, point, px, size};

    /// A row with neither an offline word nor a charging bolt.
    fn no_status() -> super::StatusGlyph {
        super::StatusGlyph {
            word: None,
            word_x: 0.0,
            word_baseline: 0.0,
            bolt_x: None,
        }
    }

    /// The card asks for a display exactly while it is wanted and has no
    /// window to show it through: an already-open window proves the display
    /// is connected, and a dismissed or empty card gives Windows nothing to
    /// draw on — it stays headless.
    #[test]
    fn the_card_asks_for_a_display_only_while_it_still_needs_a_window() {
        assert!(card_needs_display(false, false, true, 2, true));
        assert!(
            card_needs_display(false, false, true, 0, false),
            "the first observation has not arrived yet and decides the rows"
        );
        assert!(!card_needs_display(true, false, true, 2, true));
        assert!(!card_needs_display(false, true, true, 2, true));
        assert!(!card_needs_display(false, false, false, 2, true));
        assert!(!card_needs_display(false, false, true, 0, true));
    }

    #[test]
    fn the_card_height_follows_the_row_count() {
        // Two 1 px borders, the 14 px padding top and bottom, the 20 px pin
        // lane plus its 6 px gap, 26+9+6 per row, and the 16 px gap between
        // rows.
        assert_eq!(f32::from(card_size(1).height), 2.0 + 28.0 + 26.0 + 41.0);
        assert_eq!(
            f32::from(card_size(3).height),
            2.0 + 28.0 + 26.0 + 3.0 * 41.0 + 2.0 * 16.0
        );
        assert_eq!(
            f32::from(card_size(5).height),
            2.0 + 28.0 + 26.0 + 5.0 * 41.0 + 4.0 * 16.0
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
    fn the_card_input_region_excludes_the_shadow_halo() {
        let input = card_input_bounds(3);
        assert_eq!(
            input.origin,
            point(px(super::SHADOW_MARGIN), px(super::SHADOW_MARGIN))
        );
        assert_eq!(input.size, card_size(3));
    }

    /// A pin-swap replacement is opened hidden above the old card; it must not
    /// ask for the pin's hole until it is revealed, or clicks during the
    /// overlap land on an empty view instead of the card still on screen.
    #[test]
    fn a_hidden_replacement_asks_for_no_input_region() {
        assert_eq!(click_through_region(false, true, 3), None);
        assert_eq!(click_through_region(false, false, 3), None);
        assert_eq!(
            click_through_region(true, true, 3),
            Some(pin_input_bounds())
        );
        assert_eq!(
            click_through_region(true, false, 3),
            Some(card_input_bounds(3))
        );
    }

    #[test]
    fn the_window_leaves_room_for_the_card_shadow() {
        let card = card_size(3);
        let window = window_size(3);
        assert_eq!(
            window,
            size(
                card.width + px(2.0 * super::SHADOW_MARGIN),
                card.height + px(2.0 * super::SHADOW_MARGIN),
            )
        );
    }

    #[test]
    fn a_saved_origin_uses_the_display_that_contains_it() {
        let primary = Bounds::new(point(px(0.0), px(0.0)), size(px(1920.0), px(1080.0)));
        let secondary = Bounds::new(point(px(1920.0), px(0.0)), size(px(2560.0), px(1440.0)));
        let areas = [primary, secondary];

        assert_eq!(
            area_for_point(&areas, Some(primary), point(px(2200.0), px(300.0))),
            Some(secondary)
        );
        assert_eq!(
            area_for_point(&areas, Some(primary), point(px(5000.0), px(300.0))),
            Some(primary),
            "a point on no connected display falls back to the primary"
        );
        assert_eq!(area_for_point(&[], None, point(px(10.0), px(10.0))), None);
    }

    #[test]
    fn a_bottom_anchored_card_keeps_its_bottom_edge_when_it_grows() {
        let area = Bounds::new(point(px(0.0), px(0.0)), size(px(1920.0), px(1080.0)));
        let old = Bounds::new(point(px(1596.0), px(845.0)), size(px(300.0), px(211.0)));
        let new_card = size(px(300.0), px(325.0));

        assert_eq!(
            anchored_origin(Some(area), old, new_card),
            point(px(1596.0), px(731.0))
        );
        assert_eq!(
            anchored_origin(Some(area), old, new_card).y + new_card.height,
            old.origin.y + old.size.height
        );
    }

    #[test]
    fn a_card_away_from_the_bottom_or_edge_keeps_its_top_left() {
        let area = Bounds::new(point(px(0.0), px(0.0)), size(px(1920.0), px(1080.0)));
        let old = Bounds::new(point(px(100.0), px(100.0)), size(px(300.0), px(211.0)));

        assert_eq!(
            anchored_origin(Some(area), old, size(px(300.0), px(325.0))),
            point(px(100.0), px(100.0))
        );
    }

    #[test]
    fn a_growing_card_is_kept_inside_its_display() {
        let area = Bounds::new(point(px(0.0), px(0.0)), size(px(1920.0), px(1080.0)));
        let old = Bounds::new(point(px(100.0), px(800.0)), size(px(300.0), px(211.0)));

        assert_eq!(
            anchored_origin(Some(area), old, size(px(300.0), px(325.0))),
            point(px(100.0), px(755.0)),
            "the top moves up just enough to keep the bottom inside"
        );
    }

    #[test]
    fn the_saved_position_roundtrips_through_toml() {
        let positions = vec![
            super::SavedSpot {
                display: r"\\.\DISPLAY1".to_string(),
                x: 2178.0,
                y: 1120.0,
            },
            super::SavedSpot {
                display: r"\\.\DISPLAY56".to_string(),
                x: 1600.0,
                y: 586.0,
            },
        ];
        let body = toml::to_string(&SavedPosition {
            x: 42.5,
            y: -7.25,
            pinned: true,
            physical: true,
            display: Some(r"\\.\DISPLAY1".to_string()),
            positions: positions.clone(),
        })
        .expect("serialize");
        let saved: SavedPosition = toml::from_str(&body).expect("parse");
        assert_eq!((saved.x, saved.y, saved.pinned), (42.5, -7.25, true));
        assert_eq!(
            (saved.physical, saved.display.as_deref()),
            (true, Some(r"\\.\DISPLAY1"))
        );
        assert_eq!(saved.positions, positions);
    }

    #[test]
    fn a_position_file_from_before_pinning_keeps_the_card_unpinned() {
        let saved: SavedPosition = toml::from_str("x = 10.0\ny = 20.0").expect("parse");
        assert!(!saved.pinned);
        assert!(!saved.physical, "an old file is still logical coordinates");
        assert_eq!(saved.display, None);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn switching_back_to_a_display_uses_that_displays_own_spot() {
        use std::collections::BTreeMap;

        use super::{FlowIndicator, PhysicalPlacement};
        use crate::platform::MonitorInfo;
        use gpui::{Bounds, DevicePixels, DisplayId, point, size};

        fn monitor(device: &str, origin: (i32, i32), extent: (i32, i32)) -> MonitorInfo {
            MonitorInfo {
                device: device.to_string(),
                display_id: DisplayId::from(1),
                monitor: Bounds::new(
                    point(DevicePixels(origin.0), DevicePixels(origin.1)),
                    size(DevicePixels(extent.0), DevicePixels(extent.1)),
                ),
                work: Bounds::new(
                    point(DevicePixels(origin.0), DevicePixels(origin.1)),
                    size(DevicePixels(extent.0), DevicePixels(extent.1)),
                ),
                dpi: 120,
            }
        }

        let physical = monitor(r"\\.\DISPLAY1", (0, 0), (2560, 1440));
        let virtual_display = monitor(r"\\.\DISPLAY56", (0, 0), (1920, 1080));
        let active = PhysicalPlacement {
            origin: point(DevicePixels(1600), DevicePixels(586)),
            display: Some(r"\\.\DISPLAY56".to_string()),
        };
        let mut positions = BTreeMap::new();
        positions.insert(
            r"\\.\DISPLAY1".to_string(),
            point(DevicePixels(2178), DevicePixels(1120)),
        );
        positions.insert(
            r"\\.\DISPLAY56".to_string(),
            point(DevicePixels(1600), DevicePixels(586)),
        );

        let target = FlowIndicator::target_placement_from(&active, &[physical], &positions);
        assert_eq!(
            target,
            PhysicalPlacement {
                origin: point(DevicePixels(2178), DevicePixels(1120)),
                display: Some(r"\\.\DISPLAY1".to_string()),
            },
            "the physical display's own spot wins when the virtual display is gone"
        );

        let target = FlowIndicator::target_placement_from(
            &active,
            &[
                monitor(r"\\.\DISPLAY1", (0, 0), (2560, 1440)),
                virtual_display,
            ],
            &positions,
        );
        assert_eq!(
            target, active,
            "while its own display is present, the active spot stays"
        );

        positions.remove(r"\\.\DISPLAY1");
        let target = FlowIndicator::target_placement_from(
            &active,
            &[monitor(r"\\.\DISPLAY1", (0, 0), (2560, 1440))],
            &positions,
        );
        assert_eq!(
            target, active,
            "a display with no remembered spot keeps the active point for clamping"
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn a_legacy_windows_file_needs_a_physical_rewrite() {
        let saved: SavedPosition = toml::from_str("x = 10.0\ny = 20.0").expect("parse");
        let (placement, needs_save) = saved.physical_placement();
        assert_eq!(
            (placement.origin, placement.display.as_deref(), needs_save),
            (
                point(gpui::DevicePixels(10), gpui::DevicePixels(20)),
                None,
                true
            ),
            "the point is kept as seen and the tick rewrites it physically"
        );

        let saved: SavedPosition = toml::from_str(
            "x = 10.0\ny = 20.0\npinned = true\nphysical = true\ndisplay = '\\\\.\\DISPLAY1'",
        )
        .expect("parse");
        let (placement, needs_save) = saved.physical_placement();
        assert_eq!(
            (placement.origin, placement.display.as_deref(), needs_save),
            (
                point(gpui::DevicePixels(10), gpui::DevicePixels(20)),
                Some(r"\\.\DISPLAY1"),
                false
            )
        );
    }

    /// A dropped connection must not flash a stale marker on every supervisor
    /// restart, but frozen readings must not pass for live ones either.
    #[test]
    fn the_card_marks_frozen_readings_stale_only_after_the_grace_period() {
        use super::{AgentLink, STALE_AFTER};
        use std::time::{Duration, Instant};

        let mut link = AgentLink::default();
        let start = Instant::now();
        assert!(
            !link.settle(start),
            "a live connection has nothing to settle"
        );

        link.lost(start);
        link.lost(start + Duration::from_secs(1));
        assert!(
            !link.settle(start + STALE_AFTER.saturating_sub(Duration::from_millis(1))),
            "a restart inside the grace period must not flash the marker"
        );
        assert!(link.settle(start + STALE_AFTER));
        assert!(link.stale);
        assert!(
            !link.settle(start + STALE_AFTER + Duration::from_secs(30)),
            "the latch paints once, not on every tick"
        );

        link.observed();
        assert!(
            !link.stale,
            "a fresh snapshot makes the readings live again"
        );
        assert_eq!(link.lost_since, None);
    }

    /// The pinned card's edge only separates glyphs if it sits on the
    /// opposite side of the card's text; one near the text's own lightness
    /// reads as a bolder weight and smears the letters together.
    #[test]
    fn the_ring_ink_contrasts_the_card_text() {
        use super::{Palette, shadow_ink};

        let text = Palette::for_dark(true).text;
        let ring = shadow_ink().ring;
        assert!(
            (ring.l - text.l).abs() > 0.5,
            "ring lightness {} vs text {}",
            ring.l,
            text.l
        );
    }

    /// The contact shadow must start outside the hairline and stay dense:
    /// a copy on the glyph's edge reads as a bolder weight, and a sparse ring
    /// farther out shows its separate stamps as scallops.
    #[test]
    fn the_contact_kernel_stays_between_the_hairline_and_the_row_gap() {
        use super::{CONTACT_KERNEL, RING_DIRECTIONS};

        let radius = |(dx, dy): (f32, f32)| (dx * dx + dy * dy).sqrt();
        let ring_reach = RING_DIRECTIONS
            .iter()
            .map(|&(dx, dy)| radius((dx, dy)))
            .fold(0.0_f32, f32::max);
        let mut contact_min = f32::INFINITY;
        let mut contact_max = 0.0_f32;
        for &(dx, dy, alpha) in &CONTACT_KERNEL {
            let reach = radius((dx, dy));
            contact_min = contact_min.min(reach);
            contact_max = contact_max.max(reach);
            assert!(
                (0.04..=0.22).contains(&alpha),
                "contact alpha {alpha} is outside the falloff"
            );
        }
        assert!(
            contact_min > ring_reach,
            "contact at {contact_min} starts inside the hairline at {ring_reach}"
        );
        assert!(
            contact_max <= 3.0,
            "contact at {contact_max} leaves the dense near rings"
        );
    }

    /// The baked text documents must stay the faint tail: the near field is
    /// the glyph copies, and a saturated group would plate a solid shadow
    /// behind the glyphs again. The bars are not part of this document; they
    /// carry their own shadow on the track.
    #[test]
    fn the_baked_shadow_carries_only_the_ambient_tail() {
        use super::{
            Row, SHADOW_OPACITY_NAMES, VALUE_SHADOW_OPACITY, shadow_svg, value_shadow_source,
        };
        use openlogi_core::device::{BatteryColor, DeviceKind};

        let row = Row {
            name: "MX Vertical".into(),
            value: "55%".into(),
            fill: Some(55),
            color: BatteryColor::Amber,
            charging: false,
            online: true,
            kind: DeviceKind::Mouse,
        };
        let card = shadow_svg(
            &[row],
            &["MX Vertical".to_string()],
            &[19.0],
            &[no_status()],
            "Segoe UI",
        );
        assert!(
            card.contains(&format!("opacity=\"{SHADOW_OPACITY_NAMES}\"")),
            "the names group lost its ambient opacity: {card}"
        );
        assert!(
            !card.contains(r#"width="268""#),
            "the bars must stay on the track's own shadow: {card}"
        );
        let reading = value_shadow_source("55%", 19.0, "Segoe UI");
        assert!(
            reading.contains(&format!("opacity=\"{VALUE_SHADOW_OPACITY}\"")),
            "the reading lost its ambient opacity: {reading}"
        );
        // The text groups have contact copies to carry their near field, so
        // their baked opacity must stay a tail rather than a full plate.
        const {
            assert!(
                SHADOW_OPACITY_NAMES < 0.7 && VALUE_SHADOW_OPACITY < 0.7,
                "a baked text group at the contact's density is the near field again"
            );
        };
    }

    /// The bar shadow must be its own fixed asset: one document, no text, and
    /// the same resvg falloff as the card's text shadows.
    #[test]
    fn the_bar_shadow_is_served_from_its_own_asset() {
        use super::{BAR_SHADOW_PATH, bar_shadow_source, is_shadow_asset, take_shadow_asset};

        assert!(is_shadow_asset(BAR_SHADOW_PATH));
        assert!(take_shadow_asset(BAR_SHADOW_PATH).is_some());
        let svg = bar_shadow_source();
        assert!(svg.contains(r#"width="268""#), "{svg}");
        assert!(svg.contains(r#"stdDeviation="2.5""#), "{svg}");
        assert!(svg.contains(r#"stdDeviation="9""#), "{svg}");
        assert!(
            !svg.contains("<text"),
            "the bar asset must be text-free: {svg}"
        );
    }

    /// The pinned shadow must reach GPUI without text: a `<text>` node makes
    /// GPUI's SVG renderer build the whole system font database on the UI
    /// thread, which is the stall the first pin used to pay.
    #[test]
    fn baking_the_shadow_leaves_no_text_for_gpui_to_resolve() {
        let raw = r#"<svg xmlns="http://www.w3.org/2000/svg" width="60" height="20"><text x="2" y="12" font-family="sans-serif" font-size="12">MX</text></svg>"#;
        assert!(raw.contains("<text"));
        let baked = super::bake_shadow_svg(raw).expect("bake the shadow");
        assert!(!baked.contains("<text"), "text survived the bake: {baked}");
        assert!(baked.contains("<svg"), "the baked asset must stay an SVG");
        // A host with any font must also get the names back as paths; the
        // family fallback is what keeps Linux's missing Segoe UI from
        // silently dropping them.
        if super::font_database().faces().next().is_some() {
            assert!(
                baked.contains("<path"),
                "the names should become paths: {baked}"
            );
        }
    }

    /// The name shadow must ask for the face the card paints. Windows paints
    /// the MEDIUM name with Segoe UI's Semibold face, while a 500 bake lands
    /// on Regular and shapes a narrower, thinner line that stops short of the
    /// name's right end.
    #[test]
    fn the_name_shadow_asks_for_the_painted_face() {
        use super::{Row, shadow_svg};
        use openlogi_core::device::{BatteryColor, DeviceKind};

        let row = Row {
            name: "MX Vertical".into(),
            value: "55%".into(),
            fill: Some(55),
            color: BatteryColor::Amber,
            charging: false,
            online: true,
            kind: DeviceKind::Mouse,
        };
        let svg = shadow_svg(
            &[row],
            &["MX Vertical".to_string()],
            &[19.0],
            &[no_status()],
            "Segoe UI",
        );
        #[cfg(target_os = "windows")]
        assert!(svg.contains(r#"font-weight="600""#), "{svg}");
        #[cfg(not(target_os = "windows"))]
        assert!(svg.contains(r#"font-weight="500""#), "{svg}");
    }

    /// The baked shadow must draw the name the card actually paints, not the
    /// untruncated device name: otherwise its glow continues past the
    /// ellipsis into the reading's lane.
    #[test]
    fn the_shadow_draws_the_painted_name_only() {
        use super::{Row, shadow_svg};
        use openlogi_core::device::{BatteryColor, DeviceKind};

        let row = Row {
            name: "A Very Long Device Name That Cannot Fit".into(),
            value: "55%".into(),
            fill: Some(55),
            color: BatteryColor::Amber,
            charging: false,
            online: true,
            kind: DeviceKind::Mouse,
        };
        let svg = shadow_svg(
            &[row],
            &["A Very Long Device…".to_string()],
            &[19.0],
            &[no_status()],
            "Segoe UI",
        );
        assert!(svg.contains("A Very Long Device…"), "{svg}");
        assert!(
            !svg.contains("That Cannot Fit"),
            "the full name leaked into the shadow: {svg}"
        );
    }

    /// A name that does not fit the cell is truncated with GPUI's own
    /// ellipsis before the shadow is baked; a fitting name is left alone.
    #[gpui::test]
    fn a_long_name_is_truncated_before_its_shadow(cx: &mut gpui::TestAppContext) {
        use super::{
            FlowIndicatorView, NAME_FONT_WEIGHT, NAME_TEXT_SIZE, Row, name_cell_width,
            painted_name, shaped_text_width,
        };
        use openlogi_core::device::{BatteryColor, DeviceKind};

        let row = |name: &str| Row {
            name: name.into(),
            value: "100%".into(),
            fill: Some(100),
            color: BatteryColor::Green,
            charging: false,
            online: true,
            kind: DeviceKind::Mouse,
        };
        let pinned = Rc::new(Cell::new(false));
        let (_view, cx) = cx.add_window_view(|_, _| {
            FlowIndicatorView::new(
                vec![row("MX Keys")],
                0,
                Arc::new(crate::appearance::Appearance::new()),
                Rc::clone(&pinned),
            )
        });
        cx.update(|window, _| {
            let text_system = window.text_system();
            let short = row("MX Keys");
            assert_eq!(painted_name(text_system, &short), "MX Keys");

            let long = row("A Very Long Device Name That Cannot Fit In The Card");
            let painted = painted_name(text_system, &long);
            assert!(painted.ends_with('…'), "{painted}");
            assert!(
                painted.len() < long.name.len(),
                "a long name must lose characters: {painted}"
            );
            assert!(
                shaped_text_width(text_system, &painted, NAME_TEXT_SIZE, NAME_FONT_WEIGHT)
                    <= name_cell_width(text_system, &long) + 0.5,
                "the painted name must fit its cell: {painted}"
            );
        });
    }

    /// The offline word and the charging bolt wear the row's own scale and
    /// join the card's ambient document, so every glyph inside a row stands
    /// on one shadow language.
    #[gpui::test]
    fn the_status_glyphs_join_the_row_shadow(cx: &mut gpui::TestAppContext) {
        use super::{
            BOLT_SIZE, FlowIndicatorView, NAME_FONT_WEIGHT, NAME_TEXT_SIZE, Row, shadow_svg,
            shadow_weight, status_glyph,
        };
        use openlogi_core::device::{BatteryColor, DeviceKind};

        let row = |online, charging| Row {
            name: "MX Master 3S".into(),
            value: "15%".into(),
            fill: Some(15),
            color: BatteryColor::Red,
            charging,
            online,
            kind: DeviceKind::Mouse,
        };
        let pinned = Rc::new(Cell::new(false));
        let (_view, cx) = cx.add_window_view(|_, _| {
            FlowIndicatorView::new(
                vec![row(true, false)],
                0,
                Arc::new(crate::appearance::Appearance::new()),
                Rc::clone(&pinned),
            )
        });
        cx.update(|window, _| {
            let text_system = window.text_system();
            let word = rust_i18n::t!("device.offline").to_string();

            let offline = row(false, false);
            let status = status_glyph(text_system, &offline, false);
            assert_eq!(status.word.as_deref(), Some(word.as_str()));
            assert!(status.bolt_x.is_none());
            let svg = shadow_svg(
                &[offline],
                &["MX Master 3S".to_string()],
                &[19.0],
                &[status],
                "Segoe UI",
            );
            assert!(
                svg.contains(&word),
                "the offline word must join the ambient: {svg}"
            );
            assert_eq!(
                svg.matches(&format!(r#"font-size="{NAME_TEXT_SIZE}""#))
                    .count(),
                2,
                "the name and the offline word share one type scale: {svg}"
            );
            assert!(
                svg.contains(&format!(
                    r#"font-weight="{}""#,
                    shadow_weight(NAME_FONT_WEIGHT)
                )),
                "the offline word must wear the painted face: {svg}"
            );

            let status = status_glyph(text_system, &row(true, true), false);
            assert!(status.bolt_x.is_some());
            let svg = shadow_svg(
                &[row(true, true)],
                &["MX Master 3S".to_string()],
                &[19.0],
                &[status],
                "Segoe UI",
            );
            assert!(
                svg.contains(&format!("scale({})", BOLT_SIZE / 32.0)),
                "the bolt must join the ambient at its own box: {svg}"
            );

            // A stale card hides the bolt on an online row; the ambient has
            // to follow the paint.
            assert!(
                status_glyph(text_system, &row(true, true), true)
                    .bolt_x
                    .is_none()
            );
        });
    }

    /// The regular card follows the settings app's preference; only `System`
    /// consults the desktop, and the watched mode wins over a stale window.
    #[test]
    fn the_regular_card_resolves_its_scheme_from_the_panel_preference() {
        use super::FlowIndicatorView;
        use openlogi_core::config::Appearance;

        assert!(!FlowIndicatorView::resolve_dark(
            Appearance::Light,
            Some(true),
            true
        ));
        assert!(FlowIndicatorView::resolve_dark(
            Appearance::Dark,
            Some(false),
            false
        ));
        assert!(FlowIndicatorView::resolve_dark(
            Appearance::System,
            Some(true),
            false
        ));
        assert!(!FlowIndicatorView::resolve_dark(
            Appearance::System,
            Some(false),
            true
        ));
        // No portal answer yet: the window's own report is the fallback.
        assert!(FlowIndicatorView::resolve_dark(
            Appearance::System,
            None,
            true
        ));
        assert!(!FlowIndicatorView::resolve_dark(
            Appearance::System,
            None,
            false
        ));
    }

    /// The card's copy follows the panel's language setting, not whatever
    /// locale this helper process happened to inherit.
    #[test]
    fn the_flow_indicator_settings_follow_the_panel_language() {
        let chinese = openlogi_core::config::AppSettings {
            language: Some("zh-CN".into()),
            ..Default::default()
        };
        assert_eq!(
            super::Settings::from(&chinese).language.as_deref(),
            Some("zh-CN")
        );
        let system = openlogi_core::config::AppSettings::default();
        assert_eq!(super::Settings::from(&system).language, None);
    }

    /// Each scheme has to keep its text legible on its own card; the pinned
    /// card never leaves the dark one.
    #[test]
    fn both_card_schemes_keep_the_text_on_the_card() {
        use super::Palette;

        for dark in [true, false] {
            let pal = Palette::for_dark(dark);
            assert!(
                (pal.text.l - pal.card.l).abs() > 0.5,
                "dark={dark}: text lightness {} vs card {}",
                pal.text.l,
                pal.card.l
            );
        }
    }

    /// The pin chip is the card's surface moved toward its glyph, never past
    /// either: it has to stay visible against both the card and the icon in
    /// the light scheme, the dark scheme, and the dark-only pinned HUD.
    #[test]
    fn the_pin_chip_lands_between_the_card_and_its_glyph() {
        use super::Palette;

        for dark in [true, false] {
            let pal = Palette::for_dark(dark);
            let hover = pal.pin_chip(false, false);
            let pressed = pal.pin_chip(false, true);
            let low = pal.card.l.min(pal.muted.l);
            let high = pal.card.l.max(pal.muted.l);
            assert!(
                hover.l > low && hover.l < high,
                "dark={dark}: hover {} must sit between card {} and glyph {}",
                hover.l,
                pal.card.l,
                pal.muted.l
            );
            assert!(
                pressed.l > low && pressed.l < high,
                "dark={dark}: pressed {} must stay between card {} and glyph {}",
                pressed.l,
                pal.card.l,
                pal.muted.l
            );
            assert!(
                (pressed.l - hover.l).abs() > f32::EPSILON,
                "dark={dark}: pressed must read stronger than hover"
            );
            assert!(
                (hover.a - 1.0).abs() < f32::EPSILON,
                "the chip is opaque so its painted colour is the chosen point"
            );
        }

        let pinned = Palette::for_dark(true);
        let hover = pinned.pin_chip(true, false);
        let low = pinned.card.l.min(pinned.text.l);
        let high = pinned.card.l.max(pinned.text.l);
        assert!(
            hover.l > low && hover.l < high,
            "pinned hover {} must sit between the dark scheme {} and its glyph {}",
            hover.l,
            pinned.card.l,
            pinned.text.l
        );
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
        let window = cx.add_window(|_, _| {
            FlowIndicatorView::new(
                rows,
                0,
                Arc::new(crate::appearance::Appearance::new()),
                Rc::new(Cell::new(false)),
            )
        });
        cx.run_until_parked();
        let shown = window
            .read_with(cx, |view, _| view.rows.len())
            .expect("window is live");
        assert_eq!(shown, 1);
    }

    /// A stale card must say so: the header grows an `Unavailable` tag, and it
    /// goes away the moment a fresh snapshot proves the agent is back.
    #[gpui::test]
    fn only_a_stale_card_shows_the_unavailable_tag(cx: &mut gpui::TestAppContext) {
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
        let (view, cx) = cx.add_window_view(|_, _| {
            FlowIndicatorView::new(
                rows,
                0,
                Arc::new(crate::appearance::Appearance::new()),
                Rc::new(Cell::new(false)),
            )
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(
            cx.debug_bounds("flow-indicator-stale").is_none(),
            "a live card carries no stale tag"
        );

        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.stale = true;
                cx.notify();
            });
            window.draw(cx).clear(cx);
        });
        assert!(
            cx.debug_bounds("flow-indicator-stale").is_some(),
            "frozen readings are marked"
        );

        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.stale = false;
                cx.notify();
            });
            window.draw(cx).clear(cx);
        });
        assert!(
            cx.debug_bounds("flow-indicator-stale").is_none(),
            "a reconnected agent takes the tag away"
        );
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
        let pinned = Rc::new(Cell::new(false));
        let (_view, cx) = cx.add_window_view(|_, _| {
            FlowIndicatorView::new(
                rows,
                0,
                Arc::new(crate::appearance::Appearance::new()),
                Rc::clone(&pinned),
            )
        });
        cx.simulate_resize(gpui::size(gpui::px(400.0), gpui::px(400.0)));
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let card = cx
            .debug_bounds("flow-indicator-card")
            .expect("the card renders");
        assert_eq!(card.size, window, "the card must fit its window");
        assert_eq!(
            card.origin,
            point(px(super::SHADOW_MARGIN), px(super::SHADOW_MARGIN)),
            "the visible card sits inside the shadow margin"
        );
    }

    /// The pinned card keeps the same footprint and still draws its pin
    /// button — the toggle has to survive its own transparency.
    #[gpui::test]
    fn the_pinned_card_keeps_the_pin_button(cx: &mut gpui::TestAppContext) {
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
        let pinned = Rc::new(Cell::new(true));
        let (_view, cx) = cx.add_window_view(|_, _| {
            FlowIndicatorView::new(
                rows,
                0,
                Arc::new(crate::appearance::Appearance::new()),
                Rc::clone(&pinned),
            )
        });
        cx.simulate_resize(gpui::size(gpui::px(400.0), gpui::px(400.0)));
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let card = cx
            .debug_bounds("flow-indicator-card")
            .expect("the card renders");
        let pin = cx
            .debug_bounds("flow-indicator-pin")
            .expect("the pin button renders");
        assert!(
            card.contains(&pin.origin),
            "the pin button must sit inside the card"
        );
        let hole = super::pin_button_bounds();
        assert_eq!(
            pin.origin,
            card.origin + hole.origin,
            "the click-through hole must land on the pin button"
        );
        assert_eq!(pin.size, hole.size, "the hole must be the button's size");
        assert_eq!(
            pin.origin,
            super::pin_input_bounds().origin,
            "the native input hole must include the shadow margin"
        );
    }

    /// Clicking the pin button flips the shared flag. The button sits inside
    /// the card's platform drag area, so this also proves its occluding hitbox
    /// keeps the click from being swallowed by the move handle.
    #[gpui::test]
    fn clicking_the_pin_button_toggles_the_shared_flag(cx: &mut gpui::TestAppContext) {
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
        let pinned = Rc::new(Cell::new(false));
        let (view, cx) = cx.add_window_view(|_, _| {
            FlowIndicatorView::new(
                rows,
                0,
                Arc::new(crate::appearance::Appearance::new()),
                Rc::clone(&pinned),
            )
        });
        cx.simulate_resize(gpui::size(gpui::px(400.0), gpui::px(400.0)));
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let pin = cx
            .debug_bounds("flow-indicator-pin")
            .expect("the pin button renders");
        cx.simulate_click(pin.center(), gpui::Modifiers::none());
        assert!(pinned.get(), "the click must reach the pin button");
        view.update(cx, |view, _| {
            assert!(view.pinned.get());
        });
    }

    /// The header hint is card content, not a transient highlight: once the
    /// pointer leaves the card it has to go away, including on the frame GPUI
    /// draws afterwards while it still hit-tests the last position inside the
    /// window.
    #[gpui::test]
    fn the_pin_hint_clears_when_the_pointer_leaves_the_card(cx: &mut gpui::TestAppContext) {
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
        let pinned = Rc::new(Cell::new(false));
        let (view, cx) = cx.add_window_view(|_, _| {
            FlowIndicatorView::new(
                rows,
                0,
                Arc::new(crate::appearance::Appearance::new()),
                Rc::clone(&pinned),
            )
        });
        cx.simulate_resize(gpui::size(gpui::px(400.0), gpui::px(400.0)));
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let pin = cx
            .debug_bounds("flow-indicator-pin")
            .expect("the pin button renders");

        cx.simulate_mouse_move(pin.center(), None, gpui::Modifiers::none());
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(
            view.update(cx, |view, _| view.pin_hovered),
            "moving onto the pin must show the hint"
        );
        // Windows also requires the platform's pointer-presence bit, which the
        // test window never raises.
        #[cfg(not(target_os = "windows"))]
        assert!(
            cx.debug_bounds("flow-indicator-pin-hint").is_some(),
            "the hint must paint while the pointer is on the pin"
        );

        cx.update(|window, cx| {
            window.dispatch_event(
                gpui::PlatformInput::MouseExited(gpui::MouseExitEvent {
                    position: pin.center(),
                    pressed_button: None,
                    modifiers: gpui::Modifiers::none(),
                }),
                cx,
            );
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(
            !view.update(cx, |view, _| view.pin_hovered),
            "the hint must not come back after the pointer left the card"
        );
        assert!(
            cx.debug_bounds("flow-indicator-pin-hint").is_none(),
            "the hint must not paint after the pointer left the card"
        );
    }

    /// The reading's shadow is keyed by the reading alone: rows showing the
    /// same value share one baked asset, and new digits are a new key.
    #[test]
    fn the_reading_shadow_is_keyed_by_the_reading_alone() {
        use super::{VALUE_SHADOW_PAD, value_shadow_hash, value_shadow_source};

        assert_ne!(value_shadow_hash("90%"), value_shadow_hash("91%"));
        let svg = value_shadow_source("90%", 19.0, "Segoe UI");
        assert!(svg.contains("<text"), "the source must carry the digits");
        assert!(
            svg.contains("text-anchor=\"end\""),
            "the digits have to right-align on the reading's edge"
        );
        assert!(
            svg.contains(&format!("y=\"{}\"", VALUE_SHADOW_PAD + 19.0)),
            "the digits must stand on the line's baseline, not the canvas centre: {svg}"
        );
    }

    /// The reading's asset reaches GPUI without text, like the card's: its SVG
    /// renderer would otherwise build the font database on the UI thread.
    #[test]
    fn baking_a_reading_shadow_leaves_no_text_for_gpui() {
        use super::{bake_shadow_svg, value_shadow_source};

        let raw = value_shadow_source("90%", 19.0, "Segoe UI");
        let baked = bake_shadow_svg(&raw).expect("bake the reading");
        assert!(!baked.contains("<text"), "text survived the bake: {baked}");
        if super::font_database().faces().next().is_some() {
            assert!(
                baked.contains("<path"),
                "the digits should become paths: {baked}"
            );
        }
    }

    /// Bake the readings a bench renders, so its glow element has an asset.
    /// The headless test platform serves no assets; the Windows capture does.
    /// The face and baselines come from the app's own text system, like the
    /// production bake.
    #[cfg(target_os = "windows")]
    fn bake_readings_for_test(text_system: &super::WindowTextSystem, rows: &[super::Row]) {
        let family = super::shadow_font_family(text_system);
        for row in rows {
            let baseline = super::line_baseline(
                text_system,
                &row.value,
                super::VALUE_TEXT_SIZE,
                super::VALUE_FONT_WEIGHT,
            );
            let raw = super::value_shadow_source(&row.value, baseline, &family);
            super::finish_value_bake(
                super::value_shadow_hash(&row.value),
                super::bake_shadow_svg(&raw).expect("bake a reading"),
            );
        }
    }

    /// Bake the pinned card's ambient exactly as `refresh_shadow` does, so a
    /// capture renders the document the card would.
    #[cfg(target_os = "windows")]
    fn bake_card_shadow_for_test(text_system: &super::WindowTextSystem, rows: &[super::Row]) {
        let family = super::shadow_font_family(text_system);
        let names = rows
            .iter()
            .map(|row| row.name.to_string())
            .collect::<Vec<_>>();
        let baselines = names
            .iter()
            .map(|name| {
                super::line_baseline(
                    text_system,
                    name,
                    super::NAME_TEXT_SIZE,
                    super::NAME_FONT_WEIGHT,
                )
            })
            .collect::<Vec<_>>();
        let statuses = rows
            .iter()
            .map(|row| super::status_glyph(text_system, row, false))
            .collect::<Vec<_>>();
        let hash = super::shadow_rows_hash(&names, rows, &statuses);
        assert!(
            super::claim_shadow_bake(hash),
            "the card shadow was already baked"
        );
        let baked = super::bake_shadow_svg(&super::shadow_svg(
            rows, &names, &baselines, &statuses, &family,
        ))
        .expect("bake the card shadow");
        super::finish_shadow_bake(hash, baked);
    }

    /// The real row tree with the card's chrome removed, so a frame's cost is
    /// the rows' own. Only the reading's glow differs between windows.
    #[cfg(target_os = "windows")]
    struct ShadowBench {
        rows: Vec<super::Row>,
        value_shadow: bool,
    }

    #[cfg(target_os = "windows")]
    impl gpui::Render for ShadowBench {
        fn render(
            &mut self,
            window: &mut gpui::Window,
            _: &mut gpui::Context<Self>,
        ) -> impl gpui::IntoElement {
            use gpui::{ParentElement as _, Styled as _, div};

            let pal = super::Palette::for_dark(true);
            let scale = window.scale_factor();
            div()
                .w(px(super::CARD_WIDTH))
                .flex()
                .flex_col()
                .gap(px(super::ROW_GAP))
                .children(self.rows.iter().map(|row| {
                    super::FlowIndicatorView::row_view_with(
                        row,
                        row.name.as_ref(),
                        &pal,
                        true,
                        scale,
                        self.value_shadow,
                        false,
                    )
                }))
        }
    }

    /// What the reading's glow costs the card, measured through the real
    /// Windows renderer on hidden windows: the rows are the production tree,
    /// the glow is the production asset, and the platform records the same
    /// scene it would present.
    ///
    /// The bare window warms the device first, so the glow window's first
    /// frame is mostly the blur's single raster; every frame after that draws
    /// a cached texture.
    ///
    /// `cargo test -p openlogi-overlay the_reading_glow_frame_cost -- --ignored --nocapture`
    #[cfg(target_os = "windows")]
    #[test]
    #[ignore = "timing measurement: real Windows renderer, hidden windows"]
    fn the_reading_glow_frame_cost() {
        use super::{Row, card_size, window_options};
        use gpui::AppContext as _;
        use openlogi_core::config::FlowIndicatorDisplayMode;
        use openlogi_core::device::{BatteryColor, DeviceKind};

        const FRAMES: usize = 120;

        let rows = ["MX Master 3S", "MX Vertical", "MX Keys"]
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
        let card = card_size(rows.len());

        gpui_platform::application()
            .with_assets(crate::OverlayAssets)
            .run(move |cx| {
                let text_system = gpui::WindowTextSystem::new(cx.text_system().clone());
                bake_readings_for_test(&text_system, &rows);
                let mut measure = |value_shadow: bool, label: &str| {
                    let mut options = window_options(
                        FlowIndicatorDisplayMode::Topmost,
                        Bounds::new(point(px(0.0), px(0.0)), card),
                        None,
                    );
                    options.show = false;
                    let handle = cx
                        .open_window(options, |_, cx| {
                            cx.new(|_| ShadowBench {
                                rows: rows.clone(),
                                value_shadow,
                            })
                        })
                        .expect("open the bench window");
                    cx.update_window(handle.into(), |_, window, cx| {
                        window.resize(card);
                        let cold = std::time::Instant::now();
                        window.draw(cx).clear(cx);
                        let cold = cold.elapsed();
                        let mut samples = Vec::with_capacity(FRAMES);
                        for _ in 0..FRAMES {
                            let started = std::time::Instant::now();
                            window.draw(cx).clear(cx);
                            samples.push(started.elapsed());
                        }
                        samples.sort_unstable();
                        println!(
                            "{label}: first frame {cold:?}, median {:?}/frame",
                            samples[samples.len() / 2]
                        );
                    })
                    .expect("the bench window is live");
                };
                measure(false, "bare");
                measure(true, "glow");
                cx.quit();
            });
    }

    /// Renders the pinned rows through the real Windows renderer and writes a
    /// PNG, without showing a window: the platform draws into its render
    /// target offscreen, so the capture is the card's actual pixels.
    ///
    /// `cargo test -p openlogi-overlay dump_the_pinned_reading_shadow -- --ignored --nocapture`
    #[cfg(target_os = "windows")]
    #[test]
    #[ignore = "visual capture: writes pinned-reading.png to the temp directory"]
    fn dump_the_pinned_reading_shadow() {
        use super::{Row, card_size, window_options};
        use gpui::AppContext as _;
        use openlogi_core::config::FlowIndicatorDisplayMode;
        use openlogi_core::device::{BatteryColor, DeviceKind};

        let rows = ["MX Master 3S", "MX Vertical", "MX Keys"]
            .into_iter()
            .map(|name| Row {
                name: name.into(),
                value: "27%".into(),
                fill: Some(27),
                color: BatteryColor::Red,
                charging: false,
                online: true,
                kind: DeviceKind::Mouse,
            })
            .collect::<Vec<_>>();
        let card = card_size(rows.len());
        let out = std::env::temp_dir().join("pinned-reading.png");

        gpui_platform::application()
            .with_assets(crate::OverlayAssets)
            .run(move |cx| {
                let text_system = gpui::WindowTextSystem::new(cx.text_system().clone());
                bake_readings_for_test(&text_system, &rows);
                let mut options = window_options(
                    FlowIndicatorDisplayMode::Topmost,
                    Bounds::new(point(px(0.0), px(0.0)), card),
                    None,
                );
                options.show = false;
                let handle = cx
                    .open_window(options, |_, cx| {
                        cx.new(|_| ShadowBench {
                            rows: rows.clone(),
                            value_shadow: true,
                        })
                    })
                    .expect("open the card window");
                handle
                    .update(cx, |_, window, _| {
                        window.set_window_title("OpenLogi pinned reading capture");
                    })
                    .expect("the card window is live");
                cx.update_window(handle.into(), |_, window, cx| {
                    window.resize(card);
                    window.draw(cx).clear(cx);
                    let image = window.render_to_image().expect("render the card");
                    image.save(&out).expect("write the capture");
                    println!("capture: {} ({:?})", out.display(), image.dimensions());
                })
                .expect("the card window is live");
                cx.quit();
            });
    }

    /// Renders the pinned card's charging and offline states — the ambient
    /// SVG, the contact copies and the hairline — through the real Windows
    /// renderer and writes a PNG. The rows are the production tree.
    ///
    /// `cargo test -p openlogi-overlay dump_the_pinned_status_rows -- --ignored --nocapture`
    #[cfg(target_os = "windows")]
    #[test]
    #[ignore = "visual capture: writes pinned-status.png to the temp directory"]
    fn dump_the_pinned_status_rows() {
        use super::{FlowIndicatorView, Row, window_options, window_size};
        use gpui::AppContext as _;
        use openlogi_core::config::FlowIndicatorDisplayMode;
        use openlogi_core::device::{BatteryColor, DeviceKind};
        use std::time::Duration;

        let rows = vec![
            Row {
                name: "MX Master 3S".into(),
                value: "15%".into(),
                fill: Some(15),
                color: BatteryColor::Red,
                charging: false,
                online: true,
                kind: DeviceKind::Mouse,
            },
            Row {
                name: "MX Keys".into(),
                value: "Charging".into(),
                fill: None,
                color: BatteryColor::Green,
                charging: true,
                online: true,
                kind: DeviceKind::Keyboard,
            },
            Row {
                name: "MX Anywhere 3S".into(),
                value: "33%".into(),
                fill: Some(33),
                color: BatteryColor::Amber,
                charging: false,
                online: false,
                kind: DeviceKind::Mouse,
            },
        ];
        let window_box = window_size(rows.len());
        let out = std::env::temp_dir().join("pinned-status.png");
        let pinned = Rc::new(Cell::new(true));

        gpui_platform::application()
            .with_assets(crate::OverlayAssets)
            .run(move |cx| {
                let text_system = gpui::WindowTextSystem::new(cx.text_system().clone());
                bake_readings_for_test(&text_system, &rows);
                bake_card_shadow_for_test(&text_system, &rows);
                let mut options = window_options(
                    FlowIndicatorDisplayMode::Topmost,
                    Bounds::new(point(px(0.0), px(0.0)), window_box),
                    None,
                );
                options.show = false;
                let handle: gpui::AnyWindowHandle = cx
                    .open_window(options, |_, cx| {
                        cx.new(|_| {
                            FlowIndicatorView::new(
                                rows.clone(),
                                0,
                                Arc::new(crate::appearance::Appearance::new()),
                                pinned.clone(),
                            )
                        })
                    })
                    .expect("open the status card window")
                    .into();
                cx.update_window(handle, |_, window, cx| {
                    window.resize(window_box);
                    window.draw(cx).clear(cx);
                })
                .expect("the status card window is live");
                let out = out.clone();
                cx.spawn(async move |cx| {
                    cx.background_executor()
                        .timer(Duration::from_millis(2_000))
                        .await;
                    cx.update_window(handle, |_, window, cx| {
                        window.draw(cx).clear(cx);
                        let image = window.render_to_image().expect("render the status rows");
                        image.save(&out).expect("write the status capture");
                        println!("capture: {} ({:?})", out.display(), image.dimensions());
                    })
                    .expect("the status card window is live");
                    cx.update(|cx| cx.quit());
                })
                .detach();
            });
    }

    /// Renders the whole pinned card — the ambient SVG, the contact copies and
    /// the hairline — through the real Windows renderer and writes a PNG,
    /// without showing a window. The image loader is asynchronous, so the
    /// first draw kicks the assets off and the capture waits for them.
    ///
    /// `cargo test -p openlogi-overlay dump_the_pinned_card -- --ignored --nocapture`
    #[cfg(target_os = "windows")]
    #[test]
    #[ignore = "visual capture: writes pinned-card.png to the temp directory"]
    fn dump_the_pinned_card() {
        use super::{FlowIndicatorView, Row, window_options, window_size};
        use gpui::AppContext as _;
        use openlogi_core::config::FlowIndicatorDisplayMode;
        use openlogi_core::device::{BatteryColor, DeviceKind};
        use std::cell::Cell;
        use std::rc::Rc;
        use std::time::Duration;

        let rows = vec![
            Row {
                name: "MX Master 3S".into(),
                value: "44%".into(),
                fill: Some(44),
                color: BatteryColor::Amber,
                charging: false,
                online: true,
                kind: DeviceKind::Mouse,
            },
            Row {
                name: "MX Vertical".into(),
                value: "55%".into(),
                fill: Some(55),
                color: BatteryColor::Amber,
                charging: false,
                online: true,
                kind: DeviceKind::Mouse,
            },
            Row {
                name: "MX Keys".into(),
                value: "100%".into(),
                fill: Some(100),
                color: BatteryColor::Green,
                charging: false,
                online: true,
                kind: DeviceKind::Keyboard,
            },
        ];
        let window_box = window_size(rows.len());
        let out = std::env::temp_dir().join("pinned-card.png");
        let pinned = Rc::new(Cell::new(true));

        gpui_platform::application()
            .with_assets(crate::OverlayAssets)
            .run(move |cx| {
                let text_system = gpui::WindowTextSystem::new(cx.text_system().clone());
                bake_readings_for_test(&text_system, &rows);
                bake_card_shadow_for_test(&text_system, &rows);

                let mut options = window_options(
                    FlowIndicatorDisplayMode::Topmost,
                    Bounds::new(point(px(0.0), px(0.0)), window_box),
                    None,
                );
                options.show = false;
                let handle: gpui::AnyWindowHandle = cx
                    .open_window(options, |_, cx| {
                        cx.new(|_| {
                            FlowIndicatorView::new(
                                rows.clone(),
                                0,
                                Arc::new(crate::appearance::Appearance::new()),
                                pinned.clone(),
                            )
                        })
                    })
                    .expect("open the card window")
                    .into();
                cx.update_window(handle, |_, window, cx| {
                    window.resize(window_box);
                    window.draw(cx).clear(cx);
                })
                .expect("the card window is live");
                let out = out.clone();
                cx.spawn(async move |cx| {
                    cx.background_executor()
                        .timer(Duration::from_millis(2_000))
                        .await;
                    cx.update_window(handle, |_, window, cx| {
                        window.draw(cx).clear(cx);
                        let image = window.render_to_image().expect("render the card");
                        image.save(&out).expect("write the capture");
                        println!("capture: {} ({:?})", out.display(), image.dimensions());
                    })
                    .expect("the card window is live");
                    cx.update(|cx| cx.quit());
                })
                .detach();
            });
    }
}
