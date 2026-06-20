# USB Class Expansion

**Aligned Roadmap Phase:** Phase 92 — USB Class Expansion
**Status:** Complete
**Source Ref:** phase-92
**Supersedes Legacy Doc:** N/A (first USB-class-expansion doc; builds on the
Phase 78a/b/c xHCI host-stack docs)

## Overview

Phase 92 takes the xHCI host stack stood up in Phase 78a/b/c — which shipped
nothing but a Boot-Protocol keyboard/mouse path — and turns it into a *real*
USB stack with multiple device classes. It is additive layering on a fixed
foundation: the controller, the enumeration state machine, and the `usb-core`
IPC protocol are reused unchanged, and every class deliverable is a new ring-3
driver standing on the Phase 96 **bulk-endpoint substrate** (`PollBulkIn` /
`SubmitBulkOut` / `BulkData` / `ControlWrite`, `USB_MSG_MAX` = 4096, the
`handle.rs` multi-controller codec) rather than re-implementing transport. The
phase was split — for breadth-first delivery — into a CI-verified **core** plus
five numbered sub-phases that each land and validate independently: **92a**
(tier-2 hub enumeration + mass-storage mount), **92b** (live HID Report
Protocol), **92c** (isochronous UAC/UVC), **92d** (multi-controller
concurrency), and **92e** (USB-Ethernet CDC class drivers — the sub-phase this
doc closes the phase with). The headline learner outcome is that a modern,
PS/2-less machine now gets USB keyboard/mouse, mass storage that mounts at
`/mnt/usb<n>`, USB audio, hot-plug, and devices behind external hubs — proven
by the always-on `M3OS_USB_REGRESSION` gate suite, with the hardware-only paths
(UVC capture, CDC-ECM/NCM dongles) gated opt-in behind skip-with-reason exactly
as `wifi-smoke` is, because QEMU ships no model for them.

## What This Doc Covers

- **Multi-tier hub enumeration and the xHCI route string** — how a device
  *behind* a hub is addressed, and why xHCI uses a 20-bit route string instead
  of the bus/address tuple older host controllers used.
- **HID Report Protocol vs Boot Protocol** — why a real mouse/tablet/keyboard
  needs a parsed Report descriptor, not the fixed 8-byte/3-byte boot layout, and
  the one path (LED `SET_REPORT`) that writes *back* to the device.
- **USB Mass Storage (BOT, with UAS deferred)** — the CBW/CSW bulk protocol, the
  SCSI subset, the synchronous single-TRB data-IN phase, and how a LUN becomes a
  `RemoteBlockDevice` that mounts like any disk.
- **Isochronous endpoints (UAC / UVC)** — why isochronous transfer is
  fundamentally different from interrupt/bulk (fixed schedule, reserved
  bandwidth, no retry) and how PCM is split into per-frame transfer descriptors.
- **Multi-controller concurrency** — multiplexing each secondary controller's
  interrupt into one bound notification (the m3OS single-event-loop pattern) and
  why that, not per-controller threads, is the right shape here.
- **USB-Ethernet: CDC-ECM/NCM vs the vendor `ure` generalization** — the
  class-compliant generalization of Phase 96's vendor Realtek driver, the shared
  device-match registry, and the honest boundary between what is host-tested and
  what is bare-metal-only.

## Core Implementation

### Multi-tier hub enumeration and the route string (Track A / 92a)

Older host controllers (UHCI/EHCI) addressed a device by a flat 7-bit bus
address and let the controller figure out the path. xHCI instead makes the host
software name the **exact path** to a device through the hub tree using a
**20-bit route string** (xHCI §8.9): four 4-bit nibbles, one per hub tier, each
giving the downstream port number that leads toward the device. A root-hub
device has route string `0`; a device behind one hub gets a non-zero string. The
route string goes into **Slot Context dword0** and the root-hub port number goes
**separately** into **Slot Context dword1** (`slot_context_dword0` /
`slot_context_dword1`) — the route string locates the device *within* the tree,
the root-hub port says which root port the tree hangs off of. m3OS builds this
tree live as it discovers hubs and devices: `PortTopology::{add_root_port,
add_child_port, route_string, root_hub_port, depth_of}` is a flat-arena tree
bounded by `MAX_HUB_DEPTH` (5), and the resident `usbhub` walker reads the hub
descriptor (`bNbrPorts`, `bPwrOn2PwrGood`), powers and resets each downstream
port (`SET_FEATURE(PORT_POWER/PORT_RESET)`), polls `GET_PORT_STATUS` until the
port enables, then asks the server (`UsbRequest::EnumerateChild`) to run the
full Enable Slot / Address Device / Configure Endpoint sequence for the child —
addressed by that route string. Live-validated by `usb-hub-smoke`
(`XHCI_HUB:child-enumerated` for a full-speed HID device behind the hub).

