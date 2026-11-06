//! The Win32 boundary: sample once, establish the HWND's target DPI while it
//! is still a 1px bootstrap, then place the window without activating it, so
//! that no later click activates it either. The ring and the notice share
//! both halves; only their final geometry differs.

use anyhow::{Context as _, Result, ensure};
use gpui::{App, Bounds, DevicePixels, DisplayId, Pixels, Point, Size, Window, point, size};
use raw_window_handle::{HasWindowHandle as _, RawWindowHandle};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, AtomicU64, Ordering};
use std::time::Instant;
use tracing::{debug, info, warn};
use windows_sys::Win32::Foundation::{HWND, LPARAM, POINT, RECT};
use windows_sys::Win32::Graphics::Dwm::{
    DWMWA_BORDER_COLOR, DWMWA_COLOR_NONE, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_DONOTROUND,
    DwmSetWindowAttribute,
};
use windows_sys::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITOR_DEFAULTTONEAREST,
    MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MONITORINFOEXW, MonitorFromPoint, MonitorFromWindow,
};
use windows_sys::Win32::UI::HiDpi::{GetDpiForMonitor, GetDpiForWindow, MDT_EFFECTIVE_DPI};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GW_HWNDPREV, GWL_EXSTYLE, GWL_STYLE, GetCursorPos, GetWindow, GetWindowLongW, GetWindowRect,
    IsWindow, IsWindowVisible, LWA_ALPHA, SW_HIDE, SW_SHOWNOACTIVATE, SWP_FRAMECHANGED,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SWP_SHOWWINDOW, SendMessageW,
    SetLayeredWindowAttributes, SetWindowLongW, SetWindowPos, ShowWindow, WM_DPICHANGED,
    WM_SETTINGCHANGE, WS_CAPTION, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TRANSPARENT, WS_POPUP,
    WS_THICKFRAME,
};
use windows_sys::core::BOOL;

use super::{MonitorInfo, NoticePlacement, RingPlacement};
use crate::flow_indicator::{HEADER_LANE, PADDING_X, PADDING_Y, SHADOW_MARGIN, TOPMOST_BIT};

impl RingPlacement {
    pub(crate) fn capture(_cx: &mut App) -> Result<Self> {
        let sample = sample_monitor()?;
        Ok(Self {
            display_id: sample.display_id,
            cursor: sample.cursor,
            display: sample.display,
            dpi: sample.dpi,
        })
    }

    pub(crate) fn show(self, window: &mut Window) -> Result<()> {
        place(window, self.display_id, self.display, |dpi| {
            self.bounds(dpi)
        })
    }
}

impl NoticePlacement {
    pub(crate) fn capture(_cx: &mut App, size: Size<Pixels>) -> Result<Self> {
        let sample = sample_monitor()?;
        Ok(Self {
            display_id: sample.display_id,
            display: sample.display,
            work: sample.work,
            dpi: sample.dpi,
            size,
        })
    }

    pub(crate) fn show(self, window: &mut Window) -> Result<()> {
        place(window, self.display_id, self.display, |dpi| {
            self.bounds(dpi)
        })
    }
}

/// One reading of the cursor and the monitor under it.
struct MonitorSample {
    display_id: DisplayId,
    /// The cursor, or the monitor's centre when it could not be read.
    cursor: Point<DevicePixels>,
    /// The whole monitor (`rcMonitor`).
    display: Bounds<DevicePixels>,
    /// The monitor without the taskbar (`rcWork`).
    work: Bounds<DevicePixels>,
    dpi: u32,
}

#[expect(
    unsafe_code,
    reason = "read-only Win32 cursor and monitor queries with checked outputs"
)]
fn sample_monitor() -> Result<MonitorSample> {
    let mut cursor = POINT::default();
    // SAFETY: cursor is writable for the duration of the call. This GPUI
    // process is PerMonitorV2-aware, so GetCursorPos returns physical pixels.
    let has_cursor = unsafe { GetCursorPos(&raw mut cursor) } != 0;
    let monitor = if has_cursor {
        // SAFETY: POINT is passed by value. Use this same sample for both
        // selection and positioning, never a second cursor read.
        unsafe { MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST) }
    } else {
        // SAFETY: a null HWND plus DEFAULTTOPRIMARY selects the primary.
        unsafe { MonitorFromWindow(std::ptr::null_mut(), MONITOR_DEFAULTTOPRIMARY) }
    };
    let mut info = MONITORINFO {
        cbSize: u32::try_from(std::mem::size_of::<MONITORINFO>())?,
        ..MONITORINFO::default()
    };
    // SAFETY: monitor is from a native query and info has the required size;
    // a removed monitor is reported as failure, not used as geometry.
    if unsafe { GetMonitorInfoW(monitor, &raw mut info) } == 0 {
        return Err(std::io::Error::last_os_error()).context("query overlay monitor bounds");
    }
    let display = device_bounds(info.rcMonitor);
    let (mut dpi, mut dpi_y) = (0, 0);
    // SAFETY: monitor is from a native query and both outputs are writable
    // for the duration of the call; a failed HRESULT is not used as a DPI.
    let hr = unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &raw mut dpi, &raw mut dpi_y) };
    ensure!(
        hr >= 0 && dpi != 0,
        "could not read overlay monitor DPI (HRESULT {hr:#x})"
    );
    Ok(MonitorSample {
        // The pinned GPUI Windows backend uses HMONITOR as DisplayId.
        display_id: DisplayId::from(monitor as usize as u64),
        cursor: if has_cursor {
            point(DevicePixels(cursor.x), DevicePixels(cursor.y))
        } else {
            display.center()
        },
        display,
        work: device_bounds(info.rcWork),
        dpi,
    })
}

