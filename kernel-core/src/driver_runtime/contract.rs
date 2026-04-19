//! Abstract contracts for `driver_runtime` wrappers — Phase 55b Track A.4.
//!
//! This module is the single source of truth for the **shape and behavior**
//! the Track C safe wrappers (`userspace/lib/driver_runtime`) and the real
//! syscall backend (Track B) must satisfy. The contracts are intentionally
//! abstract: no syscall numbers, no page-table walker internals, no IPC
//! payload layouts. Implementations plug in below this line.
//!
//! # Authoritative behavioral spec
//!
//! The authoritative behavioral spec for every implementation of these
//! traits lives at:
//!
//! ```text
//! kernel-core/tests/driver_runtime_contract.rs
//! ```
//!
//! That file is parameterized over the pure-logic `MockBackend` reference
//! implementation in `kernel-core/tests/fixtures/driver_runtime_mock.rs`.
//! The same suite is re-run by Track C.2 against the real syscall backend
//! in QEMU — passing it is the definition of "LSP-compliant
//! `driver_runtime`". Any new implementation (alternate OS personality,
//! hardware emulator, future IOMMU path) lands by adding an impl and
//! re-running the suite.
//!
//! # Traits and associated types
//!
//! The four contracts sit on the same backend type. Each one declares one
//! associated handle type, so a backend is free to pick a runtime
//! representation that fits its syscall ABI:
//!
//! | Contract                    | Associated type   | Represents                    |
//! |-----------------------------|-------------------|-------------------------------|
//! | [`DeviceHandleContract`]    | `Handle`          | a claimed PCI device          |
//! | [`MmioContract`]            | `MmioWindow`      | a mapped BAR window           |
//! | [`DmaBufferContract`]       | `DmaBuffer`       | a DMA-mapped buffer           |
//! | [`IrqNotificationContract`] | `IrqNotif`        | an IRQ subscription           |
//!
//! Associated types — not generic parameters — are used so a single
//! backend (the mock in tests, the `driver_runtime` crate in production)
//! picks one type per role. The traits are therefore **not** object-safe,
//! which is deliberate: polymorphism over `driver_runtime` backends is
//! resolved at compile time so the kernel-side facade (`RemoteBlockDevice`,
//! `RemoteNic`) and the driver processes both get monomorphized paths
//! without dyn-dispatch overhead. Contract tests substitute impls through
//! type parameters, not trait objects.
//!
//! # Accessor sub-traits
//!
//! [`DmaBufferHandle`] and [`IrqNotificationHandle`] are the behavioral
//! bounds on the corresponding associated types. A caller of the trait
//! surface reads `user_va` / `iova` / `len` through
//! [`DmaBufferHandle`] without knowing which backend built the buffer.
//! Drop-releases-handle is an associated invariant — documented on
//! [`DmaBufferContract::allocate`] — rather than a trait method, so the
//! wrapper crate can make Drop infallible.
//!
//! # Reused types
//!
//! Nothing is redeclared. [`DeviceCapKey`], [`DeviceHostError`],
//! [`DmaHandle`], and [`MmioWindowDescriptor`] come from
//! [`crate::device_host`] — the Phase 55b A.1 ABI module that is the
//! single source of truth.

use crate::device_host::DeviceHostError;
#[cfg(doc)]
use crate::device_host::{DeviceCapKey, DmaHandle, MmioWindowDescriptor};

// ---------------------------------------------------------------------------
// DriverRuntimeError
// ---------------------------------------------------------------------------

