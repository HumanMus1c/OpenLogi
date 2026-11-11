//! The last usable battery reading each paired device reported.
//!
//! The device layer keeps a reading while the agent's probe cache lives, but a
//! reinstall, a crash, or a reboot starts that cache empty — every sleeping
//! device then has no reading until it wakes, and the tray, the Flow Indicator,
//! and the settings app would all show a dash. This store carries the last
//! reading across that boundary.
//!
//! [`LastKnown::restore`] fills it only into an offline device that has no
//! reading of its own: a restored value is never presented as live, and a
//! fresh reading always replaces it. The agent applies both halves when it
//! publishes the inventory snapshot, so every client sees the same reading.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::config::file::write_atomic;
use crate::device::{BatteryDevice, BatteryInfo, BatteryReading, DeviceInventory, DeviceKind};

/// The file the cache lives in, under [`crate::paths::config_dir`].
const LAST_KNOWN_FILE: &str = "battery-last-known.toml";
/// The Flow Indicator's private copy this store replaced. Read once so the
/// readings a user already had survive the move to the agent; the next change
/// writes the new file.
const LEGACY_FILE: &str = "flow-indicator-last-known.toml";

#[derive(Debug, Default, Serialize, Deserialize)]
struct LastKnownFile {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    devices: Vec<LastKnownEntry>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct LastKnownEntry {
    /// The prettified codename the snapshot reports; `kind` completes the
    /// identity.
    name: String,
    kind: DeviceKind,
    battery: BatteryInfo,
}

/// The last usable reading each paired device reported, kept across agent
/// restarts.
///
/// The default value is memory-only (tests and the demo device source, which
/// must not write synthetic readings into the user's file); [`Self::load`]
/// backs it with the config directory.
#[derive(Debug, Default)]
pub struct LastKnown {
    entries: Vec<LastKnownEntry>,
    /// The body last read or written, so an unchanged map does not rewrite the
    /// file on every inventory publication.
    saved: String,
    path: Option<PathBuf>,
}

impl LastKnown {
    /// Load the cache from the config directory.
    #[must_use]
    pub fn load() -> Self {
        Self::load_from(config_file(LAST_KNOWN_FILE), config_file(LEGACY_FILE))
    }

    /// Fold one published inventory into the cache and save it when a reading
    /// changed.
    pub fn observe(&mut self, inventories: &[DeviceInventory]) {
        let mut changed = false;
        for paired in inventories
            .iter()
            .flat_map(|inventory| inventory.paired.iter())
        {
            let device = BatteryDevice::from_paired(paired);
            let Some(name) = device.name.as_deref() else {
                continue;
            };
            let Some(battery) = device.battery.as_ref() else {
                continue;
            };
            if battery.reading() == BatteryReading::NoReading {
                continue;
            }
            changed |= self.record(name, device.kind, battery);
        }
        if changed {
            self.save();
        }
    }

    /// Give an offline device with no reading its last known battery. An online
    /// device, or one whose own reading still says something, keeps it.
    pub fn restore(&self, inventories: &mut [DeviceInventory]) {
        for paired in inventories
            .iter_mut()
            .flat_map(|inventory| inventory.paired.iter_mut())
        {
            if paired.online {
                continue;
            }
            let device = BatteryDevice::from_paired(paired);
            if device.has_readout() {
                continue;
            }
            let Some(name) = device.name.as_deref() else {
                continue;
            };
            if let Some(entry) = self.find(name, device.kind) {
                paired.battery = Some(entry.battery.clone());
            }
        }
    }

    fn load_from(path: Option<PathBuf>, legacy: Option<PathBuf>) -> Self {
        let Some(path) = path else {
            return Self::default();
        };
        let (body, saved) = match std::fs::read_to_string(&path) {
            Ok(body) => (body.clone(), body),
            Err(_) => match legacy.and_then(|legacy| std::fs::read_to_string(legacy).ok()) {
                Some(body) => {
                    // Empty `saved` so the first change writes the new file;
                    // until then the legacy copy stays the source.
                    (body, String::new())
                }
                None => {
                    return Self {
                        path: Some(path),
                        ..Self::default()
                    };
                }
            },
        };
        match toml::from_str::<LastKnownFile>(&body) {
            Ok(file) => Self {
                entries: file.devices,
                saved,
                path: Some(path),
            },
            Err(error) => {
                warn!(%error, "ignoring an unreadable last-known battery file");
                Self {
                    path: Some(path),
                    ..Self::default()
                }
            }
        }
    }

    fn find(&self, name: &str, kind: DeviceKind) -> Option<&LastKnownEntry> {
        self.entries
            .iter()
            .find(|entry| entry.name == name && entry.kind == kind)
    }

    fn record(&mut self, name: &str, kind: DeviceKind, battery: &BatteryInfo) -> bool {
        match self
            .entries
            .iter_mut()
            .find(|entry| entry.name == name && entry.kind == kind)
        {
            Some(entry) if entry.battery == *battery => false,
            Some(entry) => {
                entry.battery = battery.clone();
                true
            }
            None => {
                self.entries.push(LastKnownEntry {
                    name: name.to_owned(),
                    kind,
                    battery: battery.clone(),
                });
                true
            }
        }
    }

