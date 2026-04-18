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

use kernel_core::iommu::contract::{FaultHandlerFn, FaultRecord};
use spin::Mutex;

/// Shared slot for the user-supplied fault callback. Held in a `Mutex`
/// so installers serialize against one another; the IRQ path grabs a
/// local copy (a single function pointer) and releases the lock before
/// invoking the user code so the handler can itself call back into the
/// IOMMU path without re-entering.
pub static FAULT_HANDLER: Mutex<Option<FaultHandlerFn>> = Mutex::new(None);

/// Install `handler` into the shared slot, replacing any previous one.
pub fn install(handler: FaultHandlerFn) {
    *FAULT_HANDLER.lock() = Some(handler);
}

/// Snapshot the current handler without holding the lock across the
/// invocation. Called from IRQ context — returns `None` if the
/// corresponding vendor never called `install_fault_handler`.
pub fn current() -> Option<FaultHandlerFn> {
    *FAULT_HANDLER.lock()
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
    log::warn!(
        "[iommu] subsystem=iommu vendor={} requester_bdf=0x{:04x} iova={:#x} fault_reason={:#x}",
        vendor,
        requester_bdf,
        iova,
        fault_reason
    );
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
