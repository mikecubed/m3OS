//! PCI class-enumeration wrapper — Phase 78b Track C-finish.
//!
//! Exposes a safe ring-3 API for discovering PCI devices by class/subclass/prog_if
//! via the [`SYS_DEVICE_PCI_ENUMERATE`] syscall introduced in Phase 78b Track C.1.
//!
//! # ABI summary
//!
//! `sys_device_pci_enumerate(class, subclass, prog_if, out_user_ptr, max_entries) -> isize`
//!
//! | Register | Value |
//! |----------|-------|
//! | `rax` | `SYS_DEVICE_PCI_ENUMERATE` (0x1127) |
//! | `rdi` | `class: u8` |
//! | `rsi` | `subclass: u8` |
//! | `rdx` | `prog_if: u8` |
//! | `r10` | `out_user_ptr: usize` — pointer to caller-owned `u32` buffer |
//! | `r8`  | `max_entries: usize` — buffer capacity in `u32` entries |
//!
//! Returns the **total** match count (≥0) or a negative errno. Each `u32` entry
//! packs a BDF as:
//!
//! ```text
//! bits [31:20] — segment (always 0)
//! bits [19:12] — bus
//! bits [ 9: 5] — device
//! bits [ 4: 2] — function
//! bits [11:10], [1:0] — reserved (0)
//! ```
//!
//! # Authorization
//!
//! Only processes whose executable path is under `/drivers/` pass the kernel's
//! `is_authorized_driver_process` check. `xhci_driver` is staged at
//! `/drivers/xhci` and therefore qualifies.

use alloc::vec::Vec;

use kernel_core::device_host::DeviceCapKey;
use kernel_core::driver_runtime::contract::DriverRuntimeError;

use crate::syscall_backend::decode_errno_common;

#[cfg(all(target_arch = "x86_64", target_os = "none"))]
use kernel_core::device_host::syscalls::SYS_DEVICE_CONFIG_READ;
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
use kernel_core::device_host::syscalls::SYS_DEVICE_PCI_ENUMERATE;

/// Maximum number of PCI controllers the ring-3 buffer can hold in one call.
///
/// 32 entries is far more than any current platform has for a single class;
/// QEMU exposes at most one xHCI controller in the default m3OS machine
/// configuration. Callers can pass a count-query (`max_entries = 0`) first and
/// then a sized buffer for truly large counts, but for USB controllers this
/// constant is more than sufficient.
const PCI_ENUM_BUF_ENTRIES: usize = 32;

/// Raw `sys_device_pci_enumerate` syscall.
///
/// Writes up to `buf.len()` packed BDF `u32` values into `buf` and returns the
/// **total** match count (which may exceed `buf.len()` when the buffer was too
/// small). A negative return is a negated errno.
///
/// # Safety
///
/// `buf` must be a valid, writable slice for the duration of the syscall.  The
/// kernel validates the pointer via its `copy_to_user` path and returns
/// `-EFAULT` on failure.
#[inline]
unsafe fn raw_sys_device_pci_enumerate(
    class: u8,
    subclass: u8,
    prog_if: u8,
    buf: &mut [u32],
) -> isize {
    #[cfg(all(target_arch = "x86_64", target_os = "none"))]
    // SAFETY: `buf.as_mut_ptr()` is valid for `buf.len() * 4` bytes of writes;
    // the kernel validates the user-pointer range before touching it. The other
    // arguments are plain integers. `syscall5` places args in
    // rdi/rsi/rdx/r10/r8 which matches the documented ABI.
    unsafe {
        syscall_lib::syscall5(
            SYS_DEVICE_PCI_ENUMERATE,
            u64::from(class),
            u64::from(subclass),
            u64::from(prog_if),
            buf.as_mut_ptr() as u64,
            buf.len() as u64,
        ) as isize
    }
    #[cfg(not(all(target_arch = "x86_64", target_os = "none")))]
    {
        // Host-test path: no real kernel. Return an empty match count so
        // the public wrapper compiles and the fallback path is exercised.
        let _ = (class, subclass, prog_if, buf);
        0 // 0 matches — triggers the fallback in xhci driver tests
    }
}

