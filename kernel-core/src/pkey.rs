//! Pure x86 Memory Protection Keys (PKU) accounting — Phase 90a Track B.2,
//! host-testable.
//!
//! The kernel side (`kernel/`) owns the hardware: CR4.PKE, the PKRU register,
//! the per-task XSAVE component 9, and the page-table walks that stamp the key
//! into live PTEs. None of that runs on the host. What *is* pure integer logic
//! — and therefore lives here and is unit-tested on the host, mirroring how
//! `kernel_core::timerfd` factored the Phase 89 expiry math and
//! `kernel_core::eventfd` the counter math — is two things:
//!
//! 1. **PTE key-bit encode/decode.** The 4-bit protection key occupies PTE bits
//!    59..=62 (the `PKEY` field; bit 63 is `NX`, bits 52..=58 are other
//!    available-to-OS bits the kernel already uses for CoW / guard markers).
//!    Composing and extracting that field is bit math with no kernel state.
//!
//! 2. **Per-process key-allocation accounting.** A process has 16 protection
//!    keys (0..=15). Key 0 is the default key, reserved and never allocatable
//!    (and `pkey_free(0)` is `EINVAL` on Linux). `pkey_alloc` hands out the
//!    lowest free non-zero key, recording its `init_access_rights`; `pkey_free`
//!    returns it to the pool. Exhaustion is `ENOSPC`. This is the table the
//!    W^X v2 rule (Phase 90a Track C) consults to answer "was this key
//!    allocated with write-deny rights?" — so the rights are stored at alloc
//!    time, not re-derived.
//!
//! Linux references for the semantics encoded here:
//! `mm/pkeys` / `arch/x86/include/asm/pkeys.h` (16 keys, key 0 default,
//! `pkey_alloc` → ENOSPC on exhaustion, `pkey_free(0)` → EINVAL), and
//! `Documentation/core-api/protection-keys.rst` for `PKEY_DISABLE_ACCESS` /
//! `PKEY_DISABLE_WRITE`.

/// Number of protection keys the architecture supports (PTE field is 4 bits).
pub const NUM_PKEYS: u8 = 16;

/// The default protection key. Reserved: every PTE not explicitly tagged carries
/// key 0, and a process can never allocate or free it. Key 0 also has no
/// associated `init_access_rights` (its PKRU slot is always full-access).
pub const PKEY_DEFAULT: u8 = 0;

/// Lowest bit of the PTE `PKEY` field. The key occupies bits 59..=62.
pub const PKEY_PTE_SHIFT: u32 = 59;

/// 4-bit mask for the key value (0..=15), pre-shift.
pub const PKEY_MASK: u8 = 0x0F;

/// The `PKEY` field as a mask over a raw 64-bit PTE: bits 59,60,61,62.
pub const PKEY_PTE_MASK: u64 = (PKEY_MASK as u64) << PKEY_PTE_SHIFT;

// ---------------------------------------------------------------------------
// `init_access_rights` flags — the `pkey_alloc(flags, init_access_rights)`
// second argument, also the bit layout of the per-key PKRU slot. Linux values.
// ---------------------------------------------------------------------------

/// Deny *all* access (read + write) through this key. PKRU `AD` bit.
pub const PKEY_DISABLE_ACCESS: u32 = 0x1;
/// Deny *write* access through this key (reads still permitted). PKRU `WD` bit.
pub const PKEY_DISABLE_WRITE: u32 = 0x2;
/// The set of `init_access_rights` bits the kernel understands; any other bit
/// set in the `pkey_alloc` argument is rejected with `EINVAL`.
pub const PKEY_ACCESS_MASK: u32 = PKEY_DISABLE_ACCESS | PKEY_DISABLE_WRITE;

// ---------------------------------------------------------------------------
// PTE key-bit encode / decode. Pure bit math — the kernel page-table manager
// composes a PTE's flag word and folds the key in via [`with_pkey`], and reads
// it back via [`pkey_of`]. Default key 0 ⇒ all-zero field ⇒ a PTE bit-for-bit
// identical to today's (the "preserve existing behavior bit-for-bit" contract).
// ---------------------------------------------------------------------------

/// Fold a 4-bit protection key into a raw PTE flag word, replacing whatever was
/// previously in the `PKEY` field (bits 59..=62). `key` is masked to 4 bits, so
/// callers cannot smear into the `NX` bit (63) or the low available bits.
///
/// `with_pkey(flags, 0)` clears the field — i.e. it is the identity on a PTE
/// that already carries key 0, preserving the default-key bit-for-bit guarantee.
#[inline]
pub const fn with_pkey(flags: u64, key: u8) -> u64 {
    let cleared = flags & !PKEY_PTE_MASK;
    cleared | (((key & PKEY_MASK) as u64) << PKEY_PTE_SHIFT)
}

