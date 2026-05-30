# USB Host Foundation (Phase 78)

**Aligned Roadmap Phase:** Phase 78
**Status:** Complete
**Source Ref:** phase-78
**Supersedes Legacy Doc:** new

## Overview

Phase 78 delivers m3OS's first USB stack: a ring-3 xHCI host-controller driver,
a shared USB core library, a hub class driver, and a HID class driver that turns
USB keyboard and mouse input into the same event stream Phase 56 uses for PS/2.
The phase ships as three sequenced sub-phases (78a bring-up, 78b
enumeration + hub, 78c HID + integration) and closes with kernel `0.78.2`.

The central lesson is **why USB-HID is the 1.0 real-hardware unblocker**. Every
modern laptop — including the HP OmniBook (Strix Halo) used as the m3OS dev
machine — has zero PS/2 ports. Without USB, m3OS boots, paints a framebuffer,
and then sits at a black screen with no keyboard or mouse. USB-HID in Boot
Protocol is the only path to real-hardware input at 1.0.

The central safety lesson is **how a ring-3 driver issues DMA transfers without
holding raw physical-address authority**. The Phase 67 IOMMU substrate gives
each driver an IOVA domain. `DmaBuffer<T>` allocates a typed, contiguous region
visible to hardware; `iova()` returns the IOVA that the xHCI controller is
programmed with, never a raw pointer into physical memory. A compromised driver
can only DMA into the pages its per-device VT-d/AMD-Vi domain grants.

## What This Doc Covers

- **TRB rings** — the 16-byte Transfer Request Blocks the host and controller
  exchange, and the three ring types (Command, Event, Transfer).
- **Event ring + interrupter** — why IRQ delivery (MSI-X), not polling, signals
  transfer completion, and how the Event Ring Segment Table (ERST) describes the
  shared ring to the controller.
- **The full bring-up order (78a)** — BIOS/OS handoff, `HCRST` + `CNR` wait,
  `MaxSlotsEn`, DCBAA + scratchpad, command ring, event ring, MSI-X interrupter,
  `RUN`, and reaching the first `Enable Slot` Command Completion.
- **Enumeration descriptor walk (78b)** — `Enable Slot` → `Address Device` →
  `GET_DESCRIPTOR` → `SET_CONFIGURATION` → `Configure Endpoint`.
- **HID Boot Protocol (78c)** — the 8-byte keyboard boot report
  (`[modifier][reserved][k0..k5]`) and the ≥3-byte mouse report
  (`[buttons][dx][dy]`), `SET_PROTOCOL(0)` / `SET_IDLE(0)`, and how decoded
  events inject into `kbd_server`/`mouse_server` without changing the Phase 56
  `InputDispatcher`.
- **The `usb-smoke` gate** — why QMP keystroke injection, not a serial sentinel,
  is required to prove the full hardware chain.

## Core Implementation

### TRB rings and DMA safety

xHCI uses three ring types. The **Command Ring** carries host-to-controller
commands (Enable Slot, Address Device, Configure Endpoint). The **Event Ring**
carries controller-to-host notifications (Command Completions, Transfer events,
Port Status Changes). Each device endpoint gets its own **Transfer Ring** for
data movement.

Every ring is a contiguous array of 16-byte TRBs allocated as a
`DmaBuffer<T>` and described to the controller by IOVA. A **cycle bit** in
each TRB, toggled on the Link TRB that wraps the ring, tells the controller
which side owns which entries. When software enqueues a new TRB it rings a
**doorbell** — Doorbell 0 for the Command Ring, the slot's doorbell indexed by
the endpoint's DCI for a Transfer Ring.

The Event Ring is special: the controller is the producer. Its layout is
described by an ERST (Event Ring Segment Table). Software advances the
consumer-side `ERDP` after draining events and clears the `EHB` (Event Handler
Busy) bit. Because the Event Ring is the only path by which the controller
reports completion, all interesting events — including command completions and
HID transfer arrivals — arrive there.

### Controller discovery

The xHCI driver finds its controller via PCI **class enumeration**
(`enumerate_pci_class(0x0C, 0x03, 0x30)`), which returns every
class-`0x0C0330` BDF. On QEMU a **sentinel-BDF fallback** (`0000:00:06.0`) is
used when enumeration returns nothing, matching the pattern established by
the NVMe and e1000 ring-3 drivers.

### Bring-up sequence (78a)

The mandatory bring-up order before any command can be issued:

1. **BIOS/OS handoff** — clear `USBLEGSUP` to take ownership from firmware.
2. **`HCRST` + `CNR` wait** — set `USBCMD.HCRST`; poll `USBSTS.CNR` until
   clear (the controller is not addressable until `CNR` deasserts).
3. **`MaxSlotsEn`** — write `CONFIG.MaxSlotsEn`.
4. **DCBAA + scratchpad** — allocate the Device Context Base Address Array and
   the scratchpad buffer array (if `HCSPARAMS2` requires it); program `DCBAAP`.
5. **Command ring** — allocate + program `CRCR` (incl. `RCS`); install the
   Link TRB.
