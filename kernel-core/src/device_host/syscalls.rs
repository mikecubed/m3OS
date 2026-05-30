// Device-host syscall numbers — Phase 55b Track B.
//
// Single source of truth for the five device-host syscall numbers reserved in
// the `0x11xx` block. Declared in `kernel-core` so the kernel-side dispatcher
// (Track B) and the userspace `driver_runtime` wrappers (Track C) compile
// against the same constants — per the Phase 55b DRY discipline, no other
// file in the workspace is permitted to redeclare these numbers.
//
// Numbering: the `0x11xx` block is carved out of the custom m3OS syscall
// range (0x1000–0x1FFF); `0x1100–0x110F` are consumed by the IPC subsystem,
// so the device-host family starts at `0x1120` to leave room for future IPC
// additions without renumbering. Track B.1 uses `SYS_DEVICE_CLAIM`; Tracks
// B.2–B.4 reserve their numbers here now so they land without re-editing
// this block.

/// Reserve the IOMMU- and capability-gated claim on a PCI(e) BDF.
/// Track B.1 — `sys_device_claim(segment, bus, dev, func) -> isize`.
pub const SYS_DEVICE_CLAIM: u64 = 0x1120;

/// Map a claimed device's BAR window into the caller's address space.
/// Track B.2 — `sys_device_mmio_map(dev_cap, bar_index) -> isize`.
pub const SYS_DEVICE_MMIO_MAP: u64 = 0x1121;

/// Allocate a DMA-mapped buffer against a claimed device's IOMMU domain.
/// Track B.3 — `sys_device_dma_alloc(dev_cap, size, align) -> isize`.
pub const SYS_DEVICE_DMA_ALLOC: u64 = 0x1122;

/// Look up the `(user_va, iova, len)` tuple for a `Capability::Dma` handle.
/// Reserved alongside Track B.3 for the userspace wrapper's handle
/// introspection path — declared now so the block numbering stays dense.
pub const SYS_DEVICE_DMA_HANDLE_INFO: u64 = 0x1123;

/// Subscribe to a device-originated IRQ and receive it as a notification.
/// Track B.4b — `sys_device_irq_subscribe(dev_cap, bit_index, notification_arg) -> isize`.
///
/// `bit_index` (arg2, 0..=63) selects the bit within the 64-bit notification
/// word the ISR will set on delivery. `notification_arg` (arg3) is either the
/// sentinel [`NOTIFICATION_SENTINEL_NEW`] — in which case the kernel allocates
/// a fresh `Notification` owned by the caller — or a `CapHandle` to an
/// existing `Capability::Notification` the caller already holds. The ABI
/// shape (three `u32` args) is enforced by the arch dispatcher in
/// `kernel/src/arch/x86_64/syscall/mod.rs` and by
/// `syscall_numbers_are_pinned_in_the_device_host_block()` below.
pub const SYS_DEVICE_IRQ_SUBSCRIBE: u64 = 0x1124;

/// Read a value from an I/O-port BAR of a claimed device.
/// Phase 63 Track Z.1 — `sys_device_pio_read(dev_cap, bar_index, offset, width) -> isize`.
///
/// Returns the port value zero-extended into the low bits on success, or a
/// negative errno. `width` must be 1, 2, or 4; any other value returns
/// `-EINVAL`. The BAR must be a PIO BAR; MMIO BARs return `-EINVAL`.
/// `offset + width` must fit within the BAR size; an out-of-range access
/// returns `-ERANGE`.
pub const SYS_DEVICE_PIO_READ: u64 = 0x1125;

/// Write a value to an I/O-port BAR of a claimed device.
/// Phase 63 Track Z.1 — `sys_device_pio_write(dev_cap, bar_index, offset, value, width) -> isize`.
///
/// Returns 0 on success, or a negative errno. `width` must be 1, 2, or 4;
/// any other value returns `-EINVAL`. The BAR must be a PIO BAR; MMIO BARs
/// return `-EINVAL`. `offset + width` must fit within the BAR size; an
/// out-of-range access returns `-ERANGE`.
pub const SYS_DEVICE_PIO_WRITE: u64 = 0x1126;