/// Extract the 4-bit protection key (0..=15) from a raw PTE flag word.
#[inline]
pub const fn pkey_of(flags: u64) -> u8 {
    ((flags >> PKEY_PTE_SHIFT) & (PKEY_MASK as u64)) as u8
}

/// True if `key` names a valid protection key index (0..=15).
#[inline]
pub const fn is_valid_pkey(key: u8) -> bool {
    key < NUM_PKEYS
}

// ---------------------------------------------------------------------------
// Per-process key-allocation accounting.
// ---------------------------------------------------------------------------

/// The outcome of a `pkey_alloc` request, mapped to a Linux errno by the caller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AllocOutcome {
    /// A key was allocated; carries the key index (1..=15).
    Ok(u8),
    /// All non-default keys are in use → `ENOSPC`.
    NoSpace,
    /// An unknown bit was set in `flags` or `init_access_rights` → `EINVAL`.
    Invalid,
}

/// The outcome of a `pkey_free` request, mapped to a Linux errno by the caller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FreeOutcome {
    /// The key was freed (was allocated; now returned to the pool).
    Ok,
    /// The key index is out of range, is key 0 (Linux: `pkey_free(0)` →
    /// `EINVAL`), or was not currently allocated → `EINVAL`.
    Invalid,
}

/// Per-process protection-key allocation table.
///
/// 16 keys. Key 0 is permanently reserved (default key) and reported as "in
/// use" so it can never be handed out, but [`free`](PkeyTable::free) of key 0 is
/// still `EINVAL` (it is reserved, not owned). Keys 1..=15 are allocatable. Each
/// allocated key records the `init_access_rights` it was created with so the
/// W^X v2 rule can later ask whether the key denies write.
///
/// This struct is `Copy` and tiny (a `u16` bitmap + 16 byte-rights) so it can be
/// embedded directly in a process's address-space metadata.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PkeyTable {
    /// Bit `k` set ⇒ key `k` is allocated/in-use. Bit 0 is forced on (the
    /// reserved default key) so the lowest-free scan never returns 0.
    allocated: u16,
    /// `init_access_rights` (low byte: `PKEY_DISABLE_ACCESS`/`_WRITE`) for each
    /// allocated key. Index 0 is unused (default key has no stored rights).
    rights: [u8; NUM_PKEYS as usize],
}

impl Default for PkeyTable {
    fn default() -> Self {
        Self::new()
    }
}

impl PkeyTable {
    /// A fresh table: only the reserved default key 0 is "in use"; all 15
    /// non-default keys are free.
    pub const fn new() -> Self {
        Self {
            allocated: 1, // bit 0 = default key, always reserved
            rights: [0; NUM_PKEYS as usize],
        }
    }

    /// True if `key` is currently allocated (or, for key 0, reserved).
    pub const fn is_allocated(&self, key: u8) -> bool {
        if key >= NUM_PKEYS {
            return false;
        }
        self.allocated & (1u16 << key) != 0
    }

    /// The `init_access_rights` an allocated key was created with. Returns
    /// `None` for the default key 0 and for any unallocated key. The W^X v2 rule
    /// uses this to decide whether a key denies write.
    pub fn rights(&self, key: u8) -> Option<u32> {
        if key == PKEY_DEFAULT || !self.is_allocated(key) {
            return None;
        }
        Some(self.rights[key as usize] as u32)
    }

    /// True if `key` is allocated with `init_access_rights` that deny write —
    /// i.e. `PKEY_DISABLE_WRITE` or `PKEY_DISABLE_ACCESS` (deny-all implies
    /// deny-write). This is the exact predicate the W^X v2 exception checks
    /// before permitting a guarded W+X mapping under `key`.
    pub fn denies_write(&self, key: u8) -> bool {
        match self.rights(key) {
            Some(r) => r & (PKEY_DISABLE_WRITE | PKEY_DISABLE_ACCESS) != 0,
            None => false,
        }
    }

