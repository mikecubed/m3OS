//! MMIO-window wrapper — Phase 55b Track C.2.
//!
//! [`Mmio<T>`] is the ring-3 safe wrapper around a
//! [`Capability::Mmio`](kernel_core::ipc::Capability) mapping. Its shape
//! mirrors Phase 55's in-kernel `MmioRegion<T>` so drivers port with a
//! rename rather than a rewrite: the same `read_reg::<U>(offset)` and
//! `write_reg::<U>(offset, value)` signature, the same volatile
//! semantics, the same debug-assert bounds check against the mapped
//! length.
//!
//! The `T` type parameter is a pure typestate marker — it lets a driver
//! write `Mmio<NvmeRegs>` and `Mmio<E1000Regs>` so mixing up two BAR
//! windows is a compile-time error. The wrapper itself does not
//! dereference the marker type; volatile access goes through typed
//! `read_volatile` / `write_volatile` calls whose width is determined
//! by the `U: Copy` parameter of `read_reg` / `write_reg`.
//!
//! # Drop semantics
//!
//! Like [`crate::device::DeviceHandle`], there is no
//! `sys_device_mmio_unmap` syscall in Track B.2; the kernel reclaims the
//! mapping and the `Capability::Mmio` slot on process exit. Drop is a
//! no-op.

use core::marker::PhantomData;

use kernel_core::device_host::DeviceHostError;
pub use kernel_core::device_host::{MmioCacheMode, MmioWindowDescriptor};
use kernel_core::driver_runtime::contract::{DriverRuntimeError, MmioContract};
use kernel_core::ipc::CapHandle;

use crate::device::DeviceHandle;
use crate::syscall_backend::{SyscallBackend, decode_user_va_result, raw_sys_device_mmio_map};

/// Re-export of the authoritative MMIO contract from `kernel-core`.
pub use kernel_core::driver_runtime::contract::MmioContract as MmioContractExt;

/// Reason an [`Mmio`] wrapper could not be constructed in-process.
///
/// These are wrapper-layer-only diagnostics — they do not bubble up
/// from any kernel syscall. Produced by [`Mmio::new_checked`] (exposed
/// for the contract shim and test path) and consulted by [`Mmio::map`]
/// when lifting a failed post-syscall validation into
/// [`DriverRuntimeError`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MmioConstructError {
    /// `user_va` was zero. Null MMIO windows are always rejected —
    /// Phase 55b treats a zero user-VA as a driver bug.
    NullAddress,
    /// `user_va` was not aligned to `align_of::<u64>()`. MMIO accesses
    /// up to 64 bits must be naturally aligned.
    Unaligned,
    /// `len` was zero; a zero-length window cannot carry any register.
    ZeroLength,
}

/// Typed, capability-backed MMIO window wrapper.
///
/// Construction goes through [`Mmio::map`] (real-syscall path) or
/// [`Mmio::new_checked`] (contract-shim / test path). Both enforce the
/// null-pointer and alignment safety invariants up front so subsequent
/// `read_reg` / `write_reg` calls can skip the checks on the hot path.
pub struct Mmio<T> {
    /// Capability handle for the `Capability::Mmio` slot in the driver
    /// process's cap table. Retained on the wrapper so future tracks
    /// (C.3 IRQ, C.4 IPC client) can forward the cap across IPC
    /// without re-deriving it.
    cap: CapHandle,
    /// Driver-process virtual address of the mapped BAR window.
    /// Volatile accesses are computed as `user_va + offset`.
    user_va: usize,
    /// Length of the window in bytes — used for debug-assert bounds
    /// checking on every `read_reg` / `write_reg`.
    len: usize,
    /// Descriptor the kernel (or the shim) produced at map time.
    descriptor: MmioWindowDescriptor,
    _marker: PhantomData<fn() -> T>,
}

