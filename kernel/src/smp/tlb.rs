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

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use super::ipi;

// ---------------------------------------------------------------------------
// Shootdown request (shared state)
// ---------------------------------------------------------------------------

/// The virtual address to invalidate (set before sending the IPI).
static SHOOTDOWN_ADDR: AtomicU64 = AtomicU64::new(0);

/// Per-round acknowledgement bitmap. Bit `i` is set by core `i`'s IPI handler
/// ([`handle_tlb_shootdown_ipi`]) once it has flushed the in-flight shootdown.
/// The sender resets this to `0` (under [`SHOOTDOWN_LOCK`]) immediately before
/// publishing the NMIs and then waits until `(SHOOTDOWN_ACK & target_mask) ==
/// target_mask`.
///
/// This replaces the pre-Phase-90b decrementing `SHOOTDOWN_PENDING` count
/// (Track C of
/// `docs/handoffs/2026-06-14-claude-smp-tlb-shootdown-kstack-panic.md`). A
/// per-round, per-core *idempotent* bitmap makes the timeout-degrade path
/// (re-NMI the laggards, then mark them offline and continue rather than
/// `panic!`) provably safe: a stale NMI latched from an *abandoned* earlier
/// round can only ever set a bit *outside* the current round's `target_mask`
/// (the abandoned core was marked offline and is excluded from this round), so
/// it is ignored. A shared decrementing counter, by contrast, could underflow,
/// double-decrement (re-NMI), or have a late ACK corrupt a *subsequent*
/// round's count → a silent stale TLB. `MAX_CORES` (16) ≤ 64, so one `u64`
/// covers every core id.
static SHOOTDOWN_ACK: AtomicU64 = AtomicU64::new(0);

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
/// recipient; read by [`wait_for_shootdown_acks`] before/after the spin so the
/// timeout diagnostic can dump a per-recipient delta and tell us definitively
/// whether each target's IPI handler fired at all during the hang.
///
/// Sized to [`crate::smp::MAX_CORES`] (16) to match the largest core_id the
/// crate exposes. `Relaxed` ordering is sufficient: this counter is read only
/// from diagnostic context, never participates in correctness synchronisation
/// ([`SHOOTDOWN_ACK`] carries the ack semantics).
static TLB_IPI_SERVICED: [AtomicU64; crate::smp::MAX_CORES] =
    [const { AtomicU64::new(0) }; crate::smp::MAX_CORES];

// `ShootdownIrqWindow` (sessions 2 + 3) is no longer needed: TLB shootdown
// IPIs are now delivered as NMIs (see `smp::ipi::send_nmi` and
// `arch::x86_64::interrupts::nmi_handler`), which bypass the recipient's
// `IF` mask entirely. The sender's spin no longer needs to keep IF=1 to
// let other senders' shootdowns flow through this core — incoming NMIs
// fire regardless of IF. The struct and its `open()` / `Drop` pair were
// removed in session 5 (2026-05-25). See
// `docs/handoffs/2026-05-24-4gib-pci-hole-vga-mapping.md`.

/// How long the sender spins for the initial ack window before escalating to
/// the re-NMI + degrade path. 500 ms is ~5 orders of magnitude over the
/// SDM-spec ~1 µs IPI delivery latency; comfortable margin under TCG without
/// inflating real-hang detection time. `tsc_per_ms()` returns 0 before APIC
/// calibration; fall back to a ~10G-cycle absolute ceiling (~10 s at 1 GHz).
fn ack_timeout_tsc(tsc_per_ms: u64) -> u64 {
    if tsc_per_ms > 0 {
        tsc_per_ms.saturating_mul(500)
    } else {
        10_000_000_000
    }
}

/// Grace window after the re-NMI before the degrade gives up on a core. Much
/// shorter than the initial window — by this point the core has already had
/// 500 ms + a fresh NMI, so a few hundred ms is plenty for any merely-slow
/// core to ack, and an unresponsive one is wedged for good.
fn ack_regrace_tsc(tsc_per_ms: u64) -> u64 {
    if tsc_per_ms > 0 {
        tsc_per_ms.saturating_mul(100)
    } else {
        2_000_000_000
    }
}

