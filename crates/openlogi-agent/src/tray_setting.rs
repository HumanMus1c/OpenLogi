//! App settings a tray can toggle without the settings app.
//!
//! The tray writes the same `config.toml` the GUI owns, through the shared
//! [`ConfigEdit`] owner. After a save it publishes the new revision to the
//! observable state, so the GUI's `saved_elsewhere` path adopts the change and
//! asks the agent to reload it. The overlay polls the file directly.

use std::io;
use std::path::Path;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use openlogi_core::config::{Config, ConfigEdit, ConfigError, ConfigRevision};
use tracing::{info, warn};

/// How many times a save retries when another writer wins the file.
const SAVE_ATTEMPTS: u32 = 5;
/// How long each retry waits for the other writer to finish.
const SAVE_RETRY: Duration = Duration::from_millis(20);

static FLOW_INDICATOR: OnceLock<AtomicBool> = OnceLock::new();

fn flow_indicator() -> &'static AtomicBool {
    FLOW_INDICATOR.get_or_init(|| {
        AtomicBool::new(
            Config::load_or_default().is_ok_and(|config| config.app_settings.flow_indicator),
        )
    })
}

/// Adopt a config the agent just loaded.
pub(crate) fn apply_config(config: &Config) {
    flow_indicator().store(config.app_settings.flow_indicator, Ordering::Relaxed);
}

/// Whether the floating indicator is enabled, for the tray's checkmark.
#[must_use]
pub(crate) fn flow_indicator_enabled() -> bool {
    flow_indicator().load(Ordering::Relaxed)
}

/// Toggle the persisted `[app_settings].flow_indicator`, returning the new
/// revision when the file changed. A failed save leaves the old value in
/// place.
pub(crate) fn toggle_flow_indicator() -> Option<ConfigRevision> {
    let enabled = !flow_indicator_enabled();
    match save_flow_indicator(enabled) {
        Ok(revision) => {
            flow_indicator().store(enabled, Ordering::Relaxed);
            info!(enabled, "flow indicator setting changed from the tray");
            revision
        }
        Err(error) => {
            warn!(%error, "could not save the flow indicator setting");
            None
        }
    }
}

fn save_flow_indicator(enabled: bool) -> Result<Option<ConfigRevision>, String> {
    let path = openlogi_core::paths::config_path().map_err(|error| error.to_string())?;
    save_flow_indicator_at(&path, enabled)
}

fn save_flow_indicator_at(path: &Path, enabled: bool) -> Result<Option<ConfigRevision>, String> {
    for attempt in 0..SAVE_ATTEMPTS {
        let mut edit = ConfigEdit::load(path).map_err(|error| error.to_string())?;
        if edit.config().app_settings.flow_indicator == enabled {
            return Ok(None);
        }
        edit.config_mut().app_settings.flow_indicator = enabled;
        match edit.commit() {
            Ok(revision) => return Ok(Some(revision)),
            Err(ConfigError::Conflict { .. }) if attempt + 1 < SAVE_ATTEMPTS => {
                // The GUI saved between our read and write. Re-read and apply
                // the toggle to the newer file.
                std::thread::sleep(SAVE_RETRY);
            }
            Err(ConfigError::Write { source, .. })
                if source.kind() == io::ErrorKind::WouldBlock && attempt + 1 < SAVE_ATTEMPTS =>
            {
                std::thread::sleep(SAVE_RETRY);
            }
            Err(error) => return Err(error.to_string()),
        }
    }
    Err("config.toml kept changing while saving".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_setting_is_persisted_and_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(openlogi_core::paths::CONFIG_FILE);
        Config::default().save_to_path(&path).unwrap();

        let revision = save_flow_indicator_at(&path, false).unwrap();
        assert!(revision.is_some());
        assert!(
            !Config::load_from_path(&path)
                .unwrap()
                .app_settings
                .flow_indicator
        );

        // Saving the value already on disk is a no-op.
        let no_change = save_flow_indicator_at(&path, false).unwrap();
        assert!(no_change.is_none());
    }
}
