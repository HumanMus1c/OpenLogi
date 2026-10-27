//! Serializable device-model types.
//!
//! These mirror the HID++ types from the `hidpp` crate but live here so the
//! CLI and any future GUI can depend on them without dragging in the protocol
//! crate or its async transport.

use serde::{Deserialize, Serialize};

use crate::hid::DeviceRoute;

mod light;

pub use light::{
    ChangedOnLight, LightCapabilities, LightState, LightValueRange, LightValueRangeError,
    LightValueUnit,
};

/// What a paired peripheral is. Mirrors `hidpp::receiver::bolt::BoltDeviceKind`
/// but is owned by us so consumers don't depend on `hidpp`.
///
/// Several upstream "device type" vocabularies feed this one enum, and they do
/// **not** agree on numbers: the Bolt pairing register uses `Unknown=0,
/// Keyboard=1, Mouse=2, …`, while the HID++ `0x0005` feature uses
/// `Keyboard=0, …, Mouse=3, …` (no `Unknown` at all). The asset registry adds a
/// third, free-form *string* type (`"mouse"`, case-inconsistently `"MOUSE"`).
/// They are converted to this enum at their respective boundaries — never by
/// reinterpreting one source's raw byte with another's table — so the numeric
/// mismatch can't leak past those mappers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeviceKind {
    /// Mice — the family OpenLogi's binding/DPI panels primarily target.
    Mouse,
    /// Keyboards, including lighting-capable ones.
    Keyboard,
    /// Standalone numeric keypads.
    Numpad,
    /// Presentation remotes (slide clickers).
    Presenter,
    /// Remote controls; the registry's `"remotecontrol"` string also folds here.
    Remote,
    /// Trackballs — treated like mice for presumed capabilities.
    Trackball,
    /// External touchpads; the registry's `"trackpad"` string also folds here.
    Touchpad,
    /// Pen/graphics tablets.
    Tablet,
    /// Game controllers, mirrored from the Bolt pairing vocabulary.
    Gamepad,
    /// Joysticks, mirrored from the Bolt pairing vocabulary.
    Joystick,
    /// Audio headsets paired through a receiver.
    Headset,
    /// Logitech webcam (UVC), configured through `openlogi-camera`.
    Camera,
    /// Not classified by any source.
    Unknown,
    /// Standalone light or other illumination device controlled outside HID++.
    ///
    /// This is an identity hint only. UI controls are gated by the dedicated
    /// light capability descriptor, never by this variant alone.
    Light,
}

impl DeviceKind {
    /// Parse the OpenLogi asset registry's `type` string into a [`DeviceKind`].
    ///
    /// The registry field is free-form and case-inconsistent (both `"mouse"`
    /// and `"MOUSE"` ship), so we case-fold before matching. `None` for values
    /// we don't model — "no asset opinion", typed as the absence of one rather
    /// than overloading [`DeviceKind::Unknown`], which stays a real wire value
    /// a live device can carry.
    #[must_use]
    pub fn from_registry_type(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "mouse" => Some(Self::Mouse),
            "keyboard" => Some(Self::Keyboard),
            "numpad" => Some(Self::Numpad),
            "presenter" => Some(Self::Presenter),
            "remote" | "remotecontrol" => Some(Self::Remote),
            "trackball" => Some(Self::Trackball),
            "touchpad" | "trackpad" => Some(Self::Touchpad),
            "tablet" => Some(Self::Tablet),
            "gamepad" => Some(Self::Gamepad),
            "joystick" => Some(Self::Joystick),
            "headset" => Some(Self::Headset),
            "camera" => Some(Self::Camera),
            "light" | "lighting" | "illumination_light" => Some(Self::Light),
            _ => None,
        }
    }

    /// Stable catalog key for the localized name of the kind, shared by every
    /// binary that names a device by what it is.
    #[must_use]
    pub const fn translation_key(self) -> &'static str {
        match self {
            Self::Mouse => "device.mouse",
            Self::Keyboard => "device.keyboard",
            Self::Numpad => "device.numpad",
            Self::Presenter => "device.presenter",
            Self::Remote => "device.remote",
            Self::Trackball => "device.trackball",
            Self::Touchpad => "device.touchpad",
            Self::Tablet => "device.tablet",
            Self::Gamepad => "device.gamepad",
            Self::Joystick => "device.joystick",
            Self::Headset => "device.headset",
            Self::Camera => "camera.camera",
            Self::Unknown => "device.device",
            Self::Light => "device.lighting",
        }
    }
}

/// What a device can be *configured* to do, derived from the HID++ feature
/// table it reports (feature `0x0001`). This is the source of truth for which
/// configuration panels the UI offers — a panel shows iff the device exposes
/// the feature that drives it. Gating on capability rather than on
/// [`DeviceKind`] is what keeps a misclassified device from losing its panels
/// (issue #127): kind is an identity guess, capability is what the firmware
/// actually announced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "capabilities is a serialized feature-bit DTO; independent booleans keep the IPC/config shape explicit"
)]
pub struct Capabilities {
    /// Reprogrammable buttons — HID++ `0x1b00`–`0x1b04` (ReprogControls).
    pub buttons: bool,
    /// Adjustable pointer resolution — HID++ `0x2201` / `0x2202` (AdjustableDpi).
    pub pointer: bool,
    /// Solid-colour RGB the lighting panel can drive — HID++ effect engines
    /// (`0x8070` / `0x8071`) or per-zone lighting (`0x8080` / `0x8081`).
    /// Backlight-only families aren't driven by the panel, so they don't earn
    /// an inert Lighting tab.
    pub lighting: bool,
    /// Native vertical wheel inversion — HID++ `0x2121 HiResWheel` with the
    /// firmware-reported `has_invert` capability.
    pub scroll_inversion: bool,
    /// HID++ `0x2121 HiResWheel` is present, so the wheel reporting resolution
    /// can be read and changed independently of inversion support.
    #[serde(default)]
    pub hires_wheel: bool,
    /// A horizontal thumb wheel is available: either the dedicated HID++
    /// `0x2150 Thumbwheel` feature or a legacy `0x6501 Gestures2` descriptor
    /// (gesture id 46, used by MX Master 2S).
    #[serde(default)]
    pub thumbwheel: bool,
    /// Programmable haptic feedback — reverse-engineered HID++ `0x19b0`.
    #[serde(default)]
    pub haptic_feedback: bool,
    /// A divertable Haptic Sense Panel control (`0x01a0`) was found in the
    /// device's `0x1b04` control table.
    #[serde(default)]
    pub haptic_panel: bool,
    /// A DPI/ModeShift control in the device's `0x1b04` control table supports
    /// both diversion and raw-XY reporting for hold-and-swipe gestures.
    #[serde(default)]
    pub dpi_gestures: bool,
    /// Keyboard Fn-lock can be read and written — HID++ `0x40a0`, `0x40a2` or
    /// `0x40a3` fn inversion. Appended: crosses the IPC and the persisted
    /// identity.
    #[serde(default)]
    pub fn_lock: bool,
    /// The keyboard's key layout can be kept on the host OS — HID++ `0x4531
    /// MultiPlatform`, when the probe also reads that it lets software set the
    /// platform. Appended: crosses the IPC and the persisted identity.
    #[serde(default)]
    pub os_layout: bool,
    /// The device's report rate can be chosen — HID++ `0x8060
    /// AdjustableReportRate` or `0x8061 ExtendedAdjustableReportRate`.
    /// Appended: crosses the IPC and the persisted identity.
    #[serde(default)]
    pub report_rate: bool,
    /// The keyboard's backlight can be switched and tuned — HID++ `0x1982
    /// Backlight`. Appended: crosses the IPC and the persisted identity.
    #[serde(default)]
    pub backlight: bool,
    /// The device has Easy-Switch host slots whose computers' names and OS
    /// software can read — HID++ `0x1815 HostsInfo`. Appended: crosses the
    /// IPC and the persisted identity.
    #[serde(default)]
    pub easy_switch: bool,
    /// Back or Forward in the device's `0x1b04` control table can be diverted
    /// with raw-wheel reporting, so a wheel-hold binding can turn the main
    /// wheel into a virtual thumb wheel. Appended: crosses the IPC and the
    /// persisted identity.
    #[serde(default)]
    pub virtual_thumbwheel: bool,
    /// The pointer speed is set by scaling the sensor's motion — HID++
    /// `0x2205 PointerMotionScaling` — because the device has no adjustable
    /// DPI. A device that also reports [`Self::pointer`] keeps it `false`:
    /// the DPI control already sets its speed, and Logi Options+ uses DPI
    /// there too. Appended: crosses the IPC and the persisted identity.
    #[serde(default)]
    pub pointer_scaling: bool,
    /// The press force of a force-sensing button — the MX Master 4's Haptic
    /// Sense Panel — can be changed: reverse-engineered HID++ `0x19c0
    /// ForceSensingButton`, when the probe also reads that the device lets
    /// software change it. Appended: crosses the IPC and the persisted
    /// identity.
    #[serde(default)]
    pub force_sensing: bool,
    /// Fixed lock/system-key disabling through HID++ `0x4521 DisableKeys`.
    /// Measured only from feature-table presence. Appended after
    /// `force_sensing`: crosses the IPC and the persisted identity.
    #[serde(default)]
    pub disable_keys: bool,
}

