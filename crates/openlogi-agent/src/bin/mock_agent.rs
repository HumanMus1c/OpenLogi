//! Standalone hardware-free mock agent for GUI development: serves the
//! scripted demo inventory over the real agent IPC contract. See
//! `openlogi_agent::mock` for the served behavior and for the demo mode that
//! debug builds of `openlogi-agent` run instead of real hardware.
//!
//! Unlike the demo mode, this binary defaults itself to the `openlogi-dev`
//! profile — as if `OPENLOGI_PROFILE=dev` were set — so it meets the macOS dev
//! app bundle's GUI on the dev socket while an installed production build,
//! which is on the production profile, keeps running untouched.

use std::process::ExitCode;

fn main() -> ExitCode {
    openlogi_agent::mock::default_to_dev_profile();
    openlogi_agent::mock::main_from_args(std::env::args_os().skip(1))
}