fn device_bounds(rect: RECT) -> Bounds<DevicePixels> {
    Bounds::new(
        point(DevicePixels(rect.left), DevicePixels(rect.top)),
        size(
            DevicePixels(rect.right - rect.left),
            DevicePixels(rect.bottom - rect.top),
        ),
    )
}

/// Every active display, in physical virtual-desktop pixels.
///
/// The card's saved position is physical so two displays with different scale
/// factors cannot reinterpret one number into two places. The work area is
/// where a restored card is clamped; the full rect only finds the display
/// that owns a saved point.
#[expect(
    unsafe_code,
    reason = "EnumDisplayMonitors and GetMonitorInfoW with checked outputs"
)]
pub(crate) fn monitor_layout() -> Vec<MonitorInfo> {
    unsafe extern "system" fn collect(
        monitor: HMONITOR,
        _hdc: HDC,
        _rect: *mut RECT,
        data: LPARAM,
    ) -> BOOL {
        // SAFETY: `data` is the &mut Vec passed below, which outlives the
        // synchronous enumeration; the monitor queries are read-only.
        unsafe {
            let layout = &mut *(data as *mut Vec<MonitorInfo>);
            if let Some(info) = monitor_info(monitor) {
                layout.push(info);
            }
        }
        1
    }

    let mut layout: Vec<MonitorInfo> = Vec::new();
    // SAFETY: the callback only writes into `layout`, which outlives the call.
    unsafe {
        EnumDisplayMonitors(
            std::ptr::null_mut(),
            std::ptr::null(),
            Some(collect),
            std::ptr::addr_of_mut!(layout) as LPARAM,
        );
    }
    // EnumDisplayMonitors promises no order, and the card compares layouts to
    // notice a topology change.
    layout.sort_by(|a, b| a.device.cmp(&b.device));
    layout
}

/// One monitor's device name, physical rects and effective DPI.
#[expect(unsafe_code, reason = "read-only monitor queries with checked outputs")]
fn monitor_info(monitor: HMONITOR) -> Option<MonitorInfo> {
    // SAFETY: a zeroed MONITORINFOEXW is the required input shape; cbSize is
    // set before the call.
    let mut info: MONITORINFOEXW = unsafe { std::mem::zeroed() };
    info.monitorInfo.cbSize = u32::try_from(std::mem::size_of::<MONITORINFOEXW>()).ok()?;
    // SAFETY: `info` is the extended struct and carries its own size; a
    // removed monitor reports failure rather than geometry.
    if unsafe { GetMonitorInfoW(monitor, std::ptr::addr_of_mut!(info).cast::<MONITORINFO>()) } == 0
    {
        return None;
    }
    let (mut dpi, mut dpi_y) = (0, 0);
    // SAFETY: both outputs are writable for the call; a failed HRESULT is not
    // used as a DPI.
    let hr = unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &raw mut dpi, &raw mut dpi_y) };
    let dpi = if hr >= 0 && dpi != 0 { dpi } else { 96 };
    let len = info
        .szDevice
        .iter()
        .position(|&unit| unit == 0)
        .unwrap_or(info.szDevice.len());
    Some(MonitorInfo {
        device: String::from_utf16_lossy(&info.szDevice[..len]),
        display_id: DisplayId::from(monitor as usize as u64),
        monitor: device_bounds(info.monitorInfo.rcMonitor),
        work: device_bounds(info.monitorInfo.rcWork),
        dpi,
    })
}

/// The card window's HWND, for the style and geometry calls below.
fn card_window_hwnd(window: &Window) -> Option<HWND> {
    // GPUI's `Window` has its own `window_handle`; ask the raw handle trait.
    let handle = raw_window_handle::HasWindowHandle::window_handle(window).ok()?;
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return None;
    };
    Some(handle.hwnd.get() as HWND)
}

/// The physical origin of the visible card, plus the monitor's device name.
///
/// The margin is the card's own layout scale, not the monitor's DPI: while a
/// DPI change is still being reconciled the painted card is the authority.
#[expect(
    unsafe_code,
    reason = "window-rect and monitor queries on the live card HWND"
)]
#[expect(
    clippy::cast_possible_truncation,
    reason = "a logical shadow margin scaled by a display DPI is a small whole pixel count"
)]
fn physical_placement(hwnd: HWND, scale: f32) -> Option<(Point<DevicePixels>, String)> {
    // SAFETY: the window rect and its monitor are read-only queries on the
    // live card HWND.
    let (mut rect, monitor) = unsafe {
        let mut rect = RECT::default();
        if GetWindowRect(hwnd, &raw mut rect) == 0 {
            return None;
        }
        (rect, MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST))
    };
    let margin = (f64::from(SHADOW_MARGIN) * f64::from(scale)).round() as i32;
    rect.left += margin;
    rect.top += margin;
    let info = monitor_info(monitor)?;
    Some((
        point(DevicePixels(rect.left), DevicePixels(rect.top)),
        info.device,
    ))
}

/// Put the visible card at `origin` without resizing or activating it.
#[expect(
    unsafe_code,
    reason = "window-rect read and position-only SetWindowPos on the live card HWND"
)]
#[expect(
    clippy::cast_possible_truncation,
    reason = "a logical shadow margin scaled by a display DPI is a small whole pixel count"
)]
fn place_visible(hwnd: HWND, scale: f32, origin: Point<DevicePixels>) -> bool {
    let margin = (f64::from(SHADOW_MARGIN) * f64::from(scale)).round() as i32;
    let target = point(
        DevicePixels(origin.x.0 - margin),
        DevicePixels(origin.y.0 - margin),
    );
    // SAFETY: a rect read and a position-only SetWindowPos on the live card
    // HWND; no size, z-order, or activation changes.
    unsafe {
        let mut rect = RECT::default();
        if GetWindowRect(hwnd, &raw mut rect) == 0 {
            return false;
        }
        if rect.left == target.x.0 && rect.top == target.y.0 {
            return true;
        }
        SetWindowPos(
            hwnd,
            std::ptr::null_mut(),
            target.x.0,
            target.y.0,
            0,
            0,
            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        ) != 0
    }
}

