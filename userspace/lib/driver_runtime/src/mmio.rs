//! MMIO-window wrapper — Phase 55b Track C.2.
//!
//! Red-test skeleton. The concrete `Mmio<T>::map`, volatile `read_reg` /
//! `write_reg` accessors, and the [`MmioContract`] impl for the real
//! syscall backend land in the following green commit. The tests below
//! pin the observable behavior every implementation must satisfy.

use core::marker::PhantomData;

#[allow(unused_imports)]
use kernel_core::device_host::DeviceHostError;
pub use kernel_core::device_host::{MmioCacheMode, MmioWindowDescriptor};
use kernel_core::driver_runtime::contract::{DriverRuntimeError, MmioContract};
use kernel_core::ipc::CapHandle;

use crate::device::DeviceHandle;
use crate::syscall_backend::SyscallBackend;

/// Re-export of the authoritative MMIO contract from `kernel-core`.
pub use kernel_core::driver_runtime::contract::MmioContract as MmioContractExt;

/// Reason an [`Mmio`] wrapper could not be constructed in-process.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MmioConstructError {
    /// `user_va` was zero.
    NullAddress,
    /// `user_va` was not aligned to `align_of::<u64>()`.
    Unaligned,
    /// `len` was zero.
    ZeroLength,
}

/// Typed, capability-backed MMIO window wrapper. Red-state skeleton.
pub struct Mmio<T> {
    cap: CapHandle,
    user_va: usize,
    len: usize,
    descriptor: MmioWindowDescriptor,
    _marker: PhantomData<fn() -> T>,
}

impl<T> Mmio<T> {
    /// Map a BAR window. Red-state stub — always fails.
    pub fn map(
        _handle: &DeviceHandle,
        _bar_index: u8,
        _expected_len: usize,
    ) -> Result<Self, DriverRuntimeError> {
        Err(DriverRuntimeError::IrqTimeout)
    }

    /// Construct from a raw tuple. Red-state stub — rejects everything
    /// so the null/unaligned/zero-length tests assert on the specific
    /// error kind and fail until the green commit lands.
    pub(crate) fn new_checked(
        _cap: CapHandle,
        _user_va: usize,
        _len: usize,
        _descriptor: MmioWindowDescriptor,
    ) -> Result<Self, MmioConstructError> {
        // Red-state: always claim NullAddress — mismatches the Unaligned
        // and ZeroLength test expectations.
        Err(MmioConstructError::NullAddress)
    }

    pub fn descriptor(&self) -> MmioWindowDescriptor {
        self.descriptor
    }
    pub fn cap(&self) -> CapHandle {
        self.cap
    }
    pub fn len(&self) -> usize {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    pub fn user_va(&self) -> usize {
        self.user_va
    }

    /// Red-state stub: returns a zero-valued `U`. Green commit replaces
    /// this with volatile `read_volatile`.
    pub fn read_reg<U: Copy>(&self, _offset: usize) -> U {
        // SAFETY: `MaybeUninit::zeroed().assume_init()` on a `Copy` type
        // returns a bit-pattern of zeros. This is a red-state stub; the
        // green commit replaces it with volatile MMIO reads.
        unsafe { core::mem::MaybeUninit::zeroed().assume_init() }
    }

    /// Red-state stub: no-op. Green commit replaces with volatile write.
    pub fn write_reg<U: Copy>(&self, _offset: usize, _value: U) {}
}

impl<T> core::fmt::Debug for Mmio<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Mmio")
            .field("cap", &self.cap)
            .field("user_va", &self.user_va)
            .field("len", &self.len)
            .finish()
    }
}

impl<T> Drop for Mmio<T> {
    fn drop(&mut self) {}
}

impl MmioContract for SyscallBackend {
    type MmioWindow = Mmio<()>;

    fn map(
        &mut self,
        handle: &Self::Handle,
        bar: u8,
    ) -> Result<Self::MmioWindow, DriverRuntimeError> {
        Mmio::map(handle, bar, 0x1_0000)
    }

