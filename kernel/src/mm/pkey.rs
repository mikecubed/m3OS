//! Kernel page-table side of x86 Memory Protection Keys (PKU) — Phase 90a
//! Track B.2.
//!
//! The pure key-bit math and per-process allocation accounting live in
//! [`kernel_core::pkey`] (host-tested). This module is the *kernel* glue: it
//! folds a protection key into the `PageTableFlags` word the page-table manager
//! writes into a live user PTE, and it documents — in the
//! [PTE-rewrite-path audit](#pte-rewrite-path-audit) below — every kernel path
//! that mutates an existing user PTE and how each preserves (or sets) the key
//! field.
//!
//! ## Why the key lives in the PTE
//!
//! A protection key tags a *page*, not a VMA permission bit — the CPU reads the
//! 4-bit key field from the PTE (bits 59..=62), indexes PKRU with it, and
//! AND-masks the resulting per-key access rights into the page's effective
//! permissions on every access. So the tag must ride every PTE for a tagged
//! range and survive every PTE rewrite. **A dropped tag on a JIT code page is
//! an unguarded W+X page** — the W^X v2 invariant (Phase 90a Track C) only holds
//! if no rewrite path silently zeroes the key field back to the default key 0.
//!
//! ## Default-key bit-for-bit guarantee
//!
//! Key 0 is the default. [`with_pkey`](kernel_core::pkey::with_pkey)`(flags, 0)`
//! clears the field, which is a no-op on any PTE that already carries key 0 —
//! i.e. every PTE in the tree today. So routing existing composition sites
//! through [`compose_user_pte_flags`] with `pkey = 0` produces a PTE
//! bit-for-bit identical to the pre-PKU one. PKU changes nothing until a
//! non-zero key is actually requested via `sys_pkey_mprotect` (Track B.3).
//!
//! ## <a name="pte-rewrite-path-audit"></a>PTE-rewrite-path audit (Track B.2)
//!
//! Every kernel path that writes or rewrites a *user* PTE, audited for key
//! preservation. The invariant: a path that starts from an existing PTE's
//! `flags()` and toggles individual bits **preserves** the key field for free
//! (the key bits are untouched by the toggles); a path that composes a flag word
//! **from scratch** (from POSIX `prot` bits) must route the key through
//! [`compose_user_pte_flags`] or it drops the tag.
//!
//! | Path | File:symbol | Composes from | Key handling |
//! |---|---|---|---|
//! | Demand fault commit | `arch/x86_64/interrupts.rs::demand_map_user_page_locked` | scratch (`prot`) | **Routed through [`compose_user_pte_flags`]** — takes a `pkey` arg (0 until a VMA carries a key in Track B.3). This is the one from-scratch user-PTE commit; without the route a faulted-in JIT page would lose its tag. |
//! | CoW fork copy | `arch/x86_64/syscall/mod.rs::cow_clone_user_pages` | parent `pte.flags()` | **Preserved.** Child flags are `(flags & !WRITABLE) \| BIT_9` (or `flags` verbatim for non-writable / device frames) — the whole word is carried, so bits 59..=62 ride along unchanged. No change needed. |
//! | CoW fault resolution | `arch/x86_64/interrupts.rs::resolve_cow_fault` | existing `pte.flags()` | **Preserved.** New flags are `(pte.flags() \| WRITABLE) & !BIT_9` and the same physical-or-fresh frame; only WRITABLE/BIT_9 toggle, the key field is untouched. No change needed. |
//! | `mprotect` permission rewrite / range split | `arch/x86_64/syscall/mod.rs::sys_mprotect` | existing `old_flags` | **Preserved.** `final_flags` starts as `old_flags` and only toggles PRESENT / WRITABLE / NO_EXECUTE / USER / BIT_9 / BIT_10; the key field carries through. (`pkey_mprotect` — Track B.3 — is the path that will *set* a non-zero key here.) No change needed. |
//! | Fresh mmap commit | demand-paged: no PTE is written at `sys_mmap` time (anonymous mmap records a VMA; pages are filled by the demand-fault path above) | — | Covered by the demand-fault row. |
//! | `map_current_user_page_locked` / `map_user_frames*` | `mm/paging.rs`, `mm/user_space.rs` | caller-supplied `flags` | **Pass-through.** These take a full `PageTableFlags` and write it verbatim, so they carry whatever key the caller composed. The composition responsibility sits with the caller (the demand-fault path above is the one that composes from scratch). No change needed. |
//!
//! `sys_pkey_mprotect` itself (Track B.3) is the path that *sets* a non-zero key
//! into a range's PTEs; it is out of scope here and is built against
//! `sys_mprotect`'s preserve-by-`old_flags` logic plus a `with_pkey` stamp.

use kernel_core::pkey::with_pkey;
use x86_64::structures::paging::PageTableFlags;

// POSIX `prot` bits (match the syscall layer's constants).
const PROT_WRITE: u64 = 0x2;
const PROT_EXEC: u64 = 0x4;

/// Compose the `PageTableFlags` for a freshly-committed user data page from its
/// POSIX `prot` bits, folding protection key `pkey` into PTE bits 59..=62.
///
/// This is the single from-scratch user-PTE composition helper. The base flag
/// set (`PRESENT | USER_ACCESSIBLE`, plus `WRITABLE` / `NO_EXECUTE` derived from
/// `prot`) is identical to what `demand_map_user_page_locked` built inline
/// before Track B.2; the only addition is the key stamp. With `pkey == 0` the
/// stamp is a no-op, so the produced flags are bit-for-bit identical to the
/// pre-PKU PTE (the default-key guarantee).
///
/// `pkey` is masked to 4 bits by [`with_pkey`], so an out-of-range key can never
/// smear into the `NX` bit or the low available bits.
pub fn compose_user_pte_flags(prot: u64, pkey: u8) -> PageTableFlags {
    let mut flags = PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE;
    if prot & PROT_WRITE != 0 {
        flags |= PageTableFlags::WRITABLE;
    }
    if prot & PROT_EXEC == 0 {
        flags |= PageTableFlags::NO_EXECUTE;
    }
    // Fold the protection key into bits 59..=62. The `x86_64` crate's
    // `PageTableFlags` is a `bitflags` over the same raw `u64`, so we round-trip
    // through the bits and let `kernel_core::pkey` own the field math.
    PageTableFlags::from_bits_truncate(with_pkey(flags.bits(), pkey))
}

#[cfg(test)]
mod tests {
    use super::*;
    use kernel_core::pkey::pkey_of;

    #[test]
    fn default_key_is_identical_to_legacy_composition() {
        // Legacy inline composition (pre-Track-B.2) for a few prot values.
        for &prot in &[0x0u64, 0x1, 0x3, 0x5, 0x7] {
            let mut legacy = PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE;
            if prot & PROT_WRITE != 0 {
                legacy |= PageTableFlags::WRITABLE;
            }
            if prot & PROT_EXEC == 0 {
                legacy |= PageTableFlags::NO_EXECUTE;
            }
            assert_eq!(
                compose_user_pte_flags(prot, 0),
                legacy,
                "key 0 must match the legacy flag composition bit-for-bit"
            );
        }
    }

    #[test]
    fn nonzero_key_is_stamped_into_the_pte() {
        let flags = compose_user_pte_flags(0x3 /* RW */, 7);
        assert_eq!(pkey_of(flags.bits()), 7);
        // The non-key bits must equal the key-0 composition.
        let base = compose_user_pte_flags(0x3, 0);
        assert_eq!(
            flags.bits() & !kernel_core::pkey::PKEY_PTE_MASK,
            base.bits()
        );
    }
}
