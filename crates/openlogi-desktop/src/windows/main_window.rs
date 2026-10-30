//! The primary application window.
//!
//! Kept a singleton through the same [`WindowRegistry`] slot the auxiliary
//! windows use, so the dock-icon reopen handler, the tray's "Show Main Window"
//! and any repeat call all land on the live window instead of stacking a
//! duplicate — and a window closed while the app kept running in the menu bar
//! can be brought back.

use std::path::PathBuf;

use gpui::{
    App, AppContext as _, Bounds, Pixels, Point, Size, Styled as _, WindowBounds, WindowOptions, px,
};
use gpui_component::{ActiveTheme as _, Root};
use openlogi_core::brand::APP_ID;
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::app::AppView;
use crate::ui::theme;
use crate::windows::{WindowRegistry, titlebar_options};

/// The main window's last frame, beside the config file. The Flow Indicator
/// owns its own `flow-indicator.toml`; this is the settings window only.
const STATE_FILE: &str = "window-state.json";
const DEFAULT_WIDTH: f32 = 1280.0;
const DEFAULT_HEIGHT: f32 = 820.0;
const MIN_WIDTH: f32 = 720.0;
const MIN_HEIGHT: f32 = 680.0;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
struct WindowState {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
}

/// Wayland lets the compositor place a toplevel; the GNOME extension owns that
/// session's placement. Everywhere else the app can restore its own frame.
fn persistence_supported() -> bool {
    #[cfg(target_os = "linux")]
    {
        gpui::guess_compositor() != "Wayland"
    }
    #[cfg(not(target_os = "linux"))]
    {
        true
    }
}

fn state_path() -> Option<PathBuf> {
    openlogi_core::paths::config_dir()
        .ok()
        .map(|directory| directory.join(STATE_FILE))
}

fn load_state() -> Option<WindowState> {
    let path = state_path()?;
    let body = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&body).ok()
}

fn save_state(bounds: Bounds<Pixels>) {
    let Some(path) = state_path() else {
        return;
    };
    let state = WindowState {
        x: f32::from(bounds.origin.x),
        y: f32::from(bounds.origin.y),
        width: f32::from(bounds.size.width),
        height: f32::from(bounds.size.height),
    };
    let body = match serde_json::to_string(&state) {
        Ok(body) => body,
        Err(error) => {
            warn!(%error, "could not serialize the window state");
            return;
        }
    };
    if let Err(error) = std::fs::write(&path, body) {
        warn!(path = %path.display(), %error, "could not save the window state");
    }
}

fn clamp_to_area(state: WindowState, area: Bounds<Pixels>) -> Bounds<Pixels> {
    let size = Size::new(
        px(state.width.max(MIN_WIDTH)),
        px(state.height.max(MIN_HEIGHT)),
    );
    let max = Point::new(
        px((f32::from(area.origin.x) + f32::from(area.size.width) - f32::from(size.width))
            .max(f32::from(area.origin.x))),
        px((f32::from(area.origin.y) + f32::from(area.size.height) - f32::from(size.height))
            .max(f32::from(area.origin.y))),
    );
    let origin = Point::new(
        px(state
            .x
            .clamp(f32::from(area.origin.x), f32::from(max.x))),
        px(state
            .y
            .clamp(f32::from(area.origin.y), f32::from(max.y))),
    );
    Bounds::new(origin, size)
}

/// The saved frame, clamped into the primary display so a monitor change
/// cannot restore the window off-screen.
fn saved_bounds(cx: &mut App) -> Option<Bounds<Pixels>> {
    let state = load_state()?;
    Some(clamp_to_area(state, cx.primary_display()?.visible_bounds()))
}

/// Windows draws the native caption entirely above the client area, while
/// GPUI splits the non-client height evenly around it when placing a window.
/// A restored client y therefore lands half a caption too low on every
/// relaunch; the saved frame must ask that much higher to round-trip.
#[cfg(target_os = "windows")]
#[expect(unsafe_code, reason = "two plain Win32 metric queries")]
fn caption_placement_bias() -> Pixels {
    use windows_sys::Win32::UI::HiDpi::{GetDpiForSystem, GetSystemMetricsForDpi};
    use windows_sys::Win32::UI::WindowsAndMessaging::SM_CYCAPTION;

    // SAFETY: both calls only read system metrics; neither takes a pointer.
    let (caption, dpi) = unsafe {
        let dpi = GetDpiForSystem();
        (GetSystemMetricsForDpi(SM_CYCAPTION, dpi), dpi)
    };
    px(caption_bias(caption, dpi))
}

#[cfg(any(target_os = "windows", test))]
#[expect(
    clippy::cast_precision_loss,
    reason = "a caption height and a DPI are small integers, exactly representable in f32"
)]
fn caption_bias(caption: i32, dpi: u32) -> f32 {
    (caption as f32 / 2.0).ceil() / (dpi as f32 / 96.0)
}

/// Save the live main window's frame. Called once a second by the GUI runtime
/// and once more on teardown; a moved window keeps its remembered place even
/// when the app quits between polls.
pub fn persist_bounds(cx: &mut App) {
    if !persistence_supported() {
        return;
    }
    let Some(handle) = cx.default_global::<WindowRegistry>().main else {
        return;
    };
    let Ok(bounds) = handle.update(cx, |_, window, _| {
        // A maximized window reports the work area, which would replace the
        // normal frame the user actually chose (and GPUI drops the maximized
        // flag when reopening, so restoring it is not an option).
        (!window.is_maximized()).then(|| window.bounds())
    }) else {
        return;
    };
    let Some(bounds) = bounds else {
        return;
    };
    save_state(bounds);
}