impl Capabilities {
    /// Derive capabilities from the set of HID++ feature IDs a device reports.
    /// Membership of a driving feature ID flips the corresponding flag.
    #[must_use]
    pub fn from_feature_ids(ids: &[u16]) -> Self {
        const BUTTONS: [u16; 5] = [0x1b00, 0x1b01, 0x1b02, 0x1b03, 0x1b04];
        const POINTER: [u16; 2] = [0x2201, 0x2202];
        // Every family here is driven by `set_keyboard_color`, which tries
        // effect engines before per-zone paths. The 0x198x backlight is not
        // lighting: `backlight` below covers 0x1982.
        const LIGHTING: [u16; 4] = [0x8070, 0x8071, 0x8081, 0x8080];
        let has = |family: &[u16]| ids.iter().any(|id| family.contains(id));
        let pointer = has(&POINTER);
        Self {
            buttons: has(&BUTTONS),
            pointer,
            lighting: has(&LIGHTING),
            scroll_inversion: false,
            hires_wheel: ids.contains(&0x2121),
            thumbwheel: ids.contains(&0x2150),
            haptic_feedback: ids.contains(&0x19b0),
            haptic_panel: false,
            dpi_gestures: false,
            fn_lock: ids.contains(&0x40a0) || ids.contains(&0x40a2) || ids.contains(&0x40a3),
            // The probe clears it unless the keyboard lets software set the
            // platform; the feature table alone cannot say.
            os_layout: ids.contains(&0x4531),
            report_rate: ids.contains(&0x8060) || ids.contains(&0x8061),
            backlight: ids.contains(&0x1982),
            easy_switch: ids.contains(&0x1815),
            // Set by the `0x1b04` control walk; the feature table alone
            // cannot say which controls report raw wheel.
            virtual_thumbwheel: false,
            pointer_scaling: !pointer && ids.contains(&0x2205),
            // The probe clears it unless the device lets software change the
            // force; the feature table alone cannot say.
            force_sensing: ids.contains(&0x19c0),
            disable_keys: ids.contains(&0x4521),
        }
    }

    /// Best-effort capabilities for a device we could not probe (offline /
    /// never reached), guessed from its [`DeviceKind`]. Used only as a fallback
    /// when no measured [`Capabilities`] exist — a sleeping mouse should still
    /// show its button/pointer panels so its bindings (host-side) stay
    /// configurable.
    #[must_use]
    pub fn presumed_from_kind(kind: DeviceKind) -> Self {
        match kind {
            DeviceKind::Mouse | DeviceKind::Trackball => Self {
                buttons: true,
                pointer: true,
                ..Self::default()
            },
            DeviceKind::Keyboard => Self {
                lighting: true,
                ..Self::default()
            },
            _ => Self::default(),
        }
    }
}

/// Coarse battery bucket reported by the device firmware.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BatteryLevel {
    /// Almost depleted — the firmware's most urgent bucket.
    Critical,
    /// Running low; worth surfacing a charge hint.
    Low,
    /// Comfortable middle range, no user action needed.
    Good,
    /// At or near full charge.
    Full,
    /// The firmware did not report a level, or reported one we don't model.
    Unknown,
}

/// Charging state. Mirrors `hidpp 0.2`'s `BatteryStatus` plus `Unknown` for
/// values added in future protocol versions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BatteryStatus {
    /// Running on battery.
    Discharging,
    /// Charging at the normal rate.
    Charging,
    /// Charging at reduced current (e.g. from a weak power source).
    ChargingSlow,
    /// Charge complete while still connected to power.
    Full,
    /// The device reported a charging fault.
    Error,
    /// A status value this build doesn't model (future protocol additions).
    Unknown,
}

/// Battery snapshot for one paired device, as last polled over HID++.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatteryInfo {
    /// Reported charge percentage (`0..=100`), or `None` when the reading
    /// carries no percentage.
    ///
    /// `None` is not an empty battery. Some devices only report a coarse
    /// [`level`](Self::level), such as a `0x1004` device without the
    /// state-of-charge capability. Others have no reading yet, such as a
    /// device first seen while it charges. Read `level` in that case.
    pub percentage: Option<u8>,
    /// Coarse bucket for UI that doesn't want the raw percentage.
    pub level: BatteryLevel,
    /// Charging state at poll time.
    pub status: BatteryStatus,
}

/// What a battery reading can show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatteryReading {
    /// A measured state of charge.
    Percent(u8),
    /// Only the firmware's coarse level: the device has no state-of-charge
    /// gauge, so there is no percentage to show.
    Level(BatteryLevel),
    /// Neither a percentage nor a level is usable yet.
    NoReading,
}

/// What a battery surface shows as the value: the percentage, a coarse level
/// word, "Charging", or nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatteryValue {
    /// A measured state of charge.
    Percent(u8),
    /// A level-only reading, localized by the surface.
    Level(BatteryLevel),
    /// A charging cold start with no reading yet.
    Charging,
    /// Nothing usable to show.
    None,
}

impl BatteryValue {
    /// The shared catalog key for the value's localized word, when it has one.
    ///
    /// Percentages format themselves and [`BatteryValue::None`] is a dash, so
    /// only the level words and "Charging" resolve through the catalogs.
    #[must_use]
    pub const fn translation_key(self) -> Option<&'static str> {
        match self {
            Self::Level(BatteryLevel::Full) => Some("device.full"),
            Self::Level(BatteryLevel::Good) => Some("device.battery_good"),
            Self::Level(BatteryLevel::Low | BatteryLevel::Critical) => Some("device.low_battery"),
            Self::Percent(_) | Self::Level(BatteryLevel::Unknown) | Self::None => None,
            Self::Charging => Some("device.charging"),
        }
    }
}

/// The semantic colour role a battery reading maps to.
///
/// A surface chooses the actual colours; this shared mapping keeps the
/// settings app, the tray, and the floating widget on the same tone for the
/// same device state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatteryTone {
    /// A healthy discharging reading.
    Normal,
    /// Charging at either rate.
    Charging,
    /// Charge complete.
    Full,
    /// Low, by [`BatteryInfo::needs_attention`].
    Low,
    /// The firmware's most urgent bucket.
    Critical,
    /// The device reported a charging fault.
    Error,
}

/// The charge at or below which a discharging battery needs attention.
const LOW_BATTERY_PERCENT: u8 = 20;

