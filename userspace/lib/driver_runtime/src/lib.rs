//! `driver_runtime` — Phase 55b ring-3 driver host library.
//!
//! This crate is the **template** for every future hardware-owning
//! userspace service in m3OS. Post-55b tracks (Phase 56 USB HID, later
//! phases for GPU / display-engine drivers, VirtIO-blk / VirtIO-net
//! extraction, future AHCI or Realtek drivers) land by consuming the
//! public surface below — they do not reshape it. Additions to the
//! public surface are permitted; renaming, removing, or changing the
//! semantics of an existing item is a breaking change subject to the
//! ordinary workspace deprecation process.
//!
//! # Public module surface
//!
//! | Module                | Purpose                                                                      |
//! |-----------------------|------------------------------------------------------------------------------|
//! | [`device`]            | `DeviceHandle::claim` and release — wraps `sys_device_claim`                 |
//! | [`mmio`]              | `Mmio<T>` read / write — wraps `sys_device_mmio_map`                         |
//! | [`dma`]               | `DmaBuffer<T>` allocator — wraps `sys_device_dma_alloc`                      |
//! | [`irq`]               | `IrqNotification` subscribe + wait — wraps `sys_device_irq_subscribe`        |
//! | [`ipc`]               | Driver-side IPC client helpers for the block and net protocols               |
//!
//! Track C.1 lands the shell of each module so the workspace, the
//! xtask build pipeline, and `cargo xtask check` all see a compilable
//! crate that downstream drivers can depend on. The concrete wrapper
//! bodies (`DeviceHandle::claim`, `Mmio<T>`, `DmaBuffer<T>`,
//! `IrqNotification`, block / net IPC clients) land in Tracks C.2,
//! C.3, and C.4.
//!
//! # Authoritative behavioral spec
//!
//! The shape and behavior each wrapper must satisfy live once, in
//! [`kernel_core::driver_runtime::contract`], and are exercised by the
//! suite at
//!
//! ```text
//! kernel-core/tests/driver_runtime_contract.rs
//! ```
//!
//! Track C.2 re-runs that suite against the real syscall backend in
//! QEMU. Every future backend (alternate OS personality, hardware
//! emulator, IOMMU path variant) lands by implementing the contract
//! traits and re-running the suite.
//!
//! # DRY boundaries
//!
//! Per the Phase 55b task list's DRY discipline, the shared ABI types
//! (`DeviceCapKey`, `MmioWindowDescriptor`, `DmaHandle`,
//! `DeviceHostError`) and the driver-IPC schema types (`BLK_*`,
//! `NET_*`, `BlkRequestHeader`, `NetFrameHeader`, `BlockDriverError`,
//! `NetDriverError`) live exactly once — in `kernel-core` — and this
//! crate re-exports them from the matching module. Drivers must not
//! redeclare any of them.

#![no_std]

extern crate alloc;

pub mod device;
pub mod dma;
pub mod ipc;
pub mod irq;
pub mod mmio;
pub mod syscall_backend;

// Track C.2 top-level re-exports — drivers import these directly rather
// than drilling through the module names.
pub use device::{DeviceCapKey, DeviceHandle};
pub use dma::{DmaBuffer, DmaHandle};
pub use mmio::{Mmio, MmioCacheMode, MmioWindowDescriptor};
pub use syscall_backend::SyscallBackend;

// ---------------------------------------------------------------------------
// C.3 public surface re-exports
// ---------------------------------------------------------------------------
//
// Track C.3 lands the concrete `IrqNotification` wrapper, the
// `IrqBackend` indirection (so tests can swap in a mock), the
// `DeviceCapHandle` minimal device-handle bound the wrapper
// consumes, and the `irq_loop` convenience. Drivers that only
// need the default syscall path `use driver_runtime::*;` and get
// all four.
pub use irq::{DeviceCapHandle, IrqBackend, IrqNotification, irq_loop};

/// Re-export of the authoritative `DriverRuntimeError` defined in
/// `kernel-core`. Every public fallible method in this crate returns
/// `Result<T, DriverRuntimeError>` — including the C.4 IPC client
/// helpers and the C.2 / C.3 safe wrappers. Consumers pattern-match
/// at this level without pulling in `kernel-core::driver_runtime`.
pub use kernel_core::driver_runtime::contract::DriverRuntimeError;

#[cfg(test)]
mod tests {
    //! C.1 green smoke tests.
    //!
    //! These tests exist so `cargo test -p driver_runtime` compiles
    //! the crate, the host-side mock backend in
    //! `kernel-core/tests/fixtures/driver_runtime_mock.rs` keeps
    //! compiling under the same `kernel-core` path dependency the
    //! crate pulls in, and a failing assertion shows up loudly if
    //! anyone accidentally drops one of the public modules.
    //!
    //! The authoritative behavioral suite lives at
    //! `kernel-core/tests/driver_runtime_contract.rs`; Track C.2
    //! re-runs it against the concrete wrappers landed in this
    //! crate.

    use kernel_core::device_host::DeviceHostError;
    use kernel_core::driver_runtime::contract::DriverRuntimeError;

    #[test]
    fn driver_runtime_public_module_surface_declared() {
        // Touch every public module so dropping one is a compile
        // error the next time this test is built. The referenced
        // re-exports are the ABI / contract types the Track C.2
        // wrappers and the Track C.4 IPC clients will consume.
        let _: Option<crate::device::DeviceCapKey> = None;
        let _: Option<crate::mmio::MmioWindowDescriptor> = None;
        let _: Option<crate::dma::DmaHandle> = None;
        // `irq` re-exports a trait (IrqNotificationHandle) — reference
        // it as a type parameter witness via a zero-sized marker to
        // avoid needing a concrete impl before Track C.2 lands.
        fn _irq_witness<T: crate::irq::IrqNotificationHandle>(_: &T) {}
        // `ipc::block` and `ipc::net` each re-export the authoritative
        // schema type; touching one of them proves the re-export
        // chain is intact.
        let _: Option<crate::ipc::block::BlkRequestHeader> = None;
        let _: Option<crate::ipc::net::NetFrameHeader> = None;
    }

    #[test]
    fn driver_runtime_error_lifts_from_device_host_error() {
        // Cross-check that the crate sees the same `DriverRuntimeError`
        // <- `DeviceHostError` conversion the A.4 contract suite
        // pins. This is the wire through which every syscall error
        // surfaces to a driver process.
        let err = DeviceHostError::AlreadyClaimed;
        let wrapped: DriverRuntimeError = err.into();
        assert_eq!(wrapped, DriverRuntimeError::Device(err));
    }
}