    /// `pkey_alloc(flags, init_access_rights)`: hand out the lowest free
    /// non-default key, recording its rights.
    ///
    /// - `flags` must be 0 (Linux defines no `pkey_alloc` flags) — any other
    ///   value is `EINVAL`.
    /// - `init_access_rights` may set only `PKEY_DISABLE_ACCESS` /
    ///   `PKEY_DISABLE_WRITE`; any other bit is `EINVAL`.
    /// - When keys 1..=15 are all in use → `ENOSPC`.
    pub fn alloc(&mut self, flags: u32, init_access_rights: u32) -> AllocOutcome {
        if flags != 0 {
            return AllocOutcome::Invalid;
        }
        if init_access_rights & !PKEY_ACCESS_MASK != 0 {
            return AllocOutcome::Invalid;
        }
        // Lowest free key in 1..=15 (bit 0 is always set, so start scanning at 1).
        for key in 1u8..NUM_PKEYS {
            if self.allocated & (1u16 << key) == 0 {
                self.allocated |= 1u16 << key;
                self.rights[key as usize] = (init_access_rights & PKEY_ACCESS_MASK) as u8;
                return AllocOutcome::Ok(key);
            }
        }
        AllocOutcome::NoSpace
    }

    /// `pkey_free(key)`: return a key to the pool.
    ///
    /// Linux semantics: freeing key 0 is `EINVAL`; freeing an out-of-range or
    /// not-currently-allocated key is `EINVAL`. **Freeing does not untag pages**
    /// — any PTEs still carrying this key keep their tag (Linux behaves the same;
    /// the key value can be reused by a later `alloc`, and stale-tagged pages
    /// then fall under the new owner's rights). Untagging, if ever wanted, is the
    /// caller's separate responsibility.
    pub fn free(&mut self, key: u8) -> FreeOutcome {
        if key == PKEY_DEFAULT || key >= NUM_PKEYS {
            return FreeOutcome::Invalid;
        }
        if self.allocated & (1u16 << key) == 0 {
            return FreeOutcome::Invalid;
        }
        self.allocated &= !(1u16 << key);
        self.rights[key as usize] = 0;
        FreeOutcome::Ok
    }

    /// Count of currently-allocated non-default keys (0..=15). This is a
    /// **per-process** value and is intentionally *not* surfaced by
    /// `m3ctl mitigations status`, whose `MitigationReport` is a boot-wide
    /// snapshot (per-process key counts have no sensible boot-wide aggregate —
    /// see Phase 90a Track C.2). Retained as a tested invariant helper for the
    /// alloc/free unit tests and diagnostics.
    pub const fn keys_in_use(&self) -> u32 {
        // Subtract the always-set default-key bit 0.
        (self.allocated.count_ones()).saturating_sub(1)
    }
}

// ---------------------------------------------------------------------------
// `pkey_mprotect` argument validation (Track B.3) — host-tested pure logic.
//
// `sys_pkey_mprotect(addr, len, prot, pkey)` shares `sys_mprotect`'s VMA /
// permission machinery; the *only* extra logic over plain mprotect is what the
// `pkey` argument means and whether it is acceptable for *this* process. That
// decision is pure integer logic over the process's [`PkeyTable`], so it lives
// here and is unit-tested on the host.
// ---------------------------------------------------------------------------

/// The Linux sentinel `pkey == -1` (passed in a signed C `int`) means "do not
/// change the key — behave exactly like `mprotect`" (glibc/musl `mprotect` is a
/// thin wrapper that calls `pkey_mprotect` with `pkey = -1`). The syscall ABI
/// delivers arguments as `u64`, so the sentinel arrives as `0xFFFF_FFFF_FFFF_FFFF`
/// (and, because V8/musl may pass it as a sign-extended 32-bit `-1`, the kernel
/// treats the low-32 `0xFFFF_FFFF` form the same — see [`is_preserve_key`]).
pub const PKEY_PRESERVE: u64 = u64::MAX;

/// True if `pkey` (as delivered to the syscall in a `u64` register) is the
/// "preserve the existing key" alias — Linux's `pkey == -1`. Accepts both the
/// full 64-bit `-1` and a sign-extended 32-bit `-1` (`0xFFFF_FFFF`), since the
/// C `int` argument may be zero- or sign-extended into the 64-bit register
/// depending on the libc wrapper. In this mode `pkey_mprotect` is byte-for-byte
/// `mprotect` and the range's PTE key field is left untouched.
#[inline]
pub const fn is_preserve_key(pkey: u64) -> bool {
    pkey == PKEY_PRESERVE || pkey == 0xFFFF_FFFF
}