/// Enumerate PCI devices matching a class/subclass/prog_if triple.
/// Phase 78b Track C.1 —
/// `sys_device_pci_enumerate(class, subclass, prog_if, out_user_ptr, max_entries) -> isize`.
///
/// ## ABI
///
/// | Register | Value |
/// |----------|-------|
/// | `rax`    | `SYS_DEVICE_PCI_ENUMERATE` (0x1127) |
/// | `rdi`    | `class: u8`  — PCI class code (e.g. `0x0C` for Serial Bus) |
/// | `rsi`    | `subclass: u8` — PCI subclass (e.g. `0x03` for USB) |
/// | `rdx`    | `prog_if: u8` — Programming Interface (e.g. `0x30` for xHCI) |
/// | `r10`    | `out_user_ptr: usize` — pointer to caller's output buffer |
/// | `r8`     | `max_entries: usize` — capacity of the output buffer in entries |
///
/// On success the kernel writes up to `max_entries` packed BDF entries to
/// `out_user_ptr` and returns the **total** match count (which may exceed
/// `max_entries` if the buffer was too small). The caller must size the
/// buffer conservatively (e.g. 8 entries for USB controllers) or call once
/// with `max_entries=0` to query the count then call again with a real
/// buffer. A negative return value is a negated `errno`:
///
/// - `-EACCES` (`-13`): caller's exec-path is not under `/drivers/` — only
///   authorized driver processes may enumerate PCI devices.
/// - `-EFAULT` (`-14`): `out_user_ptr` is invalid or the copy-out failed.
/// - `-EINVAL` (`-22`): `class`, `subclass`, or `prog_if` is out of `u8` range
///   (rejected at the dispatcher before reaching this function).
/// - `-ESRCH` (`-3`): kernel context (PID 0) — use the in-kernel PCI scan
///   directly.
///
/// ## BDF packing format
///
/// Each entry is a `u32` in little-endian byte order:
///
/// ```text
/// bits [31:20] — PCI segment (always 0 on current platforms)
/// bits [19:12] — bus number
/// bits [11: 5] — device number
/// bits [ 4: 2] — function number
/// bits [  1:0] — reserved (always 0)
/// ```
///
/// A future phase may promote segment to `u16` and extend the entry to `u64`
/// when multi-segment PCIe is implemented; a new syscall number will be
/// allocated at that time rather than silently changing this one's layout.
pub const SYS_DEVICE_PCI_ENUMERATE: u64 = 0x1127;

/// Sentinel passed as `notification_arg` (arg3) of [`SYS_DEVICE_IRQ_SUBSCRIBE`]
/// to request that the kernel allocate a fresh `Notification` object on the
/// caller's behalf, rather than binding the IRQ to an existing notification
/// the caller already holds. Any other value in that slot is interpreted as a
/// `CapHandle` into the caller's capability table. Single source of truth for
/// both the kernel syscall handler and ring-3 driver_runtime backends.
pub const NOTIFICATION_SENTINEL_NEW: u32 = u32::MAX;

/// Lowest syscall number in the reserved device-host block.
///
/// Track B dispatch arms match `DEVICE_HOST_BASE..=DEVICE_HOST_LAST` so new
/// numbers can be appended here and dispatched through a single match arm
/// per Phase 55b task list C.2.
pub const DEVICE_HOST_BASE: u64 = SYS_DEVICE_CLAIM;

