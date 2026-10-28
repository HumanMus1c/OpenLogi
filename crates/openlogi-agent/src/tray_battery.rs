//! The battery readout the tray shows.
//!
//! The selection and wording are pure over the inventory snapshot, so the
//! rules the settings app uses ([`BatteryInfo::reading`] and
//! [`BatteryInfo::tone`]) are the rules the tray renders. The Win32 side owns
//! only the icon, tooltip, and menu plumbing.

use openlogi_core::device::{
    BatteryDevice, BatteryTone, BatteryValue, DeviceInventory, battery_devices,
    first_battery_readout,
};

#[cfg(any(target_os = "windows", target_os = "linux", test))]
mod badge;
#[cfg(any(target_os = "windows", target_os = "linux"))]
pub(crate) use badge::Badge;
#[cfg(any(target_os = "windows", target_os = "linux"))]
pub(crate) use badge::render as render_badge;
#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
pub(crate) use windows::{create_icon, destroy_icon};

/// The most device rows the tray menu lists; the widget shows the full list.
pub(crate) const MAX_MENU_ROWS: usize = 6;

/// The tooltip prefix and the fallback when no device has a readout.
pub(crate) const BRAND: &str = "OpenLogi";

/// A device's display name: its prettified codename, or the localized kind
/// when the firmware reported no name.
fn device_name(device: &BatteryDevice) -> String {
    device
        .name
        .clone()
        .unwrap_or_else(|| rust_i18n::t!(device.kind.translation_key()).to_string())
}

/// The value the settings app would show, without the status words.
fn value(device: &BatteryDevice) -> String {
    let Some(battery) = &device.battery else {
        return "–".to_owned();
    };
    match battery.value() {
        BatteryValue::Percent(percentage) => format!("{percentage}%"),
        value => value
            .translation_key()
            .map_or_else(|| "–".to_owned(), |key| rust_i18n::t!(key).to_string()),
    }
}

fn shows_charging(device: &BatteryDevice) -> bool {
    device
        .battery
        .as_ref()
        .is_some_and(|battery| battery.tone() == BatteryTone::Charging)
}

fn is_error(device: &BatteryDevice) -> bool {
    device
        .battery
        .as_ref()
        .is_some_and(|battery| battery.tone() == BatteryTone::Error)
}

/// One disabled menu line: name, value, then the charging bolt, error, or
/// offline word that explains the value.
fn menu_line(device: &BatteryDevice) -> String {
    let mut line = format!("{}  {}", device_name(device), value(device));
    if device.online && shows_charging(device) {
        line.push_str("  ⚡");
    }
    if is_error(device) {
        line.push_str("  · ");
        line.push_str(&rust_i18n::t!("device.battery_error"));
    }
    if !device.online {
        line.push_str("  · ");
        line.push_str(&rust_i18n::t!("device.offline"));
    }
    line
}

/// The tooltip's part after the brand: the device, its value, and the words
/// the settings app puts beside it.
fn tooltip_readout(device: &BatteryDevice) -> Option<String> {
    if !device.has_readout() {
        return None;
    }
    let value = value(device);
    let mut text = format!("{} {value}", device_name(device));
    if shows_charging(device) && value != rust_i18n::t!("device.charging").as_ref() {
        text.push_str(" · ");
        text.push_str(&rust_i18n::t!("device.charging"));
    }
    if is_error(device) {
        text.push_str(" · ");
        text.push_str(&rust_i18n::t!("device.battery_error"));
    }
    if !device.online {
        text.push_str(" · ");
        text.push_str(&rust_i18n::t!("device.offline"));
    }
    Some(text)
}

/// What the tray shows for one inventory snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BatterySummary {
    devices: Vec<BatteryDevice>,
}

impl BatterySummary {
    pub(crate) fn from_inventory(inventory: &[DeviceInventory]) -> Self {
        Self {
            devices: battery_devices(inventory),
        }
    }

    /// The tooltip text, or `None` for the plain brand tip.
    ///
    /// The GUI's selected device is not part of the snapshot; matching the
    /// persisted key would re-derive the device-config key, so this follows the
    /// plan's allowed fallback and reads out the first online device with
    /// something to show.
    #[cfg(any(target_os = "windows", target_os = "macos", test))]
    pub(crate) fn tooltip(&self) -> Option<String> {
        self.readout().map(|readout| format!("{BRAND} · {readout}"))
    }

    /// The readout without the brand prefix, for surfaces that carry the brand
    /// separately (the Linux tray's tooltip title).
    pub(crate) fn readout(&self) -> Option<String> {
        first_battery_readout(&self.devices).and_then(tooltip_readout)
    }

