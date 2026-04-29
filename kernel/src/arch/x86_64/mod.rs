use core::arch::global_asm;

pub mod apic;
pub mod gdt;
pub mod interrupts;
pub mod ps2;
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

/// Transfer execution to ring 3 (userspace).
///
/// Not used in Phase 6 (kernel-thread IPC demo) — will be re-enabled in
/// Phase 7+ when multi-process userspace is introduced.
#[allow(dead_code)]
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
    unsafe {
        use core::arch::asm;
        asm!(
            "push {ss}",
            "push {rsp}",
            "push {rflags}",
            "push {cs}",
            "push {rip}",
            "iretq",
            ss     = in(reg) u64::from(gdt::user_data_selector().0),
            rsp    = in(reg) user_stack_top,
            rflags = const 0x202u64,
            cs     = in(reg) u64::from(gdt::user_code_selector().0),
            rip    = in(reg) entry,
            options(noreturn)
        )
    }
}

/// Enter ring 3 at `rip` with `rsp` as the stack pointer and `rax` as the
/// return value visible to userspace code (used by `fork` to return 0 to
/// the child).
///
/// # Safety
/// Same requirements as [`enter_userspace`].  `rax` is placed in RAX before
/// `iretq` so the child sees it as its syscall return value.
#[allow(dead_code)]
pub unsafe fn enter_userspace_with_retval(rip: u64, rsp: u64, rax: u64) -> ! {
    // Phase 57b D.3: assert preempt_count == 0 before iretq to ring 3.
    crate::task::scheduler::assert_preempt_count_zero_at_user_return();
    unsafe {
        use core::arch::asm;
        asm!(
            "push {ss}",
            "push {rsp_val}",
            "push {rflags}",
            "push {cs}",
            "push {rip_val}",
            "mov rax, {rax_val}",
            "iretq",
            ss      = in(reg) u64::from(gdt::user_data_selector().0),
            rsp_val = in(reg) rsp,
            rflags  = const 0x202u64,
            cs      = in(reg) u64::from(gdt::user_code_selector().0),
            rip_val = in(reg) rip,
            rax_val = in(reg) rax,
            options(noreturn)
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

// Assembly trampoline: reads ForkEntryCtx from a pointer (rdi), restores ALL
// registers, then IRETs to ring 3.
global_asm!(
    ".global fork_enter_userspace",
    "fork_enter_userspace:",
    // On entry: rdi = pointer to ForkEntryCtx (SysV calling convention).
    "mov rax, rdi",
    // Restore callee-saved registers.
    "mov rbx, [rax + 16]",
    "mov rbp, [rax + 24]",
    "mov r12, [rax + 32]",
    "mov r13, [rax + 40]",
    "mov r14, [rax + 48]",
    "mov r15, [rax + 56]",
    // Restore caller-saved registers (syscall-preserved).
    "mov rsi, [rax + 88]",
    "mov rdx, [rax + 96]",
    "mov r8,  [rax + 104]",
    "mov r9,  [rax + 112]",
    "mov r10, [rax + 120]",
    // Build IRET frame: SS, RSP, RFLAGS, CS, RIP
    "mov rcx, [rax + 64]", // ss
    "push rcx",
    "push [rax + 8]", // user RSP
    // Use saved user RFLAGS (sanitized: ensure IF is set, clear IOPL/VM/RF).
    "mov rcx, [rax + 128]", // user RFLAGS
    "or  rcx, 0x200",       // ensure IF (interrupt enable) is set
    "and ecx, 0x000ED7FF",  // clear IOPL, VM, RF, reserved bits
    "push rcx",
    "mov rcx, [rax + 72]", // cs
    "push rcx",
    "push [rax]", // user RIP
    // Restore rdi AFTER we're done using rax as base (rdi is offset 80).
    "mov rdi, [rax + 80]",
    // RAX = 0 (fork child return value).
    "xor eax, eax",
    "iretq",
);

unsafe extern "C" {
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
