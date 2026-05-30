# Phase 78a - USB Host Foundation: xHCI Host-Controller Bring-Up

**Status:** Complete
**Source Ref:** phase-78a
**Depends on:** Phase 55b (Ring-3 Driver Host) ✅, Phase 55c (Ring-3 Driver Correctness Closure) ✅, Phase 67 (IOMMU Substrate Completion) ✅
**Builds on:** First sub-phase of the [Phase 78](./78-usb-host-foundation.md) USB theme. Extends the Phase 55b/55c ring-3 driver-host substrate with a userspace xHCI host-controller driver that claims the controller, completes the full xHCI bring-up sequence, and reaches a first command completion off the event ring via MSI-X. Sub-phases 78b (enumeration + hub) and 78c (HID + release) build on the live controller this phase produces.
**Primary Components:** `userspace/drivers/xhci/` (new), `kernel-core/src/usb/xhci/` (new — host-testable register/TRB/context/PORTSC logic), `kernel/src/syscall/device_host.rs` (Bus Master Enable + MSI-X programming on the existing `sys_device_*` primitives), `kernel/src/mm/dma.rs` (existing Phase 67 `DmaBuffer<T>`)

## Milestone Goal

The `xhci` ring-3 driver claims the QEMU `qemu-xhci` controller, maps BAR0, discovers the register regions, performs the BIOS/OS handoff and controller reset, programs the DCBAA + scratchpad + command ring + event ring (ERST), wires an MSI-X interrupter, sets the controller running, and reaches a first `Enable Slot` Command Completion event delivered off the event ring **by interrupt** — proving the controller is fully alive. No device enumeration yet; that is 78b.

## Why This Phase Exists

A USB stack is a tall tower: HID input (the actual 1.0 goal) sits on enumeration, which sits on a working host controller. The host controller is the single largest and riskiest chunk — register-region discovery, the CNR reset handshake, the DCBAA/scratchpad/context model, the command/event TRB rings with their cycle-bit and doorbell protocol, and MSI-X interrupt delivery. Splitting it into its own sub-phase means the "is the controller alive?" question is answered (and regression-gated) before any enumeration or class-driver code is written. It also surfaces the two kernel-side prerequisites — **PCI Bus Master Enable** and **MSI-X table programming** — that the ring-3 claim path does not provide today, where they can be fixed in isolation.

## Learning Goals

- Understand how an xHCI controller's MMIO is split into Capability/Operational/Runtime/Doorbell regions discovered by offset chaining (`CAPLENGTH`/`RTSOFF`/`DBOFF`), not fixed addresses.
- Learn the mandatory bring-up handshake: BIOS/OS handoff, `HCRST` reset, the `CNR` wait, and why operational registers are ignored until it clears.
- See how the DCBAA, scratchpad buffers, and slot/endpoint contexts model a device to the controller, and why context size depends on `HCCPARAMS1.CSZ`.
- Understand the producer/consumer TRB ring with its cycle bit, the Event Ring Segment Table, and how a doorbell tells the controller there is new work.
- Learn why a pure bus-master device needs PCI Bus Master Enable and an enabled MSI-X interrupter before it will DMA or post a single event.

## Feature Scope

### Track A — xHCI host controller driver (ring 3)

The `xhci` driver crate: claim + BAR map, register-region discovery + capability parse (incl. `CSZ`), BIOS/OS handoff + reset + ordered run sequence, DCBAA + scratchpad + contexts, command ring + event ring + ERST + TRB machinery (cycle bit, Link TRB, doorbell/DCI), MSI-X interrupter + single-threaded drain-on-wake event loop, and PORTSC reset + speed detection. The pure-logic layer (register decoders, TRB encode/decode + cycle bit, context layouts for both `CSZ` sizes, PORTSC bits) is host-tested in `kernel-core/src/usb/xhci/`.

### Track B — Kernel/PCI enablement + driver hosting

The two kernel-side prerequisites this controller needs to run: ensure `sys_device_claim` sets **Bus Master Enable + Memory Space** (the ring-3 claim path does not today), and ensure `sys_device_irq_subscribe` actually programs the controller's **MSI-X table + enable bit**. Plus the build/ramdisk/service wiring to stage the `xhci` driver under `/drivers/` and start it.

### Track C — Bring-up smoke gate + version bump

A headless `xhci-bringup-smoke` gate that boots with `-device qemu-xhci` and asserts a real `Enable Slot` Command Completion event arrives via the event ring + interrupter (not a "ports detected" serial sentinel). Kernel version bumped to `0.78.0`.

## Important Components and How They Work

### Register-region discovery

