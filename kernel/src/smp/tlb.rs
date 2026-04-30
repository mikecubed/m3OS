//! TLB shootdown support for SMP.
//!
//! When a page mapping is removed, all cores that might have the mapping
//! cached in their TLB must be notified to invalidate it. This module
//! provides the shootdown request/response mechanism.
//!
//! Two APIs are available:
//! - [`tlb_shootdown`]: single-address broadcast to all online cores.
//! - [`tlb_shootdown_range`]: range-based targeted shootdown using
//!   [`AddressSpace::active_cores`] to send IPIs only to the cores that
//!   have the affected address space loaded. For large ranges (above
//!   [`INVLPG_THRESHOLD`] pages), uses a full CR3 reload instead of
//!   per-page `invlpg`.

use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};

use x86_64::instructions::interrupts;

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
// Range-based shootdown state (Phase 52b, Track B)
// ---------------------------------------------------------------------------

/// Start of the virtual address range to invalidate (inclusive).
static SHOOTDOWN_RANGE_START: AtomicU64 = AtomicU64::new(0);

/// End of the virtual address range to invalidate (exclusive).
static SHOOTDOWN_RANGE_END: AtomicU64 = AtomicU64::new(0);

/// When true, remote cores should do a full CR3 reload instead of per-page
/// `invlpg`. Set when the number of pages exceeds [`INVLPG_THRESHOLD`].
static SHOOTDOWN_USE_CR3_RELOAD: AtomicBool = AtomicBool::new(false);

