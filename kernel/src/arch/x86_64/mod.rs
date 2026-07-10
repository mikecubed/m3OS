pub mod apic;
// Phase 110 Track B.3 — CET user shadow stacks (per-task shadow-stack setup,
// IA32_PL3_SSP save/restore, the #CP handler seam).
pub mod cet;
pub mod cpufreq;
pub mod cpuid;
// Phase 111 Track B — trap & debug-register substrate (#DB/#BP dispatch,
// DR0–DR7 wrapper, single-step, sw-breakpoint patch).
pub mod debug;
pub mod gdt;
pub mod interrupts;
pub mod microcode;
pub mod pat;
pub mod pkru;
pub mod preempt_trap_frame;
pub mod ps2;
#[cfg(feature = "smep-smap-test")]
pub mod smap_test;
pub mod suspend;
pub mod syscall;

/// Initialize the GDT/TSS, IDT, and syscall gate.
///
/// Does **not** enable hardware interrupts. Call [`enable_interrupts`] separately
/// once all kernel subsystems (e.g. memory) are ready.
pub fn init() {
    gdt::init();
    interrupts::init();
    syscall::init();
}

/// Initialize the PIC and unmask hardware IRQs.
///
/// # Safety
///
/// Must be called after [`init`] (IDT loaded) and after all kernel subsystems
/// that may hold spin locks during early boot have finished initializing.
/// Enabling interrupts before that point can cause IRQ handlers to observe
/// partially-initialized state.
pub unsafe fn enable_interrupts() {
    unsafe {
        interrupts::init_pics();
        x86_64::instructions::interrupts::enable();
    }
}

unsafe extern "C" {
    /// The execve initial-entry ring-3 trampoline. Phase 110 A.3b part 3: the
    /// asm lives in the user-mapped `.text.kpti_exit` section
    /// (`interrupts.rs`, bottom) so it can flip to the user CR3 immediately
    /// before its `iretq`.
    fn execve_enter_userspace(rip: u64, rsp: u64, cs: u64, ss: u64) -> !;
}

/// Transfer execution to ring 3 (userspace) — the execve initial-entry path.
///
/// Uses `iretq` to atomically switch to user code segment, user stack, and
/// the given entry point with interrupts enabled (RFLAGS.IF = 1).
///
/// # Safety
///
/// * `entry` must be a valid, mapped, executable userspace virtual address.
/// * `user_stack_top` must be a valid, mapped, writable userspace stack
///   address (highest address; stack grows downward).
/// * Must be called after `init()` so that GDT user segments are loaded.
pub unsafe fn enter_userspace(entry: u64, user_stack_top: u64) -> ! {
    // Phase 57b D.3: assert preempt_count == 0 immediately before the
    // kernel hands control to ring 3.  See
    // `kernel/src/task/scheduler.rs::assert_preempt_count_zero_at_user_return`
    // for the invariant rationale.
    crate::task::scheduler::assert_preempt_count_zero_at_user_return();
    // Do not consume deferred reschedule here. `enter_userspace` is the
    // one-way execve initial-entry trampoline, not a normal syscall return;
    // yielding here saves a continuation on the process kernel stack before
    // the new image has ever run. Timer IRQ-return preemption can reschedule
    // the task immediately after ring 3 starts.
    unsafe {
        execve_enter_userspace(
            entry,
            user_stack_top,
            u64::from(gdt::user_code_selector().0),
            u64::from(gdt::user_data_selector().0),
        )
    }
}

/// Context for entering ring 3 from a fork child, stored in a static so
/// assembly can load register values without running out of register operands.
///
/// Includes ALL registers preserved by the Linux syscall ABI (everything
/// except RAX/RCX/R11) plus the IRET frame fields.
#[repr(C)]
pub struct ForkEntryCtx {
    pub rip: u64, // offset 0
    pub rsp: u64, // offset 8
    pub rbx: u64, // offset 16
    pub rbp: u64, // offset 24
    pub r12: u64, // offset 32
    pub r13: u64, // offset 40
    pub r14: u64, // offset 48
    pub r15: u64, // offset 56
    pub ss: u64,  // offset 64
    pub cs: u64,  // offset 72
    // Caller-saved registers (syscall-preserved).
    pub rdi: u64,    // offset 80
    pub rsi: u64,    // offset 88
    pub rdx: u64,    // offset 96
    pub r8: u64,     // offset 104
    pub r9: u64,     // offset 112
    pub r10: u64,    // offset 120
    pub rflags: u64, // offset 128
}