/// The visible card's physical origin and the monitor it is on.
pub(crate) fn card_physical_placement(window: &Window) -> Option<(Point<DevicePixels>, String)> {
    physical_placement(card_window_hwnd(window)?, window.scale_factor())
}

/// Put the visible card at a physical origin; see [`place_visible`].
pub(crate) fn place_card_visible(window: &Window, origin: Point<DevicePixels>) -> bool {
    let Some(hwnd) = card_window_hwnd(window) else {
        return false;
    };
    place_visible(hwnd, window.scale_factor(), origin)
}

/// Move a bootstrap window onto its monitor, then to `bounds` at the DPI it
/// ends up with.
#[expect(
    unsafe_code,
    reason = "borrow the live GPUI HWND on its owning UI thread"
)]
fn place(
    window: &mut Window,
    display_id: DisplayId,
    display: Bounds<DevicePixels>,
    bounds: impl FnOnce(u32) -> Bounds<DevicePixels>,
) -> Result<()> {
    let RawWindowHandle::Win32(handle) = window
        .window_handle()
        .map_err(|error| anyhow::anyhow!("get overlay HWND: {error}"))?
        .as_raw()
    else {
        anyhow::bail!("overlay window has no Win32 handle");
    };
    let hwnd = handle.hwnd.get() as HWND;
    // The frame is cosmetic: a framed overlay is better than none, so a
    // failure here must not abort placement.
    if let Err(error) = make_frameless(hwnd) {
        warn!(%error, "could not remove the overlay window frame");
    }
    // Without this, the click that picks a ring slot takes the foreground
    // from the application the slot's shortcut is meant for. The ring still
    // opens if it fails: actions that press no keys keep working.
    if let Err(error) = make_nonactivating(hwnd) {
        warn!(%error, "could not keep the overlay window from taking the foreground");
    }
    // Pin the 1px bootstrap well inside the selected monitor (GPUI should
    // already have opened it there). If that crosses a DPI boundary,
    // SetWindowPos synchronously delivers WM_DPICHANGED; GPUI applies its
    // suggested rectangle and updates rendering scale before this returns.
    // Final geometry is applied only AFTER that transition, so the suggested
    // rectangle cannot move the window away from where it was placed.
    // FRAMECHANGED applies the styles set above in the same move.
    let anchor = Bounds::new(display.center(), size(DevicePixels(1), DevicePixels(1)));
    set_bounds(
        hwnd,
        anchor,
        SWP_NOACTIVATE | SWP_NOZORDER | SWP_FRAMECHANGED,
    )?;
    // SAFETY: hwnd belongs to the still-borrowed window.
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    ensure!(dpi != 0, "could not read overlay window DPI");
    // SAFETY: read-only query on the still-borrowed window.
    let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    ensure!(
        monitor as usize as u64 == u64::from(display_id),
        "overlay monitor changed during window creation"
    );
    set_bounds(
        hwnd,
        bounds(dpi),
        SWP_NOACTIVATE | SWP_NOZORDER | SWP_SHOWWINDOW,
    )
}

/// Strip the frame GPUI leaves on a pop-up window, so only the overlay's own
/// transparent pixels are drawn.
///
/// GPUI creates `WindowKind::PopUp` with a zero window style, which is
/// `WS_OVERLAPPED`, and `CreateWindowExW` gives every overlapped window a
/// caption. Windows 11 then draws that frame — an opaque backdrop, a border
/// and rounded corners — around the transparent client area. The window
/// becomes a borderless `WS_POPUP` instead; the caller's next `SetWindowPos`
/// must pass `SWP_FRAMECHANGED` to apply it. The DWM attributes keep Windows 11
/// from rounding or outlining it.
#[expect(
    unsafe_code,
    reason = "window-style and DWM attribute writes on a borrowed HWND, with typed values"
)]
fn make_frameless(hwnd: HWND) -> Result<()> {
    // GWL_STYLE is a 32-bit bit set, so the non-Ptr pair is right on every
    // target, and the casts reinterpret it without changing bits.
    // SAFETY: hwnd belongs to the still-borrowed window.
    let style = unsafe { GetWindowLongW(hwnd, GWL_STYLE) }.cast_unsigned();
    let frameless = (style & !(WS_CAPTION | WS_THICKFRAME)) | WS_POPUP;
    // SAFETY: as above; a zero return is only an error with a last error set,
    // and the previous style is never zero for this captioned window.
    if unsafe { SetWindowLongW(hwnd, GWL_STYLE, frameless.cast_signed()) } == 0 {
        return Err(std::io::Error::last_os_error()).context("make overlay window frameless");
    }
    let size = u32::try_from(std::mem::size_of::<u32>())?;
    for (attribute, value) in [
        (
            DWMWA_WINDOW_CORNER_PREFERENCE,
            DWMWCP_DONOTROUND.cast_unsigned(),
        ),
        (DWMWA_BORDER_COLOR, DWMWA_COLOR_NONE),
    ] {
        // SAFETY: value outlives this synchronous call, and `size` is the
        // DWORD both attributes take.
        let hr = unsafe {
            DwmSetWindowAttribute(
                hwnd,
                attribute.cast_unsigned(),
                std::ptr::from_ref(&value).cast(),
                size,
            )
        };
        // Windows 10 has neither attribute, and draws neither effect.
        if hr < 0 {
            debug!(attribute, "DWM attribute unsupported (HRESULT {hr:#x})");
        }
    }
    Ok(())
}