impl<T> Mmio<T> {
    /// Map BAR `bar_index` of `handle`'s claimed device into the driver
    /// process. Returns an [`Mmio<T>`] wrapping the resulting
    /// `Capability::Mmio` slot.
    ///
    /// `expected_len` is the driver's a-priori knowledge of the BAR
    /// size from its hardware spec (for example, NVMe controllers
    /// always map BAR0 as an uncacheable window sized per
    /// `CAP.MPSMIN`). It is recorded on the wrapper for bounds-
    /// checking; the kernel validated the actual BAR size during
    /// B.2's sizing dance.
    pub fn map(
        handle: &DeviceHandle,
        bar_index: u8,
        expected_len: usize,
    ) -> Result<Self, DriverRuntimeError> {
        // SAFETY: raw_sys_device_mmio_map is a pure syscall — arguments
        // are plain integers, return is an isize.
        let raw = unsafe { raw_sys_device_mmio_map(handle.cap(), bar_index) };
        let user_va = decode_user_va_result(raw)?;
        Self::new_checked(
            handle.cap(),
            user_va,
            expected_len,
            MmioWindowDescriptor {
                phys_base: 0,
                len: expected_len,
                bar_index,
                prefetchable: false,
                cache_mode: MmioCacheMode::Uncacheable,
            },
        )
        .map_err(|e| match e {
            MmioConstructError::NullAddress => {
                DriverRuntimeError::Device(DeviceHostError::Internal)
            }
            MmioConstructError::Unaligned => DriverRuntimeError::UserFaultOnMmio,
            MmioConstructError::ZeroLength => {
                DriverRuntimeError::Device(DeviceHostError::BarOutOfBounds)
            }
        })
    }

    /// Construct an [`Mmio<T>`] from a raw `(cap, user_va, len,
    /// descriptor)` tuple without invoking a syscall. Used by the
    /// contract shim and by host tests.
    ///
    /// Enforces the null-pointer and alignment invariants up front so
    /// the `read_reg` / `write_reg` hot path can elide them.
    #[inline]
    pub(crate) fn new_checked(
        cap: CapHandle,
        user_va: usize,
        len: usize,
        descriptor: MmioWindowDescriptor,
    ) -> Result<Self, MmioConstructError> {
        if user_va == 0 {
            return Err(MmioConstructError::NullAddress);
        }
        if !user_va.is_multiple_of(core::mem::align_of::<u64>()) {
            return Err(MmioConstructError::Unaligned);
        }
        if len == 0 {
            return Err(MmioConstructError::ZeroLength);
        }
        Ok(Self {
            cap,
            user_va,
            len,
            descriptor,
            _marker: PhantomData,
        })
    }

    /// The descriptor the kernel (or the shim) produced at map time.
    #[inline]
    pub fn descriptor(&self) -> MmioWindowDescriptor {
        self.descriptor
    }

    /// Capability handle for the underlying `Capability::Mmio` slot.
    #[inline]
    pub fn cap(&self) -> CapHandle {
        self.cap
    }

    /// Length of the mapped window in bytes.
    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    /// `true` if the mapped window is zero-length. Every construction
    /// path rejects zero-length windows, so callers should never see
    /// `true` here.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Driver-process virtual address of the window base.
    #[inline]
    pub fn user_va(&self) -> usize {
        self.user_va
    }

    /// Read a register of width `U: Copy` at `offset` from the window
    /// base.
    ///
    /// Safety is enforced through:
    /// - construction-time alignment check (`user_va` is 8-aligned),
    /// - per-call debug-assert bounds check against `self.len`.
    #[inline]
    pub fn read_reg<U: Copy>(&self, offset: usize) -> U {
        debug_assert!(
            offset.saturating_add(core::mem::size_of::<U>()) <= self.len,
            "Mmio::read_reg: offset {offset:#x} + size {size} > len {len:#x}",
            size = core::mem::size_of::<U>(),
            len = self.len
        );
        let ptr = (self.user_va + offset) as *const U;
        // SAFETY: `user_va` was validated non-null and 8-aligned at
        // construction; `offset + size_of::<U>() <= len` is debug-
        // asserted above. The backing memory is live for the wrapper's
        // lifetime (the kernel holds the `Capability::Mmio` slot until
        // process exit — see the module docs).
        unsafe { core::ptr::read_volatile(ptr) }
    }

