//! AMD/ATI HDA controller config-space quirk — Phase 80c Track F.1.
//!
//! Host-testable pure logic for the one AMD-specific PCI config-space write the
//! `hda_driver` performs: enabling the ATI/AMD **snoop** bit so HDA DMA is
//! cache-coherent. This mirrors Linux `snd_hda_intel`'s `azx_init_pci`, which
//! for an `AZX_SNOOP_TYPE_ATI` controller does
//! `update_pci_byte(pci, 0x42, 0x07, 0x02)`.
//!
//! ## Important scope note
//!
//! The snoop write affects **DMA cache coherency only** — it is *not* what
//! makes a codec appear in `STATESTS`. Codec enumeration is gated by the
//! controller reset timing (the in-reset PLL-settle delay + post-CRST codec
//! window) and, under VFIO, by whether the codec block is powered (see
//! [`crate::device_host::pci_pm`] for the D0 force). Without this snoop write a
//! codec still enumerates; playback would just be incoherent/garbled. It is
//! applied so audio is correct once the codec is up, not to fix enumeration.
//!
//! m3OS ships **no** kernel HDA quirk table — this is the single vendor config
//! write the ring-3 driver issues, keyed off the AMD PCI vendor ID **and** the
//! HDA class/subclass (see [`is_amd_hda_controller`]). The class check keeps the
//! kernel's config-write snoop-byte allowlist scoped to AMD *HDA* controllers
//! (least privilege): a non-HDA AMD function the caller might own could
//! interpret config offset `0x42` differently.

/// AMD PCI vendor ID. The dev-laptop HDA controller is `1022:15e3`.
pub const AMD_VENDOR_ID: u16 = 0x1022;

/// `ATI_SB450_HDAUDIO_MISC_CNTR2_ADDR` — the config-space byte carrying the
/// ATI/AMD HDA snoop-enable field.
pub const ATI_SNOOP_REG: u8 = 0x42;
/// Mask of the snoop field within [`ATI_SNOOP_REG`] (low 3 bits).
pub const ATI_SNOOP_MASK: u8 = 0x07;
/// `ATI_SB450_HDAUDIO_ENABLE_SNOOP` — the value written into the masked field
/// to enable snoop (cache-coherent DMA).
pub const ATI_SNOOP_ENABLE: u8 = 0x02;

/// Whether `vendor` identifies an AMD/ATI HDA controller needing the snoop
/// quirk. (QEMU's emulated `intel-hda` is vendor `0x8086` and is unaffected,
/// so the QEMU `hda-smoke` gate sees no behaviour change.)
#[inline]
pub fn is_amd_controller(vendor: u16) -> bool {
    vendor == AMD_VENDOR_ID
}

/// Whether the device at `(vendor, base_class, subclass)` is an AMD/ATI **HDA
/// controller** eligible for the snoop config-space write — i.e. AMD vendor AND
/// PCI class [`HDA_CLASS`](crate::hda::ids::HDA_CLASS) / subclass
/// [`HDA_SUBCLASS`](crate::hda::ids::HDA_SUBCLASS). The kernel's
/// `sys_device_config_write` allowlist uses this so the snoop byte (`0x42`) is
/// writable only on AMD HDA controllers, not every AMD function a driver might
/// own — vendor alone would be too broad for least privilege.
#[inline]
pub fn is_amd_hda_controller(vendor: u16, base_class: u8, subclass: u8) -> bool {
    is_amd_controller(vendor)
        && base_class == crate::hda::ids::HDA_CLASS
        && subclass == crate::hda::ids::HDA_SUBCLASS
}

/// Read-modify-write computation for the ATI/AMD snoop byte: clear the low 3
/// bits and set them to [`ATI_SNOOP_ENABLE`]. Mirrors Linux
/// `update_pci_byte(pci, 0x42, 0x07, 0x02)`.
#[inline]
pub fn ati_snoop_rmw(current: u8) -> u8 {
    (current & !ATI_SNOOP_MASK) | ATI_SNOOP_ENABLE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_amd_vendor() {
        assert!(is_amd_controller(0x1022)); // AMD
        assert!(!is_amd_controller(0x8086)); // Intel (QEMU intel-hda)
        assert!(!is_amd_controller(0x10ec)); // Realtek (codec, not controller)
    }

    #[test]
    fn amd_hda_controller_requires_vendor_and_hda_class() {
        use crate::hda::ids::{HDA_CLASS, HDA_SUBCLASS};
        // AMD HDA controller (1022:15e3 is class 0x04 / subclass 0x03) — eligible.
        assert!(is_amd_hda_controller(0x1022, HDA_CLASS, HDA_SUBCLASS));
        // AMD vendor but NOT an HDA controller — the snoop byte must NOT be
        // allowlisted (offset 0x42 could mean something else on this function).
        assert!(!is_amd_hda_controller(0x1022, 0x04, 0x01)); // AMD AC'97-class audio
        assert!(!is_amd_hda_controller(0x1022, 0x02, 0x00)); // AMD network controller
        assert!(!is_amd_hda_controller(0x1022, 0x01, 0x06)); // AMD SATA controller
        // HDA class but non-AMD vendor — handled by the Intel path, not snoop.
        assert!(!is_amd_hda_controller(0x8086, HDA_CLASS, HDA_SUBCLASS)); // QEMU intel-hda
    }

    #[test]
    fn snoop_rmw_sets_low_three_bits_to_enable() {
        // From cleared, enable snoop.
        assert_eq!(ati_snoop_rmw(0x00), 0x02);
        // High bits are preserved; only the low 3 are rewritten.
        assert_eq!(ati_snoop_rmw(0xF0), 0xF2);
        // A stale snoop field (e.g. 0x07) is overwritten, not OR'd.
        assert_eq!(ati_snoop_rmw(0x07), 0x02);
        assert_eq!(ati_snoop_rmw(0xFF), 0xFA);
        // Idempotent: re-applying to an already-enabled byte is a no-op.
        assert_eq!(ati_snoop_rmw(ati_snoop_rmw(0x55)), ati_snoop_rmw(0x55));
    }
}
