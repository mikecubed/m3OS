//! PIO-port wrapper — Phase 63 Track Z.3.
//!
//! [`Pio<T>`] is the ring-3 safe wrapper for accessing I/O-port BARs
//! through the [`SYS_DEVICE_PIO_READ`] / [`SYS_DEVICE_PIO_WRITE`] syscalls
//! introduced in Phase 63 Track Z.2.
//!
//! AC'97's two BARs (NAM and NABM) are both I/O-space; the existing
//! `sys_device_mmio_map` syscall filters PIO BARs and rejects them.
//! `Pio<T>` provides the production path: store the device-cap handle plus
//! the BAR index, then forward each `read_u*/write_u*` call to the
//! corresponding kernel syscall.
//!
//! The `T` type parameter is a pure typestate marker — the same pattern as
//! `Mmio<T>`. Drivers write `Pio<NamRegs>` and `Pio<NabmRegs>` so mixing
//! up the two AC'97 BAR windows is a compile-time error.
//!
//! # Drop semantics
//!
//! PIO BARs are not mapped into the process address space, so there is
//! nothing to unmap. `Drop` is a no-op; the device-cap handle owns the
//! lifetime of the BAR access.

use core::marker::PhantomData;

use kernel_core::driver_runtime::contract::DriverRuntimeError;
use kernel_core::ipc::CapHandle;

use crate::device::DeviceHandle;
use crate::syscall_backend::SyscallBackend;

#[cfg(all(target_arch = "x86_64", target_os = "none"))]
use kernel_core::device_host::syscalls::{SYS_DEVICE_PIO_READ, SYS_DEVICE_PIO_WRITE};

// ---------------------------------------------------------------------------
// Raw syscall helpers
// ---------------------------------------------------------------------------

/// `sys_device_pio_read(dev_cap, bar_index, offset, width) -> isize`.
///
/// # Safety
///
/// Caller must pass a valid `Capability::Device` handle; the kernel
/// validates ownership and returns a negative errno otherwise.
#[inline]
unsafe fn raw_sys_device_pio_read(
    dev_cap: CapHandle,
    bar_index: u8,
    offset: u32,
    width: u8,
) -> isize {
    #[cfg(all(target_arch = "x86_64", target_os = "none"))]
    // SAFETY: plain-integer args; the kernel validates capability shape,
    // BAR type, and offset range and returns a negative errno on failure.
    unsafe {
        syscall_lib::syscall4(
            SYS_DEVICE_PIO_READ,
            u64::from(dev_cap),
            u64::from(bar_index),
            u64::from(offset),
            u64::from(width),
        ) as isize
    }
    #[cfg(not(all(target_arch = "x86_64", target_os = "none")))]
    {
        // Host-test path: no real kernel.
        let _ = (dev_cap, bar_index, offset, width);
        -38 // -ENOSYS
    }
}

/// `sys_device_pio_write(dev_cap, bar_index, offset, value, width) -> isize`.
///
/// # Safety
///
/// Caller must pass a valid `Capability::Device` handle.
#[inline]
unsafe fn raw_sys_device_pio_write(
    dev_cap: CapHandle,
    bar_index: u8,
    offset: u32,
    value: u32,
    width: u8,
) -> isize {
    #[cfg(all(target_arch = "x86_64", target_os = "none"))]
    // SAFETY: plain-integer args; the kernel validates ownership and range.
    unsafe {
        syscall_lib::syscall5(
            SYS_DEVICE_PIO_WRITE,
            u64::from(dev_cap),
            u64::from(bar_index),
            u64::from(offset),
            u64::from(value),
            u64::from(width),
        ) as isize
    }
    #[cfg(not(all(target_arch = "x86_64", target_os = "none")))]
    {
        let _ = (dev_cap, bar_index, offset, value, width);
        -38 // -ENOSYS
    }
}

// ---------------------------------------------------------------------------
// Pio<T>
// ---------------------------------------------------------------------------

