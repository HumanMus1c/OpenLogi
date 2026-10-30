//! The agent's Windows notification-area (tray) icon.
//!
//! Mirrors the macOS menu-bar item ([`crate::tray`]): the always-on agent
//! hosts the tray, the GUI is on-demand. Without it the app has no visible
//! presence at all once the GUI window is closed — the agent keeps working
//! but the user has no way to tell, or to get the window back (#347).
//!
//! The menu is smaller than macOS's: Settings / About / Check-for-Updates go
//! through `openlogi://` deeplinks there, and Windows has no scheme
//! registration yet — so just "Show Main Window" (also the left-click action)
//! and "Quit OpenLogi". Show focuses the running GUI if there is one (a
//! second launch would exit on the `openlogi.lock` singleton) or spawns the
//! sibling `OpenLogi.exe` / `openlogi-desktop.exe`. Quit terminates the GUI
//! first — a surviving GUI's IPC retry loop would immediately respawn the
//! agent we are quitting — then exits.
//!
//! Everything runs on one dedicated thread: the hidden window, its message
//! pump, and the menu. The icon is re-added when Explorer restarts (the
//! `TaskbarCreated` broadcast), and the glyph tracks the taskbar theme
//! (black on a light taskbar, white on a dark one) at install time.

#![expect(
    unsafe_code,
    reason = "raw win32: Shell_NotifyIconW + a hidden window's message pump — localized here"
)]
use std::cell::RefCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicUsize, Ordering};

use openlogi_agent_core::observable::ObservableState;
use openlogi_core::config::{Config, TrayBadgeStyle};
use tracing::{debug, info, warn};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::HBRUSH;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
    Shell_NotifyIconW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CW_USEDEFAULT, CreateIconFromResourceEx, CreatePopupMenu, CreateWindowExW,
    DefWindowProcW, DestroyMenu, DispatchMessageW, EnumWindows, GetCursorPos, GetMessageW,
    GetWindowThreadProcessId, HICON, IDI_APPLICATION, IsIconic, IsWindowVisible, LR_DEFAULTCOLOR,
    LoadIconW, MF_GRAYED, MF_SEPARATOR, MF_STRING, MSG, PostMessageW, RegisterClassW,
    RegisterWindowMessageW, SW_RESTORE, SetForegroundWindow, ShowWindow, TPM_NONOTIFY,
    TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu, TranslateMessage, WM_APP, WM_CONTEXTMENU,
    WM_DPICHANGED, WM_LBUTTONUP, WM_NULL, WM_RBUTTONUP, WM_SETTINGCHANGE, WM_THEMECHANGED,
    WNDCLASSW, WS_OVERLAPPED,
};

use crate::shutdown::{self, ShutdownRequestSender};
use crate::tray_readout::{Badge, TrayReadout, create_icon, destroy_icon, render_badge};

/// What the installed icon represents. The theme flag and badge style are
/// part of the key so a taskbar theme change or a style reload rebuilds the
/// brand glyph or the badge's contrast ring.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum IconKey {
    Brand {
        light: bool,
    },
    Badge {
        badge: Badge,
        size: u32,
        light: bool,
        style: TrayBadgeStyle,
    },
}

#[derive(Clone, Copy)]
struct OwnedIcon {
    handle: HICON,
    owned: bool,
    key: IconKey,
}

/// Tray callback message the icon posts to the hidden window.
const WM_TRAY: u32 = WM_APP + 1;
/// A state change woke the tray; re-read the observable and refresh the tip.
const WM_TRAY_UPDATE: u32 = WM_APP + 2;
/// Menu command ids returned by `TrackPopupMenu`.
const ID_SHOW: usize = 1;
const ID_QUIT: usize = 2;

/// The tooltip prefix and the fallback when no device has a readout.
const BRAND_TIP: &str = "OpenLogi";
/// `NOTIFYICONDATAW::szTip`, including its terminating NUL.
const TIP_UNITS: usize = 128;

