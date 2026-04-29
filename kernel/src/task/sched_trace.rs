//! G.2 — sched-trace state-transition tracepoint.
//!
//! This module is compiled only when the `sched-trace` Cargo feature is
//! enabled. All public items compile out to nothing when the feature is OFF,
//! ensuring zero runtime overhead in default builds.
//!
//! # Enabling the feature
//!
//! ```text
//! cargo clippy -p kernel --target x86_64-unknown-none \
//!     -Zbuild-std=core,compiler_builtins,alloc \
//!     -Zbuild-std-features=compiler-builtins-mem \
//!     --features kernel/sched-trace \
//!     -- -D warnings
//! ```
//!
//! `cargo xtask check` does not forward features to the kernel clippy
//! invocation; use the direct `cargo clippy` command above.
//!
//! # Ring size
//!
//! Each core maintains its own [`SCHED_TRACE_RING_SIZE`]-entry circular
//! buffer. Entries are overwritten on wrap (newest-wins). The ring is
//! per-core to allow lock-free single-writer access from the scheduler loop.
//!
//! # Dumping the ring
//!
//! Call `dump_sched_trace_rings()` from a panic handler or a debug syscall
//! to dump all cores' rings to serial output. The existing trace dump path
//! in `kernel/src/trace.rs::dump_trace_rings()` is separate (it holds the
//! Phase 43b general-purpose `TraceEvent` ring); sched-trace entries are
//! emitted here.

#[cfg(feature = "sched-trace")]
pub use inner::record;

#[cfg(feature = "sched-trace")]
#[allow(unused)]
pub use inner::dump_sched_trace_rings;

#[cfg(feature = "sched-trace")]
mod inner {
    use core::cell::UnsafeCell;

    use kernel_core::trace_ring::SchedTrace;

    /// Number of entries per core in the sched-trace ring.
    const SCHED_TRACE_RING_SIZE: usize = 256;

    /// Minimal lock-free fixed-size ring for `SchedTrace` entries.
    ///
    /// Single-writer (the core that owns this ring). No mutex. Safe because:
    /// - Only the owning core calls `push`.
    /// - `dump_sched_trace_rings` is a crash/debug path that accepts the risk
    ///   of reading a torn entry from a concurrently-writing core.
    struct SchedTraceRing {
        buf: [SchedTrace; SCHED_TRACE_RING_SIZE],
        write_idx: usize,
    }

    impl SchedTraceRing {
        const fn new() -> Self {
            Self {
                buf: [SchedTrace::EMPTY; SCHED_TRACE_RING_SIZE],
                write_idx: 0,
            }
        }

        fn push(&mut self, entry: SchedTrace) {
            self.buf[self.write_idx] = entry;
            self.write_idx = (self.write_idx + 1) % SCHED_TRACE_RING_SIZE;
        }

        fn for_each_chronological(&self, mut f: impl FnMut(&SchedTrace)) {
            // write_idx points to the oldest slot (oldest-first order since
            // we overwrite on wrap — same convention as TraceRing).
            for i in 0..SCHED_TRACE_RING_SIZE {
                let slot = &self.buf[(self.write_idx + i) % SCHED_TRACE_RING_SIZE];
                if slot.tick == 0 && slot.caller_file.is_empty() {
                    // Slot never written — skip leading empties before ring
                    // has been fully populated. We stop at the first empty
                    // gap because entries are written sequentially.
                    // NOTE: this heuristic breaks if tick ever legitimately
                    // wraps to 0, but in practice a 1 kHz tick overflows a
                    // u64 in ~585 million years.
                    continue;
                }
                f(slot);
            }
        }
    }

    // Per-core sched-trace rings.
    //
    // We use the same MAX_CORES constant as the existing trace infrastructure
    // in `kernel/src/smp/mod.rs`.
    const MAX_CORES: usize = crate::smp::MAX_CORES;

    /// Newtype wrapper that implements `Sync` for the per-core ring cell.
    ///
    /// Safety invariant: only the owning core writes to the contained
    /// `SchedTraceRing`. Cross-core reads in `dump_sched_trace_rings` run only
    /// in crash/debug context and accept the risk of a torn read.
    struct SyncRingCell(UnsafeCell<SchedTraceRing>);

    // Safety: only the owning core writes; reads are crash-context best-effort.
    unsafe impl Sync for SyncRingCell {}

    impl SyncRingCell {
        const fn new() -> Self {
            Self(UnsafeCell::new(SchedTraceRing::new()))
        }

        fn get(&self) -> *mut SchedTraceRing {
            self.0.get()
        }
    }

    static SCHED_TRACE_RINGS: [SyncRingCell; MAX_CORES] =
        [const { SyncRingCell::new() }; MAX_CORES];

    /// Emit a sched-trace entry for a state transition.
    ///
    /// Annotated with `#[track_caller]` so `caller_file` and `caller_line`
    /// in the entry reflect the call site in `scheduler.rs`, not this
    /// function.
    ///
    /// This function is a no-op stub when `feature = "sched-trace"` is not
    /// enabled — the call sites are wrapped in `#[cfg(feature = "sched-trace")]`
    /// so this function is never compiled in default builds.
    #[track_caller]
    pub fn record(pid: u32, old_state: u8, new_state: u8) {
        if !crate::smp::is_per_core_ready() {
            return;
        }
        let core_id = crate::smp::per_core().core_id as usize;
        if core_id >= MAX_CORES {
            return;
        }
        let tick = crate::arch::x86_64::interrupts::tick_count();
        let loc = core::panic::Location::caller();
        let entry = SchedTrace {
            pid,
            old_state,
            new_state,
            caller_file: loc.file(),
            caller_line: loc.line(),
            tick,
        };
        // Safety: only the owning core writes to its ring.
        unsafe {
            (*SCHED_TRACE_RINGS[core_id].get()).push(entry);
        }
    }

    /// Dump all sched-trace rings to serial output.
    ///
    /// Called from the panic handler or a debug path. Uses `_panic_print` to
    /// avoid heap allocation in a crash context.
    pub fn dump_sched_trace_rings() {
        use crate::serial::_panic_print;

        _panic_print(format_args!("=== SCHED-TRACE RING DUMP ===\n"));

        let core_count = crate::smp::core_count();
        let mut any = false;

        for core_id in 0..core_count {
            let core_idx = core_id as usize;
            if core_idx >= MAX_CORES {
                break;
            }
            // Safety: UnsafeCell, read only in crash context.
            unsafe {
                (*SCHED_TRACE_RINGS[core_idx].get()).for_each_chronological(|e| {
                    any = true;
                    _panic_print(format_args!(
                        "  [{}] core={} pid={} {}→{} {}:{}",
                        e.tick,
                        core_id,
                        e.pid,
                        e.old_state,
                        e.new_state,
                        e.caller_file,
                        e.caller_line,
                    ));
                    _panic_print(format_args!("\n"));
                });
            }
        }

        if !any {
            _panic_print(format_args!("  (no sched-trace events recorded)\n"));
        }

        _panic_print(format_args!("=== END SCHED-TRACE RING DUMP ===\n"));
    }
}