/// Unpack a packed BDF `u32` into `(segment, bus, dev, func)`.
///
/// Mirrors [`kernel_core::device_host::pci_enum::PciDeviceInfo::pack_bdf`]'s
/// bit layout in the unpack direction:
///
/// ```text
/// bits [31:20] → segment (12 bits)
/// bits [19:12] → bus     (8 bits)
/// bits [ 9: 5] → device  (5 bits)
/// bits [ 4: 2] → function (3 bits)
/// bits [11:10], [1:0] → reserved (masked to 0 by this function)
/// ```
#[inline]
fn unpack_bdf(packed: u32) -> (u16, u8, u8, u8) {
    let segment = ((packed >> 20) & 0xFFF) as u16;
    let bus = ((packed >> 12) & 0xFF) as u8;
    let dev = ((packed >> 5) & 0x1F) as u8;
    let func = ((packed >> 2) & 0x07) as u8;
    (segment, bus, dev, func)
}

/// Enumerate PCI devices matching a `(class, subclass, prog_if)` triple.
///
/// Invokes `sys_device_pci_enumerate` with a fixed-size stack buffer
/// ([`PCI_ENUM_BUF_ENTRIES`] entries = 32), unpacks each returned BDF `u32`
/// into a [`DeviceCapKey`], and returns them in bus-scan order. A total count
/// that exceeds the buffer capacity is silently clamped — for the USB xHCI
/// use-case (≤ a handful of controllers per platform) the 32-entry buffer
/// will never be the limiting factor.
///
/// # Errors
///
/// - `DriverRuntimeError::Device(DeviceHostError::NotClaimed)` on `-EACCES`
///   (exec-path not under `/drivers/`).
/// - `DriverRuntimeError::Device(DeviceHostError::Internal)` on `-ESRCH`
///   (kernel context — unreachable from a ring-3 driver), `-EFAULT`, or any
///   other unexpected errno.
///
/// # Example
///
/// ```rust,ignore
/// use driver_runtime::pci_enum::enumerate_pci_class;
///
/// let controllers = enumerate_pci_class(0x0C, 0x03, 0x30)?;
/// for key in &controllers {
///     let handle = DeviceHandle::claim(*key)?;
///     // ... bring up controller
/// }
/// ```
pub fn enumerate_pci_class(
    class: u8,
    subclass: u8,
    prog_if: u8,
) -> Result<Vec<DeviceCapKey>, DriverRuntimeError> {
    let mut buf = [0u32; PCI_ENUM_BUF_ENTRIES];
    // SAFETY: `buf` is a valid, stack-allocated, writable slice that outlives
    // the syscall. The kernel validates the user pointer independently.
    let raw = unsafe { raw_sys_device_pci_enumerate(class, subclass, prog_if, &mut buf) };

    if raw < 0 {
        return Err(decode_errno_common(raw as i32));
    }

    // `raw` is the **total** match count; clamp to what we actually received.
    let total = raw as usize;
    let received = total.min(PCI_ENUM_BUF_ENTRIES);

    let mut keys = Vec::with_capacity(received);
    for &packed in buf.iter().take(received) {
        let (segment, bus, dev, func) = unpack_bdf(packed);
        keys.push(DeviceCapKey::new(segment, bus, dev, func));
    }
    Ok(keys)
}

// ---------------------------------------------------------------------------
// Phase 79 Track A.1 — raw-BDF PCI config-space read (pre-claim).
// ---------------------------------------------------------------------------