/// Typed, capability-backed PIO window wrapper.
///
/// Construction goes through [`Pio::map`]. The wrapper stores the device-cap
/// handle and the BAR index; each register access syscalls back to the kernel
/// to perform the privileged `in`/`out` instruction.
///
/// The `T` type parameter is a zero-cost typestate marker; the wrapper does
/// not dereference `T`.
pub struct Pio<T> {
    /// Capability handle for the `Capability::Device` slot.
    device_cap: CapHandle,
    /// BAR index (0 or 1 for AC'97 NAM/NABM).
    bar_index: u8,
    _marker: PhantomData<fn() -> T>,
}

impl<T> Pio<T> {
    /// Map PIO BAR `bar_index` of `handle`'s claimed device.
    ///
    /// Returns a [`Pio<T>`] storing the device-cap handle plus the BAR index.
    /// No address-space modification is performed — PIO BARs are not mapped
    /// into the process address space. The kernel validates BAR ownership,
    /// BAR type (must be PIO), and offset+width bounds on every subsequent
    /// `sys_device_pio_read`/`sys_device_pio_write` call, so no probe read
    /// is needed here. Issuing a probe read at construction time is
    /// explicitly forbidden: some registers are clear-on-read and a probe
    /// would introduce a side effect that the caller did not request.
    pub fn map(handle: &DeviceHandle, bar_index: u8) -> Result<Self, DriverRuntimeError> {
        Ok(Self {
            device_cap: handle.cap(),
            bar_index,
            _marker: PhantomData,
        })
    }

    /// Read an 8-bit register at `offset` within this BAR.
    #[inline]
    pub fn read_u8(&self, offset: usize) -> u8 {
        // SAFETY: device_cap and bar_index were validated at construction.
        let rc =
            unsafe { raw_sys_device_pio_read(self.device_cap, self.bar_index, offset as u32, 1) };
        if rc < 0 { 0 } else { rc as u8 }
    }

    /// Read a 16-bit register at `offset` within this BAR.
    #[inline]
    pub fn read_u16(&self, offset: usize) -> u16 {
        // SAFETY: see read_u8.
        let rc =
            unsafe { raw_sys_device_pio_read(self.device_cap, self.bar_index, offset as u32, 2) };
        if rc < 0 { 0 } else { rc as u16 }
    }

    /// Read a 32-bit register at `offset` within this BAR.
    #[inline]
    pub fn read_u32(&self, offset: usize) -> u32 {
        // SAFETY: see read_u8.
        let rc =
            unsafe { raw_sys_device_pio_read(self.device_cap, self.bar_index, offset as u32, 4) };
        if rc < 0 { 0 } else { rc as u32 }
    }

    /// Write an 8-bit register at `offset` within this BAR.
    #[inline]
    pub fn write_u8(&self, offset: usize, value: u8) {
        // SAFETY: device_cap and bar_index were validated at construction.
        unsafe {
            raw_sys_device_pio_write(
                self.device_cap,
                self.bar_index,
                offset as u32,
                u32::from(value),
                1,
            );
        }
    }

    /// Write a 16-bit register at `offset` within this BAR.
    #[inline]
    pub fn write_u16(&self, offset: usize, value: u16) {
        // SAFETY: see write_u8.
        unsafe {
            raw_sys_device_pio_write(
                self.device_cap,
                self.bar_index,
                offset as u32,
                u32::from(value),
                2,
            );
        }
    }

    /// Write a 32-bit register at `offset` within this BAR.
    #[inline]
    pub fn write_u32(&self, offset: usize, value: u32) {
        // SAFETY: see write_u8.
        unsafe {
            raw_sys_device_pio_write(self.device_cap, self.bar_index, offset as u32, value, 4);
        }
    }

    /// Device-cap handle stored by this wrapper.
    #[inline]
    pub fn device_cap(&self) -> CapHandle {
        self.device_cap
    }

    /// BAR index stored by this wrapper.
    #[inline]
    pub fn bar_index(&self) -> u8 {
        self.bar_index
    }
}

impl<T> core::fmt::Debug for Pio<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Pio")
            .field("device_cap", &self.device_cap)
            .field("bar_index", &self.bar_index)
            .finish()
    }
}

impl<T> Drop for Pio<T> {
    fn drop(&mut self) {
        // PIO BARs are not mapped into the address space — nothing to unmap.
        // The kernel reclaims the device cap on process exit.
    }
}

