//! Pure-logic image-bounds arithmetic for the dynamic linker.
//!
//! Every raw-pointer read the linker performs against data taken from
//! an on-disk ELF — symbol-table entries, hash-table arrays, version
//! tables, GOT slots — is driven by a length or index that the ELF
//! itself supplies and is therefore untrusted. `validate_dyn_pointers`
//! checks the *base* of each `PT_DYNAMIC`-referenced region at load
//! time, but it cannot bound an index that only materializes at lookup
//! time (a hash-chain symbol index, a relocation's `r_info`, a
//! `versym[sym_idx]` read). These helpers centralize the "is this byte
//! range inside the mapped image?" check so every call site applies the
//! same overflow-safe arithmetic.
//!
//! All functions are pure and `const`-friendly so they are exercised by
//! host `cargo test` without a live mapping.

/// Returns `true` when the half-open byte range `[start, start + len)`
/// lies fully within the image window `[image_base, image_base +
/// image_len)`. Every addition is checked; any overflow yields `false`.
/// A `start` below `image_base` is rejected.
///
/// `image_len == 0` is the placeholder shape used before a real mapping
/// exists; callers that want to *skip* the check in that case must do so
/// explicitly — this function treats a zero-length window as "nothing
/// fits" and returns `false` for any non-empty range.
pub fn range_in_image(start: u64, len: u64, image_base: u64, image_len: u64) -> bool {
    if start < image_base {
        return false;
    }
    match (image_base.checked_add(image_len), start.checked_add(len)) {
        (Some(image_end), Some(end)) => end <= image_end,
        _ => false,
    }
}

/// Returns `true` when element `idx` of an array of `elem_size`-byte
/// elements based at `base` lies fully within the image window — i.e.
/// `[base + idx*elem_size, base + (idx+1)*elem_size)` is in range.
/// Overflow in the index arithmetic yields `false`.
pub fn elem_in_image(base: u64, idx: u64, elem_size: u64, image_base: u64, image_len: u64) -> bool {
    match idx.checked_add(1).and_then(|n| n.checked_mul(elem_size)) {
        Some(end_off) => range_in_image(base, end_off, image_base, image_len),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Use a non-zero image_base so the lower-bound check is exercised.
    const BASE: u64 = 0x4000_0000;
    const LEN: u64 = 0x1_0000; // 64 KiB window → [0x4000_0000, 0x4001_0000)

    #[test]
    fn range_fully_inside_is_ok() {
        assert!(range_in_image(BASE, 8, BASE, LEN));
        assert!(range_in_image(BASE + 100, 8, BASE, LEN));
    }

    #[test]
    fn range_ending_exactly_at_image_end_is_ok() {
        // Last 8 bytes of the window.
        assert!(range_in_image(BASE + LEN - 8, 8, BASE, LEN));
    }

    #[test]
    fn range_one_byte_past_end_is_rejected() {
        assert!(!range_in_image(BASE + LEN - 8, 9, BASE, LEN));
        assert!(!range_in_image(BASE + LEN, 1, BASE, LEN));
    }

    #[test]
    fn range_below_base_is_rejected() {
        assert!(!range_in_image(BASE - 1, 8, BASE, LEN));
        assert!(!range_in_image(0, 8, BASE, LEN));
    }

    #[test]
    fn zero_length_range_at_end_is_ok_but_nonempty_is_not() {
        // A zero-length range exactly at the end is in-bounds (end == image_end).
        assert!(range_in_image(BASE + LEN, 0, BASE, LEN));
        // But a 1-byte range there is not.
        assert!(!range_in_image(BASE + LEN, 1, BASE, LEN));
    }

    #[test]
    fn range_start_plus_len_overflow_is_rejected() {
        assert!(!range_in_image(u64::MAX - 4, 8, BASE, LEN));
    }

    #[test]
    fn image_base_plus_len_overflow_is_rejected() {
        // image_end overflows → nothing can fit.
        assert!(!range_in_image(BASE, 8, u64::MAX - 4, 8));
    }

    #[test]
    fn elem_zero_index_checks_first_element() {
        // First 24-byte Sym entry at base.
        assert!(elem_in_image(BASE, 0, 24, BASE, LEN));
    }

    #[test]
    fn elem_last_fitting_index_is_ok_next_is_rejected() {
        // 24-byte entries; last index whose [i*24, (i+1)*24) fits.
        let last = LEN / 24 - 1;
        assert!(elem_in_image(BASE, last, 24, BASE, LEN));
        // The element after the last-fitting one must straddle/exceed the end.
        assert!(!elem_in_image(BASE, LEN / 24, 24, BASE, LEN));
    }

    #[test]
    fn elem_huge_index_overflow_is_rejected() {
        assert!(!elem_in_image(BASE, u64::MAX, 24, BASE, LEN));
        assert!(!elem_in_image(BASE, u64::MAX / 2, 24, BASE, LEN));
    }

    #[test]
    fn elem_two_byte_versym_entries() {
        // versym entries are u16 (2 bytes); index near the end.
        let last = LEN / 2 - 1;
        assert!(elem_in_image(BASE, last, 2, BASE, LEN));
        assert!(!elem_in_image(BASE, LEN / 2, 2, BASE, LEN));
    }
}
