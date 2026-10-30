//! OpenLogi background agent — headless, always-on.
//!
//! Owns the CGEventTap hook and the HID++ device path (gesture capture, DPI,
//! SmartShift), serves the GUI over a Unix-socket tarpc IPC, reconciles its own
//! launchd autostart, and (macOS) hosts the menu-bar status item. The async
//! core walks the state machine in `lifecycle` on a tokio runtime; on macOS
//! the process main thread hosts the AppKit run loop the menu bar requires.
//!
//! A `--mock` launch serves the scripted demo devices instead of real
//! hardware — see [`demo`]; without it the agent drives real devices. The
//! build tree does not decide: a release build can serve the demo too.

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
mod reload;
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
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use openlogi_agent_core::observable::ObservableState;
use openlogi_core::config::Config;
use openlogi_core::device_source::DeviceSource;
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

    // The device source is a launch decision; see `launch_device_source`.
    let Some((demo, demo_args)) = launch_device_source() else {
        return ExitCode::FAILURE;
    };

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
    // tray thread starts so the first tooltip already has a live handle.
    let observable = Arc::new(ObservableState::new(agent_version(demo)));

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
        // before the core thread moves the last handle. It names device rows
        // from the config, which is replaced on reload through the same seam.
        let tray_observable = Arc::clone(&observable);
        let tray_config = Arc::new(config.clone());
        // The tray waits for the core to declare the agent *armed*: a dormant
        // agent (launch_at_login off, started at login, no client yet) must
        // not put an icon in the menu bar only to vanish seconds later. A
        // dropped sender means the core exited without arming — fall through
        // and let the process end.
        let (armed_tx, armed_rx) = std::sync::mpsc::channel::<()>();
        // The IPC server asks the lifecycle to leave through this sender; the
        // tray keeps its own clone.
        let lifecycle_shutdown = shutdown_tx.clone();
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
                        lifecycle_shutdown,
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
                tray_config,
                tray_observable,
                device_io_signal,
                shutdown_tx,
                demo,
            );
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        start_platform_services(&config, &observable, &shutdown_tx, &device_io_signal, demo);
        if demo {
            runtime.block_on(demo::run(demo_args, shutdown_requests, observable));
        } else {
            runtime.block_on(lifecycle::run(
                config,
                shutdown_requests,
                observable,
                shutdown_tx,
            ));
        }
    }
    ExitCode::SUCCESS
}

/// The version every surface displays. A demo agent's carries the marker so no
/// surface can mistake it for a live session.
fn agent_version(demo: bool) -> String {
    if demo {
        format!(
            "{}{}",
            env!("CARGO_PKG_VERSION"),
            openlogi_core::device_source::MOCK_VERSION_SUFFIX
        )
    } else {
        env!("CARGO_PKG_VERSION").to_string()
    }
}

/// Parse the launch arguments and resolve the device source this agent serves:
/// `--mock` (or `--fixture`, which implies it) selects the scripted demo
/// inventory, a WSL guest defaults to it, and every other launch drives real
/// hardware. `None` means the arguments were invalid (already logged).
fn launch_device_source() -> Option<(bool, Vec<OsString>)> {
    match LaunchArgs::parse(std::env::args_os().skip(1)) {
        Ok(launch) => Some((
            DeviceSource::requested(launch.mock).is_mock(),
            launch.demo_args(),
        )),
        Err(error) => {
            warn!(%error, "invalid launch arguments");
            None
        }
    }
}

/// The agent's launch switch.
///
/// `--mock` serves the scripted demo inventory; `--fixture <profile.json>`
/// serves a recorded profile in frozen test time and implies `--mock`. The
/// GUI's spawn passes `--mock` for a demo suite and nothing for hardware; the
/// login item passes nothing. Any other argument is a caller mistake, not a
/// flag to ignore: a typo that silently started the real device path could
/// open, write, or capture hardware the caller meant to leave alone.
#[derive(Debug, Default, PartialEq, Eq)]
struct LaunchArgs {
    /// Serve the demo inventory.
    mock: bool,
    /// Serve this semantic profile instead of the built-in script.
    fixture: Option<PathBuf>,
}

impl LaunchArgs {
    fn parse(mut args: impl Iterator<Item = OsString>) -> Result<Self, String> {
        let mut launch = Self::default();
        while let Some(arg) = args.next() {
            match arg.to_str() {
                Some("--mock") => launch.mock = true,
                Some("--fixture") => {
                    let path = args
                        .next()
                        .ok_or_else(|| "--fixture requires a profile JSON path".to_owned())?;
                    launch.fixture = Some(PathBuf::from(path));
                    launch.mock = true;
                }
                Some(other) => {
                    return Err(format!(
                        "unknown argument {other}; expected --mock or --fixture <profile.json>"
                    ));
                }
                None => {
                    return Err(format!(
                        "unknown non-UTF-8 argument {}",
                        arg.to_string_lossy()
                    ));
                }
            }
        }
        Ok(launch)
    }

    /// The arguments the demo's profile builder understands (just the fixture,
    /// if one was given). The launch switches themselves are consumed here.
    fn demo_args(&self) -> Vec<OsString> {
        match &self.fixture {
            Some(path) => vec![OsString::from("--fixture"), path.as_os_str().to_owned()],
            None => Vec::new(),
        }
    }
}

/// Take the agent's singleton lock, or hand back the code to exit with.
///
/// A demo agent leaves a running holder in place — the GUI retires that holder
/// over IPC before it asks for the demo, and two agents would fight over the
/// same profile's socket. A production duplicate may replace a stale holder;
/// any other failure is a plain leave.
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
    demo: bool,
) {
    // Windows hosts the notification-area icon on its own win32 thread
    // (message pump included), and Linux hosts a StatusNotifierItem on its
    // own D-Bus thread; the async core keeps the main thread. The tray names
    // its device rows from the config, which a reload replaces.
    #[cfg(any(target_os = "windows", target_os = "linux"))]
    let tray_config = Arc::new(config.clone());
    #[cfg(target_os = "windows")]
    {
        tray_windows::spawn(
            config.app_settings.show_in_menu_bar,
            config.app_settings.tray_battery,
            config.app_settings.tray_badge_style,
            Arc::clone(&tray_config),
            Arc::clone(observable),
            shutdown_tx.clone(),
            demo,
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
            Arc::clone(&tray_config),
            Arc::clone(observable),
            shutdown_tx.clone(),
            demo,
        );
        resume_linux::register(device_io_signal.clone());
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        let _ = device_io_signal;
        let _ = demo;
    }
}

#[cfg(test)]
mod tests;