/// Keep a click on the overlay from taking the foreground away from the
/// application it opened over.
///
/// A ring action presses its shortcut into the window with keyboard focus,
/// so the application under the ring has to keep that focus through the
/// click that picks a slot, as it does under the macOS non-activating panel
/// and the Wayland layer surface without keyboard interactivity. GPUI shows
/// the window without activating it, but answers every `WM_MOUSEACTIVATE`
/// with `MA_ACTIVATE`, so that click made the overlay the foreground window
/// and the shortcut reached the closing overlay instead. Windows does not
/// activate a `WS_EX_NOACTIVATE` window when it is clicked. Active window
/// tracking ("activate a window by hovering over it") still asks
/// `WM_MOUSEACTIVATE`, so with that setting on, hovering can still activate it.
#[expect(
    unsafe_code,
    reason = "extended-style read and write on a borrowed HWND, with typed values"
)]
fn make_nonactivating(hwnd: HWND) -> Result<()> {
    // GWL_EXSTYLE is a 32-bit bit set like GWL_STYLE in `make_frameless`.
    // SAFETY: hwnd belongs to the still-borrowed window.
    let ex_style = unsafe { GetWindowLongW(hwnd, GWL_EXSTYLE) }.cast_unsigned();
    let nonactivating = ex_style | WS_EX_NOACTIVATE;
    // SAFETY: as above; a zero return is only an error with a last error set,
    // and GPUI never creates a pop-up with a zero extended style.
    if unsafe { SetWindowLongW(hwnd, GWL_EXSTYLE, nonactivating.cast_signed()) } == 0 {
        return Err(std::io::Error::last_os_error())
            .context("keep the overlay window from taking the foreground");
    }
    Ok(())
}

/// The click-through switch of the pinned Flow Indicator card, watched by a
/// single native thread: over the pin button the window takes clicks (it is
/// how the user unpins), anywhere else the mouse falls through to whatever is
/// under the transparent card.
///
/// Input pass-through needs `WS_EX_LAYERED` alongside `WS_EX_TRANSPARENT` —
/// the transparent bit alone only affects same-thread painting, and the live
/// card was measurably still swallowing clicks without the layered one. A
/// layered attribute of full alpha keeps DirectComposition rendering intact.
struct CardClickThrough {
    /// The card window's HWND, or 0 when no card is being watched.
    hwnd: AtomicIsize,
    /// Whether the card is pinned right now.
    pinned: AtomicBool,
    /// The DPI GPUI currently lays the card out at, in the same units the
    /// native hit test needs. `0` until the first window is reported.
    ///
    /// `GetDpiForWindow` is the monitor's answer, which can disagree with
    /// GPUI's cached scale after a display topology change; the hit test must
    /// follow the painted card, not the monitor.
    layout_dpi: AtomicU32,
    /// The monitor's DPI, reported from the window's owning UI thread. The
    /// watch thread must not be the authority: its own DPI awareness decides
    /// what `GetDpiForWindow` returns there.
    os_dpi: AtomicU32,
    /// Set by the watch when the window moved with the left button down, so
    /// the owner can tell a user drag from Windows repositioning the card.
    user_moved: AtomicBool,
}

static CARD_CLICK_THROUGH: CardClickThrough = CardClickThrough {
    hwnd: AtomicIsize::new(0),
    pinned: AtomicBool::new(false),
    layout_dpi: AtomicU32::new(0),
    os_dpi: AtomicU32::new(0),
    user_moved: AtomicBool::new(false),
};

/// When the watch last asked the window to apply a missed DPI change, in
/// milliseconds since the watch started. At most one request per second.
static LAST_DPI_RECONCILE: AtomicU64 = AtomicU64::new(0);

fn millis_since_start() -> u64 {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    u64::try_from(EPOCH.get_or_init(Instant::now).elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Let the card's mouse through outside its interactive area.
///
/// `WS_EX_TRANSPARENT` is a whole-window switch, and Windows has no
/// cross-process partial input region, so a native watch thread keeps the
/// window interactive only while the cursor is over that area: the whole
/// visible card when regular, the pin button when pinned. The shadow margin is
/// therefore click-through in both states. The watch is native because a
/// pass-through window receives no mouse events of its own to react with; the
/// style write happens only when the answer changes.
pub(crate) fn set_card_click_through(window: &Window, pinned: bool) {
    let Ok(handle) = raw_window_handle::HasWindowHandle::window_handle(window) else {
        warn!("could not get the card window handle for click-through");
        return;
    };
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        warn!("the card window has no Win32 handle for click-through");
        return;
    };
    CARD_CLICK_THROUGH
        .hwnd
        .store(handle.hwnd.get(), Ordering::Release);
    CARD_CLICK_THROUGH.pinned.store(pinned, Ordering::Release);
    set_card_layout_scale(window);
    spawn_click_through_watch();
}

/// Report the scale GPUI is laying the card out at, and the monitor DPI the
/// window actually has, to the native watch.
///
/// The watch cannot ask GPUI for the former (it runs outside the UI thread),
/// and its own DPI awareness must not decide the latter; the UI thread owns
/// both answers and stores them here on every settle.
#[expect(
    unsafe_code,
    reason = "a read-only DPI query on the live card HWND, on its owning thread"
)]
pub(crate) fn set_card_layout_scale(window: &Window) -> u32 {
    let layout = layout_dpi(window.scale_factor());
    let previous = CARD_CLICK_THROUGH.layout_dpi.swap(layout, Ordering::AcqRel);
    if previous != 0 && previous != layout {
        info!(previous, layout, "Flow Indicator layout scale changed");
    }
    if let Some(hwnd) = card_window_hwnd(window) {
        // SAFETY: a read-only DPI query on the card HWND, on the thread that
        // owns it.
        let os = unsafe { GetDpiForWindow(hwnd) };
        CARD_CLICK_THROUGH
            .os_dpi
            .store(if os == 0 { 96 } else { os }, Ordering::Release);
    }
    layout
}