    fn read_u8(&self, window: &Self::MmioWindow, offset: usize) -> u8 {
        window.read_reg::<u8>(offset)
    }
    fn read_u16(&self, window: &Self::MmioWindow, offset: usize) -> u16 {
        window.read_reg::<u16>(offset)
    }
    fn read_u32(&self, window: &Self::MmioWindow, offset: usize) -> u32 {
        window.read_reg::<u32>(offset)
    }
    fn read_u64(&self, window: &Self::MmioWindow, offset: usize) -> u64 {
        window.read_reg::<u64>(offset)
    }

    fn write_u8(&mut self, window: &Self::MmioWindow, offset: usize, value: u8) {
        window.write_reg::<u8>(offset, value);
    }
    fn write_u16(&mut self, window: &Self::MmioWindow, offset: usize, value: u16) {
        window.write_reg::<u16>(offset, value);
    }
    fn write_u32(&mut self, window: &Self::MmioWindow, offset: usize, value: u32) {
        window.write_reg::<u32>(offset, value);
    }
    fn write_u64(&mut self, window: &Self::MmioWindow, offset: usize, value: u64) {
        window.write_reg::<u64>(offset, value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor(len: usize) -> MmioWindowDescriptor {
        MmioWindowDescriptor {
            phys_base: 0xfeb0_0000,
            len,
            bar_index: 0,
            prefetchable: false,
            cache_mode: MmioCacheMode::Uncacheable,
        }
    }

    /// Null `user_va` must surface [`MmioConstructError::NullAddress`].
    #[test]
    fn new_checked_rejects_null_user_va() {
        let err = Mmio::<()>::new_checked(1, 0, 0x1000, descriptor(0x1000))
            .expect_err("null user_va must be rejected");
        assert_eq!(err, MmioConstructError::NullAddress);
    }

    /// Unaligned `user_va` must surface [`MmioConstructError::Unaligned`].
    #[test]
    fn new_checked_rejects_unaligned_user_va() {
        let err = Mmio::<()>::new_checked(1, 0x1000_0001, 0x1000, descriptor(0x1000))
            .expect_err("unaligned user_va must be rejected");
        assert_eq!(err, MmioConstructError::Unaligned);
    }

    /// Zero-length mapping must surface [`MmioConstructError::ZeroLength`].
    #[test]
    fn new_checked_rejects_zero_length() {
        let err = Mmio::<()>::new_checked(1, 0x1000_0000, 0, descriptor(0))
            .expect_err("zero length must be rejected");
        assert_eq!(err, MmioConstructError::ZeroLength);
    }

    /// Valid inputs succeed and round-trip through the accessors.
    #[test]
    fn new_checked_accepts_aligned_nonzero_inputs() {
        let m = Mmio::<()>::new_checked(7, 0x1000_0000, 0x1000, descriptor(0x1000))
            .expect("valid inputs");
        assert_eq!(m.cap(), 7);
        assert_eq!(m.user_va(), 0x1000_0000);
        assert_eq!(m.len(), 0x1000);
        assert!(!m.is_empty());
        assert_eq!(m.descriptor(), descriptor(0x1000));
    }

    /// Read-after-write round-trips at every natural MMIO width. The
    /// wrapper is backed by a host-owned byte buffer; the green impl
    /// reads/writes through volatile pointers.
    #[test]
    fn read_write_reg_round_trip_all_widths() {
        let mut backing = [0u8; 256];
        let base = backing.as_mut_ptr() as usize;
        let m =
            Mmio::<()>::new_checked(1, base, backing.len(), descriptor(backing.len())).unwrap();

        m.write_reg::<u8>(0, 0xab);
        assert_eq!(m.read_reg::<u8>(0), 0xab);

        m.write_reg::<u16>(16, 0xbeef);
        assert_eq!(m.read_reg::<u16>(16), 0xbeef);

        m.write_reg::<u32>(32, 0xdead_beef);
        assert_eq!(m.read_reg::<u32>(32), 0xdead_beef);

        m.write_reg::<u64>(64, 0xfeed_face_cafe_d00d);
        assert_eq!(m.read_reg::<u64>(64), 0xfeed_face_cafe_d00d);
    }

    /// [`SyscallBackend`] must implement [`MmioContract`].
    #[test]
    fn syscall_backend_implements_mmio_contract() {
        fn witness<T: MmioContract>() {}
        witness::<SyscallBackend>();
    }
}