/// The decision `sys_pkey_mprotect` makes about its `pkey` argument *before*
/// touching any VMA or PTE. Mirrors `AllocOutcome`/`FreeOutcome` in shape so the
/// kernel maps each arm to an action (and, for `Invalid`, to `EINVAL`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PkeyMprotectKey {
    /// `pkey == -1` (the [`is_preserve_key`] alias): preserve each PTE's
    /// existing key field — identical to plain `mprotect`.
    Preserve,
    /// `pkey == 0`: tag the range with the default key. Equivalent to clearing
    /// the key field (the [`with_pkey`]`(_, 0)` identity), so also behaves like
    /// plain `mprotect`, but the kernel still *writes* key 0 into the field.
    Default,
    /// A non-default key (1..=15) that is **allocated** in this process. The
    /// range's PTEs are tagged with `.0`, and the W^X v2 rule (Track C.1) may
    /// later consult the key's stored rights.
    Tag(u8),
    /// The key is out of range, or names a non-default key that is **not
    /// currently allocated** in this process → `EINVAL` (Linux semantics).
    Invalid,
}

/// Classify the `pkey` argument of `pkey_mprotect` against this process's key
/// table, deciding how the range's PTEs should be (re)tagged.
///
/// Linux semantics (`mm/mprotect.c::do_pkey_mprotect` → `mm_pkey_is_allocated`):
/// - `pkey == -1` → preserve (plain mprotect);
/// - `pkey == 0` → the default key is always valid (it is reserved, not
///   "allocated", but `pkey_mprotect(_, 0)` is accepted and tags with key 0);
/// - a non-default key must be currently allocated, else `EINVAL`;
/// - an out-of-range key (>= 16) → `EINVAL`.
pub fn classify_pkey_mprotect(table: &PkeyTable, pkey: u64) -> PkeyMprotectKey {
    if is_preserve_key(pkey) {
        return PkeyMprotectKey::Preserve;
    }
    if pkey >= NUM_PKEYS as u64 {
        return PkeyMprotectKey::Invalid;
    }
    let key = pkey as u8;
    if key == PKEY_DEFAULT {
        return PkeyMprotectKey::Default;
    }
    if table.is_allocated(key) {
        PkeyMprotectKey::Tag(key)
    } else {
        PkeyMprotectKey::Invalid
    }
}

// ---------------------------------------------------------------------------
// W^X v2 decision (Phase 90a Track C.1) — host-tested pure logic.
//
// This is the phase's core security decision factored out of the kernel so it
// is unit-testable on the host. The kernel's single W^X enforcement point
// (`wx_request_rejected` → `mprotect_worker` in
// `kernel/src/arch/x86_64/syscall/mod.rs`) calls this to decide whether a W+X
// request is the one documented exception or must be rejected exactly as
// Phase 75 shipped it.
//
// The rule it implements VERBATIM (from the Phase 90a A.1 Findings,
// `docs/roadmap/tasks/90a-memory-protection-keys-tasks.md`):
//
//   1. `sys_mmap` with PROT_WRITE|PROT_EXEC          → rejected (unchanged from Phase 75).
//   2. `sys_mprotect` with PROT_WRITE|PROT_EXEC      → rejected (unchanged from Phase 75).
//   3. `sys_pkey_mprotect(addr, len, prot, pkey)`:
//      a. pkey == 0 (or pkey == -1, the preserve alias) → behaves like mprotect; W+X rejected.
//      b. a non-default pkey that is ALLOCATED and whose alloc-time
//         init_access_rights include PKEY_DISABLE_WRITE or PKEY_DISABLE_ACCESS,
//         AND PKU active (CR4.PKE / pku_usable())  → W+X PERMITTED; the range's
//         PTEs are tagged with the key.
//      c. any other W+X via pkey_mprotect (unallocated key, key allocated with
//         permissive rights, PKU absent)             → rejected, same errno as (1)/(2).
//   4. No other syscall or fault path may produce a W+X PTE.
// ---------------------------------------------------------------------------