/// Whether the card's monitor DPI and GPUI's layout scale disagree.
///
/// A display topology change can leave GPUI's cached scale at the previous
/// monitor's value without a `WM_DPICHANGED`; the owner uses this to fall back
/// to reopening the window after the watch's re-sent message has had a chance.
#[expect(
    unsafe_code,
    reason = "a read-only DPI query on the live card HWND, on its owning thread"
)]
pub(crate) fn card_dpi_mismatch(window: &Window) -> bool {
    let Some(hwnd) = card_window_hwnd(window) else {
        return false;
    };
    // SAFETY: a read-only DPI query on the card HWND, on the thread that owns
    // it.
    let os = unsafe { GetDpiForWindow(hwnd) };
    os != 0 && os != layout_dpi(window.scale_factor())
}

/// Whether the card needs to be put back into the topmost band.
///
/// `WS_EX_TOPMOST` set with `SetWindowLongW` is only a style bit: a window can
/// keep the bit while sitting in the normal band (a shell or driver z-order
/// reset does this), and checking the bit alone therefore reports "topmost"
/// while ordinary windows cover the card. A visible window without the bit
/// above it is the evidence that the band was lost.
#[expect(
    unsafe_code,
    reason = "style and z-order reads on the live card HWND, on its owning thread"
)]
pub(crate) fn card_needs_raise(window: &Window) -> bool {
    let Some(hwnd) = card_window_hwnd(window) else {
        return false;
    };
    // SAFETY: read-only style and z-order queries on the card HWND, on the
    // thread that owns it.
    unsafe {
        if GetWindowLongW(hwnd, GWL_EXSTYLE) & TOPMOST_BIT == 0 {
            return true;
        }
        let mut above = GetWindow(hwnd, GW_HWNDPREV);
        while !above.is_null() {
            if IsWindowVisible(above) != 0 && GetWindowLongW(above, GWL_EXSTYLE) & TOPMOST_BIT == 0
            {
                return true;
            }
            above = GetWindow(above, GW_HWNDPREV);
        }
    }
    false
}

/// Whether the card moved while the left button was down since the last call.
pub(crate) fn take_card_user_moved() -> bool {
    CARD_CLICK_THROUGH.user_moved.swap(false, Ordering::AcqRel)
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a window scale of a few hundred percent rounds to a small positive DPI"
)]
fn layout_dpi(scale: f32) -> u32 {
    (f64::from(scale) * 96.0).round().clamp(1.0, 10_000.0) as u32
}

fn spawn_click_through_watch() {
    static SPAWNED: OnceLock<()> = OnceLock::new();
    SPAWNED.get_or_init(|| {
        if let Err(error) = std::thread::Builder::new()
            .name("openlogi-card-click-through".into())
            .spawn(watch_click_through)
        {
            warn!(%error, "could not spawn the click-through watch");
        }
    });
}

#[expect(
    unsafe_code,
    reason = "cursor, window-bounds and style reads on the card HWND from a native watch thread"
)]
fn watch_click_through() -> ! {
    let mut last_rect: Option<RECT> = None;
    loop {
        let pinned = CARD_CLICK_THROUGH.pinned.load(Ordering::Acquire);
        let hwnd = CARD_CLICK_THROUGH.hwnd.load(Ordering::Acquire) as HWND;
        let mut delay = std::time::Duration::from_millis(250);
        // SAFETY: a liveness read on the watched HWND.
        if !hwnd.is_null() && unsafe { IsWindow(hwnd) } != 0 {
            // SAFETY: style reads and a style write that is guarded to fire
            // only on a change, on the card window's HWND; the rect and key
            // state reads are read-only.
            unsafe {
                let dpi = effective_layout_dpi(hwnd);
                reconcile_dpi(hwnd, dpi);
                let mut rect = RECT::default();
                if GetWindowRect(hwnd, &raw mut rect) != 0 {
                    if let Some(previous) = last_rect
                        && (rect.left != previous.left
                            || rect.top != previous.top
                            || rect.right != previous.right
                            || rect.bottom != previous.bottom)
                        && left_button_down()
                    {
                        // The OS also moves the card when a display changes;
                        // only a move with the button held is the user's.
                        CARD_CLICK_THROUGH.user_moved.store(true, Ordering::Release);
                    }
                    last_rect = Some(rect);
                } else {
                    last_rect = None;
                }
                let inside = cursor_in_window(hwnd, 0.0, dpi);
                let ex = GetWindowLongW(hwnd, GWL_EXSTYLE);
                let active = ex & WS_EX_TRANSPARENT.cast_signed() != 0
                    && ex & WS_EX_LAYERED.cast_signed() != 0;
                let want = if pinned {
                    !cursor_over_pin_button(hwnd, dpi)
                } else {
                    !cursor_in_window(hwnd, SHADOW_MARGIN, dpi)
                };
                if want != active {
                    let flag = (WS_EX_TRANSPARENT | WS_EX_LAYERED).cast_signed();
                    SetWindowLongW(hwnd, GWL_EXSTYLE, if want { ex | flag } else { ex & !flag });
                    if want {
                        // Full alpha: the layering exists for hit-testing, not to
                        // dim the card.
                        SetLayeredWindowAttributes(hwnd, 0, 255, LWA_ALPHA);
                    }
                }
                // Inside the window, poll fast enough that a click right after
                // the cursor enters its interactive area is not lost. Outside,
                // only the approach needs watching.
                delay = if inside {
                    std::time::Duration::from_millis(15)
                } else {
                    std::time::Duration::from_millis(30)
                };
            }
        } else {
            last_rect = None;
        }
        // Every pass must reach this sleep: the style already matching is the
        // steady state, not a reason to spin the watch thread.
        std::thread::sleep(delay);
    }
}

