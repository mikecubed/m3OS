//! DMA-buffer wrapper — Phase 55b Track C.2.
//!
//! [`DmaBuffer<T>`] is the ring-3 safe wrapper around a
//! [`Capability::Dma`](kernel_core::ipc::Capability) plus the
//! [`DmaHandle`] tuple the kernel produces. Its shape matches Phase 55's
//! in-kernel `DmaBuffer<T>`: `user_ptr() -> *mut T`, `iova() -> u64`,
//! `len() -> usize`, `Deref<Target = T>`, `DerefMut`. A driver port
//! from the kernel-side to the ring-3 HAL is a rename plus an import
//! change.
//!
//! # Drop semantics
//!
//! The B.3 kernel implementation frees the DMA allocation when the
//! `Capability::Dma` slot is released. Because there is no
//! `sys_device_dma_free` syscall exposed to userspace today, Drop is a
//! logging no-op — the kernel frees the backing frames and tears down
//! the IOMMU mapping when the driver process exits (per the B.3
//! rollback path). F.2's crash-restart regression depends on this.

use core::marker::PhantomData;
use core::ops::{Deref, DerefMut};

use kernel_core::device_host::DeviceHostError;
pub use kernel_core::device_host::DmaHandle;
use kernel_core::driver_runtime::contract::{
    DmaBufferContract, DmaBufferHandle, DriverRuntimeError,
};
use kernel_core::ipc::CapHandle;

use crate::device::DeviceHandle;
use crate::syscall_backend::{
    SyscallBackend, decode_cap_handle_result, fetch_dma_handle, raw_sys_device_dma_alloc,
};

/// Re-export of the DMA allocation contracts from `kernel-core`.
pub use kernel_core::driver_runtime::contract::{
    DmaBufferContract as DmaBufferContractExt, DmaBufferHandle as DmaBufferHandleExt,
};

/// Reason a [`DmaBuffer`] wrapper could not be constructed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DmaConstructError {
    /// `user_va` was zero. A zero-valued user VA is legal for
    /// kernel-internal staging buffers (per [`DmaHandle`]'s docs), but
    /// the ring-3 wrapper always rejects it — the wrapper only
    /// constructs buffers meant to be dereferenced in the driver
    /// process.
    NullAddress,
    /// `user_va` was not aligned to `align_of::<u64>()` (the weakest
    /// alignment that admits all the ring element types the drivers
    /// use). Deref would be UB otherwise.
    Unaligned,
    /// `len` was zero; a zero-length buffer cannot hold any `T`.
    ZeroLength,
    /// `len` was smaller than `size_of::<T>()`; the backing allocation
    /// cannot hold a single `T`. Deref would be UB.
    TooSmall,
}

/// Typed, capability-backed DMA buffer wrapper.
pub struct DmaBuffer<T: ?Sized> {
    /// Capability handle for the `Capability::Dma` slot in the driver
    /// process's cap table. Retained for the contract shim and for
    /// Drop (a no-op today; see module docs).
    cap: CapHandle,
    /// `(user_va, iova, len)` tuple the kernel produced at allocate
    /// time.
    handle: DmaHandle,
    _marker: PhantomData<fn() -> T>,
}

impl<T> DmaBuffer<T> {
    /// Allocate `size` bytes of DMA-mapped memory tied to `handle`'s
    /// claimed device, aligned to `align`.
    ///
    /// Errors lift from [`DeviceHostError`] via
    /// [`DriverRuntimeError::Device`]:
    ///
    /// - `NotClaimed` — the device handle was released.
    /// - `IovaExhausted` — the IOMMU domain is full.
    /// - `CapacityExceeded` — the per-driver DMA cap is exhausted.
    /// - `Internal` — the kernel produced an invalid `DmaHandle`.
    pub fn allocate(
        handle: &DeviceHandle,
        size: usize,
        align: usize,
    ) -> Result<Self, DriverRuntimeError> {
        // SAFETY: raw_sys_device_dma_alloc is a pure syscall.
        let raw = unsafe { raw_sys_device_dma_alloc(handle.cap(), size, align) };
        let dma_cap = decode_cap_handle_result(raw)?;
        // SAFETY: fetch_dma_handle reads the kernel-written tuple into
        // a stack-allocated buffer; pointer lifetime is bounded by the
        // function body.
        let dma_handle = unsafe { fetch_dma_handle(dma_cap) }?;
        Self::new_checked(dma_cap, dma_handle).map_err(|e| match e {
            DmaConstructError::NullAddress => DriverRuntimeError::Device(DeviceHostError::Internal),
            DmaConstructError::Unaligned => DriverRuntimeError::Device(DeviceHostError::Internal),
            DmaConstructError::ZeroLength => {
                DriverRuntimeError::Device(DeviceHostError::CapacityExceeded)
            }
            DmaConstructError::TooSmall => {
                DriverRuntimeError::Device(DeviceHostError::CapacityExceeded)
            }
        })
    }

