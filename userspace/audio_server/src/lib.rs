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
pub mod stub;

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

/// Sentinel PCI BDF QEMU uses for `-device AC97,addr=0x5` under m3OS —
/// bus 0, device 5, function 0. Slot +3 is e1000_driver, +4 is
/// nvme_driver, so AC'97 lands at +5 to avoid collisions with both.
/// The matching `addr=0x5` flag lives on `-device AC97` in
/// `AC97_QEMU_AUDIO_FLAGS` so QEMU pins the slot rather than auto-
/// assigning at instantiation order.
pub const SENTINEL_BUS: u8 = 0x00;
pub const SENTINEL_DEVICE: u8 = 0x05;
pub const SENTINEL_FUNCTION: u8 = 0x00;

/// Service-manifest restart budget consumed by the supervisor's
/// `max_restart` counter.  Phase 57 D.6 acceptance pins this at 3,
/// matching the Phase 56 F.1 `on-restart` precedent.
pub const SERVICE_MAX_RESTART: u32 = 3;

/// Service-manifest restart policy literal — must match the
/// `restart=` field of `etc/services.d/audio_server.conf`.
pub const SERVICE_RESTART_POLICY: &str = "on-failure";

/// Service-manifest dependency list — `audio_server` depends on
/// `display` (the registered service name in `display_server.conf`)
/// because the chosen session-startup ordering brings display up
/// before audio (A.4). Note the dep names the registered service,
/// not the binary: `display_server.conf` has `name=display`.
pub const SERVICE_DEPENDS: &str = "display";

/// Supervisor restart-callback hook the F.4 recovery path consumes
/// to record a single `audio.device.claim` re-acquire log line on
/// every driver restart.
pub const SERVICE_ON_RESTART: &str = "audio_server.restart";

#[cfg(test)]
mod tests {
    use super::*;

    /// Phase 57 D.6 acceptance: the manifest records
    /// `restart=on-failure max_restart=3 on-restart=audio_server.restart
    /// depends=display`.  The dep names the REGISTERED service from
    /// `display_server.conf` (`name=display`), not the binary.  This
    /// test pins the constants the `populate_ext2_files` helper
    /// consumes.  A regression that silently changed any one of these
    /// would surface here before the supervisor saw the wrong shape
    /// at runtime.
    #[test]
    fn service_manifest_constants_match_acceptance() {
        assert_eq!(SERVICE_RESTART_POLICY, "on-failure");
        assert_eq!(SERVICE_MAX_RESTART, 3);
        assert_eq!(SERVICE_DEPENDS, "display");
        assert_eq!(SERVICE_ON_RESTART, "audio_server.restart");
    }

    /// The static checked-in conf file under `kernel/initrd/etc/`
    /// must declare the same shape the `populate_ext2_files` helper
    /// writes into the data disk.  Drift between the two sources
    /// would cause init's `KNOWN_CONFIGS` fallback path to load a
    /// different policy than the supervised disk path.
    #[test]
    fn checked_in_conf_file_matches_constants() {
        let conf = include_str!("../../../kernel/initrd/etc/services.d/audio_server.conf");
        assert!(
            conf.contains("name=audio_server"),
            "conf must declare name=audio_server"
        );
        assert!(
            conf.contains("command=/bin/audio_server"),
            "conf must declare command=/bin/audio_server"
        );
        assert!(
            conf.contains("type=daemon"),
            "conf must declare type=daemon"
        );
        assert!(
            conf.contains(&alloc::format!("restart={SERVICE_RESTART_POLICY}")),
            "conf must declare restart={SERVICE_RESTART_POLICY}"
        );
        assert!(
            conf.contains(&alloc::format!("max_restart={SERVICE_MAX_RESTART}")),
            "conf must declare max_restart={SERVICE_MAX_RESTART}"
        );
        assert!(
            conf.contains(&alloc::format!("depends={SERVICE_DEPENDS}")),
            "conf must declare depends={SERVICE_DEPENDS}"
        );
        assert!(
            conf.contains(&alloc::format!("on-restart={SERVICE_ON_RESTART}")),
            "conf must declare on-restart={SERVICE_ON_RESTART}"
        );
    }

    /// Sentinel BDF must match the PCI slot QEMU's `-device AC97,addr=0x5`
    /// instantiates. Slot 5 is the first slot after e1000 (slot 3) and
    /// nvme (slot 4); both pre-existing drivers claim those slots so
    /// AC'97 must avoid both.
    #[test]
    fn sentinel_bdf_matches_chosen_target_doc() {
        assert_eq!(SENTINEL_BUS, 0x00);
        assert_eq!(SENTINEL_DEVICE, 0x05);
        assert_eq!(SENTINEL_FUNCTION, 0x00);
    }
}
