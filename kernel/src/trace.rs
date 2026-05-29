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
    let entry = TraceEntry {
        tick,
        core: core_id,
        _pad: [0; 7],
        event,
    };
    unsafe {
        (*ring_ptr).push(entry);
    }
    // Also feed the deep, pid-filtered focus ring (no-op unless armed).
    focus::record(&entry);
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
    dump_trace_rings_recent(usize::MAX)
}

/// Dump only the most-recent `max_per_core` events from each core's ring.
///
/// Faster than [`dump_trace_rings`] for diagnostic paths that fire from a
/// running kernel — keeps the non-preemptible serial-write window short
/// and reduces the chance that a long dump destabilises an already-fragile
/// kernel state.
///
/// `max_per_core == usize::MAX` is treated as a sentinel meaning "all
/// events" (used by [`dump_trace_rings`]) and the header is adjusted
/// accordingly so the output reads naturally rather than printing the
/// literal sentinel.
#[cfg(feature = "trace")]
pub fn dump_trace_rings_recent(max_per_core: usize) {
    use crate::serial::_panic_print;

    if max_per_core == usize::MAX {
        _panic_print(format_args!("=== TRACE RING DUMP (all per core) ===\n"));
    } else {
        _panic_print(format_args!(
            "=== TRACE RING DUMP (last {max_per_core} per core) ===\n"
        ));
    }

    let core_count = crate::smp::core_count();
    let mut any_events = false;

    for core_id in 0..core_count {
        if let Some(data) = crate::smp::get_core_data(core_id) {
            _panic_print(format_args!("--- core={core_id} ---\n"));
            // Safety: UnsafeCell grants interior mutability. We only read.
            // In panic context, a concurrent writer on another core could
            // produce a torn entry, but this is acceptable for crash diagnostics.
            let ring_ptr = data.trace_ring.get();
            unsafe {
                (*ring_ptr).for_each_recent(max_per_core, |entry| {
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
        TraceEvent::YieldNow {
            task_idx,
            core,
            caller_file,
            caller_line,
        } => _panic_print(format_args!(
            "YieldNow {{ task_idx: {task_idx}, core: {core}, caller={caller_file}:{caller_line} }}"
        )),
        TraceEvent::BlockCurrent {
            task_idx,
            core,
            new_state,
            caller_file,
            caller_line,
        } => _panic_print(format_args!(
            "BlockCurrent {{ task_idx: {task_idx}, core: {core}, new_state: {new_state}, caller={caller_file}:{caller_line} }}"
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
        TraceEvent::Wakeup { kind, id } => {
            _panic_print(format_args!("Wakeup {{ kind: {kind}, id: {id} }}"))
        }
    }
}

// ===========================================================================
// Deep, pid-filtered focus trace ring (the per-task trace tool).
//
// The per-core `TraceRing<128>` rings are too shallow and too noisy (the idle
// task spams `YieldNow`) to study an intermittent dispatch-starvation / lost-
// wake bug: a dump spans only ~1 ms. The focus ring is a single deep
// (`FOCUS_CAP`) heap ring that records, when armed:
//   * every event for a small set of target task indices (the task[s] under
//     study — e.g. an sshd session child + its shell), AND
//   * every Dispatch/SwitchOut for any NON-idle task (the cross-core run
//     timeline, so we can see what occupied a core while a target sat Ready).
// Idle dispatch/yield churn is dropped, so the ring spans seconds, not ms.
//
// Lock discipline: the fast-path filter uses standalone atomics, so
// `record()` only takes `FOCUS` for events it actually keeps (rare). `record()`
// holds no other lock while holding `FOCUS`, so there is no lock-order cycle;
// it uses `try_lock` and drops the event on contention rather than spinning —
// safe to call from scheduler-locked / IRQ-ish contexts.
// ===========================================================================

#[cfg(feature = "trace")]
pub mod focus {
    use super::{TraceEntry, TraceEvent};
    use crate::task::scheduler::IrqSafeMutex;
    use alloc::vec::Vec;
    use core::fmt::Write;
    use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

    /// Capacity of the focus ring in entries. ~56 B/entry → ~230 KiB heap.
    const FOCUS_CAP: usize = 4096;
    /// Maximum number of target task indices that can be focused at once.
    pub const MAX_TARGETS: usize = 8;
    /// Sentinel for an empty target / idle slot (no real task index is u32::MAX).
    const EMPTY: u32 = u32::MAX;

    /// Heap-backed circular ring of [`TraceEntry`]. Lives behind [`FOCUS`].
    struct FocusRing {
        buf: Vec<TraceEntry>,
        write: usize,
        count: usize,
    }

    impl FocusRing {
        fn new() -> Self {
            Self {
                buf: Vec::with_capacity(FOCUS_CAP),
                write: 0,
                count: 0,
            }
        }

        fn push(&mut self, e: TraceEntry) {
            if self.buf.len() < FOCUS_CAP {
                self.buf.push(e);
                self.count = self.buf.len();
                self.write = self.buf.len() % FOCUS_CAP;
            } else {
                self.buf[self.write] = e;
                self.write = (self.write + 1) % FOCUS_CAP;
            }
        }

        fn clear(&mut self) {
            self.buf.clear();
            self.write = 0;
            self.count = 0;
        }

        fn for_each_from(&self, start: usize, max: usize, mut f: impl FnMut(&TraceEntry)) -> usize {
            if self.count == 0 || max == 0 || start >= self.count {
                return 0;
            }
            let ring_start = if self.count < FOCUS_CAP {
                0
            } else {
                self.write
            };
            let take = (self.count - start).min(max);
            for i in start..(start + take) {
                f(&self.buf[(ring_start + i) % FOCUS_CAP]);
            }
            take
        }
    }

    struct FocusState {
        ring: Option<FocusRing>,
        armed: bool,
        /// Arm-time `idx -> (pid, name)` snapshot, so entries are annotated with
        /// the task that owned the index *when recorded*, not at dump time
        /// (task slots get reused during teardown, which would mislabel them).
        names: Vec<(u32, u32, &'static str)>,
    }

    static FOCUS: IrqSafeMutex<FocusState> = IrqSafeMutex::new(FocusState {
        ring: None,
        armed: false,
        names: Vec::new(),
    });
    /// Fast-path "is the focus ring armed" flag — read on every `trace_event`.
    static ARMED: AtomicBool = AtomicBool::new(false);
    static TARGETS: [AtomicU32; MAX_TARGETS] = [const { AtomicU32::new(EMPTY) }; MAX_TARGETS];

    /// The subject task index of an event (the task it concerns), if any.
    pub fn event_subject_idx(event: &TraceEvent) -> Option<u32> {
        use TraceEvent::*;
        match *event {
            Dispatch { task_idx, .. }
            | SwitchOut { task_idx, .. }
            | YieldNow { task_idx, .. }
            | BlockCurrent { task_idx, .. }
            | WakeTask { task_idx, .. }
            | RunQueueEnqueue { task_idx, .. }
            | RecvBlock { task_idx, .. }
            | RecvWake { task_idx, .. }
            | SendBlock { task_idx, .. }
            | SendWake { task_idx, .. }
            | CallBlock { task_idx, .. }
            | MessageDelivered { task_idx, .. }
            | ForkTaskSpawned { task_idx, .. }
            | ForkTrampolineEnter { task_idx, .. } => Some(task_idx),
            ReplyDeliver { caller_idx, .. } => Some(caller_idx),
            ForkCtxPublish { .. } | ForkTrampolineExit { .. } | Wakeup { .. } => None,
        }
    }

    fn targets_contains(idx: u32) -> bool {
        TARGETS.iter().any(|a| a.load(Ordering::Relaxed) == idx)
    }

    /// Arm the focus ring on `targets` (task indices). `names` is the arm-time
    /// `idx -> (pid, name)` snapshot used to annotate the dump. Allocates the
    /// ring on first arm and clears it on re-arm. Called from `sys_ktrace`
    /// (heap is up). Only events whose subject is a target are recorded, so the
    /// ring spans the whole study window instead of a sub-millisecond burst of
    /// unrelated dispatch churn.
    pub fn arm(targets: &[u32], names: Vec<(u32, u32, &'static str)>) {
        for (i, slot) in TARGETS.iter().enumerate() {
            slot.store(targets.get(i).copied().unwrap_or(EMPTY), Ordering::Relaxed);
        }
        {
            let mut g = FOCUS.lock();
            match g.ring.as_mut() {
                Some(r) => r.clear(),
                None => g.ring = Some(FocusRing::new()),
            }
            g.names = names;
            g.armed = true;
        }
        ARMED.store(true, Ordering::Release);
    }

    /// Disarm the focus ring. The ring's contents (and heap) are retained so a
    /// final dump after disarm still works; a subsequent `arm` clears it.
    pub fn disarm() {
        ARMED.store(false, Ordering::Release);
        FOCUS.lock().armed = false;
    }

    /// Record an event into the focus ring if it passes the filter. Cheap and
    /// lock-free when disarmed or when the event is filtered out.
    pub fn record(entry: &TraceEntry) {
        if !ARMED.load(Ordering::Relaxed) {
            return;
        }
        // Producer-wake events have no task subject but are always recorded
        // (while armed) so the timeline shows whether the wake a focused poller
        // is waiting for ever fires.
        let is_wakeup = matches!(entry.event, TraceEvent::Wakeup { .. });
        if !is_wakeup {
            let subj = match event_subject_idx(&entry.event) {
                Some(s) => s,
                None => return,
            };
            if !targets_contains(subj) {
                return;
            }
        }
        if let Some(mut g) = FOCUS.try_lock()
            && g.armed
            && let Some(ring) = g.ring.as_mut()
        {
            ring.push(*entry);
        }
    }

    /// Number of entries currently in the focus ring.
    pub fn len() -> usize {
        FOCUS.lock().ring.as_ref().map(|r| r.count).unwrap_or(0)
    }

    /// A bounded byte-buffer writer that saturates silently on overflow.
    struct ByteCursor<'a> {
        buf: &'a mut [u8],
        pos: usize,
    }

    impl Write for ByteCursor<'_> {
        fn write_str(&mut self, s: &str) -> core::fmt::Result {
            let b = s.as_bytes();
            let n = b.len().min(self.buf.len().saturating_sub(self.pos));
            self.buf[self.pos..self.pos + n].copy_from_slice(&b[..n]);
            self.pos += n;
            Ok(())
        }
    }

    /// Format one focus entry as a single annotated text line into `line`,
    /// returning its byte length. `lookup(idx) -> Some((pid, name))` resolves
    /// the subject task index to its pid/name for readability.
    fn write_line(
        line: &mut [u8],
        e: &TraceEntry,
        lookup: &impl Fn(u32) -> Option<(u32, &'static str)>,
    ) -> usize {
        let mut c = ByteCursor { buf: line, pos: 0 };
        let subj = event_subject_idx(&e.event);
        let (pid, name) = subj
            .and_then(lookup)
            .map(|(p, n)| (p as i64, n))
            .unwrap_or((-1, "?"));
        let _ = write!(c, "t={} cpu={} ", e.tick, e.core);
        match e.event {
            TraceEvent::Dispatch { core, .. } => {
                let _ = write!(
                    c,
                    "DISPATCH idx={} pid={} {} -> core{}",
                    subj.unwrap_or(EMPTY),
                    pid,
                    name,
                    core
                );
            }
            TraceEvent::SwitchOut { core, .. } => {
                let _ = write!(
                    c,
                    "SWITCHOUT idx={} pid={} {} core{}",
                    subj.unwrap_or(EMPTY),
                    pid,
                    name,
                    core
                );
            }
            TraceEvent::YieldNow {
                core,
                caller_file,
                caller_line,
                ..
            } => {
                let _ = write!(
                    c,
                    "YIELD idx={} pid={} {} core{} @{}:{}",
                    subj.unwrap_or(EMPTY),
                    pid,
                    name,
                    core,
                    caller_file,
                    caller_line
                );
            }
            TraceEvent::BlockCurrent {
                core,
                new_state,
                caller_file,
                caller_line,
                ..
            } => {
                let _ = write!(
                    c,
                    "BLOCK idx={} pid={} {} core{} state={} @{}:{}",
                    subj.unwrap_or(EMPTY),
                    pid,
                    name,
                    core,
                    new_state,
                    caller_file,
                    caller_line
                );
            }
            TraceEvent::WakeTask {
                state_before, core, ..
            } => {
                let _ = write!(
                    c,
                    "WAKE idx={} pid={} {} ->core{} from_state={}",
                    subj.unwrap_or(EMPTY),
                    pid,
                    name,
                    core,
                    state_before
                );
            }
            TraceEvent::RunQueueEnqueue { core, .. } => {
                let _ = write!(
                    c,
                    "ENQUEUE idx={} pid={} {} ->core{}",
                    subj.unwrap_or(EMPTY),
                    pid,
                    name,
                    core
                );
            }
            TraceEvent::RecvBlock { ep, .. } => {
                let _ = write!(
                    c,
                    "RECV_BLOCK idx={} pid={} {} ep={}",
                    subj.unwrap_or(EMPTY),
                    pid,
                    name,
                    ep
                );
            }
            TraceEvent::RecvWake { ep, .. } => {
                let _ = write!(
                    c,
                    "RECV_WAKE idx={} pid={} {} ep={}",
                    subj.unwrap_or(EMPTY),
                    pid,
                    name,
                    ep
                );
            }
            TraceEvent::SendBlock { ep, .. } => {
                let _ = write!(
                    c,
                    "SEND_BLOCK idx={} pid={} {} ep={}",
                    subj.unwrap_or(EMPTY),
                    pid,
                    name,
                    ep
                );
            }
            TraceEvent::SendWake { ep, .. } => {
                let _ = write!(
                    c,
                    "SEND_WAKE idx={} pid={} {} ep={}",
                    subj.unwrap_or(EMPTY),
                    pid,
                    name,
                    ep
                );
            }
            TraceEvent::CallBlock { ep, .. } => {
                let _ = write!(
                    c,
                    "CALL_BLOCK idx={} pid={} {} ep={}",
                    subj.unwrap_or(EMPTY),
                    pid,
                    name,
                    ep
                );
            }
            TraceEvent::ReplyDeliver { ep, .. } => {
                let _ = write!(
                    c,
                    "REPLY_DELIVER caller_idx={} pid={} {} ep={}",
                    subj.unwrap_or(EMPTY),
                    pid,
                    name,
                    ep
                );
            }
            TraceEvent::MessageDelivered { ep, .. } => {
                let _ = write!(
                    c,
                    "MSG_DELIVERED idx={} pid={} {} ep={}",
                    subj.unwrap_or(EMPTY),
                    pid,
                    name,
                    ep
                );
            }
            TraceEvent::ForkTaskSpawned { core, .. } => {
                let _ = write!(
                    c,
                    "FORK_SPAWN idx={} pid={} {} core{}",
                    subj.unwrap_or(EMPTY),
                    pid,
                    name,
                    core
                );
            }
            TraceEvent::ForkTrampolineEnter { .. } => {
                let _ = write!(
                    c,
                    "FORK_TRAMP_ENTER idx={} pid={} {}",
                    subj.unwrap_or(EMPTY),
                    pid,
                    name
                );
            }
            TraceEvent::ForkCtxPublish { .. } | TraceEvent::ForkTrampolineExit { .. } => {
                let _ = write!(c, "FORK_MISC");
            }
            TraceEvent::Wakeup { kind, id } => {
                let what = match kind {
                    0 => "socket",
                    1 => "unix-socket",
                    2 => "pty-master",
                    3 => "pty-slave",
                    _ => "?",
                };
                let _ = write!(c, "PRODUCER_WAKE {what} id={id}");
            }
        }
        let _ = c.write_str("\n");
        c.pos
    }

    /// Format up to `max` focus entries starting at chronological index
    /// `start` into `out` as newline-separated annotated text, NUL-terminating
    /// the result. Only whole lines that fit are emitted (clean paging).
    /// Returns the number of entries written. Entries are annotated from the
    /// arm-time name snapshot.
    pub fn dump_text(start: usize, max: usize, out: &mut [u8]) -> usize {
        if out.is_empty() {
            return 0;
        }
        let usable = out.len() - 1; // reserve 1 byte for the NUL terminator
        let g = FOCUS.lock();
        let names = &g.names;
        let lookup = |idx: u32| {
            names
                .iter()
                .find(|(i, _, _)| *i == idx)
                .map(|(_, p, n)| (*p, *n))
        };
        let ring = match g.ring.as_ref() {
            Some(r) => r,
            None => {
                out[0] = 0;
                return 0;
            }
        };
        let mut pos = 0usize;
        let mut formatted = 0usize;
        let mut full = false;
        ring.for_each_from(start, max, |e| {
            if full {
                return;
            }
            let mut line = [0u8; 224];
            let ln = write_line(&mut line, e, &lookup);
            if pos + ln > usable {
                full = true;
                return;
            }
            out[pos..pos + ln].copy_from_slice(&line[..ln]);
            pos += ln;
            formatted += 1;
        });
        out[pos] = 0;
        formatted
    }

    /// Dump the entire focus ring to the serial console (the always-available
    /// path — used to read the trace while the userspace I/O path is wedged by
    /// the very hang under study). Entries are snapshotted under the lock (a
    /// fast memcpy into a pre-allocated Vec), then formatted with interrupts
    /// on so the slow serial writes never extend the lock / IRQ-off window.
    /// Returns the number of entries dumped.
    pub fn dump_serial() -> usize {
        // Allocate the snapshot buffers BEFORE taking the lock so no allocation
        // happens while interrupts are disabled.
        let mut entries: Vec<TraceEntry> = Vec::with_capacity(FOCUS_CAP);
        let names: Vec<(u32, u32, &'static str)>;
        {
            let g = FOCUS.lock();
            names = g.names.clone();
            if let Some(r) = g.ring.as_ref() {
                r.for_each_from(0, r.count, |e| entries.push(*e));
            }
        }
        let lookup = |idx: u32| {
            names
                .iter()
                .find(|(i, _, _)| *i == idx)
                .map(|(_, p, n)| (*p, *n))
        };
        crate::serial_println!("=== KTRACE FOCUS DUMP ({} entries) ===", entries.len());
        let mut line = [0u8; 224];
        for e in &entries {
            let ln = write_line(&mut line, e, &lookup);
            // Strip the trailing '\n' (serial_println adds its own).
            if let Ok(s) = core::str::from_utf8(&line[..ln.saturating_sub(1)]) {
                crate::serial_println!("{}", s);
            }
        }
        crate::serial_println!("=== END KTRACE FOCUS DUMP ===");
        entries.len()
    }
}
