//! User-pointer range validation — pure logic, host-testable.
//!
//! Mirrors the `validate_user_range` logic from `kernel/src/mm/user_mem.rs`
//! so boundary conditions can be tested on the host without QEMU.

/// Maximum bytes accepted in a single user-copy operation.
pub const MAX_COPY_LEN: usize = 64 * 1024; // 64 KiB

/// Lowest valid user address (below this is the null-guard region).
pub const USER_ADDR_MIN: u64 = 0x1000;

/// One past the highest valid user address (start of kernel space).
pub const USER_ADDR_LIMIT: u64 = 0x0000_8000_0000_0000;

/// Validate that `[vaddr, vaddr+len)` is a legal user-space range.
///
/// Returns `Ok(())` when:
/// - `len == 0` (empty range is always valid), or
/// - `len <= MAX_COPY_LEN`, `vaddr >= USER_ADDR_MIN`, and
///   `vaddr + len <= USER_ADDR_LIMIT` without overflow.
#[allow(clippy::result_unit_err)]
pub fn validate_user_range(vaddr: u64, len: usize) -> Result<(), ()> {
    if len == 0 {
        return Ok(());
    }
    if len > MAX_COPY_LEN {
        return Err(());
    }
    let end = vaddr.checked_add(len as u64).ok_or(())?;
    if vaddr < USER_ADDR_MIN || end > USER_ADDR_LIMIT {
        return Err(());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ---- Happy paths ----

    #[test]
    fn empty_range_always_ok() {
        assert!(validate_user_range(0, 0).is_ok());
        assert!(validate_user_range(0xFFFF_FFFF_FFFF_FFFF, 0).is_ok());
    }

    #[test]
    fn valid_single_byte() {
        assert!(validate_user_range(USER_ADDR_MIN, 1).is_ok());
    }

    #[test]
    fn valid_single_page() {
        assert!(validate_user_range(0x10_0000, 4096).is_ok());
    }

    #[test]
    fn valid_max_copy_len() {
        assert!(validate_user_range(0x10_0000, MAX_COPY_LEN).is_ok());
    }

    #[test]
    fn valid_range_touching_upper_limit() {
        // [USER_ADDR_LIMIT - 1, USER_ADDR_LIMIT) is exactly 1 byte at the top
        assert!(validate_user_range(USER_ADDR_LIMIT - 1, 1).is_ok());
    }

    // ---- Null / low-address rejection ----

    #[test]
    fn null_pointer_rejected() {
        assert!(validate_user_range(0, 1).is_err());
    }

    #[test]
    fn below_min_rejected() {
        assert!(validate_user_range(0xFFF, 1).is_err());
    }

    #[test]
    fn addr_one_rejected() {
        assert!(validate_user_range(1, 1).is_err());
    }

    // ---- Kernel-space rejection ----

    #[test]
    fn kernel_space_rejected() {
        assert!(validate_user_range(USER_ADDR_LIMIT, 1).is_err());
    }

    #[test]
    fn high_kernel_address_rejected() {
        assert!(validate_user_range(0xFFFF_8000_0000_0000, 1).is_err());
    }

    #[test]
    fn range_crosses_into_kernel() {
        // Start in user space but end crosses into kernel space
        assert!(validate_user_range(USER_ADDR_LIMIT - 1, 2).is_err());
    }

    // ---- Overflow rejection ----

    #[test]
    fn u64_overflow_rejected() {
        assert!(validate_user_range(u64::MAX, 1).is_err());
    }

    #[test]
    fn large_overflow_rejected() {
        assert!(validate_user_range(u64::MAX - 10, 100).is_err());
    }

    // ---- MAX_COPY_LEN enforcement ----

    #[test]
    fn exceeds_max_copy_len() {
        assert!(validate_user_range(0x10_0000, MAX_COPY_LEN + 1).is_err());
    }

    #[test]
    fn way_over_max_copy_len() {
        assert!(validate_user_range(0x10_0000, usize::MAX).is_err());
    }

    // ---- Boundary precision ----

    #[test]
    fn exactly_at_min_boundary() {
        assert!(validate_user_range(USER_ADDR_MIN, 1).is_ok());
        assert!(validate_user_range(USER_ADDR_MIN - 1, 1).is_err());
    }

    #[test]
    fn exactly_at_max_boundary() {
        // Range [LIMIT-1, LIMIT) fits; [LIMIT-1, LIMIT+1) doesn't
        assert!(validate_user_range(USER_ADDR_LIMIT - 1, 1).is_ok());
        assert!(validate_user_range(USER_ADDR_LIMIT - 1, 2).is_err());
    }

    #[test]
    fn max_copy_len_boundary() {
        assert!(validate_user_range(0x10_0000, MAX_COPY_LEN).is_ok());
        assert!(validate_user_range(0x10_0000, MAX_COPY_LEN + 1).is_err());
    }
}
