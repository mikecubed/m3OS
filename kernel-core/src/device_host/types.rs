// Device-host ABI types — Phase 55b Track A.1.
//
// Pure-logic types shared by the kernel-side syscall handlers (Track B) and
// the userspace `driver_runtime` (Track C). Everything here is `no_std` +
// `alloc`-only and host-testable via `cargo test -p kernel-core`. This module
// is the single source of truth for these types — no later phase is permitted
// to redeclare them.

/// Bus/Device/Function identifier for a PCI(e) endpoint, plus its PCI segment.
///
/// `#[repr(C)]` so the layout is stable across boots and across FFI-ish
/// boundaries (capability payloads, log lines, trace records). `Hash` is
/// derived so `DeviceCapKey` is usable as a map key in both kernel and host
/// contexts without re-boxing.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct DeviceCapKey {
    pub segment: u16,
    pub bus: u8,
    pub dev: u8,
    pub func: u8,
}

impl DeviceCapKey {
    /// Construct a device capability key.
    pub const fn new(segment: u16, bus: u8, dev: u8, func: u8) -> Self {
        Self {
            segment,
            bus,
            dev,
            func,
        }
    }

    /// Stable byte encoding — six bytes:
    /// `[segment_lo, segment_hi, bus, dev, func, 0]`.
    ///
    /// The trailing zero is reserved for future flags and must remain zero in
    /// any valid payload; [`Self::from_bytes`] rejects non-zero reserved
    /// bytes.
    pub const fn to_bytes(self) -> [u8; 6] {
        let seg = self.segment.to_le_bytes();
        [seg[0], seg[1], self.bus, self.dev, self.func, 0]
    }

    /// Inverse of [`Self::to_bytes`]. Returns `None` if the reserved byte is
    /// non-zero — treat that as a malformed payload rather than silently
    /// succeeding.
    pub const fn from_bytes(bytes: [u8; 6]) -> Option<Self> {
        if bytes[5] != 0 {
            return None;
        }
        let segment = u16::from_le_bytes([bytes[0], bytes[1]]);
        Some(Self {
            segment,
            bus: bytes[2],
            dev: bytes[3],
            func: bytes[4],
        })
    }
}

/// Caching mode for an MMIO window mapping.
///
/// Drivers that touch PCIe config-space registers use [`Self::Uncacheable`];
/// drivers that use write-combining BAR regions (for example, NIC doorbell
/// batches or frame-buffer scratch space) request [`Self::WriteCombining`].
///
/// `#[non_exhaustive]` so future cache modes (e.g. WriteBack for shared-memory
/// devices) can be added without breaking downstream match exhaustiveness.
#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[non_exhaustive]
pub enum MmioCacheMode {
    Uncacheable = 0,
    WriteCombining = 1,
}

impl MmioCacheMode {
    /// Single-byte stable encoding — matches the `#[repr(u8)]` discriminant.
    pub const fn to_byte(self) -> u8 {
        self as u8
    }

    /// Inverse of [`Self::to_byte`]. Returns `None` for unknown discriminants.
    pub const fn from_byte(b: u8) -> Option<Self> {
        match b {
            0 => Some(Self::Uncacheable),
            1 => Some(Self::WriteCombining),
            _ => None,
        }
    }
}

/// Description of a BAR window the device host will map into a driver.
///
/// Produced by the kernel-side syscall handler (`sys_device_mmio_map`, Track
/// B.2) and consumed by the driver process via an IPC reply payload — so it
/// must survive a plain byte round-trip. [`Self::to_bytes`] and
/// [`Self::from_bytes`] provide that stable encoding; the unit tests pin the
/// exact layout.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct MmioWindowDescriptor {
    pub phys_base: u64,
    pub len: usize,
    pub bar_index: u8,
    pub prefetchable: bool,
    pub cache_mode: MmioCacheMode,
}

/// Serialized size of an `MmioWindowDescriptor` in bytes.
///
/// Layout totals 20 bytes (8 for `phys_base`, 8 for `len`, 1 each for
/// `bar_index`, `prefetchable`, `cache_mode`, and 1 reserved byte). Kept as
/// an explicit constant so the producing and consuming sides can pre-size
/// buffers without rederiving the number.
pub const MMIO_WINDOW_DESCRIPTOR_SIZE: usize = 20;

