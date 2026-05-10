//! Pure-logic validation for `sys_device_pio_read` / `sys_device_pio_write`
//! — Phase 63 Track Z.2.
//!
//! These helpers are no_std + alloc-free and capture all the business-logic
//! checks that the kernel-side PIO syscall handlers must perform. Keeping
//! the logic here (rather than inlined into the kernel) makes it host-
//! testable via `cargo test -p kernel-core` without a QEMU instance.
//!
//! # Validation order (matches Z.2 acceptance)
//!
//! 1. `width` ∈ {1, 2, 4} — any other value → `-EINVAL` represented as
//!    [`PioValidationError::InvalidWidth`].
//! 2. `bar_is_pio` must be `true` — MMIO BARs are rejected →
//!    [`PioValidationError::NotPioBar`] (maps to `-EINVAL`).
//! 3. `offset + width ≤ bar_size` — out-of-range →
//!    [`PioValidationError::OffsetOutOfRange`] (maps to `-ERANGE`).
//!
//! The kernel syscall handler performs capability validation (−EBADF) and
//! ownership checks before calling these helpers; only the numeric checks
//! belong here.

/// Errors returned by [`validate_pio_access`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PioValidationError {
    /// `width` was not one of the supported values (1, 2, or 4).
    InvalidWidth,
    /// The BAR at `bar_index` is an MMIO BAR, not a PIO BAR.
    NotPioBar,
    /// `offset + width > bar_size`.
    OffsetOutOfRange,
}

/// Validate a PIO access before issuing the port I/O instruction.
///
/// # Arguments
///
/// * `width` — requested transfer width in bytes (must be 1, 2, or 4).
/// * `bar_is_pio` — whether the BAR at the requested index is PIO-typed.
/// * `offset` — byte offset within the BAR.
/// * `bar_size` — total byte size of the BAR as reported by PCI config space.
///
/// # Returns
///
/// * `Ok(())` when all checks pass — the caller may proceed with port I/O.
/// * `Err(PioValidationError)` describing the first failing check.
#[inline]
pub const fn validate_pio_access(
    width: u8,
    bar_is_pio: bool,
    offset: u32,
    bar_size: u32,
) -> Result<(), PioValidationError> {
    // 1. Width check first — a malformed width is always wrong regardless of
    //    whether the BAR exists or what the offset is.
    if width != 1 && width != 2 && width != 4 {
        return Err(PioValidationError::InvalidWidth);
    }

    // 2. BAR-type check — MMIO BARs must not be accessed via PIO syscalls.
    if !bar_is_pio {
        return Err(PioValidationError::NotPioBar);
    }

    // 3. Range check — `offset + width` must not overflow and must be ≤ bar_size.
    let end = match offset.checked_add(width as u32) {
        Some(e) => e,
        None => return Err(PioValidationError::OffsetOutOfRange),
    };
    if end > bar_size {
        return Err(PioValidationError::OffsetOutOfRange);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Width validation tests
    // -----------------------------------------------------------------------

    #[test]
    fn valid_8_bit_access_passes() {
        assert_eq!(validate_pio_access(1, true, 0, 64), Ok(()));
    }

    #[test]
    fn valid_16_bit_access_passes() {
        assert_eq!(validate_pio_access(2, true, 0, 64), Ok(()));
    }

    #[test]
    fn valid_32_bit_access_passes() {
        assert_eq!(validate_pio_access(4, true, 0, 64), Ok(()));
    }

    #[test]
    fn width_zero_is_invalid() {
        assert_eq!(
            validate_pio_access(0, true, 0, 64),
            Err(PioValidationError::InvalidWidth)
        );
    }

    #[test]
    fn width_three_is_invalid() {
        assert_eq!(
            validate_pio_access(3, true, 0, 64),
            Err(PioValidationError::InvalidWidth)
        );
    }

    #[test]
    fn width_eight_is_invalid() {
        // 64-bit accesses are not supported by AC'97 PIO BARs.
        assert_eq!(
            validate_pio_access(8, true, 0, 64),
            Err(PioValidationError::InvalidWidth)
        );
    }

    // -----------------------------------------------------------------------
    // BAR-type validation tests
    // -----------------------------------------------------------------------

    #[test]
    fn mmio_bar_is_rejected() {
        assert_eq!(
            validate_pio_access(1, false, 0, 64),
            Err(PioValidationError::NotPioBar)
        );
    }

    #[test]
    fn mmio_bar_rejection_takes_priority_over_range_check() {
        // Even a valid offset should not slip through an MMIO BAR.
        assert_eq!(
            validate_pio_access(4, false, 60, 64),
            Err(PioValidationError::NotPioBar)
        );
    }

    // -----------------------------------------------------------------------
    // Range validation tests
    // -----------------------------------------------------------------------

    #[test]
    fn offset_at_last_valid_byte_passes() {
        // 8-bit read at offset 63 of a 64-byte BAR: offset+width = 64 == bar_size.
        assert_eq!(validate_pio_access(1, true, 63, 64), Ok(()));
    }

    #[test]
    fn offset_exactly_at_end_is_out_of_range() {
        // 8-bit read at offset 64 of a 64-byte BAR: offset+width = 65 > bar_size.
        assert_eq!(
            validate_pio_access(1, true, 64, 64),
            Err(PioValidationError::OffsetOutOfRange)
        );
    }

    #[test]
    fn u32_read_at_last_valid_u32_offset_passes() {
        // 32-bit read at offset 60 of a 64-byte BAR: 60 + 4 = 64 == bar_size.
        assert_eq!(validate_pio_access(4, true, 60, 64), Ok(()));
    }

    #[test]
    fn u32_read_past_end_is_out_of_range() {
        // 32-bit read at offset 61 of a 64-byte BAR: 61 + 4 = 65 > bar_size.
        assert_eq!(
            validate_pio_access(4, true, 61, 64),
            Err(PioValidationError::OffsetOutOfRange)
        );
    }

    #[test]
    fn width_check_precedes_bar_type_check() {
        // An invalid width should be caught before the BAR type is inspected.
        // This exercises the validation order contract.
        assert_eq!(
            validate_pio_access(3, false, 0, 64),
            Err(PioValidationError::InvalidWidth)
        );
    }

    #[test]
    fn bar_type_check_precedes_range_check() {
        // MMIO BAR rejection should be caught before offset range is checked.
        // Even with an obviously out-of-range offset, NotPioBar should win.
        assert_eq!(
            validate_pio_access(4, false, 1000, 64),
            Err(PioValidationError::NotPioBar)
        );
    }

    #[test]
    fn offset_overflow_is_out_of_range() {
        // u32::MAX + 4 overflows — must map to OffsetOutOfRange, not panic.
        assert_eq!(
            validate_pio_access(4, true, u32::MAX, u32::MAX),
            Err(PioValidationError::OffsetOutOfRange)
        );
    }
}
