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

/// Per-core counter of `IPI_TLB_SHOOTDOWN` invocations actually serviced by
/// [`handle_tlb_shootdown_ipi`]. Incremented from the IDT entry on each
/// recipient; read by [`wait_for_shootdown_acks_or_panic`] before/after the
/// spin so the timeout diagnostic can dump a per-recipient delta and tell us
/// definitively whether each target's IPI handler fired at all during the
/// hang.
///
/// Sized to [`crate::smp::MAX_CORES`] (16) to match the largest core_id the
/// crate exposes. `Relaxed` ordering is sufficient: this counter is read only
/// from panic / diagnostic context, never participates in correctness
/// synchronisation (`SHOOTDOWN_PENDING` carries the ack semantics).
static TLB_IPI_SERVICED: [AtomicU64; crate::smp::MAX_CORES] =
    [const { AtomicU64::new(0) }; crate::smp::MAX_CORES];

/// RAII guard that temporarily enables interrupts for the lifetime of a TLB
/// shootdown handshake, then restores the previous IF state on `Drop`.
///
/// The shootdown protocol fundamentally requires IF=1 on the spinning core:
/// concurrent kernel-VMA mutations on other cores broadcast their own IPIs
/// to this core, and the only way this core can ack them (and thereby
/// release the *other* sender's spin) is to actually take the interrupt.
/// If we spin with IF=0, every other concurrent shootdown that targets us
/// stalls forever — which collapses into a multi-core IF=0 deadlock the
/// moment two or more cores are doing kernel-VMA mutations at the same
/// time. Diagnosed at 4 GiB guest RAM via
/// `docs/handoffs/2026-05-24-4gib-pci-hole-vga-mapping.md`: every recipient
/// core's per-core `timer Δ` and `tlb-ipi Δ` were both 0 during the 500 ms
/// sender spin — i.e., no recipient ever took *any* interrupt while the
/// sender was waiting — which only happens if every recipient is also
/// inside an IF=0 region.
///
/// Sender-side coverage (this RAII guard) is necessary but not sufficient:
/// recipients that happen to be spinning to acquire an unrelated
/// `IrqSafeMutex` would also be stuck IF=0 and unable to ack our IPI.  The
/// matching recipient-side fix landed in `IrqSafeMutex::lock` on the same
/// day — that path now spins with IF=1 and only masks IF once the lock is
/// held.  Together the two fixes mean every cross-core spin in the kernel
/// keeps IF=1 long enough to take a shootdown IPI.
///
/// We can safely enable interrupts inside the shootdown helpers themselves
/// because:
///   * `preempt_count` is already raised (each shootdown helper calls
///     `preempt_disable` at entry, and outer callers that hold an
///     `IrqSafeMutex` also raised it). A timer tick that lands during the
///     spin sets the reschedule flag but does *not* preempt this task off
///     this core, so the spin loop and its associated atomics keep their
///     identity.
///   * The handlers that can fire in this window are LAPIC timer (atomic
///     tick + EOI, no allocation, no locks contended by the shootdown
///     path) and the TLB-shootdown IPI itself (`handle_tlb_shootdown_ipi`:
///     atomic decrement + local TLB flush + EOI, lock-free).
///
/// The Drop restores IF to whatever it was at construction, so callers
/// that legitimately had IF=0 (e.g., already-CLI'd outer
/// `IrqSafeMutex`) observe no net change after the helper returns —
/// `IrqSafeGuard::Drop` then restores the original (pre-lock) IF state on
/// its own as usual.
struct ShootdownIrqWindow {
    was_enabled: bool,
}

impl ShootdownIrqWindow {
    #[inline(always)]
    fn open() -> Self {
        let was_enabled = x86_64::instructions::interrupts::are_enabled();
        if !was_enabled {
            x86_64::instructions::interrupts::enable();
        }
        Self { was_enabled }
    }
}

impl Drop for ShootdownIrqWindow {
    #[inline(always)]
    fn drop(&mut self) {
        if !self.was_enabled {
            x86_64::instructions::interrupts::disable();
        }
    }
}