    /// Driver-process virtual address of the buffer as a raw `*mut T`.
    /// Available on sized `T` only; the unsized `DmaBuffer<[u8]>` byte
    /// variant exposes [`DmaBufferHandle::user_va`] instead, which
    /// returns a `usize` without needing fat-pointer metadata.
    #[inline]
    pub fn user_ptr(&self) -> *mut T {
        self.handle.user_va as *mut T
    }
}

impl<T: ?Sized> DmaBuffer<T> {
    /// Construct a [`DmaBuffer<T>`] from a raw cap + handle pair
    /// without invoking a syscall. Used by the contract shim and by
    /// host tests.
    #[inline]
    pub(crate) fn new_checked(
        cap: CapHandle,
        handle: DmaHandle,
    ) -> Result<Self, DmaConstructError> {
        if handle.user_va == 0 {
            return Err(DmaConstructError::NullAddress);
        }
        if handle.len == 0 {
            return Err(DmaConstructError::ZeroLength);
        }
        if !handle.user_va.is_multiple_of(core::mem::align_of::<u64>()) {
            return Err(DmaConstructError::Unaligned);
        }
        Ok(Self {
            cap,
            handle,
            _marker: PhantomData,
        })
    }

    /// Capability handle for the underlying `Capability::Dma` slot.
    #[inline]
    pub fn cap(&self) -> CapHandle {
        self.cap
    }

    /// The `(user_va, iova, len)` tuple the kernel produced.
    #[inline]
    pub fn handle(&self) -> DmaHandle {
        self.handle
    }

    /// Device-visible IOVA the device should see in descriptor rings.
    /// Equals the physical address on the Phase 55a identity-fallback
    /// path; equals the IOMMU-installed IOVA otherwise.
    #[inline]
    pub fn iova(&self) -> u64 {
        self.handle.iova
    }

    /// Length of the buffer in bytes.
    #[inline]
    pub fn len(&self) -> usize {
        self.handle.len
    }

    /// `true` if the buffer has zero length. Every construction path
    /// rejects zero length, so callers should never see `true` here.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.handle.len == 0
    }
}

impl<T> Deref for DmaBuffer<T> {
    type Target = T;
    fn deref(&self) -> &T {
        debug_assert!(
            self.handle.len >= core::mem::size_of::<T>(),
            "DmaBuffer::deref: len {} < size_of::<T>() {}",
            self.handle.len,
            core::mem::size_of::<T>()
        );
        // SAFETY: `user_va` was validated non-null and word-aligned at
        // construction; `len >= size_of::<T>()` is debug-asserted
        // above. The backing memory is live for the wrapper's lifetime
        // (the kernel holds the `Capability::Dma` slot until process
        // exit).
        unsafe { &*(self.handle.user_va as *const T) }
    }
}

impl<T> DerefMut for DmaBuffer<T> {
    fn deref_mut(&mut self) -> &mut T {
        debug_assert!(
            self.handle.len >= core::mem::size_of::<T>(),
            "DmaBuffer::deref_mut: len {} < size_of::<T>() {}",
            self.handle.len,
            core::mem::size_of::<T>()
        );
        // SAFETY: as for Deref; `&mut self` on the wrapper rules out
        // aliasing.
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
        // See module docs — kernel frees on process exit. The `_`
        // reference keeps `cap` considered live for Drop audits.
        let _ = self.cap;
    }
}

