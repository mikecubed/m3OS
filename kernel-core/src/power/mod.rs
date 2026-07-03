//! Phase 103 — pure-logic power management (host-tested).
//!
//! The `kernel-core` half of the laptop-power phase: decode of
//! evaluated ACPI battery/AC objects ([`battery`]), the
//! `powerd` ↔ client IPC codec ([`control`]), thermal decode + trip
//! classification ([`thermal`]), the cpufreq governor state machine
//! ([`governor`]), and the `0x116x` power syscall ABI ([`syscalls`]).
//! Consumers: `powerd` (ring 3; ACPI evaluation rides acpid's IPC per
//! the Phase 101 split, governor policy ticks in `powerd` per the
//! userspace-first rule), the kernel dispatcher, and `m3ctl`.

pub mod battery;
pub mod control;
pub mod governor;
pub mod syscalls;
pub mod thermal;
