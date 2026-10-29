//! OpenLogi background agent — headless, always-on.
//!
//! Owns the CGEventTap hook and the HID++ device path (gesture capture, DPI,
//! SmartShift), serves the GUI over a Unix-socket tarpc IPC, reconciles its own
//! launchd autostart, and (macOS) hosts the menu-bar status item. The async
//! core walks the state machine in `lifecycle` on a tokio runtime; on macOS
//! the process main thread hosts the AppKit run loop the menu bar requires.
//!
//! In a debug build the agent serves the scripted demo devices instead of real
//! hardware — see [`demo`]; release builds always drive real devices.

// Without this Windows runs the exe as a console app and pops a terminal
// window whenever the GUI's sibling spawn or the Run-key autostart starts the
// agent — "headless" must mean no window of any kind. Debug builds keep the
// console so logs stay visible (matching the GUI's arrangement).
#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

#[cfg(target_os = "macos")]
mod activity_macos;
mod autostart;
mod binary_watch;
mod demo;
#[cfg(target_os = "macos")]
mod input_monitoring;
mod lifecycle;
mod logging;
mod overlay;
mod pairing;
#[cfg(target_os = "linux")]
mod resume_linux;
#[cfg(target_os = "windows")]
mod resume_windows;
// The shared locale catalogs live in `openlogi-ui`; the negotiation that picks
// one is `openlogi_core::locale`. `t!` resolves against a backend each binary
// generates itself, hence the relative path — see
// `tests::the_shared_catalog_is_wired_up` for why a wrong path is silent.
rust_i18n::i18n!("../openlogi-ui/locales", fallback = "en");
mod server;
mod shutdown;
mod startup;
#[cfg(target_os = "macos")]
mod status_item;
mod takeover;
#[cfg(target_os = "macos")]
mod tray;
#[cfg(target_os = "linux")]
mod tray_linux;
#[cfg(any(target_os = "windows", target_os = "macos", target_os = "linux", test))]
mod tray_readout;
#[cfg(target_os = "windows")]
mod tray_windows;
mod virtual_keyboard;

use std::ffi::OsString;
use std::process::ExitCode;
use std::sync::Arc;

use openlogi_agent::mock;
use openlogi_agent_core::observable::ObservableState;
use openlogi_core::config::Config;
use openlogi_core::single_instance::InstanceGuard;
#[cfg(not(target_os = "macos"))]
use openlogi_hid::DeviceIoSignal;
use tracing::{info, warn};

fn main() -> ExitCode {
    // A probe instance answers and leaves before any agent machinery — log,
    // singleton lock, devices — starts.
    #[cfg(target_os = "macos")]
    if input_monitoring::answer_probe_if_asked() {
        return ExitCode::SUCCESS;
    }
    logging::init();

    // The build tree owns the device source: a debug build serves the scripted
    // demo devices so feature testing never needs (or touches) hardware, a
    // release build always drives real devices. `OPENLOGI_MOCK_DEVICES`
    // overrides within a debug build only. The args travel with the demo —
    // it accepts the mock's `--fixture` — and are ignored otherwise.
    let demo = demo::requested();
    let demo_args: Vec<OsString> = std::env::args_os().skip(1).collect();

    // Single-instance guard: the agent owns all device I/O, the CGEventTap, and
    // the IPC socket, so a second agent must never start — launchd's KeepAlive
    // racing the GUI's one-shot auto-spawn could otherwise bring up two, and the
    // loser would steal the socket and install a duplicate event tap. Held for
    // the whole process; the OS releases it on exit (crash-recovery is free).
    let _guard = match acquire_agent_lock(demo) {
        Ok(guard) => guard,
        Err(exit) => return exit,
    };

    // Every non-signal process transition reports to the lifecycle owner. In
    // particular, the binary watcher must not exec or exit from its own thread:
    // an armed lifecycle first releases firmware diversion.
    let (shutdown_tx, shutdown_requests) = shutdown::request_channel();
    // Only the lock-holding (real) agent watches, so a losing duplicate cannot
    // restart anything. The overlay spawns only after the lifecycle decides the
    // agent is wanted; a dormant agent must not bring a helper up.
    binary_watch::spawn(shutdown_tx.clone());

    let config = Config::load_or_default().unwrap_or_else(|e| {
        warn!(error = %e, "could not load config.toml; using defaults");
        Config::default()
    });
    // The tray renders localized strings; resolve the stored preference (or
    // the system locale) before any menu is built. A live language switch
    // reaches the running agent through `reload_config`.
    openlogi_core::locale::activate(config.app_settings.language.as_deref());

    // The tray and the IPC server read the same cell; construct it before the
    // tray thread starts so the first tooltip already has a live handle. The
    // demo marks its version so no surface can mistake it for a live one.
    let observable = Arc::new(ObservableState::new(if demo {
        format!("{}{}", env!("CARGO_PKG_VERSION"), mock::VERSION_SUFFIX)
    } else {
        env!("CARGO_PKG_VERSION").to_string()
    }));

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            warn!(error = %e, "tokio runtime init failed; agent exiting");
            return ExitCode::SUCCESS;
        }
    };

    // macOS hosts the menu-bar item, which needs an NSApplication run loop on
    // the process main thread — so the async core (orchestrator, IPC, watchers,
    // hook) runs on the tokio runtime on a dedicated thread, and the main thread
    // runs AppKit. Elsewhere the core owns the main thread and the tray (if
    // any) runs on its own helper thread.
    let device_io_signal = openlogi_hid::host::device_io_signal();
    #[cfg(target_os = "macos")]
    {
        // Fail closed before the core thread can enumerate or open HID devices.
        // The AppKit loop releases this launch hold only after it has read the
        // login session's console ownership and powerd's current power state.
        let _ = device_io_signal.suspend();
        // Read the menu-bar preference before `config` moves into the core
        // thread; the main thread hosts the tray.
        let show_in_menu_bar = config.app_settings.show_in_menu_bar;
        let app_icon = config.app_settings.app_icon;
        let tray_battery = config.app_settings.tray_battery;
        // The tray reads the same state cell the IPC server hands out; clone
        // before the core thread moves the last handle.
        let tray_observable = Arc::clone(&observable);
        // The tray waits for the core to declare the agent *armed*: a dormant
        // agent (launch_at_login off, started at login, no client yet) must
        // not put an icon in the menu bar only to vanish seconds later. A
        // dropped sender means the core exited without arming — fall through
        // and let the process end.
        let (armed_tx, armed_rx) = std::sync::mpsc::channel::<()>();
        if let Err(e) = std::thread::Builder::new()
            .name("openlogi-agent-core".into())
            .spawn(move || {
                if demo {
                    runtime.block_on(demo::run(
                        demo_args,
                        shutdown_requests,
                        observable,
                        armed_tx,
                    ));
                } else {
                    runtime.block_on(lifecycle::run(
                        config,
                        shutdown_requests,
                        observable,
                        armed_tx,
                    ));
                }
            })
        {
            warn!(error = %e, "could not spawn the agent core thread; exiting");
            return ExitCode::SUCCESS;
        }
        if armed_rx.recv().is_ok() {
            tray::run_app_loop(
                show_in_menu_bar,
                app_icon,
                tray_battery,
                tray_observable,
                device_io_signal,
                shutdown_tx,
            );
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        start_platform_services(&config, &observable, &shutdown_tx, &device_io_signal);
        if demo {
            runtime.block_on(demo::run(demo_args, shutdown_requests, observable));
        } else {
            runtime.block_on(lifecycle::run(config, shutdown_requests, observable));
        }
    }
    ExitCode::SUCCESS
}

