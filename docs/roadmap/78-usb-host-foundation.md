# Phase 78 - USB Host Foundation (xHCI + Hub + HID)

**Status:** Planned — umbrella theme, delivered as sub-phases [78a](./78a-xhci-host-bringup.md) → [78b](./78b-usb-enumeration-hub.md) → [78c](./78c-usb-hid-and-release.md)
**Source Ref:** phase-78
**Depends on:** Phase 55b (Ring-3 Driver Host) ✅, Phase 55c (Ring-3 Driver Correctness Closure) ✅, Phase 67 (IOMMU Substrate Completion) ✅, Phase 56 (Display and Input Architecture) ✅, Phase 74 (IPC Capability Grants / page-grant bulk transport) ✅
**Builds on:** Extends the Phase 55b/55c ring-3 driver-host substrate with the first USB stack the project ships — a userspace xHCI host driver, a minimal USB core, and a HID class driver capable of presenting at least one keyboard and one mouse as the same `kbd_server` / `mouse_server` clients Phase 56 already feeds
**Primary Components:** `userspace/drivers/xhci/` (new), `userspace/lib/usb-core/` (new, shared lib re-exporting `kernel-core/src/usb/`), `userspace/drivers/usbhub/` (new), `userspace/drivers/usb-hid/` (new), `kernel/src/syscall/device_host.rs` (the existing Phase 55b `sys_device_*` primitives; USB-IRQ rides `sys_device_irq_subscribe`), `kernel-core/src/usb/` (new — host-testable framing/parse/state-machine logic), `kernel/src/mm/dma.rs` + `kernel-core/src/iommu/` (existing Phase 67 `DmaBuffer<T>` + IOMMU domain wiring)

> **Review note (source-verified 2026-05-30):** Before the companion task list was authored, this design doc was checked against `main` and against Redox `xhcid` / the Intel xHCI 1.2b spec / iPXE / USB-HID 1.11. Six items below drifted and are corrected in place; the full per-item record is in the [78a task list](./tasks/78a-xhci-host-bringup-tasks.md) Review note (and carried into the 78b/78c task lists). In summary: (1) the device-host syscalls are `sys_device_claim` / `sys_device_mmio_map` / `sys_device_dma_alloc` / `sys_device_irq_subscribe` (all in `kernel/src/syscall/device_host.rs`), **not** `sys_device_pci_probe` / `iommu_map_bar` / `sys_device_irq_bind`; (2) `sys_device_claim` takes a BDF only — there is **no** class-code filter, so controller discovery is a new requirement (Track A/D); (3) ring-3 drivers must be staged under `/drivers/` via `DRIVERS_ENTRIES`, gated by `is_authorized_driver_process`; (4) the Track A scope below was missing most of the mandatory xHCI bring-up (register-region discovery, BIOS/OS handoff, `CNR` reset wait, `CONFIG.MaxSlotsEn`, DCBAA, scratchpad, Event Ring + ERST, MSI-X interrupter, context-size selection, Enable Slot / Configure Endpoint / Evaluate Context, Link + Event TRBs, cycle bit + doorbell, PORTSC reset) — now enumerated; (5) `KeyEvent`/`PointerEvent` already exist with stable 20-/37-byte codecs in `kernel-core/src/input/events.rs`; (6) HID Boot needs `SET_IDLE` and an interrupt-IN endpoint configured via Configure Endpoint and polled with Normal TRBs.

## Sub-Phase Breakdown

A full xHCI + USB-core + hub + HID stack is materially larger than a single phase, so Phase 78 ships as **three sequenced sub-phases**, mirroring the Phase 76 → 76b/76c/76d pattern. Each is an independently runnable milestone with its own design doc, task list, and patch-level version bump; the kernel reaches the `0.78.x` line across the three. This umbrella doc holds the theme-level milestone, learning goals, and deferrals; the implementable tracks live in the sub-phase docs.

