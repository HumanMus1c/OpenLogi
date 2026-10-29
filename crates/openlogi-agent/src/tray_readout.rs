//! The readouts the tray shows: the device rows the menu lists, the tooltip,
//! and the icon.
//!
//! The selection and wording are pure over the observable snapshot, so the
//! rules the settings app uses ([`BatteryInfo::reading`] and
//! [`BatteryInfo::tone`]) are the rules the tray renders, and a light reads
//! out as its power and brightness the way the lighting panel does. The
//! platform sides own only the icon, tooltip, and menu plumbing.

use openlogi_core::config::{Config, battery_devices_named};
use openlogi_core::device::{
    BatteryDevice, BatteryTone, BatteryValue, DeviceInventory, DeviceKind, LightState,
    StandaloneDevice, first_battery_readout, prettify_codename,
};
use openlogi_ipc::ObservedLight;

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

/// One standalone light the menu lists: the identity a row needs, plus what
/// the light last read back (`None` while it reads nothing — raw-report
/// only, or not yet watched).
#[derive(Clone, Debug, PartialEq, Eq)]
struct LightRow {
    name: String,
    online: bool,
    state: Option<LightState>,
}

/// The menu's name for a standalone light: its tidied display name, or the
/// localized kind when the OS descriptor reported none.
fn light_name(device: &StandaloneDevice) -> String {
    let name = prettify_codename(&device.display_name);
    if name.is_empty() {
        rust_i18n::t!(device.kind.translation_key()).to_string()
    } else {
        name
    }
}

/// Collect the menu's light rows: every standalone light in standalone
/// order, with its readback matched by route.
fn light_rows(standalone: &[StandaloneDevice], lights: &[ObservedLight]) -> Vec<LightRow> {
    standalone
        .iter()
        .filter(|device| device.kind == DeviceKind::Light)
        .map(|device| LightRow {
            name: light_name(device),
            online: device.online,
            state: lights
                .iter()
                .find(|light| light.route == device.route())
                .map(|light| light.state),
        })
        .collect()
}

/// The value slot of a light row: the on word with the brightness the
/// settings app would show (normalized over the light's own range), the off
/// word alone, or the dash for no readback.
fn light_value(state: Option<LightState>) -> String {
    let Some(state) = state else {
        return "–".to_owned();
    };
    if !state.on {
        return rust_i18n::t!("common.off").to_string();
    }
    let on = rust_i18n::t!("common.on");
    let percent = state
        .brightness
        .zip(state.capabilities.brightness)
        .and_then(|(native, range)| range.percent_for_native(native));
    percent.map_or_else(|| on.to_string(), |percent| format!("{on} · {percent}%"))
}

/// One disabled menu line for a light: name, then the power/brightness
/// value, then the offline word.
fn light_menu_line(row: &LightRow) -> String {
    let mut line = format!("{}  {}", row.name, light_value(row.state));
    if !row.online {
        line.push_str("  · ");
        line.push_str(&rust_i18n::t!("device.offline"));
    }
    line
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

/// What the tray shows for one observable snapshot: the battery devices and
/// the standalone lights, with the rules that render them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TrayReadout {
    devices: Vec<BatteryDevice>,
    lights: Vec<LightRow>,
}

