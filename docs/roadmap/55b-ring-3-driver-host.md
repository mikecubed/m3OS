# Phase 55b - Ring-3 Driver Host

**Status:** Planned
**Source Ref:** phase-55b
**Depends on:** Phase 55 (Hardware Substrate) ✅, Phase 55a (IOMMU Substrate), Phase 54 (Deep Serverization) ✅, Phase 50 (IPC Completion) ✅, Phase 46 (System Services) ✅
**Builds on:** Extracts the NVMe and e1000 drivers Phase 55 placed in ring 0 as a bounded compromise into supervised ring-3 processes, using the Phase 55a IOMMU-isolated DMA path and the Phase 54 / Phase 50 serverization contracts (`vfs_server`, `net_server`) as the reference pattern.
**Primary Components:** userspace/drivers/nvme (new), userspace/drivers/e1000 (new), userspace/lib/driver_runtime (new), kernel/src/ipc, kernel/src/pci, kernel/src/mm/dma, kernel/src/iommu, kernel/src/arch/x86_64/interrupts, kernel/src/blk, kernel/src/net

## Milestone Goal

NVMe and Intel 82540EM e1000 run as supervised ring-3 driver processes. The kernel retains interrupt routing, IOMMU-gated DMA capability issuance, MMIO-region capability issuance, and the PCI device registry; it no longer contains device-specific MMIO programming, descriptor ring management, or DMA buffer lifetime logic for these two devices. A driver-process crash is restartable by the Phase 46 / Phase 51 service manager without kernel corruption or data-path loss beyond the one in-flight request that caused the crash.

## Why This Phase Exists

Phase 55 documented the ring-0 placement of NVMe and e1000 as a deliberate, bounded compromise — bring-up simplicity on a fresh HAL, with the extraction explicitly deferred. Phase 55a closed the IOMMU prerequisite: ring-3 drivers now have a safe DMA story. With both gates cleared, the project can execute the extraction before Phase 56 adds more drivers (display, input) on top of the ring-0 pattern.

Doing this now matters for three reasons:

1. Every driver Phase 56 adds while the ring-0 pattern is in place compounds the extraction debt. Extracting two drivers now is cheaper than extracting five later.
2. The Phase 55 HAL (`claim_pci_device`, `BarMapping`, `DmaBuffer<T>`, `register_device_irq`, `DriverEntry`) was deliberately shaped to be callable from a future userspace driver host. That shape has not yet been tested against a real ring-3 caller — Phase 55b is the validation.
3. Phase 58's 1.0 Gate is a support-promise phase. Shipping 1.0 with "microkernel with supervised ring-3 drivers, IOMMU-isolated, restartable" is a materially stronger claim than "microkernel-aimed; drivers are in ring 0 on 1.0 for pragmatic reasons."

## Learning Goals

- Understand how a microkernel delivers device interrupts to ring-3 drivers without giving ring 3 control over the interrupt controller (notification capabilities, one-shot IRQ forwarding).
- Learn how IOMMU-gated DMA capabilities are issued to driver processes so a driver can program its own device without being able to program any other device's DMA.
- See how the Phase 54 `vfs_server` / `net_server` extraction pattern generalizes to devices whose register semantics live in their own process.
- Understand the restart / reconnection contract between a driver process, the service manager, and the clients that depend on the driver (block layer for NVMe, network stack for e1000).

## Feature Scope

### Driver process shape

Phase 55b ships **one process per driver** (per-driver isolation, clean restart blast-radius) with shared code (HAL client, IPC boilerplate, PCI claim, IRQ notification loop) factored into a `userspace/lib/driver_runtime` crate so the per-driver crates stay small.

### Kernel-exposed driver-host primitives

Four new capability-gated kernel primitives, each reusing Phase 50's capability machinery:

- `sys_device_claim(bus, dev, func)` — transfers a `PciDeviceHandle` to the caller as a `Capability::Device` variant. Replaces the in-kernel `claim_pci_device` call site for ring-3 drivers.
- `sys_device_mmio_map(device_cap, bar_index)` — maps the BAR into the caller's address space as read-write, returning a user-space pointer. Bounds-checked against the BAR's actual size.
- `sys_device_dma_alloc(device_cap, size)` — allocates IOMMU-mapped DMA through Phase 55a's domain for that device, returns a `Capability::Grant`-style handle the caller maps to get both a user virtual address and a bus address (IOVA).
- `sys_device_irq_subscribe(device_cap, vector_hint)` — allocates an MSI / MSI-X vector for the device (or installs a legacy INTx handler) and attaches a `Notification` object the caller signals on. Reuses Phase 55's `DeviceIrq` and the 16-vector stub bank.

### NVMe driver extraction

Migrate `kernel/src/blk/nvme.rs` to `userspace/drivers/nvme/`. The kernel retains only the block-layer facade that forwards `read` / `write` requests to the driver process over IPC. The data-path smoke from Phase 55 (512 B round-trip at LBA 0) runs through the new IPC path.

### e1000 driver extraction

Migrate `kernel/src/net/e1000.rs` to `userspace/drivers/e1000/`. The kernel retains only a `net::send_frame` facade that forwards frames to the driver process; received frames flow from the driver process back into the network stack via IPC. The Phase 54 `net_server` policy layer is unchanged.

### Supervision and restart