/// Error surface returned by every `driver_runtime` contract method.
///
/// Variants mirror [`DeviceHostError`] one-for-one through the
/// [`DriverRuntimeError::Device`] variant — so a caller that already
/// pattern-matches on `DeviceHostError` lifts trivially — plus three
/// wrapper-layer-only variants:
///
/// - [`DriverRuntimeError::UserFaultOnMmio`][] — a read or write against
///   an [`MmioContract`]-mapped window touched a range the wrapper could
///   not validate against the [`MmioWindowDescriptor`] the kernel
///   issued at map time. Drivers treat this as a bug in the driver,
///   not a hardware failure — the service manager restarts the driver.
/// - [`DriverRuntimeError::DmaHandleExpired`][] — a driver held a
///   [`DmaHandle`] across a capability-revocation event and tried to
///   use it afterwards. The wrapper detects the stale IOVA by comparing
///   against the live DMA cap table; the operation is refused.
/// - [`DriverRuntimeError::IrqTimeout`][] — [`IrqNotificationHandle::wait`]
///   exceeded its deadline without receiving a delivery. Bound by
///   [`crate::device_host::DRIVER_RESTART_TIMEOUT_MS`] for the
///   restart-bound tests in Tracks D, E, and F.
///
/// Variants carry data, not strings, so both kernel-side and userspace
/// code can pattern-match without allocation. `#[non_exhaustive]` lets
/// later tracks add variants (e.g. `CapRevoked`, `DriverRestarting`)
/// without forcing downstream crates into exhaustive match.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum DriverRuntimeError {
    /// A device-host-layer error bubbled up through the syscall backend.
    /// Every [`DeviceHostError`] variant reaches the wrapper wrapped in
    /// this case — there is no lossy conversion.
    Device(DeviceHostError),
    /// A user-mode access to an MMIO window failed bounds or alignment
    /// validation in the wrapper (before it reached the device).
    UserFaultOnMmio,
    /// A driver held a DMA handle across a revocation; the wrapper
    /// refused the operation rather than let the device DMA to a stale
    /// IOVA.
    DmaHandleExpired,
    /// [`IrqNotificationHandle::wait`] exceeded its deadline without a
    /// delivery.
    IrqTimeout,
}

impl From<DeviceHostError> for DriverRuntimeError {
    fn from(e: DeviceHostError) -> Self {
        DriverRuntimeError::Device(e)
    }
}

// ---------------------------------------------------------------------------
// DeviceHandleContract
// ---------------------------------------------------------------------------

/// Claim and release a PCI(e) device by capability key.
///
/// Implementations must satisfy the following observable behavior; the
/// authoritative spec lives in
/// `kernel-core/tests/driver_runtime_contract.rs`:
///
/// - [`DeviceHandleContract::claim`] returns a fresh handle for a
///   previously unclaimed [`DeviceCapKey`]. A second `claim` of the same
///   key while a handle is live returns
///   `DriverRuntimeError::Device(DeviceHostError::AlreadyClaimed)`.
/// - [`DeviceHandleContract::release`] consumes the handle, marks the
///   backing device capability as releasable, and returns `Ok(())`.
///   A `release` against a stale (already-released) handle returns
///   `DriverRuntimeError::Device(DeviceHostError::NotClaimed)`.
/// - After `release`, the handle is no longer usable as an argument to
///   [`MmioContract::map`], [`DmaBufferContract::allocate`], or
///   [`IrqNotificationContract::subscribe`] — those three return
///   `NotClaimed`.
///
/// See `kernel-core/tests/driver_runtime_contract.rs` for the pinning
/// test cases.
pub trait DeviceHandleContract {
    /// Opaque per-backend handle for a claimed device.
    type Handle;

    /// Claim the device identified by `key`. Returns the caller's handle
    /// or a [`DriverRuntimeError`] — see the trait-level docs for the
    /// observable error cases.
    fn claim(
        &mut self,
        key: crate::device_host::DeviceCapKey,
    ) -> Result<Self::Handle, DriverRuntimeError>;

    /// Release a previously-claimed handle. The handle is consumed on
    /// success; a second release attempt (against a stale handle)
    /// surfaces `DeviceHostError::NotClaimed`.
    fn release(&mut self, handle: Self::Handle) -> Result<(), DriverRuntimeError>;
}

// ---------------------------------------------------------------------------
// MmioContract
// ---------------------------------------------------------------------------

/// Map and access a BAR window on a claimed device.
///
/// The read/write variants come in 8-, 16-, 32-, and 64-bit widths to
/// match the natural register widths MMIO devices expose. Offsets are
/// measured in bytes from the start of the mapped BAR window; it is an
/// implementation-level validation question whether an offset past the
/// end of the window surfaces [`DriverRuntimeError::UserFaultOnMmio`] or
/// a silent zero read — but the contract suite at
/// `kernel-core/tests/driver_runtime_contract.rs` pins the observable
/// read-after-write behavior at every width.
///
/// Observable behavior every implementation satisfies:
///
/// - [`MmioContract::map`] against a live handle for a BAR the kernel
///   recognizes returns a window; `map` against a released handle
///   returns `NotClaimed`; `map` against an out-of-range BAR returns
///   `InvalidBarIndex`.
/// - Each `write_{u8,u16,u32,u64}` followed by a matching-width read at
///   the same offset returns the written value. Writes of different
///   widths at the same offset overlap per little-endian byte semantics.
/// - `read_*` and `write_*` do not return a result — they follow the
///   underlying MMIO semantics where a stray load / store either
///   succeeds or faults the handling layer directly. The wrapper layer
///   raises [`DriverRuntimeError::UserFaultOnMmio`] out-of-band (via
///   the capability validation layer); this trait surface is
///   deliberately value-returning so it matches the shape of raw MMIO.
pub trait MmioContract: DeviceHandleContract {
    /// Opaque per-backend MMIO window handle.
    type MmioWindow;