/// Spin on `SHOOTDOWN_PENDING > 0` with a generous timeout. On timeout, dump
/// the request state and which APIC IDs we sent to vs the final pending
/// count, then panic. Designed for the 4 GiB-RAM-on-Zen 5 hang investigation
/// (docs/handoffs/2026-05-24-4gib-pci-hole-vga-mapping.md): converts a
/// previously-silent infinite spin into an actionable panic.
///
/// 500 ms is ~5 orders of magnitude over the SDM-spec ~1 µs IPI delivery
/// latency; comfortable margin under TCG without inflating real-hang
/// detection time. tsc_per_ms() returns 0 before APIC calibration; fall
/// back to a ~10G-cycle absolute ceiling (~10 s at 1 GHz) in that window.
fn wait_for_shootdown_acks_or_panic(
    site: &'static str,
    expected_targets: u8,
    range_start: u64,
    range_end: u64,
    recipients_repr: core::fmt::Arguments<'_>,
) {
    let tsc_per_ms = crate::arch::x86_64::apic::tsc_per_ms();
    let timeout_tsc: u64 = if tsc_per_ms > 0 {
        tsc_per_ms.saturating_mul(500)
    } else {
        10_000_000_000
    };
    // Snapshot the per-core IPI-serviced counter so a timeout panic can dump
    // a per-recipient delta and tell us definitively whether each target's
    // IDT handler actually fired during the spin window.
    let mut serviced_at_start = [0u64; crate::smp::MAX_CORES];
    for (i, slot) in TLB_IPI_SERVICED.iter().enumerate() {
        serviced_at_start[i] = slot.load(Ordering::Relaxed);
    }
    // Also snapshot per-core LAPIC timer ticks. The diagnostic discriminates:
    //   * timer-delta > 0, ipi-delta = 0  → core's interrupts work but it
    //     specifically isn't taking vector 0xFD (vector masking, IDT entry
    //     missing, dispatch race) — bug is IPI-specific.
    //   * timer-delta = 0, ipi-delta = 0  → core has IF=0 for the entire
    //     window (or its LAPIC timer is dead) — bug is interrupt-delivery
    //     generic, almost certainly the recursive log path holding IrqSafeMutex.
    //   * timer-delta > 0, ipi-delta > 0  → core ack'd but the atomic
    //     accounting is wrong somewhere.
    let mut timer_at_start = [0u64; crate::smp::MAX_CORES];
    for (i, slot) in crate::arch::x86_64::interrupts::TIMER_TICKS_PER_CORE
        .iter()
        .enumerate()
    {
        timer_at_start[i] = slot.load(Ordering::Relaxed);
    }
    let start_tsc = unsafe { core::arch::x86_64::_rdtsc() };
    let mut iterations: u64 = 0;
    while SHOOTDOWN_PENDING.load(Ordering::Acquire) > 0 {
        core::hint::spin_loop();
        iterations += 1;
        if iterations.is_multiple_of(4096) {
            let now = unsafe { core::arch::x86_64::_rdtsc() };
            if now.wrapping_sub(start_tsc) > timeout_tsc {
                let pending = SHOOTDOWN_PENDING.load(Ordering::Acquire);
                let my_core = super::per_core().core_id;
                let icr_low = unsafe { ipi::lapic_read(ipi::LAPIC_ICR_LOW) };
                // Build per-core "before → after" maps for both counters.
                let mut serviced_now = [0u64; crate::smp::MAX_CORES];
                for (i, slot) in TLB_IPI_SERVICED.iter().enumerate() {
                    serviced_now[i] = slot.load(Ordering::Relaxed);
                }
                let mut timer_now = [0u64; crate::smp::MAX_CORES];
                for (i, slot) in crate::arch::x86_64::interrupts::TIMER_TICKS_PER_CORE
                    .iter()
                    .enumerate()
                {
                    timer_now[i] = slot.load(Ordering::Relaxed);
                }
                // Use _panic_print directly so the dump goes out even if the
                // log infrastructure is wedged on the recursive DMESG_RING
                // path.
                crate::serial::_panic_print(format_args!(
                    "[tlb] {site} stuck >500ms: SHOOTDOWN_PENDING={pending} \
                     (of {expected_targets} targets), my_core={my_core}, \
                     range={range_start:#x}..{range_end:#x}, \
                     ICR_LOW={icr_low:#010x} (bit 12 = delivery-pending), \
                     recipients=[{recipients_repr}], iterations={iterations}, \
                     tsc_per_ms={tsc_per_ms}\n\
                     [tlb] per-core diagnostics — \
                     tlb-ipi serviced (before → after, delta), \
                     LAPIC-timer ticks (before → after, delta):\n"
                ));
                for i in 0..crate::smp::MAX_CORES {
                    let ipi_before = serviced_at_start[i];
                    let ipi_after = serviced_now[i];
                    let tmr_before = timer_at_start[i];
                    let tmr_after = timer_now[i];
                    if ipi_before != 0
                        || ipi_after != 0
                        || tmr_before != 0
                        || tmr_after != 0
                        || i == my_core as usize
                    {
                        let ipi_delta = ipi_after.wrapping_sub(ipi_before);
                        let tmr_delta = tmr_after.wrapping_sub(tmr_before);
                        crate::serial::_panic_print(format_args!(
                            "[tlb]   core {i}: ipi {ipi_before} → {ipi_after} \
                             (Δ{ipi_delta})  timer {tmr_before} → {tmr_after} \
                             (Δ{tmr_delta})\n"
                        ));
                    }
                }
                panic!("[tlb] {site} ack timeout — see per-core dump above");
            }
        }
    }
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
    // Phase 57b — `SHOOTDOWN_LOCK` migration shape: **preempt-only**.
    //
    // The lock holder broadcasts an IPI to every other online core and
    // spins on `SHOOTDOWN_PENDING` until each acks.  IF MUST stay enabled
    // throughout the lock-held region: a contending core that takes the
    // lock with IF=0 (or waits on the lock with IF=0) cannot service the
    // shootdown IPI from the holder, deadlocking both cores.  The
    // `tlb_shootdown_ipi_handler` itself never touches `SHOOTDOWN_LOCK`,
    // so re-entry of an IRQ handler on the holder's core during the wait
    // is safe.
    //
    // `preempt_disable` is required (Phase 57b F semantic): keeps the
    // task pinned across the IPI broadcast so 57d/57e cannot preempt the
    // holder mid-handshake.  It is lock-free (Phase 57b D.2) so it cannot
    // recurse through this caller.
    crate::task::scheduler::preempt_disable();
    'critical: {
        let _lock = SHOOTDOWN_LOCK.lock();

        // Always invalidate locally.
        x86_64::instructions::tlb::flush(x86_64::VirtAddr::new(addr));

        // Snapshot recipients in a single predicate walk. The count stored
        // in SHOOTDOWN_PENDING and the IPIs we actually send below MUST be
        // by construction equal — there is no second walk of get_core_data /
        // is_online, so an AP that flips online or offline after this point
        // cannot cause an underflow or hang. We capture apic_id directly so
        // the send loop calls send_ipi() unconditionally rather than
        // round-tripping through send_ipi_to_core's is_online re-check
        // (which would re-introduce the same TOCTTOU).
        let my_core = super::per_core().core_id;
        let mut recipients = [0u8; crate::smp::MAX_CORES];
        let mut targets: usize = 0;
        for core_id in 0..crate::smp::MAX_CORES as u8 {
            if core_id == my_core {
                continue;
            }
            if let Some(data) = super::get_core_data(core_id)
                && data.is_online.load(Ordering::Acquire)
                && targets < recipients.len()
            {
                recipients[targets] = data.apic_id;
                targets += 1;
            }
        }

        if targets == 0 {
            // Uniprocessor or every other core offline — local flush is
            // sufficient. Exit the critical block so _lock drops BEFORE the
            // post-block preempt_enable runs — re-enabling preemption while
            // the mutex is still held risks a same-core deadlock if a
            // preempted task tries another TLB shootdown.
            break 'critical;
        }

        // Clear range state so the IPI handler uses the legacy single-address path.
        SHOOTDOWN_RANGE_START.store(0, Ordering::Release);
        SHOOTDOWN_RANGE_END.store(0, Ordering::Release);

        // Set up the request before publishing IPIs.
        SHOOTDOWN_ADDR.store(addr, Ordering::Release);
        SHOOTDOWN_PENDING.store(targets as u8, Ordering::Release);

        // Send to exactly the snapshot we counted — same set, by construction.
        for &apic_id in &recipients[..targets] {
            ipi::send_ipi(apic_id, ipi::IPI_TLB_SHOOTDOWN);
        }

        // Spin-wait for all remote cores to acknowledge. `ShootdownIrqWindow`
        // enforces IF=1 for the duration of the spin so this core can
        // service incoming IPIs from other cores' concurrent shootdowns
        // (and so its own LAPIC timer keeps firing). Even when an outer
        // caller holds an `IrqSafeMutex` (which CLI'd at acquire), the
        // window enables IF here and restores the prior state on Drop —
        // the lock guard's own `InterruptRestore::drop` then restores the
        // original pre-lock IF state on lock release.
        let _irq_window = ShootdownIrqWindow::open();
        wait_for_shootdown_acks_or_panic(
            "tlb_shootdown",
            targets as u8,
            addr,
            addr + 4096,
            format_args!("{:?}", &recipients[..targets]),
        );
    }
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
    // Phase 57b — preempt-only migration shape; same rationale as
    // [`tlb_shootdown`].  IF must stay enabled across the lock-held region
    // so contending cores can service this core's IPIs.
    crate::task::scheduler::preempt_disable();
    'critical: {
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
            // No remote cores have this address space loaded — exit the
            // critical block, then `preempt_enable` below.
            break 'critical;
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
            break 'critical;
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

        // Spin-wait for acknowledgment from all targeted cores. See the
        // matching note + `ShootdownIrqWindow` rationale in `tlb_shootdown`
        // above — IF MUST be 1 during the spin so this core services
        // peers' concurrent shootdowns and ack-deadlock is avoided.
        let _irq_window = ShootdownIrqWindow::open();
        wait_for_shootdown_acks_or_panic(
            "tlb_shootdown_range",
            targets,
            aligned_start,
            aligned_end,
            format_args!("remote_mask={:#018x}", remote_mask),
        );
    }
    crate::task::scheduler::preempt_enable();
}