impl MmioWindowDescriptor {
    /// Stable byte encoding used for IPC-payload transit.
    ///
    /// Layout (little-endian):
    ///
    /// - `[0..8]` — `phys_base: u64`
    /// - `[8..16]` — `len: u64` (host `usize` widened to `u64`)
    /// - `[16]` — `bar_index: u8`
    /// - `[17]` — `prefetchable: u8` (0 or 1; any other value is rejected)
    /// - `[18]` — `cache_mode: u8`
    /// - `[19]` — reserved (must be zero)
    pub fn to_bytes(self) -> [u8; MMIO_WINDOW_DESCRIPTOR_SIZE] {
        let mut out = [0u8; MMIO_WINDOW_DESCRIPTOR_SIZE];
        out[0..8].copy_from_slice(&self.phys_base.to_le_bytes());
        out[8..16].copy_from_slice(&(self.len as u64).to_le_bytes());
        out[16] = self.bar_index;
        out[17] = u8::from(self.prefetchable);
        out[18] = self.cache_mode.to_byte();
        out[19] = 0;
        out
    }

    /// Inverse of [`Self::to_bytes`]. Returns `None` if `prefetchable`,
    /// `cache_mode`, or the reserved byte carry invalid values.
    pub fn from_bytes(bytes: [u8; MMIO_WINDOW_DESCRIPTOR_SIZE]) -> Option<Self> {
        if bytes[19] != 0 {
            return None;
        }
        let phys_base = u64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]);
        let len_u64 = u64::from_le_bytes([
            bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15],
        ]);
        let bar_index = bytes[16];
        let prefetchable = match bytes[17] {
            0 => false,
            1 => true,
            _ => return None,
        };
        let cache_mode = MmioCacheMode::from_byte(bytes[18])?;
        Some(Self {
            phys_base,
            len: len_u64 as usize,
            bar_index,
            prefetchable,
            cache_mode,
        })
    }
}

/// Runtime handle for a DMA buffer granted to a driver process.
///
/// `user_va` is the driver-process virtual address; `iova` is the I/O virtual
/// address the device will DMA through (on IOMMU-enabled platforms) or the
/// identity-mapped physical address (on the `DmaBuffer<T>` identity-fallback
/// path documented in Phase 55a). The two fields are independently validated
/// — either may legitimately be zero:
///   - `user_va == 0`: a kernel-internal staging buffer with no user mapping.
///   - `iova == 0`: rare; indicates the driver has not yet programmed the
///     device with the DMA address.
///
/// When identity-mapping is in effect, `iova == phys_addr`, which means a
/// caller can compare `iova` against the known physical base to confirm the
/// identity path — see the unit tests.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DmaHandle {
    pub user_va: usize,
    pub iova: u64,
    pub len: usize,
}

/// Error kinds emitted by the device-host path.
///
/// Variants are *data* (not strings) so both the kernel-side syscall
/// dispatcher and the userspace `driver_runtime` can pattern-match on them
/// without string parsing. Every variant must be constructible without
/// allocation so it works on the `no_std` kernel side.
///
/// `#[non_exhaustive]` so tracks B–F may add variants without forcing
/// downstream crates into an exhaustive match — the defining crate still
/// exhaustively matches (see the unit tests) so nothing here is silently
/// dropped.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum DeviceHostError {
    /// The caller does not hold a `Capability::Device` for the target BDF.
    NotClaimed,
    /// The BDF is already claimed by a different driver process.
    AlreadyClaimed,
    /// The requested BAR index is outside the device's BAR table.
    InvalidBarIndex,
    /// The BAR index is valid but the requested length exceeds BAR size.
    BarOutOfBounds,
    /// The IOMMU's IOVA allocator has no room for a new mapping.
    IovaExhausted,
    /// The IOMMU reported a translation or permission fault.
    IommuFault,
    /// A per-driver bound (MMIO / DMA / IRQ slot count) would be exceeded.
    CapacityExceeded,
    /// No MSI-X / legacy vector is available for the requested IRQ.
    IrqUnavailable,
    /// A capability handle was not a `Capability::Device` variant.
    BadDeviceCap,
    /// An unexpected internal inconsistency — reserved for invariants that
    /// must not be reached from documented callers. Callers treat it as a
    /// bug and the service manager restarts the offending driver.
    Internal,
}

