//! Real-syscall backend for the `driver_runtime` contract traits —
//! Phase 55b Track C.2.
//!
//! [`SyscallBackend`] is the ring-3 production backend that implements
//! [`DeviceHandleContract`](kernel_core::driver_runtime::contract::DeviceHandleContract),
//! [`MmioContract`](kernel_core::driver_runtime::contract::MmioContract),
//! and
//! [`DmaBufferContract`](kernel_core::driver_runtime::contract::DmaBufferContract)
//! (the `IrqNotificationContract` impl lands in Track C.3) by forwarding
//! directly to the four device-host syscalls reserved by Phase 55b
//! Track B. The contract-trait impls live in the module that owns each
//! wrapper (`device.rs`, `mmio.rs`, `dma.rs`) to keep each file focused.
//!
//! # Raw-syscall helpers
//!
//! The four `raw_*` functions below translate the contract's typed
//! parameters into the register-level ABI the kernel expects and
//! return the raw `isize`. Error decoding happens in
//! [`decode_cap_handle_result`] / [`decode_user_va_result`] /
//! [`decode_info_errno`] so each concrete call site can lift the
//! negative-errno surface into the contract-required
//! [`DriverRuntimeError`] variant.

use core::mem::size_of;

#[cfg(all(target_arch = "x86_64", target_os = "none"))]
use kernel_core::device_host::syscalls::{
    SYS_DEVICE_CLAIM, SYS_DEVICE_DMA_ALLOC, SYS_DEVICE_DMA_HANDLE_INFO, SYS_DEVICE_MMIO_MAP,
};
use kernel_core::device_host::{DeviceCapKey, DeviceHostError, DmaHandle};
use kernel_core::driver_runtime::contract::DriverRuntimeError;
use kernel_core::ipc::CapHandle;

/// Real-syscall backend. Zero-sized — construction is free.
///
/// The backend is cheap to clone because it has no state; every method
/// delegates directly to a syscall. A driver typically constructs one
/// per process and passes it by `&mut` reference where the contract
/// traits require mutable access (claim, map, allocate).
#[derive(Default, Clone, Copy, Debug)]
pub struct SyscallBackend;

impl SyscallBackend {
    /// Construct a fresh [`SyscallBackend`]. Identical to
    /// `SyscallBackend::default()`; provided so drivers can write
    /// `SyscallBackend::new()` for readability.
    #[inline]
    pub const fn new() -> Self {
        Self
    }
}

// ---------------------------------------------------------------------------
// Raw syscall helpers
// ---------------------------------------------------------------------------

/// `sys_device_claim(segment, bus, dev, func) -> isize`.
///
/// # Safety
///
/// Caller must pass a syntactically valid [`DeviceCapKey`]. The kernel
/// validates semantic ownership and returns a negative errno if the
/// BDF is already claimed or absent.
#[inline]
pub(crate) unsafe fn raw_sys_device_claim(key: DeviceCapKey) -> isize {
    #[cfg(all(target_arch = "x86_64", target_os = "none"))]
    // SAFETY: syscall-lib's syscall4 is declared unsafe and documented
    // to be safe as long as the syscall number + argument semantics are
    // respected. SYS_DEVICE_CLAIM's arguments are pure integers.
    unsafe {
        syscall_lib::syscall4(
            SYS_DEVICE_CLAIM,
            u64::from(key.segment),
            u64::from(key.bus),
            u64::from(key.dev),
            u64::from(key.func),
        ) as isize
    }
    #[cfg(not(all(target_arch = "x86_64", target_os = "none")))]
    {
        // Host-test path: no real kernel. The wrappers' public surface
        // is exercised against synthesized handles in unit tests; this
        // stub exists so the crate compiles for
        // `x86_64-unknown-linux-gnu` under `cargo test`.
        let _ = key;
        -38 // -ENOSYS
    }
}

/// `sys_device_mmio_map(dev_cap, bar_index) -> isize`.
///
/// # Safety
///
/// Caller must pass a valid `Capability::Device` handle. The kernel
/// validates capability shape and returns a negative errno otherwise.
#[inline]
pub(crate) unsafe fn raw_sys_device_mmio_map(dev_cap: CapHandle, bar_index: u8) -> isize {
    #[cfg(all(target_arch = "x86_64", target_os = "none"))]
    // SAFETY: plain-integer args; the kernel validates capability
    // shape and returns `-EBADF` otherwise.
    unsafe {
        syscall_lib::syscall2(
            SYS_DEVICE_MMIO_MAP,
            u64::from(dev_cap),
            u64::from(bar_index),
        ) as isize
    }
    #[cfg(not(all(target_arch = "x86_64", target_os = "none")))]
    {
        let _ = (dev_cap, bar_index);
        -38
    }
}

