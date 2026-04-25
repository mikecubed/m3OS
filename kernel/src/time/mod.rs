//! Phase 56 Track B.3 — kernel-side frame-tick source.
//!
//! Bridges the periodic LAPIC timer (1000 Hz, configured by `apic::init`) into
//! the configurable frame-tick rate consumed by `display_server` and any
//! future animation client. The pure-logic [`FrameTickConfig`] period math
//! lives in [`kernel_core::display::frame_tick`]; the kernel side keeps a
//! lock-free `AtomicU32` counter so the timer ISR and the syscall consumer
//! can never deadlock against each other.
//!
//! # Concurrency — no ISR-vs-task locks
//!
//! [`on_timer_tick_isr`] is called from the LAPIC timer ISR every ms.
//! [`frame_tick_drain`] is called from task / syscall context. They share
//! state via two atomics:
//!
//! * `FRAME_TICK_SUBDIV` — single-producer subdivider; the ISR is the only
//!   writer (the consumer side resets it via `set_frame_tick_hz` and the
//!   ISR itself when it rolls over).
//! * `FRAME_TICK_PENDING` — saturating count of frame-ticks accumulated
//!   since the last drain. ISR `fetch_add` (saturating-clamped); consumer
//!   `swap(0)` to drain. Both are `Relaxed` because there is no ordering
//!   relationship to enforce — the consumer reads whatever has been
//!   published so far and that is exactly the contract.
//!
//! Replacing the earlier `Mutex<FrameTickCounter>` with atomics removes
//! the ISR-vs-task deadlock risk flagged in PR #123 review thread and
//! aligns with Phase 52c's "no allocation, no locks held across ISR
//! re-entry" rule.

use core::sync::atomic::{AtomicU32, Ordering};

use kernel_core::display::frame_tick::FrameTickConfig;

/// Saturating ceiling for the pending frame-tick count. Above this we
/// stop incrementing — userspace will observe the cap on its next drain
/// and resume from zero. 1_000_000 frame-ticks ≈ 4.6 hours of pending at
/// 60 Hz: large enough that a momentarily slow consumer sees an honest
/// count, small enough that the saturation branch is reachable in
/// pathological situations and the counter cannot wrap past `u32::MAX`.
const FRAME_TICK_SAT_CAP: u32 = 1_000_000;

/// Configured frame-tick rate (Hz). Default 60 Hz per
/// `FrameTickConfig::DEFAULT_HZ`.
static FRAME_TICK_HZ: AtomicU32 = AtomicU32::new(FrameTickConfig::DEFAULT_HZ);

/// Subdivider — counts LAPIC timer fires (1 ms each) and rolls over every
/// `lapic_period_ms()` ticks to emit one frame-tick. Written by the ISR;
/// read+reset by the ISR (rollover) and by `set_frame_tick_hz`.
static FRAME_TICK_SUBDIV: AtomicU32 = AtomicU32::new(0);

/// Saturating count of frame-ticks observed since the last drain.
static FRAME_TICK_PENDING: AtomicU32 = AtomicU32::new(0);

/// Currently configured frame-tick rate in Hz. Returns a value in
/// `FrameTickConfig::MIN_HZ..=FrameTickConfig::MAX_HZ`.
pub fn frame_tick_hz() -> u32 {
    FRAME_TICK_HZ.load(Ordering::Relaxed)
}

/// Set the frame-tick rate (Hz). Returns `Some(())` if the rate is valid;
/// `None` if it is outside `MIN_HZ..=MAX_HZ`. Provided for future tuning
/// (e.g. via a control-socket verb); not currently exposed to userspace.
#[allow(dead_code)]
pub fn set_frame_tick_hz(hz: u32) -> Option<()> {
    let _ = FrameTickConfig::new(hz)?;
    FRAME_TICK_HZ.store(hz, Ordering::Relaxed);
    FRAME_TICK_SUBDIV.store(0, Ordering::Relaxed);
    Some(())
}

/// Drain pending frame-tick events. Returns the number of frame-ticks
/// observed since the last drain (saturating). Called from
/// `sys_frame_tick_drain`. Lock-free.
pub fn frame_tick_drain() -> u32 {
    FRAME_TICK_PENDING.swap(0, Ordering::Relaxed)
}

/// Called from the timer ISR every LAPIC timer fire. Subdivides the 1 kHz
/// timer into the configured frame-tick rate.
///
/// # ISR contract
///
/// * No allocation, no blocking, no IPC, no locks.
/// * Two relaxed atomic operations per fire (subdiv increment + maybe a
///   pending increment). Safe to call from any timer firing on any CPU.
pub fn on_timer_tick_isr() {
    let hz = FRAME_TICK_HZ.load(Ordering::Relaxed);
    let Some(config) = FrameTickConfig::new(hz) else {
        return;
    };
    let period_ms = config.lapic_period_ms();
    if period_ms == 0 {
        return;
    }
    let prev = FRAME_TICK_SUBDIV.fetch_add(1, Ordering::Relaxed);
    let next = prev + 1;
    if next >= period_ms {
        FRAME_TICK_SUBDIV.store(0, Ordering::Relaxed);
        // Saturating increment of the pending count. `compare_exchange`
        // loop is overkill for a pending counter that doesn't need
        // exact-once semantics; a `fetch_add` followed by clamp gets us
        // there with one atomic op on the fast path.
        let cur = FRAME_TICK_PENDING.fetch_add(1, Ordering::Relaxed);
        if cur >= FRAME_TICK_SAT_CAP {
            // Already at cap; undo the wrap by clamping back. The race
            // window where another concurrent ISR also incremented is
            // benign — the worst case is the count plateaus at SAT_CAP.
            FRAME_TICK_PENDING.store(FRAME_TICK_SAT_CAP, Ordering::Relaxed);
        }
    }
}

// Pure-logic frame-tick coalescing is covered host-side by tests in
// `kernel_core::display::frame_tick`. The kernel-side wiring above is a
// pure-atomic shim and is exercised through the QEMU integration paths.
