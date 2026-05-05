//! Phase 57e diagnostic — per-frame allocate/free trace ring.
//!
//! Records every `allocate_frame` / `free_frame` / `allocate_contiguous` /
//! `free_contiguous` operation into a global circular buffer keyed by
//! physical frame address.  On a kernel page fault the handler can dump
//! the recent history of the offending frame to localise frame UAFs and
//! double-allocate races.
//!
//! Intentionally not feature-gated while the slab UAF residual is open —
//! the cost of a single Relaxed AtomicUsize fetch_add plus four u64
//! stores per allocation is acceptable while we hunt the bug.  Remove
//! or gate behind a `frame-trace` feature once the residual is closed.

use core::cell::UnsafeCell;
use core::panic::Location;
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::serial::_panic_print;

/// Number of entries in the global ring.  16 384 × 32 B = 512 KiB.
const FRAME_TRACE_RING_SIZE: usize = 16_384;

/// Operation tag stored in `FrameTraceEntry::op`.
#[derive(Copy, Clone, Debug)]
#[repr(u8)]
pub enum FrameOp {
    Alloc = 1,
    Free = 2,
    AllocContig = 3,
    FreeContig = 4,
    FreeDirect = 5,
}

#[derive(Copy, Clone)]
#[repr(C)]
struct FrameTraceEntry {
    tick: u64,
    frame_phys: u64,
    /// `&'static Location` pointer — interned by the compiler.  Print
    /// `loc.file():loc.line()` at dump time to identify the caller.
    caller_location: u64,
    /// Layout: bits 0..7 = core_id, bits 8..15 = op, bits 16..63 = order
    /// (only used for *Contig variants).
    packed: u64,
}

impl FrameTraceEntry {
    const EMPTY: Self = Self {
        tick: 0,
        frame_phys: 0,
        caller_location: 0,
        packed: 0,
    };

    fn new(
        tick: u64,
        frame_phys: u64,
        caller: *const Location<'static>,
        core_id: u8,
        op: FrameOp,
        order: u8,
    ) -> Self {
        let packed = (core_id as u64) | ((op as u64) << 8) | ((order as u64) << 16);
        Self {
            tick,
            frame_phys,
            caller_location: caller as u64,
            packed,
        }
    }

    fn core_id(&self) -> u8 {
        (self.packed & 0xFF) as u8
    }

    fn op_byte(&self) -> u8 {
        ((self.packed >> 8) & 0xFF) as u8
    }

    fn order(&self) -> u8 {
        ((self.packed >> 16) & 0xFF) as u8
    }
}

#[repr(C)]
struct Slot(UnsafeCell<FrameTraceEntry>);

unsafe impl Sync for Slot {}

struct FrameTraceRing {
    head: AtomicUsize,
    slots: [Slot; FRAME_TRACE_RING_SIZE],
}

unsafe impl Sync for FrameTraceRing {}

impl FrameTraceRing {
    const fn new() -> Self {
        // SAFETY: Slot is repr(C) with a single UnsafeCell field; zero is
        // a valid bit pattern for FrameTraceEntry::EMPTY.
        Self {
            head: AtomicUsize::new(0),
            slots: [const { Slot(UnsafeCell::new(FrameTraceEntry::EMPTY)) }; FRAME_TRACE_RING_SIZE],
        }
    }

    fn record(&self, op: FrameOp, frame_phys: u64, order: u8, caller: &'static Location<'static>) {
        let idx = self.head.fetch_add(1, Ordering::Relaxed) % FRAME_TRACE_RING_SIZE;
        let tick = crate::arch::x86_64::interrupts::tick_count();
        let core_id = if crate::smp::is_per_core_ready() {
            crate::smp::per_core().core_id
        } else {
            0xFF
        };
        let entry = FrameTraceEntry::new(tick, frame_phys, caller, core_id, op, order);
        // SAFETY: ring is best-effort diagnostic; torn writes across cores
        // are tolerated.  The slot pointer is always valid (static array).
        unsafe {
            *self.slots[idx].0.get() = entry;
        }
    }