/// The `TaskbarCreated` broadcast id, resolved once the window exists. Zero
/// until then; real ids are never zero (`RegisterWindowMessageW` starts at
/// 0xC000).
static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);
/// The tray window, for a config reload to wake. Zero until created.
static TRAY_HWND: AtomicUsize = AtomicUsize::new(0);
/// Whether the battery readout is enabled. Set from the config at install and
/// again on every config reload.
static TRAY_BATTERY: AtomicBool = AtomicBool::new(true);
/// The badge shape, encoded as the discriminant the core setting serializes:
/// `0` = circle, `1` = rounded square. Owned here because the tray thread
/// reads it on every refresh while the IPC server writes it on reload.
static TRAY_BADGE_STYLE: AtomicU8 = AtomicU8::new(0);
/// One refresh message at a time: an inventory burst coalesces into a single
/// re-read.
static UPDATE_PENDING: AtomicBool = AtomicBool::new(false);
/// The config the rows name devices from, replaced on every reload. The
/// tray thread reads it while the IPC server writes it, so it is behind a
/// lock rather than an atomic.
static CONFIG: std::sync::RwLock<Option<Arc<Config>>> = std::sync::RwLock::new(None);
/// Whether this agent serves the demo inventory. A GUI launched from the
/// tray must start in the same source, or it would retire the demo agent the
/// user just clicked.
static MOCK_AGENT: AtomicBool = AtomicBool::new(false);

/// The current badge shape, for the icon key and the renderer.
fn badge_style() -> TrayBadgeStyle {
    match TRAY_BADGE_STYLE.load(Ordering::Relaxed) {
        1 => TrayBadgeStyle::RoundedSquare,
        _ => TrayBadgeStyle::Circle,
    }
}

thread_local! {
    /// Where the win32 tray callback hands process termination to the async
    /// lifecycle. The callback and message pump share this one tray thread.
    static SHUTDOWN_TX: RefCell<Option<ShutdownRequestSender>> = const { RefCell::new(None) };
    /// The state the tooltip and menu read. Same thread as the menu and the
    /// pump, so a plain `RefCell` is enough.
    static OBSERVABLE: RefCell<Option<Arc<ObservableState>>> = const { RefCell::new(None) };
    /// The tip currently installed, to skip no-op `NIM_MODIFY`s.
    static LAST_TIP: RefCell<String> = const { RefCell::new(String::new()) };
    /// The icon the shell currently shows, and whether we must destroy it.
    static CURRENT_ICON: RefCell<Option<OwnedIcon>> = const { RefCell::new(None) };
    /// The last badge whose renderer/HICON creation failed, so a repeated
    /// refresh logs the fallback once instead of once per update.
    static LAST_BADGE_FAILURE: RefCell<Option<Badge>> = const { RefCell::new(None) };
}

/// Host the tray icon on its own thread. No-op when the user disabled the
/// menu-bar/tray preference (same `show_in_menu_bar` setting macOS honors;
/// takes effect on the agent's next launch, as there).
///
/// `tray_battery` controls the tooltip and menu readout, and `observable` is
/// the inventory it reads; the icon itself is unchanged by this setting.
/// Failures are logged, never fatal — the agent's real work (hook, HID++,
/// IPC) must not die because a shell icon couldn't be installed.
pub fn spawn(
    show_in_tray: bool,
    tray_battery: bool,
    badge_style: TrayBadgeStyle,
    config: Arc<Config>,
    observable: Arc<ObservableState>,
    shutdown_tx: ShutdownRequestSender,
    mock: bool,
) {
    TRAY_BATTERY.store(tray_battery, Ordering::Relaxed);
    MOCK_AGENT.store(mock, Ordering::Relaxed);
    set_config(config);
    TRAY_BADGE_STYLE.store(
        match badge_style {
            // The wide face is Linux-only; the notification area squares
            // every icon, so Windows keeps the circle badge for it.
            TrayBadgeStyle::Circle | TrayBadgeStyle::Wide => 0,
            TrayBadgeStyle::RoundedSquare => 1,
        },
        Ordering::Relaxed,
    );
    if !show_in_tray {
        info!("tray icon disabled by preference — agent stays invisible");
        return;
    }
    if let Err(e) = std::thread::Builder::new()
        .name("openlogi-tray".into())
        .spawn(move || run_tray_loop(observable, shutdown_tx))
    {
        warn!(error = %e, "could not spawn the tray thread");
    }
}

/// Apply a config reload's `tray_battery` choice. Wakes the tray thread when
/// one is running; before the window exists (or after it is gone) this only
/// stores the value the next read will use.
pub fn set_battery_readout(enabled: bool) {
    TRAY_BATTERY.store(enabled, Ordering::Relaxed);
    wake();
}

