//! Phase 110 Track B.3 — host-testable model of Intel CET **user shadow stacks**.
//!
//! CET (Control-flow Enforcement Technology) shadow stacks are a hardware
//! control-flow-integrity layer: every `CALL` pushes the return address onto a
//! protected **shadow stack** in addition to the data stack, and every `RET`
//! checks that the two agree — so a return-address overwrite (ROP, a stack
//! buffer overflow past the canary) is caught by the CPU as a `#CP`
//! (Control-Protection, vector 21) exception before control transfers.
//!
//! This module pins the **pure policy + bit layouts** as host-tested logic, the
//! same split the KPTI (`kpti`/`kpti_pcid`) and Spectre (`spectre`) models use:
//! the CPUID feature decode, the `IA32_U_CET`/`IA32_S_CET` MSR bit layout, the
//! `IA32_*_SSP` MSR numbers, and the **shadow-stack page-table encoding**
//! (the load-bearing subtlety — a shadow-stack page is marked *read-only + dirty*
//! so ordinary stores fault but shadow-stack pushes succeed). The kernel side
//! (`kernel/src/arch/x86_64/cet.rs`) drives real MSRs/CR4 and page tables from
//! these constants.
//!
//! **Configuration m3OS targets:** *user* shadow stacks only. `IA32_U_CET`
//! enables ring-3 shadow stacks; `IA32_S_CET.SH_STK_EN` stays **0**, so ring 0
//! runs without a supervisor shadow stack (no kernel shadow stack, no IST
//! shadow-stack tokens to manage). Per the SDM, on a ring-3 → ring-0 transition
//! with the supervisor shadow stack disabled the CPU saves the outgoing user
//! `SSP` into `IA32_PL3_SSP` and loads `SSP = 0`; `IRET` back to ring 3 reloads
//! `SSP` from `IA32_PL3_SSP`. So within one kernel entry/exit the user SSP is
//! preserved by hardware, and the kernel need only save/restore `IA32_PL3_SSP`
//! across **task switches** and around **signal delivery** — which is exactly
//! what Track B.3 wires.

// ─── CPUID feature decode ────────────────────────────────────────────────────

/// `CPUID.(EAX=07H,ECX=0):ECX[7]` — CET shadow-stack (`CET_SS`) support.
pub const CPUID_07_ECX_CET_SS: u32 = 1 << 7;
/// `CPUID.(EAX=07H,ECX=0):EDX[20]` — CET indirect-branch-tracking (`CET_IBT`).
/// Decoded for completeness / the posture report; B.3 wires only shadow stacks.
pub const CPUID_07_EDX_CET_IBT: u32 = 1 << 20;

/// Decoded CET feature surface, from the guarded leaf-7 registers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CetFeatures {
    /// `CET_SS` — shadow stacks are architecturally supported.
    pub shstk: bool,
    /// `CET_IBT` — indirect branch tracking is architecturally supported.
    pub ibt: bool,
}

impl CetFeatures {
    /// Decode from `CPUID.07H.0:ECX` and `:EDX`. The caller must have already
    /// verified the max basic leaf is ≥ 7 (else these registers are a lower
    /// leaf's data — the `probe_smep_smap` trap); pass `(0, 0)` when it is not.
    #[inline]
    pub fn from_leaf7(ecx: u32, edx: u32) -> Self {
        Self {
            shstk: ecx & CPUID_07_ECX_CET_SS != 0,
            ibt: edx & CPUID_07_EDX_CET_IBT != 0,
        }
    }

    /// Whether the kernel may enable **user shadow stacks** on this CPU: the
    /// architectural `CET_SS` bit is set. (IBT is independent and not required.)
    #[inline]
    pub fn shstk_usable(self) -> bool {
        self.shstk
    }
}

// ─── Control register + MSR numbers ──────────────────────────────────────────

/// `CR4.CET` (bit 23) — the master enable for CET. Must be set (with
/// `CR0.WP = 1`, which m3OS always has) before either shadow stacks or IBT can
/// be enabled via the `IA32_*_CET` MSRs.
pub const CR4_CET: u64 = 1 << 23;

