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

use kernel_core::cet::MSR_IA32_PL3_SSP;
use x86_64::registers::model_specific::Msr;

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