impl BatteryInfo {
    /// What this reading can show: a measured percentage, the firmware's
    /// coarse level, or nothing usable yet.
    ///
    /// Some devices cannot gauge charge while under load. On a cold start they
    /// report 0% until a later poll, which must not be presented as an empty
    /// or critically depleted battery.
    #[must_use]
    pub fn reading(&self) -> BatteryReading {
        match self.percentage {
            Some(0) if self.is_charging() => BatteryReading::NoReading,
            Some(percentage) => BatteryReading::Percent(percentage),
            None if matches!(self.level, BatteryLevel::Unknown) => BatteryReading::NoReading,
            None => BatteryReading::Level(self.level),
        }
    }

    /// What a surface shows as this reading's value.
    #[must_use]
    pub fn value(&self) -> BatteryValue {
        match self.reading() {
            BatteryReading::Percent(percentage) => BatteryValue::Percent(percentage),
            BatteryReading::Level(level) => BatteryValue::Level(level),
            BatteryReading::NoReading if self.charging_without_reading() => BatteryValue::Charging,
            BatteryReading::NoReading => BatteryValue::None,
        }
    }

    /// The colour role this reading maps to.
    ///
    /// Status decides first: charging, full, and error readings never carry a
    /// low-battery tone. A discharging reading is critical only at the
    /// firmware's [`BatteryLevel::Critical`]; otherwise
    /// [`Self::needs_attention`] decides between low and normal.
    #[must_use]
    pub fn tone(&self) -> BatteryTone {
        match self.status {
            BatteryStatus::Charging | BatteryStatus::ChargingSlow => BatteryTone::Charging,
            BatteryStatus::Error => BatteryTone::Error,
            BatteryStatus::Full => BatteryTone::Full,
            BatteryStatus::Discharging | BatteryStatus::Unknown => {
                if self.level == BatteryLevel::Critical {
                    BatteryTone::Critical
                } else if self.needs_attention() {
                    BatteryTone::Low
                } else {
                    BatteryTone::Normal
                }
            }
        }
    }

    /// How full a bar should draw this reading, in percent.
    ///
    /// A level-only reading fills to a fixed point inside its band — a coarse
    /// representation, not a charge estimate. `None` means no usable reading:
    /// the bar keeps an empty track and must never show `0%`.
    #[must_use]
    pub fn bar_percentage(&self) -> Option<u8> {
        match self.reading() {
            BatteryReading::Percent(percentage) => Some(percentage),
            BatteryReading::Level(BatteryLevel::Critical) => Some(10),
            BatteryReading::Level(BatteryLevel::Low) => Some(25),
            BatteryReading::Level(BatteryLevel::Good) => Some(70),
            BatteryReading::Level(BatteryLevel::Full) => Some(100),
            BatteryReading::Level(BatteryLevel::Unknown) | BatteryReading::NoReading => None,
        }
    }

    /// Whether the device is charging with neither a percentage nor a level
    /// to show, so "Charging" is all there is to say.
    #[must_use]
    pub fn charging_without_reading(&self) -> bool {
        self.is_charging() && matches!(self.reading(), BatteryReading::NoReading)
    }

    /// Charging at either rate; [`BatteryStatus::Full`] is charge complete,
    /// not charging.
    fn is_charging(&self) -> bool {
        matches!(
            self.status,
            BatteryStatus::Charging | BatteryStatus::ChargingSlow
        )
    }

    /// Whether this reading should draw the user's attention: a discharging
    /// battery at 20% or less, or, on a device that reports no percentage, at
    /// the firmware's low or critical level.
    ///
    /// Every low-battery cue asks this one rule: the settings app's warning
    /// tone and caption, and the agent's low-battery notice.
    #[must_use]
    pub fn needs_attention(&self) -> bool {
        let low = match self.percentage {
            Some(percentage) => percentage <= LOW_BATTERY_PERCENT,
            None => matches!(self.level, BatteryLevel::Low | BatteryLevel::Critical),
        };
        low && !matches!(
            self.status,
            BatteryStatus::Charging | BatteryStatus::ChargingSlow | BatteryStatus::Full
        )
    }
}

/// A device as a battery surface sees it: the identity a row needs, plus the
/// reading the agent last observed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatteryDevice {
    /// Firmware codename tidied for display. `None` when the device reported
    /// no usable name; the surface falls back to its kind's localized name.
    pub name: Option<String>,
    /// Best-guess classification; surfaces use it for the row glyph.
    pub kind: DeviceKind,
    /// Whether the device was reachable at the last enumeration.
    pub online: bool,
    /// Last battery reading, `None` when the device reports no battery.
    pub battery: Option<BatteryInfo>,
}

impl BatteryDevice {
    /// Project one paired device onto the facts a battery surface needs.
    #[must_use]
    pub fn from_paired(device: &PairedDevice) -> Self {
        let name = device
            .codename
            .as_deref()
            .map(prettify_codename)
            .filter(|name| !name.is_empty());
        Self {
            name,
            kind: device.kind,
            online: device.online,
            battery: device.battery.clone(),
        }
    }

    /// Whether this device can contribute a visible readout. A charging cold
    /// start shows "Charging" and an error has a word even without a
    /// percentage; a reading with neither stays out.
    #[must_use]
    pub fn has_readout(&self) -> bool {
        self.battery.as_ref().is_some_and(|battery| {
            battery.reading() != BatteryReading::NoReading
                || battery.charging_without_reading()
                || battery.tone() == BatteryTone::Error
        })
    }
}

/// Every paired device in an inventory snapshot, in inventory order.
#[must_use]
pub fn battery_devices(inventory: &[DeviceInventory]) -> Vec<BatteryDevice> {
    inventory
        .iter()
        .flat_map(|inventory| inventory.paired.iter())
        .map(BatteryDevice::from_paired)
        .collect()
}

/// The first online device with something to show, for a single-device readout
/// such as the tray tooltip.
#[must_use]
pub fn first_battery_readout(devices: &[BatteryDevice]) -> Option<&BatteryDevice> {
    devices
        .iter()
        .find(|device| device.online && device.has_readout())
}

/// Rows for the floating indicator: devices with something to show, offline
/// ones only when `show_offline`, sorted low-first and capped at `limit`.
///
/// Charging and full devices sort after healthy discharging ones, and readings
/// with no value sort last. The tie-break is the display name, so a stable
/// inventory does not reorder equal rows between polls.
#[must_use]
pub fn flow_indicator_devices(
    devices: &[BatteryDevice],
    show_offline: bool,
    limit: usize,
) -> Vec<&BatteryDevice> {
    let mut rows: Vec<&BatteryDevice> = devices
        .iter()
        .filter(|device| device.has_readout() && (device.online || show_offline))
        .collect();
    rows.sort_by_key(|device| indicator_sort_key(device));
    rows.truncate(limit);
    rows
}

fn indicator_sort_key(device: &BatteryDevice) -> (u8, u8, bool, String) {
    let tone = device.battery.as_ref().map(BatteryInfo::tone);
    let rank = match tone {
        Some(BatteryTone::Critical | BatteryTone::Low | BatteryTone::Error) => 0,
        Some(BatteryTone::Normal) => 1,
        Some(BatteryTone::Charging | BatteryTone::Full) => 2,
        None => 3,
    };
    let fill = device
        .battery
        .as_ref()
        .and_then(BatteryInfo::bar_percentage)
        .unwrap_or(u8::MAX);
    (
        rank,
        fill,
        !device.online,
        device.name.clone().unwrap_or_default(),
    )
}