/// `IA32_U_CET` (0x6A0) — user-mode CET configuration.
pub const MSR_IA32_U_CET: u32 = 0x6A0;
/// `IA32_S_CET` (0x6A2) — supervisor-mode CET configuration. m3OS leaves
/// `SH_STK_EN` clear here (no kernel shadow stack).
pub const MSR_IA32_S_CET: u32 = 0x6A2;
/// `IA32_PL0_SSP` (0x6A4) — ring-0 shadow-stack pointer (unused: supervisor
/// shadow stacks are off).
pub const MSR_IA32_PL0_SSP: u32 = 0x6A4;
/// `IA32_PL3_SSP` (0x6A7) — ring-3 shadow-stack pointer. The one the kernel
/// saves/restores across task switches + signals.
pub const MSR_IA32_PL3_SSP: u32 = 0x6A7;

// `IA32_{U,S}_CET` bit layout (shared).
/// Bit 0 — `SH_STK_EN`: enable the shadow stack at this privilege.
pub const CET_SH_STK_EN: u64 = 1 << 0;
/// Bit 1 — `WR_SHSTK_EN`: allow the `WRSS`/`WRUSS` instructions (kernel writes
/// to a shadow stack, e.g. seeding a signal restore token). Off by default.
pub const CET_WR_SHSTK_EN: u64 = 1 << 1;
/// Bit 2 — `ENDBR_EN`: enable indirect-branch tracking. Off (B.3 = shadow
/// stacks only; IBT would require ENDBR-annotated userspace binaries).
pub const CET_ENDBR_EN: u64 = 1 << 2;

/// Compose the `IA32_U_CET` value for the m3OS user-shadow-stack posture.
///
/// `SH_STK_EN` when shadow stacks are enabled; `WR_SHSTK_EN` set when the
/// kernel needs `WRUSS` to seed a signal restore token onto the user shadow
/// stack (Track B.3 signal path). IBT (`ENDBR_EN`) is never set.
#[inline]
pub fn compose_u_cet(shstk_en: bool, wr_shstk_en: bool) -> u64 {
    let mut v = 0;
    if shstk_en {
        v |= CET_SH_STK_EN;
    }
    if wr_shstk_en {
        v |= CET_WR_SHSTK_EN;
    }
    v
}

// ─── Shadow-stack page-table encoding (the load-bearing subtlety) ────────────
//
// A linear address translates to a *shadow-stack* page when, with CR4.CET = 1,
// its leaf PTE has **R/W = 0** (bit 1 clear — ordinary data stores fault) and
// **Dirty = 1** (bit 6 set — which, on a not-writable page, the CPU reads as
// "this is a shadow-stack page"). Shadow-stack pushes (`CALL`) and `WRUSS`
// stores are the only writes the CPU permits. All *higher-level* entries on the
// path must be R/W = 1 (writable) so the shadow-stack determination is made at
// the leaf, not masked by a read-only intermediate. Shadow-stack pages are data
// and must be non-executable (XD = 1).
//
// The predicate/composer work over the standard x86-64 PTE flag bits so they
// are host-testable without the kernel's `PageTableFlags` type.

/// Standard x86-64 leaf-PTE flag bit positions (the subset the encoding needs).
pub const PTE_PRESENT: u64 = 1 << 0;
pub const PTE_WRITABLE: u64 = 1 << 1;
pub const PTE_USER: u64 = 1 << 2;
pub const PTE_ACCESSED: u64 = 1 << 5;
pub const PTE_DIRTY: u64 = 1 << 6;
pub const PTE_NO_EXECUTE: u64 = 1 << 63;

/// Compose the leaf-PTE flag bits for a **user** shadow-stack page: present,
/// user-accessible, read-only (so data stores fault), dirty (so the CPU treats
/// it as a shadow stack), accessed, and non-executable. Deliberately **not**
/// writable — that is what makes it a shadow stack rather than an ordinary
/// user page.
#[inline]
pub fn compose_user_shadow_stack_pte() -> u64 {
    PTE_PRESENT | PTE_USER | PTE_DIRTY | PTE_ACCESSED | PTE_NO_EXECUTE
}

