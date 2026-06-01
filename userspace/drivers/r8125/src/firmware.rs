//! r8125 firmware-load path (Track D.1).
//!
//! 8168G-and-later and all 8125/8126 parts need a signed PHY-firmware blob
//! (`rtl_nic/*.fw`) to link reliably. Blobs are **not** vendored — the
//! coordinator (E.2) stages them from host `linux-firmware` at image-build time.
//! This module turns "here are the blob bytes (or there are none)" into a
//! decision + a degraded-link warning **sentinel string** rather than panicking,
//! satisfying the Track D.1 acceptance "skip with a degraded-link warning
//! sentinel rather than panicking."
//!
//! The structural blob validation itself lives in `kernel_core::r8169`
//! (`validate_firmware_header` / `resolve_firmware`), which is host-tested; this
//! module is the thin policy layer that maps the validation outcome to a
//! sentinel and is itself host-tested below.

use kernel_core::r8169 as hw;

/// Degraded-link warning sentinel emitted when firmware is required but the blob
/// is absent or corrupt. The driver continues (the PHY may still bring a link up
/// at a reduced/unreliable rate); it never panics. The exact spelling is
/// load-bearing for any smoke harness that greps for it.
pub const FW_DEGRADED_SENTINEL: &str = "R8125_FW:degraded:WARN\n";

/// Sentinel emitted when a valid firmware blob is staged and accepted.
pub const FW_LOADED_SENTINEL: &str = "R8125_FW:loaded:OK\n";

/// Outcome of the firmware-load policy for a given chip version + optional blob.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FirmwarePlan {
    /// The underlying validation/decision result from `kernel_core::r8169`.
    pub load: hw::FirmwareLoad,
}

impl FirmwarePlan {
    /// The sentinel string the caller should emit for this plan, or `None` when
    /// the chip needs no firmware (nothing to log).
    #[inline]
    pub fn sentinel(&self) -> Option<&'static str> {
        match self.load {
            hw::FirmwareLoad::Loaded(_) => Some(FW_LOADED_SENTINEL),
            hw::FirmwareLoad::Absent | hw::FirmwareLoad::Corrupt(_) => Some(FW_DEGRADED_SENTINEL),
            hw::FirmwareLoad::NotRequired => None,
        }
    }

    /// True when the chip will run with a degraded link (firmware required but
    /// not loadable). The driver must continue, not panic.
    #[inline]
    pub fn is_degraded(&self) -> bool {
        self.load.is_degraded()
    }
}

/// Decide the firmware plan for `version`, given an optional staged blob.
///
/// Delegates the structural validation to the host-tested
/// `kernel_core::r8169::resolve_firmware`, then wraps it as a [`FirmwarePlan`]
/// the caller can query for a sentinel. Never panics on an absent/corrupt blob.
#[inline]
pub fn plan_firmware(version: hw::MacVersion, blob: Option<&[u8]>) -> FirmwarePlan {
    FirmwarePlan {
        load: hw::resolve_firmware(version, blob),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    extern crate alloc;
    use alloc::vec;
    use alloc::vec::Vec;

    fn fake_fw(version: &str, instr: &[u32]) -> Vec<u8> {
        let mut v = vec![0u8; hw::RTL_FW_VER_SIZE];
        let vb = version.as_bytes();
        let n = vb.len().min(hw::RTL_FW_VER_SIZE);
        v[..n].copy_from_slice(&vb[..n]);
        v.extend_from_slice(&[0u8; 8]); // fw_offset + fw_reg
        for w in instr {
            v.extend_from_slice(&w.to_le_bytes());
        }
        v
    }

    #[test]
    fn not_required_chip_emits_no_sentinel() {
        // Classic GbE (no firmware) — no sentinel.
        let plan = plan_firmware(hw::MacVersion::Ver(2), None);
        assert_eq!(plan.sentinel(), None);
        assert!(!plan.is_degraded());
    }

    #[test]
    fn absent_blob_degrades_with_warning_not_panic() {
        // 8125 requires firmware; none staged -> degraded warning sentinel.
        let plan = plan_firmware(hw::MacVersion::Ver(61), None);
        assert!(plan.is_degraded());
        assert_eq!(plan.sentinel(), Some(FW_DEGRADED_SENTINEL));
    }

    #[test]
    fn corrupt_blob_degrades_with_warning_not_panic() {
        let bad = [0u8; 4]; // too short + all-NUL version
        let plan = plan_firmware(hw::MacVersion::Ver(61), Some(&bad));
        assert!(plan.is_degraded());
        assert_eq!(plan.sentinel(), Some(FW_DEGRADED_SENTINEL));
    }

    #[test]
    fn valid_blob_loads() {
        let blob = fake_fw("rtl8125a-3", &[0xAABB_CCDD, 0x1122_3344]);
        let plan = plan_firmware(hw::MacVersion::Ver(61), Some(&blob));
        assert!(!plan.is_degraded());
        assert_eq!(plan.sentinel(), Some(FW_LOADED_SENTINEL));
        assert!(matches!(plan.load, hw::FirmwareLoad::Loaded(_)));
    }

    #[test]
    fn sentinels_are_load_bearing_strings() {
        assert_eq!(FW_DEGRADED_SENTINEL, "R8125_FW:degraded:WARN\n");
        assert_eq!(FW_LOADED_SENTINEL, "R8125_FW:loaded:OK\n");
    }
}
