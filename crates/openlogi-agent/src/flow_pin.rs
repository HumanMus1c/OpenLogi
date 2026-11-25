//! The Flow Indicator's pinned state, owned by the agent so the tray and the
//! overlay agree on one value. The overlay persists it to its placement file;
//! this cell is the live source the tray reads and the overlay observes.

use std::sync::OnceLock;
use std::time::Instant;

use openlogi_ipc::{FlowPinnedObservation, Generation, OBSERVE_HOLD};
use serde::Deserialize;
use tokio::sync::watch;
use tracing::info;

#[derive(Debug, Deserialize, Default)]
struct FlowPlacement {
    #[serde(default)]
    pinned: bool,
}

/// The agent-owned pinned state and its change notification.
pub struct FlowPin {
    tx: watch::Sender<FlowPinnedObservation>,
}

impl FlowPin {
    fn new(pinned: bool) -> Self {
        let (tx, _) = watch::channel(FlowPinnedObservation {
            generation: 1,
            pinned,
        });
        Self { tx }
    }

    fn load() -> Self {
        let pinned = openlogi_core::paths::config_dir()
            .ok()
            .map(|dir| dir.join("flow-indicator.toml"))
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|body| toml::from_str::<FlowPlacement>(&body).ok())
            .is_some_and(|placement| placement.pinned);
        info!(pinned, "flow indicator pin state loaded");
        Self::new(pinned)
    }

    /// The current pinned state, for the tray's checkmark.
    #[must_use]
    pub fn pinned(&self) -> bool {
        self.tx.borrow().pinned
    }

    /// Set the state and wake every observer. A no-op write notifies nobody.
    pub fn set(&self, pinned: bool) {
        let current = self.pinned();
        if current == pinned {
            return;
        }
        let generation = self.tx.borrow().generation.wrapping_add(1).max(1);
        self.tx
            .send_replace(FlowPinnedObservation { generation, pinned });
        info!(pinned, "flow indicator pin state changed");
    }

    /// Flip the state, returning the new value.
    #[must_use]
    pub fn toggle(&self) -> bool {
        let pinned = !self.pinned();
        self.set(pinned);
        pinned
    }

    /// Serve one `Agent::observe_flow_pinned`: answer once the state differs
    /// from generation `since`, or with the unchanged state after the hold.
    pub async fn observe(&self, since: Generation) -> FlowPinnedObservation {
        let hold_ends = Instant::now() + OBSERVE_HOLD;
        let mut rx = self.tx.subscribe();
        loop {
            if rx.borrow().generation != since || Instant::now() >= hold_ends {
                return *rx.borrow();
            }
            let remaining = hold_ends.saturating_duration_since(Instant::now());
            let _ = tokio::time::timeout(remaining, rx.changed()).await;
        }
    }
}

static FLOW_PIN: OnceLock<FlowPin> = OnceLock::new();

/// The process-wide pinned state, seeded from the overlay's placement file.
pub fn flow_pin() -> &'static FlowPin {
    FLOW_PIN.get_or_init(FlowPin::load)
}

#[cfg(test)]
mod tests {
    use super::FlowPin;

    #[tokio::test]
    async fn a_change_advances_the_generation_and_the_observation_follows() {
        let pin = FlowPin::new(false);
        let first = pin.observe(0).await;
        assert!(!first.pinned);
        pin.set(true);
        let second = pin.observe(first.generation).await;
        assert!(second.pinned);
        assert_eq!(second.generation, first.generation + 1);
        assert!(!pin.toggle());
        assert!(!pin.pinned());
    }
}
