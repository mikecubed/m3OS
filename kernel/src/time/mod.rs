//! Phase 56 Track B.3 — kernel-side frame-tick source.
//!
//! Bridges the periodic LAPIC timer (1000 Hz, configured by `apic::init`) into
//! the configurable frame-tick rate consumed by `display_server` and any
//! future animation client. The pure-logic config + saturating coalescer
//! lives in [`kernel_core::display::frame_tick`]; this module owns the
//! kernel-side wiring (the per-tick subdivider that turns timer ticks into
//! frame-tick events and the syscall-facing accessors).
//!
//! # Concurrency
//!
//! - [`maybe_emit_frame_tick`] is called from the timer ISR every LAPIC
//!   timer fire. It increments a private subdivision counter and, every
//!   `lapic_period_ms()` ticks, accumulates one frame-tick into
//!   [`FRAME_TICK_COUNTER`].
//! - [`frame_tick_drain`] is the syscall-facing consumer; it returns the
//!   number of frame-ticks accumulated since the last drain. Coalescing is
//!   saturating in `FrameTickCounter`, so a slow consumer never sees the
//!   queue grow without bound.
//!
//! Both paths use a `spin::Mutex` because the pure-logic counter holds two
//! `u64` halves and reading/updating them atomically across the `accumulate`
//! / `drain` calls is the simplest correct option. The lock is uncontended in
//! practice: the ISR holds it for the duration of an integer increment and
//! the syscall path holds it for one drain call.

use core::sync::atomic::{AtomicU32, Ordering};

use kernel_core::display::frame_tick::{FrameTickConfig, FrameTickCounter};
use spin::Mutex;

/// Configured frame-tick rate (Hz). Default 60 Hz per
/// `FrameTickConfig::DEFAULT_HZ`.
static FRAME_TICK_HZ: AtomicU32 = AtomicU32::new(FrameTickConfig::DEFAULT_HZ);

/// Subdivider — counts LAPIC timer fires (1 ms each) and rolls over every
/// `lapic_period_ms()` ticks to emit one frame-tick. Read/written only by
/// the timer ISR (single-producer); a relaxed atomic is sufficient because
/// the only consumer is itself.
static FRAME_TICK_SUBDIV: AtomicU32 = AtomicU32::new(0);

/// Saturating counter shared with userspace via `sys_frame_tick_drain`.
static FRAME_TICK_COUNTER: Mutex<FrameTickCounter> = Mutex::new(FrameTickCounter::new());

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
/// observed since the last drain (saturating coalesce inside
/// `FrameTickCounter`). Called from `sys_frame_tick_drain`.
pub fn frame_tick_drain() -> u32 {
    let (missed, _total) = FRAME_TICK_COUNTER.lock().drain();
    missed
}

/// Called from the timer ISR every LAPIC timer fire. Subdivides the 1 kHz
/// timer into the configured frame-tick rate.
///
/// # ISR contract
///
/// * No allocation, no blocking, no IPC. Holds the `FRAME_TICK_COUNTER`
///   mutex for the duration of one integer increment.
/// * Safe to call from any timer firing on any CPU; coalescing is
///   saturating so a slow consumer is harmless.
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
        FRAME_TICK_COUNTER.lock().accumulate(1);
    }
}

// Pure-logic frame-tick coalescing is covered host-side by tests in
// `kernel_core::display::frame_tick`. The kernel-side wiring above is a
// thin AtomicU32 + Mutex<FrameTickCounter> shim and exercised through the
// QEMU integration paths.
