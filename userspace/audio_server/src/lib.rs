//! `audio_server` — Phase 57 Track D ring-3 audio driver.
//!
//! The Phase 57 audio target is the Intel 82801AA AC'97 controller
//! (`0x8086:0x2415`); see `docs/appendix/phase-57-audio-target-choice.md`.
//! `audio_server` claims that PCI function via `sys_device_claim`,
//! programs the controller's BDL DMA path, and serves the
//! `kernel-core::audio` protocol on a Phase 50 IPC endpoint.
//!
//! # Module layout (Single Responsibility)
//!
//! | Module    | Concern                                                                              |
//! |-----------|--------------------------------------------------------------------------------------|
//! | [`device`]  | AC'97 register init, BDL DMA programming, IRQ status decoding                      |
//! | [`stream`]  | PCM ring + per-stream stats (`AudioRingState` consumer, `AudioBackend` driver)     |
//! | [`irq`]    | Phase 55c `IrqNotification::bind_to_endpoint` + `recv_multi` io loop dispatch       |
//! | [`client`]  | Single-client admission policy + rate-limited rejection log                        |
//!
//! The split is the Phase 55b template applied to audio: pure logic
//! lives behind a [`device::AudioBackend`] trait so the io loop can be
//! tested against a fake backend, and the AC'97-specific MMIO + DMA
//! code lives behind a [`device::MmioOps`] seam so a `FakeMmio` can
//! drive register-write ordering tests on the host.
//!
//! # `#![no_std]` discipline
//!
//! Every module is `#![no_std]` + `alloc` (the binary supplies a
//! `BrkAllocator`). Host tests build under `cargo test -p audio_server
//! --target x86_64-unknown-linux-gnu` because the lib target compiles
//! without the OS-only `entry_point!` macro (gated on the
//! `os-binary` feature).

#![cfg_attr(not(test), no_std)]

extern crate alloc;
#[cfg(test)]
extern crate std;

pub mod client;
pub mod device;
pub mod irq;
pub mod stream;

/// Boot-log marker written when the driver starts. Used by xtask smoke
/// scripts to confirm the daemon spawned.
pub const BOOT_LOG_MARKER: &str = "audio_server: spawned\n";

/// Sentinel emitted immediately before entering the IRQ / IPC server
/// loop. Smoke scripts wait for this line to confirm the driver is live
/// and accepting clients.
pub const SERVER_READY_SENTINEL: &str = "AUDIO_SMOKE:server:READY\n";

/// Service name under which the driver registers its command endpoint.
///
/// `audio_client` (Track E) looks the endpoint up by this name to
/// connect to `audio_server`.
pub const SERVICE_NAME: &str = "audio.cmd";

/// Sentinel PCI BDF QEMU uses for `-device AC97` under m3OS — bus 0,
/// device 4, function 0. Slot +4 is the next unused slot after the
/// e1000 family at +3 and avoids colliding with virtio defaults.
pub const SENTINEL_BUS: u8 = 0x00;
pub const SENTINEL_DEVICE: u8 = 0x04;
pub const SENTINEL_FUNCTION: u8 = 0x00;
