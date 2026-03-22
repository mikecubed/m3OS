pub mod gdt;
pub mod interrupts;

/// Initialize architecture-specific structures: GDT/TSS, IDT, PIC.
///
/// After this returns, hardware interrupts are enabled.
pub fn init() {
    gdt::init();
    interrupts::init();
    interrupts::init_pics();
    x86_64::instructions::interrupts::enable();
}