/// The DPI the card is painted at, falling back to the monitor's before the
/// UI thread has reported one.
#[expect(unsafe_code, reason = "a read-only DPI query on the watched HWND")]
fn effective_layout_dpi(hwnd: HWND) -> u32 {
    let stored = CARD_CLICK_THROUGH.layout_dpi.load(Ordering::Acquire);
    if stored != 0 {
        return stored;
    }
    // SAFETY: a read-only DPI query on the watched HWND.
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    if dpi == 0 { 96 } else { dpi }
}

/// Ask the window to apply a DPI change GPUI missed.
///
/// A display topology change can leave GPUI's cached scale behind the monitor
/// the card is on: the card then paints at the old scale while the monitor
/// reports the new DPI, and the native hit test drifts off the painted pin.
/// `WM_DPICHANGED` is how GPUI learns the new scale, so the watch re-sends it
/// with the window's current physical rect; the next owner tick resizes the
/// card and stores the new layout scale.
#[expect(
    unsafe_code,
    reason = "DPI/rect reads and a same-process synchronous WM_DPICHANGED send on the watched HWND"
)]
fn reconcile_dpi(hwnd: HWND, layout_dpi: u32) {
    // The UI thread's answer is the authority. Falling back to this thread's
    // own query would let its DPI awareness mask a real mismatch.
    let stored = CARD_CLICK_THROUGH.os_dpi.load(Ordering::Acquire);
    let os_dpi = if stored != 0 {
        stored
    } else {
        // SAFETY: a read-only DPI query on the watched HWND.
        unsafe { GetDpiForWindow(hwnd) }
    };
    if os_dpi == 0 || os_dpi == layout_dpi {
        return;
    }
    let now = millis_since_start();
    if now.saturating_sub(LAST_DPI_RECONCILE.load(Ordering::Relaxed)) < 1_000 {
        return;
    }
    LAST_DPI_RECONCILE.store(now, Ordering::Relaxed);
    let mut rect = RECT::default();
    // SAFETY: a read-only rect query on the watched HWND.
    if unsafe { GetWindowRect(hwnd, &raw mut rect) } == 0 {
        return;
    }
    info!(
        os_dpi,
        layout_dpi, "Flow Indicator: re-applying a missed DPI change"
    );
    let wparam = ((os_dpi as usize) << 16) | os_dpi as usize;
    // SAFETY: SendMessage is synchronous and the window belongs to this
    // process, so the window proc reads `rect` before this call returns; it
    // runs on the window's thread, outside GPUI's app borrow.
    unsafe {
        SendMessageW(
            hwnd,
            WM_DPICHANGED,
            wparam,
            std::ptr::addr_of!(rect) as isize,
        );
    }
}

/// Whether the left mouse button is down, for telling a user drag from an
/// OS-driven window move.
#[expect(unsafe_code, reason = "a read-only key-state query")]
fn left_button_down() -> bool {
    // SAFETY: a read-only key-state query.
    unsafe { GetAsyncKeyState(i32::from(VK_LBUTTON)) < 0 }
}

/// Whether the cursor is inside the window, optionally inset by a logical
/// margin on every side.
#[expect(
    unsafe_code,
    reason = "cursor and window-bounds reads on the card HWND from the watch thread"
)]
#[expect(
    clippy::cast_possible_truncation,
    reason = "the layout inset is a whole logical pixel, so its f32→i64 conversion is exact"
)]
unsafe fn cursor_in_window(hwnd: HWND, inset: f32, dpi: u32) -> bool {
    // SAFETY: cursor and window-bounds reads on the card window's HWND, all
    // read-only.
    unsafe {
        let mut cursor = POINT::default();
        if GetCursorPos(&raw mut cursor) == 0 {
            return false;
        }
        let mut bounds = RECT::default();
        if GetWindowRect(hwnd, &raw mut bounds) == 0 {
            return false;
        }
        let dpi = i64::from(dpi);
        if dpi == 0 {
            return false;
        }
        let inset = inset as i64 * dpi / 96;
        let left = i64::from(bounds.left) + inset;
        let right = i64::from(bounds.right) - inset;
        let top = i64::from(bounds.top) + inset;
        let bottom = i64::from(bounds.bottom) - inset;
        i64::from(cursor.x) >= left
            && i64::from(cursor.x) <= right
            && i64::from(cursor.y) >= top
            && i64::from(cursor.y) <= bottom
    }
}