| Sub-phase | Theme | Milestone | Version | Design | Tasks |
|---|---|---|---|---|---|
| 78a | xHCI host-controller bring-up | The `xhci` ring-3 driver claims the controller, completes full bring-up, and reaches a first `Enable Slot` Command Completion event off the event ring via MSI-X | `0.78.0` | [78a](./78a-xhci-host-bringup.md) | [Tasks](./tasks/78a-xhci-host-bringup-tasks.md) |
| 78b | USB enumeration + hub | Descriptor walk + `Address Device` + `SET_CONFIGURATION` + Configure Endpoint + hub class + multi-controller PCI enumeration; the full device tree enumerates and prints on boot | `0.78.1` | [78b](./78b-usb-enumeration-hub.md) | [Tasks](./tasks/78b-usb-enumeration-hub-tasks.md) |
| 78c | USB HID + integration + release | Boot keyboard + mouse → `kbd_server`/`mouse_server`; full `usb-smoke` QMP gate; learning doc; the `0.78.2` capability cut | `0.78.2` | [78c](./78c-usb-hid-and-release.md) | [Tasks](./tasks/78c-usb-hid-and-release-tasks.md) |

The Feature Scope tracks below map to these sub-phases: **Track A → 78a**; **Track B → 78b**; **Track C → 78c**; **Track D is split** (Bus Master Enable / MSI-X programming + `xhci` hosting in 78a, the multi-controller PCI class enumeration in 78b, the `usb-hid` wiring + full `usb-smoke` gate in 78c); **Track E → 78c**. The original single-phase task list was superseded by the three sub-phase task lists on 2026-05-30.

## Milestone Goal

m3OS boots on a modern x86_64 laptop or desktop without a PS/2 port and finds at least one USB keyboard and one USB mouse via xHCI enumeration. Both devices feed `kbd_server` / `mouse_server` through the same wire format Phase 56 introduced for PS/2 input. This is the single biggest 1.0 unblocker per Phase 74a §3.

## Why This Phase Exists

Phase 74a §3 documents the hard truth: m3OS today has zero bytes of USB code anywhere in the kernel or userspace. Every modern laptop has zero PS/2 ports. On the dev laptop (HP OmniBook with Strix Halo), m3OS would boot, paint a framebuffer, and then sit at a black screen with no keyboard or mouse. A 1.0 release without USB-HID is not a real-hardware 1.0 release.

xHCI is the only host-controller standard worth implementing — EHCI and UHCI are obsolete, OHCI never mattered on x86. xHCI handles USB 1.1/2.0/3.x devices on the same controller, so one driver covers the whole legacy + modern device space.

## Learning Goals

- Understand how a USB host controller schedules transfers via TRB rings (Transfer Request Blocks)
- Learn how USB enumeration walks the descriptor tree: device → configuration → interface → endpoint
- See how HID report descriptors are parsed (input items, usage pages, usage IDs) and how the keyboard/mouse boot protocols simplify the parsing problem
- Understand why a hub is its own USB device that the host driver must enumerate before downstream devices are reachable
- Learn how the IOMMU substrate from Phase 67 enables a ring-3 USB driver to issue DMA transfers safely

## Feature Scope

### Track A — xHCI host controller driver (ring 3) → sub-phase 78a

(Sub-IDs A.1–A.7 align 1:1 with the [78a task list](./tasks/78a-xhci-host-bringup-tasks.md).)

