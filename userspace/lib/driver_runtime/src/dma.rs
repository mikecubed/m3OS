//! DMA-buffer wrapper — Phase 55b Track C.2.
//!
//! Red-test skeleton. Concrete `DmaBuffer<T>::allocate`, `user_ptr` /
//! `iova` / `len` accessors, `Deref` / `DerefMut` impls, and the
//! [`DmaBufferContract`] impl for the real syscall backend land in the
//! following green commit. Tests below pin the observable behavior.

use core::marker::PhantomData;
use core::ops::{Deref, DerefMut};

#[allow(unused_imports)]
use kernel_core::device_host::DeviceHostError;
pub use kernel_core::device_host::DmaHandle;
use kernel_core::driver_runtime::contract::{
    DmaBufferContract, DmaBufferHandle, DriverRuntimeError,
};
use kernel_core::ipc::CapHandle;

use crate::device::DeviceHandle;
use crate::syscall_backend::SyscallBackend;

/// Re-export of the DMA allocation contracts from `kernel-core`.
pub use kernel_core::driver_runtime::contract::{
    DmaBufferContract as DmaBufferContractExt, DmaBufferHandle as DmaBufferHandleExt,
};

/// Reason a [`DmaBuffer`] wrapper could not be constructed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DmaConstructError {
    /// `user_va` was zero.
    NullAddress,
    /// `user_va` was not aligned to `align_of::<T>()`.
    Unaligned,
    /// `len` was zero.
    ZeroLength,
    /// `len` was smaller than `size_of::<T>()`.
    TooSmall,
}

/// Typed, capability-backed DMA buffer wrapper. Red-state skeleton.
pub struct DmaBuffer<T: ?Sized> {
    cap: CapHandle,
    handle: DmaHandle,
    _marker: PhantomData<fn() -> T>,
}

impl<T> DmaBuffer<T> {
    /// Allocate DMA-mapped memory. Red-state stub — always fails.
    pub fn allocate(
        _handle: &DeviceHandle,
        _size: usize,
        _align: usize,
    ) -> Result<Self, DriverRuntimeError> {
        Err(DriverRuntimeError::IrqTimeout)
    }
}

impl<T: ?Sized> DmaBuffer<T> {
    pub fn cap(&self) -> CapHandle {
        self.cap
    }
    pub fn handle(&self) -> DmaHandle {
        self.handle
    }
    pub fn iova(&self) -> u64 {
        self.handle.iova
    }
    pub fn len(&self) -> usize {
        self.handle.len
    }
    pub fn is_empty(&self) -> bool {
        self.handle.len == 0
    }

    /// Red-state stub: always rejects with `NullAddress` so the
    /// Unaligned / ZeroLength tests fail until the green commit replaces
    /// the body with the real checks.
    pub(crate) fn new_checked(
        _cap: CapHandle,
        _handle: DmaHandle,
    ) -> Result<Self, DmaConstructError> {
        Err(DmaConstructError::NullAddress)
    }
}

impl<T> DmaBuffer<T> {
    pub fn user_ptr(&self) -> *mut T {
        self.handle.user_va as *mut T
    }
}

impl<T> Deref for DmaBuffer<T> {
    type Target = T;
    fn deref(&self) -> &T {
        // SAFETY: red-state; the green commit adds debug-assert bounds
        // checks and documents the lifetime invariant.
        unsafe { &*(self.handle.user_va as *const T) }
    }
}

impl<T> DerefMut for DmaBuffer<T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *(self.handle.user_va as *mut T) }
    }
}

impl<T: ?Sized> DmaBufferHandle for DmaBuffer<T> {
    fn user_va(&self) -> usize {
        self.handle.user_va
    }
    fn iova(&self) -> u64 {
        self.handle.iova
    }
    fn len(&self) -> usize {
        self.handle.len
    }
}

impl<T: ?Sized> Drop for DmaBuffer<T> {
    fn drop(&mut self) {
        let _ = self.cap;
    }
}

impl<T: ?Sized> core::fmt::Debug for DmaBuffer<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("DmaBuffer")
            .field("cap", &self.cap)
            .field("user_va", &self.handle.user_va)
            .field("iova", &self.handle.iova)
            .field("len", &self.handle.len)
            .finish()
    }
}