/// Above this many pages, a full CR3 reload is cheaper than iterating
/// `invlpg` for each page.
const INVLPG_THRESHOLD: u64 = 32;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Count the number of cores that are actually online.
fn online_core_count() -> u8 {
    let mut count = 0u8;
    for i in 0..super::core_count() {
        if let Some(data) = super::get_core_data(i)
            && data.is_online.load(core::sync::atomic::Ordering::Acquire)
        {
            count += 1;
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
    // Phase 57b G.8 — `SHOOTDOWN_LOCK` is classified `explicit-preempt-and-cli`
    // per Track A.1 audit (IRQ-shared via `tlb_shootdown_ipi_handler`). Wrap
    // the critical section in `preempt_disable` + `without_interrupts` +
    // `preempt_enable` so the F.1 preempt-discipline stays balanced.
    // `preempt_disable` is lock-free (Phase 57b D.2), so calling it before
    // `without_interrupts` cannot recurse.
    crate::task::scheduler::preempt_disable();
    interrupts::without_interrupts(|| {
        let _lock = SHOOTDOWN_LOCK.lock();

        let online = online_core_count();

        // Always invalidate locally.
        x86_64::instructions::tlb::flush(x86_64::VirtAddr::new(addr));

        if online <= 1 {
            return; // single-core fast path
        }

        // Clear range state so the IPI handler uses the legacy single-address path.
        SHOOTDOWN_RANGE_START.store(0, Ordering::Release);
        SHOOTDOWN_RANGE_END.store(0, Ordering::Release);

        // Set up the request.
        SHOOTDOWN_ADDR.store(addr, Ordering::Release);
        SHOOTDOWN_PENDING.store(online - 1, Ordering::Release);

        // Send TLB shootdown IPI to all other cores.
        ipi::send_ipi_all_excluding_self(ipi::IPI_TLB_SHOOTDOWN);

        // Spin-wait for all remote cores to acknowledge. Remote cores keep
        // their own IF=1 and run the shootdown handler without contending
        // for `SHOOTDOWN_LOCK` (the IPI handler only touches the
        // SHOOTDOWN_* atomics).
        while SHOOTDOWN_PENDING.load(Ordering::Acquire) > 0 {
            core::hint::spin_loop();
        }
    });
    crate::task::scheduler::preempt_enable();
}

/// Invalidate a range of page mappings on targeted cores.
///
/// Uses [`crate::mm::AddressSpace::active_cores`] to send IPIs only to
/// cores that have the affected address space loaded. For ranges over
/// [`INVLPG_THRESHOLD`] pages, uses a full CR3 reload instead of per-page
/// `invlpg`.
///
/// Falls back to a local-only flush if no remote cores are active.
pub fn tlb_shootdown_range(addr_space: &crate::mm::AddressSpace, start: u64, end: u64) {
    // Phase 57b G.8 — `SHOOTDOWN_LOCK` is `explicit-preempt-and-cli` per
    // Track A.1 audit. Wrap the critical section in `preempt_disable` +
    // `without_interrupts` + `preempt_enable` so the F.1 preempt-discipline
    // stays balanced. `preempt_disable` is lock-free (Phase 57b D.2), so
    // calling it before `without_interrupts` cannot recurse.
    crate::task::scheduler::preempt_disable();
    interrupts::without_interrupts(|| {
        let _lock = SHOOTDOWN_LOCK.lock();

        // Align the range to page boundaries so every page intersecting
        // [start, end) is invalidated, even when start or end are not aligned.
        let aligned_start = start & !(4096 - 1);
        let aligned_end = end.saturating_add(4096 - 1) & !(4096 - 1);

        // Base the threshold decision on the aligned flush range.
        let page_count = aligned_end.saturating_sub(aligned_start).div_ceil(4096);
        let use_cr3_reload = page_count > INVLPG_THRESHOLD;

        // Local flush first.
        if use_cr3_reload {
            // Full TLB flush via CR3 reload.
            let (frame, flags) = x86_64::registers::control::Cr3::read();
            unsafe {
                x86_64::registers::control::Cr3::write(frame, flags);
            }
        } else {
            // Per-page invlpg.
            let mut addr = aligned_start;
            while addr < aligned_end {
                x86_64::instructions::tlb::flush(x86_64::VirtAddr::new(addr));
                addr += 4096;
            }
        }

        // Find remote cores that need flushing.
        let active = addr_space.active_cores();
        let my_core = super::per_core().core_id;
        let remote_mask = active & !(1u64 << my_core);

        if remote_mask == 0 {
            return; // No remote cores have this address space loaded.
        }

        // Set up range request for the IPI handler (pass aligned boundaries).
        SHOOTDOWN_RANGE_START.store(aligned_start, Ordering::Release);
        SHOOTDOWN_RANGE_END.store(aligned_end, Ordering::Release);
        SHOOTDOWN_USE_CR3_RELOAD.store(use_cr3_reload, Ordering::Release);

        // Count targeted cores first so the pending count is initialized before
        // any IPI is sent — otherwise a remote core can handle the IPI and
        // fetch_sub(1) while SHOOTDOWN_PENDING is still 0, causing underflow.
        //
        // Use get_core_data + is_online per bit instead of online_core_count()
        // as an upper bound — online_core_count() is a count, not a max core_id,
        // and would skip higher-numbered cores if a lower-numbered core is offline.
        let mut targets = 0u8;
        for core_id in 0..64u8 {
            if remote_mask & (1u64 << core_id) != 0
                && let Some(data) = super::get_core_data(core_id)
                && data.is_online.load(Ordering::Acquire)
            {
                targets = targets.saturating_add(1);
            }
        }

        if targets == 0 {
            return;
        }

        SHOOTDOWN_PENDING.store(targets, Ordering::Release);

        // Now that the pending count is visible, send the IPIs.
        // send_ipi_to_core already checks existence + is_online, matching the
        // count above.
        for core_id in 0..64u8 {
            if remote_mask & (1u64 << core_id) != 0 {
                ipi::send_ipi_to_core(core_id, ipi::IPI_TLB_SHOOTDOWN);
            }
        }

        // Spin-wait for acknowledgment from all targeted cores.
        while SHOOTDOWN_PENDING.load(Ordering::Acquire) > 0 {
            core::hint::spin_loop();
        }
    });
    crate::task::scheduler::preempt_enable();
}

/// Handle a TLB shootdown IPI on the receiving core.
///
/// Called from the IDT handler. Reads the target address or range, executes
/// the appropriate flush, and decrements the pending count.
pub fn handle_tlb_shootdown_ipi() {
    let start = SHOOTDOWN_RANGE_START.load(Ordering::Acquire);
    let end = SHOOTDOWN_RANGE_END.load(Ordering::Acquire);

    if start == 0 && end == 0 {
        // Legacy single-address shootdown.
        let addr = SHOOTDOWN_ADDR.load(Ordering::Acquire);
        x86_64::instructions::tlb::flush(x86_64::VirtAddr::new(addr));
    } else if SHOOTDOWN_USE_CR3_RELOAD.load(Ordering::Acquire) {
        // Large range: full TLB flush via CR3 reload.
        let (frame, flags) = x86_64::registers::control::Cr3::read();
        unsafe {
            x86_64::registers::control::Cr3::write(frame, flags);
        }
    } else {
        // Small range: per-page invlpg. Align so every page intersecting
        // [start, end) is invalidated, mirroring tlb_shootdown_range.
        let aligned_start = start & !(4096 - 1);
        let aligned_end = end.saturating_add(4096 - 1) & !(4096 - 1);
        let mut addr = aligned_start;
        while addr < aligned_end {
            x86_64::instructions::tlb::flush(x86_64::VirtAddr::new(addr));
            addr += 4096;
        }
    }

    SHOOTDOWN_PENDING.fetch_sub(1, Ordering::Release);
}
