//! `ldso_core` — pure-logic core of the m3OS dynamic linker.
//!
//! Modules:
//! * [`reloc`]  — x86_64 relocation primitives (host-testable).
//! * [`dynlink`] — `PT_DYNAMIC` parsing, dependency graph, hash
//!   lookup, constructor invocation (host-testable).
//! * [`elf64`]   — minimal ELF64 type stubs and dynamic-tag constants
//!   shared by the modules above.
//! * [`handle`]  — Phase 76c `dlopen`/`dlclose` handle-table slab
//!   (host-testable).
//!
//! Everything in this library uses only `core::` so the same source
//! compiles for the linker's `no_std` target build *and* the host
//! `cargo test` build (test harness pulls in `std`, but the code
//! under test does not).

#![cfg_attr(not(test), no_std)]
#![deny(unsafe_op_in_unsafe_fn)]

pub mod dynlink;
pub mod elf64;
pub mod gnu_hash;
pub mod handle;
pub mod reloc;
pub mod ver;
