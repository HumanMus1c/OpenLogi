//! Which device source a process serves at runtime.
//!
//! The build tree no longer decides: a `--mock` launch serves the scripted
//! demo inventory, and everything else drives real HID++ hardware. The
//! environment variable [`crate::env::MOCK_DEVICES`] is the scriptable form of
//! the same switch.
//!
//! A WSL guest defaults to the demo inventory when neither selects a source:
//! WSL exposes no USB HID devices unless a receiver is passed through, so a
//! real agent there has nothing to serve and the app would open empty. Set
//! [`crate::env::MOCK_DEVICES`] to `0` to drive real hardware in WSL (for a
//! passthrough receiver); every other host defaults to hardware.

use crate::env;

/// The suffix a mock agent appends to its version, so no surface can mistake
/// a demo session for a live one.
pub const MOCK_VERSION_SUFFIX: &str = "-mock";

/// Where a process's device inventory comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceSource {
    /// Real HID++ devices, enumerated and opened by the agent.
    Hardware,
    /// The scripted demo inventory, served without any HID I/O.
    Mock,
}

impl DeviceSource {
    /// The source this process was asked to serve: an explicit `--mock` flag,
    /// [`crate::env::MOCK_DEVICES`], or the host default (the demo in WSL,
    /// hardware elsewhere).
    ///
    /// An explicit flag wins over the variable; the variable wins over the
    /// host default. An unparseable variable counts as unset.
    #[must_use]
    pub fn requested(mock_flag: bool) -> Self {
        let knob = std::env::var(env::MOCK_DEVICES).ok();
        Self::resolve(mock_flag, knob.as_deref(), running_in_wsl())
    }

    /// Whether this source serves the scripted demo inventory.
    #[must_use]
    pub const fn is_mock(self) -> bool {
        matches!(self, Self::Mock)
    }

    /// [`Self::requested`] without the process environment, for tests.
    fn resolve(mock_flag: bool, knob: Option<&str>, wsl: bool) -> Self {
        if mock_flag {
            return Self::Mock;
        }
        match knob.and_then(parse_knob) {
            Some(true) => Self::Mock,
            None if wsl => Self::Mock,
            Some(false) | None => Self::Hardware,
        }
    }
}

/// The knob's accepted spellings: `1/true/yes/on` and `0/false/no/off`, case-
/// insensitively. `None` means "not a boolean" and falls through to the host
/// default.
fn parse_knob(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

/// Whether this process runs inside a WSL guest, judged from the Linux kernel
/// release (`…-microsoft-standard-WSL2`), which launchd-style environment
/// stripping cannot change.
fn running_in_wsl() -> bool {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string("/proc/sys/kernel/osrelease")
            .is_ok_and(|release| is_wsl_kernel(&release))
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

#[cfg(target_os = "linux")]
fn is_wsl_kernel(release: &str) -> bool {
    release.to_ascii_lowercase().contains("microsoft")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_explicit_flag_wins() {
        assert_eq!(DeviceSource::resolve(true, None, false), DeviceSource::Mock);
        assert_eq!(
            DeviceSource::resolve(true, Some("0"), false),
            DeviceSource::Mock
        );
    }

    #[test]
    fn the_knob_forces_either_source() {
        assert_eq!(
            DeviceSource::resolve(false, Some("1"), false),
            DeviceSource::Mock
        );
        assert_eq!(
            DeviceSource::resolve(false, Some("0"), true),
            DeviceSource::Hardware
        );
        assert_eq!(
            DeviceSource::resolve(false, Some("off"), false),
            DeviceSource::Hardware
        );
    }

    #[test]
    fn an_unparseable_knob_falls_through_to_the_host_default() {
        assert_eq!(
            DeviceSource::resolve(false, Some("demo"), false),
            DeviceSource::Hardware
        );
        assert_eq!(
            DeviceSource::resolve(false, Some("demo"), true),
            DeviceSource::Mock
        );
    }

    #[test]
    fn wsl_defaults_to_the_demo_and_other_hosts_to_hardware() {
        assert_eq!(DeviceSource::resolve(false, None, true), DeviceSource::Mock);
        assert_eq!(
            DeviceSource::resolve(false, None, false),
            DeviceSource::Hardware
        );
    }

    #[test]
    fn the_knob_accepts_only_known_spellings() {
        assert_eq!(parse_knob("1"), Some(true));
        assert_eq!(parse_knob("on"), Some(true));
        assert_eq!(parse_knob("TRUE"), Some(true));
        assert_eq!(parse_knob(" 0 "), Some(false));
        assert_eq!(parse_knob("Off"), Some(false));
        assert_eq!(parse_knob(""), None);
        assert_eq!(parse_knob("demo"), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_wsl_kernel_is_recognised() {
        assert!(is_wsl_kernel("6.6.114.1-microsoft-standard-WSL2"));
        assert!(!is_wsl_kernel("6.8.0-45-generic"));
    }
}