/// Whether a leaf PTE (by its flag bits) encodes a shadow-stack page: present,
/// **not** writable, and dirty. This is the exact combination the CPU
/// interprets as a shadow stack when `CR4.CET = 1`.
#[inline]
pub fn is_shadow_stack_pte(flags: u64) -> bool {
    flags & PTE_PRESENT != 0 && flags & PTE_WRITABLE == 0 && flags & PTE_DIRTY != 0
}

// ─── Shadow-stack restore token (signal / RSTORSSP handshake) ────────────────
//
// A "shadow-stack restore token" is an 8-byte value the kernel places on a
// user shadow stack so a later `RSTORSSP` can atomically verify + switch onto
// it. The token stored at (8-byte-aligned) address `T` has value
// `(T + 8) | mode`, where bit 0 = 1 selects 64-bit mode and bit 1 = 0. m3OS's
// signal path uses an explicit `IA32_PL3_SSP` save/restore rather than the
// `RSTORSSP` dance, but the token format is modelled here for the (optional)
// hardening that seeds one, and host-tested so the bit layout is unambiguous.

/// Compose a 64-bit-mode shadow-stack restore token that lives at `token_addr`
/// (which must be 8-byte aligned). Bit 0 (mode) = 1 (64-bit); the payload is
/// `token_addr + 8` (the SSP value just above the token). Returns `None` if
/// `token_addr` is not 8-byte aligned.
#[inline]
pub fn shadow_stack_restore_token(token_addr: u64) -> Option<u64> {
    if token_addr & 0x7 != 0 {
        return None;
    }
    // Payload is the SSP directly above the token slot; bit 0 marks 64-bit mode.
    Some((token_addr + 8) | 0x1)
}

// ─── Live user-SSP source selection (nested-signal correctness) ──────────────
//
// The kernel needs the *live* user shadow-stack pointer at two loci in the
// signal path — seeding the handler's restorer (`WRUSS`) on delivery, and
// re-syncing `IA32_PL3_SSP` before the `IRETQ` at `sigreturn`. The live value
// lives in one of two hardware places depending on how ring 0 was entered:
//
//   * **IDT delivery** (interrupt/exception): with the supervisor shadow stack
//     disabled, the CPU saves the outgoing user `SSP` into `IA32_PL3_SSP` and
//     loads `SSP = 0`. So a `RDSSP` in ring 0 reads `0`; the MSR is live.
//   * **`SYSCALL`**: SSP is *not* switched (there is no supervisor shadow stack
//     to switch to), so the `SSP` register still holds the live user value while
//     `IA32_PL3_SSP` is stale — last written by an unrelated earlier transition
//     (for a nested signal, by the *outer* handler's own delivery seed).
//
// This is the crux of the nested-signal bug: a handler entered via `IRETQ` runs
// with its live SSP in the register, and a *nested* signal taken on that
// handler's `SYSCALL` (e.g. `kill`) must read the handler's advanced SSP from
// the register, not the stale seed value left in the MSR. Reading `RDSSP` and
// falling back to the MSR only when it is zero picks the authoritative source in
// both cases — with no clobberable per-task slot, so arbitrarily deep nesting is
// correct because every frame's live SSP comes straight from hardware.

