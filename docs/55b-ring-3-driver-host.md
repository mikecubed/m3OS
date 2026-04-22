# Ring-3 Driver Host

**Aligned Roadmap Phase:** Phase 55b
**Status:** Complete
**Source Ref:** phase-55b
**Supersedes Legacy Doc:** (none — new content)

## Overview

Phase 55b moves m3OS's NVMe and Intel e1000 drivers out of ring 0 and into
supervised ring-3 userspace processes. The kernel gains five new capability-
gated device-host syscalls (`sys_device_claim`, `sys_device_mmio_map`,
`sys_device_dma_alloc`, `sys_device_dma_handle_info`, `sys_device_irq_subscribe`)
that give a ring-3 driver a safe, bounded view of one device and nothing more.
A new `driver_runtime` library provides the Rust types (`DeviceHandle`, `Mmio`,
`DmaBuffer`, `IrqNotification`) that userspace drivers build on. The Phase 45 /
Phase 51 service manager supervises driver processes with restart-on-failure,
and the Phase 55a IOMMU substrate ensures that a crashed or misbehaving driver
cannot DMA into memory it was never allocated.

## What This Doc Covers

- **Device-host capability primitives** — the five syscalls that gate hardware
  access to a single named PCI device, and why each is a separate capability.
- **MMIO bounds-checking** — how the kernel enforces that a ring-3 driver can
  only map the BARs that belong to the claimed device, within the address range
  the kernel validated at claim time.
- **IOMMU-gated DMA** — how `sys_device_dma_alloc` routes every allocation
  through the per-device translation domain established in Phase 55a, so
  bus-master writes from a faulty driver cannot escape the driver's IOMMU
  window. The IOMMU mechanics themselves (VT-d / AMD-Vi table walk, DMAR /
  IVRS parsing) belong to Phase 55a and are covered in
  `docs/55a-iommu-substrate.md`.
- **Notification-forwarded IRQs** — how `sys_device_irq_subscribe` installs a
  ring-0 MSI handler that signals a userspace `IrqNotification` (Phase 6
  `Notification` object), letting the driver poll or block without any ring-0
  callback code outside the kernel.
- **Supervised restart** — how the service manager (Phase 46 / 51) restarts a
  crashed driver, and the in-flight-request contract a ring-3 driver must
  honour so clients tolerate a transient outage cleanly.

The following topics are covered in other docs:
- IOMMU table-walk internals and VT-d / AMD-Vi hardware details: `docs/55a-iommu-substrate.md`.
- Service-manager `.conf` format, restart backoff, and crash classification: `docs/51-service-model-maturity.md`.
- Capability object model and `Notification` semantics: `docs/06-ipc.md` and `docs/50-ipc-completion.md`.

## Core Implementation

### Why drivers belong in ring 3

In a monolithic kernel a driver crash corrupts kernel state and panics the
whole machine. Moving a driver to ring 3 means that a null-deref in the NVMe
completion path is a process fault: the kernel kills the process, the service
manager logs the exit, waits the configured backoff, and restarts it. Storage
requests that were in-flight return an error to callers rather than halting the
system. The reduced trusted computing base (TCB) also matters: every line of
driver code removed from ring 0 is a line that cannot introduce a kernel
privilege-escalation path.

seL4 took ring-3 drivers to the extreme at its founding: all device drivers
live in user-level from the start, and the kernel provides only typed memory
and IPC primitives. Phase 55b is more modest — it extracts only the two most
complex drivers, leaves VirtIO in the kernel for Phase 57, and adds the minimum
kernel machinery needed to let ring-3 code operate hardware safely.

Linux takes the opposite trade-off: drivers run in ring 0 for performance and
simplicity at the cost of crash isolation. Kernel modules share the ring-0
address space; a bug in one module can corrupt all kernel state. DKMS and
BPF-based sandboxing are bolt-on mitigations rather than structural.

### The five device-host primitives

`sys_device_claim(bdf)` is the entry point. The kernel looks up the PCI Bus /
Device / Function address in its enumerated device table, asserts no other
process has claimed it, installs the IOMMU translation domain (Phase 55a), and
returns a `Device` capability — an opaque integer index into the calling
process's capability table that acts as a receipt for the hardware ownership.

`sys_device_mmio_map(device_cap, bar_index)` accepts a `Device` capability and a
BAR index, verifies that `bar_index` is a valid BAR for that device, maps the
BAR's full physical range into the calling process's address space as
device-memory (non-cacheable, write-through), and returns an `Mmio`
capability that records the exact virtual range. Any access outside that
range is a regular page fault; the kernel does not need a special MMIO guard
because the page table enforces the bounds.

`sys_device_dma_alloc(device_cap, size, align)` asks the IOMMU unit to carve
a contiguous physical region, add a host-to-device mapping entry in the
per-device IOMMU domain, and return a `DmaBuffer` capability plus the virtual
and bus addresses the driver uses for descriptor rings and data buffers. A
driver process without a `Device` capability cannot call this syscall; the
kernel rejects the request before touching the IOMMU.

