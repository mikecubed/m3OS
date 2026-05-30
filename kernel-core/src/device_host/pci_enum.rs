// Phase 78b Track C.1 — pure PCI class-enumeration filter logic.
//
// This module provides host-testable, `no_std`-compatible filter logic for
// the `sys_device_pci_enumerate` syscall. The kernel syscall handler calls
// `collect_matching_bdfs` after snapshotting the live PCI device list; the
// host test suite drives it with a synthetic device list so the matching
// logic is fully verified without a QEMU round-trip.
//
// **Design constraint**: kernel-core must not depend on the kernel's
// `PciDevice` type (that would introduce a circular dependency). The filter
// operates on the minimal `PciDeviceInfo` tuple defined here instead.
// The kernel's syscall handler is responsible for projecting `PciDevice`
// fields into `PciDeviceInfo` values before calling `collect_matching_bdfs`.

/// Minimal projection of a PCI function's identity, extracted from the
/// kernel's `PciDevice` for the purpose of class-based enumeration.
///
/// `segment` is always 0 on current m3OS platforms (single-segment PCIe);
/// it is included so the BDF packing format is unambiguous if multi-segment
/// support is ever added.
///
/// `#[repr(C)]` and `Clone + Copy` so the kernel can cheaply project
/// `PciDevice` fields into this struct without alloc.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PciDeviceInfo {
    /// PCI Express segment group (always 0 on current platforms).
    pub segment: u16,
    /// PCI bus number (0–255).
    pub bus: u8,
    /// PCI device number (0–31).
    pub dev: u8,
    /// PCI function number (0–7).
    pub func: u8,
    /// PCI class code (e.g. `0x0C` = Serial Bus Controller).
    pub class_code: u8,
    /// PCI subclass (e.g. `0x03` = USB).
    pub subclass: u8,
    /// Programming Interface byte (e.g. `0x30` = xHCI).
    pub prog_if: u8,
}

impl PciDeviceInfo {
    /// Construct a `PciDeviceInfo` from raw fields.
    #[inline]
    pub const fn new(
        segment: u16,
        bus: u8,
        dev: u8,
        func: u8,
        class_code: u8,
        subclass: u8,
        prog_if: u8,
    ) -> Self {
        Self {
            segment,
            bus,
            dev,
            func,
            class_code,
            subclass,
            prog_if,
        }
    }

    /// Returns `true` when this device matches the given class / subclass /
    /// prog_if triple exactly. All three fields must match.
    #[inline]
    pub const fn matches_class(&self, class: u8, subclass: u8, prog_if: u8) -> bool {
        self.class_code == class && self.subclass == subclass && self.prog_if == prog_if
    }

    /// Pack this device's BDF into a `u32` in the format documented in
    /// [`SYS_DEVICE_PCI_ENUMERATE`](super::syscalls::SYS_DEVICE_PCI_ENUMERATE):
    ///
    /// ```text
    /// bits [31:20] — PCI segment (currently always 0)
    /// bits [19:12] — bus number
    /// bits [11: 5] — device number
    /// bits [ 4: 2] — function number
    /// bits [  1:0] — reserved (always 0)
    /// ```
    #[inline]
    pub const fn pack_bdf(&self) -> u32 {
        ((self.segment as u32) << 20)
            | ((self.bus as u32) << 12)
            | ((self.dev as u32) << 5)
            | ((self.func as u32) << 2)
    }
}

