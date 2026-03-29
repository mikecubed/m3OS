//! TLB shootdown support for SMP.
//!
//! When a page mapping is removed, all cores that might have the mapping
//! cached in their TLB must be notified to invalidate it. This module
//! provides the shootdown request/response mechanism.

use core::sync::atomic::{AtomicU64, AtomicU8, Ordering};

use super::ipi;

// ---------------------------------------------------------------------------
// Shootdown request (shared state)
// ---------------------------------------------------------------------------

/// The virtual address to invalidate (set before sending the IPI).
static SHOOTDOWN_ADDR: AtomicU64 = AtomicU64::new(0);

/// Number of cores that still need to acknowledge the shootdown.
static SHOOTDOWN_PENDING: AtomicU8 = AtomicU8::new(0);

/// Serializes concurrent TLB shootdown requests.
static SHOOTDOWN_LOCK: spin::Mutex<()> = spin::Mutex::new(());

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Count the number of cores that are actually online.
fn online_core_count() -> u8 {
    let mut count = 0u8;
    for i in 0..super::core_count() {
        if let Some(data) = super::get_core_data(i) {
            if data.is_online.load(core::sync::atomic::Ordering::Acquire) {
                count += 1;
            }
        }
    }
    count
}

// ---------------------------------------------------------------------------
// Public API (T031, T034)
// ---------------------------------------------------------------------------

/// Invalidate a page mapping on all cores.
///
/// Executes `invlpg` locally and sends a TLB shootdown IPI to all other
/// online cores. Spins until all cores have acknowledged.
///
/// If only one core is online, skips the IPI (single-core fast path, T034).
pub fn tlb_shootdown(addr: u64) {
    let _lock = SHOOTDOWN_LOCK.lock();

    let online = online_core_count();

    // Always invalidate locally.
    x86_64::instructions::tlb::flush(x86_64::VirtAddr::new(addr));

    if online <= 1 {
        return; // single-core fast path
    }

    // Set up the request.
    SHOOTDOWN_ADDR.store(addr, Ordering::Release);
    SHOOTDOWN_PENDING.store(online - 1, Ordering::Release);

    // Send TLB shootdown IPI to all other cores.
    ipi::send_ipi_all_excluding_self(ipi::IPI_TLB_SHOOTDOWN);

    // Spin-wait for all remote cores to acknowledge.
    while SHOOTDOWN_PENDING.load(Ordering::Acquire) > 0 {
        core::hint::spin_loop();
    }
}

/// Handle a TLB shootdown IPI on the receiving core.
///
/// Called from the IDT handler. Reads the target address, executes `invlpg`,
/// and decrements the pending count.
pub fn handle_tlb_shootdown_ipi() {
    let addr = SHOOTDOWN_ADDR.load(Ordering::Acquire);
    x86_64::instructions::tlb::flush(x86_64::VirtAddr::new(addr));
    SHOOTDOWN_PENDING.fetch_sub(1, Ordering::Release);
}
