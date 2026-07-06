//! In-kernel remote-debugging support (Phase 111 Track C) — the `kgdb` GDB-RSP
//! stub and its polled COM2 transport.
//!
//! Everything here is compiled only under the `kgdb` cargo feature (see
//! `kernel/Cargo.toml` for the security posture: the stub is arbitrary kernel
//! memory peek/poke and is OFF in production, like `panic-test`/`trace`).
//! The exception-level substrate the stub consumes (trap dispatch, debug
//! registers, `int3` patching) lives in `crate::arch::x86_64::debug` and is
//! always present.

pub mod com2;
pub mod gdbstub;
