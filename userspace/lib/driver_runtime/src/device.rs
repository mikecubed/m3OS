//! Device-claim wrapper — Phase 55b Track C.2.
//!
//! [`DeviceHandle`] is the ring-3 safe wrapper around a
//! [`Capability::Device`](kernel_core::ipc::Capability) obtained via the
//! Phase 55b B.1 `sys_device_claim` syscall. Its public surface matches the
//! abstract [`DeviceHandleContract`] from `kernel-core` so drivers can use
//! either `DeviceHandle::claim` directly (production path) or the
//! [`SyscallBackend`] contract shim (test / future-backend path).
//!
//! # Drop semantics
//!
//! The B.1 kernel implementation has no `sys_device_release` syscall — the
//! kernel cleans up `Capability::Device` entries on process exit or kill.
//! Accordingly, [`DeviceHandle`]'s `Drop` impl is a logging no-op: it
//! observes the `released` flag and emits no syscall. The F.2 crash-and-
//! restart regression test (Track F.2) depends on this: a driver process
//! killed by the service manager releases its device claim through normal
//! process teardown, not through an explicit release syscall.
//!
//! The [`DeviceHandle::release`] method is provided for symmetry with
//! [`DeviceHandleContract::release`] and for drivers that want to
//! explicitly relinquish a claim before exit. Because there is no
//! dedicated release syscall, it currently consumes the handle and
//! performs the same no-op — a future phase may wire a real
//! `sys_device_release` here without changing call sites.

use core::sync::atomic::{AtomicBool, Ordering};

use kernel_core::driver_runtime::contract::{DeviceHandleContract, DriverRuntimeError};
use kernel_core::ipc::CapHandle;

use crate::syscall_backend::{SyscallBackend, decode_cap_handle_result, raw_sys_device_claim};

// Re-export of the authoritative device-claim contract from `kernel-core`.
pub use kernel_core::driver_runtime::contract::DeviceHandleContract as DeviceHandleContractExt;
// Re-export of the capability-key ABI type shared with kernel-side
// syscall handlers.
pub use kernel_core::device_host::DeviceCapKey;

/// Ring-3 safe wrapper for a claimed PCI(e) device capability.
///
/// The wrapper owns a Phase 50 [`CapHandle`] referring to a
/// `Capability::Device` slot in the driver process's capability table,
/// plus the matching [`DeviceCapKey`] so the driver can re-identify the
/// BDF for log lines and IPC payloads without a round-trip to the
/// kernel.
///
/// Construction goes through [`DeviceHandle::claim`]; Drop is a logging
/// no-op because the kernel releases the capability on process exit (see
/// the module docs).
pub struct DeviceHandle {
    cap: CapHandle,
    key: DeviceCapKey,
    // `released` is set by `release` so Drop can observe whether the
    // explicit path was taken; F.2's restart regression uses that
    // distinction.
    released: AtomicBool,
}

impl DeviceHandle {
    /// Claim the PCI(e) device identified by `key`. On success the
    /// kernel installs a `Capability::Device` in the driver process's
    /// capability table and this wrapper owns the resulting handle.
    ///
    /// Errors lift one-for-one from the kernel-side surface into
    /// [`DriverRuntimeError::Device`]:
    ///
    /// - `AlreadyClaimed` — another driver already holds the claim.
    /// - `NotClaimed` — the BDF is not present in the PCI bus.
    /// - `CapacityExceeded` — the caller's capability table is full.
    /// - `BadDeviceCap` — the kernel rejected the capability shape.
    pub fn claim(key: DeviceCapKey) -> Result<Self, DriverRuntimeError> {
        // SAFETY: raw_sys_device_claim is a pure syscall — the inputs
        // are plain integers and the return is an isize. No pointer
        // lifetime constraints are involved.
        let raw = unsafe { raw_sys_device_claim(key) };
        let cap = decode_cap_handle_result(raw)?;
        Ok(Self {
            cap,
            key,
            released: AtomicBool::new(false),
        })
    }

    /// Capability handle this wrapper owns. Used by [`crate::mmio`] and
    /// [`crate::dma`] to derive further capabilities (MMIO windows, DMA
    /// buffers, IRQ subscriptions) from the same claim.
    #[inline]
    pub fn cap(&self) -> CapHandle {
        self.cap
    }

    /// The [`DeviceCapKey`] this handle was claimed against. Preserved
    /// on the wrapper so drivers can format log lines and IPC payloads
    /// without re-querying the kernel.
    #[inline]
    pub fn key(&self) -> DeviceCapKey {
        self.key
    }

    /// Explicitly release the claim. Consumes `self`. Because there is
    /// no dedicated `sys_device_release` syscall today, this is a no-op
    /// — the kernel releases the claim when the driver process exits.
    /// The method is consumed-by-value so a future phase can wire a
    /// real release syscall without changing call sites.
    pub fn release(self) -> Result<(), DriverRuntimeError> {
        self.released.store(true, Ordering::Release);
        Ok(())
    }
}

impl core::fmt::Debug for DeviceHandle {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("DeviceHandle")
            .field("cap", &self.cap)
            .field("key", &self.key)
            .field("released", &self.released.load(Ordering::Acquire))
            .finish()
    }
}

impl Drop for DeviceHandle {
    fn drop(&mut self) {
        // Logging no-op. We do not emit trace output here: (a) this
        // crate is `no_std` + `alloc` without a logging facade, and (b)
        // the Phase 55b observability discipline records
        // `device_host.claim` at claim time; the release is implicit on
        // process exit and covered by the service-manager log. The
        // `Acquire` load pairs with `release`'s `Release` store for
        // future code that might branch on it.
        let _ = self.released.load(Ordering::Acquire);
    }
}

impl DeviceHandleContract for SyscallBackend {
    type Handle = DeviceHandle;

    fn claim(&mut self, key: DeviceCapKey) -> Result<Self::Handle, DriverRuntimeError> {
        DeviceHandle::claim(key)
    }

    fn release(&mut self, handle: Self::Handle) -> Result<(), DriverRuntimeError> {
        handle.release()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kernel_core::device_host::DeviceHostError;

    #[test]
    fn syscall_backend_implements_device_handle_contract() {
        fn witness<T: DeviceHandleContract>() {}
        witness::<SyscallBackend>();
    }

    #[test]
    fn device_handle_exposes_cap_and_key() {
        let key = DeviceCapKey::new(0, 0x01, 0x00, 0);
        let h = DeviceHandle {
            cap: 42,
            key,
            released: AtomicBool::new(false),
        };
        assert_eq!(h.cap(), 42);
        assert_eq!(h.key(), key);
    }

    #[test]
    fn device_handle_release_is_consuming_and_idempotent_with_drop() {
        let key = DeviceCapKey::new(0, 0x02, 0x00, 0);
        let h = DeviceHandle {
            cap: 7,
            key,
            released: AtomicBool::new(false),
        };
        h.release().expect("release succeeds");
    }

    #[test]
    fn device_handle_claim_on_host_returns_internal_error() {
        // Host-test path: syscall_backend's stub returns `-ENOSYS`
        // (-38), which decodes to DeviceHostError::Internal.
        let key = DeviceCapKey::new(0, 0x03, 0x00, 0);
        let err = DeviceHandle::claim(key).expect_err("no kernel on host");
        assert_eq!(err, DriverRuntimeError::Device(DeviceHostError::Internal));
    }
}
