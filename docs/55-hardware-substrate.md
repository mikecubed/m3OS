# Hardware Substrate

**Aligned Roadmap Phase:** Phase 55
**Status:** Complete
**Source Ref:** phase-55
**Supersedes Legacy Doc:** (none -- new content)

## Overview

Phase 55 turns m3OS's hardware story from "QEMU/VirtIO-centric development
target" into a bounded, named real-hardware support promise. The scope covers a
small native hardware-access layer (BAR mapping, DMA buffers, IRQ delivery,
device binding), PCI modernization (PCIe MCFG, MSI/MSI-X, device claim), and
two reference drivers: NVMe storage and the Intel 82540EM classic e1000
network card. The existing VirtIO drivers are migrated onto the new layer so
the abstractions are validated by known-good code before NVMe and e1000 build
on top of them.

## What This Doc Covers

- The **hardware-access layer** (BAR mapping, DMA buffers, device IRQ
  contract, driver registry) that every driver in Phase 55 is built on.
- The **donor strategy** the project follows when sourcing driver code:
  specs first, Redox second, BSD third, Linux as reference only.
- The **reference matrix** of hardware Phase 55 commits to: VirtIO-blk /
  VirtIO-net as the existing baseline, plus NVMe and Intel 82540EM e1000 as
  the first non-VirtIO storage and network paths.
- The **driver architecture** for NVMe and e1000: how each interacts with the
  hardware-access layer instead of inventing bespoke kernel glue.
- How Phase 55 **differs from later hardware work** (ring-3 driver host,
  broader device matrix, IOMMU-aware DMA).
- The **key files** introduced by Phase 55 and what each contributes.
- The **evaluation-gate verification** required to close the phase.

The authoritative design doc is
[docs/roadmap/55-hardware-substrate.md](./roadmap/55-hardware-substrate.md) —
this learner-facing doc cross-references it rather than duplicating the
normative content.

## Donor Strategy

Phase 55 adopts the driver-sourcing strategy recorded in
[docs/evaluation/hardware-driver-strategy.md](./evaluation/hardware-driver-strategy.md)
as explicit phase policy. The adoption order is:

1. **Public specs and datasheets first.** Register layouts, command
   structures, descriptor formats, and reset sequencing come from the NVMe
   spec and the Intel 82540EM manuals before any third-party driver code is
   consulted.