The xHCI BAR exposes four register banks located by offset chaining: Operational at `BAR + CAPLENGTH`, Runtime at `BAR + RTSOFF`, Doorbell at `BAR + DBOFF`. `HCSPARAMS1` gives MaxSlots/MaxIntrs/MaxPorts, `HCSPARAMS2` gives Max Scratchpad Buffers, and `HCCPARAMS1.CSZ` selects 32- vs 64-byte contexts — which changes every later structure layout, so it must be read before any context is allocated.

### TRB rings and the event ring

A single Command Ring (host → controller) and a single Event Ring (controller → host, described by an ERST) carry 16-byte TRBs with a producer/consumer cycle bit. The Command Ring ends in a Link TRB that toggles the cycle; the Event Ring wraps per its ERST segment sizes with no Link TRB. Software enqueues a TRB then rings a doorbell (Doorbell 0 for commands). Every ring, the DCBAA, the scratchpad pages, and each context are IOMMU-routed `DmaBuffer<T>`s programmed by their IOVA, so the controller can only DMA into granted pages.

### Bus Master Enable + MSI-X (the silent-failure prerequisites)

xHCI is a pure bus-master DMA device — it DMAs nothing and posts zero events until PCI Bus Master Enable (Command reg `0x04` bit 2) is set, and no IRQ fires until an MSI-X vector is wired and `IMAN.IE` is enabled. Both are kernel-side and neither is provided by the ring-3 claim path today, so Track B fills them.

## How This Builds on Earlier Phases

- Reuses the Phase 55b ring-3 driver-host primitives (`sys_device_claim`, `sys_device_mmio_map`, `sys_device_dma_alloc`, `sys_device_irq_subscribe`) and the `userspace/lib/driver_runtime` HAL exactly as NVMe/e1000 do.
- Reuses Phase 67's IOMMU-routed `DmaBuffer<T>` for every controller-visible structure, programming the IOVA (not raw physical) into hardware.
- Adds two small kernel-side enablements (BME, MSI-X table programming) to the existing device-host syscalls without new policy in ring 0.

## Implementation Outline

1. Scaffold the `xhci` crate; claim `qemu-xhci` at its sentinel BDF; map BAR0; print `[xhci] N ports detected`.
2. Discover register regions + capabilities (`CAPLENGTH`/`RTSOFF`/`DBOFF`, `HCSPARAMS1/2`, `HCCPARAMS1.CSZ`).
3. BIOS/OS handoff (`USBLEGSUP`); reset (`HCRST` + `CNR` wait); `CONFIG.MaxSlotsEn`.
4. Allocate DCBAA + scratchpad + contexts; command ring + event ring + ERST.
5. Wire the MSI-X interrupter; ensure BME is on; set `USBCMD.R/S`.
6. Issue `Enable Slot`, ring Doorbell 0, and consume its Command Completion event via the interrupt-driven event loop.
7. Add the `xhci-bringup-smoke` gate; bump kernel to `0.78.0`.

## Acceptance Criteria

- `cargo xtask run` with `-device qemu-xhci` brings the `xhci` driver to a running controller and consumes a real `Enable Slot` Command Completion event off the event ring via MSI-X.
- A `cargo xtask xhci-bringup-smoke` gate asserts that completion event (opt-in pre-push `M3OS_USB_REGRESSION=1`); a `[xhci] N ports detected` serial sentinel alone is not sufficient.
- `sys_device_claim` enables PCI Bus Master Enable + Memory Space; `sys_device_irq_subscribe` programs the controller's MSI-X table + enable bit (both verified by read-back / a live interrupt).
- The pure-logic layer (register decoders, TRB/cycle-bit, context layouts for both `CSZ` sizes, PORTSC bits) is host-tested in `kernel-core/src/usb/xhci/`.
- Kernel bumped to `0.78.0`.

## Companion Task List

- [Phase 78a Task List](./tasks/78a-xhci-host-bringup-tasks.md)

## How Real OS Implementations Differ

- Linux/iPXE perform the same bring-up sequence but support many controllers and quirk tables; 78a targets `qemu-xhci` plus the dev-laptop controllers via a sentinel BDF (multi-controller discovery is 78b).
- Real drivers hand the controller raw physical addresses; m3OS routes every structure through a per-device IOMMU domain.
- Production drivers use multiple interrupters / interrupt moderation tuning; 78a uses a single interrupter with a basic `IMOD`.

## Deferred Until Later

- Device enumeration, descriptor parsing, address assignment, Configure Endpoint (Phase 78b)
- Hub class + multi-controller PCI class enumeration (Phase 78b)
- HID class driver + input integration + the full `usb-smoke` keystroke gate (Phase 78c)
- Hot-plug event surface, USB mass storage / audio / video (post-1.0)