/// Tidy a device name reported over HID++ for display.
///
/// Logitech reports gaming names in ALL CAPS (e.g. `"G513 RGB MECHANICAL
/// GAMING KEYBOARD"`). This title-cases each word so the name reads like the
/// curated asset names (`"MX Master 3S"`), and keeps model numbers (tokens
/// with a digit, e.g. `G513`) and short acronyms (`RGB`, `TKL`, `SE`) as they
/// are. A name already in mixed case is returned unchanged.
#[must_use]
pub fn prettify_codename(raw: &str) -> String {
    if raw.chars().any(char::is_lowercase) {
        return raw.to_string();
    }
    raw.split_whitespace()
        .map(|word| {
            if word.len() <= 3 || word.bytes().any(|b| b.is_ascii_digit()) {
                word.to_string()
            } else {
                let mut chars = word.chars();
                chars.next().map_or_else(String::new, |first| {
                    first.to_uppercase().collect::<String>() + &chars.as_str().to_lowercase()
                })
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// A keyboard lock key whose state the on-screen indicator reports.
///
/// Variants are append-only because this enum crosses bincode IPC.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LockKey {
    /// Num Lock.
    NumLock,
    /// Caps Lock.
    CapsLock,
    /// Scroll Lock.
    ScrollLock,
}

impl LockKey {
    /// Whether this platform shows the indicator for this key.
    ///
    /// Caps Lock shows everywhere. macOS gives Num Lock and Scroll Lock no
    /// meaning, so they show only on the other platforms. Logi Options+ marks
    /// both Windows-only; it ships for Windows and macOS only, and Linux treats
    /// these keys as Windows does.
    #[must_use]
    pub const fn is_indicated_here(self) -> bool {
        match self {
            Self::CapsLock => true,
            Self::NumLock | Self::ScrollLock => !cfg!(target_os = "macos"),
        }
    }
}

/// Identity of an enumerated receiver — no paired-device state (that lives
/// in [`DeviceInventory::paired`]). For a direct (Bluetooth/wired) device,
/// a synthetic entry mirroring the device's own HID identity fills this role.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReceiverInfo {
    /// Product string from the HID enumeration (e.g. `"Logi Bolt Receiver"`).
    pub name: String,
    /// USB vendor ID (`0x046d` for Logitech).
    pub vendor_id: u16,
    /// USB product ID distinguishing the receiver model.
    pub product_id: u16,
    /// Platform-reported serial, when one is exposed. Deliberately excluded
    /// from diagnostics (see [`crate::diagnostics::ReceiverDiag`]).
    pub unique_id: Option<String>,
}

/// HID++ `DeviceInformation` (feature 0x0003) snapshot used to identify a
/// device against external registries (e.g. the OpenLogi asset index).
///
/// `model_ids` is the per-transport PID array reported by the firmware,
/// ordered to match the transports flagged in [`Self::transports`] (USB,
/// eQuad, BTLE, Bluetooth) — slots that aren't enabled stay `0`. The Logi
/// Options+ asset registry's `modelId` (e.g. `"6b023"`) is the concatenation
/// of an extended-model byte and one of these PIDs, so callers usually want
/// to format `extended_model_id` + `model_ids[N]` to match.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceModelInfo {
    /// Number of firmware entities (main firmware, bootloader, …) the
    /// device reports.
    pub entity_count: u8,
    /// HID++ DeviceInformation serial number, when the device supports the
    /// optional serial-number function.
    pub serial_number: Option<String>,
    /// Per-unit ID bytes — unique to the physical unit, unlike the
    /// model-level fields around it.
    pub unit_id: [u8; 4],
    /// Which transports the firmware supports; defines the slot order of
    /// [`Self::model_ids`].
    pub transports: DeviceTransports,
    /// Per-transport PIDs for the transports enabled in [`Self::transports`],
    /// packed from the front in ascending HID++ transport-bit order:
    /// Bluetooth (bit 0), BTLE (bit 1), eQuad (bit 2), USB (bit 3). Only
    /// enabled transports take a slot — a disabled transport is skipped, not
    /// zero-filled — so unused trailing slots stay `0` and the array holds at
    /// most three PIDs.
    ///
    /// Note this is the reverse of the field order of [`DeviceTransports`],
    /// which lists `usb` first. A device with `usb + equad` reports
    /// `[eQuad PID, USB PID, 0]`; one with `equad + btle` reports
    /// `[BTLE PID, eQuad PID, 0]`.
    pub model_ids: [u16; 3],
    /// Extra model byte prefixed to a PID to form the asset registry's
    /// `modelId` — see [`Self::model_key`].
    pub extended_model_id: u8,
}

impl DeviceModelInfo {
    /// The model's key in the OpenLogi asset registry: the string the depot
    /// `manifest.json` uses for its `modelId` field, formatted
    /// `{extended_model_id:x}{model_ids[0]:04x}`. An MX Master 4 with
    /// `extended_model_id = 0x02` and `model_ids[0] = 0xb042` resolves to
    /// `"2b042"`. Per-device configuration is keyed by
    /// [`crate::device_order::DeviceIdentity::config_key`], not by this.
    #[must_use]
    pub fn model_key(&self) -> String {
        format!("{:x}{:04x}", self.extended_model_id, self.model_ids[0])
    }

    /// Identifies which transport `product_id` belongs to, by matching it
    /// against the packed per-transport [`Self::model_ids`] slots.
    ///
    /// A `Direct` route only ever carries one vendor/product id — the HID
    /// node this session actually enumerated — so on a multi-transport
    /// device this pinpoints the transport that's *live right now*, as
    /// opposed to [`Self::transports`], which is the static set the firmware
    /// merely claims to support. Returns `None` when `product_id` matches
    /// none of the enabled slots (unknown model info, or a PID this device
    /// doesn't report).
    #[must_use]
    pub fn transport_for_product_id(&self, product_id: u16) -> Option<ModelTransport> {
        let t = self.transports;
        [
            (t.bluetooth, ModelTransport::Bluetooth),
            (t.btle, ModelTransport::Btle),
            (t.equad, ModelTransport::Equad),
            (t.usb, ModelTransport::Usb),
        ]
        .into_iter()
        .filter_map(|(enabled, kind)| enabled.then_some(kind))
        .zip(self.model_ids)
        .find(|(_, pid)| *pid == product_id)
        .map(|(kind, _)| kind)
    }
}

/// One of the individual transports packed into [`DeviceModelInfo::model_ids`],
/// as resolved by [`DeviceModelInfo::transport_for_product_id`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelTransport {
    /// Classic Bluetooth.
    Bluetooth,
    /// Bluetooth Low Energy.
    Btle,
    /// Logitech eQuad (Unifying/Bolt receiver RF protocol).
    Equad,
    /// Wired USB.
    Usb,
}

/// Mirror of hidpp's `DeviceTransport` bitfield — one bool per protocol the
/// device firmware exposes. The shape is dictated by HID++ feature 0x0003;
/// a state machine doesn't fit since a single device can announce multiple
/// transports simultaneously.
#[expect(
    clippy::struct_excessive_bools,
    reason = "bitfield mirroring HID++ DeviceInformation; transports are independent flags"
)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceTransports {
    /// Wired USB.
    pub usb: bool,
    /// Logitech eQuad — the Unifying/Bolt receiver RF protocol.
    pub equad: bool,
    /// Bluetooth Low Energy.
    pub btle: bool,
    /// Classic Bluetooth.
    pub bluetooth: bool,
}

/// One device in the agent's inventory snapshot: a receiver pairing slot,
/// or a direct (Bluetooth/wired) attachment under its synthetic
/// [`ReceiverInfo`]. Embedded in [`DeviceInventory`], so its field order is
/// IPC wire format — see that type's contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairedDevice {
    /// Receiver-assigned slot (1..=6 for Bolt).
    pub slot: u8,
    /// Firmware codename (e.g. `"MX Master 3S"`), when reported.
    pub codename: Option<String>,
    /// Wireless product ID. `None` for offline / unreachable devices on hidpp 0.2.
    pub wpid: Option<u16>,
    /// Best-guess classification. Identity only — panel gating uses
    /// [`Self::capabilities`] instead, so a misread kind can't hide panels
    /// (issue #127).
    pub kind: DeviceKind,
    /// Whether the device was reachable at enumeration time; offline devices
    /// keep their slot with reduced detail.
    pub online: bool,
    /// Last battery reading, `None` when offline or the device doesn't
    /// report battery.
    pub battery: Option<BatteryInfo>,
    /// Output of HID++ feature 0x0003 — populated for online devices that
    /// expose the feature. Drives asset-registry lookups in the GUI.
    pub model_info: Option<DeviceModelInfo>,
    /// Configuration capabilities derived from the device's HID++ feature
    /// table. `None` for devices we couldn't probe (offline / unreachable);
    /// the GUI then falls back to [`Capabilities::presumed_from_kind`].
    pub capabilities: Option<Capabilities>,
    /// Moves each time the device itself reports changing a setting — the
    /// user toggling Fn lock on the keyboard, or a DPI button changing the
    /// sensor — so a client re-reads that device's settings rather than keep
    /// showing what it read before. Opaque: only a difference from the
    /// previous snapshot of the same route means anything, and it restarts
    /// when the agent reopens the device's channel.
    ///
    /// Appended. `default` keeps device fixtures recorded before it loading;
    /// the IPC encoding is positional and always carries it.
    #[serde(default)]
    pub settings_revision: u64,
}