/// `sys_device_dma_alloc(dev_cap, size, align) -> isize`.
///
/// # Safety
///
/// Caller must pass a valid `Capability::Device` handle.
#[inline]
pub(crate) unsafe fn raw_sys_device_dma_alloc(
    dev_cap: CapHandle,
    size: usize,
    align: usize,
) -> isize {
    #[cfg(all(target_arch = "x86_64", target_os = "none"))]
    // SAFETY: plain-integer args; size / align are validated
    // kernel-side.
    unsafe {
        syscall_lib::syscall3(
            SYS_DEVICE_DMA_ALLOC,
            u64::from(dev_cap),
            size as u64,
            align as u64,
        ) as isize
    }
    #[cfg(not(all(target_arch = "x86_64", target_os = "none")))]
    {
        let _ = (dev_cap, size, align);
        -38
    }
}

/// `sys_device_dma_handle_info(dma_cap, out_user_ptr) -> isize`.
///
/// Reads the `(user_va, iova, len)` tuple for `dma_cap` into
/// `out_user_ptr` (must point to at least 24 bytes of writable
/// memory).
///
/// # Safety
///
/// Caller must pass a valid `Capability::Dma` handle and a pointer
/// satisfying the kernel's `copy_to_user` contract: non-null, properly
/// aligned, writable for the full struct size.
#[inline]
pub(crate) unsafe fn raw_sys_device_dma_handle_info(
    dma_cap: CapHandle,
    out_user_ptr: *mut u8,
) -> isize {
    #[cfg(all(target_arch = "x86_64", target_os = "none"))]
    // SAFETY: the wrapper passes a stack-allocated 24-byte buffer whose
    // lifetime exceeds this call.
    unsafe {
        syscall_lib::syscall2(
            SYS_DEVICE_DMA_HANDLE_INFO,
            u64::from(dma_cap),
            out_user_ptr as u64,
        ) as isize
    }
    #[cfg(not(all(target_arch = "x86_64", target_os = "none")))]
    {
        let _ = (dma_cap, out_user_ptr);
        -38
    }
}

/// Serialized size of the `DmaHandle` wire format.
const DMA_HANDLE_WIRE_LEN: usize = 24;

#[inline]
fn dma_handle_from_wire(bytes: &[u8; DMA_HANDLE_WIRE_LEN]) -> DmaHandle {
    let user_va = u64::from_le_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ]);
    let iova = u64::from_le_bytes([
        bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15],
    ]);
    let len = u64::from_le_bytes([
        bytes[16], bytes[17], bytes[18], bytes[19], bytes[20], bytes[21], bytes[22], bytes[23],
    ]);
    DmaHandle {
        user_va: user_va as usize,
        iova,
        len: len as usize,
    }
}

/// Call `sys_device_dma_handle_info` and return the decoded
/// [`DmaHandle`].
///
/// # Safety
///
/// Caller must pass a valid `Capability::Dma` handle.
#[inline]
pub(crate) unsafe fn fetch_dma_handle(dma_cap: CapHandle) -> Result<DmaHandle, DriverRuntimeError> {
    let mut wire = [0u8; DMA_HANDLE_WIRE_LEN];
    // SAFETY: `wire.as_mut_ptr()` is valid for 24 bytes of writes.
    let rc = unsafe { raw_sys_device_dma_handle_info(dma_cap, wire.as_mut_ptr()) };
    if rc < 0 {
        return Err(decode_info_errno(rc as i32));
    }
    Ok(dma_handle_from_wire(&wire))
}

// ---------------------------------------------------------------------------
// Error decoding
// ---------------------------------------------------------------------------

/// Decode a capability-handle syscall return (`sys_device_claim`,
/// `sys_device_dma_alloc`, `sys_device_irq_subscribe`) into a
/// [`CapHandle`] or a [`DriverRuntimeError`].
///
/// Negative returns are treated as signed errnos; non-negative returns
/// are unpacked into `u32` capability handles (capped at `u32::MAX`).
#[inline]
pub(crate) fn decode_cap_handle_result(raw: isize) -> Result<CapHandle, DriverRuntimeError> {
    if raw < 0 {
        return Err(decode_errno_common(raw as i32));
    }
    let as_u64 = raw as u64;
    if as_u64 > u64::from(u32::MAX) {
        return Err(DriverRuntimeError::Device(DeviceHostError::Internal));
    }
    Ok(as_u64 as CapHandle)
}

/// Decode a user-VA syscall return (`sys_device_mmio_map`) into a
/// `usize` or a [`DriverRuntimeError`]. User VAs on x86_64 fit in
/// `isize` because the kernel caps user VAs below the canonical
/// kernel half.
#[inline]
pub(crate) fn decode_user_va_result(raw: isize) -> Result<usize, DriverRuntimeError> {
    if raw < 0 {
        return Err(decode_errno_common(raw as i32));
    }
    Ok(raw as usize)
}