- **A.1** — `xhci` driver crate scaffold. Claim the controller via `sys_device_claim` (a known/sentinel BDF for `qemu-xhci`, as NVMe/e1000 do) and map BAR0 via `sys_device_mmio_map`. (Class-code discovery — there is no `sys_device_claim` class filter — is Track D.1.)
- **A.2** — Register-region discovery: compute Operational/Runtime/Doorbell bases from `CAPLENGTH`/`RTSOFF`/`DBOFF`; decode `HCSPARAMS1`/`HCSPARAMS2`/`HCCPARAMS1` (incl. the `CSZ` 32-vs-64-byte context bit, which sizes every later context).
- **A.3** — Bring-up sequence: BIOS/OS handoff via the `USBLEGSUP` extended capability; controller reset (`USBCMD.HCRST`, poll `USBSTS.CNR=0`) before any `CONFIG`/`DCBAAP`/`CRCR` write; set `CONFIG.MaxSlotsEn`; run the ordered init (`CONFIG`→`DCBAAP`→scratchpad→`CRCR`→`ERSTSZ`/`ERSTBA`/`ERDP`→MSI-X→**Bus Master Enable confirmed (D.2)**) and only then set `USBCMD.R/S`.
- **A.4** — DMA structures via `DmaBuffer<T>` (IOMMU-routed, IOVA programmed into hardware): the **DCBAA** (+`DCBAAP`), the **Scratchpad Buffer Array** into `DCBAA[0]` when `HCSPARAMS2` requires it, and the Input/Slot/Endpoint contexts (sized per `CSZ`).
- **A.5** — TRB ring machinery: the **Command Ring** (+`CRCR`, Link TRB, RCS), per-endpoint Transfer Rings, and the **Event Ring** + **ERST** (`ERSTSZ`→`ERSTBA`→`ERDP`); Normal/Setup/Data/Status/**Link** TRBs; consume the **Command Completion**, **Transfer**, and **Port Status Change** event TRBs; producer cycle bit, event-ring **consumer** cycle state + `ERDP`/`EHB`, the **DCI** doorbell formula (EP0=DCI 1, `epN = 2N + IN`; Doorbell 0 = commands).
- **A.6** — MSI-X interrupter (`IMAN.IE`/`IMOD`) via `sys_device_irq_subscribe`, plus a **single-threaded drain-on-wake event loop** (the NVMe model — no userspace thread primitive exists) that drains the event ring on the `Notification` and wakes the matching outstanding request (no busy-poll). Legacy INTx is unreliable on xHCI — MSI-X is the supported path.
- **A.7** — PORTSC port reset + speed detection (RW1C-safe writes; `CSC` edge vs `CCS` level); USB2 ports need an explicit `PR` write, USB3 trains automatically. Detected speed selects EP0 Max Packet Size (Low 8 / Full 8 / High 64 / **SuperSpeed 512**).

### Track B — USB core (host-testable in `kernel-core`, shared via `usb-core` lib) → sub-phase 78b

- **B.1** — Descriptor model + parser in `kernel-core/src/usb/`: `GET_DESCRIPTOR(DEVICE)`, then `GET_DESCRIPTOR(CONFIG)` short-then-full by `wTotalLength`, walking interfaces/endpoints. Host-tested against captured blobs.
- **B.2** — Enumeration state machine: **Enable Slot** → **Address Device** (BSR two-step for full-speed EP0 Max Packet Size) → descriptor walk → **SET_CONFIGURATION** → **Configure Endpoint** (and **Evaluate Context** to correct EP0 packet size). xHCI's Address Device replaces the raw USB `SET_ADDRESS`.
- **B.3** — Hub class support (`bDeviceClass 0x09`): `SetPortFeature(PORT_POWER)`, reset and walk downstream ports; `PortId` topology (root-port + hub-depth + parent) for nested hubs. Runs as the `usbhub` ring-3 driver (Redox `usbhubd` model).
- **B.4** — Host↔class IPC protocol crate (`usb-core`): descriptors/setup as IPC payloads, transfer buffers as Phase 74 page grants; a thin client API (m3OS analogue of Redox `XhciClientHandle`).

### Track C — HID class driver (ring 3) → sub-phase 78c

- **C.1** — Boot Protocol keyboard. `SET_PROTOCOL(0)` + `SET_IDLE(0)`; poll the interrupt-IN endpoint (configured via Configure Endpoint) with Normal TRBs at `bInterval`; first 8 bytes of the report → HID Usage IDs → `KeyEvent` via the existing `kernel-core` 20-byte codec.
- **C.2** — Boot Protocol mouse. First 3 bytes of the report (accept ≥3, ignore trailing wheel bytes) → relative `dx`/`dy` + button bitfield → `PointerEvent` via the existing 37-byte codec.
- **C.3** — Inject into `kbd_server`/`mouse_server` so USB becomes an additional **producer** merged into the same pull stream `display_server` already drains — the Phase 56 dispatcher and `InputWiring` are unchanged. (This requires a bounded pending-event queue in those servers, which are synchronous pull loops today.)
- **C.4** — Report Protocol skeleton (parse HID report descriptor, derive field offsets) — host-tested only, not wired to a device. Boot Protocol is enough for 1.0 keyboards and mice; Report Protocol unlocks touchpads, gaming mice, multi-touch displays — deferred unless time permits.

### Track D — Kernel-side wiring → split across 78a (BME/MSI-X + `xhci` hosting), 78b (D.1), 78c (`usb-hid` wiring + `usb-smoke`)

- **D.1** — Controller discovery. `sys_device_claim` takes a BDF only (no class filter), so a committed capability-gated `sys_device_pci_enumerate(class, subclass, prog_if)` (built on the existing in-kernel `PciMatch::ClassSubclass`) returns every class-`0x0C0330` BDF — required for the dev laptop's six controllers. The `qemu-xhci` sentinel BDF is an interim bootstrap only. IRQ delivery rides the existing `sys_device_irq_subscribe` + `register_device_irq` path — no new IRQ syscall.
- **D.2** — PCI enablement: ensure `sys_device_claim` sets **Bus Master Enable + Memory Space** (Command reg `0x04`) for the claimed device (the ring-3 claim path does not today — only the in-kernel virtio drivers do), and that `sys_device_irq_subscribe` programs the xHCI controller's PCI MSI-X table + enable bit (not just a kernel vector).
- **D.3** — Build/ramdisk/service wiring: stage the `xhci`/`usbhub`/`usb-hid` driver binaries under `DRIVERS_ENTRIES` (`/drivers/`, so the `is_authorized_driver_process` gate passes), add `usb-core` as a **library** member (no service — enumeration runs inside the `xhci` host, the Redox model), and add `xhci.conf` + `usbhub.conf` + `usb-hid.conf` to `kernel/initrd/etc/services.d/` and `init` `KNOWN_CONFIGS` so `session_manager` starts the stack (as static daemons) before `greeter`.
- **D.4** — `usb-smoke` QMP gate (QEMU `-device qemu-xhci -device usb-kbd -device usb-mouse`): assert a real `Enable Slot` Command Completion event, then inject a QMP `send-key`, observe the resulting boot-keyboard Transfer event, and confirm the keystroke reaches the prompt — not a `[xhci] N ports detected` serial sentinel.

### Track E — Documentation + release → sub-phase 78c

- **E.1** — Phase 78 learning doc (`docs/78-usb-host-foundation.md`), authored at the 78c close.
- **E.2** — Kernel cut to `0.78.2` and the new `AGENTS.md` "USB host stack" capability-inventory bullet (`kernel/Cargo.toml`, `Cargo.lock`, `AGENTS.md`, `docs/roadmap/README.md`). The `0.78.0`/`0.78.1` bumps in 78a/78b update the version string only; the capability bullet lands at 78c.

## Important Components and How They Work

### TRB rings

xHCI uses three kinds of ring: a single Command Ring (host → controller), a single Event Ring (controller → host) shared across slots and described by an Event Ring Segment Table (ERST), and one Transfer Ring per endpoint per slot. Each ring is a contiguous region carrying 16-byte TRBs with a producer/consumer **cycle bit**; the Command and Transfer rings end in a **Link TRB** that loops to the start and toggles the cycle, while the Event Ring wraps per its ERST segment sizes and has no Link TRB. Software enqueues a TRB, then rings a **doorbell** (Doorbell 0 for the Command Ring; the slot's doorbell with the endpoint DCI for a Transfer Ring) to tell the controller there is new work. DMA is the only way data crosses the ring 3 / hardware boundary — every ring, the DCBAA, the scratchpad pages, and each device context are allocated as IOMMU-routed `DmaBuffer<T>`s (Phase 67) and the controller is programmed with their **IOVA**, so a compromised driver can only DMA into the pages its per-device VT-d/AMD-Vi domain grants. The Event Ring is the only channel by which the controller reports Command Completion, Transfer, and Port Status Change events.

### USB enumeration

When a port reports Connect Status Change (via a Port Status Change event), the host driver resets the port (PORTSC), reads the speed, then issues **Enable Slot** to get a slot ID, allocates an Output Device Context into `DCBAA[slot]`, and runs **Address Device** (replacing the raw USB `SET_ADDRESS`; a BSR=1 pre-read first discovers full-speed EP0 Max Packet Size). It then walks the descriptor tree over EP0 control transfers — `GET_DESCRIPTOR(DEVICE)`, `GET_DESCRIPTOR(CONFIG)` short-then-full — chooses a configuration with `SET_CONFIGURATION`, and brings the interrupt-IN endpoint into the controller with **Configure Endpoint**. The HID class driver matches `bInterfaceClass == 0x03` (HID); the hub class driver matches `bDeviceClass == 0x09` (HUB).

### Boot Protocol HID parsing

For an interface with `bInterfaceClass 0x03` / `bInterfaceSubClass 0x01` (Boot) / `bInterfaceProtocol 0x01` (keyboard) or `0x02` (mouse), the driver issues `SET_PROTOCOL(0)` (fixed boot reports, no report-descriptor parsing) and `SET_IDLE(0)` (suppress duplicate/streamed reports), then polls the interrupt-IN endpoint with Normal TRBs at the descriptor's `bInterval`. Keyboard reports are 8 bytes (1 modifier byte + 1 reserved byte + 6 keycode bytes, all USB HID Usage IDs, with a rollover code); mouse reports are 3 bytes (1 button-bitfield byte + signed `dx` byte + signed `dy` byte). The driver translates HID Usage IDs to `KeyEvent`s (and the 3-byte reports to `PointerEvent`s) and injects them into `kbd_server`/`mouse_server`, which merge them with PS/2 into the same pull stream `display_server` already drains — so the Phase 56 dispatcher is untouched. This is enough for keyboard + mouse boot input on essentially every USB HID device in existence.

## How This Builds on Earlier Phases

- Reuses the Phase 55b ring-3 driver-host primitives (`sys_device_claim`, `sys_device_mmio_map`, `sys_device_dma_alloc`, `sys_device_irq_subscribe`, `sys_device_pio_read`/`pio_write`) without modification — the only possible new kernel surface is the Track D.1 PCI class-enumeration path for multi-controller hardware.
- Reuses Phase 67's IOMMU-routed `DmaBuffer<T>` for safe DMA on every controller-visible structure — kernel-side `bus_address()` (`kernel/src/mm/dma.rs`) and the ring-3 `.iova()` accessor (`userspace/lib/driver_runtime/src/dma.rs`); BAR mapping is via `sys_device_mmio_map`.
- Reuses Phase 56's `KeyEvent` / `PointerEvent` wire formats (`kernel-core/src/input/events.rs`, 20-/37-byte codecs) — `usb-hid` becomes an additional **producer** that injects into `kbd_server`/`mouse_server` exactly alongside the PS/2 stream, leaving the dispatcher unchanged.
- Slots into the `session_manager` start sequence (`DECLARED_SESSION_STEP_NAMES`, `kernel-core/src/session_supervisor.rs:89`) so the USB input stack is ready before `greeter`; mirrors the NVMe/e1000 supervised ring-3 driver lifecycle and restart contract.

## Implementation Outline

1. Bring up the `xhci` driver: claim the `qemu-xhci` controller, map BAR0, discover the register regions + capabilities, do the BIOS handoff + `HCRST`/`CNR` reset, set `CONFIG.MaxSlotsEn`, allocate DCBAA + scratchpad + command ring + event ring/ERST, wire the MSI-X interrupter, set `RUN`, and reach a first `Enable Slot` Command Completion event. Print `[xhci] N ports detected`.
2. Add USB core + descriptor walker + the enumeration state machine (Enable Slot → Address Device BSR → descriptors → SET_CONFIGURATION → Configure Endpoint); print the full enumerated tree on boot.
3. Add hub class (`usbhub`) so downstream devices on the laptop's six controllers come up.
4. Add HID class with Boot Protocol (`SET_PROTOCOL(0)` + `SET_IDLE(0)` + interrupt-IN polling); verify keystrokes from a USB keyboard reach `kbd_server` and the login prompt.
5. Add Boot Protocol mouse; verify pointer movement reaches `mouse_server`.
6. Stage the drivers under `/drivers/` (`DRIVERS_ENTRIES`), wire the three new services into `session_manager` and `init` `KNOWN_CONFIGS`, and add the `usb-smoke` QMP gate.
7. Author the learning doc and cut kernel `0.78.2` with the new USB capability entry (the closing cut of the three-sub-phase theme).

## Acceptance Criteria

- `cargo xtask run` under `qemu-xhci` emulation (`-device qemu-xhci -device usb-kbd -device usb-mouse`) enumerates a virtual USB keyboard and feeds keystrokes to the m3OS login prompt.
- A new `cargo xtask usb-smoke` gate verifies the full chain, asserting a real `Enable Slot` Command Completion event, a real boot-keyboard Transfer event, and a QMP `send-key` keystroke reaching the prompt (QMP `screendump`/serial echo) — not just a `[xhci] N ports detected` serial sentinel. Opt-in pre-push gate `M3OS_USB_REGRESSION=1`.
- The pure-logic layer (register decoders, TRB/cycle-bit, context layouts for both `CSZ` sizes, PORTSC bits, descriptor parser, enumeration state machine, HID report/usage decoders) is host-tested in `kernel-core/src/usb/`.
- On the dev laptop (HP OmniBook, Strix Halo): a USB keyboard plugged into any of the six xHCI controllers types into the m3OS shell (requires the Track D.1 class-enumeration path).
- `display_server`-aware: keystrokes route through the focus-aware dispatcher introduced in Phase 56 (no change to the dispatcher or `display_server` `InputWiring` — `usb-hid` injects into `kbd_server`/`mouse_server`).
- No regression in PS/2 input — both producers coexist; PS/2 still works for QEMU's i8042 emulation.
- Kernel reaches `0.78.2` across the three sub-phases (`0.78.0` at 78a, `0.78.1` at 78b, `0.78.2` at 78c).

## Companion Task List

Phase 78 is delivered as three sub-phases, each with its own implementation-ready task list (source-verified against `main` + Redox `xhcid` + the xHCI 1.2b spec, 2026-05-30):

- [Phase 78a Task List](./tasks/78a-xhci-host-bringup-tasks.md) — xHCI host-controller bring-up
- [Phase 78b Task List](./tasks/78b-usb-enumeration-hub-tasks.md) — USB enumeration + hub
- [Phase 78c Task List](./tasks/78c-usb-hid-and-release-tasks.md) — HID + integration + release

## How Real OS Implementations Differ

- Linux ships drivers for EHCI, UHCI, OHCI, xHCI plus dozens of optional class drivers (mass-storage, CDC-ACM, audio, video, MTP). m3OS at 1.0 supports xHCI + Hub + HID only.
- Linux's USB stack handles hot-plug events through a userspace-visible netlink interface; m3OS at 1.0 enumerates once at boot and does not surface hot-plug.
- Linux's HID stack has thousands of quirk entries for misbehaving devices; m3OS at 1.0 ships zero quirks and accepts whatever the Boot Protocol returns.
- Real OSes implement USB-3 SuperSpeed and SuperSpeedPlus link training paths; m3OS at 1.0 uses xHCI's controller-managed link training but does not expose link-state policy.

## Deferred Until Later

- USB mass storage (would let USB sticks work — meaningful but post-1.0)
- USB audio (HDA in Phase 80 is the 1.0 audio bet)
- USB video (UVC webcams — post-1.0)
- Hot-plug event surface to userspace
- USB Report Protocol parsing (multi-touch, touchpads, gaming mice)
- USB-C / Power Delivery / DisplayPort alternate mode
- xHCI debug capability (DbC) for kernel debug-over-USB