/// Address of a standalone raw-HID interface.
///
/// The identity is an opaque transport-generated string. It is deliberately
/// kept separate from the HID++ receiver/slot address so a raw device cannot
/// accidentally enter the HID++ `Direct` path.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RawDeviceAddress {
    /// HID vendor ID.
    pub vendor_id: u16,
    /// HID product ID.
    pub product_id: u16,
    /// HID usage page.
    pub usage_page: u16,
    /// HID usage ID.
    pub usage_id: u16,
    /// Identity chosen by the transport: a serial when available, otherwise
    /// an explicitly transient OS-node identity. It is never an enumeration
    /// index and a transient value is not persisted as a physical key.
    pub identity: String,
}

/// A standalone device that is not a HID++ receiver pairing slot.
///
/// This is the inventory bridge for Litra and future non-HID++ categories.
/// Receiver-backed devices continue to use [`PairedDevice`] inside
/// [`DeviceInventory`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StandaloneDevice {
    /// Raw HID address used to re-find the interface.
    pub address: RawDeviceAddress,
    /// Human-readable name supplied by the OS/HID descriptor.
    pub display_name: String,
    /// Human-readable manufacturer, when available.
    pub manufacturer: Option<String>,
    /// Device serial, when the HID backend exposes one.
    pub serial_number: Option<String>,
    /// Stable four-byte identity when the protocol/driver provides one.
    /// Raw HID drivers may use zeroes when no such field exists.
    pub unit_id: [u8; 4],
    /// Identity classification. Capability fields gate controls.
    pub kind: DeviceKind,
    /// Whether this interface was present in the latest completed scan.
    pub online: bool,
    /// HID++ capabilities are absent for a non-HID++ device.
    pub capabilities: Option<Capabilities>,
    /// Standalone capability descriptor, if the selected driver recognizes it.
    pub light_capabilities: Option<LightCapabilities>,
    /// Stable identifier of the driver family that owns this raw interface.
    /// This is deliberately separate from the product ID so a future family
    /// can share a protocol driver across several product variants.
    pub driver_id: String,
    /// Optional model-level identity in the OpenLogi asset registry.
    ///
    /// This is deliberately appended: `StandaloneDevice` crosses the
    /// append-only GUI↔agent bincode wire format.
    #[serde(default)]
    pub registry_model_id: Option<String>,
}

impl StandaloneDevice {
    /// The route that reaches this device.
    #[must_use]
    pub fn route(&self) -> DeviceRoute {
        DeviceRoute::from(&self.address)
    }
}

/// One receiver and its paired devices — the unit the agent's inventory
/// snapshot is made of.
///
/// Crosses the agent↔GUI IPC (everything it embeds too: [`ReceiverInfo`],
/// [`PairedDevice`], battery/model-info/capability types). bincode encodes
/// field and variant *order*, so reordering, retyping, or wrapping any field
/// in this tree is a wire-format change and requires a `PROTOCOL_VERSION`
/// bump (guarded by `openlogi-ipc/tests/wire_format.rs`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceInventory {
    /// The receiver's identity — synthetic (mirroring the device itself)
    /// for a direct Bluetooth/wired attachment.
    pub receiver: ReceiverInfo,
    /// The devices reached through this receiver; a direct attachment
    /// carries exactly one entry.
    pub paired: Vec<PairedDevice>,
}

#[cfg(test)]
mod tests {
    use super::{
        BatteryDevice, BatteryInfo, BatteryLevel, BatteryReading, BatteryStatus, BatteryTone,
        BatteryValue, Capabilities, DeviceInventory, DeviceKind, DeviceModelInfo, DeviceTransports,
        LightValueRange, LightValueUnit, LockKey, ModelTransport, PairedDevice, ReceiverInfo,
        battery_devices, first_battery_readout, flow_indicator_devices, prettify_codename,
    };

    #[test]
    fn a_low_discharging_battery_needs_attention() {
        let battery = |percentage, level, status| BatteryInfo {
            percentage,
            level,
            status,
        };
        let good = BatteryLevel::Good;
        let discharging = BatteryStatus::Discharging;

        assert!(battery(Some(20), good, discharging).needs_attention());
        assert!(battery(Some(0), good, discharging).needs_attention());
        assert!(!battery(Some(21), BatteryLevel::Low, discharging).needs_attention());
        // A percentage decides on its own; the level only stands in for one.
        assert!(!battery(None, good, discharging).needs_attention());
        assert!(battery(None, BatteryLevel::Low, discharging).needs_attention());
        assert!(battery(None, BatteryLevel::Critical, discharging).needs_attention());
        assert!(!battery(None, BatteryLevel::Unknown, discharging).needs_attention());
        assert!(battery(Some(10), good, BatteryStatus::Unknown).needs_attention());
        assert!(battery(Some(10), good, BatteryStatus::Error).needs_attention());
        for powered in [
            BatteryStatus::Charging,
            BatteryStatus::ChargingSlow,
            BatteryStatus::Full,
        ] {
            assert!(
                !battery(Some(5), BatteryLevel::Critical, powered).needs_attention(),
                "{powered:?} is already being charged"
            );
            assert!(!battery(None, BatteryLevel::Critical, powered).needs_attention());
        }
    }

    fn reading_battery(
        percentage: Option<u8>,
        level: BatteryLevel,
        status: BatteryStatus,
    ) -> BatteryInfo {
        BatteryInfo {
            percentage,
            level,
            status,
        }
    }

    #[test]
    fn battery_reading_prefers_a_percentage_and_ignores_a_charging_cold_start() {
        use BatteryReading::{Level, NoReading, Percent};

        let cases = [
            (
                Some(0),
                BatteryLevel::Good,
                BatteryStatus::Charging,
                NoReading,
            ),
            (
                Some(0),
                BatteryLevel::Critical,
                BatteryStatus::ChargingSlow,
                NoReading,
            ),
            (
                Some(0),
                BatteryLevel::Good,
                BatteryStatus::Discharging,
                Percent(0),
            ),
            (Some(0), BatteryLevel::Good, BatteryStatus::Full, Percent(0)),
            (
                Some(64),
                BatteryLevel::Good,
                BatteryStatus::Discharging,
                Percent(64),
            ),
            (
                Some(100),
                BatteryLevel::Good,
                BatteryStatus::Unknown,
                Percent(100),
            ),
            // A charging fault does not erase a percentage the firmware
            // still reports; the error is the tone's job, not the reading's.
            (
                Some(80),
                BatteryLevel::Good,
                BatteryStatus::Error,
                Percent(80),
            ),
            (
                None,
                BatteryLevel::Full,
                BatteryStatus::Full,
                Level(BatteryLevel::Full),
            ),
            (
                None,
                BatteryLevel::Good,
                BatteryStatus::Discharging,
                Level(BatteryLevel::Good),
            ),
            (
                None,
                BatteryLevel::Low,
                BatteryStatus::Charging,
                Level(BatteryLevel::Low),
            ),
            (
                None,
                BatteryLevel::Critical,
                BatteryStatus::Error,
                Level(BatteryLevel::Critical),
            ),
            (
                None,
                BatteryLevel::Unknown,
                BatteryStatus::Charging,
                NoReading,
            ),
            (
                None,
                BatteryLevel::Unknown,
                BatteryStatus::Unknown,
                NoReading,
            ),
        ];
        for (percentage, level, status, expected) in cases {
            let battery = reading_battery(percentage, level, status);
            assert_eq!(battery.reading(), expected, "{battery:?}");
        }
    }