### HID Report Protocol vs Boot Protocol (Track B / 92b)

Boot Protocol is a BIOS-era simplification: a keyboard reports a fixed 8-byte
packet and a mouse a fixed 3-byte packet, so firmware can read input without
understanding the device. Real devices instead describe their report format with
a **HID Report descriptor**, a bytecode the host must *parse* to learn where each
axis/button/wheel lives. `parse_report_descriptor` walks that bytecode into an
array of `ReportField { usage_page, usage, bit_offset, bit_size }` (handling
Usage Min/Max ranges, Report IDs, and the relative/absolute flag), and
`decode_pointer_report` unpacks an arbitrary-bit-field report against that layout
— multi-axis motion, a signed scroll wheel, up to 32 buttons — into
`mouse_server`, while `decode_consumer_usages` maps Usage Page 0x0C
(media/volume) keys. The one path that *writes* to the device is **LED output**:
a Report-Protocol keyboard exposes OUTPUT items for Caps/Num/Scroll Lock, so
`usb-hid` issues a `SET_REPORT(Output)` over the live `ControlWrite` EP0 path
carrying the LED bitfield. Live-validated by `usb-report-smoke` (a `usb-tablet`
decodes against the parsed layout → `HID_REPORT:pointer`; a `caps_lock` press →
`USB_HID:led`; a key injected right after the control write still decodes,
proving no interrupt-IN drop across the interleaved EP0 transfer).

### USB Mass Storage: BOT (UAS deferred) (Track D / 92a)

A USB flash drive is a **bulk-class** device — the exact transport the Phase 96
NIC already used. The legacy protocol is **Bulk-Only Transport (BOT)**: every
SCSI command is wrapped in a 31-byte **CBW** (Command Block Wrapper) sent on the
bulk-OUT pipe, an optional data phase moves on a bulk pipe, and a 13-byte **CSW**
(Command Status Wrapper) is read on bulk-IN. The `usb-storage` daemon speaks a
small SCSI subset — `TEST UNIT READY`, `INQUIRY`, `READ CAPACITY(10)`,
`READ(10)`, `WRITE(10)`, `REQUEST SENSE` — with the codec living host-testable in
`kernel_core::usb::mass_storage` so the kernel stays SCSI-unaware. The data-IN
phase needed a deliberately **synchronous single-TRB `SubmitBulkIn`** (not the
streaming auto-re-arm path the NIC uses): the streaming path keeps surplus IN
tokens armed, which a storage device — back in CBW-wait after its CSW — answers
with an endpoint STALL. Each LUN then registers as a `RemoteBlockDevice` over the
shared block protocol, so the VFS mounts a USB stick exactly like a SATA/NVMe
disk; the kernel gained a multi-remote-block registry plus a VFS secondary-mount
table so `mount /dev/usb0 /mnt/usb0` routes to the daemon. Live-validated by
`usb-mount-smoke` (mount + ls + read + overwrite-readback against a 4096-byte-block
ext2). **UAS** (queued SCSI over streams, USB 3.0) has a host-tested
Information-Unit codec but its live datapath is a deferred follow-up; BOT is the
validated path.

### Isochronous endpoints: UAC / UVC (Track E / 92c)

Interrupt and bulk endpoints are *reliable and best-effort* — the controller
retries on error and there is no timing guarantee. **Isochronous** endpoints are
the opposite: they carry real-time media (audio, video) on a **fixed per-frame
schedule** with **reserved bandwidth** and **no retry** — a missed or underrun
frame is simply lost, because retransmitting late audio is worse than dropping
it. This needs a distinct TRB shape (`Trb::isoch`, TRB type 5, with SIA/Frame-ID)
and a distinct endpoint type at Configure Endpoint (`EP_TYPE_ISOCH_OUT/IN`,
`EP_CERR_0` for no-retry). The UAC speaker driver `usb-audio` splits the mixed
PCM stream from `audio_server` into **≤ `wMaxPacketSize`-sized per-frame Isoch
TDs** (a full-speed isoch TD carries at most one packet per frame) and submits
them via `Controller::submit_isoch_out`, treating Ring-Underrun and
Missed-Service as non-fatal. It registers `audio.hw` as a peer PCM sink beside
AC'97/HDA — live-validated by `usb-audio-smoke` (a tone mixed through
`audio_server` → the USB sink → isoch OUT → QEMU `usb-audio` → a **non-silent
captured WAV**). The UVC camera path (`usb-video` + `camera_server`, the
`kernel_core::usb::uvc` probe/commit codec) is host-tested but its live capture
is **bare-metal/VFIO-only** — QEMU ships no UVC model.

