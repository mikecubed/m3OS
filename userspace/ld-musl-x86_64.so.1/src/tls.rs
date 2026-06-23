//! Pure-logic static-TLS block-layout arithmetic for the dynamic linker.
//!
//! x86_64 is TLS variant II: the static TLS block sits *below* the thread
//! pointer and the musl `struct pthread` TCB sits *at* it. The loader must
//! place the block so that `block_start == TP - offset`, where `offset` is the
//! exact value the static linker baked into the executable's local-exec
//! `%fs:-N` references. Getting that offset wrong puts every thread-local at
//! the wrong address.
//!
//! These functions are pure and exercised by host `cargo test` without a live
//! mapping (the offset math is the part most prone to silent divergence from
//! the linker, so it is the part worth unit-testing).

/// Minimum alignment musl floors the TLS *region / TCB placement* to on
/// x86_64. The `struct pthread` (which carries the `%fs:0x28` stack canary and
/// must satisfy SSE alignment) needs at least 16-byte alignment regardless of
/// the TLS segment's own `p_align`.
pub const MIN_TLS_ALIGN: u64 = 16;

/// The main module's variant-II TP-relative TLS block size — the value the
/// loader subtracts from the thread pointer to find the block start, matching
/// the offset the static linker baked into the executable's local-exec
/// `%fs:-N` references.
///
/// This is `round_up(memsz, p_align)` using the segment's **own** `p_align`,
/// **not** the 16-byte placement floor ([`MIN_TLS_ALIGN`]). musl's variant-II
/// offset formula (`off = size + align-1; off -= (off + image) & (align-1)`)
/// reduces to exactly `round_up(size, p_align)` when the TLS image is
/// `p_align`-aligned, which the linker guarantees. Over-rounding the offset to
/// 16 for a segment whose `p_align < 16` and whose `memsz` is not a multiple of
/// 16 would place the block lower than the linker assumed, so every
/// thread-local would resolve to the wrong address.
///
/// `p_align == 0` ("no alignment requirement") is treated as `1`.
pub fn main_tls_offset(memsz: u64, p_align: u64) -> u64 {
    let align = p_align.max(1);
    memsz.saturating_add(align - 1) & !(align - 1)
}

/// Variant-II cumulative TLS offset for the *next* module in load order, given
/// the running total so far (Phase 95c follow-up — multi-module static TLS for
/// DT_NEEDED DSOs that carry `PT_TLS`, e.g. rustc's `librustc_driver.so`).
///
/// Each module's block sits at `TP - returned_offset`; the return value is also
/// the new running total to feed the next module. Matches musl's `dynlink.c`
/// variant-II rule `tls_off = round_up(tls_off + size, align)`. For the first
/// module (`running_off == 0`) this reduces to [`main_tls_offset`], so the main
/// executable's offset is unchanged when it is assigned module 1.
///
/// `p_align == 0` is treated as `1`.
pub fn next_tls_offset(running_off: u64, memsz: u64, p_align: u64) -> u64 {
    let align = p_align.max(1);
    running_off.saturating_add(memsz).saturating_add(align - 1) & !(align - 1)
}

/// The alignment to which the TLS region and thread pointer must be **placed**:
/// the segment's own `p_align` floored to [`MIN_TLS_ALIGN`]. Because this is a
/// multiple of the segment `p_align`, a TP aligned to it also aligns the block
/// start (`TP - main_tls_offset(..)`) to the segment alignment.
///
/// `p_align == 0` is treated as `1` before the floor (so the result is
/// [`MIN_TLS_ALIGN`]).
pub fn tls_place_align(p_align: u64) -> u64 {
    p_align.max(1).max(MIN_TLS_ALIGN)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offset_uses_segment_align_not_min_floor() {
        // The python3.12 case (p_align=8, memsz=16): round_up(16,8)==16, and it
        // happens to equal round_up(16,16) — the target is unaffected.
        assert_eq!(main_tls_offset(16, 8), 16);
        // The case the 16-floor would have broken: p_align=8, memsz=24. The
        // linker bakes round_up(24,8)=24; a 16-floored round would give 32.
        assert_eq!(main_tls_offset(24, 8), 24);
        // A genuinely 16-aligned segment rounds to 16-multiples.
        assert_eq!(main_tls_offset(24, 16), 32);
        assert_eq!(main_tls_offset(16, 16), 16);
    }

    #[test]
    fn offset_edge_cases() {
        assert_eq!(main_tls_offset(0, 8), 0);
        assert_eq!(main_tls_offset(1, 8), 8);
        assert_eq!(main_tls_offset(7, 8), 8);
        // p_align==0 → treated as 1 → identity round.
        assert_eq!(main_tls_offset(13, 0), 13);
        assert_eq!(main_tls_offset(13, 1), 13);
    }

    #[test]
    fn next_tls_offset_first_module_matches_main_offset() {
        // For the first module (running_off == 0) the cumulative formula must
        // reduce to `main_tls_offset` so the main exe's offset never changes.
        for &(memsz, align) in &[(16u64, 8u64), (24, 8), (24, 16), (0, 8), (7, 8), (13, 0)] {
            assert_eq!(
                next_tls_offset(0, memsz, align),
                main_tls_offset(memsz, align)
            );
        }
    }

    #[test]
    fn next_tls_offset_accumulates_variant_ii() {
        // Module 1: round_up(0+24, 8) = 24. Module 2 (memsz 16, align 16):
        // round_up(24+16, 16) = round_up(40,16) = 48. Module 3 (memsz 8,
        // align 8): round_up(48+8, 8) = 56.
        let o1 = next_tls_offset(0, 24, 8);
        assert_eq!(o1, 24);
        let o2 = next_tls_offset(o1, 16, 16);
        assert_eq!(o2, 48);
        let o3 = next_tls_offset(o2, 8, 8);
        assert_eq!(o3, 56);
        // Offsets are strictly increasing so module blocks never overlap.
        assert!(o1 < o2 && o2 < o3);
    }

    #[test]
    fn place_align_floors_to_min() {
        assert_eq!(tls_place_align(8), 16);
        assert_eq!(tls_place_align(4), 16);
        assert_eq!(tls_place_align(16), 16);
        assert_eq!(tls_place_align(32), 32);
        assert_eq!(tls_place_align(0), 16);
    }
}