6. **Event ring + ERST** — allocate the ring and segment table; write
   `ERSTSZ`→`ERSTBA`→`ERDP`.
7. **MSI-X interrupter** — `sys_device_irq_subscribe` programs the controller's
   MSI-X table and enables `IMAN.IE`/`IMOD`.
8. **`RUN`** — set `USBCMD.R/S`. The controller is now live.
9. **Enable Slot command** — enqueue an Enable Slot TRB, ring doorbell 0. The
   Command Completion event arrives **by interrupt** on the event ring; the
   sentinel is `XHCI_BRINGUP:enable-slot:OK`.

Note: xHCI is a pure bus-master device. It DMAs nothing and posts zero events
until PCI **Bus Master Enable** (Command register bit 2) is set. This is handled
by `sys_device_claim` in `kernel/src/syscall/device_host.rs`.

### Enumeration (78b)

`kernel_core::usb::enumerate::run_enumeration` is a host-testable state machine
that drives `UsbHostOps` implemented over real DMA rings
(`userspace/drivers/xhci/src/enumerate.rs`). The state machine steps:

1. Port reports Connect Status Change → port reset → speed detection.
2. **Enable Slot** → slot ID returned in Command Completion event.
3. Allocate Output Device Context into `DCBAA[slot_id]`.
4. **Address Device** (BSR=1 first for full-speed devices to discover EP0
   Max Packet Size; corrected via `Evaluate Context`; BSR=0 assigns the address).
   xHCI's Address Device replaces the raw USB `SET_ADDRESS`.
5. **GET_DESCRIPTOR(DEVICE)** short read → then
   **GET_DESCRIPTOR(CONFIG)** short read by `wTotalLength`, then full read.
   These are EP0 control transfers using Setup/Data/Status stage TRBs.
6. **SET_CONFIGURATION** → **Configure Endpoint** (brings the interrupt-IN
   endpoint into the controller).

The enumeration sentinel on PASS is `XHCI_ENUM:configured`.

### The xHCI IPC server (78c)

After enumeration the driver becomes a live server:

1. Registers the `usb` service.
2. **Binds its IRQ notification into the command endpoint** via `sys_notif_bind`
   (`0x1111`).
3. Runs a single `ipc_recv_msg` loop that wakes on **either** an IPC request
   **or** an IRQ transfer-completion wake (`RECV_KIND_NOTIFICATION = 1`). This
   is the same pattern the e1000 driver uses.

HID Boot-Protocol setup (`SET_PROTOCOL(0)` / `SET_IDLE(0)`) and interrupt-IN
endpoint arming happen **before** binding, so the live loop never blocks on
hardware inside a request handler.

### HID decode and the inject path (78c)

`usb-hid` (`userspace/drivers/usb-hid`) is a separate ring-3 daemon that:

1. Looks up the `usb` service and pulls attached devices via `NextAttach`.
2. Polls the keyboard's interrupt-IN endpoint with `PollInterruptIn` — a Normal
   TRB enqueued into a DMA report buffer. The server's IRQ-drain captures the
   8-byte boot report into a per-endpoint FIFO; `PollInterruptIn` drains from
   it.
3. Decodes the 8-byte boot report `[modifier][reserved][k0..k5]` with
   `kernel_core::usb::hid::BootKeyboardDecoder`, diffing successive reports into
   press/release edges.
4. Resolves the HID Usage ID to a symbol with the same `Keymap` that
   `kbd_server` uses.
5. Injects a `KeyEvent` into `kbd_server` via the `KBD_EVENT_INJECT` (label 5)
   IPC path.

`kbd_server` and `mouse_server` gained a **bounded inject queue** drained into
their existing `*_EVENT_PULL` replies — USB and PS/2 are **parallel producers**,
with USB events draining before the PS/2 stream. The Phase 56 `InputDispatcher`
(`kernel-core/src/input/dispatch.rs`) and `display_server` `InputWiring` are
**unchanged**.

### Why QMP, not a serial sentinel

The `usb-smoke` gate injects a real keystroke over QMP `send-key` into the
emulated QEMU `usb-kbd` device and asserts the decoded `KeyEvent`
(`USB_HID:key kind=0 sym=0x00000061`). A serial line like
`[xhci] N ports detected` proves only that the daemon ran; only QMP injection
proves the full hardware chain — controller event ring → interrupter → IRQ drain
→ HID decode → kbd_server → prompt.

## Key Files

