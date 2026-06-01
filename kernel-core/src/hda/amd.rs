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
//! write the ring-3 driver issues, keyed only off the AMD PCI vendor ID.

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