    fn dump_for_frame(&self, target: u64, max: usize) {
        let head = self.head.load(Ordering::Acquire);
        let total = head.min(FRAME_TRACE_RING_SIZE);
        let mut printed = 0usize;
        let mut scanned = 0usize;
        while printed < max && scanned < total {
            let idx = head.wrapping_sub(1 + scanned) % FRAME_TRACE_RING_SIZE;
            // SAFETY: same as record.
            let entry = unsafe { *self.slots[idx].0.get() };
            scanned += 1;
            if entry.frame_phys != target {
                continue;
            }
            let op_str = match entry.op_byte() {
                1 => "ALLOC      ",
                2 => "FREE       ",
                3 => "ALLOC_CONTG",
                4 => "FREE_CONTG ",
                5 => "FREE_DIRECT",
                _ => "??         ",
            };
            let loc_ptr = entry.caller_location;
            if loc_ptr != 0 {
                let loc = unsafe { &*(loc_ptr as *const Location<'static>) };
                _panic_print(format_args!(
                    "[frame-trace] tick={} core={} op={} frame={:#x} order={} caller={}:{}\n",
                    entry.tick,
                    entry.core_id(),
                    op_str,
                    entry.frame_phys,
                    entry.order(),
                    loc.file(),
                    loc.line(),
                ));
            } else {
                _panic_print(format_args!(
                    "[frame-trace] tick={} core={} op={} frame={:#x} order={} caller=<unset>\n",
                    entry.tick,
                    entry.core_id(),
                    op_str,
                    entry.frame_phys,
                    entry.order(),
                ));
            }
            printed += 1;
        }
        if printed == 0 {
            _panic_print(format_args!(
                "[frame-trace] no recorded operations for frame {:#x} (head={} total={})\n",
                target, head, total
            ));
        } else {
            _panic_print(format_args!(
                "[frame-trace] {} matching entries for frame {:#x} (scanned {} of {} ring entries)\n",
                printed, target, scanned, total
            ));
        }
    }
}

static FRAME_TRACE: FrameTraceRing = FrameTraceRing::new();

/// Record an allocate event keyed by `frame_phys`.
#[inline]
pub fn record_alloc(frame_phys: u64, caller: &'static Location<'static>) {
    FRAME_TRACE.record(FrameOp::Alloc, frame_phys, 0, caller);
}

/// Record an `allocate_contiguous` event for `1 << order` pages starting at
/// `frame_phys`.
#[inline]
pub fn record_alloc_contig(frame_phys: u64, order: u8, caller: &'static Location<'static>) {
    FRAME_TRACE.record(FrameOp::AllocContig, frame_phys, order, caller);
}

/// Record a free event keyed by `frame_phys`.
#[inline]
pub fn record_free(frame_phys: u64, caller: &'static Location<'static>) {
    FRAME_TRACE.record(FrameOp::Free, frame_phys, 0, caller);
}

/// Record a `free_contiguous` event for `1 << order` pages starting at
/// `frame_phys`.
#[inline]
pub fn record_free_contig(frame_phys: u64, order: u8, caller: &'static Location<'static>) {
    FRAME_TRACE.record(FrameOp::FreeContig, frame_phys, order, caller);
}

/// Record a `free_frame_direct` event keyed by `frame_phys`.
#[inline]
pub fn record_free_direct(frame_phys: u64, caller: &'static Location<'static>) {
    FRAME_TRACE.record(FrameOp::FreeDirect, frame_phys, 0, caller);
}

/// Dump the most recent `max` operations for `target` to the panic-print
/// channel.  Best-effort; tolerates torn writes.
pub fn dump_for_frame(target: u64, max: usize) {
    FRAME_TRACE.dump_for_frame(target, max);
}