`sys_device_irq_subscribe(device_cap, bit_index, notification_arg)` enables
MSI/MSI-X for the device, installs a ring-0 ISR whose only effect is to set
bit `bit_index` on a 64-bit `Notification` word, and returns a `DeviceIrq`
capability. `bit_index` must be in the range `0..=63`. `notification_arg`
selects the target notification object: passing the sentinel
`NOTIFICATION_SENTINEL_NEW` asks the kernel to allocate a fresh
`Notification` owned by the caller; any other value is treated as a
`CapHandle` to an existing `Capability::Notification` the caller already
holds. The driver calls `sys_notification_wait` on that object to block
until the next interrupt. The ISR is twelve instructions: read MSI data,
find the notification via a lock-free lookup, set the bit, EOI. No
allocation, no IPC, no scheduling from interrupt context.

`sys_device_dma_handle_info(dma_cap)` is a query-only primitive that returns
the bus address and byte length recorded when the buffer was allocated. Drivers
need this to program descriptor-ring entries without storing bus addresses in
userspace-side globals, which would break on restart.

### Safe ring-3 environment: MMIO + IOMMU + notification IRQs together

MMIO bounds-checking confines the driver's register access to the device's own
BARs. IOMMU-gated DMA confines the device's bus-master writes to the driver's
allocated buffers. Notification-forwarded IRQs let the driver receive hardware
events without any ring-0 callback outside the kernel proper. The three
together mean:

1. A driver cannot read or write any address it did not explicitly map.
2. A device cannot DMA into any physical address outside the driver's IOMMU
   window, even if the driver programs a corrupt descriptor.
3. An interrupt does not execute driver code in ring 0.

The remaining attack surface is the syscall interface itself: five narrow entry
points, each validated against a `Device` capability the kernel issued.

### Restart and the in-flight-request contract

When a ring-3 driver process exits (normally or by fault), the kernel's
capability cleanup path releases all `Device`, `Mmio`, `DmaBuffer`, and
`DeviceIrq` capabilities belonging to that process. Release order is
deterministic:

1. `DeviceIrq` capabilities — ISR disabled, MSI entry cleared.
2. `DmaBuffer` capabilities — IOMMU domain entries unmapped, physical pages freed.
3. `Mmio` capabilities — virtual mappings removed from the process page table.
4. `Device` capability — device removed from the claimed-device registry, IOMMU domain torn down.

The facade (kernel-side proxy that the rest of the kernel calls as if the
driver were local) must handle the window between the driver's exit and its
restart. In-flight block requests return `EIO`; in-flight network sends are
dropped; pending reads are queued behind a restart barrier and retried when the
driver reconnects to the service-manager-assigned socket. This contract is
tested in Track F.3 (cross-device isolation) and F.4 (QEMU smoke tests).

### Lock ordering

To prevent deadlock the kernel acquires registries in a fixed order. Any code
path that needs more than one of these registries must take them in this
sequence:

```
SCHEDULER → DEVICE_HOST_REGISTRY → MMIO_REGISTRY → DMA_REGISTRY
    → IRQ_BINDING_REGISTRY → PCI_DEVICE_REGISTRY → IOMMU registry
```

The IOMMU registry is always outermost (last acquired) because IOMMU operations
can call back into the DMA allocator on some paths. SCHEDULER is always
innermost (first acquired) when a syscall must reschedule after blocking.
Cross-references: B.1 module doc in `kernel/src/syscall/device_host.rs`, A.1
module doc in `kernel-core/src/device_host/`.

### Capability-cascade cleanup on process exit

A `Device` capability is the root of a tree:

```
Device
├── Mmio (one per sys_device_mmio_map call)
├── DmaBuffer (one per sys_device_dma_alloc call)
└── DeviceIrq (one per sys_device_irq_subscribe call)
```

When the owning process exits, the kernel walks this tree bottom-up and
releases each leaf before the root. A child capability held by a second process
(via `sys_cap_grant`) is revoked first: the grantee's handle becomes invalid
and any pending syscall on it returns `EBADF`. This prevents use-after-free on
capability indices if a shared `DmaBuffer` capability outlives the driver.

## Outcome Metrics and Scope Observations

Phase 55b achieved its primary architectural goal — NVMe and e1000 run as
supervised ring-3 processes with IOMMU isolation and service-manager restart —
but the LOC-reduction acceptance criteria were miscalibrated against the
delivered implementation:

| Metric | Target | Actual | Note |
|---|---|---|---|
| Kernel net LOC change (Phase 55b) | <= -1800 | +1917 | `device_host.rs` adds 2204 lines of ring-0 syscall infrastructure not budgeted in the task doc |
| Driver-isolation delta (kernel drivers removed) | <= -1800 | -1597 | NVMe and e1000 kernel code removed, but facades add ~607 lines back |
| Facade size (`blk/remote.rs` + `net/remote.rs`) | ~300 LOC | 518 LOC | IPC framing and restart-barrier logic are larger than estimated |