/// Replace the config the menu rows resolve device names from. Wakes the tray
/// thread so a renamed device shows up without waiting for the next inventory
/// change.
pub fn set_config(config: Arc<Config>) {
    if let Ok(mut current) = CONFIG.write() {
        *current = Some(config);
    }
    wake();
}

/// Apply a config reload's `tray_badge_style` choice. Same seam as
/// [`set_battery_readout`]: store, then wake the tray thread so the icon key
/// differs and the badge re-renders.
pub fn set_badge_style(style: TrayBadgeStyle) {
    TRAY_BADGE_STYLE.store(
        match style {
            TrayBadgeStyle::Circle | TrayBadgeStyle::Wide => 0,
            TrayBadgeStyle::RoundedSquare => 1,
        },
        Ordering::Relaxed,
    );
    wake();
}

/// Ask the tray thread to re-read the snapshot, when its window exists.
fn wake() {
    let hwnd = TRAY_HWND.load(Ordering::Relaxed) as HWND;
    if hwnd.is_null() {
        return;
    }
    // SAFETY: PostMessageW is safe from any thread, and the handle is the
    // window this process created; zero — the not-yet-created and already-
    // ended cases — is checked above.
    unsafe { PostMessageW(hwnd, WM_TRAY_UPDATE, 0, 0) };
}

/// Create the hidden window, install the icon, and pump messages for the
/// agent's lifetime.
fn run_tray_loop(observable: Arc<ObservableState>, shutdown_tx: ShutdownRequestSender) {
    SHUTDOWN_TX.with_borrow_mut(|slot| *slot = Some(shutdown_tx));
    OBSERVABLE.with_borrow_mut(|slot| *slot = Some(observable.clone()));
    // The agent process is DPI-unaware (no manifest), so without this the
    // hidden window virtualizes to 96 DPI and the badge renders at the 100%
    // 16 px while the 125%+ taskbar slot is 20 px — the shell upscales the
    // bitmap and the badge reads soft and small next to DPI-aware icons.
    // Per-monitor-v2 on this thread makes `GetDpiForWindow` report the real
    // DPI (and delivers WM_DPICHANGED, which the handler already refreshes
    // on). Everything else this thread does is coordinate-free or
    // self-consistent: the menu pairs `GetCursorPos` with `TrackPopupMenu`,
    // both physical on an aware thread.
    set_per_monitor_dpi_awareness();
    let class_name = wide("OpenLogiAgentTray");
    // SAFETY: plain win32 registration/creation calls with pointers that
    // outlive the calls; the class name buffer lives until thread exit.
    unsafe {
        let hinstance = GetModuleHandleW(std::ptr::null());
        let wc = WNDCLASSW {
            style: 0,
            lpfnWndProc: Some(wnd_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: hinstance,
            hIcon: std::ptr::null_mut(),
            hCursor: std::ptr::null_mut(),
            hbrBackground: std::ptr::null_mut::<core::ffi::c_void>() as HBRUSH,
            lpszMenuName: std::ptr::null(),
            lpszClassName: class_name.as_ptr(),
        };
        if RegisterClassW(&raw const wc) == 0 {
            warn!("tray window class registration failed — no tray icon");
            return;
        }
        // A normal (never-shown) top-level window, not message-only: only
        // top-level windows receive the TaskbarCreated broadcast that tells
        // us to re-add the icon after an Explorer restart.
        let hwnd = CreateWindowExW(
            0,
            class_name.as_ptr(),
            wide(openlogi_core::brand::Helper::Agent.display_name()).as_ptr(),
            WS_OVERLAPPED,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            0,
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            hinstance,
            std::ptr::null(),
        );
        if hwnd.is_null() {
            warn!("tray window creation failed — no tray icon");
            return;
        }
        TRAY_HWND.store(hwnd as usize, Ordering::Release);
        TASKBAR_CREATED.store(
            RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()),
            Ordering::Relaxed,
        );
        apply_tray(hwnd, true);
        spawn_state_watcher(hwnd, observable);

        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&raw mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&raw const msg);
            DispatchMessageW(&raw const msg);
        }
        destroy_current_icon();
        TRAY_HWND.store(0, Ordering::Release);
    }
}

