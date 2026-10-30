//! The environment variables OpenLogi reads at runtime.
//!
//! Named here once because more than one place reads them — the agent, the
//! GUI, the overlay, the CLI, and the mock agent — and a knob spelled
//! differently in one of them is a knob that silently does nothing there.
//! Build-time knobs (`option_env!` in the updater) and the packaging overrides
//! `xtask` reads belong to the one place that reads them.

/// Forces the profile a process runs under, `dev` or `prod`; see
/// [`Profile`](crate::paths::Profile) for what that decides. Unset, the
/// profile is detected from the bundle the executable lives in.
pub const PROFILE: &str = "OPENLOGI_PROFILE";

/// The `tracing` filter every process installs, in `EnvFilter` directive
/// syntax (`hidpp=trace`, `openlogi_agent=debug,info`, …).
pub const LOG: &str = "OPENLOGI_LOG";

/// What every process logs when [`LOG`] is unset.
pub const LOG_DEFAULT: &str = "info";

/// An asset mirror that overrides the configured source, for the GUI's asset
/// sync and the CLI's `assets sync`.
pub const ASSETS: &str = "OPENLOGI_ASSETS";

/// Pins the GUI's device renders to one depot regardless of what is attached
/// — a development convenience read in two places.
pub const FORCE_DEPOT: &str = "OPENLOGI_FORCE_DEPOT";

/// Selects the device source at runtime: `1/true/yes/on` serves the scripted
/// demo inventory, `0/false/no/off` drives real HID++ hardware. Unset, a WSL
/// guest serves the demo (it exposes no HID devices) and every other host
/// drives hardware. The agent's and GUI's `--mock` launch flags are the
/// user-facing form of `1` and win over this variable.
pub const MOCK_DEVICES: &str = "OPENLOGI_MOCK_DEVICES";