/// Highest syscall number reserved in the device-host block.
///
/// Adjust upward when adding new device-host syscalls; the Track B acceptance
/// items pin this constant as the authoritative upper bound.
pub const DEVICE_HOST_LAST: u64 = SYS_DEVICE_PCI_ENUMERATE;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn syscall_numbers_are_pinned_in_the_device_host_block() {
        // Pin the exact numeric values so a typo during a future phase turns
        // into a test failure rather than a silent renumbering.
        assert_eq!(SYS_DEVICE_CLAIM, 0x1120);
        assert_eq!(SYS_DEVICE_MMIO_MAP, 0x1121);
        assert_eq!(SYS_DEVICE_DMA_ALLOC, 0x1122);
        assert_eq!(SYS_DEVICE_DMA_HANDLE_INFO, 0x1123);
        assert_eq!(SYS_DEVICE_IRQ_SUBSCRIBE, 0x1124);
        // Phase 63 Track Z.1 — PIO syscalls appended after IRQ_SUBSCRIBE.
        assert_eq!(SYS_DEVICE_PIO_READ, 0x1125);
        assert_eq!(SYS_DEVICE_PIO_WRITE, 0x1126);
        // Phase 78b Track C.1 — PCI class enumeration.
        assert_eq!(SYS_DEVICE_PCI_ENUMERATE, 0x1127);
    }

    #[test]
    fn syscall_numbers_are_distinct() {
        let all = [
            SYS_DEVICE_CLAIM,
            SYS_DEVICE_MMIO_MAP,
            SYS_DEVICE_DMA_ALLOC,
            SYS_DEVICE_DMA_HANDLE_INFO,
            SYS_DEVICE_IRQ_SUBSCRIBE,
            SYS_DEVICE_PIO_READ,
            SYS_DEVICE_PIO_WRITE,
            SYS_DEVICE_PCI_ENUMERATE,
        ];
        for (i, a) in all.iter().enumerate() {
            for (j, b) in all.iter().enumerate() {
                if i != j {
                    assert_ne!(a, b, "syscall numbers {i} and {j} collide");
                }
            }
        }
    }

    #[test]
    fn every_device_host_syscall_falls_inside_the_reserved_block() {
        let all = [
            SYS_DEVICE_CLAIM,
            SYS_DEVICE_MMIO_MAP,
            SYS_DEVICE_DMA_ALLOC,
            SYS_DEVICE_DMA_HANDLE_INFO,
            SYS_DEVICE_IRQ_SUBSCRIBE,
            SYS_DEVICE_PIO_READ,
            SYS_DEVICE_PIO_WRITE,
            SYS_DEVICE_PCI_ENUMERATE,
        ];
        for n in all {
            assert!(
                (DEVICE_HOST_BASE..=DEVICE_HOST_LAST).contains(&n),
                "syscall {n:#x} outside device-host block"
            );
        }
    }

    #[test]
    fn pio_syscalls_follow_irq_subscribe_without_gap() {
        // Phase 63 Track Z.1 pin: PIO numbers must be contiguous with
        // IRQ_SUBSCRIBE so the block stays dense per the Phase 55b numbering
        // discipline.
        assert_eq!(SYS_DEVICE_PIO_READ, SYS_DEVICE_IRQ_SUBSCRIBE + 1);
        assert_eq!(SYS_DEVICE_PIO_WRITE, SYS_DEVICE_PIO_READ + 1);
        // Phase 78b Track C.1 pin: PCI_ENUMERATE follows PIO_WRITE without gap.
        assert_eq!(SYS_DEVICE_PCI_ENUMERATE, SYS_DEVICE_PIO_WRITE + 1);
        assert_eq!(DEVICE_HOST_LAST, SYS_DEVICE_PCI_ENUMERATE);
    }

    #[test]
    fn device_host_block_does_not_collide_with_ipc_block() {
        // IPC block is 0x1100..=0x1110 (see arch/x86_64/syscall/mod.rs
        // `IPC_BASE` / `IPC_LAST`). The device-host block must sit above it.
        const IPC_LAST_RESERVED: u64 = 0x1110;
        assert!(DEVICE_HOST_BASE > IPC_LAST_RESERVED);
    }
}