/// Filter `devices` by `(class, subclass, prog_if)` and write matching BDF
/// entries (packed as `u32` via [`PciDeviceInfo::pack_bdf`]) into `out`.
///
/// Returns the **total** count of matching devices, regardless of how many fit
/// in `out`. The caller can pass an empty slice to query the count, then call
/// again with an appropriately sized buffer. Devices are written in
/// enumeration order (the order they appear in `devices`), which is bus-scan
/// order on the kernel side.
///
/// This function performs no allocation and is suitable for `no_std` + IRQ
/// contexts (though in practice it is called from task context only, with the
/// PCI device list snapshotted before calling).
pub fn collect_matching_bdfs(
    devices: &[PciDeviceInfo],
    class: u8,
    subclass: u8,
    prog_if: u8,
    out: &mut [u32],
) -> usize {
    let mut total = 0usize;
    let mut written = 0usize;
    for dev in devices {
        if dev.matches_class(class, subclass, prog_if) {
            if written < out.len() {
                out[written] = dev.pack_bdf();
                written += 1;
            }
            total += 1;
        }
    }
    total
}

// ---------------------------------------------------------------------------
// Host tests (TDD: written before the kernel integration wiring)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a synthetic device list representative of a small QEMU machine:
    ///
    /// | idx | BDF      | class:sub:pi | Description              |
    /// |-----|----------|--------------|--------------------------|
    /// |  0  | 00:00.0  | 06:00:00     | Host bridge              |
    /// |  1  | 00:01.0  | 03:00:00     | VGA (EHCI near-miss)     |
    /// |  2  | 00:02.0  | 0C:03:20     | USB EHCI (prog_if miss)  |
    /// |  3  | 00:03.0  | 0C:03:30     | USB xHCI — MATCH         |
    /// |  4  | 00:04.0  | 01:08:02     | NVMe                     |
    /// |  5  | 00:05.0  | 0C:03:30     | USB xHCI — MATCH         |
    /// |  6  | 00:06.0  | 0C:03:30     | USB xHCI — MATCH         |
    /// |  7  | 00:07.0  | 02:00:00     | Ethernet                 |
    /// |  8  | 00:1f.2  | 01:06:01     | SATA AHCI                |
    fn synthetic_devices() -> [PciDeviceInfo; 9] {
        [
            PciDeviceInfo::new(0, 0x00, 0x00, 0, 0x06, 0x00, 0x00), // host bridge
            PciDeviceInfo::new(0, 0x00, 0x01, 0, 0x03, 0x00, 0x00), // VGA
            PciDeviceInfo::new(0, 0x00, 0x02, 0, 0x0C, 0x03, 0x20), // EHCI (prog_if 0x20, not 0x30)
            PciDeviceInfo::new(0, 0x00, 0x03, 0, 0x0C, 0x03, 0x30), // xHCI #1
            PciDeviceInfo::new(0, 0x00, 0x04, 0, 0x01, 0x08, 0x02), // NVMe
            PciDeviceInfo::new(0, 0x00, 0x05, 0, 0x0C, 0x03, 0x30), // xHCI #2
            PciDeviceInfo::new(0, 0x00, 0x06, 0, 0x0C, 0x03, 0x30), // xHCI #3
            PciDeviceInfo::new(0, 0x00, 0x07, 0, 0x02, 0x00, 0x00), // Ethernet
            PciDeviceInfo::new(0, 0x00, 0x1f, 2, 0x01, 0x06, 0x01), // SATA AHCI
        ]
    }

    // --- PciDeviceInfo::matches_class -----------------------------------------

    #[test]
    fn matches_class_requires_all_three_fields() {
        let xhci = PciDeviceInfo::new(0, 0, 3, 0, 0x0C, 0x03, 0x30);
        assert!(
            xhci.matches_class(0x0C, 0x03, 0x30),
            "exact match must succeed"
        );
        assert!(
            !xhci.matches_class(0x0B, 0x03, 0x30),
            "wrong class must fail"
        );
        assert!(
            !xhci.matches_class(0x0C, 0x04, 0x30),
            "wrong subclass must fail"
        );
        assert!(
            !xhci.matches_class(0x0C, 0x03, 0x20),
            "wrong prog_if must fail"
        );
    }

    #[test]
    fn matches_class_distinguishes_xhci_from_ehci() {
        let xhci = PciDeviceInfo::new(0, 0, 3, 0, 0x0C, 0x03, 0x30);
        let ehci = PciDeviceInfo::new(0, 0, 2, 0, 0x0C, 0x03, 0x20);
        assert!(xhci.matches_class(0x0C, 0x03, 0x30));
        assert!(!ehci.matches_class(0x0C, 0x03, 0x30));
    }

    // --- PciDeviceInfo::pack_bdf ----------------------------------------------

    #[test]
    fn pack_bdf_encodes_bus_dev_func_correctly() {
        // BDF 00:03.0 → seg=0, bus=0, dev=3, func=0
        let dev = PciDeviceInfo::new(0, 0x00, 0x03, 0x00, 0x0C, 0x03, 0x30);
        let packed = dev.pack_bdf();
        let seg = (packed >> 20) & 0xFFF;
        let bus = (packed >> 12) & 0xFF;
        let device_num = (packed >> 5) & 0x7F;
        let func = (packed >> 2) & 0x07;
        assert_eq!(seg, 0, "segment must be 0");
        assert_eq!(bus, 0x00, "bus mismatch");
        assert_eq!(device_num, 0x03, "device number mismatch");
        assert_eq!(func, 0x00, "function mismatch");
        assert_eq!(packed & 0x3, 0, "reserved bits must be 0");
    }

    #[test]
    fn pack_bdf_encodes_multi_function_device() {
        // BDF 00:1f.2 (SATA AHCI)
        let dev = PciDeviceInfo::new(0, 0x00, 0x1f, 0x02, 0x01, 0x06, 0x01);
        let packed = dev.pack_bdf();
        let bus = (packed >> 12) & 0xFF;
        let device_num = (packed >> 5) & 0x7F;
        let func = (packed >> 2) & 0x07;
        assert_eq!(bus, 0x00);
        assert_eq!(device_num, 0x1f);
        assert_eq!(func, 0x02);
    }

    #[test]
    fn pack_bdf_round_trips_for_all_synthetic_devices() {
        let devs = synthetic_devices();
        for d in &devs {
            let packed = d.pack_bdf();
            let seg = ((packed >> 20) & 0xFFF) as u16;
            let bus = ((packed >> 12) & 0xFF) as u8;
            let dev_num = ((packed >> 5) & 0x7F) as u8;
            let func = ((packed >> 2) & 0x07) as u8;
            assert_eq!(seg, d.segment, "segment roundtrip failed for {:?}", d);
            assert_eq!(bus, d.bus, "bus roundtrip failed for {:?}", d);
            assert_eq!(dev_num, d.dev, "dev roundtrip failed for {:?}", d);
            assert_eq!(func, d.func, "func roundtrip failed for {:?}", d);
        }
    }

    // --- collect_matching_bdfs -----------------------------------------------

    /// Core acceptance test: exactly the three xHCI controllers are returned
    /// from the synthetic device list, in bus-scan order, with the EHCI
    /// (prog_if 0x20) excluded.
    #[test]
    fn collect_matching_bdfs_returns_only_xhci_controllers() {
        let devs = synthetic_devices();
        let mut out = [0u32; 8];
        let total = collect_matching_bdfs(&devs, 0x0C, 0x03, 0x30, &mut out);

        assert_eq!(total, 3, "expected exactly 3 xHCI controllers");

        // Verify BDF 00:03.0 is first.
        let b0 = out[0];
        assert_eq!((b0 >> 12) & 0xFF, 0x00, "first match bus");
        assert_eq!((b0 >> 5) & 0x7F, 0x03, "first match dev");
        assert_eq!((b0 >> 2) & 0x07, 0x00, "first match func");

        // Verify BDF 00:05.0 is second.
        let b1 = out[1];
        assert_eq!((b1 >> 12) & 0xFF, 0x00, "second match bus");
        assert_eq!((b1 >> 5) & 0x7F, 0x05, "second match dev");

        // Verify BDF 00:06.0 is third.
        let b2 = out[2];
        assert_eq!((b2 >> 12) & 0xFF, 0x00, "third match bus");
        assert_eq!((b2 >> 5) & 0x7F, 0x06, "third match dev");

        // Entries beyond total should remain zero (not touched).
        assert_eq!(out[3], 0, "slot 3 must be untouched");
    }

    #[test]
    fn collect_matching_bdfs_excludes_ehci_prog_if_miss() {
        let devs = synthetic_devices();
        let mut out = [0u32; 8];
        let total = collect_matching_bdfs(&devs, 0x0C, 0x03, 0x30, &mut out);
        // The EHCI at 00:02.0 (prog_if 0x20) must not appear.
        for i in 0..total {
            let dev_num = (out[i] >> 5) & 0x7F;
            assert_ne!(
                dev_num, 0x02,
                "EHCI at dev 2 must not appear in xHCI results"
            );
        }
    }

    #[test]
    fn collect_matching_bdfs_empty_slice_returns_count() {
        // Calling with an empty output slice returns the total without writing.
        let devs = synthetic_devices();
        let mut out: [u32; 0] = [];
        let total = collect_matching_bdfs(&devs, 0x0C, 0x03, 0x30, &mut out);
        assert_eq!(
            total, 3,
            "count must be correct even with empty output buffer"
        );
    }

    #[test]
    fn collect_matching_bdfs_capped_at_max_entries() {
        // If the caller provides a buffer smaller than the match count, only
        // max_entries entries are written but the full count is still returned.
        let devs = synthetic_devices();
        let mut out = [0u32; 2];
        let total = collect_matching_bdfs(&devs, 0x0C, 0x03, 0x30, &mut out);
        assert_eq!(total, 3, "total must reflect all matches");
        assert_ne!(out[0], 0, "first entry must be written");
        assert_ne!(out[1], 0, "second entry must be written");
    }

    #[test]
    fn collect_matching_bdfs_no_match_returns_zero() {
        // A class that has no devices in the list.
        let devs = synthetic_devices();
        let mut out = [0u32; 8];
        let total = collect_matching_bdfs(&devs, 0xFF, 0xFF, 0xFF, &mut out);
        assert_eq!(total, 0);
        assert!(out.iter().all(|&v| v == 0), "no entries must be written");
    }

    #[test]
    fn collect_matching_bdfs_single_entry_list() {
        let devs = [PciDeviceInfo::new(0, 0x01, 0x00, 0, 0x0C, 0x03, 0x30)];
        let mut out = [0u32; 4];
        let total = collect_matching_bdfs(&devs, 0x0C, 0x03, 0x30, &mut out);
        assert_eq!(total, 1);
        let bus = (out[0] >> 12) & 0xFF;
        assert_eq!(bus, 0x01);
    }

    #[test]
    fn collect_matching_bdfs_preserves_enumeration_order() {
        // Three xHCI devices at different bus numbers; result must come out
        // in the order they appear in the slice.
        let devs = [
            PciDeviceInfo::new(0, 0x02, 0x00, 0, 0x0C, 0x03, 0x30),
            PciDeviceInfo::new(0, 0x00, 0x00, 0, 0x0C, 0x03, 0x30),
            PciDeviceInfo::new(0, 0x01, 0x00, 0, 0x0C, 0x03, 0x30),
        ];
        let mut out = [0u32; 4];
        let total = collect_matching_bdfs(&devs, 0x0C, 0x03, 0x30, &mut out);
        assert_eq!(total, 3);
        assert_eq!((out[0] >> 12) & 0xFF, 0x02, "first must be bus 0x02");
        assert_eq!((out[1] >> 12) & 0xFF, 0x00, "second must be bus 0x00");
        assert_eq!((out[2] >> 12) & 0xFF, 0x01, "third must be bus 0x01");
    }
}
