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
    // Push the five values that `iretq` pops in order:
    //   SS, RSP, RFLAGS, CS, RIP
    // RFLAGS: bit 9 (IF) set → interrupts enabled in userspace.
    asm!(
        "push {ss}",
        "push {rsp}",
        "push {rflags}",
        "push {cs}",
        "push {rip}",
        "iretq",
        ss     = in(reg) u64::from(gdt::USER_DATA_SELECTOR),
        rsp    = in(reg) user_stack_top,
        rflags = const 0x202u64, // IF=1 (bit 9) + reserved bit 1 (always must be 1)
        cs     = in(reg) u64::from(gdt::USER_CODE_SELECTOR),
        rip    = in(reg) entry,
        options(noreturn)
    )
}