/// Wake the tray thread whenever the observable's inventory changes. The
/// watcher only posts; the handler reads the current state, so a burst of
/// inventory updates collapses into at most one pending refresh.
fn spawn_state_watcher(hwnd: HWND, observable: Arc<ObservableState>) {
    let hwnd = hwnd as usize;
    if let Err(e) = std::thread::Builder::new()
        .name("openlogi-tray-state".into())
        .spawn(move || {
            let mut state = observable.subscribe();
            while futures::executor::block_on(state.changed()).is_ok() {
                if !UPDATE_PENDING.swap(true, Ordering::AcqRel) {
                    // SAFETY: PostMessageW is safe from any thread; the
                    // handle is the tray window created above and lives until
                    // the message pump ends.
                    unsafe { PostMessageW(hwnd as HWND, WM_TRAY_UPDATE, 0, 0) };
                }
            }
        })
    {
        warn!(error = %e, "could not spawn the tray state watcher");
    }
}

#[expect(
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation,
    reason = "WM_TRAY packs the mouse message id into the low bits of LPARAM"
)]
unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_TRAY => {
            match lparam as u32 {
                WM_LBUTTONUP => open_or_focus_gui(),
                // SAFETY: win32 dispatches this callback on the tray thread —
                // the one that created `hwnd` and pumps its messages — so the
                // handle is live for the whole call, and `TrackPopupMenu` gets
                // the owning thread it requires.
                WM_RBUTTONUP | WM_CONTEXTMENU => unsafe { show_menu(hwnd) },
                _ => {}
            }
            0
        }
        WM_TRAY_UPDATE => {
            refresh_tray(hwnd);
            0
        }
        WM_SETTINGCHANGE | WM_THEMECHANGED | WM_DPICHANGED => {
            // The taskbar theme or the window DPI changed: the badge's ring
            // and size, and the brand glyph's colour, are part of the icon key.
            refresh_tray(hwnd);
            0
        }
        m if m != 0 && m == TASKBAR_CREATED.load(Ordering::Relaxed) => {
            // Explorer restarted; every tray icon was dropped. Re-add ours,
            // reusing the current handle when the state has not changed.
            apply_tray(hwnd, true);
            0
        }
        // SAFETY: handing the system back the message it just delivered,
        // unchanged: `hwnd` is live for the duration of the callback and
        // `wparam`/`lparam` are the payload win32 paired with `msg`, which is
        // exactly what the default handler expects.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// Re-read the observable and apply the badge/tooltip. The update message
/// coalesces a burst of inventory changes into one refresh.
fn refresh_tray(hwnd: HWND) {
    UPDATE_PENDING.store(false, Ordering::Release);
    apply_tray(hwnd, false);
}

/// Install the icon, or refresh both the icon and the tip in place.
///
/// The badge/tip are recomputed from the same snapshot, and the icon is rebuilt
/// only when the badge, taskbar theme, or icon size changed.
fn apply_tray(hwnd: HWND, add: bool) {
    let summary = current_summary();
    let tip = tip_for(summary.as_ref());
    let key = desired_key(hwnd, summary.as_ref());
    let current = CURRENT_ICON.with_borrow(|slot| *slot);

    if !add
        && current.is_some_and(|icon| icon.key == key)
        && LAST_TIP.with_borrow(|last| last == &tip)
    {
        return;
    }

    let reuse = current.filter(|icon| icon.key == key);
    let icon = match reuse {
        Some(icon) => icon,
        None => match build_icon(key) {
            Some(icon) if !icon.handle.is_null() => icon,
            Some(_) => {
                warn!("tray icon creation failed — keeping the current icon");
                return;
            }
            None => return,
        },
    };
    let built_new = reuse.is_none();

    if !notify_icon(hwnd, add, icon.handle, &tip) {
        warn!(
            add,
            "Shell_NotifyIconW failed — tray icon and tooltip stay as they were"
        );
        if built_new && icon.owned {
            destroy_icon(icon.handle);
        }
        return;
    }

    let previous = CURRENT_ICON.with_borrow_mut(|slot| slot.replace(icon));
    if let Some(previous) = previous
        && previous.owned
        && previous.handle != icon.handle
    {
        destroy_icon(previous.handle);
    }
    LAST_TIP.with_borrow_mut(|last| *last = tip);
    if add {
        info!(?key, "tray icon installed");
    } else if built_new {
        info!(?key, "tray icon updated");
    } else {
        debug!(?key, "tray tooltip updated");
    }
}

