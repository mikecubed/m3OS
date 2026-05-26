# Phase 79 - Modern Intel/Realtek NIC

**Status:** Planned
**Source Ref:** phase-79
**Depends on:** Phase 55b (Ring-3 Driver Hosting) ✅, Phase 55c (Ring-3 Driver Correctness Closure) ✅, Phase 67 (IOMMU Substrate) ✅, Phase 77 (Pre-1.0 Correctness)
**Builds on:** Extends the Phase 55b ring-3 NIC story (today: 82540EM-only e1000 driver `0x8086:0x100E`) with the actual NIC silicon shipping on 2010-and-later x86 desktops — at minimum Intel e1000e/igb/igc and Realtek RTL8169/8125
**Primary Components:** `userspace/drivers/e1000e/` (new), `userspace/drivers/igb-igc/` (new), `userspace/drivers/r8169/` (new), `userspace/drivers/r8125/` (new), `kernel/src/net/` (no change — kernel TCP/IP already drives `RemoteNic` from Phase 55b)

## Milestone Goal

m3OS finds and uses a real wired NIC on a modern x86 desktop or laptop without falling back to "VirtIO-net only." The supported set at the end of this phase: Intel e1000e (`0x10D3`, `0x153A`, `0x153B`, `0x1502`), Intel igb (server PCH NICs), Intel igc (i225-V / i226-V — common on 2021+ boards), Realtek RTL8169 (`0x8168`), Realtek RTL8125 (`0x8161`).

## Why This Phase Exists

The Phase 74a §3 audit grades the current e1000 driver as a real-hardware show-stopper: it is hard-gated to the QEMU-emulated 82540EM device ID. Every Intel NIC shipped in the last 15 years uses a different silicon family (and a different register layout). Realtek NICs dominate the consumer-board market and use a completely different chipset entirely.

Without this phase, "wired ethernet works in m3OS" is true only on the QEMU reference and a handful of museum-grade desktops. A 1.0 release that cannot reach the LAN on a real Intel or Realtek board is not a 1.0 release.

## Learning Goals

- Understand how NIC driver work decomposes into "transmit ring + receive ring + interrupt-on-completion," shared across all modern designs
- See how Intel's e1000e/igb/igc evolved on top of the same descriptor format with successive offload features (TSO, GRO, multi-queue, RSS)
- Learn how Realtek's r8169 family differs from Intel's design (different ring layout, different register names, same conceptual model)
- Understand why multi-queue + RSS is deferred — single-queue is plenty for 1.0
- Practice writing a second NIC driver that reuses the Phase 55b ring-3 host primitives without modification

## Feature Scope

### Track A — Intel e1000e family

- **A.1** — PCI probe for the e1000e device IDs. Map BAR0. Read MAC address from EEPROM (or the on-die-fused location depending on chipset variant).
- **A.2** — Initialize Transmit + Receive rings (the descriptor format is upward-compatible with the existing 82540EM driver, so the ring code shares 80% with the current `e1000` driver). One TX queue, one RX queue.
- **A.3** — Link state polling + interrupt-on-completion via the Phase 55b `sys_device_irq_bind` path.

### Track B — Intel igb / igc

- **B.1** — igb covers PCH-integrated NICs on Intel server boards and a few desktop PCHs. The register layout is similar enough to e1000e to share the queue management code; the differences are in MAC initialization and PHY access.
- **B.2** — igc covers i225-V (Comet Lake desktop PCH) and i226-V (Alder Lake+). Critical for 2021+ Intel desktop boards.

### Track C — Realtek RTL8169 family

- **C.1** — PCI probe for `0x10EC:0x8168`. Map BAR0. RTL8169-style descriptor ring (different layout from Intel).
- **C.2** — Per-revision quirks: RTL8169 has eight commercially shipped silicon revisions with slightly different reset sequences. Implement the union of the documented quirks — Linux's `r8169_main.c` is the reference.

### Track D — Realtek RTL8125

- **D.1** — 2.5 GbE / 5 GbE consumer NIC, very common on modern boards. Different register block from RTL8169; treat as a fresh driver.

### Track E — Kernel-side bookkeeping

- **E.1** — `kernel-core::net::nic_registry` gains a non-singleton `Vec<RemoteNic>` (today it carries one). Routing chooses the default interface; multi-NIC routing tables are explicitly out of scope (post-1.0).
- **E.2** — Add `e1000e.conf`, `igb.conf`, `igc.conf`, `r8169.conf`, `r8125.conf` to `kernel/initrd/etc/services.d/` so `session_manager` probes all five at boot and the first one to match wins.