    #[test]
    fn battery_value_names_the_word_a_surface_should_show() {
        use BatteryValue::{Charging, Level, None as NoValue, Percent};

        let good = BatteryLevel::Good;
        assert_eq!(
            reading_battery(Some(64), good, BatteryStatus::Discharging).value(),
            Percent(64)
        );
        assert_eq!(
            reading_battery(None, BatteryLevel::Low, BatteryStatus::Discharging).value(),
            Level(BatteryLevel::Low)
        );
        assert_eq!(
            reading_battery(None, good, BatteryStatus::Charging).value(),
            Level(BatteryLevel::Good)
        );
        assert_eq!(
            reading_battery(Some(0), good, BatteryStatus::Charging).value(),
            Charging
        );
        assert_eq!(
            reading_battery(None, BatteryLevel::Unknown, BatteryStatus::Unknown).value(),
            NoValue
        );
        assert_eq!(
            BatteryValue::Level(BatteryLevel::Critical).translation_key(),
            Some("device.low_battery")
        );
        assert_eq!(
            BatteryValue::Charging.translation_key(),
            Some("device.charging")
        );
        assert_eq!(BatteryValue::Percent(50).translation_key(), None);
    }

    #[test]
    fn battery_tone_is_decided_by_status_then_charge() {
        use BatteryTone::{Charging, Critical, Error, Full, Low, Normal};

        let cases = [
            (
                Some(5),
                BatteryLevel::Critical,
                BatteryStatus::Charging,
                Charging,
            ),
            (
                Some(0),
                BatteryLevel::Good,
                BatteryStatus::Charging,
                Charging,
            ),
            (
                Some(80),
                BatteryLevel::Good,
                BatteryStatus::ChargingSlow,
                Charging,
            ),
            (Some(80), BatteryLevel::Good, BatteryStatus::Full, Full),
            (Some(5), BatteryLevel::Critical, BatteryStatus::Full, Full),
            (Some(80), BatteryLevel::Good, BatteryStatus::Error, Error),
            (None, BatteryLevel::Unknown, BatteryStatus::Error, Error),
            (
                Some(8),
                BatteryLevel::Critical,
                BatteryStatus::Discharging,
                Critical,
            ),
            (
                None,
                BatteryLevel::Critical,
                BatteryStatus::Discharging,
                Critical,
            ),
            (Some(5), BatteryLevel::Good, BatteryStatus::Discharging, Low),
            (Some(20), BatteryLevel::Low, BatteryStatus::Discharging, Low),
            (
                Some(21),
                BatteryLevel::Low,
                BatteryStatus::Discharging,
                Normal,
            ),
            (None, BatteryLevel::Low, BatteryStatus::Discharging, Low),
            (None, BatteryLevel::Good, BatteryStatus::Discharging, Normal),
            (
                None,
                BatteryLevel::Unknown,
                BatteryStatus::Discharging,
                Normal,
            ),
            (Some(10), BatteryLevel::Good, BatteryStatus::Unknown, Low),
            (Some(21), BatteryLevel::Good, BatteryStatus::Unknown, Normal),
        ];
        for (percentage, level, status, expected) in cases {
            let battery = reading_battery(percentage, level, status);
            assert_eq!(battery.tone(), expected, "{battery:?}");
        }
    }

    #[test]
    fn battery_bar_percentage_fills_a_level_inside_its_band() {
        let good = BatteryLevel::Good;
        let discharging = BatteryStatus::Discharging;
        let bar =
            |percentage, level| reading_battery(percentage, level, discharging).bar_percentage();

        assert_eq!(bar(Some(42), good), Some(42));
        assert_eq!(bar(Some(0), good), Some(0));
        assert_eq!(bar(None, BatteryLevel::Critical), Some(10));
        assert_eq!(bar(None, BatteryLevel::Low), Some(25));
        assert_eq!(bar(None, BatteryLevel::Good), Some(70));
        assert_eq!(bar(None, BatteryLevel::Full), Some(100));
        assert_eq!(bar(None, BatteryLevel::Unknown), None);
        assert_eq!(
            reading_battery(Some(0), good, BatteryStatus::Charging).bar_percentage(),
            None,
            "a cold-start 0% is not an empty bar"
        );
    }

    #[test]
    fn battery_charging_without_reading_only_describes_a_cold_start() {
        let good = BatteryLevel::Good;

        assert!(reading_battery(Some(0), good, BatteryStatus::Charging).charging_without_reading());
        assert!(
            reading_battery(Some(0), good, BatteryStatus::ChargingSlow).charging_without_reading()
        );
        assert!(
            reading_battery(None, BatteryLevel::Unknown, BatteryStatus::Charging)
                .charging_without_reading()
        );
        assert!(
            !reading_battery(Some(40), good, BatteryStatus::Charging).charging_without_reading()
        );
        assert!(
            !reading_battery(Some(0), good, BatteryStatus::Discharging).charging_without_reading()
        );
        // A level-only device keeps its level to show.
        assert!(!reading_battery(None, good, BatteryStatus::Charging).charging_without_reading());
        // Full is charge complete, not a charging cold start.
        assert!(
            !reading_battery(None, BatteryLevel::Unknown, BatteryStatus::Full)
                .charging_without_reading()
        );
    }

    fn display_device(
        name: Option<&str>,
        online: bool,
        battery: Option<BatteryInfo>,
    ) -> BatteryDevice {
        BatteryDevice {
            name: name.map(str::to_owned),
            kind: DeviceKind::Mouse,
            online,
            battery,
        }
    }

    #[test]
    fn battery_devices_flatten_in_inventory_order_and_prettify_names() {
        let inventory = DeviceInventory {
            receiver: ReceiverInfo {
                name: "Logi Bolt Receiver".to_string(),
                vendor_id: 0x046d,
                product_id: 0xc548,
                unique_id: None,
            },
            paired: vec![
                PairedDevice {
                    slot: 1,
                    codename: Some("MX MASTER 3S".to_string()),
                    wpid: None,
                    kind: DeviceKind::Mouse,
                    online: true,
                    battery: None,
                    model_info: None,
                    capabilities: None,
                    settings_revision: 0,
                },
                PairedDevice {
                    slot: 2,
                    codename: None,
                    wpid: None,
                    kind: DeviceKind::Keyboard,
                    online: false,
                    battery: None,
                    model_info: None,
                    capabilities: None,
                    settings_revision: 0,
                },
            ],
        };

        let devices = battery_devices(&[inventory]);
        assert_eq!(devices[0].name.as_deref(), Some("MX Master 3S"));
        assert_eq!(devices[1].name, None);
        assert_eq!(devices[1].kind, DeviceKind::Keyboard);
    }

    #[test]
    fn the_first_battery_readout_is_the_first_online_device_with_something_to_show() {
        let devices = vec![
            display_device(
                Some("Offline"),
                false,
                Some(reading_battery(
                    Some(90),
                    BatteryLevel::Good,
                    BatteryStatus::Discharging,
                )),
            ),
            display_device(Some("No Battery"), true, None),
            display_device(
                Some("Unread"),
                true,
                Some(reading_battery(
                    None,
                    BatteryLevel::Unknown,
                    BatteryStatus::Unknown,
                )),
            ),
            display_device(
                Some("Chosen"),
                true,
                Some(reading_battery(
                    Some(64),
                    BatteryLevel::Good,
                    BatteryStatus::Discharging,
                )),
            ),
        ];

        assert_eq!(
            first_battery_readout(&devices).and_then(|device| device.name.as_deref()),
            Some("Chosen")
        );
    }

