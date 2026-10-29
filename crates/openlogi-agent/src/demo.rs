//! The demo mode: a debug-build agent serves the scripted demo devices
//! instead of real HID hardware.
//!
//! The build tree owns the device source. A debug build demos so feature
//! testing (GUI, overlay, tray) runs the same on both desktop OSes without
//! hardware and without touching the user's devices; a release build always
//! drives real devices, and nothing in this module runs there. Everything
//! around the device layer is the agent's own production machinery: the same
//! IPC contract, the same tray, the same shutdown handling, and the scripted
//! inventory of [`openlogi_agent::mock`]. [`ObservableState`] — the cell the
//! tray reads — is fed by a bridge that publishes the demo state the way the
//! orchestrator publishes the real one, so the tray badge and hover card show
//! demo batteries exactly as they would show real ones.
//!
//! `OPENLOGI_MOCK_DEVICES` overrides within a debug build: `0` drives real
//! hardware (for debugging a device issue with debug symbols), `1` keeps the
//! demo. A release build ignores the knob.

use std::ffi::OsString;
use std::sync::Arc;

use openlogi_agent::mock;
use openlogi_agent_core::observable::ObservableState;
use tracing::{info, warn};

use crate::shutdown::{ShutdownRequest, ShutdownRequests, ShutdownSignals};

/// Whether this agent serves the scripted demo devices: debug builds by
/// default, release builds never.
pub(crate) fn requested() -> bool {
    if !cfg!(debug_assertions) {
        // The knob cannot turn a release build into a demo: the release tree
        // tests real devices by definition.
        if std::env::var_os(openlogi_core::env::MOCK_DEVICES).is_some() {
            info!(
                knob = openlogi_core::env::MOCK_DEVICES,
                "release build — driving real devices regardless"
            );
        }
        return false;
    }
    match std::env::var(openlogi_core::env::MOCK_DEVICES) {
        Ok(value) => parse_knob(&value).unwrap_or_else(|| {
            warn!(
                %value,
                "unknown OPENLOGI_MOCK_DEVICES value — serving the demo devices"
            );
            true
        }),
        Err(_) => true,
    }
}

