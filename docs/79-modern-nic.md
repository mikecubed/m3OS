# Phase 79 — Modern Intel/Realtek NIC (Learning Doc)

**Status:** Complete
**Source Ref:** phase-79
**Depends on:** Phase 55b (Ring-3 Driver Host), Phase 55c (Ring-3 Driver Correctness Closure), Phase 67 (IOMMU Substrate Completion), Phase 77 (RFC 6298 TCP retransmit)
**Builds on:** the single 82540EM ring-3 e1000 driver (`0x8086:0x100E`, BDF-gated) — generalising it to the NIC silicon that actually ships on 2010-and-later x86 desktops/laptops.
**Primary Components:** `userspace/drivers/{e1000e,igb,igc,r8169,r8125}/`, `userspace/lib/driver_runtime/src/net_ring.rs` (shared ring engine), `kernel-core/src/nic_ids.rs` (device-ID tables), `kernel-core/src/r8169.rs` (XID table), `kernel/src/net/remote.rs` (`REMOTE_NIC` set), the `sys_device_config_read` syscall, and the `multi-nic-smoke` xtask gate.

## Milestone Goal

m3OS finds and uses a real wired NIC on a modern x86 box without falling back to "VirtIO-net only": Intel **e1000e** (82574/82579/I217/I218/I219), **igb** (82575/82576/I210/I211/I350/I354), **igc** (I225/I226), Realtek **RTL8111/8168** Gigabit and **RTL8125** 2.5GbE. The driver model is unchanged from Phase 55b — each NIC is an IOMMU-isolated ring-3 driver feeding the in-kernel TCP/IP stack through `RemoteNic`.

## Why This Phase Exists

The pre-1.0 audit grades the BDF-hardcoded single-e1000 driver as a real-hardware show-stopper: it binds exactly one emulated card. Every Intel NIC of the last 15 years uses a different silicon family (and register/descriptor layout), and Realtek dominates the consumer-board market with an entirely different chipset. "Wired ethernet works in m3OS" must mean more than the QEMU reference.

## Learning Goals

- **Every modern NIC driver is the same three pieces:** a transmit ring, a receive ring, and interrupt-on-completion. Only the register names and the descriptor bit-layout change between vendors. Internalising this is the whole point of writing the third and fourth driver after the first.
- **Intel's families evolved on top of the original e1000 descriptor.** e1000e still accepts the **legacy 16-byte** descriptor; igb/igc require the **advanced (read/write-back union)** descriptor. So "reuse the 82540EM ring code" is *true* for one family and *false* for the next — captured here by the `NicDescriptors` trait with `Legacy16` and `Advanced` implementations behind one generic ring engine.
- **Realtek inverts several Intel assumptions.** No head/tail registers — ownership is a per-descriptor **OWN bit**; TX is kicked by a **TxPoll doorbell**, not a tail write; and the driver dispatches on a runtime **XID** read from `TxConfig`, *not* the PCI device ID.
- **Capability-mediated hardware handoff.** A ring-3 driver obtains and is confined to its device through a PCI claim, an IOMMU-mapped BAR, IOMMU-constrained `DmaBuffer<T>` rings, and a bound IRQ notification — the same model Redox scheme drivers and DPDK/VFIO use.
- **Discovery before claim.** Because `sys_device_claim` enables bus-mastering and has no release, you cannot probe-by-claim. The new `sys_device_config_read` reads vendor:device IDs *before* claiming, so exactly one family driver binds each function.
- **Why single-queue, one-IRQ-per-packet, no-offload is the right 1.0 scope**, and the ordered scaling ladder beyond it.

## Feature Scope

### The universal NIC model (all five drivers)

Every driver: claim its PCI function (`DeviceHandle::claim`), map BAR0 (`Mmio<T>`), allocate TX/RX descriptor rings + per-slot buffers as IOMMU-constrained `DmaBuffer<T>`, subscribe to its IRQ (`IrqNotification`), register a `net.nic` IPC endpoint, and run a single-threaded loop multiplexing the IRQ notification with served IPC (Phase 55c bound-notification pattern). The kernel `RemoteNic` façade is unchanged — the in-kernel TCP/IP stack does not care which silicon is at the other end.

### Interrupt mode: INTx vs MSI-X (a load-bearing subtlety)

The driver loop is **interrupt-driven for RX** — it blocks until the IRQ notification wakes it, then drains the RX ring. So if the interrupt never reaches the driver, the RX ring never drains and *no packets move even though link is up and the device claims/initialises cleanly.* This is exactly the trap e1000e fell into: the device-host IRQ allocator (`allocate_device_vector`) preferred **MSI-X** whenever the device advertised it, and the 82574/82576 *do*. But these legacy-model NIC drivers set the simple `ICR`/`IMS` (Realtek: classic `ISR`) cause registers and program **no MSI-X cause routing** (the 82574/82576 `IVAR`/`EIMS` block). The kernel duly enabled MSI-X, but the device — with `IVAR` unprogrammed — sent nothing. The classic 82540EM e1000 dodged this only because it has no MSI-X capability and so fell to INTx.

