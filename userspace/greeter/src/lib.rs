//! Phase 71 — GUI login manager (greeter) library.
//!
//! Pure-logic modules host-tested via
//! `cargo test -p greeter --target x86_64-unknown-linux-gnu`. The
//! `main.rs` binary composes the production wiring against syscall_lib
//! and the Phase 56 display protocol; this crate exposes the pieces
//! that don't depend on either.
//!
//! ## Modules
//!
//! - [`image`] — BMP / PNG decoders + scale-to-fit blitter.
//! - [`config`] — `/etc/greeter.conf` parser with built-in defaults.
//! - [`auth`] — 3-failure / 5 s backoff state machine + session descriptor.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod auth;
pub mod config;
/// Phase 105 Track C — the image decoders + blitter moved into the shared
/// `imagefmt` crate; re-exported here so `greeter::image::…` paths (and the
/// greeter's own render code) keep working unchanged.
pub use imagefmt as image;
pub mod render;
pub mod session_desc;