### Multi-controller concurrency (Track F / 92d)

With more than one xHCI controller, a device on a *secondary* controller must be
serviced on **its own interrupt**, not only when traffic happens to arrive on the
primary. The textbook answer is one OS thread per controller, but m3OS's native
userspace heap (`BrkAllocator`) is single-threaded by design and the ring-drain
path allocates — a per-controller service thread would race the allocator.
Instead, Phase 92d uses the **m3OS single-event-loop pattern**: each secondary
controller's MSI-X IRQ is *subscribed into the primary's bound notification* at a
distinct bit (`Controller::init_interrupter_into` →
`IrqNotification::subscribe_into` → the kernel's caller-provided-notification
path), so the one server loop wakes on *any* controller's interrupt. This is the
analog of adding every fd to a single `epoll` set. A `Notification(bits)` wake
then drains **only** the controller(s) whose bit fired (bit-directed draining),
and the loop skips the ERDP/IMAN MMIO write on an empty ring. Two perf
follow-ups landed alongside: per-controller **interrupt moderation** (IMOD, 1 ms)
to coalesce bulk-completion storms, and an **interleaved-drain** so a slow control
transfer on one controller services the others' event rings rather than letting a
co-resident ring overflow. Live-validated by `usb-multi-controller-smoke`
(`XHCI:controller-1:ready` + a mouse decode on the second controller).

### USB-Ethernet: CDC-ECM/NCM vs the vendor `ure` generalization (Track G / 92e)

This is the conceptual heart of the final sub-phase. Phase 96's `ure` is a
**vendor** driver — it knows the Realtek RTL815x register map and drives one
chip family. **CDC-ECM** (Communications Device Class — Ethernet Control Model)
is the *class-compliant generalization* of that idea: a standard set of CDC
functional descriptors plus plain Ethernet-frames-over-bulk, so the *same* bulk
primitives and `RemoteNic` facade bring up *any* class-compliant dongle, not one
chip. **CDC-NCM** (Network Control Model) is ECM plus throughput: instead of one
Ethernet frame per bulk transfer, it aggregates multiple frames into one **NTB**
(NCM Transfer Block) framed by an NTH16 header + an NDP16 datagram-pointer table.

Phase 92e delivers two things. First, a shared **device-match registry**:
`kernel_core::usb::cdc::match_usb_net_driver(vid, pid, class, subclass) ->
Option<UsbNetDriver>` routes a Realtek `0x0bda:0x815x` device to the vendor `ure`
verdict and a class-`0x02` (ECM subclass `0x06` / NCM `0x0d`) or `0x0a`-data
interface to the CDC driver, with `refine_cdc_variant(config)` picking ECM vs NCM
from the config blob. Second, a new live ring-3 **`usb-net` daemon**
(`userspace/drivers/usb-net/src/main.rs`) that binds a CDC interface, parses the
CDC Ethernet functional descriptor (`find_ethernet_functional_desc`) and reads
the MAC from its string descriptor (`parse_ecm_mac`), issues `SET_INTERFACE` to
select the data alt-setting, and presents an L2 `RemoteNic` (registering
`net.nic`, serving TX via `SubmitBulkOut` and RX via `PollBulkIn`, NTB-framed for
NCM via `build_ntb16`/`parse_ntb16`), releasing per-device state on an
`attached:false` detach (C.4).

**Honesty boundary, two layers deep.** First: the `ure` *binary* is **not on
`main`** — it lives on the unmerged `docs/96-bare-metal-usb-ethernet` branch — so
today the registry returns the `ure` *verdict* (which `usb-net` logs), but the
actual hand-off to the `ure` driver lands with the Phase 96 merge. Second: QEMU
ships **no CDC-ECM/NCM device model**, so the live CDC datapath is
bare-metal/VFIO-only — the `usb-eth-smoke` CDC arm skips-with-reason in CI,
mirroring the `wifi-smoke` pattern. What *is* CI-verified is the host-logic: the
device-match registry, the CDC functional-descriptor parse, NTB-16 build/parse
framing, and the MAC parse are all host-tested in `kernel_core::usb::cdc` (38
tests).

## Key Files