    /// Map a BAR window for the claimed device `handle`. See the
    /// trait-level docs for error cases.
    fn map(
        &mut self,
        handle: &Self::Handle,
        bar: u8,
    ) -> Result<Self::MmioWindow, DriverRuntimeError>;

    /// 8-bit register read at `offset` within the window.
    fn read_u8(&self, window: &Self::MmioWindow, offset: usize) -> u8;
    /// 16-bit register read at `offset` within the window (little-endian).
    fn read_u16(&self, window: &Self::MmioWindow, offset: usize) -> u16;
    /// 32-bit register read at `offset` within the window (little-endian).
    fn read_u32(&self, window: &Self::MmioWindow, offset: usize) -> u32;
    /// 64-bit register read at `offset` within the window (little-endian).
    fn read_u64(&self, window: &Self::MmioWindow, offset: usize) -> u64;

    /// 8-bit register write at `offset` within the window.
    fn write_u8(&mut self, window: &Self::MmioWindow, offset: usize, value: u8);
    /// 16-bit register write at `offset` within the window (little-endian).
    fn write_u16(&mut self, window: &Self::MmioWindow, offset: usize, value: u16);
    /// 32-bit register write at `offset` within the window (little-endian).
    fn write_u32(&mut self, window: &Self::MmioWindow, offset: usize, value: u32);
    /// 64-bit register write at `offset` within the window (little-endian).
    fn write_u64(&mut self, window: &Self::MmioWindow, offset: usize, value: u64);
}

// ---------------------------------------------------------------------------
// DmaBufferContract
// ---------------------------------------------------------------------------

/// Behavioral bounds every `DmaBuffer` handle satisfies.
///
/// Pulled out into its own trait so callers can consume a buffer's
/// `user_va` / `iova` / `len` without knowing which backend issued it.
/// Drop semantics live on the associated type: every implementation of
/// [`DmaBufferContract::allocate`] must produce a handle whose `Drop`
/// impl releases the underlying DMA record — the reference mock in
/// `kernel-core/tests/fixtures/driver_runtime_mock.rs` observes this via
/// `live_dma_count()`, and the authoritative contract suite asserts on
/// it.
pub trait DmaBufferHandle {
    /// Driver-process virtual address for the buffer. May legitimately
    /// be zero for kernel-internal staging buffers; the contract suite
    /// pins the non-zero case for normal driver allocations.
    fn user_va(&self) -> usize;

    /// Device-visible IOVA (or identity-mapped physical address on the
    /// Phase 55a `DmaBuffer<T>` fallback path).
    fn iova(&self) -> u64;

    /// Length of the DMA buffer in bytes. Matches the `size` passed to
    /// [`DmaBufferContract::allocate`].
    fn len(&self) -> usize;

    /// `true` if the buffer has zero length. Provided for Clippy parity
    /// with [`Self::len`]; not load-bearing in the contract suite.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Allocate DMA-mapped buffers tied to a claimed device.
///
/// Observable behavior every implementation satisfies (see the
/// authoritative spec at `kernel-core/tests/driver_runtime_contract.rs`):
///
/// - [`DmaBufferContract::allocate`] records `size` and `align` on the
///   returned buffer exactly as passed; `len() == size` on the result.
/// - Two successive allocations on the same handle return distinct
///   IOVAs (and distinct user VAs) — the backend's IOVA allocator is
///   responsible for the non-overlap guarantee.
/// - `allocate` against a released handle returns `NotClaimed`;
///   `allocate` when the IOVA space is exhausted returns
///   `IovaExhausted`.
/// - **Drop releases the handle**: `<Self::DmaBuffer as Drop>::drop`
///   returns the DMA record to the backend's free pool so a later
///   allocation may reuse the slot. This is the invariant Track C.2
///   will exercise against the real syscall backend; the mock exposes
///   `live_dma_count()` to make it observable in host tests.
pub trait DmaBufferContract: DeviceHandleContract {
    /// Per-backend DMA buffer handle. Must implement [`DmaBufferHandle`]
    /// (for the `user_va` / `iova` / `len` accessors) and [`Drop`] (which
    /// releases the handle).
    type DmaBuffer: DmaBufferHandle + Drop;