    #[test]
    fn a_charging_cold_start_is_a_battery_readout() {
        let devices = [display_device(
            Some("Charging"),
            true,
            Some(reading_battery(
                Some(0),
                BatteryLevel::Good,
                BatteryStatus::Charging,
            )),
        )];

        assert_eq!(
            first_battery_readout(&devices).and_then(|device| device.name.as_deref()),
            Some("Charging")
        );
    }

    #[test]
    fn flow_indicator_rows_are_low_first_capped_and_filter_offline() {
        let devices = vec![
            display_device(
                Some("Nearly Empty"),
                true,
                Some(reading_battery(
                    Some(5),
                    BatteryLevel::Critical,
                    BatteryStatus::Discharging,
                )),
            ),
            display_device(
                Some("Offline Low"),
                false,
                Some(reading_battery(
                    Some(15),
                    BatteryLevel::Low,
                    BatteryStatus::Discharging,
                )),
            ),
            display_device(
                Some("Healthy"),
                true,
                Some(reading_battery(
                    Some(87),
                    BatteryLevel::Good,
                    BatteryStatus::Discharging,
                )),
            ),
            display_device(
                Some("Charging"),
                true,
                Some(reading_battery(
                    Some(50),
                    BatteryLevel::Good,
                    BatteryStatus::Charging,
                )),
            ),
            display_device(Some("No Battery"), true, None),
            display_device(Some("Offline Empty"), false, None),
        ];

        let names = |rows: Vec<&BatteryDevice>| {
            rows.into_iter()
                .filter_map(|device| device.name.clone())
                .collect::<Vec<_>>()
        };

        assert_eq!(
            names(flow_indicator_devices(&devices, true, 4)),
            ["Nearly Empty", "Offline Low", "Healthy", "Charging"]
        );
        assert_eq!(
            names(flow_indicator_devices(&devices, false, 4)),
            ["Nearly Empty", "Healthy", "Charging"]
        );
        assert_eq!(
            names(flow_indicator_devices(&devices, true, 2)),
            ["Nearly Empty", "Offline Low"]
        );
    }

    #[test]
    fn flow_indicator_rows_break_ties_by_name() {
        let devices = vec![
            display_device(
                Some("Zeta"),
                true,
                Some(reading_battery(
                    Some(10),
                    BatteryLevel::Low,
                    BatteryStatus::Discharging,
                )),
            ),
            display_device(
                Some("Alpha"),
                true,
                Some(reading_battery(
                    Some(10),
                    BatteryLevel::Low,
                    BatteryStatus::Discharging,
                )),
            ),
        ];

        assert_eq!(
            flow_indicator_devices(&devices, true, 5)
                .into_iter()
                .filter_map(|device| device.name.as_deref())
                .collect::<Vec<_>>(),
            ["Alpha", "Zeta"]
        );
    }

    #[test]
    fn prettify_codename_title_cases_shouted_names_only() {
        assert_eq!(
            prettify_codename("G513 RGB MECHANICAL GAMING KEYBOARD"),
            "G513 RGB Mechanical Gaming Keyboard"
        );
        assert_eq!(prettify_codename("PRO X  SUPERLIGHT"), "PRO X Superlight");
        assert_eq!(prettify_codename("MX Master 3S"), "MX Master 3S");
        assert_eq!(prettify_codename(""), "");
    }

    fn inventory(slot: u8, wpid: Option<u16>, battery_percentage: u8) -> DeviceInventory {
        DeviceInventory {
            receiver: ReceiverInfo {
                name: "Logi Bolt Receiver".to_string(),
                vendor_id: 0x046d,
                product_id: 0xc548,
                unique_id: Some("receiver-1".to_string()),
            },
            paired: vec![PairedDevice {
                slot,
                codename: Some("MX Test".to_string()),
                wpid,
                kind: DeviceKind::Mouse,
                online: true,
                battery: Some(BatteryInfo {
                    percentage: Some(battery_percentage),
                    level: BatteryLevel::Good,
                    status: BatteryStatus::Discharging,
                }),
                model_info: Some(DeviceModelInfo {
                    entity_count: 1,
                    serial_number: Some("serial-1".to_string()),
                    unit_id: [1, 2, 3, 4],
                    transports: DeviceTransports {
                        usb: true,
                        equad: true,
                        btle: false,
                        bluetooth: false,
                    },
                    model_ids: [0xb023, 0, 0],
                    extended_model_id: 0x02,
                }),
                capabilities: Some(Capabilities {
                    buttons: true,
                    pointer: true,
                    lighting: false,
                    scroll_inversion: false,
                    hires_wheel: false,
                    thumbwheel: false,
                    haptic_feedback: false,
                    haptic_panel: false,
                    dpi_gestures: false,
                    fn_lock: false,
                    os_layout: false,
                    report_rate: false,
                    backlight: false,
                    easy_switch: false,
                    virtual_thumbwheel: false,
                    pointer_scaling: false,
                    force_sensing: false,
                    disable_keys: false,
                }),
                settings_revision: 0,
            }],
        }
    }

    #[test]
    fn device_inventory_equality_includes_nested_device_fields() {
        let base = inventory(1, Some(0xb023), 86);
        assert_eq!(base, base.clone());

        assert_ne!(
            base,
            inventory(2, Some(0xb023), 86),
            "slot changes must affect inventory equality"
        );
        assert_ne!(
            base,
            inventory(1, Some(0xb024), 86),
            "wireless product id changes must affect inventory equality"
        );
        assert_ne!(
            base,
            inventory(1, Some(0xb023), 87),
            "nested battery changes must affect inventory equality"
        );
    }

    #[test]
    fn transport_for_product_id_resolves_the_live_slot() {
        // G915 X LS from issue #1218: usb + equad + btle all supported,
        // model_ids packed BTLE/eQuad/USB per the ascending-bit-order
        // contract documented on `DeviceModelInfo::model_ids`.
        let model = DeviceModelInfo {
            entity_count: 3,
            serial_number: None,
            unit_id: [0, 0, 0, 0],
            transports: DeviceTransports {
                usb: true,
                equad: true,
                btle: true,
                bluetooth: false,
            },
            model_ids: [0xb38a, 0x40b5, 0xc356],
            extended_model_id: 0x01,
        };

        assert_eq!(
            model.transport_for_product_id(0xc356),
            Some(ModelTransport::Usb),
            "the USB-slot pid must resolve to Usb even though the device \
             also supports BTLE"
        );
        assert_eq!(
            model.transport_for_product_id(0xb38a),
            Some(ModelTransport::Btle)
        );
        assert_eq!(
            model.transport_for_product_id(0x40b5),
            Some(ModelTransport::Equad)
        );
        assert_eq!(
            model.transport_for_product_id(0xdead),
            None,
            "an unrecognized pid must not silently match a slot"
        );
    }

    #[test]
    fn registry_type_is_case_folded() {
        // The registry ships both `"mouse"` and `"MOUSE"`; both must resolve so
        // the asset cross-check can't silently miss a depot.
        assert_eq!(
            DeviceKind::from_registry_type("mouse"),
            Some(DeviceKind::Mouse)
        );
        assert_eq!(
            DeviceKind::from_registry_type("MOUSE"),
            Some(DeviceKind::Mouse)
        );
        assert_eq!(
            DeviceKind::from_registry_type("  Keyboard "),
            Some(DeviceKind::Keyboard)
        );
    }

    #[test]
    fn unknown_registry_type_defers_to_the_caller() {
        // Unmodelled / empty → no asset opinion.
        assert_eq!(DeviceKind::from_registry_type("webcam"), None);
        assert_eq!(DeviceKind::from_registry_type(""), None);
    }