impl ForkEntryCtx {
    pub const ZERO: Self = Self {
        rip: 0,
        rsp: 0,
        rbx: 0,
        rbp: 0,
        r12: 0,
        r13: 0,
        r14: 0,
        r15: 0,
        ss: 0,
        cs: 0,
        rdi: 0,
        rsi: 0,
        rdx: 0,
        r8: 0,
        r9: 0,
        r10: 0,
        rflags: 0,
    };
}

// FORK_ENTRY_CTX has moved to PerCoreData (Phase 35).
// The fork_enter_userspace assembly reads it via gs-relative addressing.

unsafe extern "C" {
    /// The fork-child ring-3 entry trampoline. Phase 110 A.3b part 3: the asm
    /// lives in the user-mapped `.text.kpti_exit` section (`interrupts.rs`,
    /// bottom) so it can flip to the user CR3 immediately before its `iretq`.
    /// Restores ALL syscall-preserved registers from the `ForkEntryCtx` and
    /// enters ring 3 with RAX = 0 (the child's fork return value).
    fn fork_enter_userspace(ctx: *const ForkEntryCtx) -> !;
}

/// Enter ring 3 for a fork child with full register restore.
///
/// Restores ALL registers preserved by the Linux syscall ABI so the child
/// resumes with the exact same register state as the parent had at the
/// `syscall` instruction.
#[allow(clippy::too_many_arguments)]
pub unsafe fn enter_userspace_fork(
    rip: u64,
    rsp: u64,
    rbx: u64,
    rbp: u64,
    r12: u64,
    r13: u64,
    r14: u64,
    r15: u64,
    rdi: u64,
    rsi: u64,
    rdx: u64,
    r8: u64,
    r9: u64,
    r10: u64,
    rflags: u64,
) -> ! {
    // Phase 57b D.3: assert preempt_count == 0 before the assembly
    // trampoline runs `iretq` to ring 3.
    crate::task::scheduler::assert_preempt_count_zero_at_user_return();
    // NOTE: do NOT call check_deferred_preempt_at_user_return() here.
    // enter_userspace_fork is the fork trampoline: it runs before the child task
    // has a valid scheduler RSP. Yielding mid-trampoline (if preempt_resched_pending
    // is set) panics with a zero scheduler RSP. Normal syscall/signal-return
    // boundaries handle deferred reschedules after the task has a stable
    // continuation.

    // Phase 110 B.3 (Tiger Lake) — arm a CET user shadow stack for a user task
    // that reaches its first ring-3 entry without one. Only PID 1 (init) does:
    // it is kernel-spawned through this fork trampoline (`spawn_userspace_init`)
    // with `cet_ssp = 0`, whereas `execve` arms its own fresh image
    // (`setup_current_task_shadow_stack` before `enter_userspace`) and fork
    // children inherit the parent's (nonzero) SSP + copied shadow-stack pages.
    // Without this, on CET silicon init's first `CALL` pushes a return address
    // to `IA32_PL3_SSP = 0` and faults — the Dell/Tiger Lake stall right after
    // "exec'ing userspace PID 1" (POST marker 22, no userspace output). We run
    // in init's live CR3 here, so `setup_current_task_shadow_stack` maps into and
    // advances init's own address space exactly as the execve path does. Gated
    // on `cet_active`, so it is inert on QEMU (and the `== 0` MSR read is skipped
    // on a no-CET CPU, which would otherwise `#GP`).
    if crate::mitigations::state().is_some_and(|s| s.cet_active)
        && crate::task::scheduler::current_task_cet_ssp_live() == 0
    {
        // SAFETY: the fork trampoline runs in the target task's context with its
        // CR3 live, immediately before the `iretq` to its ring 3.
        if !unsafe { crate::arch::x86_64::cet::setup_current_task_shadow_stack() } {
            // Frame exhaustion — fail closed rather than `iretq` into ring 3 with
            // no shadow stack (the first `CALL` would fault). Kills only this
            // process; for PID 1 that is a fatal boot condition either way.
            crate::arch::x86_64::syscall::terminate_thread_group_and_exit(
                crate::process::current_pid(),
                -9,
            );
        }
    }

    // Write to per-core ForkEntryCtx and pass pointer to assembly trampoline.
    let data =
        crate::smp::per_core() as *const crate::smp::PerCoreData as *mut crate::smp::PerCoreData;
    unsafe {
        (*data).fork_entry_ctx = ForkEntryCtx {
            rip,
            rsp,
            rbx,
            rbp,
            r12,
            r13,
            r14,
            r15,
            ss: u64::from(gdt::user_data_selector().0),
            cs: u64::from(gdt::user_code_selector().0),
            rdi,
            rsi,
            rdx,
            r8,
            r9,
            r10,
            rflags,
        };
        fork_enter_userspace(core::ptr::addr_of!((*data).fork_entry_ctx))
    }
}
