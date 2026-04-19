// Phase 55b Track A.1 — failing-test commit.
//
// Stubs are present so the crate compiles, but their runtime behaviour is
// intentionally wrong. The Red half of Red → Green: the tests below exercise
// the real expected behaviour and therefore fail assertion. The follow-up
// implementation commit replaces these stubs with the correct logic.

/// Bus/Device/Function identifier for a PCI(e) endpoint, plus its PCI segment.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct DeviceCapKey {
    pub segment: u16,
    pub bus: u8,
    pub dev: u8,
    pub func: u8,
}

impl DeviceCapKey {
    pub const fn new(segment: u16, bus: u8, dev: u8, func: u8) -> Self {
        Self {
            segment,
            bus,
            dev,
            func,
        }
    }

    // STUB: wrong encoding so the round-trip test fails.
    pub const fn to_bytes(self) -> [u8; 6] {
        [0; 6]
    }

    // STUB: always fails to decode so the decode-success test fails.
    pub const fn from_bytes(_bytes: [u8; 6]) -> Option<Self> {
        None
    }
}

#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[non_exhaustive]
pub enum MmioCacheMode {
    Uncacheable = 0,
    WriteCombining = 1,
}

impl MmioCacheMode {
    // STUB: returns 0 for both — fails the round-trip distinction.
    pub const fn to_byte(self) -> u8 {
        0
    }

    pub const fn from_byte(_b: u8) -> Option<Self> {
        None
    }
}

#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct MmioWindowDescriptor {
    pub phys_base: u64,
    pub len: usize,
    pub bar_index: u8,
    pub prefetchable: bool,
    pub cache_mode: MmioCacheMode,
}

pub const MMIO_WINDOW_DESCRIPTOR_SIZE: usize = 20;

impl MmioWindowDescriptor {
    // STUB: always zero — round-trip fails.
    pub fn to_bytes(self) -> [u8; MMIO_WINDOW_DESCRIPTOR_SIZE] {
        [0; MMIO_WINDOW_DESCRIPTOR_SIZE]
    }

    // STUB: always None — decode fails.
    pub fn from_bytes(_bytes: [u8; MMIO_WINDOW_DESCRIPTOR_SIZE]) -> Option<Self> {
        None
    }
}

#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DmaHandle {
    pub user_va: usize,
    pub iova: u64,
    pub len: usize,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum DeviceHostError {
    NotClaimed,
    AlreadyClaimed,
    InvalidBarIndex,
    BarOutOfBounds,
    IovaExhausted,
    IommuFault,
    CapacityExceeded,
    IrqUnavailable,
    BadDeviceCap,
    Internal,
}

// STUB: wrong value so the timeout assertion fails.
pub const DRIVER_RESTART_TIMEOUT_MS: u32 = 0;

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
