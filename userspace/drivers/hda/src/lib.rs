//! `hda_driver` — Phase 80b ring-3 Intel HDA audio hardware driver.
//!
//! Out-of-process driver serving the `driver_ipc::audio` protocol on the
//! `audio.hw` service, exactly like `userspace/drivers/ac97`. It owns the HDA
//! controller (BAR0 MMIO), drives CORB/RIRB verb DMA, enumerates the codec
//! widget graph, configures one output stream, and DMAs `audio_server`'s mixed
//! PCM (copied in from the shared ring) to the codec's DAC.
//!
//! The host-testable pure logic (register/verb/widget/format/irq decode) lives
//! in [`kernel_core::hda`]; this crate is the production register-poking layer.

#![cfg_attr(not(test), no_std)]

extern crate alloc;
#[cfg(test)]
extern crate std;

// Driver-internal production modules (register-poking).
#[cfg(not(test))]
pub mod codec;
#[cfg(not(test))]
pub mod controller;
#[cfg(not(test))]
pub mod corb;
#[cfg(not(test))]
pub mod stream;

/// Service name the driver registers and `audio_server` resolves.
pub const SERVICE_NAME: &str = "audio.hw";

/// Boot-log marker written when the driver starts.
pub const BOOT_LOG_MARKER: &str = "hda_driver: spawned\n";

/// Sentinel emitted once the controller + codec are up and the server loop is
/// about to start. The `hda-smoke` gate waits for this line.
pub const SERVER_READY_SENTINEL: &str = "HDA_SMOKE:server:READY\n";

/// HDA BAR0 MMIO window length. Intel/AMD HDA controllers expose a register
/// file well under 16 KiB; 0x4000 covers the controller registers plus the
/// stream-descriptor blocks with headroom.
pub const HDA_BAR0_LEN: usize = 0x4000;