/// Decide whether a **W+X** request is the documented pkey-guarded W^X v2
/// exception (permitted) or must be rejected exactly as Phase 75 shipped it.
///
/// This is consulted **only** when the request actually asks for both
/// `PROT_WRITE` and `PROT_EXEC` — for any non-W+X request the kernel never
/// reaches here (nothing to gate). Returns `true` iff *all* of the following
/// hold (rule clause 3.b above), and `false` (⇒ reject) otherwise:
///
/// - the request came through `sys_pkey_mprotect` with a **non-default**,
///   currently-**allocated** key — i.e. a [`PkeyMprotectKey::Tag`] decision
///   (clauses 1, 2, 3.a — `sys_mmap`/`sys_mprotect`/`pkey==0`/`pkey==-1` —
///   never produce `Tag`, so they fall straight through to reject);
/// - that key's **alloc-time** `init_access_rights` deny write
///   ([`PkeyTable::denies_write`]: `PKEY_DISABLE_WRITE` or `PKEY_DISABLE_ACCESS`);
/// - **PKU is active** on this CPU (`pku_active`, the kernel's `pku_usable()` —
///   CR4.PKE set + the XSAVE PKRU component live). A write-deny key is meaningless
///   without the hardware to enforce it, so absent PKU the exception is refused
///   and the request is rejected like plain `mprotect`.
///
/// `key_decision` is the already-classified outcome from
/// [`classify_pkey_mprotect`]; `table` is the calling process's [`PkeyTable`]
/// (the same one used to classify), consulted here for the key's stored rights.
pub fn wx_v2_permits(table: &PkeyTable, key_decision: PkeyMprotectKey, pku_active: bool) -> bool {
    match key_decision {
        // The only path to a guarded W+X mapping: a non-default, allocated key.
        // Clauses 1/2/3.a (mmap, plain mprotect, pkey==0 → Default, pkey==-1 →
        // Preserve) never reach `Tag`, so they are not permitted here.
        PkeyMprotectKey::Tag(key) => pku_active && table.denies_write(key),
        // Preserve (plain mprotect / pkey==-1), Default (pkey==0), and the
        // defensive Invalid arm are all rejected — exactly Phase 75.
        PkeyMprotectKey::Preserve | PkeyMprotectKey::Default | PkeyMprotectKey::Invalid => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- PTE key-bit encode / decode -----------------------------------------

    #[test]
    fn pkey_field_occupies_bits_59_to_62() {
        // Bit math sanity: the field mask is exactly bits 59,60,61,62.
        assert_eq!(PKEY_PTE_MASK, 0b1111u64 << 59);
        assert_eq!(
            PKEY_PTE_MASK,
            (1u64 << 59) | (1u64 << 60) | (1u64 << 61) | (1u64 << 62)
        );
        // The field must not touch NX (bit 63) or the low available bits (52..58).
        assert_eq!(PKEY_PTE_MASK & (1u64 << 63), 0);
        assert_eq!(PKEY_PTE_MASK & (1u64 << 58), 0);
    }

    #[test]
    fn default_key_zero_is_bit_for_bit_identity() {
        // The "preserve existing behavior bit-for-bit" contract: tagging an
        // arbitrary flag word with key 0 must leave it unchanged, and reading
        // an untagged word back yields key 0.
        for flags in [
            0u64,
            0x8000_0000_0000_0001, // PRESENT | NX
            0x0000_0000_0000_0E03, // PRESENT|WRITABLE|USER + some marker bits
            0x07FF_FFFF_FFFF_FFFF, // a wide word with the key field clear
        ] {
            assert_eq!(with_pkey(flags, 0), flags, "key 0 must be identity");
            assert_eq!(pkey_of(flags), 0, "untagged word decodes to key 0");
        }
    }

    #[test]
    fn encode_then_decode_roundtrips_all_keys() {
        let base = 0x8000_0000_0000_0001u64; // PRESENT | NX, key field clear
        for key in 0u8..NUM_PKEYS {
            let tagged = with_pkey(base, key);
            assert_eq!(pkey_of(tagged), key, "key {key} did not round-trip");
            // Tagging must change *only* the key field — the rest is preserved.
            assert_eq!(tagged & !PKEY_PTE_MASK, base & !PKEY_PTE_MASK);
        }
    }

    #[test]
    fn with_pkey_replaces_an_existing_tag() {
        // Re-tagging must overwrite, not OR-in (which would corrupt the field).
        let tagged_5 = with_pkey(0, 5);
        assert_eq!(pkey_of(tagged_5), 5);
        let retagged_2 = with_pkey(tagged_5, 2);
        assert_eq!(pkey_of(retagged_2), 2);
    }

    #[test]
    fn with_pkey_masks_out_of_range_key_to_4_bits() {
        // A key >= 16 must not smear into NX (bit 63). 0x1F & 0xF == 0xF.
        let tagged = with_pkey(0, 0x1F);
        assert_eq!(tagged & (1u64 << 63), 0, "must not touch NX");
        assert_eq!(pkey_of(tagged), 0x0F);
    }

    #[test]
    fn is_valid_pkey_range() {
        assert!(is_valid_pkey(0));
        assert!(is_valid_pkey(15));
        assert!(!is_valid_pkey(16));
        assert!(!is_valid_pkey(255));
    }

    // -- allocation accounting -----------------------------------------------

    #[test]
    fn fresh_table_reserves_key_zero_only() {
        let t = PkeyTable::new();
        assert!(t.is_allocated(0), "key 0 is always reserved/in-use");
        assert_eq!(t.keys_in_use(), 0, "no non-default keys allocated yet");
        for key in 1u8..NUM_PKEYS {
            assert!(!t.is_allocated(key), "key {key} should be free");
        }
        // Default key carries no stored rights.
        assert_eq!(t.rights(0), None);
        assert!(!t.denies_write(0));
    }

    #[test]
    fn alloc_hands_out_lowest_free_nonzero_key() {
        let mut t = PkeyTable::new();
        assert_eq!(t.alloc(0, 0), AllocOutcome::Ok(1));
        assert_eq!(t.alloc(0, 0), AllocOutcome::Ok(2));
        assert_eq!(t.alloc(0, 0), AllocOutcome::Ok(3));
        assert_eq!(t.keys_in_use(), 3);
    }

    #[test]
    fn alloc_records_init_access_rights() {
        let mut t = PkeyTable::new();
        let AllocOutcome::Ok(k) = t.alloc(0, PKEY_DISABLE_WRITE) else {
            panic!("alloc failed");
        };
        assert_eq!(t.rights(k), Some(PKEY_DISABLE_WRITE));
        assert!(t.denies_write(k), "write-deny key must report denies_write");

        let AllocOutcome::Ok(k2) = t.alloc(0, PKEY_DISABLE_ACCESS) else {
            panic!("alloc failed");
        };
        assert!(t.denies_write(k2), "deny-all implies deny-write");

        let AllocOutcome::Ok(k3) = t.alloc(0, 0) else {
            panic!("alloc failed");
        };
        assert_eq!(t.rights(k3), Some(0));
        assert!(
            !t.denies_write(k3),
            "a permissive key must NOT satisfy the v2 write-deny predicate"
        );
    }

    #[test]
    fn alloc_rejects_unknown_flags_and_rights() {
        let mut t = PkeyTable::new();
        // pkey_alloc defines no flags; non-zero flags → EINVAL.
        assert_eq!(t.alloc(1, 0), AllocOutcome::Invalid);
        // An init_access_rights bit outside the known mask → EINVAL.
        assert_eq!(t.alloc(0, 0x4), AllocOutcome::Invalid);
        assert_eq!(t.alloc(0, !PKEY_ACCESS_MASK), AllocOutcome::Invalid);
        // A rejected alloc must not consume a key.
        assert_eq!(t.keys_in_use(), 0);
    }

    #[test]
    fn alloc_exhaustion_returns_no_space() {
        let mut t = PkeyTable::new();
        // 15 allocatable keys (1..=15).
        for expected in 1u8..NUM_PKEYS {
            assert_eq!(t.alloc(0, 0), AllocOutcome::Ok(expected));
        }
        assert_eq!(t.keys_in_use(), 15);
        // 16th request → ENOSPC.
        assert_eq!(t.alloc(0, 0), AllocOutcome::NoSpace);
        // Still exhausted, idempotently.
        assert_eq!(t.alloc(0, 0), AllocOutcome::NoSpace);
    }

    #[test]
    fn free_returns_key_to_pool_and_clears_rights() {
        let mut t = PkeyTable::new();
        let AllocOutcome::Ok(k) = t.alloc(0, PKEY_DISABLE_WRITE) else {
            panic!()
        };
        assert!(t.is_allocated(k));
        assert_eq!(t.free(k), FreeOutcome::Ok);
        assert!(!t.is_allocated(k));
        assert_eq!(t.rights(k), None, "freed key carries no rights");
        assert_eq!(t.keys_in_use(), 0);
    }

    #[test]
    fn free_lowest_then_realloc_reuses_it() {
        let mut t = PkeyTable::new();
        assert_eq!(t.alloc(0, 0), AllocOutcome::Ok(1));
        assert_eq!(t.alloc(0, 0), AllocOutcome::Ok(2));
        assert_eq!(t.free(1), FreeOutcome::Ok);
        // Lowest-free scan reuses key 1 before key 3.
        assert_eq!(t.alloc(0, 0), AllocOutcome::Ok(1));
    }

    #[test]
    fn free_key_zero_is_einval() {
        let mut t = PkeyTable::new();
        assert_eq!(
            t.free(0),
            FreeOutcome::Invalid,
            "pkey_free(0) is EINVAL on Linux"
        );
        // Key 0 stays reserved.
        assert!(t.is_allocated(0));
    }

    #[test]
    fn free_unallocated_or_out_of_range_is_einval() {
        let mut t = PkeyTable::new();
        // Never allocated.
        assert_eq!(t.free(7), FreeOutcome::Invalid);
        // Out of range.
        assert_eq!(t.free(16), FreeOutcome::Invalid);
        assert_eq!(t.free(255), FreeOutcome::Invalid);
        // Double-free is EINVAL.
        let AllocOutcome::Ok(k) = t.alloc(0, 0) else {
            panic!()
        };
        assert_eq!(t.free(k), FreeOutcome::Ok);
        assert_eq!(t.free(k), FreeOutcome::Invalid);
    }

    #[test]
    fn free_does_not_untag_pages() {
        // Documented contract: freeing a key is pure table bookkeeping and has
        // no effect on any PTE. We assert the encode/decode math is independent
        // of the table — a tagged PTE keeps its bits regardless of free().
        let mut t = PkeyTable::new();
        let AllocOutcome::Ok(k) = t.alloc(0, PKEY_DISABLE_WRITE) else {
            panic!()
        };
        let tagged_pte = with_pkey(0x8000_0000_0000_0023, k); // a live W^X-ish PTE
        assert_eq!(pkey_of(tagged_pte), k);
        assert_eq!(t.free(k), FreeOutcome::Ok);
        // The PTE word is untouched by free() — the tag persists.
        assert_eq!(pkey_of(tagged_pte), k, "free must not untag a PTE");
    }

    #[test]
    fn default_table_equals_new() {
        assert_eq!(PkeyTable::default(), PkeyTable::new());
    }

    // -- pkey_mprotect argument validation (Track B.3) -----------------------

    #[test]
    fn preserve_key_alias_matches_minus_one_forms() {
        // Full 64-bit -1.
        assert!(is_preserve_key(u64::MAX));
        // Sign-extended 32-bit -1 (libc may zero-extend the C int).
        assert!(is_preserve_key(0xFFFF_FFFF));
        // Real keys and the default key are NOT the preserve alias.
        assert!(!is_preserve_key(0));
        assert!(!is_preserve_key(1));
        assert!(!is_preserve_key(15));
    }

    #[test]
    fn classify_preserve_is_plain_mprotect() {
        let t = PkeyTable::new();
        assert_eq!(
            classify_pkey_mprotect(&t, u64::MAX),
            PkeyMprotectKey::Preserve
        );
        assert_eq!(
            classify_pkey_mprotect(&t, 0xFFFF_FFFF),
            PkeyMprotectKey::Preserve
        );
    }

    #[test]
    fn classify_default_key_is_always_valid() {
        // pkey == 0 is accepted even on a fresh table (key 0 is reserved, but
        // pkey_mprotect(_, 0) tags with the default key — Linux accepts it).
        let t = PkeyTable::new();
        assert_eq!(classify_pkey_mprotect(&t, 0), PkeyMprotectKey::Default);
    }

    #[test]
    fn classify_allocated_nondefault_key_tags() {
        let mut t = PkeyTable::new();
        let AllocOutcome::Ok(k) = t.alloc(0, PKEY_DISABLE_WRITE) else {
            panic!()
        };
        assert_eq!(
            classify_pkey_mprotect(&t, k as u64),
            PkeyMprotectKey::Tag(k)
        );
    }

    #[test]
    fn classify_unallocated_nondefault_key_is_invalid() {
        let t = PkeyTable::new();
        // Key 7 was never allocated.
        assert_eq!(classify_pkey_mprotect(&t, 7), PkeyMprotectKey::Invalid);
    }

    #[test]
    fn classify_out_of_range_key_is_invalid() {
        let t = PkeyTable::new();
        assert_eq!(classify_pkey_mprotect(&t, 16), PkeyMprotectKey::Invalid);
        // Note: 0xFFFF_FFFF and u64::MAX are the preserve alias, not invalid;
        // an arbitrary large non-sentinel value is invalid.
        assert_eq!(
            classify_pkey_mprotect(&t, 0x1_0000_0000),
            PkeyMprotectKey::Invalid
        );
    }

    #[test]
    fn classify_freed_key_becomes_invalid_again() {
        let mut t = PkeyTable::new();
        let AllocOutcome::Ok(k) = t.alloc(0, 0) else {
            panic!()
        };
        assert_eq!(
            classify_pkey_mprotect(&t, k as u64),
            PkeyMprotectKey::Tag(k)
        );
        assert_eq!(t.free(k), FreeOutcome::Ok);
        // After free the same key number is no longer allocated → EINVAL.
        assert_eq!(
            classify_pkey_mprotect(&t, k as u64),
            PkeyMprotectKey::Invalid
        );
    }

    // -- W^X v2 accept/reject matrix (Track C.1) -----------------------------
    //
    // The single decision the kernel's W^X enforcement point makes for a W+X
    // request. The full A.1 contract: permitted iff (pkey_mprotect path) ∧
    // (non-default allocated key) ∧ (write-deny init rights) ∧ (PKU active).

    /// Helper: build a table with one allocated key carrying `rights`, returning
    /// the table and the key.
    fn table_with_key(rights: u32) -> (PkeyTable, u8) {
        let mut t = PkeyTable::new();
        let AllocOutcome::Ok(k) = t.alloc(0, rights) else {
            panic!("alloc failed")
        };
        (t, k)
    }

    #[test]
    fn wx_v2_permits_write_deny_key_with_pku() {
        // Clause 3.b — the ONE permitted W+X path: pkey_mprotect with a
        // non-default allocated write-deny key and PKU active.
        let (t, k) = table_with_key(PKEY_DISABLE_WRITE);
        assert!(
            wx_v2_permits(&t, PkeyMprotectKey::Tag(k), true),
            "PKEY_DISABLE_WRITE key + PKU must permit the guarded W+X mapping"
        );
        // PKEY_DISABLE_ACCESS (deny-all) implies deny-write → also permitted.
        let (t2, k2) = table_with_key(PKEY_DISABLE_ACCESS);
        assert!(
            wx_v2_permits(&t2, PkeyMprotectKey::Tag(k2), true),
            "deny-all key implies deny-write → permitted"
        );
    }

    #[test]
    fn wx_v2_rejects_permissive_key() {
        // Clause 3.c — a non-default allocated key whose init rights are
        // permissive (do NOT deny write) must be rejected even with PKU active:
        // an unguarded W+X page would result.
        let (t, k) = table_with_key(0);
        assert!(
            !wx_v2_permits(&t, PkeyMprotectKey::Tag(k), true),
            "a permissive (non-write-deny) key must NOT permit W+X"
        );
    }

    #[test]
    fn wx_v2_rejects_pku_absent() {
        // Clause 3.c — even a write-deny key is rejected when PKU is not active
        // on this CPU; without the hardware the key cannot be enforced.
        let (t, k) = table_with_key(PKEY_DISABLE_WRITE);
        assert!(
            !wx_v2_permits(&t, PkeyMprotectKey::Tag(k), false),
            "PKU absent → the exception is refused, W+X rejected"
        );
    }

    #[test]
    fn wx_v2_rejects_plain_mprotect() {
        // Clauses 1/2/3.a — plain mprotect (Preserve) and pkey_mprotect(_, 0)
        // (Default) never reach a Tag decision, so W+X is rejected exactly as
        // Phase 75, regardless of PKU. (The table is irrelevant for these arms.)
        let t = PkeyTable::new();
        assert!(
            !wx_v2_permits(&t, PkeyMprotectKey::Preserve, true),
            "plain mprotect (pkey==-1) W+X must stay rejected"
        );
        assert!(
            !wx_v2_permits(&t, PkeyMprotectKey::Default, true),
            "pkey_mprotect(_, 0) W+X must stay rejected"
        );
        // Defensive: the Invalid arm (caller pre-rejects) is also never permitted.
        assert!(!wx_v2_permits(&t, PkeyMprotectKey::Invalid, true));
    }

    #[test]
    fn wx_v2_full_matrix() {
        // Exhaustive truth table over (decision × pku) for a write-deny and a
        // permissive key. Only (Tag(write-deny), pku=true) is permitted.
        let (deny_t, deny_k) = table_with_key(PKEY_DISABLE_WRITE);
        let (perm_t, perm_k) = table_with_key(0);
        let base = PkeyTable::new();

        // (decision, table, pku, expected_permit)
        let cases: &[(PkeyMprotectKey, &PkeyTable, bool, bool)] = &[
            (PkeyMprotectKey::Tag(deny_k), &deny_t, true, true), // the one yes
            (PkeyMprotectKey::Tag(deny_k), &deny_t, false, false), // PKU off
            (PkeyMprotectKey::Tag(perm_k), &perm_t, true, false), // permissive key
            (PkeyMprotectKey::Tag(perm_k), &perm_t, false, false),
            (PkeyMprotectKey::Preserve, &base, true, false), // plain mprotect
            (PkeyMprotectKey::Preserve, &base, false, false),
            (PkeyMprotectKey::Default, &base, true, false), // pkey==0
            (PkeyMprotectKey::Default, &base, false, false),
            (PkeyMprotectKey::Invalid, &base, true, false),
            (PkeyMprotectKey::Invalid, &base, false, false),
        ];
        for &(decision, table, pku, expected) in cases {
            assert_eq!(
                wx_v2_permits(table, decision, pku),
                expected,
                "decision={decision:?} pku={pku} should permit={expected}"
            );
        }
    }
}
