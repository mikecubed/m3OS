use core::arch::global_asm;

pub mod apic;
pub mod gdt;
pub mod interrupts;
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
    interrupts::init_pics();
    x86_64::instructions::interrupts::enable();
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

/// Enter ring 3 at `rip` with `rsp` as the stack pointer and `rax` as the
/// return value visible to userspace code (used by `fork` to return 0 to
/// the child).
///
/// # Safety
/// Same requirements as [`enter_userspace`].  `rax` is placed in RAX before
/// `iretq` so the child sees it as its syscall return value.
#[allow(dead_code)]
pub unsafe fn enter_userspace_with_retval(rip: u64, rsp: u64, rax: u64) -> ! {
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

/// Context for entering ring 3 from a fork child, stored in a static so
/// assembly can load register values without running out of register operands.
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
}

/// Static storage for the fork child entry context.
/// Single-CPU: only one fork child enters userspace at a time.
#[no_mangle]
pub static mut FORK_ENTRY_CTX: ForkEntryCtx = ForkEntryCtx {
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
};

// Assembly trampoline: reads ForkEntryCtx, restores registers, IRETs to ring 3.
global_asm!(
    ".global fork_enter_userspace",
    "fork_enter_userspace:",
    // On entry: FORK_ENTRY_CTX is populated.
    // Load callee-saved registers.
    "lea rdi, [rip + FORK_ENTRY_CTX]",
    "mov rbx, [rdi + 16]",
    "mov rbp, [rdi + 24]",
    "mov r12, [rdi + 32]",
    "mov r13, [rdi + 40]",
    "mov r14, [rdi + 48]",
    "mov r15, [rdi + 56]",
    // Load ss selector value into rax temporarily.
    "mov rax, [rdi + 64]", // ss
    "push rax",
    "push [rdi + 8]", // user RSP
    "mov rax, 0x202",
    "push rax",            // RFLAGS
    "mov rax, [rdi + 72]", // cs
    "push rax",
    "push [rdi]",   // user RIP
    "xor eax, eax", // rax = 0 (fork child return)
    "iretq",
);

extern "C" {
    fn fork_enter_userspace() -> !;
}

/// Enter ring 3 for a fork child with full callee-saved register restore.
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
) -> ! {
    FORK_ENTRY_CTX = ForkEntryCtx {
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
    };
    fork_enter_userspace()
}
