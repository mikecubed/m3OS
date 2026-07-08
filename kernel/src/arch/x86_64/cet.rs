//! Phase 110 Track B.3 — kernel-side CET user shadow stacks.
//!
//! `kernel_core::cet` pins the pure policy + bit layouts (CPUID decode, the
//! `IA32_*_CET` / `IA32_PL3_SSP` MSR numbers + bit fields, the shadow-stack PTE
//! encoding). This module is the **kernel** half: the per-task shadow-stack
//! allocation, the `IA32_PL3_SSP` save/restore the dispatch boundary calls, and
//! the `#CP` (Control-Protection, vector 21) handler seam.
//!
//! **Everything here is gated on [`crate::mitigations::state().cet_active`]** and
//! is a no-op on every QEMU lane (TCG models no CET). The *active* path
//! (shadow-stack pushes, the `#CP` fault, the MSR reads/writes) runs only on CET
//! silicon — validated on the Dell Tiger Lake, since QEMU cannot exercise it.
//!
//! **SSP lifecycle (the load-bearing model, SDM Vol 3A §6.14).** m3OS enables
//! *user* shadow stacks only (`IA32_S_CET.SH_STK_EN = 0`, no kernel shadow
//! stack). On a ring-3 → ring-0 transition the CPU saves the outgoing user
//! `SSP` into `IA32_PL3_SSP` and loads `SSP = 0`; `IRET` back to ring 3 reloads
//! `SSP` from `IA32_PL3_SSP`. So within one kernel entry/exit the user SSP is
//! preserved by hardware — the kernel only has to save/restore `IA32_PL3_SSP`
//! across **task switches** (a different task's kernel entry overwrote the MSR)
//! and around **signal delivery** (Track B.3 4/n). These MSR save/restore points
//! are co-located with the FPU/XSAVE save/restore, which has the identical
//! per-task-CPU-state lifecycle — that co-location is the correctness argument.

use kernel_core::cet::{MSR_IA32_PL3_SSP, compose_user_shadow_stack_pte};
use x86_64::registers::model_specific::Msr;

/// Base VA of the per-address-space CET shadow-stack region. Lives in
/// `PML4[255]` (a `kernel_core::kpti::USER_PML4_SLOTS` slot, so shadow-stack
/// pages are reachable on the KPTI user CR3), comfortably below the data stack
/// top (`ELF_STACK_TOP` `0x7FFF_FF00_0000`) so the two never collide.
pub const SHSTK_REGION_BASE: u64 = 0x7FF0_0000_0000;
/// Per-thread stride within the shadow-stack region: 2 MiB apart, so each
/// thread's 16 KiB shadow stack (plus headroom) is isolated.
pub const SHSTK_STRIDE: u64 = 0x20_0000;
/// Default user shadow-stack size: 16 KiB (4 pages) — one 8-byte slot per
/// pending `CALL`, ~2048 frames of call depth, more than any normal program.
/// Overflow faults `#PF` on the unmapped page below, killing only that process.
pub const USER_SHADOW_STACK_SIZE: u64 = 4 * 4096;

/// Whether CET user shadow stacks are active this boot. The single gate every
/// function in this module consults; `false` on QEMU and before `init_bsp`.
#[inline]
fn cet_active() -> bool {
    crate::mitigations::state().is_some_and(|s| s.cet_active)
}

/// Read the live `IA32_PL3_SSP` (user shadow-stack pointer). Only valid when CET
/// is active; the caller must gate on [`cet_active`].
///
/// # Safety
/// `IA32_PL3_SSP` must exist (CET active) — reading it on a no-CET CPU `#GP`s.
#[inline]
unsafe fn read_pl3_ssp() -> u64 {
    unsafe { Msr::new(MSR_IA32_PL3_SSP).read() }
}

/// Write `IA32_PL3_SSP`. Only valid when CET is active.
///
/// # Safety
/// `IA32_PL3_SSP` must exist (CET active); ring 0.
#[inline]
unsafe fn write_pl3_ssp(ssp: u64) {
    unsafe { Msr::new(MSR_IA32_PL3_SSP).write(ssp) };
}

/// Read the current core's live `IA32_PL3_SSP` (the running task's user SSP),
/// or `0` when CET is inactive. Used by fork so the child inherits the parent's
/// SSP (its copied address space includes the parent's shadow-stack pages).
#[inline]
pub fn read_task_ssp_live() -> u64 {
    if !cet_active() {
        return 0;
    }
    // SAFETY: gated on `cet_active`, so `IA32_PL3_SSP` exists; ring 0.
    unsafe { read_pl3_ssp() }
}

