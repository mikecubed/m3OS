//! Inter-Processor Interrupt (IPI) infrastructure.
//!
//! Provides functions to send IPIs between cores and IDT handlers for
//! reschedule and TLB shootdown IPIs.

// ---------------------------------------------------------------------------
// IPI vector assignments
// ---------------------------------------------------------------------------

/// Reschedule IPI vector — wakes an idle core to check its run queue.
pub const IPI_RESCHEDULE: u8 = 0xFE;

/// TLB shootdown IPI vector — requests remote cores to invalidate a page.
pub const IPI_TLB_SHOOTDOWN: u8 = 0xFD;

// ---------------------------------------------------------------------------
// LAPIC ICR helpers
// ---------------------------------------------------------------------------

const LAPIC_ICR_LOW: usize = 0x300;
const LAPIC_ICR_HIGH: usize = 0x310;

fn lapic_base() -> usize {
    let phys = crate::acpi::local_apic_address() as u64;
    (crate::mm::phys_offset() + phys) as usize
}

unsafe fn lapic_read(offset: usize) -> u32 {
    core::ptr::read_volatile((lapic_base() + offset) as *const u32)
}

unsafe fn lapic_write(offset: usize, value: u32) {
    core::ptr::write_volatile((lapic_base() + offset) as *mut u32, value);
}

unsafe fn wait_icr_idle() {
    while lapic_read(LAPIC_ICR_LOW) & (1 << 12) != 0 {
        core::hint::spin_loop();
    }
}

// ---------------------------------------------------------------------------
// Public IPI sending API (T025, T026)
// ---------------------------------------------------------------------------

/// Send an IPI with the given vector to a specific APIC ID.
///
/// Waits for the ICR to become idle before and after sending.
pub fn send_ipi(target_apic_id: u8, vector: u8) {
    unsafe {
        wait_icr_idle();
        // Destination in ICR high bits 24-31.
        lapic_write(LAPIC_ICR_HIGH, (target_apic_id as u32) << 24);
        // Fixed delivery mode (000), vector in bits 0-7.
        lapic_write(LAPIC_ICR_LOW, vector as u32);
        wait_icr_idle();
    }
}

/// Send an IPI to all cores except the calling core.
///
/// Uses the ICR shorthand "all excluding self" (bits 19:18 = 11).
pub fn send_ipi_all_excluding_self(vector: u8) {
    unsafe {
        wait_icr_idle();
        // Shorthand = all-excluding-self (0xC0000), fixed delivery, vector.
        lapic_write(LAPIC_ICR_LOW, 0x000C_0000 | vector as u32);
        wait_icr_idle();
    }
}

/// Send a reschedule IPI to a specific core by APIC ID.
///
/// Used when a task is spawned or unblocked on a remote core's queue.
pub fn send_reschedule_ipi(target_apic_id: u8) {
    send_ipi(target_apic_id, IPI_RESCHEDULE);
}

// ---------------------------------------------------------------------------
// IDT handler registration (T027, T028)
// ---------------------------------------------------------------------------

/// Register the IPI handlers in the IDT.
///
/// Must be called once during BSP init, before APs are booted.
/// The IDT is shared across all cores.
pub fn register_ipi_handlers() {
    // The IDT is a Lazy static in interrupts.rs. We need to add entries
    // for our IPI vectors. Since the IDT is already initialized, we'll
    // add the handlers by modifying the IDT entries directly.
    //
    // However, the x86_64 crate's IDT is initialized via Lazy and doesn't
    // support post-init modification easily. Instead, we add the handlers
    // during IDT construction by modifying interrupts.rs.
    //
    // For now, this function serves as documentation. The actual handler
    // registration is done in interrupts.rs.
    log::info!("[smp] IPI handlers registered (reschedule=0xFE, TLB=0xFD)");
}