/// Add or modify the shell icon with a bounded tooltip.
#[expect(
    clippy::cast_possible_truncation,
    reason = "NOTIFYICONDATAW is a few hundred bytes"
)]
fn notify_icon(hwnd: HWND, add: bool, icon: HICON, tip: &str) -> bool {
    let wide_tip = wide_truncated(tip, TIP_UNITS - 1);
    // SAFETY: `nid` is fully initialized below; the tip buffer is bounded and
    // copied into the fixed-size array.
    unsafe {
        let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
        nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = hwnd;
        nid.uID = 1;
        nid.uFlags = NIF_ICON | NIF_TIP | if add { NIF_MESSAGE } else { 0 };
        nid.uCallbackMessage = WM_TRAY;
        nid.hIcon = icon;
        nid.szTip[..wide_tip.len()].copy_from_slice(&wide_tip);
        Shell_NotifyIconW(if add { NIM_ADD } else { NIM_MODIFY }, &raw const nid) != 0
    }
}

/// The config the rows resolve names from, when one has been installed.
fn current_config() -> Option<Arc<Config>> {
    CONFIG.read().ok().and_then(|config| config.clone())
}

/// The tooltip and badge read one snapshot so they cannot disagree.
fn current_summary() -> Option<TrayReadout> {
    let config = current_config();
    OBSERVABLE.with_borrow(|observable| {
        observable.as_ref().map(|observable| {
            observable.read(|snapshot| {
                TrayReadout::from_parts(
                    &snapshot.inventory,
                    &snapshot.standalone,
                    &snapshot.lights,
                    config.as_deref(),
                )
            })
        })
    })
}

fn tip_for(summary: Option<&TrayReadout>) -> String {
    if !TRAY_BATTERY.load(Ordering::Relaxed) {
        return BRAND_TIP.to_owned();
    }
    summary
        .and_then(TrayReadout::tooltip)
        .unwrap_or_else(|| BRAND_TIP.to_owned())
}

fn desired_key(hwnd: HWND, summary: Option<&TrayReadout>) -> IconKey {
    let light = taskbar_is_light();
    if !TRAY_BATTERY.load(Ordering::Relaxed) {
        return IconKey::Brand { light };
    }
    IconKey::Badge {
        badge: summary.map_or_else(Badge::none, TrayReadout::badge),
        size: small_icon_size(hwnd),
        light,
        style: badge_style(),
    }
}