/// Invalidate a range of kernel-shared page mappings on every online core.
///
/// Same handshake structure as [`tlb_shootdown_range`] but skips the
/// per-address-space `active_cores` filter — used when the affected
/// mapping lives in the upper half of every process's PML4 (heap grow,
/// future kernel-VMA mutations), where every online core is potentially
/// holding a stale TLB or paging-structure-cache entry regardless of
/// which userspace process is currently active on it.
///
/// `start` and `end` are inclusive/exclusive virtual-address bounds; both
/// are page-aligned internally. Falls back to a local-only flush on a
/// uniprocessor system.
pub fn tlb_shootdown_range_kernel(start: u64, end: u64) {
    // Phase 57b — preempt-only migration shape; same rationale as
    // [`tlb_shootdown`]. IF must stay enabled across the lock-held region
    // so contending cores can service this core's IPIs.
    crate::task::scheduler::preempt_disable();
    'critical: {
        let _lock = SHOOTDOWN_LOCK.lock();

        // Align the range to page boundaries so every page intersecting
        // [start, end) is invalidated, even when start or end are not aligned.
        let aligned_start = start & !(4096 - 1);
        let aligned_end = end.saturating_add(4096 - 1) & !(4096 - 1);

        let page_count = aligned_end.saturating_sub(aligned_start).div_ceil(4096);
        let use_cr3_reload = page_count > INVLPG_THRESHOLD;

        // Local flush first.
        if use_cr3_reload {
            let (frame, flags) = x86_64::registers::control::Cr3::read();
            unsafe {
                x86_64::registers::control::Cr3::write(frame, flags);
            }
        } else {
            let mut addr = aligned_start;
            while addr < aligned_end {
                x86_64::instructions::tlb::flush(x86_64::VirtAddr::new(addr));
                addr += 4096;
            }
        }

        // Snapshot recipients in a single predicate walk. Capturing the
        // recipient `apic_id`s up front (rather than walking the predicate
        // again to send) closes the count-vs-send TOCTTOU: an AP that flips
        // `is_online` between two walks would otherwise receive an IPI but
        // not be reflected in `SHOOTDOWN_PENDING`, underflowing the u8.
        //
        // The send loop calls `send_ipi(apic_id, ...)` directly rather than
        // round-tripping through `send_ipi_to_core`, whose `is_online`
        // re-check would reintroduce the same race in a different
        // direction (a recipient that went *offline* between count and send
        // would silently skip its decrement).
        let my_core = super::per_core().core_id;
        let mut recipients = [0u8; crate::smp::MAX_CORES];
        let mut targets: usize = 0;
        for core_id in 0..crate::smp::MAX_CORES as u8 {
            if core_id == my_core {
                continue;
            }
            if let Some(data) = super::get_core_data(core_id)
                && data.is_online.load(Ordering::Acquire)
                && targets < recipients.len()
            {
                recipients[targets] = data.apic_id;
                targets += 1;
            }
        }

        if targets == 0 {
            // Uniprocessor or every other core is offline — local flush is
            // sufficient.
            break 'critical;
        }

        SHOOTDOWN_RANGE_START.store(aligned_start, Ordering::Release);
        SHOOTDOWN_RANGE_END.store(aligned_end, Ordering::Release);
        SHOOTDOWN_USE_CR3_RELOAD.store(use_cr3_reload, Ordering::Release);
        SHOOTDOWN_PENDING.store(targets as u8, Ordering::Release);

        // Send to exactly the snapshot. Recipients and pending count are
        // by construction equal — there is no second predicate walk that
        // could disagree with the snapshot.
        for &apic_id in &recipients[..targets] {
            ipi::send_ipi(apic_id, ipi::IPI_TLB_SHOOTDOWN);
        }

        // IPI-bounded spin; same IF/preempt discipline as
        // `tlb_shootdown_range`. `ShootdownIrqWindow` keeps IF=1 across
        // the wait even when callers (e.g. `mm::heap::grow_heap`) hold an
        // outer `IrqSafeMutex` whose acquire would otherwise leave IF=0
        // — the prior bug that caused the 4 GiB-on-Zen-5 deadlock.
        let _irq_window = ShootdownIrqWindow::open();
        wait_for_shootdown_acks_or_panic(
            "tlb_shootdown_range_kernel",
            targets as u8,
            aligned_start,
            aligned_end,
            format_args!("{:?}", &recipients[..targets]),
        );
    }
    crate::task::scheduler::preempt_enable();
}

/// Handle a TLB shootdown IPI on the receiving core.
///
/// Called from the IDT handler. Reads the target address or range, executes
/// the appropriate flush, and decrements the pending count.
pub fn handle_tlb_shootdown_ipi() {
    // Diagnostic counter for the 4 GiB-hang investigation: bump the
    // per-core ack count before doing any TLB work. If a sender times out
    // waiting for our ack, `wait_for_shootdown_acks_or_panic` reads this
    // counter to determine whether our IDT entry fired at all.
    if let Some(pc) = super::try_per_core()
        && let Some(slot) = TLB_IPI_SERVICED.get(pc.core_id as usize)
    {
        slot.fetch_add(1, Ordering::Relaxed);
    }

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