    /// Allocate `size` bytes of DMA-mapped memory aligned to `align`.
    fn allocate(
        &mut self,
        handle: &Self::Handle,
        size: usize,
        align: usize,
    ) -> Result<Self::DmaBuffer, DriverRuntimeError>;
}

// ---------------------------------------------------------------------------
// IrqNotificationContract
// ---------------------------------------------------------------------------

/// Behavioral bounds every `IrqNotif` handle satisfies.
///
/// Wait / ack live on the handle itself so a driver can hand the
/// subscription off to a worker thread without re-borrowing the
/// backend. The authoritative contract suite at
/// `kernel-core/tests/driver_runtime_contract.rs` covers the
/// deliver → wait → ack sequence and the no-delivery timeout path.
pub trait IrqNotificationHandle {
    /// Block until the next IRQ delivery, or return
    /// [`DriverRuntimeError::IrqTimeout`] if the deadline expires.
    /// Implementations set the deadline in terms of
    /// [`crate::device_host::DRIVER_RESTART_TIMEOUT_MS`].
    fn wait(&mut self) -> Result<(), DriverRuntimeError>;

    /// Acknowledge the most recent delivery. A driver's interrupt
    /// handler is not expected to be re-entrant, so `ack` is a pure
    /// bookkeeping call — the kernel has already re-armed the MSI-X
    /// entry by the time `wait` returned.
    fn ack(&mut self) -> Result<(), DriverRuntimeError>;
}

/// Subscribe to a device's IRQ line.
///
/// Observable behavior every implementation satisfies:
///
/// - [`IrqNotificationContract::subscribe`] against a live handle
///   returns a fresh `IrqNotif` tied to the device's MSI-X vector
///   allocation (or to the legacy pin, if MSI-X is unavailable).
/// - `subscribe` against a released handle returns `NotClaimed`;
///   `subscribe` when no vector is available returns `IrqUnavailable`.
/// - `vector_hint == Some(n)` asks the backend to prefer vector `n`
///   within the device's allocated range. Backends are free to ignore
///   the hint — the contract only asserts that `subscribe` succeeds
///   with or without a hint.
/// - The returned handle's Drop releases the subscription so the
///   kernel can reclaim the MSI-X slot.
pub trait IrqNotificationContract: DeviceHandleContract {
    /// Per-backend IRQ subscription handle. Must implement
    /// [`IrqNotificationHandle`] (for `wait` and `ack`) and [`Drop`]
    /// (which releases the subscription).
    type IrqNotif: IrqNotificationHandle + Drop;

    /// Subscribe to IRQs for the device backing `handle`, optionally
    /// hinting at a preferred MSI-X vector index.
    fn subscribe(
        &mut self,
        handle: &Self::Handle,
        vector_hint: Option<u8>,
    ) -> Result<Self::IrqNotif, DriverRuntimeError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device_host::DeviceHostError;

    #[test]
    fn driver_runtime_error_from_device_host_error_is_lossless() {
        for e in [
            DeviceHostError::NotClaimed,
            DeviceHostError::AlreadyClaimed,
            DeviceHostError::InvalidBarIndex,
            DeviceHostError::BarOutOfBounds,
            DeviceHostError::IovaExhausted,
            DeviceHostError::IommuFault,
            DeviceHostError::CapacityExceeded,
            DeviceHostError::IrqUnavailable,
            DeviceHostError::BadDeviceCap,
            DeviceHostError::Internal,
        ] {
            let wrapped: DriverRuntimeError = e.into();
            match wrapped {
                DriverRuntimeError::Device(inner) => assert_eq!(inner, e),
                other => panic!("unexpected variant: {:?}", other),
            }
        }
    }

    #[test]
    fn driver_runtime_error_wrapper_variants_are_distinct() {
        assert_ne!(
            DriverRuntimeError::UserFaultOnMmio,
            DriverRuntimeError::DmaHandleExpired
        );
        assert_ne!(
            DriverRuntimeError::DmaHandleExpired,
            DriverRuntimeError::IrqTimeout
        );
        assert_ne!(
            DriverRuntimeError::UserFaultOnMmio,
            DriverRuntimeError::IrqTimeout
        );
        assert_ne!(
            DriverRuntimeError::UserFaultOnMmio,
            DriverRuntimeError::Device(DeviceHostError::NotClaimed)
        );
    }
}
