/// Maximum buffer size for small-copy bulk-data transfers (64 KiB).
pub const MAX_BUFFER_LEN: usize = 64 * 1024;

/// Lowest valid user-space address (pages below this are guard pages / null).
const USER_ADDR_MIN: u64 = 0x1000;

/// Upper bound of canonical user-space addresses on x86-64.
const USER_ADDR_MAX: u64 = 0x0000_8000_0000_0000;

/// Errors returned by [`validate_user_buffer`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BufferError {
    /// The address is null (zero).
    NullPointer,
    /// The address falls outside the valid user-space range.
    AddressOutOfRange,
    /// The requested length exceeds [`MAX_BUFFER_LEN`].
    LengthExceeded,
    /// `addr + len` wraps around the 64-bit address space.
    Wraparound,
}

/// Validate that a user-space buffer described by `(addr, len)` is safe for
/// the kernel to access via `copy_from_user` / `copy_to_user`.
///
/// This is a pure-logic check on the address and length values.  It does
/// **not** walk page tables — that is handled by `kernel/src/mm/user_mem.rs`.
///
/// # Rules
///
/// - Zero-length buffers are always accepted (no-op transfer).
/// - `addr` must be non-null (`> 0`).
/// - `addr` must be in the user-space canonical range (`0x1000 ..
///   0x0000_8000_0000_0000`).
/// - `len` must not exceed [`MAX_BUFFER_LEN`] (64 KiB).
/// - `addr + len` must not overflow `u64`.
pub fn validate_user_buffer(addr: u64, len: usize) -> Result<(), BufferError> {
    // Zero-length is always OK — nothing to copy.
    if len == 0 {
        return Ok(());
    }

    // Null pointer check.
    if addr == 0 {
        return Err(BufferError::NullPointer);
    }

    // Range check — must be within canonical user-space.
    if !(USER_ADDR_MIN..USER_ADDR_MAX).contains(&addr) {
        return Err(BufferError::AddressOutOfRange);
    }

    // Length check.
    if len > MAX_BUFFER_LEN {
        return Err(BufferError::LengthExceeded);
    }

    // Wraparound check.
    if addr.checked_add(len as u64).is_none() {
        return Err(BufferError::Wraparound);
    }

    // Also check that the end doesn't exceed user-space.
    if addr + len as u64 > USER_ADDR_MAX {
        return Err(BufferError::AddressOutOfRange);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_buffer_at_low_address() {
        assert_eq!(validate_user_buffer(0x1000, 4096), Ok(()));
    }

    #[test]
    fn valid_buffer_at_high_address() {
        // Just below the user-space ceiling, 1 byte.
        assert_eq!(validate_user_buffer(USER_ADDR_MAX - 1, 1), Ok(()));
    }

    #[test]
    fn valid_buffer_max_length() {
        assert_eq!(validate_user_buffer(0x10_0000, MAX_BUFFER_LEN), Ok(()));
    }

    #[test]
    fn zero_length_is_ok() {
        // Zero-length with addr 0 is fine — nothing to access.
        assert_eq!(validate_user_buffer(0, 0), Ok(()));
        // Zero-length with a valid address.
        assert_eq!(validate_user_buffer(0x2000, 0), Ok(()));
    }

    #[test]
    fn null_pointer_rejected() {
        assert_eq!(validate_user_buffer(0, 100), Err(BufferError::NullPointer));
    }

    #[test]
    fn address_below_min_rejected() {
        assert_eq!(
            validate_user_buffer(0x0FFF, 1),
            Err(BufferError::AddressOutOfRange)
        );
    }

    #[test]
    fn address_at_kernel_boundary_rejected() {
        assert_eq!(
            validate_user_buffer(USER_ADDR_MAX, 1),
            Err(BufferError::AddressOutOfRange)
        );
    }

    #[test]
    fn address_in_kernel_space_rejected() {
        assert_eq!(
            validate_user_buffer(0xFFFF_8000_0000_0000, 1),
            Err(BufferError::AddressOutOfRange)
        );
    }

    #[test]
    fn length_exceeded() {
        assert_eq!(
            validate_user_buffer(0x1000, MAX_BUFFER_LEN + 1),
            Err(BufferError::LengthExceeded)
        );
    }

    #[test]
    fn wraparound_rejected() {
        // addr near u64::MAX so addr + len wraps.
        assert_eq!(
            validate_user_buffer(u64::MAX - 10, 100),
            Err(BufferError::AddressOutOfRange) // caught by range check first
        );
    }

    #[test]
    fn end_beyond_user_space_rejected() {
        // Start is valid but start + len crosses into kernel space.
        assert_eq!(
            validate_user_buffer(USER_ADDR_MAX - 10, 20),
            Err(BufferError::AddressOutOfRange)
        );
    }

    #[test]
    fn typical_vfs_path() {
        assert_eq!(validate_user_buffer(0x0040_0000, 4096), Ok(()));
    }

    #[test]
    fn typical_network_packet() {
        assert_eq!(validate_user_buffer(0x0040_0000, 1500), Ok(()));
    }
}
