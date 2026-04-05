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
    // Safety: trace_ring is only written by the owning core (single-writer).
    // We obtain a mutable reference through the per-core data pointer.
    let data_ptr =
        crate::smp::per_core() as *const crate::smp::PerCoreData as *mut crate::smp::PerCoreData;
    unsafe {
        (*data_ptr).trace_ring.push(TraceEntry {
            tick,
            core: core_id,
            event,
        });
    }
}

#[cfg(not(feature = "trace"))]
pub fn trace_event(_event: kernel_core::trace_ring::TraceEvent) {}

/// Dump all trace rings from all online cores to serial output.
///
/// Uses `_panic_print` to avoid deadlocking if called from a panic handler.
/// Compiles to nothing when the `trace` feature is off.
#[cfg(feature = "trace")]
pub fn dump_trace_rings() {
    use crate::serial::_panic_print;
    use alloc::vec::Vec;

    _panic_print(format_args!("=== TRACE RING DUMP ===\n"));

    let core_count = crate::smp::core_count();
    let mut all_entries: Vec<TraceEntry> = Vec::new();

    for core_id in 0..core_count {
        if let Some(data) = crate::smp::get_core_data(core_id) {
            // Safety: we're in a panic/fault context; the trace ring is lockless
            // and we only read it. Single-writer guarantees mean partial writes
            // are bounded to one entry.
            let data_ptr = data as *const crate::smp::PerCoreData as *mut crate::smp::PerCoreData;
            let snap = unsafe { (*data_ptr).trace_ring.snapshot() };
            all_entries.extend_from_slice(&snap);
        }
    }

    // Sort by tick for a unified timeline.
    all_entries.sort_by_key(|e| e.tick);

    if all_entries.is_empty() {
        _panic_print(format_args!("  (no trace events recorded)\n"));
    } else {
        for entry in &all_entries {
            _panic_print(format_args!(
                "  [{}] core={} {}\n",
                entry.tick,
                entry.core,
                format_trace_event(&entry.event),
            ));
        }
    }

    _panic_print(format_args!("=== END TRACE RING DUMP ===\n"));
}

#[cfg(not(feature = "trace"))]
pub fn dump_trace_rings() {}

#[cfg(feature = "trace")]
fn format_trace_event(event: &TraceEvent) -> alloc::string::String {
    use alloc::format;
    match event {
        TraceEvent::Dispatch {
            task_idx,
            core,
            rsp,
        } => format!("Dispatch {{ task_idx: {task_idx}, core: {core}, rsp: {rsp:#x} }}"),
        TraceEvent::SwitchOut {
            task_idx,
            core,
            saved_rsp,
        } => {
            format!("SwitchOut {{ task_idx: {task_idx}, core: {core}, saved_rsp: {saved_rsp:#x} }}")
        }
        TraceEvent::YieldNow { task_idx, core } => {
            format!("YieldNow {{ task_idx: {task_idx}, core: {core} }}")
        }
        TraceEvent::BlockCurrent {
            task_idx,
            core,
            new_state,
        } => {
            format!("BlockCurrent {{ task_idx: {task_idx}, core: {core}, new_state: {new_state} }}")
        }
        TraceEvent::WakeTask {
            task_idx,
            state_before,
            core,
        } => format!(
            "WakeTask {{ task_idx: {task_idx}, state_before: {state_before}, core: {core} }}"
        ),
        TraceEvent::RunQueueEnqueue { task_idx, core } => {
            format!("RunQueueEnqueue {{ task_idx: {task_idx}, core: {core} }}")
        }
        TraceEvent::ForkCtxPublish { pid, rip, rsp } => {
            format!("ForkCtxPublish {{ pid: {pid}, rip: {rip:#x}, rsp: {rsp:#x} }}")
        }
        TraceEvent::ForkTaskSpawned {
            pid,
            task_idx,
            core,
        } => format!("ForkTaskSpawned {{ pid: {pid}, task_idx: {task_idx}, core: {core} }}"),
        TraceEvent::ForkTrampolineEnter { pid, task_idx } => {
            format!("ForkTrampolineEnter {{ pid: {pid}, task_idx: {task_idx} }}")
        }
        TraceEvent::ForkTrampolineExit { pid, rip, rsp } => {
            format!("ForkTrampolineExit {{ pid: {pid}, rip: {rip:#x}, rsp: {rsp:#x} }}")
        }
        TraceEvent::RecvBlock { task_idx, ep } => {
            format!("RecvBlock {{ task_idx: {task_idx}, ep: {ep} }}")
        }
        TraceEvent::RecvWake { task_idx, ep } => {
            format!("RecvWake {{ task_idx: {task_idx}, ep: {ep} }}")
        }
        TraceEvent::SendBlock { task_idx, ep } => {
            format!("SendBlock {{ task_idx: {task_idx}, ep: {ep} }}")
        }
        TraceEvent::SendWake { task_idx, ep } => {
            format!("SendWake {{ task_idx: {task_idx}, ep: {ep} }}")
        }
        TraceEvent::CallBlock { task_idx, ep } => {
            format!("CallBlock {{ task_idx: {task_idx}, ep: {ep} }}")
        }
        TraceEvent::ReplyDeliver { caller_idx, ep } => {
            format!("ReplyDeliver {{ caller_idx: {caller_idx}, ep: {ep} }}")
        }
        TraceEvent::MessageDelivered { task_idx, ep } => {
            format!("MessageDelivered {{ task_idx: {task_idx}, ep: {ep} }}")
        }
    }
}