    #[test]
    fn capabilities_track_the_driving_feature_ids() {
        use super::Capabilities;
        // A typical MX mouse: ReprogControls (0x1b04) + ExtendedAdjustableDpi
        // (0x2202), no lighting.
        let mouse =
            Capabilities::from_feature_ids(&[0x0003, 0x1b04, 0x2121, 0x2150, 0x2202, 0x2110]);
        assert_eq!(
            mouse,
            Capabilities {
                buttons: true,
                pointer: true,
                lighting: false,
                scroll_inversion: false,
                hires_wheel: true,
                thumbwheel: true,
                haptic_feedback: false,
                haptic_panel: false,
                dpi_gestures: false,
                fn_lock: false,
                os_layout: false,
                report_rate: false,
                backlight: false,
                easy_switch: false,
                virtual_thumbwheel: false,
                pointer_scaling: false,
                force_sensing: false,
                disable_keys: false,
            }
        );
        assert!(!Capabilities::from_feature_ids(&[0x0003, 0x1b04]).thumbwheel);
        // A wired G-series keyboard: PerKeyLighting (0x8080), no DPI/buttons.
        let keyboard = Capabilities::from_feature_ids(&[0x0001, 0x8080]);
        assert_eq!(
            keyboard,
            Capabilities {
                buttons: false,
                pointer: false,
                lighting: true,
                scroll_inversion: false,
                hires_wheel: false,
                thumbwheel: false,
                haptic_feedback: false,
                haptic_panel: false,
                dpi_gestures: false,
                fn_lock: false,
                os_layout: false,
                report_rate: false,
                backlight: false,
                easy_switch: false,
                virtual_thumbwheel: false,
                pointer_scaling: false,
                force_sensing: false,
                disable_keys: false,
            }
        );
        // No driving features → nothing offered.
        assert_eq!(
            Capabilities::from_feature_ids(&[0x0000, 0x0003]),
            Capabilities::default()
        );
        // 0x4531 offers the OS layout until the probe reads it cannot be set.
        assert!(Capabilities::from_feature_ids(&[0x0001, 0x4531]).os_layout);
        assert!(!Capabilities::from_feature_ids(&[0x0001, 0x40a3]).os_layout);
        // Either report-rate feature offers the report rate.
        assert!(Capabilities::from_feature_ids(&[0x0001, 0x8060]).report_rate);
        assert!(Capabilities::from_feature_ids(&[0x0001, 0x8061]).report_rate);
        assert!(!Capabilities::from_feature_ids(&[0x0001, 0x2202]).report_rate);
        // Only the 0x1982 backlight is driven; the older 0x1981 is not.
        assert!(Capabilities::from_feature_ids(&[0x0001, 0x1982]).backlight);
        assert!(!Capabilities::from_feature_ids(&[0x0001, 0x1981]).backlight);
        // 0x1815 HostsInfo offers the Easy-Switch hosts; 0x1814 ChangeHost
        // alone switches channels but cannot name them.
        assert!(Capabilities::from_feature_ids(&[0x0001, 0x1815]).easy_switch);
        assert!(!Capabilities::from_feature_ids(&[0x0001, 0x1814]).easy_switch);
        // 0x2205 sets the speed only where no DPI feature does (M590, MX
        // Ergo); a mouse with both keeps its DPI control.
        let scaled = Capabilities::from_feature_ids(&[0x0001, 0x2205]);
        assert!(scaled.pointer_scaling && !scaled.pointer);
        let both = Capabilities::from_feature_ids(&[0x0001, 0x2201, 0x2205]);
        assert!(both.pointer && !both.pointer_scaling);
        // 0x19c0 offers the press force until the probe reads it cannot be
        // changed; the 0x19b0 haptic engine alone does not.
        assert!(Capabilities::from_feature_ids(&[0x0001, 0x19c0]).force_sensing);
        assert!(!Capabilities::from_feature_ids(&[0x0001, 0x19b0]).force_sensing);
    }

    #[test]
    fn every_drivable_lighting_family_earns_the_tab() {
        // `set_keyboard_color` walks 0x8070 → 0x8071 → 0x8081 → 0x8080.
        for id in [0x8070, 0x8071, 0x8080, 0x8081] {
            assert!(
                Capabilities::from_feature_ids(&[0x0001, id]).lighting,
                "0x{id:04x} must offer the lighting tab"
            );
        }
        // Backlight (0x198x) stays out — the panel cannot drive it.
        assert!(!Capabilities::from_feature_ids(&[0x0001, 0x1982]).lighting);
        // Any fn-inversion feature gives the keyboard an Fn-lock control.
        assert!(Capabilities::from_feature_ids(&[0x0001, 0x40a3]).fn_lock);
        assert!(Capabilities::from_feature_ids(&[0x0001, 0x40a2]).fn_lock);
        assert!(Capabilities::from_feature_ids(&[0x0001, 0x40a0]).fn_lock);
        assert!(!Capabilities::from_feature_ids(&[0x0001, 0x1982]).fn_lock);
    }

    #[test]
    fn only_measured_disable_keys_feature_enables_the_capability() {
        assert!(Capabilities::from_feature_ids(&[0x4521]).disable_keys);
        assert!(!Capabilities::from_feature_ids(&[0x4522]).disable_keys);
        assert!(!Capabilities::presumed_from_kind(DeviceKind::Keyboard).disable_keys);
    }

    #[test]
    fn persisted_capabilities_without_appended_wheel_fields_load_as_unsupported()
    -> Result<(), toml::de::Error> {
        use super::Capabilities;

        let capabilities: Capabilities = toml::from_str(
            r"
                buttons = true
                pointer = true
                lighting = false
                scroll_inversion = true
            ",
        )?;

        assert!(!capabilities.hires_wheel);
        assert!(!capabilities.thumbwheel);
        assert!(capabilities.scroll_inversion);
        Ok(())
    }

    #[test]
    fn presumed_capabilities_keep_an_unprobed_mouse_configurable() {
        use super::Capabilities;
        let mouse = Capabilities::presumed_from_kind(DeviceKind::Mouse);
        assert!(mouse.buttons && mouse.pointer && !mouse.lighting);
        assert!(!mouse.thumbwheel);
        assert!(Capabilities::presumed_from_kind(DeviceKind::Keyboard).lighting);
        // An unidentified device presumes nothing — it must be measured.
        assert_eq!(
            Capabilities::presumed_from_kind(DeviceKind::Unknown),
            Capabilities::default()
        );
    }

    #[test]
    fn light_ranges_reject_invalid_grids_and_units() {
        LightValueRange::new(10, 1, 1, LightValueUnit::Lumens)
            .expect_err("a minimum above the maximum must be rejected");
        LightValueRange::new(0, 10, 0, LightValueUnit::Lumens)
            .expect_err("a zero step must be rejected");
        LightValueRange::new(0, 10, 3, LightValueUnit::Lumens)
            .expect_err("a step that does not divide the span must be rejected");
        LightValueRange::new(0, 101, 1, LightValueUnit::Percent)
            .expect_err("a percent range above 100 must be rejected");
    }

    #[test]
    fn light_ranges_quantize_without_leaving_the_advertised_grid() {
        let range = LightValueRange::new(20, 250, 10, LightValueUnit::Lumens).expect("valid range");
        assert_eq!(range.native_for_percent(0), Some(20));
        assert_eq!(range.native_for_percent(50), Some(140));
        assert_eq!(range.native_for_percent(100), Some(250));
        assert_eq!(range.quantize(249), 250);
        assert!(range.contains(range.native_for_percent(65).expect("mapped value")));
    }

    #[test]
    fn invalid_light_ranges_fail_toml_deserialization() {
        let result = toml::from_str::<LightValueRange>(
            "min = 2700\nmax = 6500\nstep = 0\nunit = 'kelvin'\n",
        );
        result.expect_err("a zero step must not survive deserialization");
    }

    #[test]
    fn caps_lock_shows_everywhere_and_num_and_scroll_lock_not_on_macos() {
        assert!(LockKey::CapsLock.is_indicated_here());
        for key in [LockKey::NumLock, LockKey::ScrollLock] {
            assert_eq!(
                key.is_indicated_here(),
                !cfg!(target_os = "macos"),
                "{key:?}"
            );
        }
    }
}
