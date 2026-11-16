//! Shared library of the `openlogi-agent` package.
//!
//! The agent and its hardware-free mock both need the Flow Indicator's
//! agent-owned pinned state; keeping it here gives both binaries one owner.

pub mod flow_pin;
