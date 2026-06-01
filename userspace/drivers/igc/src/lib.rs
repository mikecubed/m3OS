//! igc_driver library target — Phase 79 Track B.2.
//!
//! Intel igc covers **only** the I225/I226 discrete Foxville 2.5GbE PCIe
//! controllers (2021+ desktop boards). It uses the same advanced-descriptor +
//! EICR model as igb; the 2.5GBASE-T PHY needs Clause-45 MMD indirection for
//! copper auto-neg disambiguation. The driver-routing split is load-bearing:
//! igb claims I210/I211/I350/82575/82576/I354; igc claims **only** I225/I226.
//!
//! QEMU has no igc model, so this family is hardware-only (the `multi-nic-smoke`
//! gate prints the exclusion reason); the device-ID match + descriptor logic are
//! host-tested.

#![cfg_attr(not(test), no_std)]

extern crate alloc;
#[cfg(test)]
extern crate std;

use driver_runtime::{DeviceCapKey, PciFunctionId, select_nic};
use kernel_core::nic_ids;

// Bring-up + IO-loop modules. Declared `pub` so `cargo test` can exercise the
// pure helpers (including the Clause-45 MMD-PHY accessor); the binary crate's
// single entry point lives in `main.rs`.
pub mod init;
pub mod io;
pub mod regs;
pub mod rings;

/// Boot-log marker.
pub const BOOT_LOG_MARKER: &str = "igc_driver: spawned\n";

/// Sentinel emitted immediately before entering the IRQ/IPC server loop.
pub const SERVER_READY_SENTINEL: &str = "IGC_SMOKE:server:READY\n";

/// Service name under which the driver registers its TX endpoint.
pub const SERVICE_NAME: &str = "net.nic";

/// Kernel-owned ingress service the driver publishes RX/link events to.
pub const INGRESS_SERVICE_NAME: &str = "net.nic.ingress";

/// Pick the first enumerated function that is an Intel igc-family NIC (I225/I226).
pub fn select_igc(candidates: &[PciFunctionId]) -> Option<DeviceCapKey> {
    select_nic(candidates, nic_ids::VENDOR_INTEL, nic_ids::is_igc)
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
    fn selects_i225_and_i226_only() {
        assert!(select_igc(&[f(4, 0x8086, 0x15F2)]).is_some()); // I225
        assert!(select_igc(&[f(4, 0x8086, 0x125B)]).is_some()); // I226
    }

    #[test]
    fn claims_no_igb_id() {
        // I210/I211 → igb, never igc.
        assert!(select_igc(&[f(4, 0x8086, 0x1533)]).is_none());
        assert!(select_igc(&[f(4, 0x8086, 0x1539)]).is_none());
        assert!(select_igc(&[f(4, 0x8086, 0x10C9)]).is_none()); // 82576
    }

    #[test]
    fn sentinels_match_acceptance() {
        assert_eq!(BOOT_LOG_MARKER, "igc_driver: spawned\n");
        assert_eq!(SERVER_READY_SENTINEL, "IGC_SMOKE:server:READY\n");
        assert_eq!(SERVICE_NAME, "net.nic");
        assert_eq!(INGRESS_SERVICE_NAME, "net.nic.ingress");
    }
}