/// Take the agent's singleton lock, or hand back the code to exit with.
///
/// A demo agent leaves a running holder in place — the takeover escape hatch
/// already refuses in debug builds, and two agents would fight over the same
/// profile's socket. A production duplicate may replace a stale holder; any
/// other failure is a plain leave.
fn acquire_agent_lock(demo: bool) -> Result<InstanceGuard, ExitCode> {
    match openlogi_core::single_instance::acquire(openlogi_core::single_instance::Role::Agent) {
        Ok(guard) => Ok(guard),
        Err(openlogi_core::single_instance::InstanceError::AlreadyRunning { path }) if demo => {
            warn!(
                path = %path.display(),
                "another openlogi-agent is already running — the demo agent leaves it in place"
            );
            Err(ExitCode::FAILURE)
        }
        Err(openlogi_core::single_instance::InstanceError::AlreadyRunning { path }) => {
            // The holder may be a leftover from before this binary's update —
            // a pre-self-restart agent never exits on its own, and it would
            // wedge the (newer) GUI on its connecting screen forever. If it
            // provably speaks an older protocol, replace it; otherwise exit
            // as the duplicate we are.
            let Some(guard) = takeover::try_replace_stale() else {
                info!(path = %path.display(), "another openlogi-agent is already running — exiting");
                return Err(ExitCode::SUCCESS);
            };
            info!("replaced a stale agent — continuing as the new one");
            Ok(guard)
        }
        Err(e) => {
            warn!(error = %e, "single-instance check failed — exiting");
            Err(ExitCode::SUCCESS)
        }
    }
}

/// Start the platform helpers that run beside the async core on the non-macOS
/// targets: the tray and the native resume listener.
#[cfg(not(target_os = "macos"))]
fn start_platform_services(
    config: &Config,
    observable: &Arc<ObservableState>,
    shutdown_tx: &crate::shutdown::ShutdownRequestSender,
    device_io_signal: &DeviceIoSignal,
) {
    // Windows hosts the notification-area icon on its own win32 thread
    // (message pump included), and Linux hosts a StatusNotifierItem on its
    // own D-Bus thread; the async core keeps the main thread.
    #[cfg(target_os = "windows")]
    {
        tray_windows::spawn(
            config.app_settings.show_in_menu_bar,
            config.app_settings.tray_battery,
            config.app_settings.tray_badge_style,
            Arc::clone(observable),
            shutdown_tx.clone(),
        );
        // Native resume notifications feed the same event seam as macOS
        // and Linux: inventory wakes immediately and replays volatile
        // settings on its settled authoritative snapshot.
        resume_windows::register(device_io_signal.clone());
    }
    #[cfg(target_os = "linux")]
    {
        tray_linux::spawn(
            config.app_settings.show_in_menu_bar,
            config.app_settings.tray_battery,
            config.app_settings.tray_badge_style,
            Arc::clone(observable),
            shutdown_tx.clone(),
        );
        resume_linux::register(device_io_signal.clone());
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    let _ = device_io_signal;
}

#[cfg(test)]
mod tests;