/// Save the current core's live `IA32_PL3_SSP` into `slot` at task switch-out.
/// Co-located with `save_fpu_state` in the dispatch epilogue: at that point the
/// MSR still holds the outgoing task's user SSP (hardware preserved it from that
/// task's kernel entry, and nothing has loaded a new task's SSP yet). No-op
/// unless CET is active — so it never touches the MSR on QEMU.
#[inline]
pub fn save_task_ssp(slot: &mut u64) {
    if !cet_active() {
        return;
    }
    // SAFETY: gated on `cet_active`, so `IA32_PL3_SSP` exists; ring 0.
    *slot = unsafe { read_pl3_ssp() };
}

/// Restore a task's saved `IA32_PL3_SSP` at switch-in. Co-located with
/// `restore_fpu_state` in the dispatch prep. A `0` slot (kernel task, or a task
/// with no shadow stack yet) writes `IA32_PL3_SSP = 0` — the task then performs
/// no shadow-stack operations until one is installed, which is harmless. No-op
/// unless CET is active.
#[inline]
pub fn restore_task_ssp(ssp: u64) {
    if !cet_active() {
        return;
    }
    // SAFETY: gated on `cet_active`, so `IA32_PL3_SSP` exists; ring 0.
    unsafe { write_pl3_ssp(ssp) };
}

/// Allocate + map a user shadow stack covering `[base_va, base_va + size)` in
/// the **current** address space (the caller must have the target process's CR3
/// live and hold its page-table lock), with the shadow-stack PTE encoding
/// (present, user, read-only, dirty, NX — [`compose_user_shadow_stack_pte`]).
/// Returns the initial `SSP` (top of the region: shadow stacks grow down, and
/// the first `CALL` decrements then writes). `None` on frame exhaustion.
///
/// # Safety
/// The current CR3 must be the target user address space, the page-table lock
/// held, and `[base_va, base_va+size)` free of existing mappings. On failure
/// the already-mapped pages stay linked (they are `USER_ACCESSIBLE` leaves,
/// reclaimed with the abandoned page table); only the last frame is freed here.
pub unsafe fn map_user_shadow_stack(base_va: u64, size: u64) -> Option<u64> {
    use x86_64::{VirtAddr, structures::paging::PageTableFlags};
    debug_assert_eq!(base_va & 0xFFF, 0, "shadow-stack base must be page-aligned");
    debug_assert_eq!(size & 0xFFF, 0, "shadow-stack size must be page-multiple");
    let flags = PageTableFlags::from_bits_truncate(compose_user_shadow_stack_pte());

    let mut va = base_va;
    while va < base_va + size {
        let frame = crate::mm::frame_allocator::allocate_frame_zeroed()?;
        // SAFETY: caller holds the page-table lock over the current (target)
        // CR3; the range is free; the flags are the shadow-stack leaf encoding
        // (intermediates are forced writable+user by the mapper, as CET
        // requires — the shadow-stack determination is made at the leaf).
        if unsafe {
            crate::mm::paging::map_current_user_page_locked(VirtAddr::new(va), frame, flags)
        }
        .is_err()
        {
            crate::mm::frame_allocator::free_frame(frame.start_address().as_u64());
            return None;
        }
        va += 4096;
    }
    Some(base_va + size)
}

/// Install a fresh CET user shadow stack for the **current** task and arm it:
/// allocate a shadow-stack region in the current address space (the CR3 must be
/// the target process's, live now), set the task's saved `cet_ssp`, and write
/// the live `IA32_PL3_SSP` so the imminent `iretq`/`sysret` to ring 3 uses it.
/// Returns `true` on success (or when CET is inactive — a no-op success so the
/// caller's fail-closed check passes on QEMU), `false` on frame exhaustion (the
/// caller should fail the exec/clone — a user task on a CET-active core with no
/// shadow stack would fault its first `CALL`).
///
/// # Safety
/// The current CR3 is the target process's live address space, and the caller
/// is on that task's context (about to return to its ring 3).
pub unsafe fn setup_current_task_shadow_stack() -> bool {
    if !cet_active() {
        return true;
    }
    let Some(addr_space) = crate::process::current_addr_space() else {
        return true; // kernel task — no shadow stack
    };
    let base = unsafe { addr_space.as_ref() }.alloc_shadow_stack_va();
    let ssp = {
        let _guard = unsafe { addr_space.as_ref() }.lock_page_tables();
        // SAFETY: the current CR3 is this address space; lock held; the bumped
        // region is fresh (never handed out before).
        match unsafe { map_user_shadow_stack(base, USER_SHADOW_STACK_SIZE) } {
            Some(top) => top,
            None => return false,
        }
    };
    // Record for future context switches + arm the live MSR for the return.
    crate::task::scheduler::set_current_task_cet_ssp(ssp);
    // SAFETY: gated on cet_active; ring 0.
    unsafe { write_pl3_ssp(ssp) };
    true
}