The fix gates on PCI class: **Ethernet-class (`0x02`) device-host devices use legacy INTx**, while storage (nvme, `0x01`) and the xHCI host controller (serial-bus, `0x0C`) — which *do* program their own MSI-X routing — keep MSI-X. With INTx the 82574/82576 assert the legacy interrupt pin on any unmasked cause, the kernel routes it through the I/O APIC, and the shared loop drains RX as designed. Lesson: *MSI-X is not free* — enabling it commits the driver to programming per-cause vector routing, and "interrupt-capable" silicon can be silently interrupt-*less* if that routing is skipped. A single-message **MSI** would also have worked (one message per `cause & mask`), which is why the acceptance reads "INTx **or single-MSI** (no MSI-X required)".

### Intel legacy vs advanced descriptors

The classic e1000 descriptor is a 16-byte struct modeled by `kernel_core::e1000::{E1000RxDesc,E1000TxDesc}`. **e1000e accepts it** — so the e1000e driver reuses the e1000 driver's `init`/`io`/`rings` verbatim, differing only in PCI discovery. **igb and igc do not**: they require the *advanced* descriptor, a read/write-back union where TX writes `buffer_addr`/`cmd_type_len`/`olinfo_status` and hardware writes back a status union, and interrupts move from `ICR` to the `EICR`/`EIMS` block. The `driver_runtime::net_ring::NicDescriptors` trait carries the per-family descriptor layout + encode/decode; the alloc/BAL-BAH/LEN/head-tail/DD-drain/doorbell control flow is shared.

### Realtek OWN-bit / TxPoll / XID model

`r8169` is structurally different, not a renamed register map. The ring is 256-byte-aligned; ownership is per-descriptor via `DescOwn` (`0x80000000`), the last descriptor carries `EOR` (`0x40000000`), and `FS`/`LS` (`0x20000000`/`0x10000000`) mark packet boundaries. 64-bit base addresses use split registers (`Tx 0x20/0x24`, `Rx 0xE4/0xE8`); TX starts via the **TxPoll doorbell** (`0x38`, `NPQ=0x40`); config writes are bracketed by the `Cfg9346` (`0x50`) unlock(`0xC0`)/lock(`0x00`) window. The driver computes a `mac_version` from the `TxConfig` (`0x40`) **XID** via a `{mask,value}` table (`kernel_core::r8169`), and every reset/init/PHY/IRQ quirk branches on that version. `RTL8125` layers a 32-bit "V2" interrupt block (`IMR_V2_CLEAR 0x150`/`ISR_V2 0x154`/`IMR_V2_SET 0x158`) and a signed-PHY-firmware requirement on top.

### Per-family QEMU emulation reality

This drives the `multi-nic-smoke` gate design:

| Family | QEMU device | CI-testable? |
|---|---|---|
| 82540EM (existing) | `-device e1000` | ✅ regression baseline |
| e1000e (82574L) | `-device e1000e` | ✅ primary new target |
| igb (82576) | `-device igb` | ⚠️ QEMU ≥ 8.0, partial model |
| igc (I225/I226) | *(none)* | ❌ hardware-only |
| RTL8111/8168, RTL8125 | *(none — QEMU emulates only RTL8139)* | ❌ hardware/VFIO-only |

So `multi-nic-smoke` exercises e1000/e1000e in CI (and igb behind a version guard); igc and all Realtek paths are gated behind opt-in `M3OS_*_REGRESSION` env vars and skipped-with-reason otherwise. The hardware-only families ship **structurally complete + host-tested**; their real-card `ping`/`curl` validation is a Phase-83 hardware handoff.

## Important Components and How They Work

- **`sys_device_config_read` (0x1128).** `(segment, bus, dev, func, packed=(offset<<8)|width) -> value|errno`, `/drivers/`-gated, reads PCI config space by raw BDF without claiming. `driver_runtime::read_vendor_device` + `enumerate_ethernet_functions` + `select_nic` build the discovery flow.
- **`kernel_core::nic_ids`.** Device-ID slices + `is_<family>` predicates, cross-family-disjoint by host test. The driver-routing split (igb claims I210/I211/I350/82575/82576/I354; igc claims only I225/I226; r8169 owns 1GbE; r8125 owns `0x8125`/`0x8126`) lives here.
- **`driver_runtime::net_ring`.** `NicDescriptors` trait, `Legacy16` (e1000/e1000e) + `Advanced` (igb/igc) impls, and the shared ring math.
- **`kernel/src/net/remote.rs`.** `REMOTE_NIC: Vec<NicEntry>` with index-0 default route; the single-NIC fast path is preserved.

## How This Builds on Earlier Phases

