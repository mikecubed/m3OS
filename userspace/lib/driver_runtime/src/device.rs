//! Device-claim wrapper — Phase 55b Track C.2.
//!
//! Red-test skeleton. The concrete `DeviceHandle::claim`, RAII Drop, and
//! [`DeviceHandleContract`] impl for the real-syscall backend land in the
//! following green commit. The tests below pin the observable behavior
//! every implementation must satisfy; they compile against the skeleton
//! and fail at runtime until the green implementation lands.

use core::sync::atomic::AtomicBool;

use kernel_core::driver_runtime::contract::{DeviceHandleContract, DriverRuntimeError};
use kernel_core::ipc::CapHandle;

use crate::syscall_backend::SyscallBackend;

// Re-export of the authoritative device-claim contract from `kernel-core`.
pub use kernel_core::driver_runtime::contract::DeviceHandleContract as DeviceHandleContractExt;
// Re-export of the capability-key ABI type shared with kernel-side
// syscall handlers.
pub use kernel_core::device_host::DeviceCapKey;

/// Ring-3 safe wrapper for a claimed PCI(e) device capability.
///
/// Red-state skeleton — the green commit fills in the real
/// `sys_device_claim` path.
pub struct DeviceHandle {
    cap: CapHandle,
    key: DeviceCapKey,
    released: AtomicBool,
}

impl DeviceHandle {
    /// Claim a device by BDF. Red-state stub: always returns
    /// [`DriverRuntimeError::Device(DeviceHostError::Internal)`] so the
    /// acceptance tests fail until the green commit lands.
    pub fn claim(_key: DeviceCapKey) -> Result<Self, DriverRuntimeError> {
        // Red-state sentinel — distinct from every legitimate syscall
        // error so the acceptance tests' assertions mis-match until the
        // green commit replaces this body with the real syscall path.
        Err(DriverRuntimeError::IrqTimeout)
    }

    /// Capability handle this wrapper owns. Red-state returns a sentinel.
    #[inline]
    pub fn cap(&self) -> CapHandle {
        self.cap
    }

    /// The [`DeviceCapKey`] this handle was claimed against.
    #[inline]
    pub fn key(&self) -> DeviceCapKey {
        self.key
    }

    /// Explicitly release the claim. Red-state stub.
    pub fn release(self) -> Result<(), DriverRuntimeError> {
        // Red-state sentinel — see `claim` above.
        Err(DriverRuntimeError::IrqTimeout)
    }
}

impl core::fmt::Debug for DeviceHandle {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("DeviceHandle")
            .field("cap", &self.cap)
            .field("key", &self.key)
            .finish()
    }
}

impl Drop for DeviceHandle {
    fn drop(&mut self) {
        // Red-state no-op.
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

    /// Contract witness: `SyscallBackend` must implement
    /// [`DeviceHandleContract`]. Failing to compile this test is
    /// equivalent to failing the contract suite at runtime.
    #[test]
    fn syscall_backend_implements_device_handle_contract() {
        fn witness<T: DeviceHandleContract>() {}
        witness::<SyscallBackend>();
    }

    /// `DeviceHandle::cap` / `DeviceHandle::key` must round-trip the
    /// values a successful claim produced. In the red state this runs
    /// against a directly-constructed handle so a missing accessor is a
    /// compile error.
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

    /// `DeviceHandle::release` consumes the handle and reports success
    /// exactly once in the green implementation. The red stub returns
    /// an error so this test fails at runtime until the green commit.
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

    /// On the host (no kernel), `claim` must surface an `Internal`
    /// device-host error — the syscall is implemented as `-ENOSYS` by
    /// the host-test stub in `syscall_backend`, and the wrapper maps
    /// that onto `DriverRuntimeError::Device(DeviceHostError::Internal)`.
    #[test]
    fn device_handle_claim_on_host_returns_internal_error() {
        let key = DeviceCapKey::new(0, 0x03, 0x00, 0);
        let err = DeviceHandle::claim(key).expect_err("no kernel on host");
        assert_eq!(err, DriverRuntimeError::Device(DeviceHostError::Internal));
    }
}