/// Decode a negative errno into a [`DriverRuntimeError`], covering the
/// values the Track B syscalls return. Shared between claim /
/// mmio_map / dma_alloc — the syscalls return overlapping errno values
/// (-EBADF = `BadDeviceCap`, -EBUSY = `AlreadyClaimed`, etc.) so we
/// collapse them here.
#[inline]
pub(crate) fn decode_errno_common(errno: i32) -> DriverRuntimeError {
    match errno {
        -16 => DriverRuntimeError::Device(DeviceHostError::AlreadyClaimed), // EBUSY
        -19 => DriverRuntimeError::Device(DeviceHostError::NotClaimed),     // ENODEV
        -9 => DriverRuntimeError::Device(DeviceHostError::BadDeviceCap),    // EBADF
        -12 => DriverRuntimeError::Device(DeviceHostError::CapacityExceeded), // ENOMEM
        -22 => DriverRuntimeError::Device(DeviceHostError::InvalidBarIndex), // EINVAL
        -1 => DriverRuntimeError::Device(DeviceHostError::NotClaimed),      // EPERM
        -13 => DriverRuntimeError::Device(DeviceHostError::NotClaimed),     // EACCES
        -3 => DriverRuntimeError::Device(DeviceHostError::Internal),        // ESRCH
        -5 => DriverRuntimeError::Device(DeviceHostError::IommuFault),      // EIO
        -14 => DriverRuntimeError::Device(DeviceHostError::Internal),       // EFAULT
        -23 => DriverRuntimeError::Device(DeviceHostError::CapacityExceeded), // ENFILE
        -38 => DriverRuntimeError::Device(DeviceHostError::Internal),       // ENOSYS
        _ => DriverRuntimeError::Device(DeviceHostError::Internal),
    }
}

/// Decode a negative errno from `sys_device_dma_handle_info`. The
/// kernel returns `-EBADF` for a stale cap; the wrapper surfaces that
/// as `DmaHandleExpired` because — per the contract — that is the
/// surface for a cap held across a revocation.
#[inline]
pub(crate) fn decode_info_errno(errno: i32) -> DriverRuntimeError {
    match errno {
        -9 => DriverRuntimeError::DmaHandleExpired, // EBADF
        -14 => DriverRuntimeError::Device(DeviceHostError::Internal),
        other => decode_errno_common(other),
    }
}

// Const assertion: the wire layout must remain 24 bytes.
const _: () = {
    assert!(DMA_HANDLE_WIRE_LEN == 3 * size_of::<u64>());
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn syscall_backend_is_zero_sized_and_default_constructible() {
        assert_eq!(core::mem::size_of::<SyscallBackend>(), 0);
        let _ = SyscallBackend::new();
        let _ = SyscallBackend;
    }

    #[test]
    fn decode_cap_handle_result_accepts_nonneg() {
        assert_eq!(decode_cap_handle_result(0), Ok(0));
        assert_eq!(decode_cap_handle_result(42), Ok(42));
        assert_eq!(decode_cap_handle_result(u32::MAX as isize), Ok(u32::MAX));
    }

    #[test]
    fn decode_cap_handle_result_clamps_oversized_return() {
        let too_big = (u32::MAX as isize).saturating_add(1);
        assert_eq!(
            decode_cap_handle_result(too_big),
            Err(DriverRuntimeError::Device(DeviceHostError::Internal))
        );
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
            Err(DriverRuntimeError::Device(
                DeviceHostError::CapacityExceeded
            ))
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

    #[test]
    fn decode_user_va_result_maps_invalid_bar_to_invalid_bar_index() {
        assert_eq!(
            decode_user_va_result(-22),
            Err(DriverRuntimeError::Device(DeviceHostError::InvalidBarIndex))
        );
    }

    #[test]
    fn decode_info_errno_maps_stale_cap_to_dma_handle_expired() {
        assert_eq!(decode_info_errno(-9), DriverRuntimeError::DmaHandleExpired);
    }

    #[test]
    fn dma_handle_from_wire_round_trip() {
        let user_va: u64 = 0x0000_7000_0000;
        let iova: u64 = 0x1_0000_0000;
        let len: u64 = 4096;
        let mut wire = [0u8; DMA_HANDLE_WIRE_LEN];
        wire[0..8].copy_from_slice(&user_va.to_le_bytes());
        wire[8..16].copy_from_slice(&iova.to_le_bytes());
        wire[16..24].copy_from_slice(&len.to_le_bytes());
        let h = dma_handle_from_wire(&wire);
        assert_eq!(h.user_va, user_va as usize);
        assert_eq!(h.iova, iova);
        assert_eq!(h.len, len as usize);
    }
}
