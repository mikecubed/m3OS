//! POSIX file-mode helpers shared between kernel and host tests.
//!
//! Phase 66 introduces this module to host the sticky-bit (`S_ISVTX`)
//! enforcement helper used by `sys_linux_unlink` and `sys_linux_rename`.
//! Keeping the logic in `kernel-core` lets us cover the truth table with
//! cheap `cargo test -p kernel-core` host-side cases rather than going
//! through the full QEMU harness for every revision.

/// POSIX sticky bit (`S_ISVTX = 0o1000`). When set on a directory, only the
/// directory owner, the file owner, or root may unlink or rename entries
/// inside it. This is what makes `/tmp` safe for shared user scratch space.
pub const S_ISVTX: u16 = 0o1000;

/// Sticky-bit access denial.
///
/// `check_sticky` returns this variant when the caller does not own the
/// target file, does not own the parent directory, is not root, and the
/// sticky bit is set on the parent. Callers translate `Denied` to
/// `-EACCES`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StickyError {
    Denied,
}

/// Enforce sticky-bit deletion semantics for `unlink`/`rename`.
///
/// Returns `Ok(())` if any of the following holds:
///   - `S_ISVTX` is clear on `parent_mode`,
///   - the caller is root (`caller_is_root == true`),
///   - the caller owns the file (`caller_uid == file_uid`),
///   - the caller owns the parent directory (`caller_uid == dir_uid`).
///
/// Otherwise returns `Err(StickyError::Denied)`.
pub fn check_sticky(
    parent_mode: u16,
    file_uid: u32,
    dir_uid: u32,
    caller_uid: u32,
    caller_is_root: bool,
) -> Result<(), StickyError> {
    if parent_mode & S_ISVTX == 0 {
        return Ok(());
    }
    if caller_is_root {
        return Ok(());
    }
    if caller_uid == file_uid || caller_uid == dir_uid {
        return Ok(());
    }
    Err(StickyError::Denied)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bit_clear_always_ok() {
        // Mode 0o755 has no sticky bit; deletion must be allowed regardless
        // of ownership.
        assert_eq!(check_sticky(0o755, 100, 100, 200, false), Ok(()));
        assert_eq!(check_sticky(0o777, 1, 2, 3, false), Ok(()));
    }

    #[test]
    fn bit_set_caller_is_root_ok() {
        // Root bypasses sticky-bit enforcement.
        assert_eq!(check_sticky(0o1777, 100, 100, 0, true), Ok(()));
    }

    #[test]
    fn bit_set_owner_match_ok() {
        // Caller owns the target file → allowed.
        assert_eq!(check_sticky(0o1777, 200, 100, 200, false), Ok(()));
    }

    #[test]
    fn bit_set_dir_owner_match_ok() {
        // Caller owns the parent directory → allowed even if not the file
        // owner.
        assert_eq!(check_sticky(0o1777, 100, 200, 200, false), Ok(()));
    }

    #[test]
    fn bit_set_neither_denied() {
        // Sticky set, caller is neither file owner, directory owner, nor
        // root → `-EACCES`.
        assert_eq!(
            check_sticky(0o1777, 100, 200, 300, false),
            Err(StickyError::Denied)
        );
    }

    #[test]
    fn high_bits_outside_isvtx_ignored() {
        // Only S_ISVTX matters; other suid/sgid bits do not gate this check.
        assert_eq!(check_sticky(0o6755, 100, 200, 300, false), Ok(()));
    }
}