impl DmaBufferContract for SyscallBackend {
    type DmaBuffer = DmaBuffer<[u8]>;

    fn allocate(
        &mut self,
        _handle: &Self::Handle,
        _size: usize,
        _align: usize,
    ) -> Result<Self::DmaBuffer, DriverRuntimeError> {
        Err(DriverRuntimeError::IrqTimeout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handle(user_va: usize, iova: u64, len: usize) -> DmaHandle {
        DmaHandle { user_va, iova, len }
    }

    /// Null `user_va` must surface [`DmaConstructError::NullAddress`].
    #[test]
    fn new_checked_rejects_null_user_va() {
        let err = DmaBuffer::<u32>::new_checked(1, handle(0, 0x1000, 4096))
            .expect_err("null user_va must be rejected");
        assert_eq!(err, DmaConstructError::NullAddress);
    }

    /// Zero-length buffer must surface [`DmaConstructError::ZeroLength`].
    #[test]
    fn new_checked_rejects_zero_length() {
        let err = DmaBuffer::<u32>::new_checked(1, handle(0x1000_0000, 0x1000, 0))
            .expect_err("zero length must be rejected");
        assert_eq!(err, DmaConstructError::ZeroLength);
    }

    /// Unaligned `user_va` must surface [`DmaConstructError::Unaligned`].
    #[test]
    fn new_checked_rejects_unaligned() {
        let err = DmaBuffer::<u32>::new_checked(1, handle(0x1000_0003, 0x1000, 4096))
            .expect_err("unaligned user_va must be rejected");
        assert_eq!(err, DmaConstructError::Unaligned);
    }

    /// Valid inputs construct a buffer whose accessors match the handle.
    #[test]
    fn new_checked_accepts_valid_inputs() {
        let buf = DmaBuffer::<u32>::new_checked(1, handle(0x1000_0000, 0x2_0000_0000, 4096))
            .expect("valid inputs");
        assert_eq!(buf.cap(), 1);
        assert_eq!(DmaBufferHandle::user_va(&buf), 0x1000_0000);
        assert_eq!(DmaBufferHandle::iova(&buf), 0x2_0000_0000);
        assert_eq!(DmaBufferHandle::len(&buf), 4096);
        assert_eq!(buf.iova(), 0x2_0000_0000);
        assert_eq!(buf.len(), 4096);
        assert!(!buf.is_empty());
    }

    /// [`Deref`] and [`DerefMut`] read and write the backing memory.
    #[test]
    fn deref_reads_and_writes_backing_memory() {
        let mut backing = [0u32; 16];
        let user_va = backing.as_mut_ptr() as usize;
        let len = core::mem::size_of_val(&backing);

        let mut buf =
            DmaBuffer::<u32>::new_checked(1, handle(user_va, 0xfeed_0000, len)).unwrap();
        *buf = 0xdead_beef;
        assert_eq!(backing[0], 0xdead_beef);

        assert_eq!(*buf, 0xdead_beef);
    }

    /// `user_ptr` returns the `user_va` from the handle.
    #[test]
    fn user_ptr_matches_handle_user_va() {
        let mut backing = [0u8; 64];
        let user_va = backing.as_mut_ptr() as usize;
        let buf = DmaBuffer::<u8>::new_checked(5, handle(user_va, 0x4000, 64)).unwrap();
        assert_eq!(buf.user_ptr() as usize, user_va);
    }

    /// `DmaBuffer<[u8]>` (the contract default) preserves length.
    #[test]
    fn byte_slice_buffer_length_preserved() {
        let mut backing = [0u8; 128];
        let user_va = backing.as_mut_ptr() as usize;
        let buf = DmaBuffer::<[u8]>::new_checked(9, handle(user_va, 0x8000, 128)).unwrap();
        assert_eq!(DmaBufferHandle::len(&buf), 128);
    }

    /// [`SyscallBackend`] must implement [`DmaBufferContract`].
    #[test]
    fn syscall_backend_implements_dma_buffer_contract() {
        fn witness<T: DmaBufferContract>() {}
        witness::<SyscallBackend>();
    }
}
