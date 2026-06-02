//! mt792x_driver library target — Phase 81 Track DRV-shell.
//!
//! MediaTek MT7921/MT7922/MT7920/MT7902/MT7925 PCIe Wi-Fi driver hardware
//! shell. PCI device selection and service/sentinel constants are host-testable
//! here. The hardware bring-up modules (init, fw, mcu, rings) are gated on
//! `#[cfg(not(test))]` so they are only compiled for the bare-metal target —
//! they depend on DmaBuffer/Mmio/syscall_lib which only build for the kernel
//! target, mirroring how r8169/e1000 declare their hardware modules.
//!
//! Wave 3 (DRV-net) adds the net.nic registration + RX/TX rewrite + EAPOL
//! demux + key-install path on top of this hardware shell.

#![cfg_attr(not(test), no_std)]

extern crate alloc;
#[cfg(test)]
extern crate std;

use driver_runtime::DeviceCapKey;
use driver_runtime::pci_enum::{PciFunctionId, select_nic};
use kernel_core::nic_ids;

// Hardware bring-up modules — gated on not(test) because they depend on
// syscall_lib/DmaBuffer/Mmio which only build for the bare-metal target.
// The host lib-test path compiles only the pure-logic selection code above.
// Mirroring the exact pattern used in r8169/src/lib.rs and e1000/src/lib.rs.
#[cfg(not(test))]
pub mod fw;
// Pure firmware-download protocol constants + decode — host-testable (no
// hardware deps), unlike `fw` which drives the `McuRing`.
pub mod fw_proto;
#[cfg(not(test))]
pub mod init;
#[cfg(not(test))]
pub mod key;
#[cfg(not(test))]
pub mod mcu;
#[cfg(not(test))]
pub mod rings;

// The net.nic data path (Track DRV-net). Declared unconditionally: its pure
// Ethernet⇄802.11 rewrite + EAPOL-demux functions are host-tested, while the
// `run_io_loop` that touches the WFDMA rings / MCU / FSM is `#[cfg(not(test))]`
// inside the module.
pub mod io;

/// Service name under which the present Wi-Fi NIC registers its TX endpoint.
/// Shared across NIC families — only the NIC actually present registers it.
pub const SERVICE_NAME: &str = "net.nic";

/// Kernel-owned ingress service for RX-frame / link-state publishing.
pub const INGRESS_SERVICE_NAME: &str = "net.nic.ingress";

/// Sentinel emitted immediately before entering the IRQ/IPC server loop.
pub const SERVER_READY_SENTINEL: &str = "MT792X_SMOKE:server:READY\n";

/// Sentinel emitted when the firmware blob is absent and the driver degrades.
///
/// The driver emits this and continues (no panic, no build break) — mirroring
/// the r8125 `FW_DEGRADED_SENTINEL` pattern. The real firmware blob is staged
/// later by the coordinator (E.2), license-gated. Until then Wi-Fi is disabled
/// but the driver binary compiles and boots cleanly.
pub const FW_ABSENT_SENTINEL: &str = "MT792X_FW:absent:firmware blob absent \u{2014} Wi-Fi disabled, see docs/legal/firmware-licenses.md\n";

/// Pick the first enumerated function that is a MediaTek mt792x-family Wi-Fi NIC.
///
/// This is the Task A.2 selection function — host-testable because it operates
/// purely on the pre-claim PciFunctionId slice (no syscalls, no hardware).
pub fn select_mt792x(functions: &[PciFunctionId]) -> Option<DeviceCapKey> {
    select_nic(functions, nic_ids::VENDOR_MEDIATEK, nic_ids::is_mt792x)
}

#[cfg(test)]
mod tests {
    use super::*;
    use driver_runtime::DeviceCapKey;
    use driver_runtime::pci_enum::PciFunctionId;

    fn make_fn(dev: u8, vendor: u16, device: u16) -> PciFunctionId {
        PciFunctionId {
            key: DeviceCapKey::new(0, 0, dev, 0),
            vendor,
            device,
        }
    }

    #[test]
    fn select_prefers_mt792x() {
        // A list containing one mt792x device and one foreign device.
        let funcs = [
            make_fn(3, 0x14C3, 0x7961), // MT7921 — mt792x
            make_fn(4, 0x8086, 0x100E), // Intel e1000 — foreign
        ];
        let key = select_mt792x(&funcs);
        assert!(key.is_some(), "must select the mt792x device");
        assert_eq!(key.unwrap(), DeviceCapKey::new(0, 0, 3, 0));
    }

    #[test]
    fn select_returns_none_when_no_mt792x() {
        // Only Intel and Realtek NICs — no MediaTek.
        let funcs = [
            make_fn(2, 0x8086, 0x100E), // Intel e1000
            make_fn(3, 0x10EC, 0x8168), // Realtek r8169
        ];
        assert!(
            select_mt792x(&funcs).is_none(),
            "must return None when no mt792x device is present"
        );
    }

    #[test]
    fn select_returns_none_on_empty_list() {
        assert!(select_mt792x(&[]).is_none());
    }

    #[test]
    fn select_covers_all_mt792x_families() {
        // Every known mt792x device ID must be selected over a co-listed foreign NIC.
        let foreign = make_fn(9, 0x8086, 0x100E);
        for &dev_id in &[
            0x7961u16, 0x7922, 0x7920, 0x7902, 0x7925, 0x0608, 0x0616, 0x0717,
        ] {
            let funcs = [make_fn(5, 0x14C3, dev_id), foreign];
            assert!(
                select_mt792x(&funcs).is_some(),
                "mt792x device {dev_id:#06x} must be selected"
            );
        }
    }

    #[test]
    fn sentinels_and_service_names_are_load_bearing() {
        assert_eq!(SERVICE_NAME, "net.nic");
        assert_eq!(INGRESS_SERVICE_NAME, "net.nic.ingress");
        assert_eq!(SERVER_READY_SENTINEL, "MT792X_SMOKE:server:READY\n");
        // FW_ABSENT_SENTINEL must contain the key diagnostic keyword.
        assert!(FW_ABSENT_SENTINEL.contains("MT792X_FW:absent:"));
    }
}