| File | Purpose |
|---|---|
| `kernel-core/src/usb/cdc.rs` | CDC functional-descriptor parse + ECM MAC parse + NTB-16 framing + the `match_usb_net_driver` device-match registry (Track G / 92e) — 38 host tests |
| `kernel-core/src/usb/hub.rs` | Hub descriptor/port-status encoders + `PortTopology` route-string computation (Track A / 92a) |
| `kernel-core/src/usb/hid_report.rs` | `parse_report_descriptor` → `ReportField` layout + `decode_pointer_report` / `decode_consumer_usages` (Track B / 92b) |
| `kernel-core/src/usb/mass_storage.rs` | Host-testable BOT CBW/CSW + SCSI command codec (Track D / 92a) |
| `kernel-core/src/usb/uac.rs` | UAC AudioStreaming parse + `find_isoch_out_stream` (Track E / 92c) |
| `kernel-core/src/usb/uvc.rs` | UVC probe/commit streaming-control codec + `find_video_stream` + `camera_ipc` (Track E / 92c) |
| `userspace/drivers/usb-net/src/main.rs` | The live CDC-ECM/NCM class driver: bind, functional-descriptor + MAC parse, alt-setting select, `RemoteNic` (Track G / 92e) |
| `userspace/drivers/usb-storage/src/main.rs` | The live BOT mass-storage daemon: SCSI over the bulk pair + `RemoteBlockDevice` block server (Track D / 92a) |
| `userspace/drivers/usb-audio/src/main.rs` | The live UAC speaker driver: isoch OUT PCM sink registered as `audio.hw` (Track E / 92c) |
| `userspace/drivers/usb-video/src/main.rs` | The UVC capture driver feeding `camera_server` (host-tested + bare-metal-only) (Track E / 92c) |
| `userspace/drivers/usbhub/src/main.rs` | The resident hub walker: descriptor read, per-port power/reset, tier-2 child enumeration (Track A / 92a) |
| `userspace/drivers/xhci/src/server.rs` | Host stack: interface surfacing, `EnumerateChild`, hot-plug, multi-controller IRQ multiplexing |
| `userspace/drivers/xhci/src/controller.rs` | Host stack: isoch TRB scheduling (`submit_isoch_out`), synchronous `SubmitBulkIn`, per-controller interrupter setup |

## How This Phase Differs From Later USB Work

- **This phase builds on the Phase 78a/b/c xHCI host stack** (controller
  bring-up, enumeration state machine, the `usb` IPC service) and the **Phase 96
  bulk-endpoint substrate** (`PollBulkIn`/`SubmitBulkOut`/`BulkData`/`ControlWrite`,
  `USB_MSG_MAX`, the multi-controller handle codec). It adds *class drivers* on
  that transport, not new transport.
- **`GetDescriptors`/`ConfigureEndpoints`/`SubmitTransfer` stay mostly `ENOSYS`
  by design** — descriptors are pre-resolved into the `AttachNotice` at
  enumeration and endpoints are configured during bring-up, so only the residual
  paths a class driver genuinely needs (large-descriptor `GetDescriptors`,
  zero-copy shm DMA) were lit up.
- **USB power management** (selective suspend / link power states), **USB4 /
  Thunderbolt** tunneling, and TT bandwidth accounting are **out of scope** for
  this phase.

## Related Roadmap Docs

- [Phase 92 design doc](./roadmap/92-usb-class-expansion.md)
- [Phase 92 task list](./roadmap/tasks/92-usb-class-expansion-tasks.md)
- [Phase 78a — xHCI host bring-up (foundation)](./roadmap/78a-xhci-host-bringup.md)
- [Phase 78b — USB enumeration + hub (predecessor)](./roadmap/78b-usb-enumeration-hub.md)
- [Phase 78c — HID Boot Protocol + `usb` IPC service (extended here)](./roadmap/78c-usb-hid-and-release.md)

## Deferred or Later-Phase Topics

- **Live UAS** (queued SCSI over streams) — the Information-Unit codec is
  host-tested, but the live datapath falls back to BOT.
- **UVC isochronous-IN capture + full VS Format/Frame negotiation** — the
  `usb-video` path prefers a bulk-IN alt-setting and is bare-metal-only (no QEMU
  UVC model); YUY2/MJPEG selection is deferred.
- **CDC-NCM live dongle bring-up + NCM parameter negotiation**
  (`SET_NTB_INPUT_SIZE`) — the NTB-16 framing is host-tested; the live arm is
  bare-metal/VFIO-only.
- **RNDIS** USB-Ethernet (the Windows-centric alternative to CDC) — out of the
  Track G registry's scope.
- **The `ure` binary itself** — lands with the Phase 96 merge; today the registry
  only returns its verdict.
- **Full async EP0 control transfers + per-controller service threads** — both
  need a thread-safe native allocator first (the `BrkAllocator` is
  single-threaded by design); the single-event-loop multiplexing is the
  architecturally correct pattern in the meantime.
- **Re-attach of a USB-Ethernet dongle after detach** — `usb-net` releases
  per-device state on detach, but full re-attach robustness is a later item.
