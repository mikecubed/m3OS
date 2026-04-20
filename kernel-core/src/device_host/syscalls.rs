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
/// Track B.4 — `sys_device_irq_subscribe(dev_cap, vector_hint, notification_index) -> isize`.
///
/// `notification_index` names the caller-provided notification object that
/// the kernel will signal when the IRQ fires; the notification is *not*
/// allocated implicitly by this call. The ABI shape (three `u32` args) is
/// enforced by the arch dispatcher in `kernel/src/arch/x86_64/syscall/mod.rs`
/// and by `syscall_numbers_are_pinned_in_the_device_host_block()` below.
pub const SYS_DEVICE_IRQ_SUBSCRIBE: u64 = 0x1124;

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
pub const DEVICE_HOST_LAST: u64 = SYS_DEVICE_IRQ_SUBSCRIBE;

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
    }

    #[test]
    fn syscall_numbers_are_distinct() {
        let all = [
            SYS_DEVICE_CLAIM,
            SYS_DEVICE_MMIO_MAP,
            SYS_DEVICE_DMA_ALLOC,
            SYS_DEVICE_DMA_HANDLE_INFO,
            SYS_DEVICE_IRQ_SUBSCRIBE,
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
        ];
        for n in all {
            assert!(
                (DEVICE_HOST_BASE..=DEVICE_HOST_LAST).contains(&n),
                "syscall {n:#x} outside device-host block"
            );
        }
    }

    #[test]
    fn device_host_block_does_not_collide_with_ipc_block() {
        // IPC block is 0x1100..=0x1110 (see arch/x86_64/syscall/mod.rs
        // `IPC_BASE` / `IPC_LAST`). The device-host block must sit above it.
        const IPC_LAST_RESERVED: u64 = 0x1110;
        assert!(DEVICE_HOST_BASE > IPC_LAST_RESERVED);
    }
}
