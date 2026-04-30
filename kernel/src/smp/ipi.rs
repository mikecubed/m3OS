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

/// Allocator-local cache drain IPI vector — requests remote cores to self-drain
/// their page cache and, when a reclaim round is active, their slab-local
/// magazines / cross-CPU free lists.
pub const IPI_CACHE_DRAIN: u8 = 0xFC;

// ---------------------------------------------------------------------------
// LAPIC ICR helpers
// ---------------------------------------------------------------------------

pub(super) const LAPIC_ICR_LOW: usize = 0x300;
pub(super) const LAPIC_ICR_HIGH: usize = 0x310;

fn lapic_base() -> usize {
    let phys = crate::acpi::local_apic_address() as u64;
    (crate::mm::phys_offset() + phys) as usize
}

pub(super) unsafe fn lapic_read(offset: usize) -> u32 {
    unsafe { core::ptr::read_volatile((lapic_base() + offset) as *const u32) }
}

pub(super) unsafe fn lapic_write(offset: usize, value: u32) {
    unsafe {
        core::ptr::write_volatile((lapic_base() + offset) as *mut u32, value);
    }
}

pub(super) unsafe fn wait_icr_idle() {
    unsafe {
        // HW-bounded: ~1 µs (Intel SDM Vol 3A §10.6, 'Local APIC ICR Delivery').
        // The LAPIC clears the delivery-pending bit in hardware after the IPI is
        // accepted by the target local APIC.  No software agent holds this condition;
        // converting to block+wake would require an interrupt-on-delivery that the
        // LAPIC does not provide.
        // preempt_disable() wrapper added in Phase 57e Track B (load-bearing for PREEMPT_FULL only).
        while lapic_read(LAPIC_ICR_LOW) & (1 << 12) != 0 {
            core::hint::spin_loop();
        }
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

/// Send an IPI with the given vector to a specific logical core ID.
///
/// Looks up the core's APIC ID from the per-core data table and delegates
/// to `send_ipi`. Returns without sending if the core ID is invalid or
/// the core is not yet online.
pub fn send_ipi_to_core(core_id: u8, vector: u8) {
    if let Some(data) = super::get_core_data(core_id)
        && data.is_online.load(core::sync::atomic::Ordering::Acquire)
    {
        send_ipi(data.apic_id, vector);
    }
}
