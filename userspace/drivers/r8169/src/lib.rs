//! r8169_driver library target — Phase 79 Track C.
//!
//! Realtek RTL8111/8168 PCIe Gigabit (the common modern consumer part), the
//! original parallel-PCI RTL8169, and the RTL810xE Fast Ethernet. Structurally
//! different from Intel: no head/tail registers — ownership is per-descriptor
//! via the `DescOwn` bit, the last descriptor carries `EOR`, and TX is started
//! by writing the **TxPoll doorbell** (0x38), not a tail register. The driver
//! dispatches on a runtime **XID** read from TxConfig (0x40), not the PCI
//! device ID; the XID→`mac_version` table lives in `kernel_core::r8169`.
//!
//! QEMU has no r8169 model (it emulates only the RTL8139 C+ chip), so this
//! family is hardware-only; the device-ID match, ring bit-layout, and XID
//! version table are host-tested.

#![cfg_attr(not(test), no_std)]

extern crate alloc;
#[cfg(test)]
extern crate std;

use driver_runtime::{DeviceCapKey, PciFunctionId, select_nic};
use kernel_core::nic_ids;

// Hardware bring-up modules (no QEMU r8169 model — these run on real silicon).
// Declared `pub` so the binary's `program_main` (and the r8125 driver, which
// reuses the r8169 ring base) can drive them; the pure bit-level logic they
// consume is host-tested in `kernel_core::r8169`. Gated on `not(test)`: they
// pull in `syscall_lib` / `DmaBuffer`, which only build for the bare-metal
// target, so the host `cargo test` of this lib must not compile them. The bin
// (which always builds with `cfg(test)` false and the runtime deps present)
// sees them; the host lib-test harness does not. This mirrors how the in-tree
// e1000 driver declares its `init` / `io` / `rings` modules.
#[cfg(not(test))]
pub mod init;
#[cfg(not(test))]
pub mod io;
#[cfg(not(test))]
pub mod rings;

/// Service name under which the present NIC registers its TX endpoint. Shared
/// across NIC families — only the NIC actually present registers it.
pub const SERVICE_NAME: &str = "net.nic";
/// Kernel-owned ingress service for RX-frame / link-state publishing.
pub const INGRESS_SERVICE_NAME: &str = "net.nic.ingress";
/// Sentinel emitted immediately before entering the IRQ/IPC server loop.
pub const SERVER_READY_SENTINEL: &str = "R8169_SMOKE:server:READY\n";

/// Pick the first enumerated function that is a Realtek r8169-family NIC.
pub fn select_r8169(candidates: &[PciFunctionId]) -> Option<DeviceCapKey> {
    select_nic(candidates, nic_ids::VENDOR_REALTEK, nic_ids::is_r8169)
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
    fn selects_realtek_gbe_set() {
        for id in [0x8168u16, 0x8169, 0x8161, 0x8167, 0x8136] {
            assert!(select_r8169(&[f(4, 0x10EC, id)]).is_some(), "{id:#06x}");
        }
    }

    #[test]
    fn does_not_select_2_5g_or_wrong_vendor() {
        assert!(select_r8169(&[f(4, 0x10EC, 0x8125)]).is_none()); // 2.5G → r8125
        assert!(select_r8169(&[f(4, 0x8086, 0x8168)]).is_none()); // wrong vendor
    }

    #[test]
    fn sentinel_and_service_names_are_load_bearing() {
        // The smoke harness greps for this exact sentinel; the kernel RemoteNic
        // facade looks up these exact service names.
        assert_eq!(SERVER_READY_SENTINEL, "R8169_SMOKE:server:READY\n");
        assert_eq!(SERVICE_NAME, "net.nic");
        assert_eq!(INGRESS_SERVICE_NAME, "net.nic.ingress");
    }
}
