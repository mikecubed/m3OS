//! Phase 64 — `session_manager` library surface.
//!
//! The Phase 57 daemon was binary-only. Phase 64 introduces the
//! [`table`] and [`lifecycle`] modules and exposes them as a `[lib]`
//! target so their state machines are host-testable without QEMU.
//!
//! ## `#![no_std]` discipline
//!
//! Production builds (the `[[bin]]` target and host-test builds via
//! `cargo test -p session_manager --target x86_64-unknown-linux-gnu`)
//! follow the same `audio_server` pattern: `no_std` everywhere except
//! the `cfg(test)` paths, which pull in `std` only for the test
//! harness. The OS binary is gated by the `os-binary` feature so the
//! host-test build skips the `_start` symbol entirely.

#![cfg_attr(not(test), no_std)]

extern crate alloc;
#[cfg(test)]
extern crate std;

pub mod table;