Reuses the Phase 55b ring-3 driver-host syscalls (`sys_device_claim`/`_mmio_map`/`_dma_alloc`/`_irq_subscribe`) unchanged; reuses Phase 67's IOMMU `DmaBuffer<T>` so each driver is sandboxed by a per-device VT-d/AMD-Vi domain; reuses the Phase 55c bound-notification multiplexing + EAGAIN-on-restart contract; reuses the legacy descriptor + ring math from `kernel_core::e1000` for e1000e and generalises them behind `NicDescriptors` for igb/igc; lifts the Phase 55b single-NIC assumption to a bounded `Vec`; hard-depends on the Phase 77 RFC 6298 retransmit fix (without it the new drivers "almost work" then hang on the first dropped packet).

## Implementation Outline

1. Extract the shared ring engine + `Descriptor` trait, keeping the 82540EM driver working on `Legacy16`.
2. Add `sys_device_config_read` + `nic_ids` so drivers discover by device ID.
3. Bring up **e1000e** against `-device e1000e`; DHCP + ping + an HTTP GET over the kernel stack.
4. Bring up **igb** against `-device igb` (≥ 8.0) on the advanced-descriptor path.
5. Bring up **igc** structurally (no QEMU model).
6. Bring up **r8169/RTL8111-8168** (XID + OWN-bit/TxPoll ring + Cfg9346) — hardware-only.
7. Bring up **RTL8125** (corrected `0x8125`, V2 interrupts, signed firmware) — hardware-only.
8. Lift `REMOTE_NIC` to a `Vec`; wire the per-driver `.conf`/ramdisk/bins/members; add `multi-nic-smoke`.
9. Write this doc + cross-link `docs/16-network.md`; bump kernel `0.79.0`; update the roadmap README.

## Acceptance Criteria

See `docs/roadmap/79-modern-nic.md` and `docs/roadmap/tasks/79-modern-nic-tasks.md` for the authoritative, per-track acceptance checklists. Summary: e1000e DHCP+ping+HTTP-GET under `multi-nic-smoke`; igb link under `-device igb` (QEMU ≥ 8.0); igc/r8169/r8125 structurally complete + host-tested (device-ID, descriptor, XID, firmware-header logic) with real-hardware validation deferred to Phase 83 where no card is present; the NIC registry holds ≥ 2 NICs with the single-NIC path unbroken; `cargo xtask check` green and kernel at `0.79.0`.

## Companion Task List

- [Phase 79 Task List](./roadmap/tasks/79-modern-nic-tasks.md)
- [Phase 79 Design Doc](./roadmap/79-modern-nic.md)

## How Real OS Implementations Differ

- **Linux** ships drivers for every NIC ever made (hundreds of PCI IDs); m3OS ships these families and defers the rest.
- **Redox OS** runs *both* the NIC driver and the TCP/IP stack in ring 3 (daemon-to-daemon "network" scheme); m3OS keeps the stack in-kernel and only the driver in ring 3, bridged by `RemoteNic` — fewer context switches, simpler 1.0 correctness. Redox's `pcid`+`physmap`+`irq` handoff is one-to-one with m3OS's PCI-claim + IOMMU-BAR + IRQ-notification.
- **DPDK/VFIO** program the IOMMU so the device can only DMA into driver-owned memory — the same model as m3OS's IOMMU-mapped BAR + `DmaBuffer<T>` — proving the ring-3 NIC direction is production-validated. They run poll-mode (zero interrupts); m3OS's one-IRQ-per-packet is the deliberate low-rate opposite.
- **FreeBSD/OpenBSD** (`em`/`igc`/`re`) keep interrupts but enable ITR moderation and drain the whole RX ring per IRQ; OpenBSD `em` is the minimal single-queue moderated reference matching m3OS scope.
- Real OSes implement TSO/GSO/GRO/LRO offload, NAPI/iflib polling, PHY power management, MSI-X multi-queue RSS, and XDP fast paths — all deferred.

## Deferred Until Later

Ordered scaling ladder beyond the 1.0 one-IRQ-per-packet path: (1) larger DMA rings + interrupt moderation (ITR) with full RX-ring drain; (2) shared-memory frame rings replacing per-frame IPC copies (seL4 sDDF / Genode `Nic_session`); (3) smoltcp-style `RxToken`/`TxToken` zero-copy; (4) MSI-X multi-queue + RSS and/or moving the stack to ring 3.

Out of scope entirely for Phase 79: Broadcom/Marvell/Aquantia/Mellanox wired NICs; TSO/GSO/GRO/LRO offload; multi-queue RSS; multi-NIC routing tables (single default interface only); per-NIC differing MTU / jumbo frames; the I219 CSME/ULP reset-handoff dance; WoL/EEE/PHY power management; RTL8126 5GbE beyond opportunistic ID matching; Wi-Fi (Phase 81); bonding/VLAN/bridging.
