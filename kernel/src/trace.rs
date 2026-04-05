//! Kernel trace ring: per-core lockless event recording.
//!
//! When the `trace` feature is enabled, `trace_event()` records scheduler,
//! fork, and IPC events into the current core's ring buffer.
//! When disabled, all functions compile to no-ops.

#[cfg(feature = "trace")]
use kernel_core::trace_ring::{TraceEntry, TraceEvent};

/// Emit a trace event on the current core's trace ring.
///
/// Compiles to nothing when the `trace` feature is off.
#[cfg(feature = "trace")]
pub fn trace_event(event: TraceEvent) {
    if !crate::smp::is_per_core_ready() {
        return;
    }
    let core_id = crate::smp::per_core().core_id;
    let tick = crate::arch::x86_64::interrupts::tick_count();
    // Safety: trace_ring is wrapped in UnsafeCell for interior mutability.
    // Only the owning core writes to its ring (single-writer guarantee via
    // gs_base per-core data). The UnsafeCell makes the mutable access sound.
    let ring_ptr = crate::smp::per_core().trace_ring.get();
    unsafe {
        (*ring_ptr).push(TraceEntry {
            tick,
            core: core_id,
            _pad: [0; 7],
            event,
        });
    }
}

#[cfg(not(feature = "trace"))]
pub fn trace_event(_event: kernel_core::trace_ring::TraceEvent) {}

/// Dump all trace rings from all online cores to serial output.
///
/// Uses `_panic_print` to avoid deadlocking if called from a panic handler.
/// Prints each core's ring independently (no heap allocation) to avoid
/// panicking again if the heap is corrupted.
///
/// Safety note on cross-core reads: in panic/fault context, the faulting
/// core has halted interrupts. Other cores may still be running and writing
/// to their rings. The UnsafeCell permits this access, and TraceRing uses
/// plain (non-atomic) fields, so a concurrent write could produce a torn
/// entry. This is acceptable in crash diagnostics — a single torn entry
/// is bounded and the timeline is best-effort.
///
/// Compiles to nothing when the `trace` feature is off.
#[cfg(feature = "trace")]
pub fn dump_trace_rings() {
    use crate::serial::_panic_print;

    _panic_print(format_args!("=== TRACE RING DUMP ===\n"));

    let core_count = crate::smp::core_count();
    let mut any_events = false;

    for core_id in 0..core_count {
        if let Some(data) = crate::smp::get_core_data(core_id) {
            // Safety: UnsafeCell grants interior mutability. We only read.
            // In panic context, a concurrent writer on another core could
            // produce a torn entry, but this is acceptable for crash diagnostics.
            // Uses for_each_chronological() to avoid heap allocation.
            let ring_ptr = data.trace_ring.get();
            unsafe {
                (*ring_ptr).for_each_chronological(|entry| {
                    any_events = true;
                    _panic_print(format_args!("  [{}] core={} ", entry.tick, entry.core));
                    print_trace_event(&entry.event);
                    _panic_print(format_args!("\n"));
                });
            }
        }
    }

    if !any_events {
        _panic_print(format_args!("  (no trace events recorded)\n"));
    }

    _panic_print(format_args!("=== END TRACE RING DUMP ===\n"));
}

#[cfg(not(feature = "trace"))]
pub fn dump_trace_rings() {}

/// Print a trace event directly to serial without heap allocation.
#[cfg(feature = "trace")]
fn print_trace_event(event: &TraceEvent) {
    use crate::serial::_panic_print;
    match event {
        TraceEvent::Dispatch {
            task_idx,
            core,
            rsp,
        } => _panic_print(format_args!(
            "Dispatch {{ task_idx: {task_idx}, core: {core}, rsp: {rsp:#x} }}"
        )),
        TraceEvent::SwitchOut {
            task_idx,
            core,
            saved_rsp,
        } => _panic_print(format_args!(
            "SwitchOut {{ task_idx: {task_idx}, core: {core}, saved_rsp: {saved_rsp:#x} }}"
        )),
        TraceEvent::YieldNow { task_idx, core } => _panic_print(format_args!(
            "YieldNow {{ task_idx: {task_idx}, core: {core} }}"
        )),
        TraceEvent::BlockCurrent {
            task_idx,
            core,
            new_state,
        } => _panic_print(format_args!(
            "BlockCurrent {{ task_idx: {task_idx}, core: {core}, new_state: {new_state} }}"
        )),
        TraceEvent::WakeTask {
            task_idx,
            state_before,
            core,
        } => _panic_print(format_args!(
            "WakeTask {{ task_idx: {task_idx}, state_before: {state_before}, core: {core} }}"
        )),
        TraceEvent::RunQueueEnqueue { task_idx, core } => _panic_print(format_args!(
            "RunQueueEnqueue {{ task_idx: {task_idx}, core: {core} }}"
        )),
        TraceEvent::ForkCtxPublish { pid, rip, rsp } => _panic_print(format_args!(
            "ForkCtxPublish {{ pid: {pid}, rip: {rip:#x}, rsp: {rsp:#x} }}"
        )),
        TraceEvent::ForkTaskSpawned {
            pid,
            task_idx,
            core,
        } => _panic_print(format_args!(
            "ForkTaskSpawned {{ pid: {pid}, task_idx: {task_idx}, core: {core} }}"
        )),
        TraceEvent::ForkTrampolineEnter { pid, task_idx } => _panic_print(format_args!(
            "ForkTrampolineEnter {{ pid: {pid}, task_idx: {task_idx} }}"
        )),
        TraceEvent::ForkTrampolineExit { pid, rip, rsp } => _panic_print(format_args!(
            "ForkTrampolineExit {{ pid: {pid}, rip: {rip:#x}, rsp: {rsp:#x} }}"
        )),
        TraceEvent::RecvBlock { task_idx, ep } => _panic_print(format_args!(
            "RecvBlock {{ task_idx: {task_idx}, ep: {ep} }}"
        )),
        TraceEvent::RecvWake { task_idx, ep } => _panic_print(format_args!(
            "RecvWake {{ task_idx: {task_idx}, ep: {ep} }}"
        )),
        TraceEvent::SendBlock { task_idx, ep } => _panic_print(format_args!(
            "SendBlock {{ task_idx: {task_idx}, ep: {ep} }}"
        )),
        TraceEvent::SendWake { task_idx, ep } => _panic_print(format_args!(
            "SendWake {{ task_idx: {task_idx}, ep: {ep} }}"
        )),
        TraceEvent::CallBlock { task_idx, ep } => _panic_print(format_args!(
            "CallBlock {{ task_idx: {task_idx}, ep: {ep} }}"
        )),
        TraceEvent::ReplyDeliver { caller_idx, ep } => _panic_print(format_args!(
            "ReplyDeliver {{ caller_idx: {caller_idx}, ep: {ep} }}"
        )),
        TraceEvent::MessageDelivered { task_idx, ep } => _panic_print(format_args!(
            "MessageDelivered {{ task_idx: {task_idx}, ep: {ep} }}"
        )),
    }
}
