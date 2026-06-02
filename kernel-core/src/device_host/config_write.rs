//! PCI config-space write validation — Phase 80c Track F.1.
//!
//! Pure-logic validation for the `sys_device_config_write` syscall, which lets
//! an authorized ring-3 driver write its **already-claimed** device's PCI
//! configuration space. The motivating use is AMD HDA snoop enablement (see
//! [`crate::hda::amd`]): the controller's codec does not enumerate until a
//! vendor snoop bit is set in config space, and m3OS ships no kernel HDA quirk
//! table, so the ring-3 driver performs the write itself.
//!
//! The offset/width/alignment rules are identical to a config-space *read*
//! ([`super::config_read`]); a write adds one rule the read does not need: the
//! `value` must fit within `width` bytes. Placed in `kernel-core` so the rules
//! are host-testable without a real PCI bus.

use super::config_read::{ConfigReadError, validate_config_read};

/// Error variants for a rejected config-space write.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ConfigWriteError {
    /// `width` was not 1, 2, or 4.
    BadWidth,
    /// `offset` is not naturally aligned to `width`.
    Misaligned,
    /// `offset + width` exceeds the 256-byte legacy config space.
    OutOfRange,
    /// `value` has bits set outside the low `width` bytes.
    ValueTooWide,
}

impl From<ConfigReadError> for ConfigWriteError {
    fn from(e: ConfigReadError) -> Self {
        match e {
            ConfigReadError::BadWidth => ConfigWriteError::BadWidth,
            ConfigReadError::Misaligned => ConfigWriteError::Misaligned,
            ConfigReadError::OutOfRange => ConfigWriteError::OutOfRange,
        }
    }
}

/// Validate a config-space write request.
///
/// * `width` must be 1, 2, or 4 (same as a read).
/// * `offset` must be naturally aligned to `width` (same as a read).
/// * `offset + width` must be `<= 256` (same as a read).
/// * `value` must fit in `width` bytes — a width-1 write may not carry bits
///   above `0xFF`, etc. This is the only rule a write adds over a read; it
///   guards against a caller silently truncating (or, on a read-modify-write
///   path, corrupting an unintended neighbouring byte).
///
/// Returns `Ok(())` when the access is well-formed.
pub fn validate_config_write(offset: u16, width: u8, value: u32) -> Result<(), ConfigWriteError> {
    validate_config_read(offset, width)?;
    // `width` is now known to be 1, 2, or 4. A width-4 write accepts any u32.
    if width < 4 {
        let max = (1u32 << (width as u32 * 8)) - 1;
        if value > max {
            return Err(ConfigWriteError::ValueTooWide);
        }
    }
    Ok(())
}

