//! Phase 110 Track A.5 — host-testable model of the KPTI PCID / INVPCID
//! TLB-cost recovery.
//!
//! A naive KPTI flushes the whole TLB on **every** CR3 switch. Every syscall
//! and every ring-3 IRQ does two switches (user→kernel on the way in,
//! kernel→user on the way out), so a syscall-heavy workload pays a full TLB
//! reload twice per call — the ~30 % overhead the Phase 84 charter bounds.
//!
//! PCID (process-context identifier, `CR3[11:0]` when `CR4.PCIDE = 1`) tags TLB
//! entries so translations for different contexts coexist without aliasing, and
//! the `CR3[63]` **no-flush** bit lets a `mov cr3` *keep* the target PCID's
//! entries instead of flushing them. m3OS uses a **fixed two-PCID scheme**:
//!
//! * [`KERNEL_PCID`] tags every process's **kernel-half** translations.
//! * [`USER_PCID`] tags every process's **user-half** translations.
//!
//! The two halves of *one* process are the same underlying user mappings
//! (`build_user_half` shares `PML4[0]`/`PML4[255]`) plus, on the kernel side, a
//! superset — so **within a process** the kernel↔user round trip is safe to do
//! **no-flush**: the entry/exit trampolines load [`compose_cr3`] values with
//! the no-flush bit set, and no syscall/IRQ pays a flush. That is the whole
//! recovery.
//!
//! Correctness across an **address-space change** (scheduler dispatch to a
//! different process, `execve`, `fork`, the return to pure kernel context) is
//! kept by *flushing both* PCIDs at that boundary — the two global PCIDs are
//! reused by every process, so a switch-in must drop the previous occupant's
//! entries under both tags before the no-flush trampolines start trusting them
//! again ([`cr3_load_on_addr_space_switch`] describes the load; the SMP
//! shootdown mirrors it by invalidating an address under *both* PCIDs). This is
//! coarser than Linux's per-process ASID pool (which can no-flush a dispatch to
//! a recently-run process too) but recovers the dominant syscall/IRQ path with
//! a fraction of the machinery; a per-CPU last-CR3 no-flush optimization for
//! same-process re-dispatch is a documented follow-up.
//!
//! The instruction-emitting CR3 writes and `invpcid` live in
//! `kernel/src/mm` + `kernel/src/smp` (they touch privileged registers). This
//! module pins the **pure arithmetic** — the CR3-value composition and the
//! feature-gate decision — as host-tested logic so the bit layout is reviewable
//! without QEMU. Bare metal is the only place the scheme is *live*: QEMU TCG
//! implements neither PCID nor INVPCID, so on every CI/QEMU lane
//! [`pcid_supported`] is `false` and the kernel runs the full-flush fallback
//! (identical to the Phase 110 A.4 behavior). The PCID-active path is validated
//! on the Dell (see `docs/roadmap/next-dell-session.md`).

/// The PCID tagging every process's **kernel-half** TLB entries. Nonzero so it
/// never collides with the `PCID = 0` a pre-`CR4.PCIDE` boot (or a raw
/// page-aligned `mov cr3`) implies.
pub const KERNEL_PCID: u16 = 1;

/// The PCID tagging every process's **user-half** TLB entries. Distinct from
/// [`KERNEL_PCID`] so the in-process kernel↔user round trip can be no-flush
/// without either half seeing the other's stale translations.
pub const USER_PCID: u16 = 2;

/// `CR3[63]` — when `CR4.PCIDE = 1`, a `mov cr3` with this bit set does **not**
/// flush the target PCID's TLB entries (Intel SDM Vol. 3A §4.10.4.1). Set on
/// the entry/exit trampoline loads (same-process kernel↔user); cleared on an
/// address-space switch so the load flushes.
pub const CR3_NOFLUSH: u64 = 1 << 63;

/// Mask of the PCID field (`CR3[11:0]`). Also the alignment slack of a 4 KiB
/// page-aligned PML4 physical address, so `pml4_phys & !PCID_MASK` clears any
/// stray low bits before the PCID is OR'd in.
pub const PCID_MASK: u64 = 0xFFF;

/// Compose a CR3 register value from a PML4 physical frame, a PCID, and the
/// no-flush choice.
///
/// `pml4_phys` is a 4 KiB-aligned physical address; its low 12 bits are cleared
/// before the `pcid` (masked to 12 bits) is inserted, and [`CR3_NOFLUSH`] is
/// set iff `noflush`. When `CR4.PCIDE = 0` (every QEMU lane) the CPU ignores
/// bits 11:0 and 63, so a value built here still loads correctly as a plain
/// page-aligned CR3 — but the kernel only *builds* PCID-tagged values when
/// [`pcid_supported`] held, so this is exercised for real only on bare metal.
#[inline]
pub const fn compose_cr3(pml4_phys: u64, pcid: u16, noflush: bool) -> u64 {
    let base = (pml4_phys & !PCID_MASK) | ((pcid as u64) & PCID_MASK);
    if noflush { base | CR3_NOFLUSH } else { base }
}

/// The kernel-half CR3 value for a process: [`KERNEL_PCID`], no-flush iff the
/// switch stays inside the same process (the entry trampoline, `noflush =
/// true`) rather than crossing an address-space boundary (dispatch/execve/fork,
/// `noflush = false`, so the load flushes `KERNEL_PCID`).
#[inline]
pub const fn kernel_cr3(pml4_phys: u64, noflush: bool) -> u64 {
    compose_cr3(pml4_phys, KERNEL_PCID, noflush)
}

/// The user-half CR3 value for a process: [`USER_PCID`], no-flush iff the exit
/// trampoline is returning to the *same* process it entered from (the common
/// case — `noflush = true`).
#[inline]
pub const fn user_cr3(pml4_phys: u64, noflush: bool) -> u64 {
    compose_cr3(pml4_phys, USER_PCID, noflush)
}