| File | Purpose |
|---|---|
| `userspace/drivers/xhci/src/main.rs` | Driver entry point; controller bring-up, PCI claim, service start |
| `userspace/drivers/xhci/src/controller.rs` | xHCI register access, command/event TRB ring, DMA structures, interrupt-IN Normal-TRB enqueue/decode |
| `userspace/drivers/xhci/src/enumerate.rs` | Live enumeration: implements `UsbHostOps` over real DMA rings; EP0 control transfers via Setup/Data/Status TRBs |
| `userspace/drivers/xhci/src/server.rs` | IPC server: registers `usb`, `sys_notif_bind` IRQ wiring, `ipc_recv_msg` multiplex loop, device table |
| `userspace/drivers/usbhub/src/main.rs` | Hub class driver; `SetPortFeature(PORT_POWER)`, downstream port reset + enumeration |
| `userspace/drivers/usb-hid/src/main.rs` | HID class daemon; Boot keyboard + mouse decode, `KBD_EVENT_INJECT`/`MOUSE_EVENT_INJECT` |
| `userspace/lib/usb-core/src/protocol.rs` | `AttachNotice`/`UsbRequest`/`UsbReply` wire codec; `USB_SERVICE_NAME = "usb"` |
| `kernel-core/src/usb/descriptor.rs` | USB descriptor model and parser (device/config/interface/endpoint); host-tested |
| `kernel-core/src/usb/enumerate.rs` | Host-testable `run_enumeration` state machine driving `UsbHostOps` |
| `kernel-core/src/usb/hid.rs` | `BootKeyboardDecoder` + `hid_usage_to_keycode`; `parse_boot_mouse_report`; host-tested |
| `kernel-core/src/usb/hid_report.rs` | Report-Protocol descriptor parser skeleton (host-tested only; deferred from live use) |
| `kernel-core/src/usb/hub.rs` | Hub-class logic; `PortId` topology model (root port, hub depth, parent) |
| `kernel-core/src/usb/xhci/regs.rs` | Capability/Operational/Runtime/Doorbell register layouts |
| `kernel-core/src/usb/xhci/trb.rs` | TRB type definitions, cycle-bit logic, Link TRB |
| `kernel-core/src/usb/xhci/port.rs` | PORTSC bit definitions, reset + speed detection |
| `kernel-core/src/usb/xhci/context.rs` | Slot/Endpoint context layouts for both `CSZ` sizes (32 vs 64 bytes) |
| `kernel/src/syscall/device_host.rs` | `sys_device_claim` (sets Bus Master Enable), `sys_device_irq_subscribe` (programs MSI-X table), `sys_device_pci_enumerate` |
| `kernel-core/src/input/events.rs` | `KeyEvent` (20-byte codec) and `PointerEvent` (37-byte codec) — unchanged by Phase 78 |

## How This Phase Differs From Later USB Work

- **USB mass storage** (USB sticks) is post-1.0. The enumeration infrastructure
  is in place; a bulk-endpoint transfer client and a block-device facade are not.
- **USB audio** is explicitly deferred. HDA (planned for Phase 80) is the 1.0
  audio bet; USB audio adds another class driver atop the same enumeration stack.
- **UVC video** (USB webcams) is post-1.0.
- **Report Protocol** — the `hid_report.rs` descriptor parser is host-tested but
  **not wired to any live device at 1.0**. Boot Protocol handles every standard
  keyboard and mouse; Report Protocol unlocks touchpads, gaming mice, and
  multi-touch and will come later.
- **Live mouse** — the Boot-Protocol mouse decode is host-tested in
  `kernel-core::usb::hid` (including 4-byte report → `PointerEvent`), but the
  live `usb-mouse` device path is **not wired at 1.0**. The `usb-smoke` gate
  covers the keyboard chain; mouse injection proved unreachable in the QMP
  harness at this milestone. Live mouse + multi-slot is a tracked post-1.0 item.
- **Multi-device / multi-slot** — the merged `Controller` holds one `slot_ctx`.
  The enumeration state machine supports multiple slots; multi-device support
  in the live server is a post-1.0 refactor.
- **Hot-plug** — devices are enumerated once at boot. There is no hot-plug event
  surface to userspace.

## Related Roadmap Docs

- [Phase 78 umbrella design doc](./roadmap/78-usb-host-foundation.md) — theme,
  milestone goal, and full feature scope across all three sub-phases
- [Phase 78a — xHCI Host-Controller Bring-Up](./roadmap/78a-xhci-host-bringup.md)
- [Phase 78a Task List](./roadmap/tasks/78a-xhci-host-bringup-tasks.md)
- [Phase 78b — USB Enumeration + Hub](./roadmap/78b-usb-enumeration-hub.md)
- [Phase 78b Task List](./roadmap/tasks/78b-usb-enumeration-hub-tasks.md)
- [Phase 78c — USB HID + Integration + Release](./roadmap/78c-usb-hid-and-release.md)
- [Phase 78c Task List](./roadmap/tasks/78c-usb-hid-and-release-tasks.md)

## Deferred or Later-Phase Topics

- USB mass storage, USB audio, UVC video — post-1.0 class drivers
- Report-Protocol live use — skeleton exists in `kernel-core/src/usb/hid_report.rs`
- Live mouse over USB and multi-device / multi-slot controller management
- Hot-plug event surface to userspace
- USB-C / Power Delivery / DisplayPort alternate mode
- xHCI Debug Capability (DbC) for kernel debug-over-USB
- USB SuperSpeedPlus link-state policy (xHCI manages link training automatically
  today; m3OS does not expose link-state configuration)