The task doc (Track F.5) set LOC targets assuming the device-host syscall layer
would reuse existing infrastructure. In practice, five new syscalls with full
capability validation, IOMMU integration, lock-ordering enforcement, and
cascade cleanup add substantial ring-0 code that was not counted in the
original estimate. The architectural invariants (ring-3 isolation, supervised
restart, IOMMU gating) are correctly implemented; the LOC targets were a
planning gap rather than an implementation failure.

## Key Files

| File | Purpose |
|---|---|
| `kernel/src/syscall/device_host.rs` | Five device-host syscall implementations; capability validation, IOMMU integration, lock-ordering enforcement, cascade cleanup on exit |
| `kernel/src/blk/remote.rs` | Ring-0 facade for the ring-3 NVMe driver; accepts VFS block requests and forwards them over IPC, with in-flight-request tracking and restart barrier |
| `kernel/src/net/remote.rs` | Ring-0 facade for the ring-3 e1000 driver; accepts network stack send/receive calls and forwards them over IPC |
| `kernel-core/src/device_host/` | Shared types for device-host capability protocol: `DeviceHostMsg`, `MmioRegion`, `DmaRegion`, `IrqToken`; used by both kernel and userspace |
| `kernel-core/src/driver_ipc/` | IPC message framing for the block and network driver protocols; shared between kernel facades and userspace driver processes |
| `userspace/lib/driver_runtime/` | Rust library providing `DeviceHandle`, `Mmio<T>`, `DmaBuffer<T>`, `IrqNotification`, `BlockServer`, `NetServer` — the safe abstractions ring-3 drivers use |
| `userspace/drivers/nvme/` | Userspace NVMe driver: submission/completion queue management, admin commands, namespace enumeration, interrupt-driven I/O path — 1314 LOC moved from kernel |
| `userspace/drivers/e1000/` | Userspace Intel 82540EM e1000 driver: descriptor ring setup, Tx/Rx paths, link-state management — 802 LOC moved from kernel |

## How This Phase Differs From Later Driver Work

- **Phase 56 (Display and Input)** introduces `display_server`, `kbd_server`,
  and `mouse_server`. These use AF_UNIX sockets and Phase 50 page grants rather
  than the four device-host syscalls, because framebuffer and HID devices do
  not require IOMMU-gated DMA or MSI interrupts. However, Phase 56 adopts the
  `driver_runtime` API as the template for any future hardware-owning userspace
  service (USB HID, GPU, display engine), and reuses the `.conf` +
  `restart=on-failure` + `max_restart` supervision pattern validated by Track
  F.1 in this phase.
- **Phase 57 (VirtIO extraction)** reuses Track B (`device_host` syscalls) and
  Track C (`driver_runtime` library) unchanged. VirtIO was intentionally left
  in ring 0 here to bound Phase 55b scope; Phase 57 extracts VirtIO-blk and
  VirtIO-net using the same machinery.
- **Phase 58 (1.0 gate)** depends on Phase 55b being closed before the
  ring-3-drivers claim can appear in the 1.0 release criteria. No Phase 58
  work begins until this phase's QEMU smoke tests (Track F.4) and the LOC
  audit (Track F.5) are recorded.

## Related Roadmap Docs

- [Phase 55b roadmap doc](./roadmap/55b-ring-3-driver-host.md)
- [Phase 55b task doc](./roadmap/tasks/55b-ring-3-driver-host-tasks.md)
- [Phase 55b residuals — scheduling record](./appendix/phase-55b-residuals.md) (two follow-ups that surfaced during closure)

## Deferred or Later-Phase Topics

> **Note:** Two concrete follow-ups discovered during the Phase 55b closure
> pass — userspace-visible send-path restart handling and the IOMMU VT-d MMIO
> translation bug — are now both owned by Phase 55c and are tracked separately in
> [`docs/appendix/phase-55b-residuals.md`](./appendix/phase-55b-residuals.md)
> so they can be scheduled against their real owners rather than left
> implicit here.


- **VirtIO-blk / VirtIO-net extraction** — Phase 57 extracts these using the
  Track B and C machinery established here.
- **Driver-side seccomp / syscall filtering** — ring-3 drivers currently have
  access to all userspace syscalls; a future phase adds a seccomp profile that
  restricts a driver process to the device-host syscalls and IPC primitives it
  actually needs.
- **Hot-plug** — `sys_device_claim` assumes static PCI topology discovered at
  boot; PCIe hot-plug events (device addition / removal while the OS is running)
  require a hot-plug daemon and dynamic device-host registry updates not
  implemented here.
- **Zero-downtime live update** — the current restart contract allows a brief
  outage (in-flight requests return `EIO`). Seamless driver update without
  dropping any I/O requires state serialization and checkpoint/restore across
  the old and new driver processes, deferred to a post-1.0 phase.
- **Multi-queue NVMe** — the Phase 55b NVMe driver uses a single submission /
  completion queue pair. Multi-queue (one queue per CPU core) is the production
  NVMe configuration and is left for the Phase 57 range when SMP-aware driver
  scheduling is revisited.
