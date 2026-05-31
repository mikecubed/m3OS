//! e1000e_driver library target — Phase 79 Track A.
//!
//! e1000e accepts the same legacy 16-byte descriptor and the same RAL0/RAH0
//! MAC path as the classic 82540EM, so this driver **reuses the e1000 driver's
//! `init` / `io` / `rings` modules verbatim** (via the `e1000_driver` lib
//! dependency). The only new logic is PCI device discovery: instead of a
//! hardcoded BDF, the driver enumerates Ethernet controllers, reads each
//! function's vendor:device ID through the Phase 79 `sys_device_config_read`
//! path, and claims the first one in the e1000e family ID set
//! (`kernel_core::nic_ids::is_e1000e`).
//!
//! This `[lib]` target exposes the pure selection helper so it is host-testable
//! without a real PCI bus; the `_start` bring-up entry point lives in `main.rs`
//! behind the `os-binary` feature.

#![cfg_attr(not(test), no_std)]

extern crate alloc;
#[cfg(test)]
extern crate std;

use driver_runtime::{DeviceCapKey, PciFunctionId, select_nic};
use kernel_core::nic_ids;

/// Pick the first enumerated function that is an Intel e1000e-family NIC.
///
/// Returns the [`DeviceCapKey`] to claim, or `None` when no candidate matches
/// (QEMU launched without `-device e1000e`, or only other NIC families present).
pub fn select_e1000e(candidates: &[PciFunctionId]) -> Option<DeviceCapKey> {
    select_nic(candidates, nic_ids::VENDOR_INTEL, nic_ids::is_e1000e)
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
    fn selects_e1000e_82574l_and_skips_others() {
        let cands = [
            f(3, 0x1AF4, 0x1000), // virtio-net — wrong vendor
            f(4, 0x8086, 0x10D3), // 82574L e1000e — QEMU's -device e1000e
        ];
        assert_eq!(select_e1000e(&cands), Some(DeviceCapKey::new(0, 0, 4, 0)));
    }

    #[test]
    fn does_not_select_classic_e1000_or_igb() {
        let cands = [
            f(3, 0x8086, 0x100E), // classic 82540EM — belongs to e1000 driver
            f(4, 0x8086, 0x10C9), // igb 82576 — belongs to igb driver
        ];
        assert_eq!(select_e1000e(&cands), None);
    }

    #[test]
    fn selects_representative_i219() {
        let cands = [f(3, 0x8086, 0x15B7)];
        assert_eq!(select_e1000e(&cands), Some(DeviceCapKey::new(0, 0, 3, 0)));
    }
}