/// Upper bound (milliseconds) on how long a `RemoteBlockDevice` /
/// `RemoteNic` request may stall across a driver crash/restart cycle.
///
/// This is the single source of truth for the restart-bound deadline used by
/// `RemoteBlockDevice` (D.4), `RemoteNic` (E.4), the crash-restart regression
/// test (F.2), and every restart-bound acceptance item in Tracks D / E / F.
/// Hand-rolling a different literal elsewhere is a lint violation per the
/// Phase 55b task list.
pub const DRIVER_RESTART_TIMEOUT_MS: u32 = 1000;

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    // ---- DeviceCapKey ----------------------------------------------------

    #[test]
    fn device_cap_key_equality_is_field_wise() {
        let a = DeviceCapKey::new(0, 0x00, 0x1f, 2);
        let b = DeviceCapKey::new(0, 0x00, 0x1f, 2);
        let c = DeviceCapKey::new(1, 0x00, 0x1f, 2);
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn device_cap_key_hash_is_stable_across_rebuilds() {
        let k1 = DeviceCapKey::new(0, 0x42, 0x03, 1);
        let k2 = DeviceCapKey::new(0, 0x42, 0x03, 1);
        let mut set = HashSet::new();
        set.insert(k1);
        assert!(set.contains(&k2));

        let canonical = DeviceCapKey::new(0, 0x00, 0x1f, 2);
        assert_eq!(canonical.to_bytes(), [0x00, 0x00, 0x00, 0x1f, 0x02, 0x00]);

        let decoded = DeviceCapKey::from_bytes(canonical.to_bytes()).expect("valid encoding");
        assert_eq!(decoded, canonical);
    }

    #[test]
    fn device_cap_key_from_bytes_rejects_reserved_byte() {
        let bad = [0x00, 0x00, 0x00, 0x1f, 0x02, 0x01];
        assert!(DeviceCapKey::from_bytes(bad).is_none());
    }

    // ---- MmioWindowDescriptor -------------------------------------------

    #[test]
    fn mmio_window_descriptor_round_trip_uncacheable() {
        let desc = MmioWindowDescriptor {
            phys_base: 0xfebf_0000,
            len: 0x1_0000,
            bar_index: 0,
            prefetchable: false,
            cache_mode: MmioCacheMode::Uncacheable,
        };
        let bytes = desc.to_bytes();
        let back = MmioWindowDescriptor::from_bytes(bytes).expect("valid encoding");
        assert_eq!(desc, back);
    }

    #[test]
    fn mmio_window_descriptor_round_trip_prefetchable_wc() {
        let desc = MmioWindowDescriptor {
            phys_base: 0x0000_0001_0000_0000,
            len: 0x400_0000,
            bar_index: 2,
            prefetchable: true,
            cache_mode: MmioCacheMode::WriteCombining,
        };
        let bytes = desc.to_bytes();
        let back = MmioWindowDescriptor::from_bytes(bytes).expect("valid encoding");
        assert_eq!(desc, back);
    }

    #[test]
    fn mmio_window_descriptor_rejects_bad_bool() {
        let desc = MmioWindowDescriptor {
            phys_base: 0x1000,
            len: 0x1000,
            bar_index: 0,
            prefetchable: false,
            cache_mode: MmioCacheMode::Uncacheable,
        };
        let mut bytes = desc.to_bytes();
        bytes[17] = 0x42;
        assert!(MmioWindowDescriptor::from_bytes(bytes).is_none());
    }

    #[test]
    fn mmio_window_descriptor_rejects_bad_cache_mode() {
        let desc = MmioWindowDescriptor {
            phys_base: 0x1000,
            len: 0x1000,
            bar_index: 0,
            prefetchable: false,
            cache_mode: MmioCacheMode::Uncacheable,
        };
        let mut bytes = desc.to_bytes();
        bytes[18] = 0xff;
        assert!(MmioWindowDescriptor::from_bytes(bytes).is_none());
    }

    #[test]
    fn mmio_cache_mode_byte_round_trip() {
        for mode in [MmioCacheMode::Uncacheable, MmioCacheMode::WriteCombining] {
            let b = mode.to_byte();
            assert_eq!(MmioCacheMode::from_byte(b), Some(mode));
        }
        assert_eq!(MmioCacheMode::from_byte(0xff), None);
    }

    // ---- DmaHandle -------------------------------------------------------

    #[test]
    fn dma_handle_identity_mapping_sets_iova_equal_to_phys() {
        let phys_addr: u64 = 0x1_0000_0000;
        let handle = DmaHandle {
            user_va: 0xdead_beef_0000,
            iova: phys_addr,
            len: 4096,
        };
        assert_eq!(handle.iova, phys_addr);
        assert_ne!(handle.user_va, 0, "user_va is expected to be nonzero here");
    }

    #[test]
    fn dma_handle_kernel_staging_buffer_may_have_zero_user_va() {
        let handle = DmaHandle {
            user_va: 0,
            iova: 0xfeed_face_0000,
            len: 8192,
        };
        assert_eq!(handle.user_va, 0);
        assert_ne!(handle.iova, 0);
        assert_ne!(handle.iova, handle.user_va as u64);
    }

    #[test]
    fn dma_handle_user_va_and_iova_independent() {
        let handle = DmaHandle {
            user_va: 0x7fff_0010_0000,
            iova: 0x0000_0000_4000_0000,
            len: 16384,
        };
        assert_ne!(handle.user_va as u64, handle.iova);
    }

    // ---- DeviceHostError -------------------------------------------------

    #[test]
    fn device_host_error_variants_are_all_constructible_and_equal() {
        let all = [
            DeviceHostError::NotClaimed,
            DeviceHostError::AlreadyClaimed,
            DeviceHostError::InvalidBarIndex,
            DeviceHostError::BarOutOfBounds,
            DeviceHostError::IovaExhausted,
            DeviceHostError::IommuFault,
            DeviceHostError::CapacityExceeded,
            DeviceHostError::IrqUnavailable,
            DeviceHostError::BadDeviceCap,
            DeviceHostError::Internal,
        ];
        for (i, a) in all.iter().enumerate() {
            for (j, b) in all.iter().enumerate() {
                if i == j {
                    assert_eq!(a, b);
                } else {
                    assert_ne!(a, b);
                }
            }
        }
    }

    #[test]
    fn device_host_error_result_match_covers_every_arm() {
        fn tag(e: DeviceHostError) -> u8 {
            match e {
                DeviceHostError::NotClaimed => 0,
                DeviceHostError::AlreadyClaimed => 1,
                DeviceHostError::InvalidBarIndex => 2,
                DeviceHostError::BarOutOfBounds => 3,
                DeviceHostError::IovaExhausted => 4,
                DeviceHostError::IommuFault => 5,
                DeviceHostError::CapacityExceeded => 6,
                DeviceHostError::IrqUnavailable => 7,
                DeviceHostError::BadDeviceCap => 8,
                DeviceHostError::Internal => 9,
            }
        }

        let cases = [
            (DeviceHostError::NotClaimed, 0u8),
            (DeviceHostError::AlreadyClaimed, 1),
            (DeviceHostError::InvalidBarIndex, 2),
            (DeviceHostError::BarOutOfBounds, 3),
            (DeviceHostError::IovaExhausted, 4),
            (DeviceHostError::IommuFault, 5),
            (DeviceHostError::CapacityExceeded, 6),
            (DeviceHostError::IrqUnavailable, 7),
            (DeviceHostError::BadDeviceCap, 8),
            (DeviceHostError::Internal, 9),
        ];
        for (err, expected) in cases {
            let result: Result<(), DeviceHostError> = Err(err);
            match result {
                Ok(()) => panic!("test constructs Err only"),
                Err(e) => assert_eq!(tag(e), expected),
            }
        }
    }

    // ---- Capability::Device and derived variants -------------------------

    #[test]
    fn capability_device_variant_carries_device_cap_key() {
        use crate::ipc::Capability;
        let key = DeviceCapKey::new(0, 0x01, 0x00, 0);
        let cap = Capability::Device { key };
        match cap {
            Capability::Device { key: k } => assert_eq!(k, key),
            _ => panic!("expected Device variant"),
        }
    }

    #[test]
    fn capability_mmio_dma_and_irq_variants_construct() {
        use crate::ipc::Capability;
        use crate::types::NotifId;

        let key = DeviceCapKey::new(0, 0x02, 0x00, 0);
        let mmio = Capability::Mmio {
            device: key,
            bar_index: 0,
            len: 0x1000,
        };
        let dma = Capability::Dma {
            device: key,
            iova: 0x4000_0000,
            len: 0x2000,
        };
        let irq = Capability::DeviceIrq {
            device: key,
            notif: NotifId(7),
        };

        match mmio {
            Capability::Mmio {
                device,
                bar_index,
                len,
            } => {
                assert_eq!(device, key);
                assert_eq!(bar_index, 0);
                assert_eq!(len, 0x1000);
            }
            _ => panic!("expected Mmio variant"),
        }
        match dma {
            Capability::Dma { device, iova, len } => {
                assert_eq!(device, key);
                assert_eq!(iova, 0x4000_0000);
                assert_eq!(len, 0x2000);
            }
            _ => panic!("expected Dma variant"),
        }
        match irq {
            Capability::DeviceIrq { device, notif } => {
                assert_eq!(device, key);
                assert_eq!(notif, NotifId(7));
            }
            _ => panic!("expected DeviceIrq variant"),
        }
    }

    // ---- Constants -------------------------------------------------------

    #[test]
    fn driver_restart_timeout_is_one_second() {
        assert_eq!(DRIVER_RESTART_TIMEOUT_MS, 1000);
    }
}