/// Policy gate: which config-space offsets the owning ring-3 driver of a
/// *claimed* device may **write**. This is far narrower than the
/// well-formedness check in [`validate_config_write`] — being well-formed only
/// proves the access fits config space, not that the kernel will allow it.
///
/// A claimed device's BARs/DMA are within the driver's authority, but **PCI
/// interrupt routing is not**: the kernel programs a device's MSI/MSI-X message
/// address + data (`MsiCapability::program_single`) to deliver a kernel-chosen
/// IDT vector to a chosen LAPIC. With interrupt remapping not engaged, letting a
/// ring-3 driver write the MSI/MSI-X capability would let it retarget its
/// device's interrupt to an arbitrary vector/LAPIC — forging an interrupt the
/// kernel never armed. Likewise a post-claim BAR rewrite would relocate where
/// the device decodes host MMIO, desyncing the kernel's claim-time IOMMU
/// BAR-coverage and user MMIO mapping; and a Command-register write could clear
/// Bus Master / Memory Space out from under the claim/drop ordering invariant.
///
/// So the gate is an allowlist, not a blocklist: only the two writes the ring-3
/// HDA driver legitimately needs (the generic register path cannot express
/// them) are permitted, and everything else — MSI/MSI-X, BARs, Command, the
/// capability pointer, every other capability structure — is denied.
///
/// * `pmcsr_offset` — config offset of the device's Power-Management Control /
///   Status Register, if it has a PM capability. A width-2 write here forces
///   PCI power state D0 during bring-up. `None` when the device has no PM cap.
/// * `vendor_byte_offset` — config offset of the single vendor-specific byte the
///   driver is permitted to write (the AMD/ATI HDA snoop byte at `0x42`), or
///   `None` for devices with no sanctioned vendor write. A width-1 write only.
///
/// Returns `true` only for an exact `(offset, width)` match against one of the
/// permitted writes. The caller has already run [`validate_config_write`].
pub fn config_write_permitted(
    offset: u16,
    width: u8,
    pmcsr_offset: Option<u16>,
    vendor_byte_offset: Option<u16>,
) -> bool {
    // PMCSR: a 16-bit write at the PM capability's control register.
    if width == 2 && pmcsr_offset == Some(offset) {
        return true;
    }
    // The one sanctioned vendor byte (AMD/ATI HDA snoop): a 1-byte write.
    if width == 1 && vendor_byte_offset == Some(offset) {
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_well_formed_writes() {
        // Well-formedness only — the kernel-side `config_write_permitted`
        // allowlist separately *denies* the command register and BARs by
        // policy; these assertions just confirm the access shape is valid.
        assert_eq!(validate_config_write(0x42, 1, 0x02), Ok(())); // AMD snoop byte
        assert_eq!(validate_config_write(0x04, 2, 0x0006), Ok(())); // command reg (shape ok)
        assert_eq!(validate_config_write(0x10, 4, 0xDEAD_BEEF), Ok(())); // BAR0 (shape ok)
    }

    #[test]
    fn rejects_bad_width() {
        assert_eq!(
            validate_config_write(0, 3, 0),
            Err(ConfigWriteError::BadWidth)
        );
        assert_eq!(
            validate_config_write(0, 0, 0),
            Err(ConfigWriteError::BadWidth)
        );
    }

    #[test]
    fn rejects_misaligned() {
        assert_eq!(
            validate_config_write(0x01, 2, 0),
            Err(ConfigWriteError::Misaligned)
        );
        assert_eq!(
            validate_config_write(0x02, 4, 0),
            Err(ConfigWriteError::Misaligned)
        );
    }

    #[test]
    fn rejects_out_of_range() {
        assert_eq!(
            validate_config_write(0x100, 1, 0),
            Err(ConfigWriteError::OutOfRange)
        );
        // 0x100 is 4-aligned but 0x100 + 4 > 256.
        assert_eq!(
            validate_config_write(0x100, 4, 0),
            Err(ConfigWriteError::OutOfRange)
        );
    }

    #[test]
    fn rejects_value_wider_than_width() {
        // width 1 cannot carry bits above 0xFF.
        assert_eq!(
            validate_config_write(0x42, 1, 0x100),
            Err(ConfigWriteError::ValueTooWide)
        );
        // width 2 cannot carry bits above 0xFFFF.
        assert_eq!(
            validate_config_write(0x04, 2, 0x1_0000),
            Err(ConfigWriteError::ValueTooWide)
        );
        // boundary values are accepted.
        assert_eq!(validate_config_write(0x42, 1, 0xFF), Ok(()));
        assert_eq!(validate_config_write(0x04, 2, 0xFFFF), Ok(()));
        // width 4 accepts the full u32 range.
        assert_eq!(validate_config_write(0x10, 4, u32::MAX), Ok(()));
    }

    #[test]
    fn allowlist_permits_pmcsr_force_d0() {
        // PM cap at 0x60 → PMCSR at 0x64 (pm_cap + 4). Width-2 write allowed.
        assert!(config_write_permitted(0x64, 2, Some(0x64), None));
        // Wrong width at the PMCSR offset is denied.
        assert!(!config_write_permitted(0x64, 1, Some(0x64), None));
        assert!(!config_write_permitted(0x64, 4, Some(0x64), None));
        // A device with no PM capability cannot write a PMCSR.
        assert!(!config_write_permitted(0x64, 2, None, None));
    }

    #[test]
    fn allowlist_permits_amd_snoop_byte() {
        // AMD/ATI HDA snoop byte at 0x42, width 1.
        assert!(config_write_permitted(0x42, 1, None, Some(0x42)));
        // Wrong width is denied even at the sanctioned offset.
        assert!(!config_write_permitted(0x42, 2, None, Some(0x42)));
        // Non-AMD device (no sanctioned vendor write) is denied.
        assert!(!config_write_permitted(0x42, 1, None, None));
    }

    #[test]
    fn allowlist_denies_security_critical_registers() {
        // Even a well-formed write to these is rejected by policy. PM cap at
        // 0x60 (PMCSR 0x64), AMD snoop sanctioned at 0x42.
        let pmcsr = Some(0x64u16);
        let snoop = Some(0x42u16);
        // Command register (0x04).
        assert!(!config_write_permitted(0x04, 2, pmcsr, snoop));
        // BAR0..BAR5 (0x10..0x27).
        for bar in [0x10u16, 0x14, 0x18, 0x1C, 0x20, 0x24] {
            assert!(!config_write_permitted(bar, 4, pmcsr, snoop));
        }
        // Capabilities pointer (0x34).
        assert!(!config_write_permitted(0x34, 1, pmcsr, snoop));
        // A hypothetical MSI capability message-address/data (e.g. at 0x50/0x54)
        // — the interrupt-forging vector the allowlist exists to close.
        assert!(!config_write_permitted(0x50, 4, pmcsr, snoop));
        assert!(!config_write_permitted(0x54, 4, pmcsr, snoop));
        // The PMCSR's own offset at the wrong width is still denied.
        assert!(!config_write_permitted(0x64, 4, pmcsr, snoop));
    }
}