2. **Redox second.** The [Redox drivers repository](https://gitlab.redox-os.org/redox-os/drivers)
   is m3OS's closest external donor: Rust, microkernel-oriented, MIT-licensed,
   and already covers NVMe (`nvmed`) and e1000 (`e1000d`). Per
   [redox-driver-porting.md](./evaluation/redox-driver-porting.md) the reusable
   part is device logic; Redox's scheme/daemon/event glue is not ported.
3. **BSD third.** BSD drivers are consulted as a permissive reference when
   specs or Redox are insufficient, but not used as a primary donor.
4. **Linux as reference only.** Linux is treated as a hardware encyclopedia for
   PCI IDs, quirks, reset sequencing, and timeouts. No Linux driver code is
   imported; m3OS's MIT licensing and native driver model make direct Linux
   reuse the wrong trade-off for this phase.

### Specific Redox drivers consulted

Phase 55 shipped the NVMe (Track D) and e1000 (Track E) drivers **without
copying code from Redox's `nvmed` or `e1000d`**. Both implementers worked
directly from specs (NVMe Base Specification 1.4; Intel 82540EM manual
§13.4/§13.5 for e1000) and from the already-landed VirtIO drivers as a
behavioral reference. Redox remained the first external reference point the
project would consult on implementation questions, but no specific Redox
files had to be imported, quoted, or translated to get the reference drivers
to pass their data-path smoke tests.

The practical consequence is that the Phase 55 drivers are fully native m3OS
code with no Redox licensing or integration coupling. Future phases (broader
device coverage, ring-3 driver host) will likely need deeper Redox references
and should record those here as they happen.

### Deviations from the donor strategy

Phase 55 did not deviate from the above order. The ring-0 placement of the
NVMe and e1000 drivers (see the Documentation Notes in
[docs/roadmap/tasks/55-hardware-substrate-tasks.md](./roadmap/tasks/55-hardware-substrate-tasks.md))
is an architectural trade-off, not a donor-strategy deviation: it is about
where the code runs, not where the code came from.

## Hardware-Access Layer

The hardware-access layer is the thin abstraction that every Phase 55 driver
goes through. It is deliberately narrow: the kernel-side driver still knows
its own device's register semantics, but the **mechanics** of reaching those
registers — mapping BARs, allocating DMA-safe memory, installing interrupt
handlers, claiming the PCI device — are shared.

### Compose order

A Phase 55 driver composes the layer in a fixed order:

1. **Register with the driver registry.** `register_driver(DriverEntry { .. })`
   records a PCI match rule (vendor/device IDs, or class/subclass/prog-if) and
   an init function. See `kernel/src/pci/mod.rs`.
2. **Get probed by `probe_all_drivers`.** During boot, the kernel walks the
   PCI device table in bus/device/function order and invokes the registered
   `probe_fn` for any match whose device has not yet been claimed.
3. **Claim the PCI device.** `claim_pci_device` returns an exclusive
   `PciDeviceHandle` that owns config-space access for that function and
   blocks a second driver from binding the same device. See
   `kernel/src/pci/mod.rs`.
4. **Map one or more BARs.** `handle.map_bar(bar_index)` returns a
   `BarMapping` (MMIO or PIO), using standard PCI BAR sizing to discover the
   region's extent and mapping MMIO BARs into the kernel virtual address
   space with uncacheable memory type. See `kernel/src/pci/bar.rs`.
5. **Allocate DMA memory.** `alloc_dma_buffer::<T>(len, align)` returns a
   `DmaBuffer<T>` that holds both the kernel virtual address (for CPU access)
   and the bus-visible physical address (for programming into device
   descriptors). The allocation is physically contiguous, backed by the
   buddy allocator from Phase 53a. See `kernel/src/mm/dma.rs`.
6. **Install an IRQ.** `handle.install_msi_irq(vector, handler)` routes an
   MSI/MSI-X vector to the driver's ISR; drivers that find no MSI capability
   fall back to `install_legacy_intx_irq` on the PCI interrupt line. See
   `kernel/src/pci/mod.rs` (and `DeviceIrq` for the handler contract).

### Why this contract

Each step of the compose order corresponds to a kernel invariant the driver
must respect:

- **`claim_pci_device` as the ownership gate.** Two drivers cannot fight for
  the same device, and the claim itself is the audit trail for who owns what.
- **`BarMapping` as the register-access shape.** Every driver sees BAR data
  the same way (`read_reg::<T>(offset)` / `write_reg::<T>(offset, value)`),
  so bugs in the MMIO path are one-place-fixable instead of per-driver.
- **`DmaBuffer` as the DMA shape.** The driver writes the
  `DmaBuffer::physical_address()` into its descriptor rings. The kernel
  guarantees the CPU side of the mapping matches, and `Drop` returns the
  frames to the buddy allocator. Drivers cannot forget to release DMA memory.
- **`DeviceIrq` as the ISR shape.** Handler code runs in interrupt context
  with no allocation, no blocking, and no IPC. The trait pins that discipline
  in the type system so a driver cannot accidentally (for example) call
  `wake_task` on a task that has been freed.

### Reusing the layer for the VirtIO baseline

The existing VirtIO-blk and VirtIO-net drivers were migrated to use the same
contract in C.5. This means the layer is validated by known-good code before
NVMe and e1000 build on it. The migration also replaced VirtIO's former
spin-polling completion path with IRQ-driven wakeup via `wake_task` /
`block_current_unless_woken`, which closes the same class of latency issue
that the NVMe and e1000 drivers would have inherited if completion had
stayed polled.

## Reference Matrix

The bounded set of supported hardware for Phase 55. The authoritative matrix
lives in the design doc's "Reference Hardware Matrix" section; the summary
below mirrors the supported entries with learner-friendly framing.

| Device class | Target | PCI ID | QEMU flag | Status |
|---|---|---|---|---|
| Block storage (VirtIO) | VirtIO-blk baseline | `0x1af4:0x1001` | default (`-drive if=virtio`) | Validated (baseline) |
| Block storage (NVMe) | QEMU NVMe controller | `0x1b36:0x0010` | `cargo xtask run --device nvme` | Validated in QEMU |
| Network (VirtIO) | VirtIO-net baseline | `0x1af4:0x1000` | default (`-device virtio-net-pci`) | Validated (baseline) |
| Network (Intel e1000) | Intel 82540EM classic e1000 | `0x8086:0x100E` | `cargo xtask run --device e1000` | Validated in QEMU |
| IOMMU (Intel VT-d) | QEMU `-device intel-iommu` | n/a | `cargo xtask run --iommu` | Validated via Phase 55a |

The **physical-hardware promise** is deferred: Phase 55 commits to QEMU
emulation only. The Reference Matrix in the design doc names the device
classes that are out of scope (e1000e family, xHCI, AHCI, Realtek, HDA).

The **IOMMU caveat** that Phase 55 carried — that VT-d / AMD-Vi enabled
systems could block driver DMA until IOMMU mappings existed — was
closed by [Phase 55a — IOMMU Substrate](./55a-iommu-substrate.md). The
`cargo xtask run --iommu` configuration is the canonical validated-IOMMU
entry; see the Phase 55a learning doc and
[docs/roadmap/55a-iommu-substrate.md](./roadmap/55a-iommu-substrate.md)
for the parser, per-device domains, and the identity fallback the default
`cargo xtask run` configuration still uses.

For the exact QEMU command fragments, see "Reference QEMU configurations" in
[docs/roadmap/55-hardware-substrate.md](./roadmap/55-hardware-substrate.md).
The F.1 xtask flags implement those exact fragments.

## Driver Architecture

### NVMe storage (`kernel/src/blk/nvme.rs`)

NVMe is a queue-based storage interface that closely resembles VirtIO in
architecture — the controller exposes submission and completion queues, the
driver programs doorbell registers when work is enqueued, and the device
signals completion via MSI/MSI-X (or legacy INTx as a fallback). The Phase 55
bring-up path is:

1. **Probe and claim.** `nvme_probe` matches on class `01:08:02` (NVMe) and
   claims the device through the driver registry.
2. **Map BAR0.** NVMe registers are MMIO-only; BAR0 covers a small
   architecturally fixed region including `CAP`, `VS`, `CC`, `CSTS`, `AQA`,
   `ASQ`, `ACQ`, and the per-queue doorbell bank.
3. **Controller reset.** Clear `CC.EN`, wait for `CSTS.RDY=0` with a timeout
   bounded by `CAP.TO` × 500 ms so a wedged controller surfaces as a
   bring-up error rather than a hang.
4. **Admin queue.** Allocate admin submission/completion queues as
   `DmaBuffer<[NvmeCommand]>` and `DmaBuffer<[NvmeCompletion]>`. Program
   `AQA` / `ASQ` / `ACQ`. Enable the controller (`CC.EN=1`) and wait for
   `CSTS.RDY=1`.
5. **Identify.** Issue Identify Controller (`opcode 0x06`, CNS=1) to record
   the model/serial/firmware strings; then Identify Namespace to record the
   namespace capacity and LBA format.
6. **I/O queue pair.** Create one I/O CQ (`opcode 0x05`) and one I/O SQ
   (`opcode 0x01`) via the admin queue.
7. **MSI/MSI-X completion.** Install an interrupt handler on the MSI vector
   via the hardware-access layer; fall back to polling if MSI allocation
   fails.
8. **Data-path smoke.** Write a deterministic 512-byte pattern to LBA 0, read
   it back, compare. On mismatch, clear `NVME_READY` so the block dispatch
   layer falls back to VirtIO-blk instead of silently corrupting data.

Read/Write commands use **Physical Region Page (PRP)** lists — PRP1 and PRP2
for small transfers; a spill-over PRP list page for larger transfers. Each
PRP entry is a physical address taken from the `DmaBuffer` staging area the
driver allocates for the transfer. This is the path that satisfies the
Phase 55 acceptance criterion "QEMU NVMe device reads and writes return
correct data."

The key NVMe pure-logic types (`NvmeCommand`, `NvmeCompletion`, `NvmeCap`,
opcode constants) live in `kernel-core/src/nvme.rs` so they are
host-testable and so the kernel side never redefines the 64-byte command
layout.

### Intel 82540EM e1000 (`kernel/src/net/e1000.rs`)

The classic e1000 is the simplest common NIC design — fixed-size descriptor
rings in host memory, MMIO registers to program ring base addresses and
head/tail pointers, level-triggered INTx interrupt for completion. The
bring-up path is:

1. **Probe and claim.** `e1000_probe` matches on vendor `0x8086` device
   `0x100E` (82540EM) and claims the device.
2. **Map BAR0.** e1000 registers are MMIO-only.
3. **Global reset.** Write `CTRL.RST`, wait for the bit to clear. Program
   `CTRL` with `ASDE` (auto-speed detection) and `SLU` (set link up).
4. **MAC and multicast.** Read `RAL0` / `RAH0` to extract the 6-byte MAC.
   Zero the Multicast Table Array.
5. **Receive ring.** Allocate a 256-entry `DmaBuffer<[E1000RxDesc]>` and
   pre-allocate per-descriptor 2 KiB packet buffers. Program `RDBAL` /
   `RDBAH` / `RDLEN` / `RDH` / `RDT`. Enable receive with `RCTL` configured
   for broadcast accept and CRC strip.
6. **Transmit ring.** Allocate a 256-entry `DmaBuffer<[E1000TxDesc]>`.
   Program `TDBAL` / `TDBAH` / `TDLEN`. Enable transmit with `TCTL`
   configured with collision threshold 0x10 and backoff 0x40.
7. **IRQ install.** Classic e1000 has no MSI-X capability in QEMU, so the
   driver takes the legacy-INTx fallback on the board's interrupt line. The
   ISR reads `ICR` (read-to-clear), walks the RX ring from software tail to
   hardware head, and wakes the background `net_task` via `wake_task`.
8. **Ring integration.** `net/mod.rs` gains a `send_frame` / `mac_address`
   dispatch that selects e1000 when `E1000_READY`, else VirtIO-net. ARP and
   IPv4 send through the dispatcher so existing TCP/UDP/ICMP code works
   unchanged.

The ISR is deliberately the minimum possible path: read `ICR`, read
`STATUS`, update atomics, wake the net task. The E.1-E.4 fix commit
(`968e683`) hardened this path against a specific IRQ-storm race where an
LSC interrupt could fire between `IMS` arming and the driver being stored
in the `DRIVER` slot; the current sequence orders the slot write before the
`IMS` write to close that window.

e1000 pure-logic types (`E1000Regs`, `E1000RxDesc`, `E1000TxDesc`, flag
bitflags) live in `kernel-core/src/e1000.rs`.

### Manual live validation

The F.1 xtask flags (`cargo xtask run --device nvme|e1000`) give every
operator a one-command way to exercise the drivers against QEMU. The
following manual checks were run when Phase 55 closed:

- `cargo xtask run --device nvme` prints `nvme data-path smoke OK (512B
  round-trip at LBA 0)` and reaches `m3OS init (PID 1) — service manager`.
- `cargo xtask run --device e1000` prints `e1000 MAC:
  52:54:00:12:34:56`, `initial STATUS=... link_up=true`, and reaches
  userspace init.
- `cargo xtask run --device nvme --device e1000` brings both drivers up
  in the same boot.
- `ping 10.0.2.2` (QEMU SLIRP gateway) returning from inside the guest
  proves ICMP through e1000. TCP through e1000 was validated via the
  existing telnet/SSH hostfwd rules.

These manual steps are the public equivalent of the in-kernel smoke tests —
they are recorded here rather than as automated CI because the operator
already has to look at the serial log to validate a boot smoke, and the
in-kernel smoke already fails closed (`NVME_READY=false`, `E1000_READY=false`)
if anything is wrong.

## Differences From Later Hardware Work

- **Ring-0 placement is deliberate and bounded.** Phase 55 places NVMe
  and e1000 in ring 0 as a staged bring-up trade-off: get the hardware
  paths working under kernel supervision before moving the trust boundary.
  The hardware-access layer is designed so its contracts are callable from
  a future userspace driver host (BAR mapping returns a handle, DMA
  allocation returns a buffer with a physical address, IRQ installation
  takes a handler) rather than being baked into kernel-only call sites.
  That extraction is completed in
  [Phase 55b — Ring-3 Driver Host](./roadmap/55b-ring-3-driver-host.md),
  which adds the `device_host` kernel subsystem, the five device-host
  syscalls (`sys_device_claim`, `sys_device_mmio_map`,
  `sys_device_dma_alloc`, `sys_device_dma_handle_info`,
  `sys_device_irq_subscribe`), and supervised ring-3
  `nvme_driver` / `e1000_driver` services built on
  `userspace/lib/driver_runtime/`.
- **Narrow device coverage.** Phase 55 does not attempt broad NIC or
  storage coverage. The e1000e family (82574, 82576) is explicitly **out**
  of scope; so are AHCI, xHCI, Realtek NICs, HDA, and GPU/audio hardware.
  Each of those is expected to be its own phase with its own reference
  target.
- **IOMMU-aware DMA isolation landed separately.** Phase 55 shipped the
  flat physical-to-bus identity path; VT-d and AMD-Vi translated domains
  landed in [Phase 55a — IOMMU Substrate](./55a-iommu-substrate.md). Under
  `cargo xtask run --iommu`, every `DmaBuffer` is routed through a per-
  device IOMMU domain. Under plain `cargo xtask run`, Phase 55a's
  `IdentityDomain` fallback preserves the Phase 55 behavior.
- **Partial interrupt coverage.** MSI-X is plumbed for devices that
  advertise the capability (NVMe, VirtIO). Devices that only expose legacy
  INTx (classic e1000 under QEMU) work but lose per-queue vectors. A later
  phase will extend the hardware-access layer to handle MSI-X vector
  steering across cores.
- **PCIe passthrough boundary.** MCFG is parsed, MSI/MSI-X are routed,
  extended config space is reachable, but PCIe-specific features (AER,
  power management D3cold, hot-plug) are not. Phase 55 treats the PCI
  layer as "good enough for bring-up" rather than production-complete.

## Key Files

| File | Purpose |
|---|---|
| `kernel/src/acpi/mod.rs` | MCFG ACPI table parser — feeds PCIe MMIO base / segment / bus range into the PCI layer |
| `kernel/src/pci/mod.rs` | Legacy + MMIO PCI config-space access, driver registry, device claim, MSI/MSI-X capability parsing, `DeviceIrq` installation |
| `kernel/src/pci/bar.rs` | `BarMapping` / `MmioRegion` / `map_bar` — reusable BAR decoding and MMIO/PIO abstraction |
| `kernel/src/mm/dma.rs` | `DmaBuffer<T>` / `alloc_dma_buffer` — physically-contiguous DMA-safe memory with bus-address accessor and buddy-allocator reclaim on `Drop` |
| `kernel/src/arch/x86_64/interrupts.rs` | Device-IRQ stub bank that the hardware-access layer installs handlers into |
| `kernel/src/blk/nvme.rs` | NVMe controller driver: probe, reset, admin queue, Identify, I/O queue pair, PRP-based read/write, MSI completion |
| `kernel/src/blk/mod.rs` | Block dispatch — routes `read_sectors` / `write_sectors` to NVMe when `NVME_READY`, else VirtIO-blk |
| `kernel/src/net/e1000.rs` | Intel 82540EM classic e1000 driver: probe, reset, TX/RX rings, IRQ, packet dispatch |
| `kernel/src/net/mod.rs` | Network dispatch — `send_frame` / `mac_address` select e1000 when `E1000_READY`, else VirtIO-net |
| `kernel/src/blk/virtio_blk.rs` | Existing VirtIO-blk driver migrated onto the hardware-access layer (C.5) |
| `kernel/src/net/virtio_net.rs` | Existing VirtIO-net driver migrated onto the hardware-access layer (C.5) |
| `kernel-core/src/pci.rs` | Host-testable PCI capability parsing (MSI, MSI-X) |
| `kernel-core/src/nvme.rs` | Host-testable NVMe register/command/completion definitions |
| `kernel-core/src/e1000.rs` | Host-testable e1000 register/descriptor/flag definitions |
| `xtask/src/main.rs` | F.1 `--device nvme|e1000` flags for `cargo xtask run`, `run-gui`, and `test` |

## Evaluation Gate Verification

The design doc's four evaluation gates are verified as follows:

- **Service-boundary readiness.** Phase 54 serverization narrowed the kernel
  enough that the NVMe and e1000 drivers do not widen the TCB beyond the
  hardware-access layer contract. Ring-0 placement is a deliberate trade-off
  with a documented ring-3 extraction path (see Documentation Notes in the
  task doc). The hardware-access layer itself is the proposed future seam
  between ring 0 and a userspace driver host.
- **Donor-source readiness.** NVMe spec 1.4 and Intel 82540EM manual
  §13.4/§13.5 are the primary sources. Redox `nvmed` and `e1000d` remained
  the closest Rust external donors but no Redox code was imported — the
  drivers are written natively against the specs with the existing VirtIO
  drivers as behavioral references. See the "Specific Redox drivers
  consulted" section above.
- **Validation environment.** The F.1 xtask flags make every QEMU config in
  the reference matrix reproducible with a single command. The Reference
  QEMU Configurations section of the design doc defines the exact fragments
  those flags produce, so operators can run the matrix without inspecting
  xtask internals. Physical-hardware validation is deferred per the matrix.
- **Release posture.** The reference hardware matrix is the narrow hardware
  promise for the milestone. Entries outside the matrix are explicitly out
  of scope. The matrix is cross-referenced from both the design doc and
  this learning doc.

All four gates are verified. Phase 55 is closed with these results; a
future phase can revisit them as scope expands.

## Related Roadmap Docs

- [Phase 55 roadmap doc](./roadmap/55-hardware-substrate.md)
- [Phase 55 task doc](./roadmap/tasks/55-hardware-substrate-tasks.md)
- [Hardware driver strategy evaluation](./evaluation/hardware-driver-strategy.md)
- [Redox driver porting evaluation](./evaluation/redox-driver-porting.md)

## Deferred or Later-Phase Topics

- Broad laptop/desktop hardware certification
- Wi-Fi, GPU, and USB peripheral matrices
- IOMMU-aware DMA isolation (VT-d / AMD-Vi) — delivered by
  [Phase 55a — IOMMU Substrate](./55a-iommu-substrate.md)
- **Completed in Phase 55b:** Ring-3 extraction of the NVMe and e1000
  drivers following the Phase 54 `vfs_server` / `net_server` pattern —
  delivered by [Phase 55b — Ring-3 Driver Host](./roadmap/55b-ring-3-driver-host.md)
- Hardware-acceleration features not needed for the reference targets
- e1000e family (82574, 82576, etc.), AHCI, xHCI, Realtek, HDA
- PCIe AER, D3cold power management, hot-plug