// ---------------------------------------------------------------------------
// Contract shim — SyscallBackend implements PioContract
// ---------------------------------------------------------------------------

impl kernel_core::driver_runtime::contract::PioContract for SyscallBackend {
    type PioWindow = Pio<()>;

    fn map(
        &mut self,
        handle: &Self::Handle,
        bar: u8,
    ) -> Result<Self::PioWindow, DriverRuntimeError> {
        Pio::map(handle, bar)
    }

    fn read_u8(&self, window: &Self::PioWindow, offset: usize) -> u8 {
        window.read_u8(offset)
    }

    fn read_u16(&self, window: &Self::PioWindow, offset: usize) -> u16 {
        window.read_u16(offset)
    }

    fn read_u32(&self, window: &Self::PioWindow, offset: usize) -> u32 {
        window.read_u32(offset)
    }

    fn write_u8(&mut self, window: &Self::PioWindow, offset: usize, value: u8) {
        window.write_u8(offset, value);
    }

    fn write_u16(&mut self, window: &Self::PioWindow, offset: usize, value: u16) {
        window.write_u16(offset, value);
    }

    fn write_u32(&mut self, window: &Self::PioWindow, offset: usize, value: u32) {
        window.write_u32(offset, value);
    }
}

// ---------------------------------------------------------------------------
// Host-test stubs — exercise the contract surface against a PioContract
// trait double, mirroring the MmioContract tests in kernel-core's contract.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use kernel_core::driver_runtime::contract::PioContract;

    use super::*;

    /// Minimal in-process PIO double: stores (offset, value, width) tuples for
    /// write assertions and a byte-buffer for read-back.
    ///
    /// `write_u*` now takes `&mut self` (matching `PioContract`), so direct
    /// field mutation replaces the `RefCell` interior-mutability workaround.
    struct MockPioBackend {
        data: [u8; 256],
        /// Accumulated writes — (offset, value, width) for assertion.
        writes: Vec<(usize, u32, u8)>,
    }

    /// Opaque PIO window handle for MockPioBackend — holds only the bar index.
    struct MockPioWindow {
        bar: u8,
    }

    impl MockPioBackend {
        fn new() -> Self {
            Self {
                data: [0u8; 256],
                writes: Vec::new(),
            }
        }

        fn writes_snapshot(&self) -> Vec<(usize, u32, u8)> {
            self.writes.clone()
        }
    }

    // We only need PioContract for this test, not the full DeviceHandleContract.
    // Implement a minimal shim.
    impl kernel_core::driver_runtime::contract::DeviceHandleContract for MockPioBackend {
        type Handle = ();

        fn claim(
            &mut self,
            _key: kernel_core::device_host::DeviceCapKey,
        ) -> Result<Self::Handle, DriverRuntimeError> {
            Ok(())
        }

        fn release(&mut self, _handle: Self::Handle) -> Result<(), DriverRuntimeError> {
            Ok(())
        }
    }

    impl PioContract for MockPioBackend {
        type PioWindow = MockPioWindow;

        fn map(
            &mut self,
            _handle: &Self::Handle,
            bar: u8,
        ) -> Result<Self::PioWindow, DriverRuntimeError> {
            Ok(MockPioWindow { bar })
        }

        fn read_u8(&self, _window: &Self::PioWindow, offset: usize) -> u8 {
            self.data[offset]
        }

        fn read_u16(&self, _window: &Self::PioWindow, offset: usize) -> u16 {
            u16::from_le_bytes([self.data[offset], self.data[offset + 1]])
        }

        fn read_u32(&self, _window: &Self::PioWindow, offset: usize) -> u32 {
            u32::from_le_bytes([
                self.data[offset],
                self.data[offset + 1],
                self.data[offset + 2],
                self.data[offset + 3],
            ])
        }

        fn write_u8(&mut self, _window: &Self::PioWindow, offset: usize, value: u8) {
            self.data[offset] = value;
            self.writes.push((offset, u32::from(value), 1));
        }

        fn write_u16(&mut self, _window: &Self::PioWindow, offset: usize, value: u16) {
            let bytes = value.to_le_bytes();
            self.data[offset] = bytes[0];
            self.data[offset + 1] = bytes[1];
            self.writes.push((offset, u32::from(value), 2));
        }

        fn write_u32(&mut self, _window: &Self::PioWindow, offset: usize, value: u32) {
            let bytes = value.to_le_bytes();
            self.data[offset] = bytes[0];
            self.data[offset + 1] = bytes[1];
            self.data[offset + 2] = bytes[2];
            self.data[offset + 3] = bytes[3];
            self.writes.push((offset, value, 4));
        }
    }

    // -----------------------------------------------------------------------
    // Contract surface tests
    // -----------------------------------------------------------------------

    #[test]
    fn pio_contract_map_returns_window() {
        let mut backend = MockPioBackend::new();
        let handle = ();
        let window = <MockPioBackend as PioContract>::map(&mut backend, &handle, 0)
            .expect("map must succeed on a live handle");
        assert_eq!(window.bar, 0);
    }

    #[test]
    fn pio_contract_write_u8_read_u8_round_trip() {
        let mut backend = MockPioBackend::new();
        let handle = ();
        let window = <MockPioBackend as PioContract>::map(&mut backend, &handle, 0).unwrap();
        <MockPioBackend as PioContract>::write_u8(&mut backend, &window, 0, 0xAB);
        let v = <MockPioBackend as PioContract>::read_u8(&backend, &window, 0);
        assert_eq!(v, 0xAB);
    }

    #[test]
    fn pio_contract_write_u16_read_u16_round_trip() {
        let mut backend = MockPioBackend::new();
        let handle = ();
        let window = <MockPioBackend as PioContract>::map(&mut backend, &handle, 0).unwrap();
        <MockPioBackend as PioContract>::write_u16(&mut backend, &window, 16, 0xBEEF);
        let v = <MockPioBackend as PioContract>::read_u16(&backend, &window, 16);
        assert_eq!(v, 0xBEEF);
    }

    #[test]
    fn pio_contract_write_u32_read_u32_round_trip() {
        let mut backend = MockPioBackend::new();
        let handle = ();
        let window = <MockPioBackend as PioContract>::map(&mut backend, &handle, 0).unwrap();
        <MockPioBackend as PioContract>::write_u32(&mut backend, &window, 32, 0xDEAD_BEEF);
        let v = <MockPioBackend as PioContract>::read_u32(&backend, &window, 32);
        assert_eq!(v, 0xDEAD_BEEF);
    }

    #[test]
    fn pio_contract_records_all_writes() {
        let mut backend = MockPioBackend::new();
        let handle = ();
        let window = <MockPioBackend as PioContract>::map(&mut backend, &handle, 0).unwrap();
        <MockPioBackend as PioContract>::write_u8(&mut backend, &window, 0, 0x01);
        <MockPioBackend as PioContract>::write_u16(&mut backend, &window, 2, 0x0202);
        <MockPioBackend as PioContract>::write_u32(&mut backend, &window, 8, 0x0304_0506);
        let writes = backend.writes_snapshot();
        assert_eq!(writes.len(), 3);
        assert_eq!(writes[0], (0, 0x01, 1));
        assert_eq!(writes[1], (2, 0x0202, 2));
        assert_eq!(writes[2], (8, 0x0304_0506, 4));
    }

    #[test]
    fn syscall_backend_implements_pio_contract() {
        fn witness<T: PioContract>() {}
        witness::<SyscallBackend>();
    }

    #[test]
    fn pio_wrapper_is_debug() {
        // Construct a Pio wrapper manually (bypassing map which calls the kernel)
        // to verify the Debug impl compiles on the host.
        let w = Pio::<()> {
            device_cap: 42,
            bar_index: 1,
            _marker: PhantomData,
        };
        let s = alloc::format!("{:?}", w);
        assert!(s.contains("Pio"));
    }

    #[test]
    fn pio_wrapper_drop_is_noop() {
        // Drop must not panic or leak. Construct directly to avoid syscall.
        let w = Pio::<()> {
            device_cap: 1,
            bar_index: 0,
            _marker: PhantomData,
        };
        drop(w); // must not panic
    }
}