## Important Components and How They Work

### Ring 3 NIC driver lifecycle

Same shape as the existing Phase 55b e1000 driver: open a device handle through `sys_device_pci_probe`, map BAR0 via `iommu_map_bar`, allocate TX/RX rings via `DmaBuffer<T>` so the IOMMU constrains DMA targets, register IRQ, then process completions in a small per-driver event loop. The kernel-side `RemoteNic` façade is unchanged — the kernel TCP/IP stack does not care which NIC silicon is at the other end.

### Intel vs Realtek descriptor formats

Intel's e1000-family descriptor is a 16-byte struct with `buffer_address`, `length`, and a packed status/control word. Realtek's r8169 descriptor is also 16 bytes but lays the same fields out in a different order with different bit positions. The shared core decides which descriptor format the wrapper crate emits at compile time.

### Why no multi-queue at 1.0

Modern NICs support 8–16 RX queues steered by RSS hash buckets. m3OS's kernel TCP/IP today processes RX from a single queue without contention; adding multiqueue requires both the kernel side (per-queue WaitQueues) and the userspace side (per-queue rings + IRQ vectors). Phase 74a §7 lists multi-queue NVMe and MSI-X per-core steering as "optional pre-1.0, deferred unless time permits" — the same logic applies here.

## How This Builds on Earlier Phases

- Reuses the Phase 55b ring-3 driver-host primitives unchanged.
- Reuses Phase 67's IOMMU `DmaBuffer<T>` for safe DMA — every new driver here is sandboxed by an IOMMU domain.
- Extends the existing Phase 55b e1000 driver pattern as a template — the new drivers share the Phase 55c bound-notification multiplexing and EAGAIN-on-restart contract.
- Lifts the `RemoteNic`-singleton assumption to a small `Vec`.

## Implementation Outline

1. Bring up e1000e against QEMU's `-device e1000e` emulation; verify packet flow over the existing kernel TCP/IP stack.
2. Bring up igc next (modern Intel desktop boards are the most likely 1.0 target).
3. Bring up r8169 against a real card — QEMU's `rtl8139` is not r8169-compatible; this requires a bare-metal validation step.
4. Bring up r8125 (2.5G) against a real card if available; otherwise structurally complete it and ship the validation as a Phase 83 acceptance item.
5. Bring up igb last — server-board focus is lowest priority for the consumer-laptop 1.0 target.
6. Add per-driver `.conf` files to the initrd.
7. Bump kernel to `0.79.0`.

## Acceptance Criteria

- `cargo xtask run` under `-device e1000e` boots, gets a DHCP lease, and runs `ping` successfully.
- A new `cargo xtask multi-nic-smoke` gate exercises each driver in turn against its corresponding QEMU emulation (where available).
- On the dev laptop (if it has a wired Intel NIC): m3OS pulls an IPv4 lease via DHCP and can `curl http://...` an HTTP endpoint on the LAN.
- The Phase 77 TCP retransmission fix is the prerequisite — without it the new drivers will appear to "almost work" then hang on the first dropped packet.
- No regression in the existing 82540EM driver — both old and new e1000 paths coexist; QEMU's `-device e1000-82540em` still works.
- Kernel bumped to `0.79.0`.

## Companion Task List

- [Phase 79 Task List](./tasks/79-modern-nic-tasks.md) — to be authored when implementation planning begins.

## How Real OS Implementations Differ

- Linux ships drivers for every NIC silicon ever made (literally hundreds of PCI IDs), with a maintenance burden proportional to the silicon count. m3OS ships five families and explicitly defers the rest.
- Real OSes implement TSO/GSO/GRO/LRO offload paths to extract every bit of throughput; m3OS does no offload at 1.0.
- Linux's NIC drivers cooperate with NAPI for IRQ-mitigation polling; m3OS drives a one-IRQ-per-packet path at 1.0.
- Real OSes support PHY power management, EEE, WoL, multi-queue RSS, FlowDirector, eBPF/XDP fast paths — all deferred.

## Deferred Until Later

- Broadcom / Marvell / Aquantia / Mellanox / Solarflare wired NICs — post-1.0
- TSO/GSO/GRO/LRO offload — post-1.0
- Multi-queue RSS — post-1.0
- WoL / EEE / PHY power management — post-1.0
- Wi-Fi — Phase 81
- Bonding / VLAN / bridging — post-1.0
