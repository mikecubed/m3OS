//! Phase 103 — pure-logic power management (host-tested).
//!
//! The `kernel-core` half of the laptop-power phase: decode of
//! evaluated ACPI battery/AC objects ([`battery`]) and the
//! `powerd` ↔ client IPC codec ([`control`]). Thermal decode and the
//! cpufreq governor land in later slices. Consumers: `powerd` (ring 3;
//! evaluation itself rides acpid's IPC per the Phase 101 split) and
//! `m3ctl`.

pub mod battery;
pub mod control;