/// Switch this thread to per-monitor-v2 DPI awareness. Fails closed: if the
/// context cannot be set (pre-1607 Windows), the thread stays unaware and the
/// badge just renders at the 100% size as before.
fn set_per_monitor_dpi_awareness() {
    use windows_sys::Win32::UI::HiDpi::{
        DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetThreadDpiAwarenessContext,
    };
    // SAFETY: the call only changes this thread's DPI virtualization mode;
    // passing a documented constant context and checking the previous one is
    // the documented pattern.
    unsafe {
        SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

/// The shell's small-icon size for this window's DPI (16/20/24 at 100/125/150%).
fn small_icon_size(hwnd: HWND) -> u32 {
    use windows_sys::Win32::UI::HiDpi::{GetDpiForWindow, GetSystemMetricsForDpi};
    use windows_sys::Win32::UI::WindowsAndMessaging::SM_CXSMICON;

    // SAFETY: read-only metric queries on our own window.
    let size = unsafe {
        let dpi = GetDpiForWindow(hwnd);
        let dpi = if dpi == 0 { 96 } else { dpi };
        GetSystemMetricsForDpi(SM_CXSMICON, dpi)
    };
    u32::try_from(size).unwrap_or(16).clamp(16, 64)
}

fn build_icon(key: IconKey) -> Option<OwnedIcon> {
    match key {
        IconKey::Brand { light } => brand_icon(light),
        IconKey::Badge {
            badge,
            size,
            light,
            style,
        } => {
            let rgba = render_badge(badge, size, light, style);
            if let Some(handle) = create_icon(&rgba, size) {
                return Some(OwnedIcon {
                    handle,
                    owned: true,
                    key,
                });
            }
            LAST_BADGE_FAILURE.with_borrow_mut(|last| {
                if last.as_ref() != Some(&badge) {
                    *last = Some(badge);
                    warn!(?badge, "badge icon creation failed — using the brand glyph");
                }
            });
            // Keep the badge key so the same failed badge does not retry on
            // every refresh; a new reading or a theme change tries again.
            brand_icon(light).map(|brand| OwnedIcon { key, ..brand })
        }
    }
}

/// The brand mark in black on a light taskbar, white on a dark one. Both
/// variants are the macOS status-item asset; `CreateIconFromResourceEx`
/// accepts raw PNG buffers (the same PNG-compressed form .ico files carry
/// since Vista). Falls back to the stock application icon rather than
/// showing nothing.
#[expect(
    clippy::cast_possible_truncation,
    reason = "the PNG is embedded at build time and is a few kilobytes"
)]
fn brand_icon(light: bool) -> Option<OwnedIcon> {
    const BLACK: &[u8] = include_bytes!("../assets/tray-icon@2x.png");
    const WHITE: &[u8] = include_bytes!("../assets/tray-icon-white@2x.png");
    let png: &[u8] = if light { BLACK } else { WHITE };
    // SAFETY: the buffer is a valid embedded PNG; the call copies it.
    let icon = unsafe {
        CreateIconFromResourceEx(
            png.as_ptr(),
            png.len() as u32,
            1, // fIcon (not a cursor)
            0x0003_0000,
            0, // cx/cy 0: use the resource's own size
            0,
            LR_DEFAULTCOLOR,
        )
    };
    if icon.is_null() {
        warn!("tray icon PNG rejected — falling back to the stock icon");
        // SAFETY: loading a stock system icon, which we must not destroy.
        let stock = unsafe { LoadIconW(std::ptr::null_mut(), IDI_APPLICATION) };
        return (!stock.is_null()).then_some(OwnedIcon {
            handle: stock,
            owned: false,
            key: IconKey::Brand { light },
        });
    }
    Some(OwnedIcon {
        handle: icon,
        owned: true,
        key: IconKey::Brand { light },
    })
}

/// Destroy the icon we own, if any. The shell's copy is independent.
fn destroy_current_icon() {
    let icon = CURRENT_ICON.with_borrow_mut(Option::take);
    if let Some(icon) = icon
        && icon.owned
    {
        destroy_icon(icon.handle);
    }
}

/// Whether the taskbar renders light (needs the black glyph). Missing value
/// means the Windows default: dark.
fn taskbar_is_light() -> bool {
    use winreg::RegKey;
    use winreg::enums::HKEY_CURRENT_USER;
    RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize")
        .and_then(|k| k.get_value::<u32, _>("SystemUsesLightTheme"))
        .is_ok_and(|v| v == 1)
}

/// Show the context menu at the cursor and run the chosen command.
#[expect(
    clippy::cast_sign_loss,
    reason = "TrackPopupMenu returns the command id it was given, never negative"
)]
unsafe fn show_menu(hwnd: HWND) {
    // SAFETY: menu handles are created and destroyed here; the
    // SetForegroundWindow/WM_NULL bracket is the documented TrackPopupMenu
    // dance for tray menus (without it the menu won't dismiss on outside
    // clicks).
    unsafe {
        let menu = CreatePopupMenu();
        if menu.is_null() {
            return;
        }
        // Built fresh on every right-click, so `t!` follows a live language
        // switch with no rebuild plumbing.
        AppendMenuW(
            menu,
            MF_STRING,
            ID_SHOW,
            wide(&rust_i18n::t!("app.show_main_window")).as_ptr(),
        );
        AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
        let rows: Vec<String> = if TRAY_BATTERY.load(Ordering::Relaxed) {
            let config = current_config();
            OBSERVABLE.with_borrow(|observable| {
                observable.as_ref().map_or_else(Vec::new, |observable| {
                    observable.read(|snapshot| {
                        TrayReadout::from_parts(
                            &snapshot.inventory,
                            &snapshot.standalone,
                            &snapshot.lights,
                            config.as_deref(),
                        )
                        .menu_lines()
                        .collect()
                    })
                })
            })
        } else {
            Vec::new()
        };
        for row in &rows {
            AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, wide(row).as_ptr());
        }
        if !rows.is_empty() {
            AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
        }
        AppendMenuW(
            menu,
            MF_STRING,
            ID_QUIT,
            wide(&rust_i18n::t!("app.quit_openlogi")).as_ptr(),
        );

        let mut pt = POINT { x: 0, y: 0 };
        GetCursorPos(&raw mut pt);
        SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON,
            pt.x,
            pt.y,
            0,
            hwnd,
            std::ptr::null(),
        );
        windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW(hwnd, WM_NULL, 0, 0);
        DestroyMenu(menu);

        match cmd as usize {
            ID_SHOW => open_or_focus_gui(),
            ID_QUIT => quit(hwnd),
            _ => {}
        }
    }
}

