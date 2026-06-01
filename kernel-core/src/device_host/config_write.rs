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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_well_formed_writes() {
        assert_eq!(validate_config_write(0x42, 1, 0x02), Ok(())); // AMD snoop byte
        assert_eq!(validate_config_write(0x04, 2, 0x0006), Ok(())); // command reg
        assert_eq!(validate_config_write(0x10, 4, 0xDEAD_BEEF), Ok(())); // BAR0
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
}