    fn save(&mut self) {
        let Some(path) = self.path.as_deref() else {
            return;
        };
        let Ok(body) = toml::to_string(&LastKnownFile {
            devices: self.entries.clone(),
        }) else {
            return;
        };
        if body == self.saved {
            return;
        }
        if let Some(parent) = path.parent()
            && let Err(error) = std::fs::create_dir_all(parent)
        {
            warn!(%error, "could not create the config directory");
            return;
        }
        match write_atomic(path, body.as_bytes()) {
            Ok(()) => self.saved = body,
            Err(error) => warn!(%error, "could not save the last-known battery readings"),
        }
    }
}

fn config_file(name: &str) -> Option<PathBuf> {
    crate::paths::config_dir().ok().map(|dir| dir.join(name))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{LAST_KNOWN_FILE, LEGACY_FILE, LastKnown};
    use crate::device::{
        BatteryInfo, BatteryLevel, BatteryStatus, DeviceInventory, DeviceKind, PairedDevice,
        ReceiverInfo,
    };

    fn inventory(online: bool, battery: Option<BatteryInfo>) -> DeviceInventory {
        DeviceInventory {
            receiver: ReceiverInfo {
                name: "Logi Bolt Receiver".to_owned(),
                vendor_id: 0x046d,
                product_id: 0xc548,
                unique_id: Some("unit-test".to_owned()),
            },
            paired: vec![PairedDevice {
                slot: 1,
                codename: Some("G Pro".to_owned()),
                wpid: None,
                kind: DeviceKind::Mouse,
                online,
                battery,
                model_info: None,
                capabilities: None,
                settings_revision: 0,
                hidpp10: false,
            }],
        }
    }

    fn battery(percentage: Option<u8>, status: BatteryStatus) -> BatteryInfo {
        BatteryInfo {
            percentage,
            level: BatteryLevel::Good,
            status,
        }
    }

    /// The shape a charge-start device reports before its gauge settles.
    fn cold_start() -> BatteryInfo {
        BatteryInfo {
            percentage: None,
            level: BatteryLevel::Unknown,
            status: BatteryStatus::Charging,
        }
    }

    fn disk(path: &Path) -> LastKnown {
        LastKnown {
            path: Some(path.to_path_buf()),
            ..LastKnown::default()
        }
    }

    #[test]
    fn an_offline_device_with_no_reading_gets_its_last_known_one() {
        let mut cache = LastKnown::default();
        cache.observe(&[inventory(
            true,
            Some(battery(Some(59), BatteryStatus::Discharging)),
        )]);

        let mut offline = [inventory(false, None)];
        cache.restore(&mut offline);
        let restored = offline[0].paired[0].battery.as_ref();
        assert_eq!(restored.and_then(|battery| battery.percentage), Some(59));
        assert!(
            !offline[0].paired[0].online,
            "the restored device stays an offline row"
        );

        let mut live = [inventory(
            true,
            Some(battery(Some(80), BatteryStatus::Discharging)),
        )];
        cache.restore(&mut live);
        assert_eq!(
            live[0].paired[0]
                .battery
                .as_ref()
                .and_then(|battery| battery.percentage),
            Some(80),
            "a live reading is never replaced by the cache"
        );
    }

    #[test]
    fn a_charging_cold_start_keeps_the_saved_percentage() {
        let mut cache = LastKnown::default();
        cache.observe(&[inventory(
            false,
            Some(battery(Some(42), BatteryStatus::Discharging)),
        )]);
        // A charging cold start carries no percentage; it must not erase the
        // saved one.
        cache.observe(&[inventory(false, Some(cold_start()))]);
        assert_eq!(cache.entries[0].battery.percentage, Some(42));

        // A device with its own readout — even "charging, no percentage" — is
        // not overwritten by the cache.
        let mut charging = [inventory(false, Some(cold_start()))];
        cache.restore(&mut charging);
        assert_eq!(
            charging[0].paired[0]
                .battery
                .as_ref()
                .map(|battery| battery.status),
            Some(BatteryStatus::Charging)
        );

        let mut unseen = [inventory(false, None)];
        unseen[0].paired[0].codename = Some("Never Seen".to_owned());
        cache.restore(&mut unseen);
        assert!(unseen[0].paired[0].battery.is_none());
    }

    #[test]
    fn a_reading_survives_a_restart_through_the_file() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let path = dir.path().join(LAST_KNOWN_FILE);

        let mut cache = disk(&path);
        cache.observe(&[inventory(
            true,
            Some(battery(Some(59), BatteryStatus::Discharging)),
        )]);
        assert!(path.exists(), "a recorded reading is written to disk");

        let mut offline = [inventory(false, None)];
        LastKnown::load_from(Some(path), None).restore(&mut offline);
        assert_eq!(
            offline[0].paired[0]
                .battery
                .as_ref()
                .and_then(|battery| battery.percentage),
            Some(59),
            "a fresh process restores the saved reading"
        );
    }

    #[test]
    fn the_flow_indicators_old_file_is_read_once() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let legacy = dir.path().join(LEGACY_FILE);
        let entry = "[[devices]]\nname = \"G Pro\"\nkind = \"mouse\"\n\n[devices.battery]\n\
                     percentage = 51\nlevel = \"good\"\nstatus = \"discharging\"\n";
        std::fs::write(&legacy, entry).expect("seed the legacy cache");

        let cache = LastKnown::load_from(Some(dir.path().join(LAST_KNOWN_FILE)), Some(legacy));
        let mut offline = [inventory(false, None)];
        cache.restore(&mut offline);
        assert_eq!(
            offline[0].paired[0]
                .battery
                .as_ref()
                .and_then(|battery| battery.percentage),
            Some(51)
        );
    }
}
