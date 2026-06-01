//! r8125_driver library target — Phase 79 Track D.
//!
//! Realtek RTL8125/8125B 2.5GbE (device `0x8125`, **not** the 1GbE `0x8161`),
//! and opportunistically the RTL8126 5GbE (`0x8126`). RTL8125 is a
//! second-generation MAC: it replaces the 16-bit IMR/ISR with a **32-bit V2
//! interrupt block** (IMR_V2_CLEAR 0x150 / ISR_V2 0x154 / IMR_V2_SET 0x158 +
//! INT_CFG0_8125 0x34), and needs a signed PHY-firmware blob to link reliably.
//! It shares the r8169 OWN-bit/TxPoll ring base and XID versioning.
//!
//! QEMU has no r8125 model, so this family is hardware-only; the corrected
//! device-ID match, V2 interrupt-register selection, and firmware-blob header
//! validation are host-tested.

#![cfg_attr(not(test), no_std)]

extern crate alloc;
#[cfg(test)]
extern crate std;

use driver_runtime::{DeviceCapKey, PciFunctionId, select_nic};
use kernel_core::nic_ids;

// V2 interrupt block + firmware policy are pure logic (only depend on
// `kernel_core`), so they are host-tested directly here in both the lib-test and
// os-binary configurations.
pub mod firmware;
pub mod interrupt;

// The V2-interrupt RX/TX loop reuses the r8169 hardware ring base + IRQ wiring,
// which are gated on `os-binary`; so is this module.
#[cfg(feature = "os-binary")]
pub mod io;

/// Service name under which the present NIC registers its TX endpoint (shared).
pub const SERVICE_NAME: &str = "net.nic";
/// Kernel-owned ingress service for RX-frame / link-state publishing.
pub const INGRESS_SERVICE_NAME: &str = "net.nic.ingress";
/// Sentinel emitted immediately before entering the IRQ/IPC server loop.
pub const SERVER_READY_SENTINEL: &str = "R8125_SMOKE:server:READY\n";

/// Pick the first enumerated function that is a Realtek RTL8125/8126-family NIC.
pub fn select_r8125(candidates: &[PciFunctionId]) -> Option<DeviceCapKey> {
    select_nic(candidates, nic_ids::VENDOR_REALTEK, nic_ids::is_r8125)
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
    fn binds_0x8125_not_0x8161() {
        assert!(select_r8125(&[f(4, 0x10EC, 0x8125)]).is_some());
        assert!(select_r8125(&[f(4, 0x10EC, 0x8126)]).is_some()); // RTL8126 5GbE
        // 0x8161 is a 1GbE part — must NOT be claimed as 2.5G.
        assert!(select_r8125(&[f(4, 0x10EC, 0x8161)]).is_none());
        // 1GbE 0x8168 belongs to r8169.
        assert!(select_r8125(&[f(4, 0x10EC, 0x8168)]).is_none());
    }

    #[test]
    fn sentinel_and_service_names_are_load_bearing() {
        assert_eq!(SERVER_READY_SENTINEL, "R8125_SMOKE:server:READY\n");
        assert_eq!(SERVICE_NAME, "net.nic");
        assert_eq!(INGRESS_SERVICE_NAME, "net.nic.ingress");
    }
}
