//! PCI config-space read validation — Phase 79 Track A.1.
//!
//! Pure-logic validation for the `sys_device_config_read` syscall, which lets
//! an authorized ring-3 driver read a device's PCI configuration space by raw
//! BDF (before claiming it) so NIC drivers can match on vendor:device ID and
//! decide which family driver should claim a given function.
//!
//! Placed in `kernel-core` so the width / alignment / bounds rules are
//! host-testable without a real PCI bus.

/// Error variants for a rejected config-space read.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ConfigReadError {
    /// `width` was not 1, 2, or 4.
    BadWidth,
    /// `offset` is not naturally aligned to `width`.
    Misaligned,
    /// `offset + width` exceeds the 256-byte legacy config space.
    OutOfRange,
}

/// The legacy PCI configuration space is 256 bytes per function.
pub const PCI_CONFIG_SPACE_LEN: u16 = 256;

/// Validate a config-space read request.
///
/// * `width` must be 1, 2, or 4.
/// * `offset` must be naturally aligned to `width` (a hardware requirement of
///   the CONFIG_ADDRESS/CONFIG_DATA dword-aligned access path).
/// * `offset + width` must be `<= 256`.
///
/// Returns `Ok(())` when the access is well-formed.
pub fn validate_config_read(offset: u16, width: u8) -> Result<(), ConfigReadError> {
    if width != 1 && width != 2 && width != 4 {
        return Err(ConfigReadError::BadWidth);
    }
    if !offset.is_multiple_of(width as u16) {
        return Err(ConfigReadError::Misaligned);
    }
    if offset as u32 + width as u32 > PCI_CONFIG_SPACE_LEN as u32 {
        return Err(ConfigReadError::OutOfRange);
    }
    Ok(())
}

/// Mask a raw 32-bit dword read from config space down to the requested
/// `width` bytes starting at `offset`.
///
/// The kernel reads aligned 32-bit dwords; this extracts the little-endian
/// sub-field the caller asked for. `width` must already have been validated.
#[inline]
pub fn extract_field(dword: u32, offset: u16, width: u8) -> u32 {
    match width {
        1 => (dword >> ((offset & 0x3) * 8)) & 0xFF,
        2 => (dword >> ((offset & 0x2) * 8)) & 0xFFFF,
        _ => dword,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_bad_width() {
        assert_eq!(validate_config_read(0, 3), Err(ConfigReadError::BadWidth));
        assert_eq!(validate_config_read(0, 0), Err(ConfigReadError::BadWidth));
        assert_eq!(validate_config_read(0, 8), Err(ConfigReadError::BadWidth));
    }

    #[test]
    fn accepts_aligned_widths() {
        assert_eq!(validate_config_read(0x00, 4), Ok(())); // vendor:device dword
        assert_eq!(validate_config_read(0x00, 2), Ok(())); // vendor id
        assert_eq!(validate_config_read(0x02, 2), Ok(())); // device id
        assert_eq!(validate_config_read(0x08, 1), Ok(())); // revision id
    }

    #[test]
    fn rejects_misaligned() {
        assert_eq!(
            validate_config_read(0x01, 2),
            Err(ConfigReadError::Misaligned)
        );
        assert_eq!(
            validate_config_read(0x02, 4),
            Err(ConfigReadError::Misaligned)
        );
    }

    #[test]
    fn rejects_out_of_range() {
        // Naturally aligned but past the 256-byte legacy config space.
        assert_eq!(
            validate_config_read(0x100, 4),
            Err(ConfigReadError::OutOfRange)
        );
        assert_eq!(
            validate_config_read(0x100, 2),
            Err(ConfigReadError::OutOfRange)
        );
        assert_eq!(
            validate_config_read(0x100, 1),
            Err(ConfigReadError::OutOfRange)
        );
    }

    #[test]
    fn last_dword_in_bounds() {
        assert_eq!(validate_config_read(0xFC, 4), Ok(()));
        assert_eq!(validate_config_read(0xFF, 1), Ok(()));
    }

    #[test]
    fn extract_field_picks_correct_subfield() {
        // QEMU e1000e: vendor 0x8086, device 0x10D3 → dword 0x10D3_8086.
        let dword = 0x10D3_8086u32;
        assert_eq!(extract_field(dword, 0x00, 2), 0x8086); // vendor
        assert_eq!(extract_field(dword, 0x02, 2), 0x10D3); // device
        assert_eq!(extract_field(dword, 0x00, 4), 0x10D3_8086);
        assert_eq!(extract_field(dword, 0x00, 1), 0x86);
        assert_eq!(extract_field(dword, 0x01, 1), 0x80);
    }
}