impl<T: ?Sized> core::fmt::Debug for DmaBuffer<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("DmaBuffer")
            .field("cap", &self.cap)
            .field("user_va", &format_args!("{:#x}", self.handle.user_va))
            .field("iova", &format_args!("{:#x}", self.handle.iova))
            .field("len", &format_args!("{:#x}", self.handle.len))
            .finish()
    }
}

// ---------------------------------------------------------------------------
// Contract shim
// ---------------------------------------------------------------------------

impl DmaBufferContract for SyscallBackend {
    // The contract default is a byte-slice DMA buffer — matches Phase
    // 55's `DmaBuffer<[u8]>::allocate` byte-raw allocation path.
    type DmaBuffer = DmaBuffer<[u8]>;

    fn allocate(
        &mut self,
        handle: &Self::Handle,
        size: usize,
        align: usize,
    ) -> Result<Self::DmaBuffer, DriverRuntimeError> {
        // SAFETY: identical to `DmaBuffer::<T>::allocate`.
        let raw = unsafe { raw_sys_device_dma_alloc(handle.cap(), size, align) };
        let dma_cap = decode_cap_handle_result(raw)?;
        let dma_handle = unsafe { fetch_dma_handle(dma_cap) }?;
        DmaBuffer::<[u8]>::new_checked(dma_cap, dma_handle).map_err(|e| match e {
            DmaConstructError::NullAddress => DriverRuntimeError::Device(DeviceHostError::Internal),
            DmaConstructError::Unaligned => DriverRuntimeError::Device(DeviceHostError::Internal),
            DmaConstructError::ZeroLength => {
                DriverRuntimeError::Device(DeviceHostError::CapacityExceeded)
            }
            DmaConstructError::TooSmall => {
                DriverRuntimeError::Device(DeviceHostError::CapacityExceeded)
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handle(user_va: usize, iova: u64, len: usize) -> DmaHandle {
        DmaHandle { user_va, iova, len }
    }

    #[test]
    fn new_checked_rejects_null_user_va() {
        let err = DmaBuffer::<u32>::new_checked(1, handle(0, 0x1000, 4096))
            .expect_err("null user_va must be rejected");
        assert_eq!(err, DmaConstructError::NullAddress);
    }

    #[test]
    fn new_checked_rejects_zero_length() {
        let err = DmaBuffer::<u32>::new_checked(1, handle(0x1000_0000, 0x1000, 0))
            .expect_err("zero length must be rejected");
        assert_eq!(err, DmaConstructError::ZeroLength);
    }

    #[test]
    fn new_checked_rejects_unaligned() {
        let err = DmaBuffer::<u32>::new_checked(1, handle(0x1000_0003, 0x1000, 4096))
            .expect_err("unaligned user_va must be rejected");
        assert_eq!(err, DmaConstructError::Unaligned);
    }

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

    #[test]
    fn deref_reads_and_writes_backing_memory() {
        let mut backing = [0u32; 16];
        let user_va = backing.as_mut_ptr() as usize;
        let len = core::mem::size_of_val(&backing);

        let mut buf = DmaBuffer::<u32>::new_checked(1, handle(user_va, 0xfeed_0000, len)).unwrap();
        *buf = 0xdead_beef;
        assert_eq!(backing[0], 0xdead_beef);

        assert_eq!(*buf, 0xdead_beef);
    }

    #[test]
    fn user_ptr_matches_handle_user_va() {
        let mut backing = [0u8; 64];
        let user_va = backing.as_mut_ptr() as usize;
        let buf = DmaBuffer::<u8>::new_checked(5, handle(user_va, 0x4000, 64)).unwrap();
        assert_eq!(buf.user_ptr() as usize, user_va);
    }

    #[test]
    fn byte_slice_buffer_length_preserved() {
        let mut backing = [0u8; 128];
        let user_va = backing.as_mut_ptr() as usize;
        let buf = DmaBuffer::<[u8]>::new_checked(9, handle(user_va, 0x8000, 128)).unwrap();
        assert_eq!(DmaBufferHandle::len(&buf), 128);
    }

    #[test]
    fn syscall_backend_implements_dma_buffer_contract() {
        fn witness<T: DmaBufferContract>() {}
        witness::<SyscallBackend>();
    }
}