/// A saved client frame turned into the frame GPUI must be asked for.
#[cfg(target_os = "windows")]
fn restored_bounds(bounds: Bounds<Pixels>) -> Bounds<Pixels> {
    Bounds::new(
        Point::new(
            bounds.origin.x,
            bounds.origin.y - caption_placement_bias(),
        ),
        bounds.size,
    )
}

#[cfg(not(target_os = "windows"))]
const fn restored_bounds(bounds: Bounds<Pixels>) -> Bounds<Pixels> {
    bounds
}

fn window_options(cx: &mut App) -> WindowOptions {
    let saved = if persistence_supported() {
        saved_bounds(cx)
    } else {
        None
    };
    let bounds = saved.map_or_else(
        || {
            Bounds::centered(
                None,
                Size::new(px(DEFAULT_WIDTH), px(DEFAULT_HEIGHT)),
                cx,
            )
        },
        restored_bounds,
    );
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        // Advertise a Wayland xdg-toplevel app_id (and X11 WM_CLASS). Without it
        // the window ships no app_id, so GNOME's `get_wm_class()` returns empty
        // and our own `gnome_shell` frontmost backend reports OpenLogi as `None`
        // (and the dash can't group the window under its launcher icon). The id
        // is the shared `brand::APP_ID`, matching the desktop file's
        // `StartupWMClass` and the macOS bundle-id family.
        app_id: Some(APP_ID.into()),
        // Min height keeps the buttons tab's mouse model above its scale floor
        // (`MODEL_MIN_H` + the chrome/padding reserve) so its side labels never
        // overlap; below this the model can't shrink further without crowding.
        window_min_size: Some(Size::new(px(MIN_WIDTH), px(MIN_HEIGHT))),
        // Linux: transparent chrome so `AppView::render` can draw a client-side
        // `TitleBar` (the compositor declines server-side decorations and gpui's
        // fallback is unpainted). macOS/Windows keep their native titlebar.
        titlebar: Some(titlebar_options("OpenLogi")),
        ..WindowOptions::default()
    }
}

/// Open the main window — or focus the one already open.
pub fn open(cx: &mut App) {
    let existing = cx.default_global::<WindowRegistry>().main;
    if let Some(handle) = existing
        && handle
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
    {
        cx.activate(true);
        return;
    }

    let options = window_options(cx);
    let opened = cx.open_window(options, |window, cx| {
        theme::apply_from_settings(Some(window), cx);

        let view = cx.new(|cx| AppView::new(window, cx));

        let appearance_obs = window.observe_window_appearance(|window, cx| {
            theme::apply_from_settings(Some(window), cx);
        });
        view.update(cx, |v, _| v.set_appearance_obs(appearance_obs));

        cx.new(|cx| Root::new(view, window, cx).bg(cx.theme().background))
    });

    match opened {
        Ok(handle) => {
            let _ = handle.update(cx, |_, window, _| window.activate_window());
            cx.default_global::<WindowRegistry>().main = Some(handle);
            cx.activate(true);
        }
        Err(e) => warn!(error = %e, "could not open the main window"),
    }
}

/// Open the main window as the session anchor when no window is currently open.
///
/// The auxiliary windows are standalone, so opening one with no main window up
/// would leave the app quitting the moment that window closes.
pub fn ensure(cx: &mut App) {
    if cx.windows().is_empty() {
        open(cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{point, size};

    #[test]
    fn a_saved_frame_is_clamped_to_the_display() {
        let area = Bounds::new(point(px(0.0), px(0.0)), size(px(1920.0), px(1080.0)));
        let state = WindowState {
            x: 5000.0,
            y: 5000.0,
            width: 1240.0,
            height: 780.0,
        };

        assert_eq!(
            clamp_to_area(state, area),
            Bounds::new(point(px(680.0), px(300.0)), size(px(1240.0), px(780.0)))
        );
    }

    #[test]
    fn a_negative_frame_is_clamped_and_the_minimum_size_is_kept() {
        let area = Bounds::new(point(px(0.0), px(0.0)), size(px(1920.0), px(1080.0)));
        let state = WindowState {
            x: -50.0,
            y: -50.0,
            width: 100.0,
            height: 100.0,
        };

        assert_eq!(
            clamp_to_area(state, area),
            Bounds::new(point(px(0.0), px(0.0)), size(px(720.0), px(680.0)))
        );
    }

    #[test]
    fn the_caption_bias_matches_windows_frame_geometry() {
        // A 96-DPI caption is 23 px. GPUI splits the 39 px frame 19/20 while
        // Windows puts 31 px above the client, so restoring without this
        // correction drifts 12 px lower on every launch.
        assert!((caption_bias(23, 96) - 12.0).abs() < f32::EPSILON);
        assert!((caption_bias(35, 144) - 12.0).abs() < f32::EPSILON);
    }

    #[test]
    fn the_saved_state_roundtrips() {
        let state = WindowState {
            x: 42.5,
            y: -7.25,
            width: 1240.0,
            height: 780.0,
        };
        assert_eq!(
            serde_json::from_str::<WindowState>(&serde_json::to_string(&state).unwrap()).unwrap(),
            state
        );
    }
}