/// Raw `sys_device_config_read` syscall.
///
/// Reads `width` (1/2/4) bytes at config-space `offset` for the device addressed
/// by `key`, without claiming it. Returns the value (≥0) or a negated errno.
///
/// # Safety
///
/// Pure integer syscall — no pointer lifetimes are involved.
#[inline]
unsafe fn raw_sys_device_config_read(key: DeviceCapKey, offset: u16, width: u8) -> isize {
    let packed = kernel_core::device_host::syscalls::pack_config_read_arg(offset, width);
    #[cfg(all(target_arch = "x86_64", target_os = "none"))]
    // SAFETY: all arguments are plain integers. `syscall5` places them in
    // rdi/rsi/rdx/r10/r8 matching the documented ABI.
    unsafe {
        syscall_lib::syscall5(
            SYS_DEVICE_CONFIG_READ,
            u64::from(key.segment),
            u64::from(key.bus),
            u64::from(key.dev),
            u64::from(key.func),
            packed,
        ) as isize
    }
    #[cfg(not(all(target_arch = "x86_64", target_os = "none")))]
    {
        // Host-test path: no real kernel / PCI bus. Report ENODEV so the
        // public wrapper exercises its error branch deterministically.
        let _ = (key, offset, width, packed);
        -19
    }
}

/// Read `width` (1/2/4) bytes of PCI config space at `offset` for `key`,
/// **without claiming** the device.
///
/// Used by NIC drivers to read the vendor/device ID of each enumerated
/// Ethernet function so exactly one family driver claims a given device.
///
/// # Errors
///
/// - `DriverRuntimeError::Device(DeviceHostError::NotClaimed)` on `-EACCES`
///   (exec-path not under `/drivers/`).
/// - `DriverRuntimeError::Device(DeviceHostError::NotClaimed)` on `-ENODEV`
///   (no function at the BDF).
/// - `DriverRuntimeError::Device(DeviceHostError::Internal)` on any other errno.
pub fn pci_config_read(
    key: DeviceCapKey,
    offset: u16,
    width: u8,
) -> Result<u32, DriverRuntimeError> {
    // SAFETY: see `raw_sys_device_config_read`.
    let raw = unsafe { raw_sys_device_config_read(key, offset, width) };
    if raw < 0 {
        return Err(decode_errno_common(raw as i32));
    }
    Ok(raw as u32)
}

/// Read the `(vendor_id, device_id)` pair of an enumerated PCI function.
///
/// Convenience over [`pci_config_read`] for the offset-0 dword: the low 16 bits
/// are the vendor ID, the high 16 bits the device ID.
pub fn read_vendor_device(key: DeviceCapKey) -> Result<(u16, u16), DriverRuntimeError> {
    let dword = pci_config_read(key, 0x00, 4)?;
    Ok(((dword & 0xFFFF) as u16, ((dword >> 16) & 0xFFFF) as u16))
}

/// An enumerated PCI function plus its identifying vendor/device IDs.
///
/// Phase 79: the shared shape every NIC driver matches against when deciding
/// which Ethernet function to claim. Produced by [`enumerate_ethernet_functions`]
/// and filtered by [`select_nic`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PciFunctionId {
    pub key: DeviceCapKey,
    pub vendor: u16,
    pub device: u16,
}

/// Enumerate every Ethernet controller (class 0x02 / subclass 0x00 / prog_if
/// 0x00) and read each function's vendor:device ID via the pre-claim
/// [`pci_config_read`] path.
///
/// Functions whose config read fails are silently skipped. On the host-test
/// path the enumerate syscall returns no matches, so this returns an empty
/// vector.
pub fn enumerate_ethernet_functions() -> Vec<PciFunctionId> {
    use kernel_core::nic_ids;
    let mut out: Vec<PciFunctionId> = Vec::new();
    let keys = match enumerate_pci_class(
        nic_ids::ETHERNET_CLASS,
        nic_ids::ETHERNET_SUBCLASS,
        nic_ids::ETHERNET_PROG_IF,
    ) {
        Ok(keys) => keys,
        Err(_) => return out,
    };
    for key in keys {
        if let Ok((vendor, device)) = read_vendor_device(key) {
            out.push(PciFunctionId {
                key,
                vendor,
                device,
            });
        }
    }
    out
}