/// Whether the fixed two-PCID scheme is usable on this CPU.
///
/// Requires **both**:
/// * `CPUID.01H:ECX[17]` (PCID) — so `CR4.PCIDE` can be enabled and CR3 carries
///   a PCID field at all; and
/// * `CPUID.07H:0.EBX[10]` (INVPCID) — so the SMP shootdown can invalidate a
///   single address under a *non-current* PCID (the user PCID while running on
///   the kernel PCID, and vice-versa). Without INVPCID the only way to reach the
///   other PCID's entries is a full flush, which would erase the recovery, so
///   the kernel keeps the whole scheme off and runs the A.4 full-flush fallback.
///
/// Both bits are `0` under QEMU TCG, so every CI lane takes the fallback.
#[inline]
pub const fn pcid_supported(leaf1_ecx: u32, leaf7_ebx: u32) -> bool {
    const CPUID_01_ECX_PCID: u32 = 1 << 17;
    const CPUID_07_EBX_INVPCID: u32 = 1 << 10;
    (leaf1_ecx & CPUID_01_ECX_PCID) != 0 && (leaf7_ebx & CPUID_07_EBX_INVPCID) != 0
}

/// The CR3 value to load when switching **into** an address space across a
/// process boundary (scheduler dispatch, `execve`, `fork`, restore-to-kernel).
///
/// The two PCIDs are reused by every process, so the previous occupant's
/// entries under [`KERNEL_PCID`] must be dropped: the value is built **without**
/// the no-flush bit, so the `mov cr3` flushes `KERNEL_PCID`. The *user* PCID is
/// flushed separately by the caller ([`USER_PCID`] via `invpcid`), because a
/// single CR3 load only affects the PCID it loads. See
/// `kernel::mm::write_kernel_cr3`.
#[inline]
pub const fn cr3_load_on_addr_space_switch(kernel_pml4_phys: u64) -> u64 {
    kernel_cr3(kernel_pml4_phys, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compose_sets_pcid_and_noflush_bits() {
        let frame = 0x1234_5000u64; // 4 KiB aligned
        let v = compose_cr3(frame, KERNEL_PCID, false);
        assert_eq!(v & !PCID_MASK & !CR3_NOFLUSH, frame, "frame preserved");
        assert_eq!(v & PCID_MASK, KERNEL_PCID as u64, "pcid in low 12 bits");
        assert_eq!(v & CR3_NOFLUSH, 0, "no-flush clear when noflush=false");

        let v = compose_cr3(frame, USER_PCID, true);
        assert_eq!(v & PCID_MASK, USER_PCID as u64);
        assert_eq!(
            v & CR3_NOFLUSH,
            CR3_NOFLUSH,
            "no-flush set when noflush=true"
        );
        assert_eq!(v & !PCID_MASK & !CR3_NOFLUSH, frame);
    }

    #[test]
    fn compose_clears_stray_low_bits_of_frame() {
        // A frame value that already has junk in the PCID field must not let
        // that junk survive into the PCID — the mask clears it first.
        let dirty = 0x1234_5000u64 | 0x0ABu64;
        let v = compose_cr3(dirty, KERNEL_PCID, false);
        assert_eq!(v & PCID_MASK, KERNEL_PCID as u64, "only the PCID remains");
        assert_eq!(v & !PCID_MASK, 0x1234_5000, "aligned frame recovered");
    }

    #[test]
    fn kernel_and_user_pcids_are_distinct_and_nonzero() {
        assert_ne!(KERNEL_PCID, USER_PCID);
        assert_ne!(
            KERNEL_PCID, 0,
            "0 is the pre-PCIDE / raw-cr3 tag; must not reuse"
        );
        assert_ne!(USER_PCID, 0);
        assert!((KERNEL_PCID as u64) <= PCID_MASK);
        assert!((USER_PCID as u64) <= PCID_MASK);
    }

    #[test]
    fn kernel_cr3_and_user_cr3_tag_correctly() {
        let kf = 0x2000_0000u64;
        let uf = 0x2100_0000u64;
        // Entry trampoline: same-process, no-flush kernel load.
        assert_eq!(kernel_cr3(kf, true), kf | KERNEL_PCID as u64 | CR3_NOFLUSH);
        // Exit trampoline: same-process, no-flush user load.
        assert_eq!(user_cr3(uf, true), uf | USER_PCID as u64 | CR3_NOFLUSH);
        // Cross-process kernel load flushes (no no-flush bit).
        assert_eq!(kernel_cr3(kf, false), kf | KERNEL_PCID as u64);
    }

    #[test]
    fn addr_space_switch_load_flushes_kernel_pcid() {
        let kf = 0x3000_0000u64;
        let v = cr3_load_on_addr_space_switch(kf);
        assert_eq!(v & CR3_NOFLUSH, 0, "cross-process load must flush");
        assert_eq!(v & PCID_MASK, KERNEL_PCID as u64);
        assert_eq!(v & !PCID_MASK, kf);
    }

    #[test]
    fn pcid_support_needs_both_pcid_and_invpcid() {
        const PCID: u32 = 1 << 17;
        const INVPCID: u32 = 1 << 10;
        assert!(pcid_supported(PCID, INVPCID), "both present → supported");
        assert!(!pcid_supported(PCID, 0), "PCID without INVPCID → fallback");
        assert!(
            !pcid_supported(0, INVPCID),
            "INVPCID without PCID → fallback"
        );
        assert!(!pcid_supported(0, 0), "neither → fallback");
        // QEMU TCG reports neither bit → every CI lane takes the fallback.
        assert!(!pcid_supported(0x0000_0000, 0x0000_0000));
    }
}
