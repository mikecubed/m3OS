//! e1000_driver library target — exposes crate modules for integration tests.
//!
//! The binary entry point and platform bootstrap live in `main.rs`; this
//! `[lib]` target is compiled alongside `[[bin]] e1000_driver` so the
//! `tests/` integration suite can import `init`, `io`, and `rings`
//! without `#[path]` hacks.
//!
//! The library deliberately has no `#[global_allocator]` or
//! `#[panic_handler]` — those belong to the binary.  When linked into a
//! test binary, the std-provided allocator and panic handler are used.

#![cfg_attr(not(test), no_std)]

extern crate alloc;
#[cfg(test)]
extern crate std;

pub mod init;
pub mod io;
pub mod rings;