/// Whether the cursor sits on the pin button: the header lane's right end,
/// plus a 1 px grace so a click at the button's edge cannot slip through.
///
/// All geometry is physical pixels — the process is PerMonitorV2-aware, so
/// the cursor and the window bounds are already physical — and the logical
/// constants scale by the scale GPUI paints the card at, in integers. Using the
/// monitor's DPI here instead made the hot zone drift off the painted pin
/// whenever GPUI had missed a DPI change.
#[expect(
    unsafe_code,
    reason = "cursor and window-bounds reads on the card HWND from the watch thread"
)]
#[expect(
    clippy::cast_possible_truncation,
    reason = "the layout constants are whole logical pixels, so their f32→i64 conversion is exact"
)]
unsafe fn cursor_over_pin_button(hwnd: HWND, dpi: u32) -> bool {
    // SAFETY: cursor and window-bounds reads on the card window's HWND, all
    // read-only.
    unsafe {
        let mut cursor = POINT::default();
        if GetCursorPos(&raw mut cursor) == 0 {
            return false;
        }
        let mut bounds = RECT::default();
        if GetWindowRect(hwnd, &raw mut bounds) == 0 {
            return false;
        }
        let dpi = i64::from(dpi);
        if dpi == 0 {
            return false;
        }
        let scaled = |logical: f32| logical as i64 * dpi / 96;
        let right = i64::from(bounds.right) - scaled(SHADOW_MARGIN);
        let grace = dpi / 96;
        let button_left = right - scaled(PADDING_X) - scaled(HEADER_LANE);
        let button_right = right - scaled(PADDING_X);
        let button_top = i64::from(bounds.top) + scaled(SHADOW_MARGIN) + scaled(PADDING_Y);
        let button_bottom = button_top + scaled(HEADER_LANE);
        i64::from(cursor.x) >= button_left - grace
            && i64::from(cursor.x) <= button_right + grace
            && i64::from(cursor.y) >= button_top - grace
            && i64::from(cursor.y) <= button_bottom + grace
    }
}

/// Take a window off the screen without destroying it, or bring it back
/// without activating it.
///
/// The Flow Indicator must keep its window to keep its placement, and here a
/// shrunk window is not invisible: GPUI creates pop-ups as `WS_OVERLAPPED`, so
/// Windows clamps them to the frame minimum and DWM keeps drawing the frame
/// and its shadow around the 1x1 client area.
#[expect(
    unsafe_code,
    reason = "ShowWindow on the live GPUI window's HWND, on its owning UI thread"
)]
pub(crate) fn set_card_visible(window: &Window, visible: bool) {
    let Ok(handle) = raw_window_handle::HasWindowHandle::window_handle(window) else {
        warn!("could not get the overlay window handle to hide it");
        return;
    };
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        warn!("the overlay window has no Win32 handle to hide");
        return;
    };
    let hwnd = handle.hwnd.get() as HWND;
    // SAFETY: hwnd is the live window behind `window`; the call only flips
    // its visibility state.
    unsafe {
        ShowWindow(hwnd, if visible { SW_SHOWNOACTIVATE } else { SW_HIDE });
    }
}

/// Remove the Win32 frame from the Flow Indicator's window — the treatment
/// the ring and the notice get from `place`.
///
/// The card outlives its opening: sync resizes it for row changes, so the
/// style change must also refresh GPUI's cached window-minus-client offset.
/// GPUI measures it once at creation, while the window is still captioned,
/// and only again on `WM_DPICHANGED`/`WM_SETTINGCHANGE`; left stale, every
/// later `resize` would size the window to the caption-era frame and leave
/// a transparent band around the card.
#[expect(
    unsafe_code,
    reason = "SetWindowPos and SendMessage on the live GPUI window's HWND, on its owning thread"
)]
pub(crate) fn make_card_frameless(window: &Window) {
    let Ok(handle) = raw_window_handle::HasWindowHandle::window_handle(window) else {
        warn!("could not get the card window handle to strip its frame");
        return;
    };
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        warn!("the card window has no Win32 handle to strip its frame");
        return;
    };
    let hwnd = handle.hwnd.get() as HWND;
    if let Err(error) = make_frameless(hwnd) {
        warn!(%error, "could not remove the Flow Indicator window frame");
        return;
    }
    // `place` applies the new style through its own move; the card has no
    // such move, so ask for the frame change directly.
    // SAFETY: flag-only SetWindowPos on the live GPUI window's HWND, on the
    // thread GPUI requires.
    unsafe {
        SetWindowPos(
            hwnd,
            std::ptr::null_mut(),
            0,
            0,
            0,
            0,
            SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
    // SAFETY: a settings notification to the window's own thread, which is
    // the current one. GPUI answers it by re-measuring the frame offset; its
    // handler reads system parameters only for specific actions and `1` names
    // none of them.
    unsafe { SendMessageW(hwnd, WM_SETTINGCHANGE, 1, 0) };
}

/// Move the card by a logical offset without resizing or activating it.
///
/// A row-count change resizes the window from its top-left; when the card sits
/// on the bottom edge, the owner moves it up by the height delta so the edge
/// stays put. The physical offset is derived from the window's current DPI, so
/// the move stays on the card's own monitor.
#[expect(
    unsafe_code,
    reason = "DPI, window-rect and a position-only move on a live window's HWND, on its owning thread"
)]
#[expect(
    clippy::cast_possible_truncation,
    reason = "card offsets are screen-sized, so the rounded physical offset fits an i32"
)]
fn move_window_by(hwnd: HWND, delta: Point<Pixels>) {
    // SAFETY: reads and a move on the live window's HWND, on its owning thread;
    // the DPI and window rect are read-only.
    let (dpi, rect) = unsafe {
        (GetDpiForWindow(hwnd), {
            let mut rect = RECT::default();
            if GetWindowRect(hwnd, &raw mut rect) == 0 {
                return;
            }
            rect
        })
    };
    if dpi == 0 {
        return;
    }
    let scale = f64::from(dpi) / 96.0;
    let dx = (f64::from(f32::from(delta.x)) * scale).round() as i32;
    let dy = (f64::from(f32::from(delta.y)) * scale).round() as i32;
    // SAFETY: a position-only move of the live window's HWND, on the thread
    // GPUI requires, without activating it.
    if unsafe {
        SetWindowPos(
            hwnd,
            std::ptr::null_mut(),
            rect.left + dx,
            rect.top + dy,
            0,
            0,
            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        )
    } == 0
    {
        warn!("could not move the Flow Indicator card after a resize");
    }
}