/// Focus the running GUI's window, or launch the sibling GUI binary when no
/// GUI is running (a second launch would just exit on the `openlogi.lock`
/// singleton, so spawning blindly does nothing visible).
fn open_or_focus_gui() {
    let pids = gui_pids();
    if pids.is_empty() {
        spawn_gui();
        return;
    }
    if !focus_window_of(&pids) {
        // Running but windowless should not happen (the GUI always has its
        // main window); log rather than spawn a doomed duplicate.
        warn!("GUI process is running but no window was found to focus");
    }
}

/// PIDs of this user's running GUI processes: `OpenLogi.exe` (installed /
/// portable layout) or `openlogi-desktop.exe` (cargo target dir).
///
/// Matching by *name* rather than by install directory is deliberate: the
/// GUI is a per-user singleton (`openlogi.lock` lives under the profile), so
/// whichever copy is running — MSI, portable, dev — it is the only one that
/// *can* run, it is the one talking to this agent (the IPC pipe name is
/// machine-global), and a directory-scoped Show would spawn a sibling that
/// immediately loses the singleton and exits, doing nothing visible. The
/// same-user filter keeps other sessions (fast user switching) out of
/// Show/Quit — their windows are invisible to `EnumWindows` and their
/// processes unkillable anyway, but don't even consider them.
fn gui_pids() -> Vec<u32> {
    use sysinfo::{Pid, Process, ProcessesToUpdate, System};
    let mut system = System::new();
    system.refresh_processes(ProcessesToUpdate::All, true);
    let own_user = system
        .process(Pid::from_u32(std::process::id()))
        .and_then(Process::user_id);
    system
        .processes()
        .values()
        .filter(|p| {
            is_gui_process_name(&p.name().to_string_lossy())
                && (own_user.is_none() || p.user_id() == own_user)
        })
        .map(|p| p.pid().as_u32())
        .collect()
}

/// Whether a process image name is one of the GUI binaries.
///
/// `OpenLogi.exe` is matched case-*sensitively*: the CLI is `openlogi.exe`,
/// which `eq_ignore_ascii_case` would accept, and the dev target dir holds
/// both. Windows reports image names with their on-disk case, so this holds.
fn is_gui_process_name(name: &str) -> bool {
    name == "OpenLogi.exe" || name.eq_ignore_ascii_case("openlogi-desktop.exe")
}

/// Bring the first visible top-level window owned by one of `pids` to the
/// foreground, restoring it if minimized. Returns whether one was found.
fn focus_window_of(pids: &[u32]) -> bool {
    struct Search<'a> {
        pids: &'a [u32],
        focused: bool,
    }
    unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> i32 {
        // SAFETY: lparam is the &mut Search passed to EnumWindows below and
        // outlives the enumeration; the win32 queries take a valid hwnd.
        unsafe {
            let search = &mut *(lparam as *mut Search<'_>);
            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, &raw mut pid);
            if search.pids.contains(&pid) && IsWindowVisible(hwnd) != 0 {
                if IsIconic(hwnd) != 0 {
                    ShowWindow(hwnd, SW_RESTORE);
                }
                SetForegroundWindow(hwnd);
                search.focused = true;
                return 0; // stop enumerating
            }
            1
        }
    }
    let mut search = Search {
        pids,
        focused: false,
    };
    // SAFETY: the callback only dereferences the &mut Search for the duration
    // of this call.
    unsafe {
        EnumWindows(Some(enum_proc), std::ptr::addr_of_mut!(search) as LPARAM);
    }
    search.focused
}