impl TrayReadout {
    /// Build the readout from one snapshot. `config` supplies the product
    /// names the settings app persisted; `None` (no config yet) keeps the
    /// firmware codenames the rows showed before.
    pub(crate) fn from_parts(
        inventory: &[DeviceInventory],
        standalone: &[StandaloneDevice],
        lights: &[ObservedLight],
        config: Option<&Config>,
    ) -> Self {
        Self {
            devices: config.map_or_else(
                || openlogi_core::device::battery_devices(inventory),
                |config| battery_devices_named(inventory, config),
            ),
            lights: light_rows(standalone, lights),
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
        self.battery_readout()
            .map(|readout| format!("{BRAND} · {readout}"))
    }

    /// The battery readout without the brand prefix, for surfaces that carry
    /// the brand separately (the Linux tray's tooltip title).
    pub(crate) fn battery_readout(&self) -> Option<String> {
        first_battery_readout(&self.devices).and_then(tooltip_readout)
    }

    /// The menu's device rows: the battery devices in inventory order, then
    /// the standalone lights, capped together.
    pub(crate) fn menu_lines(&self) -> impl Iterator<Item = String> + '_ {
        self.devices
            .iter()
            .map(menu_line)
            .chain(self.lights.iter().map(light_menu_line))
            .take(MAX_MENU_ROWS)
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
    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn has_rows(&self) -> bool {
        !self.devices.is_empty() || !self.lights.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_MENU_ROWS, TrayReadout};
    use openlogi_core::device::{
        BatteryInfo, BatteryLevel, BatteryStatus, ChangedOnLight, DeviceInventory, DeviceKind,
        LightCapabilities, LightState, LightValueRange, LightValueUnit, PairedDevice,
        RawDeviceAddress, ReceiverInfo, StandaloneDevice,
    };
    use openlogi_core::hid::DeviceRoute;
    use openlogi_ipc::ObservedLight;

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

    fn summary(devices: Vec<PairedDevice>) -> TrayReadout {
        TrayReadout::from_parts(&[inventory(devices)], &[], &[], None)
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
    fn a_persisted_product_name_replaces_the_codename() {
        use openlogi_core::config::{Config, DeviceIdentity};
        use openlogi_core::device::Capabilities;
        use openlogi_core::device_order::PhysicalDeviceKey;

        let mut config = Config::default();
        let key = PhysicalDeviceKey::parse("unit:4f4c4401").expect("a physical device key");
        config.adopt_route(&key, "receiver:ol-bolt-uid-0001:slot:1", None);
        config.set_device_identity(
            key.as_str(),
            DeviceIdentity {
                display_name: "MX Master 3S".to_owned(),
                model_info: None,
                codename: Some("Synthetic Performance Mouse".to_owned()),
                kind: DeviceKind::Mouse,
                capabilities: Capabilities::default(),
                light_capabilities: None,
                driver_id: None,
                registry_model_id: None,
            },
        );

        let device = || {
            paired(
                1,
                Some("Synthetic Performance Mouse"),
                DeviceKind::Mouse,
                true,
                Some(battery(
                    Some(64),
                    BatteryLevel::Good,
                    BatteryStatus::Discharging,
                )),
            )
        };
        let with_uid = |devices: Vec<PairedDevice>| {
            let mut inventory = inventory(devices);
            inventory.receiver.unique_id = Some("ol-bolt-uid-0001".to_owned());
            inventory
        };

        // The persisted name resolves through the same route/identity path the
        // settings app uses, so the row reads like the device's card.
        let readout = TrayReadout::from_parts(&[with_uid(vec![device()])], &[], &[], Some(&config));
        assert_eq!(
            readout.tooltip().as_deref(),
            Some("OpenLogi · MX Master 3S 64%")
        );

        // A user-assigned name outranks the product name, and clearing it
        // restores the product name.
        config.set_device_custom_name(key.as_str(), Some("Office mouse".to_owned()));
        let readout = TrayReadout::from_parts(&[with_uid(vec![device()])], &[], &[], Some(&config));
        assert_eq!(
            readout.tooltip().as_deref(),
            Some("OpenLogi · Office mouse 64%")
        );
        config.set_device_custom_name(key.as_str(), None);

        // An unknown device (no entry) and a run without a config keep the
        // prettified codename.
        let unknown = paired(
            2,
            Some("Synthetic Offline Mouse"),
            DeviceKind::Mouse,
            false,
            None,
        );
        let rows: Vec<String> =
            TrayReadout::from_parts(&[with_uid(vec![unknown])], &[], &[], Some(&config))
                .menu_lines()
                .collect();
        assert!(rows[0].starts_with("Synthetic Offline Mouse"));

        let rows: Vec<String> =
            TrayReadout::from_parts(&[with_uid(vec![device()])], &[], &[], None)
                .menu_lines()
                .collect();
        assert!(rows[0].starts_with("Synthetic Performance Mouse"));
    }

    #[test]
    fn nothing_to_read_out_falls_back_to_the_brand_tip() {
        assert_eq!(TrayReadout::from_parts(&[], &[], &[], None).tooltip(), None);
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
        assert_eq!(TrayReadout::from_parts(&[], &[], &[], None).title(), None);
    }

    fn percent_range() -> LightCapabilities {
        LightCapabilities {
            power: true,
            brightness: Some(
                LightValueRange::new(0, 100, 1, LightValueUnit::Percent).expect("percent range"),
            ),
            ..LightCapabilities::default()
        }
    }

    fn light_state(
        capabilities: LightCapabilities,
        on: bool,
        brightness: Option<u16>,
    ) -> LightState {
        LightState {
            capabilities,
            on,
            brightness,
            temperature_kelvin: None,
            brightness_limit: None,
        }
    }

    fn standalone_light(name: &str, online: bool) -> StandaloneDevice {
        StandaloneDevice {
            address: RawDeviceAddress {
                vendor_id: 0x046d,
                product_id: 0xc900,
                usage_page: 0xff43,
                usage_id: 0x0202,
                identity: "serial:glow-1".to_owned(),
            },
            display_name: name.to_owned(),
            manufacturer: None,
            serial_number: None,
            unit_id: [1, 2, 3, 4],
            kind: DeviceKind::Light,
            online,
            capabilities: None,
            light_capabilities: None,
            driver_id: "litra".to_owned(),
            registry_model_id: None,
        }
    }

    fn observed(route: DeviceRoute, state: LightState) -> ObservedLight {
        ObservedLight {
            route,
            state,
            changed_on_light: ChangedOnLight::default(),
        }
    }

    fn summary_with_lights(
        devices: &[PairedDevice],
        standalone: &[StandaloneDevice],
        lights: &[ObservedLight],
    ) -> TrayReadout {
        TrayReadout::from_parts(&[inventory(devices.to_vec())], standalone, lights, None)
    }

    #[test]
    fn a_lit_light_reads_out_as_on_with_its_brightness() {
        let device = standalone_light("Synthetic Studio Light", true);
        let route = device.route();
        let rows: Vec<String> = summary_with_lights(
            &[],
            &[device],
            &[observed(
                route,
                light_state(percent_range(), true, Some(42)),
            )],
        )
        .menu_lines()
        .collect();

        assert_eq!(
            rows,
            vec![format!(
                "Synthetic Studio Light  {} · 42%",
                rust_i18n::t!("common.on")
            )]
        );
    }

    #[test]
    fn a_lumen_range_reads_out_as_a_normalized_percent() {
        let capabilities = LightCapabilities {
            power: true,
            brightness: Some(
                LightValueRange::new(20, 150, 1, LightValueUnit::Lumens).expect("lumen range"),
            ),
            ..LightCapabilities::default()
        };
        let mut device = standalone_light("Studio Light", true);
        device.light_capabilities = Some(capabilities);
        let route = device.route();
        let rows: Vec<String> = summary_with_lights(
            &[],
            &[device],
            &[observed(route, light_state(capabilities, true, Some(150)))],
        )
        .menu_lines()
        .collect();

        assert!(rows[0].ends_with(&format!("{} · 100%", rust_i18n::t!("common.on"))));
    }

    #[test]
    fn an_off_light_reads_out_as_the_off_word_without_a_percent() {
        let device = standalone_light("Studio Light", true);
        let route = device.route();
        let rows: Vec<String> = summary_with_lights(
            &[],
            &[device],
            &[observed(
                route,
                light_state(percent_range(), false, Some(42)),
            )],
        )
        .menu_lines()
        .collect();

        assert_eq!(
            rows,
            vec![format!("Studio Light  {}", rust_i18n::t!("common.off"))]
        );
    }

    #[test]
    fn a_light_without_a_readback_shows_the_dash() {
        let rows: Vec<String> =
            summary_with_lights(&[], &[standalone_light("Studio Light", true)], &[])
                .menu_lines()
                .collect();

        assert_eq!(rows, vec!["Studio Light  –"]);
    }

    #[test]
    fn an_offline_light_keeps_its_row_with_the_offline_word() {
        let rows: Vec<String> =
            summary_with_lights(&[], &[standalone_light("Studio Light", false)], &[])
                .menu_lines()
                .collect();

        assert_eq!(
            rows,
            vec![format!(
                "Studio Light  –  · {}",
                rust_i18n::t!("device.offline")
            )]
        );
    }

    #[test]
    fn a_nameless_light_falls_back_to_the_kind() {
        let rows: Vec<String> = summary_with_lights(&[], &[standalone_light("", true)], &[])
            .menu_lines()
            .collect();

        assert_eq!(
            rows,
            vec![format!("{}  –", rust_i18n::t!("device.lighting"))]
        );
    }

    #[test]
    fn lights_list_after_the_battery_devices() {
        let rows: Vec<String> = summary_with_lights(
            &[paired(
                1,
                Some("MX Master 3S"),
                DeviceKind::Mouse,
                true,
                Some(battery(
                    Some(90),
                    BatteryLevel::Good,
                    BatteryStatus::Discharging,
                )),
            )],
            &[standalone_light("Studio Light", true)],
            &[],
        )
        .menu_lines()
        .collect();

        assert_eq!(rows.len(), 2);
        assert!(rows[0].starts_with("MX Master 3S"));
        assert!(rows[1].starts_with("Studio Light"));
    }

    #[test]
    fn lights_have_no_battery_readout_so_the_brand_tip_stays() {
        let device = standalone_light("Studio Light", true);
        let route = device.route();
        let summary = summary_with_lights(
            &[],
            &[device],
            &[observed(
                route,
                light_state(percent_range(), true, Some(42)),
            )],
        );

        assert_eq!(summary.tooltip(), None);
        assert_eq!(summary.battery_readout(), None);
        assert_eq!(summary.title(), None);
        assert!(summary.has_rows());
    }

    #[test]
    fn the_cap_covers_battery_rows_and_lights_together() {
        let devices: Vec<PairedDevice> = (1..=6)
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
        let standalone: Vec<StandaloneDevice> = (0..2)
            .map(|n| standalone_light(&format!("Light {n}"), true))
            .collect();
        let lights: Vec<ObservedLight> = standalone
            .iter()
            .map(|device| observed(device.route(), light_state(percent_range(), true, None)))
            .collect();
        let rows: Vec<String> = summary_with_lights(&devices, &standalone, &lights)
            .menu_lines()
            .collect();

        assert_eq!(rows.len(), MAX_MENU_ROWS);
        assert!(rows[MAX_MENU_ROWS - 1].contains("Device 6"));
        assert!(rows.iter().all(|row| !row.contains("Light")));
    }
}