/// The knob's accepted spellings: `1/true/yes/on` and `0/false/no/off`,
/// case-insensitively. `None` means "not a boolean" — [`requested`] falls
/// back to the tree default and says so.
fn parse_knob(value: &str) -> Option<bool> {
    match value.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

/// Run the demo core: serve the scripted devices over the real IPC socket,
/// mirror them into the observable state the tray reads, and leave on any
/// shutdown path. Never returns: every exit is `process::exit`, exactly like
/// the production lifecycle's.
pub(crate) async fn run(
    args: Vec<OsString>,
    mut shutdown_requests: ShutdownRequests,
    observable: Arc<ObservableState>,
    #[cfg(target_os = "macos")] armed_tx: std::sync::mpsc::Sender<()>,
) {
    let state = match mock::state_from_args(args.into_iter()) {
        Ok(state) => state,
        Err(error) => {
            warn!(%error, "could not build the demo profile");
            exit_demo(1);
        }
    };
    let agent = mock::MockAgent::new(state);
    // The console is inert when stdin is not interactive (a GUI-spawned
    // agent): it stops at EOF and the demo keeps serving.
    mock::spawn_console(agent.clone());
    tokio::spawn(publish_loop(agent.clone(), observable));
    // The demo serves the moment it is up, so the tray may show at once —
    // there is no arming ladder to climb.
    #[cfg(target_os = "macos")]
    let _ = armed_tx.send(());
    let mut signals = ShutdownSignals::install();
    info!(
        "serving the scripted demo devices — {}=0 drives real hardware",
        openlogi_core::env::MOCK_DEVICES
    );
    tokio::select! {
        served = mock::serve(agent) => {
            // The accept loop only ends with a bind failure.
            if let Err(error) = served {
                warn!(%error, "could not bind the IPC socket");
                exit_demo(1);
            }
        }
        () = signals.recv() => info!("shutdown signal — demo agent leaving"),
        Some(request) = shutdown_requests.recv() => handle_shutdown_request(request),
    }
    exit_demo(0);
}

/// Fold one shutdown request into the demo's exit. There is no firmware, hook,
/// or input-worker state to restore, so every path is a plain exit; a
/// replacement leaves respawning — the GUI's spawn retry, or the developer —
/// to start the fresh build.
fn handle_shutdown_request(request: ShutdownRequest) {
    match request {
        #[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
        ShutdownRequest::TrayQuit { core_guard } => {
            // Held to the exit, as the lifecycle holds it through teardown.
            let _core_guard = core_guard;
            info!("tray quit — demo agent leaving");
        }
        ShutdownRequest::Uninstalled => {
            info!("the app was uninstalled — demo agent leaving");
        }
        ShutdownRequest::Restart { path, retry } => {
            info!(path = %path.display(), "executable replaced — demo agent leaving");
            let _ = retry.send(());
        }
    }
}

/// Mirror the demo state into the observable cell the tray reads, forever.
///
/// The mock state is the single authority; this publishes its derivations the
/// way the orchestrator publishes the real ones. Every setter deduplicates, so
/// polling costs nothing when nothing moved.
async fn publish_loop(agent: mock::MockAgent, observable: Arc<ObservableState>) {
    loop {
        publish_once(&agent, &observable).await;
        tokio::time::sleep(mock::OBSERVE_POLL_PERIOD).await;
    }
}

/// Publish one rendering of the demo state into `observable`.
async fn publish_once(agent: &mock::MockAgent, observable: &ObservableState) {
    let snapshot = agent.current().await.snapshot;
    observable.set_inventory(
        snapshot.status.inventory,
        &snapshot.inventory,
        &snapshot.standalone,
        snapshot.status.hid_open_failures,
    );
    observable.set_launch_at_login(snapshot.status.launch_at_login);
    observable.set_camera_active(snapshot.camera_active);
    observable.set_pairing(snapshot.pairing.clone());
    observable.set_foreground(snapshot.foreground.current.clone());
    observable.set_lights(snapshot.lights);
}

/// End the process from the demo core. The core runs off the process main
/// thread on macOS, and returning would leave the tray threads keeping the
/// process alive — the production lifecycle exits the same way, for the same
/// reason.
fn exit_demo(status: i32) -> ! {
    #[expect(
        clippy::exit,
        reason = "the demo core owns no firmware, hook, or input-worker state to restore, runs off the process main thread on macOS, and returning would leave the tray threads holding the process alive"
    )]
    std::process::exit(status)
}

#[cfg(test)]
mod tests {
    use openlogi_ipc::InventoryHealth;

    use super::*;

    #[test]
    fn the_knob_accepts_only_known_spellings() {
        assert_eq!(parse_knob("1"), Some(true));
        assert_eq!(parse_knob("on"), Some(true));
        assert_eq!(parse_knob("TRUE"), Some(true));
        assert_eq!(parse_knob("0"), Some(false));
        assert_eq!(parse_knob("Off"), Some(false));
        assert_eq!(parse_knob(""), None);
        assert_eq!(parse_knob("demo"), None);
    }

    #[tokio::test]
    async fn the_bridge_publishes_the_demo_inventory_into_the_tray_cell() {
        let agent = mock::MockAgent::new(
            mock::state_from_args(std::iter::empty()).expect("built-in demo state"),
        );
        let observable = ObservableState::new("test".to_owned());
        publish_once(&agent, &observable).await;

        let snapshot = observable.snapshot();
        assert_eq!(snapshot.status.inventory, InventoryHealth::Ready);
        // The canonical demo: one Bolt receiver pairing three devices, one
        // directly attached mouse, and the standalone light.
        assert_eq!(snapshot.inventory.len(), 2);
        assert_eq!(
            snapshot
                .inventory
                .iter()
                .map(|inventory| inventory.paired.len())
                .sum::<usize>(),
            4
        );
        assert_eq!(snapshot.standalone.len(), 1);
        assert!(
            snapshot.inventory[0].paired[0].battery.is_some(),
            "the tray badge needs the online mouse's battery"
        );
    }
}