/// Return the BDF of the first enumerated function matching `vendor` and the
/// per-family `is_family` device-ID predicate (one of the
/// `kernel_core::nic_ids::is_*` functions), or `None`.
pub fn select_nic(
    functions: &[PciFunctionId],
    vendor: u16,
    is_family: fn(u16) -> bool,
) -> Option<DeviceCapKey> {
    functions
        .iter()
        .find(|f| f.vendor == vendor && is_family(f.device))
        .map(|f| f.key)
}

// ---------------------------------------------------------------------------
// Host tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // The host stub returns 0 (no matches), so enumerate_pci_class returns Ok([]).
    #[test]
    fn enumerate_pci_class_returns_empty_on_host() {
        let result = enumerate_pci_class(0x0C, 0x03, 0x30);
        assert!(result.is_ok(), "host stub must not error");
        assert!(result.unwrap().is_empty(), "host stub returns 0 matches");
    }

    // --- unpack_bdf round-trip tests ---

    #[test]
    fn unpack_bdf_round_trips_zero_bdf() {
        // BDF 0000:00:00.0 → packed = 0
        let (seg, bus, dev, func) = unpack_bdf(0);
        assert_eq!((seg, bus, dev, func), (0, 0, 0, 0));
    }

    #[test]
    fn unpack_bdf_round_trips_xhci_sentinel() {
        // BDF 0000:00:06.0 (the QEMU xHCI sentinel)
        // pack: segment=0, bus=0, dev=6, func=0
        // bits: [31:20]=0, [19:12]=0, [9:5]=6, [4:2]=0
        let packed: u32 = (6u32 << 5); // dev=6 in bits [9:5]
        let (seg, bus, dev, func) = unpack_bdf(packed);
        assert_eq!(seg, 0);
        assert_eq!(bus, 0);
        assert_eq!(dev, 6);
        assert_eq!(func, 0);
    }

    #[test]
    fn unpack_bdf_round_trips_sata_ahci() {
        // BDF 0000:00:1f.2 — bus=0, dev=0x1f, func=2
        let packed: u32 = ((0x1fu32) << 5) | (2u32 << 2);
        let (seg, bus, dev, func) = unpack_bdf(packed);
        assert_eq!(seg, 0);
        assert_eq!(bus, 0);
        assert_eq!(dev, 0x1f);
        assert_eq!(func, 2);
    }

    #[test]
    fn unpack_bdf_strips_reserved_bits() {
        // Set reserved bits [11:10] and [1:0] — must not bleed into fields.
        // dev=3 at [9:5], plus reserved bits set
        let packed: u32 = (3u32 << 5) | 0b11 | (0b11u32 << 10);
        let (seg, bus, dev, func) = unpack_bdf(packed);
        assert_eq!(seg, 0);
        assert_eq!(bus, 0);
        assert_eq!(dev, 3); // bits [9:5] = 3, unaffected by [11:10]
        assert_eq!(func, 0); // bits [4:2] = 0 (the reserved [1:0] bits are masked out)
    }

    #[test]
    fn unpack_bdf_handles_nonzero_bus() {
        // bus=2 at [19:12]
        let packed: u32 = (2u32 << 12);
        let (seg, bus, dev, func) = unpack_bdf(packed);
        assert_eq!(bus, 2);
        assert_eq!(seg, 0);
        assert_eq!(dev, 0);
        assert_eq!(func, 0);
    }

    #[test]
    fn device_cap_key_fields_survive_unpack() {
        // Simulate two packed entries from the syscall buffer.
        let entries: &[u32] = &[
            (0u32 << 12) | (6u32 << 5),               // 0000:00:06.0
            (1u32 << 12) | (3u32 << 5) | (1u32 << 2), // 0001:01:03.1 (hypothetical)
        ];
        let k0 = {
            let (s, b, d, f) = unpack_bdf(entries[0]);
            DeviceCapKey::new(s, b, d, f)
        };
        let k1 = {
            let (s, b, d, f) = unpack_bdf(entries[1]);
            DeviceCapKey::new(s, b, d, f)
        };
        assert_eq!(k0, DeviceCapKey::new(0, 0, 6, 0));
        assert_eq!(k1, DeviceCapKey::new(0, 1, 3, 1));
    }
}