/// Launch the GUI binary sitting next to the agent.
fn spawn_gui() {
    let Ok(exe) = std::env::current_exe() else {
        warn!("could not resolve the agent's own path — cannot launch the GUI");
        return;
    };
    let Some(dir) = exe.parent() else { return };
    // Dev target dir first: it holds both `openlogi.exe` (CLI) and
    // `openlogi-desktop.exe`, and the CLI shares `OpenLogi.exe`'s name on the
    // case-insensitive filesystem — so `dir.join("OpenLogi.exe").exists()`
    // there resolves to the CLI and would launch it. Probing the unambiguous
    // `openlogi-desktop.exe` first avoids that; the installed layout has only
    // `OpenLogi.exe` and falls through to it.
    let gui = ["openlogi-desktop.exe", "OpenLogi.exe"]
        .iter()
        .map(|name| dir.join(name))
        .find(|p| p.exists());
    let Some(gui) = gui else {
        warn!(dir = %dir.display(), "no GUI binary found next to the agent");
        return;
    };
    let mut command = std::process::Command::new(&gui);
    if MOCK_AGENT.load(Ordering::Relaxed) {
        command.arg("--mock");
    }
    match command.spawn() {
        Ok(_) => info!(
            path = %gui.display(),
            mock = MOCK_AGENT.load(Ordering::Relaxed),
            "tray — launched the GUI"
        ),
        Err(e) => warn!(error = %e, path = %gui.display(), "tray — could not launch the GUI"),
    }
}

/// Quit the whole app: GUI first (its IPC retry loop would otherwise respawn
/// the agent we are about to exit), then the icon, then the agent. Mirrors
/// the macOS Quit semantics; the GUI holds no unsaved state (config writes
/// are immediate).
#[expect(
    clippy::cast_possible_truncation,
    reason = "NOTIFYICONDATAW is a few hundred bytes"
)]
fn quit(hwnd: HWND) {
    use sysinfo::{Pid, ProcessesToUpdate, System};
    let mut system = System::new();
    system.refresh_processes(ProcessesToUpdate::All, true);
    for pid in gui_pids() {
        if let Some(process) = system.process(Pid::from_u32(pid)) {
            if process.kill() {
                info!(pid, "tray Quit — terminated the GUI");
            } else {
                warn!(pid, "tray Quit — could not terminate the GUI");
            }
        }
    }
    // SAFETY: removing the icon this thread added.
    unsafe {
        let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
        nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = hwnd;
        nid.uID = 1;
        Shell_NotifyIconW(NIM_DELETE, &raw const nid);
    }
    destroy_current_icon();
    crate::overlay::evict_on_quit();
    info!("tray Quit — requesting graceful agent shutdown");
    let requests = SHUTDOWN_TX.with_borrow(Clone::clone);
    shutdown::request_tray_quit(requests.as_ref(), 0);
}

/// NUL-terminated UTF-16 for win32 W-APIs.
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// `s` as a NUL-terminated UTF-16 buffer of at most `max` code units, not
/// counting the terminator. Cuts on a character boundary so a truncated
/// tooltip cannot end in half a surrogate pair.
fn wide_truncated(s: &str, max: usize) -> Vec<u16> {
    let mut units = Vec::with_capacity(max + 1);
    for character in s.chars() {
        let mut encoded = [0u16; 2];
        let encoded = character.encode_utf16(&mut encoded);
        if units.len() + encoded.len() > max {
            break;
        }
        units.extend_from_slice(encoded);
    }
    units.push(0);
    units
}

#[cfg(test)]
mod tests {
    use super::{is_gui_process_name, wide_truncated};

    #[test]
    fn the_cli_binary_is_not_the_gui() {
        assert!(is_gui_process_name("OpenLogi.exe"));
        assert!(is_gui_process_name("openlogi-desktop.exe"));
        assert!(!is_gui_process_name("openlogi.exe")); // the CLI
    }

    #[test]
    fn the_tooltip_fits_the_notify_icon_buffer() {
        const MAX: usize = 127;

        let exact = "a".repeat(MAX);
        let wide = wide_truncated(&exact, MAX);
        assert_eq!(wide.len(), MAX + 1);
        assert_eq!(wide[MAX], 0);

        let long = "a".repeat(200);
        assert_eq!(wide_truncated(&long, MAX).len(), MAX + 1);

        // A surrogate pair that would straddle the limit is dropped whole.
        let boundary = format!("{}😀", "a".repeat(MAX - 1));
        let wide = wide_truncated(&boundary, MAX);
        assert_eq!(wide.len(), MAX);
        assert_eq!(wide[MAX - 1], 0);
    }
}
