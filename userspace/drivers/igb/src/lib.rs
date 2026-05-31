//! igb_driver library target — Phase 79 Track B.1.
//!
//! Intel igb covers the 82575/82576 server NICs and the very common I210/I211
//! desktop/embedded parts, plus I350 and I354. igb shares the ring **control
//! flow** with e1000e but requires the **advanced** read/write-back descriptor
//! (it does not accept the legacy 16-byte layout), and its interrupts use the
//! EICR/EIMS block rather than ICR. The advanced descriptor encode/decode lives
//! in `driver_runtime::net_ring` (the `Advanced` impl of `NicDescriptors`); the
//! bring-up + IO loop live in this crate's binary.
//!
//! This `[lib]` target exposes the host-testable device-selection helper; the
//! `_start` bring-up entry point is in `main.rs` behind `os-binary`.

#![cfg_attr(not(test), no_std)]

extern crate alloc;
#[cfg(test)]
extern crate std;

use driver_runtime::{DeviceCapKey, PciFunctionId, select_nic};
use kernel_core::nic_ids;

// Bring-up + IO-loop modules. Declared `pub` so `cargo test` can exercise the
// pure helpers; the binary crate's single entry point lives in `main.rs`.
pub mod init;
pub mod io;
pub mod regs;
pub mod rings;

/// Boot-log marker written when the driver scaffold starts.
pub const BOOT_LOG_MARKER: &str = "igb_driver: spawned\n";

/// Sentinel emitted once link is confirmed at bring-up.
pub const LINK_PASS_SENTINEL: &str = "IGB_SMOKE:link:PASS\n";

/// Sentinel emitted immediately before entering the IRQ/IPC server loop.
pub const SERVER_READY_SENTINEL: &str = "IGB_SMOKE:server:READY\n";

/// Service name under which the driver registers its TX endpoint.
pub const SERVICE_NAME: &str = "net.nic";

/// Kernel-owned ingress service the driver publishes RX/link events to.
pub const INGRESS_SERVICE_NAME: &str = "net.nic.ingress";

/// Pick the first enumerated function that is an Intel igb-family NIC.
pub fn select_igb(candidates: &[PciFunctionId]) -> Option<DeviceCapKey> {
    select_nic(candidates, nic_ids::VENDOR_INTEL, nic_ids::is_igb)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(dev: u8, vendor: u16, device: u16) -> PciFunctionId {
        PciFunctionId {
            key: DeviceCapKey::new(0, 0, dev, 0),
            vendor,
            device,
        }
    }

    #[test]
    fn selects_82576_qemu_igb() {
        let cands = [f(4, 0x8086, 0x10C9)]; // QEMU -device igb (82576)
        assert_eq!(select_igb(&cands), Some(DeviceCapKey::new(0, 0, 4, 0)));
    }

    #[test]
    fn claims_no_e1000e_or_igc_id() {
        // I225 igc and 82574L e1000e must not be claimed by igb.
        assert!(select_igb(&[f(4, 0x8086, 0x15F2)]).is_none()); // igc I225
        assert!(select_igb(&[f(4, 0x8086, 0x10D3)]).is_none()); // e1000e 82574L
        // I210/I211 belong to igb.
        assert!(select_igb(&[f(4, 0x8086, 0x1533)]).is_some());
        assert!(select_igb(&[f(4, 0x8086, 0x1539)]).is_some());
    }

    #[test]
    fn sentinels_match_acceptance() {
        assert_eq!(BOOT_LOG_MARKER, "igb_driver: spawned\n");
        assert_eq!(LINK_PASS_SENTINEL, "IGB_SMOKE:link:PASS\n");
        assert_eq!(SERVER_READY_SENTINEL, "IGB_SMOKE:server:READY\n");
        assert_eq!(SERVICE_NAME, "net.nic");
        assert_eq!(INGRESS_SERVICE_NAME, "net.nic.ingress");
    }
}
