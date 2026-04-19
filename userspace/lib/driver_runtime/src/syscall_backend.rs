//! Real-syscall backend for the `driver_runtime` contract traits — Phase
//! 55b Track C.2.
//!
//! Red-test skeleton. The `SyscallBackend` type and error-decoding
//! helpers compile against the contract traits so the wrappers in
//! `device.rs`, `mmio.rs`, and `dma.rs` can reference them; the
//! syscall-invoking bodies and the full errno-decode tables land in the
//! following green commit.

use kernel_core::device_host::{DeviceHostError, DmaHandle};
use kernel_core::driver_runtime::contract::DriverRuntimeError;
use kernel_core::ipc::CapHandle;

/// Real-syscall backend. Zero-sized — construction is free.
#[derive(Default, Clone, Copy, Debug)]
pub struct SyscallBackend;

impl SyscallBackend {
    /// Construct a fresh [`SyscallBackend`].
    #[inline]
    pub const fn new() -> Self {
        Self
    }
}

// ---------------------------------------------------------------------------
// Error decoding — red-state stubs
// ---------------------------------------------------------------------------

/// Decode a capability-handle syscall return into a [`CapHandle`] or a
/// [`DriverRuntimeError`]. Red-state stub: always returns `Internal`.
#[inline]
#[allow(dead_code)]
pub(crate) fn decode_cap_handle_result(_raw: isize) -> Result<CapHandle, DriverRuntimeError> {
    Err(DriverRuntimeError::Device(DeviceHostError::Internal))
}

/// Decode a user-VA syscall return into a `usize` or a
/// [`DriverRuntimeError`]. Red-state stub.
#[inline]
#[allow(dead_code)]
pub(crate) fn decode_user_va_result(_raw: isize) -> Result<usize, DriverRuntimeError> {
    Err(DriverRuntimeError::Device(DeviceHostError::Internal))
}

/// Fetch the `(user_va, iova, len)` tuple for a `Capability::Dma`.
/// Red-state stub.
///
/// # Safety
///
/// Caller must pass a valid `Capability::Dma` handle.
#[inline]
#[allow(dead_code)]
pub(crate) unsafe fn fetch_dma_handle(_dma_cap: CapHandle) -> Result<DmaHandle, DriverRuntimeError> {
    Err(DriverRuntimeError::Device(DeviceHostError::Internal))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// [`SyscallBackend`] must remain zero-sized — it is held by the
    /// wrappers and the drivers as a zero-cost default.
    #[test]
    fn syscall_backend_is_zero_sized_and_default_constructible() {
        assert_eq!(core::mem::size_of::<SyscallBackend>(), 0);
        let _ = SyscallBackend::new();
        let _ = SyscallBackend;
    }

    /// The error-decoding helpers live in this module so host tests
    /// cover every branch without invoking real syscalls. The green
    /// commit adds the full errno → `DriverRuntimeError` table.
    #[test]
    fn decode_cap_handle_result_accepts_nonneg() {
        // Red-state: stub rejects everything, which makes this test
        // fail until the green commit lands.
        assert_eq!(decode_cap_handle_result(0), Ok(0));
    }

    #[test]
    fn decode_cap_handle_result_maps_every_track_b_errno() {
        assert_eq!(
            decode_cap_handle_result(-16),
            Err(DriverRuntimeError::Device(DeviceHostError::AlreadyClaimed))
        );
        assert_eq!(
            decode_cap_handle_result(-19),
            Err(DriverRuntimeError::Device(DeviceHostError::NotClaimed))
        );
        assert_eq!(
            decode_cap_handle_result(-9),
            Err(DriverRuntimeError::Device(DeviceHostError::BadDeviceCap))
        );
        assert_eq!(
            decode_cap_handle_result(-12),
            Err(DriverRuntimeError::Device(DeviceHostError::CapacityExceeded))
        );
        assert_eq!(
            decode_cap_handle_result(-22),
            Err(DriverRuntimeError::Device(DeviceHostError::InvalidBarIndex))
        );
    }

    #[test]
    fn decode_user_va_result_accepts_nonneg() {
        assert_eq!(decode_user_va_result(0x1000_0000), Ok(0x1000_0000));
    }
}