/// Wait until every targeted core has acknowledged the in-flight shootdown by
/// setting its bit in [`SHOOTDOWN_ACK`].
///
/// **Degrades instead of panicking** (Track C of
/// `docs/handoffs/2026-06-14-claude-smp-tlb-shootdown-kstack-panic.md`). On the
/// happy path (the overwhelming default) it returns as soon as
/// `(SHOOTDOWN_ACK & target_mask) == target_mask`. On the initial-window
/// timeout it (1) dumps the per-core diagnostic, (2) re-NMIs the still-
/// outstanding cores once and waits a short grace window, and (3) if any core
/// *still* hasn't acked, marks that core offline (so future shootdowns exclude
/// it for free — Track B) and **returns** rather than `panic!`ing the whole
/// machine. With the Track A NMI-on-IST stack a wedged core services the
/// shootdown NMI on a clean per-core stack and acks even while halted, so the
/// degrade path should be effectively unreachable; reaching it at all means a
/// core is genuinely dark, and abandoning its stale TLB (it will never run
/// userspace again) is strictly better than killing the box.
fn wait_for_shootdown_acks(site: &'static str, target_mask: u64, range_start: u64, range_end: u64) {
    let tsc_per_ms = crate::arch::x86_64::apic::tsc_per_ms();
    let timeout_tsc = ack_timeout_tsc(tsc_per_ms);

    // Snapshot the per-core IPI-serviced + LAPIC-timer counters so a timeout
    // dump can show a per-recipient before→after delta and tell us
    // definitively whether each target's IDT handler fired at all during the
    // window. The discriminator:
    //   * timer-delta > 0, ipi-delta = 0  → core's interrupts work but it
    //     specifically isn't taking the shootdown NMI (delivery race) —
    //     IPI-specific.
    //   * timer-delta = 0, ipi-delta = 0  → core had IF=0 the whole window (or
    //     its LAPIC timer is dead) — generic interrupt-delivery wedge.
    //   * timer-delta > 0, ipi-delta > 0  → core serviced but its ack bit
    //     accounting is wrong somewhere.
    let mut serviced_at_start = [0u64; crate::smp::MAX_CORES];
    for (i, slot) in TLB_IPI_SERVICED.iter().enumerate() {
        serviced_at_start[i] = slot.load(Ordering::Relaxed);
    }
    let mut timer_at_start = [0u64; crate::smp::MAX_CORES];
    for (i, slot) in crate::arch::x86_64::interrupts::TIMER_TICKS_PER_CORE
        .iter()
        .enumerate()
    {
        timer_at_start[i] = slot.load(Ordering::Relaxed);
    }

    // Phase 1 — initial ack window.
    if spin_until_acked(target_mask, timeout_tsc) {
        return;
    }

    // Timeout. Dump the per-core diagnostic before escalating.
    let outstanding = target_mask & !SHOOTDOWN_ACK.load(Ordering::Acquire);
    dump_ack_timeout_diagnostic(
        site,
        target_mask,
        outstanding,
        range_start,
        range_end,
        tsc_per_ms,
        &serviced_at_start,
        &timer_at_start,
    );

    // Phase 2 — Track C re-NMI: poke the still-outstanding cores once more,
    // then wait a short grace window. `send_nmi_to_core` re-checks `is_online`,
    // so a core already taken offline elsewhere is skipped (it can't ack and
    // will be abandoned below). NMIs coalesce, so a laggard whose original NMI
    // is still pending won't double-service.
    for core_id in 0..crate::smp::MAX_CORES as u8 {
        if outstanding & (1u64 << core_id) != 0 {
            ipi::send_nmi_to_core(core_id);
        }
    }
    if spin_until_acked(target_mask, ack_regrace_tsc(tsc_per_ms)) {
        crate::serial::_panic_print(format_args!(
            "[tlb] {site}: recovered after re-NMI — all targets acked in the \
             grace window (machine survives)\n"
        ));
        return;
    }

    // Phase 3 — degrade. Any core still outstanding is dark. Mark it offline
    // (Track B: future shootdowns exclude it for free) and continue. Its bit
    // staying clear in this round's mask is harmless; a stale NMI it might
    // service much later can only set a bit outside a future round's
    // target_mask (it is offline now), so it cannot corrupt later rounds.
    let still = target_mask & !SHOOTDOWN_ACK.load(Ordering::Acquire);
    for core_id in 0..crate::smp::MAX_CORES as u8 {
        if still & (1u64 << core_id) != 0
            && let Some(data) = super::get_core_data(core_id)
        {
            data.is_online.store(false, Ordering::Release);
        }
    }
    crate::serial::_panic_print(format_args!(
        "[tlb] {site}: DEGRADED — {n} core(s) (mask={still:#018x}) failed to ack \
         a TLB shootdown within the grace window; marked offline and \
         continuing. The machine survives (was a whole-system panic before \
         Track C); the abandoned cores' stale TLBs are dropped — a halted core \
         never runs userspace again.\n",
        n = still.count_ones(),
    ));
}