/// Select the live user shadow-stack pointer from the two hardware sources: the
/// `RDSSP` register read (`rdssp`) and the `IA32_PL3_SSP` MSR (`pl3_ssp_msr`).
/// `RDSSP` is authoritative when non-zero (a `SYSCALL`-entered path, where the
/// register still holds the live SSP); a zero `RDSSP` means an IDT entry zeroed
/// the register and saved the live SSP into the MSR, so the MSR wins.
#[inline]
pub fn select_live_ssp(rdssp: u64, pl3_ssp_msr: u64) -> u64 {
    if rdssp != 0 { rdssp } else { pl3_ssp_msr }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpuid_decode_reads_shstk_and_ibt_bits() {
        let none = CetFeatures::from_leaf7(0, 0);
        assert!(!none.shstk && !none.ibt && !none.shstk_usable());

        let shstk_only = CetFeatures::from_leaf7(CPUID_07_ECX_CET_SS, 0);
        assert!(shstk_only.shstk && !shstk_only.ibt && shstk_only.shstk_usable());

        let ibt_only = CetFeatures::from_leaf7(0, CPUID_07_EDX_CET_IBT);
        assert!(!ibt_only.shstk && ibt_only.ibt && !ibt_only.shstk_usable());

        let both = CetFeatures::from_leaf7(CPUID_07_ECX_CET_SS, CPUID_07_EDX_CET_IBT);
        assert!(both.shstk && both.ibt && both.shstk_usable());
    }

    #[test]
    fn cpuid_decode_ignores_unrelated_bits() {
        // Neighbouring bits must not be mistaken for CET_SS/CET_IBT.
        let noise_ecx = CetFeatures::from_leaf7(!CPUID_07_ECX_CET_SS, 0);
        assert!(!noise_ecx.shstk);
        let noise_edx = CetFeatures::from_leaf7(0, !CPUID_07_EDX_CET_IBT);
        assert!(!noise_edx.ibt);
    }

    #[test]
    fn u_cet_composition() {
        assert_eq!(compose_u_cet(false, false), 0);
        assert_eq!(compose_u_cet(true, false), CET_SH_STK_EN);
        assert_eq!(compose_u_cet(true, true), CET_SH_STK_EN | CET_WR_SHSTK_EN);
        // ENDBR (IBT) is never composed by B.3.
        assert_eq!(compose_u_cet(true, true) & CET_ENDBR_EN, 0);
    }

    #[test]
    fn shadow_stack_pte_is_present_ro_dirty_nx_user() {
        let pte = compose_user_shadow_stack_pte();
        assert!(pte & PTE_PRESENT != 0, "must be present");
        assert!(
            pte & PTE_WRITABLE == 0,
            "must NOT be writable (that's the point)"
        );
        assert!(pte & PTE_DIRTY != 0, "dirty marks it a shadow stack");
        assert!(
            pte & PTE_USER != 0,
            "ring-3 shadow stack is user-accessible"
        );
        assert!(
            pte & PTE_NO_EXECUTE != 0,
            "shadow stacks are data, non-executable"
        );
        assert!(is_shadow_stack_pte(pte));
    }

    #[test]
    fn is_shadow_stack_pte_rejects_ordinary_pages() {
        // Ordinary writable user data page: present + writable + dirty — NOT a
        // shadow stack (writable disqualifies it).
        let data = PTE_PRESENT | PTE_WRITABLE | PTE_USER | PTE_DIRTY;
        assert!(!is_shadow_stack_pte(data));
        // Read-only but clean (e.g. .rodata): present + !writable + !dirty.
        let rodata = PTE_PRESENT | PTE_USER;
        assert!(!is_shadow_stack_pte(rodata));
        // Absent page.
        assert!(!is_shadow_stack_pte(0));
    }

    #[test]
    fn restore_token_format_and_alignment() {
        // Aligned token: payload = addr + 8, 64-bit mode bit set.
        assert_eq!(shadow_stack_restore_token(0x1000), Some(0x1008 | 0x1));
        assert_eq!(
            shadow_stack_restore_token(0x7fff_0000),
            Some(0x7fff_0008 | 0x1)
        );
        // Misaligned token address is rejected (RSTORSSP requires 8-byte align).
        assert_eq!(shadow_stack_restore_token(0x1004), None);
        assert_eq!(shadow_stack_restore_token(0x1001), None);
    }

    #[test]
    fn live_ssp_prefers_register_when_nonzero() {
        // SYSCALL path: RDSSP holds the live SSP, MSR is stale — register wins.
        // This is the nested-signal case: the outer handler's advanced SSP
        // (0x7ff0) must beat the stale seed left in the MSR (0x8000).
        assert_eq!(select_live_ssp(0x7ff0, 0x8000), 0x7ff0);
        assert_eq!(select_live_ssp(0x1, 0x0), 0x1);
    }

    #[test]
    fn live_ssp_falls_back_to_msr_when_register_zero() {
        // IDT path: the CPU zeroed SSP and saved the live value into the MSR —
        // fall back to the MSR. Also the CET-off / RDSSP-is-a-NOP degenerate case
        // (register read stays the pre-zeroed 0), which correctly yields the MSR.
        assert_eq!(select_live_ssp(0x0, 0x9abc), 0x9abc);
        assert_eq!(select_live_ssp(0x0, 0x0), 0x0);
    }
}