    /// The menu's device rows in inventory order, capped.
    pub(crate) fn menu_lines(&self) -> impl Iterator<Item = String> + '_ {
        self.devices.iter().take(MAX_MENU_ROWS).map(menu_line)
    }

    /// The badge for the device the tooltip reads out, or a neutral dash when
    /// no device has a readout.
    #[cfg(any(target_os = "windows", target_os = "linux"))]
    pub(crate) fn badge(&self) -> Badge {
        first_battery_readout(&self.devices).map_or_else(Badge::none, Badge::for_device)
    }

    /// The macOS menu-bar title: the percentage, or the charging bolt for a
    /// cold start that has no gauge yet. Level-only and error readings keep
    /// the glyph — the words live in the menu rows, which fit.
    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn title(&self) -> Option<String> {
        first_battery_readout(&self.devices).and_then(|device| {
            let battery = device.battery.as_ref()?;
            if !device.has_readout() || is_error(device) {
                return None;
            }
            if device.online && shows_charging(device) {
                return Some("⚡".to_owned());
            }
            match battery.value() {
                BatteryValue::Percent(percentage) => Some(format!("{percentage}%")),
                _ => None,
            }
        })
    }

    /// Whether any device row would be listed.
    #[cfg(target_os = "macos")]
    pub(crate) fn has_rows(&self) -> bool {
        !self.devices.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::{BatterySummary, MAX_MENU_ROWS};
    use openlogi_core::device::{
        BatteryInfo, BatteryLevel, BatteryStatus, DeviceInventory, DeviceKind, PairedDevice,
        ReceiverInfo,
    };

    fn battery(percentage: Option<u8>, level: BatteryLevel, status: BatteryStatus) -> BatteryInfo {
        BatteryInfo {
            percentage,
            level,
            status,
        }
    }

    fn paired(
        slot: u8,
        codename: Option<&str>,
        kind: DeviceKind,
        online: bool,
        battery: Option<BatteryInfo>,
    ) -> PairedDevice {
        PairedDevice {
            slot,
            codename: codename.map(str::to_owned),
            wpid: None,
            kind,
            online,
            battery,
            model_info: None,
            capabilities: None,
            settings_revision: 0,
        }
    }

    fn inventory(paired: Vec<PairedDevice>) -> DeviceInventory {
        DeviceInventory {
            receiver: ReceiverInfo {
                name: "Logi Bolt Receiver".to_owned(),
                vendor_id: 0x046d,
                product_id: 0xc548,
                unique_id: None,
            },
            paired,
        }
    }

    fn summary(devices: Vec<PairedDevice>) -> BatterySummary {
        BatterySummary::from_inventory(&[inventory(devices)])
    }

    #[test]
    fn the_tooltip_reads_the_first_online_device_with_a_reading() {
        let summary = summary(vec![
            paired(
                1,
                Some("Offline Mouse"),
                DeviceKind::Mouse,
                false,
                Some(battery(
                    Some(90),
                    BatteryLevel::Good,
                    BatteryStatus::Discharging,
                )),
            ),
            paired(2, Some("No Battery Mouse"), DeviceKind::Mouse, true, None),
            paired(
                3,
                Some("Unread Mouse"),
                DeviceKind::Mouse,
                true,
                Some(battery(None, BatteryLevel::Unknown, BatteryStatus::Unknown)),
            ),
            paired(
                4,
                Some("MX Master 3S"),
                DeviceKind::Mouse,
                true,
                Some(battery(
                    Some(64),
                    BatteryLevel::Good,
                    BatteryStatus::Discharging,
                )),
            ),
            paired(
                5,
                Some("MX Keys"),
                DeviceKind::Keyboard,
                true,
                Some(battery(
                    Some(30),
                    BatteryLevel::Good,
                    BatteryStatus::Discharging,
                )),
            ),
        ]);

        assert_eq!(
            summary.tooltip().as_deref(),
            Some("OpenLogi · MX Master 3S 64%")
        );
    }

    #[test]
    fn a_charging_cold_start_reads_out_as_charging_not_zero_percent() {
        let summary = summary(vec![paired(
            1,
            Some("MX Master 3S"),
            DeviceKind::Mouse,
            true,
            Some(battery(
                Some(0),
                BatteryLevel::Good,
                BatteryStatus::Charging,
            )),
        )]);

        let tooltip = summary.tooltip().expect("a cold start is a readout");
        assert!(tooltip.contains(&rust_i18n::t!("device.charging").to_string()));
        assert!(!tooltip.contains("0%"));
    }

    #[test]
    fn an_error_is_selected_and_named() {
        let summary = summary(vec![paired(
            1,
            Some("MX Master 3S"),
            DeviceKind::Mouse,
            true,
            Some(battery(None, BatteryLevel::Unknown, BatteryStatus::Error)),
        )]);

        let tooltip = summary.tooltip().expect("the error is worth showing");
        assert!(tooltip.contains('–'));
        assert!(tooltip.contains(&rust_i18n::t!("device.battery_error").to_string()));
    }

    #[test]
    fn offline_devices_stay_in_the_menu_with_an_offline_word() {
        let rows: Vec<String> = summary(vec![
            paired(
                1,
                Some("MX Keys"),
                DeviceKind::Keyboard,
                false,
                Some(battery(
                    Some(15),
                    BatteryLevel::Low,
                    BatteryStatus::Discharging,
                )),
            ),
            paired(
                2,
                Some("MX Master 3S"),
                DeviceKind::Mouse,
                true,
                Some(battery(
                    Some(90),
                    BatteryLevel::Good,
                    BatteryStatus::Discharging,
                )),
            ),
        ])
        .menu_lines()
        .collect();

        assert_eq!(rows.len(), 2);
        assert!(rows[0].contains("15%"));
        assert!(rows[0].contains(&rust_i18n::t!("device.offline").to_string()));
        assert!(rows[1].contains("90%"));
    }

    #[test]
    fn the_menu_lists_at_most_six_devices() {
        let devices = (1..=7)
            .map(|slot| {
                paired(
                    slot,
                    Some(&format!("Device {slot}")),
                    DeviceKind::Mouse,
                    true,
                    Some(battery(
                        Some(90),
                        BatteryLevel::Good,
                        BatteryStatus::Discharging,
                    )),
                )
            })
            .collect();
        let rows: Vec<String> = summary(devices).menu_lines().collect();

        assert_eq!(rows.len(), MAX_MENU_ROWS);
        assert!(rows[0].contains("Device 1"));
        assert!(rows[MAX_MENU_ROWS - 1].contains("Device 6"));
    }

    #[test]
    fn a_missing_codename_falls_back_to_the_kind() {
        let summary = summary(vec![paired(
            1,
            None,
            DeviceKind::Keyboard,
            true,
            Some(battery(
                Some(50),
                BatteryLevel::Good,
                BatteryStatus::Discharging,
            )),
        )]);

        assert_eq!(
            summary.tooltip(),
            Some(format!(
                "OpenLogi · {} 50%",
                rust_i18n::t!("device.keyboard")
            ))
        );
    }

    #[test]
    fn nothing_to_read_out_falls_back_to_the_brand_tip() {
        assert_eq!(BatterySummary::from_inventory(&[]).tooltip(), None);
        assert_eq!(
            summary(vec![paired(
                1,
                Some("MX Master 3S"),
                DeviceKind::Mouse,
                true,
                None,
            )])
            .tooltip(),
            None
        );
        assert_eq!(
            summary(vec![paired(
                1,
                Some("MX Master 3S"),
                DeviceKind::Mouse,
                true,
                Some(battery(None, BatteryLevel::Unknown, BatteryStatus::Unknown)),
            )])
            .tooltip(),
            None
        );
    }

    #[test]
    fn a_charging_device_gets_the_menu_bolt_and_the_tooltip_word() {
        let summary = summary(vec![paired(
            1,
            Some("MX Master 3S"),
            DeviceKind::Mouse,
            true,
            Some(battery(
                Some(87),
                BatteryLevel::Good,
                BatteryStatus::Charging,
            )),
        )]);
        let rows: Vec<String> = summary.menu_lines().collect();

        assert!(rows[0].contains("87%"));
        assert!(rows[0].contains('⚡'));
        assert!(
            summary
                .tooltip()
                .expect("87% is a readout")
                .contains(&rust_i18n::t!("device.charging").to_string())
        );
    }

    #[test]
    fn the_macos_title_shows_the_percentage_or_the_charging_bolt() {
        let percent = summary(vec![paired(
            1,
            Some("MX Master 3S"),
            DeviceKind::Mouse,
            true,
            Some(battery(
                Some(87),
                BatteryLevel::Good,
                BatteryStatus::Discharging,
            )),
        )]);
        assert_eq!(percent.title().as_deref(), Some("87%"));

        // A cold-start 0% while charging must not read out as "0%".
        let charging = summary(vec![paired(
            1,
            Some("MX Master 3S"),
            DeviceKind::Mouse,
            true,
            Some(battery(
                Some(0),
                BatteryLevel::Good,
                BatteryStatus::Charging,
            )),
        )]);
        assert_eq!(charging.title().as_deref(), Some("⚡"));

        // A level-only reading has no compact form for the menu bar.
        let level_only = summary(vec![paired(
            1,
            Some("MX Master 3S"),
            DeviceKind::Mouse,
            true,
            Some(battery(
                None,
                BatteryLevel::Good,
                BatteryStatus::Discharging,
            )),
        )]);
        assert_eq!(level_only.title(), None);

        let error = summary(vec![paired(
            1,
            Some("MX Master 3S"),
            DeviceKind::Mouse,
            true,
            Some(battery(None, BatteryLevel::Unknown, BatteryStatus::Error)),
        )]);
        assert_eq!(error.title(), None);

        // No device with a readout keeps the glyph.
        assert_eq!(BatterySummary::from_inventory(&[]).title(), None);
    }
}