/// Spin until `(SHOOTDOWN_ACK & target_mask) == target_mask` or `timeout_tsc`
/// cycles elapse. Returns `true` if all targets acked, `false` on timeout.
fn spin_until_acked(target_mask: u64, timeout_tsc: u64) -> bool {
    let start_tsc = unsafe { core::arch::x86_64::_rdtsc() };
    let mut iterations: u64 = 0;
    loop {
        if (SHOOTDOWN_ACK.load(Ordering::Acquire) & target_mask) == target_mask {
            return true;
        }
        core::hint::spin_loop();
        iterations += 1;
        if iterations.is_multiple_of(4096) {
            let now = unsafe { core::arch::x86_64::_rdtsc() };
            if now.wrapping_sub(start_tsc) > timeout_tsc {
                return false;
            }
        }
    }
}

/// Emit the per-core ack-timeout diagnostic via `_panic_print` (so it survives
/// even if the `log`/`DMESG_RING` path is wedged). Non-fatal — the caller
/// escalates to re-NMI + degrade after this.
#[allow(clippy::too_many_arguments)]
fn dump_ack_timeout_diagnostic(
    site: &'static str,
    target_mask: u64,
    outstanding: u64,
    range_start: u64,
    range_end: u64,
    tsc_per_ms: u64,
    serviced_at_start: &[u64; crate::smp::MAX_CORES],
    timer_at_start: &[u64; crate::smp::MAX_CORES],
) {
    let my_core = super::per_core().core_id;
    let icr_low = unsafe { ipi::lapic_read(ipi::LAPIC_ICR_LOW) };
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
    crate::serial::_panic_print(format_args!(
        "[tlb] {site} stuck >500ms: outstanding_mask={outstanding:#018x} \
         (of target_mask={target_mask:#018x}), my_core={my_core}, \
         range={range_start:#x}..{range_end:#x}, \
         ICR_LOW={icr_low:#010x} (bit 12 = delivery-pending), \
         tsc_per_ms={tsc_per_ms} — escalating to re-NMI + degrade (no panic)\n\
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
    // spins on `SHOOTDOWN_ACK` until each acks.  IF MUST stay enabled
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

        // Snapshot recipients in a single predicate walk. We capture both the
        // recipient `apic_id` (so the send loop calls `send_nmi()` directly,
        // avoiding `send_nmi_to_core`'s is_online re-check) and the recipient
        // `core_id` as a bit in `target_mask`. The mask we wait on and the
        // NMIs we actually send are derived from this one snapshot, so an AP
        // that flips online/offline after this point cannot desync them: a
        // core that goes offline post-snapshot simply never sets its ack bit
        // and is handled by the wait's re-NMI + degrade path.
        let my_core = super::per_core().core_id;
        let mut recipients = [0u8; crate::smp::MAX_CORES];
        let mut target_mask: u64 = 0;
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
                target_mask |= 1u64 << core_id;
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

        // Set up the request and reset the ack bitmap before publishing the
        // NMIs. Resetting under the lock (which serializes rounds) means any
        // stale bit left by a prior abandoned round is cleared here.
        SHOOTDOWN_ADDR.store(addr, Ordering::Release);
        SHOOTDOWN_ACK.store(0, Ordering::Release);

        // Send to exactly the snapshot. NMI delivery (vs Fixed-mode IPI) so the
        // recipient cores service the shootdown even when they are inside a
        // CLI'd IrqSafeMutex region. See `arch::x86_64::interrupts::nmi_handler`
        // for the receiver and
        // `docs/handoffs/2026-05-24-4gib-pci-hole-vga-mapping.md` for the bug
        // class this addresses.
        for &apic_id in &recipients[..targets] {
            ipi::send_nmi(apic_id);
        }

        // Wait for every targeted core's ack bit. With NMI delivery recipients
        // service the shootdown regardless of their IF state, so the sender no
        // longer needs to keep IF=1 here.
        wait_for_shootdown_acks("tlb_shootdown", target_mask, addr, addr + 4096);
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

        // Build the ack target_mask from the online subset of `remote_mask` in
        // a single walk; the NMIs we send below are derived from the same mask,
        // so the set we wait on and the set we poke are identical by
        // construction. `get_core_data` returns `None` for core ids ≥
        // MAX_CORES, so the mask is naturally bounded to real cores (whose
        // handler can actually set the bit).
        let mut target_mask: u64 = 0;
        for core_id in 0..crate::smp::MAX_CORES as u8 {
            if remote_mask & (1u64 << core_id) != 0
                && let Some(data) = super::get_core_data(core_id)
                && data.is_online.load(Ordering::Acquire)
            {
                target_mask |= 1u64 << core_id;
            }
        }

        if target_mask == 0 {
            break 'critical;
        }

        // Reset the ack bitmap before publishing the NMIs (under the lock, so a
        // stale bit from a prior abandoned round is cleared here).
        SHOOTDOWN_ACK.store(0, Ordering::Release);

        // Send to exactly the cores in target_mask. NMI delivery bypasses
        // recipient IF, so cores in CLI'd regions still service the shootdown.
        for core_id in 0..crate::smp::MAX_CORES as u8 {
            if target_mask & (1u64 << core_id) != 0 {
                ipi::send_nmi_to_core(core_id);
            }
        }

        // Wait for every targeted core's ack bit. NMI delivery ensures
        // recipients ack regardless of their IF state.
        wait_for_shootdown_acks(
            "tlb_shootdown_range",
            target_mask,
            aligned_start,
            aligned_end,
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

        // Snapshot recipients in a single predicate walk. Capture both the
        // recipient `apic_id` (so the send loop calls `send_nmi()` directly,
        // avoiding `send_nmi_to_core`'s is_online re-check) and the recipient
        // `core_id` as a bit in `target_mask`. The set we send NMIs to and the
        // mask we wait on are derived from this one snapshot — no second
        // predicate walk that could disagree with it.
        let my_core = super::per_core().core_id;
        let mut recipients = [0u8; crate::smp::MAX_CORES];
        let mut target_mask: u64 = 0;
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
                target_mask |= 1u64 << core_id;
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
        // Reset the ack bitmap before publishing the NMIs (under the lock).
        SHOOTDOWN_ACK.store(0, Ordering::Release);

        // Send to exactly the snapshot. NMI delivery (vs Fixed) so recipients
        // in CLI'd IrqSafeMutex regions still service the shootdown.
        for &apic_id in &recipients[..targets] {
            ipi::send_nmi(apic_id);
        }

        // NMI-bounded wait. With NMI delivery the recipient cores ack
        // regardless of IF, so the prior `ShootdownIrqWindow` (forcing IF=1
        // during the wait) is no longer required — callers from inside any
        // `IrqSafeMutex` region are safe.
        wait_for_shootdown_acks(
            "tlb_shootdown_range_kernel",
            target_mask,
            aligned_start,
            aligned_end,
        );
    }
    crate::task::scheduler::preempt_enable();
}

/// Handle a TLB shootdown IPI on the receiving core.
///
/// Called from the IDT handler. Reads the target address or range, executes
/// the appropriate flush, then sets this core's bit in [`SHOOTDOWN_ACK`] to
/// acknowledge it.
pub fn handle_tlb_shootdown_ipi() {
    // Resolve this core's id once: used both for the diagnostic
    // `TLB_IPI_SERVICED` counter (bumped up front so a sender timeout dump can
    // tell whether our IDT entry fired at all) and for the `SHOOTDOWN_ACK` bit
    // we set after the flush.
    let my_core = super::try_per_core().map(|pc| pc.core_id);
    if let Some(core_id) = my_core
        && let Some(slot) = TLB_IPI_SERVICED.get(core_id as usize)
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

    // Acknowledge: set this core's bit AFTER the flush is complete, with
    // Release ordering so a sender observing the bit (Acquire) is guaranteed
    // our TLB invalidation already happened. `fetch_or` is idempotent — a
    // redundant re-NMI from the degrade path cannot double-count or corrupt
    // the round. If per-core data isn't resolvable (should never happen: NMIs
    // are only sent to online cores with per-core data up) we skip the ack;
    // the sender's re-NMI + degrade path then handles us as a laggard rather
    // than hanging forever.
    if let Some(core_id) = my_core {
        SHOOTDOWN_ACK.fetch_or(1u64 << core_id, Ordering::Release);
    }
}
