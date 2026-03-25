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