/// Move the card window by a logical offset; see [`move_window_by`].
pub(crate) fn move_card_by(window: &Window, delta: Point<Pixels>) {
    let Ok(handle) = raw_window_handle::HasWindowHandle::window_handle(window) else {
        warn!("could not get the card window handle to move it");
        return;
    };
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        warn!("the card window has no Win32 handle to move it");
        return;
    };
    move_window_by(handle.hwnd.get() as HWND, delta);
}

/// Keep the Flow Indicator's window from taking the foreground when clicked.
///
/// GPUI's pop-up answers `WM_MOUSEACTIVATE` with `MA_ACTIVATE`, so without
/// `WS_EX_NOACTIVATE` clicking the pin or starting a drag makes the overlay the
/// foreground process. The card carries no keyboard controls, so it never
/// needs activation.
pub(crate) fn make_card_nonactivating(window: &Window) {
    let Ok(handle) = raw_window_handle::HasWindowHandle::window_handle(window) else {
        warn!("could not get the card window handle to keep it non-activating");
        return;
    };
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        warn!("the card window has no Win32 handle to keep it non-activating");
        return;
    };
    if let Err(error) = make_nonactivating(handle.hwnd.get() as HWND) {
        warn!(%error, "could not keep the card from taking the foreground");
    }
}

#[expect(
    unsafe_code,
    reason = "SetWindowPos on a borrowed HWND, with physical screen bounds"
)]
fn set_bounds(hwnd: HWND, bounds: Bounds<DevicePixels>, flags: u32) -> Result<()> {
    // SAFETY: the caller holds the live GPUI window on its owning thread;
    // NOZORDER makes the null insert-after handle irrelevant.
    if unsafe {
        SetWindowPos(
            hwnd,
            std::ptr::null_mut(),
            bounds.origin.x.0,
            bounds.origin.y.0,
            bounds.size.width.0,
            bounds.size.height.0,
            flags,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error()).context("position overlay window");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use gpui::px;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    };

    use super::*;

    /// A bare Win32 pop-up with the extended style GPUI gives a
    /// `WindowKind::PopUp`, destroyed on drop.
    struct PopUp(HWND);

    impl PopUp {
        const GPUI_EX_STYLE: u32 = WS_EX_TOOLWINDOW | WS_EX_TOPMOST;

        #[expect(
            unsafe_code,
            reason = "create a plain system-class window for the test"
        )]
        fn open() -> Self {
            let class: Vec<u16> = "STATIC\0".encode_utf16().collect();
            // SAFETY: `class` is a NUL-terminated system class name that
            // outlives the call; every handle argument may be null.
            let hwnd = unsafe {
                CreateWindowExW(
                    Self::GPUI_EX_STYLE,
                    class.as_ptr(),
                    std::ptr::null(),
                    WS_POPUP,
                    0,
                    0,
                    1,
                    1,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null(),
                )
            };
            assert!(
                !hwnd.is_null(),
                "create a test pop-up: {}",
                std::io::Error::last_os_error()
            );
            Self(hwnd)
        }

        #[expect(
            unsafe_code,
            reason = "read the extended style of the test's own window"
        )]
        fn ex_style(&self) -> u32 {
            // SAFETY: `self.0` is a live window this thread created.
            unsafe { GetWindowLongW(self.0, GWL_EXSTYLE) }.cast_unsigned()
        }
    }

    impl Drop for PopUp {
        #[expect(unsafe_code, reason = "destroy the test's own window")]
        fn drop(&mut self) {
            // SAFETY: `self.0` is a live window this thread created, and it is
            // destroyed only here.
            unsafe { DestroyWindow(self.0) };
        }
    }

    #[test]
    fn a_click_cannot_activate_an_overlay_window() {
        let window = PopUp::open();
        assert_eq!(
            window.ex_style() & WS_EX_NOACTIVATE,
            0,
            "a bare pop-up is activated by a click, as GPUI's was"
        );
        make_nonactivating(window.0).expect("set the overlay's extended style");
        let ex_style = window.ex_style();
        assert_ne!(
            ex_style & WS_EX_NOACTIVATE,
            0,
            "a click must leave the foreground with the application under the overlay"
        );
        assert_eq!(
            ex_style & PopUp::GPUI_EX_STYLE,
            PopUp::GPUI_EX_STYLE,
            "the overlay stays a topmost tool window"
        );
    }

    #[test]
    #[expect(unsafe_code, reason = "read the test pop-up's DPI and rect")]
    #[expect(
        clippy::cast_possible_truncation,
        reason = "the test offsets are small whole physical pixels"
    )]
    fn the_card_move_uses_the_window_dpi() {
        let window = PopUp::open();
        let before = {
            let mut rect = RECT::default();
            // SAFETY: `window.0` is a live window this thread created.
            assert_ne!(unsafe { GetWindowRect(window.0, &raw mut rect) }, 0);
            rect
        };
        // SAFETY: `window.0` is a live window this thread created.
        let dpi = unsafe { GetDpiForWindow(window.0) };
        assert_ne!(dpi, 0, "a live window reports its DPI");
        move_window_by(window.0, point(px(10.0), px(20.0)));
        let after = {
            let mut rect = RECT::default();
            // SAFETY: `window.0` is a live window this thread created.
            assert_ne!(unsafe { GetWindowRect(window.0, &raw mut rect) }, 0);
            rect
        };
        let scale = f64::from(dpi) / 96.0;
        assert_eq!(after.left - before.left, (10.0 * scale).round() as i32);
        assert_eq!(after.top - before.top, (20.0 * scale).round() as i32);
    }
}
