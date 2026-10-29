//! Shared library of the `openlogi-agent` package.
//!
//! The package's binaries — the always-on agent and the dev-only
//! `openlogi-agent-mock` — are thin shells over this library. It currently
//! owns the scripted demo devices ([`mock`]), which a debug build of
//! `openlogi-agent` serves in place of real hardware and the mock binary
//! serves standalone.

pub mod mock;