Each driver process registers with the Phase 46 / Phase 51 service manager. On driver-process crash, the supervisor restarts the process, which re-runs its Phase 55 HAL init (device claim, MMIO map, DMA alloc, IRQ subscribe). In-flight requests at crash time fail to the caller with a documented error; subsequent requests succeed after restart completes.

## Important Components and How They Work

### `userspace/lib/driver_runtime`

Shared library crate for ring-3 drivers. Wraps the four new kernel primitives in a safe, HAL-shaped API: `DeviceHandle`, `Mmio`, `DmaBuffer<T>`, `IrqNotification`. Per-driver crates (`drivers/nvme`, `drivers/e1000`) consume this library and stay focused on register semantics. The API surface deliberately mirrors the Phase 55 kernel-side HAL so the register-handling code ports with minimal change.

### Kernel block-layer and net-layer facades

`kernel/src/blk/mod.rs` gains a `RemoteBlockDevice` that forwards `read` / `write` to a registered IPC endpoint. `kernel/src/net/mod.rs` gains a `RemoteNic` that forwards `send_frame` and accepts incoming frames via an IPC wake-up. These are the only kernel-side changes the block and network stacks see — all existing clients (`vfs_server` reads, TCP packets) route through them unchanged.

### Capability-gated syscalls

Every new driver-host syscall takes a `Capability::Device` handle as its first argument after the syscall number. The kernel validates the capability on every call (Phase 50 pattern). A driver process that is killed and restarted re-acquires its capabilities through `sys_device_claim`; no state survives the kill.

## How This Builds on Earlier Phases

- Extends the Phase 54 `vfs_server` / `net_server` / `fat_server` ring-3 service pattern to cover device drivers.
- Reuses Phase 50's capability grants and Phase 46 / Phase 51's service supervision without introducing a new supervision model.
- Replaces the Phase 55 in-kernel `claim_pci_device` / `DmaBuffer::allocate` / `register_device_irq` call sites for NVMe and e1000 with ring-3-safe syscall equivalents that route through the same underlying mechanisms.
- Depends on Phase 55a for IOMMU-gated DMA so a ring-3 driver cannot use its DMA capability to corrupt arbitrary physical memory.

## Implementation Outline

1. Create `userspace/lib/driver_runtime` with the four HAL-shaped wrappers and an IRQ-notification loop helper.
2. Add the four kernel primitives (`sys_device_claim`, `sys_device_mmio_map`, `sys_device_dma_alloc`, `sys_device_irq_subscribe`) behind capability checks.
3. Port `kernel/src/blk/nvme.rs` to `userspace/drivers/nvme/`, wire the kernel block layer's `RemoteBlockDevice` facade, and verify the Phase 55 data-path smoke runs end-to-end.
4. Port `kernel/src/net/e1000.rs` to `userspace/drivers/e1000/`, wire the kernel network layer's `RemoteNic` facade, and verify ICMP ping through the extracted driver.
5. Register both driver processes with the Phase 46 / Phase 51 supervisor; add a crash-and-restart regression test that kills the NVMe driver mid-workload and verifies restart plus subsequent I/O success.
6. Delete the in-kernel NVMe and e1000 source files; audit `kernel/src/` for any remaining device-specific code that belongs in ring 3.

## Acceptance Criteria

- `kernel/src/blk/nvme.rs` and `kernel/src/net/e1000.rs` are deleted; their logic lives in `userspace/drivers/nvme/` and `userspace/drivers/e1000/`.
- `cargo xtask run --device nvme` and `cargo xtask run --device e1000` still pass their Phase 55 data-path and link-state smoke checks, now routed through ring-3 drivers.
- A driver-process crash during active I/O is restarted by the supervisor; subsequent I/O succeeds within a documented bound.
- A driver process cannot access a BAR or DMA region belonging to a device it did not claim, verified by a negative test attempting cross-device MMIO access.
- Kernel line count (excluding comments) drops by at least the combined NVMe + e1000 driver size measured at Phase 55 close (approximately 2000 lines).
- Kernel version is bumped to `v0.55.2` across `kernel/Cargo.toml`, `AGENTS.md`, `README.md`, and both roadmap READMEs.

## Companion Task List

- Phase 55b task list — defer until implementation planning begins.

## How Real OS Implementations Differ

- seL4 and Redox both run drivers in userspace from the start; m3OS's ring-0 bring-up through Phase 55 is a deliberate staged approach, not a permanent architectural difference.
- Linux keeps drivers in ring 0 and relies on IOMMU groups plus kernel-level review for protection; m3OS is taking the opposite trade-off post-Phase-55b.
- Mature microkernels provide richer IPC for drivers (batched doorbell notifications, shared-memory rings with explicit hand-off semantics); Phase 55b ships the minimal capability set and leaves batching as a later optimization.
- Production driver hosts typically include a driver sandbox that restricts which syscalls each driver can invoke beyond its device capabilities; Phase 55b treats the driver process as unprivileged-by-default and revisits sandbox policy in a later hardening phase.

## Deferred Until Later

- Driver-side seccomp / syscall sandbox beyond the default "only device-host syscalls allowed" posture.
- Hot-plug / surprise-removal handling for PCIe devices.
- Extracting VirtIO-blk and VirtIO-net on the same pattern (Phase 55b covers only NVMe and e1000; VirtIO extraction is the obvious follow-up but out of scope for this phase).
- Driver live-update / zero-downtime restart; Phase 55b ships cold-restart only.
- Multi-queue NVMe beyond the single I/O queue pair Phase 55 already ships.