    /// Write `value` to a register of width `U: Copy` at `offset` from
    /// the window base. Width constraints match [`Self::read_reg`].
    #[inline]
    pub fn write_reg<U: Copy>(&self, offset: usize, value: U) {
        debug_assert!(
            offset.saturating_add(core::mem::size_of::<U>()) <= self.len,
            "Mmio::write_reg: offset {offset:#x} + size {size} > len {len:#x}",
            size = core::mem::size_of::<U>(),
            len = self.len
        );
        let ptr = (self.user_va + offset) as *mut U;
        // SAFETY: identical to `read_reg`; `&self` suffices because
        // MMIO writes are treated as interior-mutable — a driver can
        // hold multiple `&Mmio<T>` refs and issue writes from each.
        unsafe { core::ptr::write_volatile(ptr, value) }
    }
}

impl<T> core::fmt::Debug for Mmio<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Mmio")
            .field("cap", &self.cap)
            .field("user_va", &format_args!("{:#x}", self.user_va))
            .field("len", &format_args!("{:#x}", self.len))
            .field("bar_index", &self.descriptor.bar_index)
            .finish()
    }
}

impl<T> Drop for Mmio<T> {
    fn drop(&mut self) {
        // See module docs — kernel reclaims on process exit.
    }
}

// ---------------------------------------------------------------------------
// Contract shim
// ---------------------------------------------------------------------------

impl MmioContract for SyscallBackend {
    type MmioWindow = Mmio<()>;

    fn map(
        &mut self,
        handle: &Self::Handle,
        bar: u8,
    ) -> Result<Self::MmioWindow, DriverRuntimeError> {
        // The contract surface does not carry the driver's expected
        // BAR length; a conservative 64 KiB matches the Phase 55
        // NVMe/e1000 BAR0 sizes and the MockBackend's synthesized
        // descriptor in A.4.
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

    #[test]
    fn new_checked_rejects_null_user_va() {
        let err = Mmio::<()>::new_checked(1, 0, 0x1000, descriptor(0x1000))
            .expect_err("null user_va must be rejected");
        assert_eq!(err, MmioConstructError::NullAddress);
    }

    #[test]
    fn new_checked_rejects_unaligned_user_va() {
        let err = Mmio::<()>::new_checked(1, 0x1000_0001, 0x1000, descriptor(0x1000))
            .expect_err("unaligned user_va must be rejected");
        assert_eq!(err, MmioConstructError::Unaligned);
    }

    #[test]
    fn new_checked_rejects_zero_length() {
        let err = Mmio::<()>::new_checked(1, 0x1000_0000, 0, descriptor(0))
            .expect_err("zero length must be rejected");
        assert_eq!(err, MmioConstructError::ZeroLength);
    }

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

    #[test]
    fn read_write_reg_round_trip_all_widths() {
        // Back the wrapper with a host-owned byte buffer. Volatile reads
        // and writes go through the wrapper's raw pointer path.
        let mut backing = [0u8; 256];
        let base = backing.as_mut_ptr() as usize;
        let m = Mmio::<()>::new_checked(1, base, backing.len(), descriptor(backing.len())).unwrap();

        m.write_reg::<u8>(0, 0xab);
        assert_eq!(m.read_reg::<u8>(0), 0xab);

        m.write_reg::<u16>(16, 0xbeef);
        assert_eq!(m.read_reg::<u16>(16), 0xbeef);

        m.write_reg::<u32>(32, 0xdead_beef);
        assert_eq!(m.read_reg::<u32>(32), 0xdead_beef);

        m.write_reg::<u64>(64, 0xfeed_face_cafe_d00d);
        assert_eq!(m.read_reg::<u64>(64), 0xfeed_face_cafe_d00d);
    }

    #[test]
    fn syscall_backend_implements_mmio_contract() {
        fn witness<T: MmioContract>() {}
        witness::<SyscallBackend>();
    }
}
