//! Shared IOMMU fault logging helpers — Phase 55a Tracks C.4 and D.4.
//!
//! Fault IRQ handlers on both vendors (Intel VT-d and AMD-Vi) decode
//! hardware-specific fault records into a vendor-neutral
//! [`kernel_core::iommu::contract::FaultRecord`] and hand them to this
//! module. The module provides two cheap, non-allocating services:
//!
//! 1. [`log_fault_event`] — structured `log::warn!` with subsystem tag,
//!    vendor, requester BDF, IOVA, and fault reason. Format is stable so
//!    integration tests can grep for it.
//! 2. [`FAULT_HANDLER`] — a single global slot holding the optional
//!    user-supplied [`FaultHandlerFn`] a vendor driver installs via
//!    `IommuUnit::install_fault_handler`. Drivers both register into and
//!    dispatch through this slot, so a test can install a handler once
//!    without caring which unit raised the fault.
//!
//! # IRQ-safety contract
//!
//! Both callers run in IRQ context. This module does **not** allocate,
//! does **not** take any blocking lock, and does **not** perform any I/O
//! beyond the `log` crate backend (which the serial subsystem makes
//! IRQ-safe). Keep new code in this file to the same standard.

use core::sync::atomic::{AtomicPtr, AtomicU64, Ordering};

use kernel_core::iommu::contract::{FaultHandlerFn, FaultRecord};

/// Monotonic count of IOMMU faults the kernel has observed since boot.
///
/// Incremented once per call to [`log_fault_event`] — so every fault
/// delivered through the shared dispatch path bumps the counter, whether
/// or not a user handler is installed. Diagnostic and test code reads
/// this via [`fault_count`] to verify the fault-delivery path is alive
/// without having to parse serial logs.
///
/// `Relaxed` ordering is sufficient: this is a single-writer-per-fault
/// observable that the test code reads with no cross-thread
/// synchronization requirements.
static FAULT_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Shared slot for the user-supplied fault callback.
///
/// Held as a lock-free [`AtomicPtr`] rather than a mutex so the IRQ
/// dispatch path can never block on installer code. On x86_64 a
/// function pointer and a raw pointer share the same size, layout, and
/// alignment, so a `FaultHandlerFn` round-trips cleanly through
/// `AtomicPtr<()>` via `as`/`transmute`. `Release` on write pairs with
/// `Acquire` on read to publish the handler to other cores; the IRQ
/// path never allocates, never spins, and never contends with an
/// installer.
static FAULT_HANDLER: AtomicPtr<()> = AtomicPtr::new(core::ptr::null_mut());

/// Install `handler` into the shared slot, replacing any previous one.
pub fn install(handler: FaultHandlerFn) {
    FAULT_HANDLER.store(handler as *mut (), Ordering::Release);
}

/// Snapshot the current handler from IRQ context. Returns `None` if
/// no vendor has called [`install`] yet. Lock-free and bounded — safe
/// to invoke from an IOMMU-fault ISR.
pub fn current() -> Option<FaultHandlerFn> {
    let raw = FAULT_HANDLER.load(Ordering::Acquire);
    if raw.is_null() {
        None
    } else {
        // SAFETY: every non-null value in the slot was published by
        // `install` from a valid `FaultHandlerFn`. Function pointers and
        // `*mut ()` share size and alignment on x86_64, so the transmute
        // round-trips the exact bit pattern `install` stored.
        Some(unsafe { core::mem::transmute::<*mut (), FaultHandlerFn>(raw) })
    }
}

/// Default IOMMU fault handler installed at boot so the IRQ vector is
/// always reserved, the trampoline is always in the IDT, and hardware
/// FEDATA / FEADDR are always programmed to deliver faults. The body is
/// intentionally empty — [`log_fault_event`] has already recorded the
/// fault before this function runs, which is the only observable the
/// kernel needs today. A test or diagnostic tool can replace this with
/// a richer handler via [`install`].
pub fn default_handler(_record: &FaultRecord) {
    // Intentionally empty — the shared log already captured the event.
}

/// Structured log line emitted for every IOMMU fault the kernel
/// handles. Format is stable so F.3's integration test can search
/// serial logs for it.
///
/// Example:
///
/// ```text
/// [iommu] subsystem=iommu vendor=vtd requester_bdf=0x0100 iova=0xdeadbeef fault_reason=0x0005
/// ```
pub fn log_fault_event(vendor: &str, requester_bdf: u16, iova: u64, fault_reason: u16) {
    FAULT_COUNTER.fetch_add(1, Ordering::Relaxed);
    log::warn!(
        "[iommu] subsystem=iommu vendor={} requester_bdf=0x{:04x} iova={:#x} fault_reason={:#x}",
        vendor,
        requester_bdf,
        iova,
        fault_reason
    );
}

/// Monotonic IOMMU-fault count since boot.
///
/// Read by the F.3 fault-delivery test and by diagnostic tooling. Does
/// not distinguish VT-d from AMD-Vi and does not preserve per-BDF
/// history — it is strictly a "has any fault been logged?" observable.
///
/// Marked `#[allow(dead_code)]` because the only in-tree caller today
/// is `#[cfg(test)]` F.3 and a future diagnostic command will surface
/// the value through `meminfo` / `dmesg`.
#[allow(dead_code)]
pub fn fault_count() -> u64 {
    FAULT_COUNTER.load(Ordering::Relaxed)
}

/// Deliver one fault record through the log path and the installed
/// handler. Safe to call from IRQ context; performs no allocation and
/// never blocks.
pub fn dispatch(vendor: &str, record: &FaultRecord) {
    log_fault_event(
        vendor,
        record.requester_bdf,
        record.iova.0,
        record.fault_reason,
    );
    if let Some(handler) = current() {
        handler(record);
    }
}
