# Phase 90 - USB Class Expansion

**Status:** Planned (post-1.0)
**Source Ref:** phase-90
**Depends on:** Phase 78a (xHCI Host Bring-Up) ✅, Phase 78b (USB Enumeration + Hub) ✅, Phase 78c (HID Boot Protocol + usb IPC service) ✅, Phase 83 (Release 1.0 Gate)
**Builds on:** Extends the Phase 78 USB foundation — xHCI host driver, root-hub enumeration, HID Boot-Protocol keyboard + mouse, and the `usb` IPC service — with the full set of USB features explicitly deferred from 78c
**Primary Components:** `userspace/drivers/usbhub` (live hub walker), `userspace/drivers/xhci_server` (GetDescriptors, multi-tier slot assignment, isochronous scheduling), `kernel-core/src/usb/hid_report.rs` (Report Protocol wiring), `userspace/drivers/usb_mass_storage` (new — BOT + UAS facade), `userspace/drivers/usb_audio` (new — UAC isochronous PCM), `userspace/drivers/usb_video` (new — UVC isochronous frames)

## Milestone Goal

m3OS supports the full USB class ecosystem deferred from the 1.0 release: multi-tier hubs enumerate devices behind them, HID Report Protocol enables touchpads and gaming mice, hot-plug attach/detach events reach userspace dynamically, USB flash drives mount as block devices, and isochronous endpoints carry USB audio and video class streams. Multiple xHCI controllers are serviced concurrently.

## Why This Phase Exists

Phase 78a/78b/78c were deliberately scoped to "minimum credible USB at 1.0": xHCI bring-up, root-hub enumeration, and HID Boot Protocol for one keyboard and one mouse. Every item in this phase is documented in the 78c design doc or task list with an explicit "→ Phase 90" deferral. Phase 90 makes good on those deferrals after the 1.0 gate, bringing m3OS to the USB class coverage expected of a general-purpose OS.

## Learning Goals

- Understand how external USB hubs extend the root-hub topology: the Hub Descriptor drives per-port power sequencing and reset; downstream devices get their own slot assignments from the xHCI controller just like root-port devices
- See how HID Report Descriptors encode arbitrary input events (axes, buttons, LED outputs) and how Boot Protocol trades expressiveness for simplicity
- Learn how hot-plug differs from static enumeration: Port Status Change events arrive on the xHCI event ring and must trigger dynamic slot assignment or teardown
- Understand Bulk-Only Transport (BOT) and UAS for mass storage: command block wrappers, bulk-in/out endpoint pairs, and how a USB stick becomes a block device
- See why isochronous endpoints have fixed bandwidth reservations and why they have no retry — USB audio and video trade guaranteed delivery for timing

## Feature Scope

### Track A — Multi-tier hub enumeration

- **A.1** — Promote `usbhub` from a stub that classifies and exits to a live IPC consumer: walk `NextAttach` for `CLASS_HUB` (0x09) devices and remain resident.
- **A.2** — Hub bring-up: issue `GET_DESCRIPTOR(Hub)` over the `ControlRequest` IPC path to read the hub descriptor; drive `SET_FEATURE(PORT_POWER)` and `SET_FEATURE(PORT_RESET)` per downstream port.
- **A.3** — Downstream device reporting: notify the xHCI server of devices behind the hub so they receive Enable Slot / Address Device / Configure Endpoint sequences.
- **A.4** — Wire the xHCI server to publish hub-class devices and respond to `GetDescriptors` live (today both reply `ENOSYS`).

### Track B — HID Report Protocol

- **B.1** — Wire `kernel-core/src/usb/hid_report.rs::parse_report_descriptor` into the running `kbd_server` / `mouse_server` path; today the parser is host-tested-only and 1.0 ships Boot Protocol only.
- **B.2** — Touchpad and gaming-mouse support: multi-axis relative movement, additional buttons, scroll wheels.
- **B.3** — Consumer-control keys (media keys, brightness) via Report Protocol usage pages.
- **B.4** — Keyboard LED output: `SET_REPORT` control transfers for Caps Lock / Num Lock / Scroll Lock LEDs.

### Track C — Live hot-plug event surface

- **C.1** — Replace the static device table (built once at bring-up) with a live Port Status Change → `AttachNotice` push pipeline.
- **C.2** — Send `AttachNotice { attached: false }` on disconnect (the field exists in the protocol but is never sent at 1.0).
- **C.3** — Dynamic re-enumeration on attach: repeat the Address Device / Configure Endpoint sequence for newly appeared ports without restarting the xHCI server.
- **C.4** — Propagate detach to class drivers (`usbhub`, `kbd_server`, mass-storage facade) so they release capabilities cleanly.

### Track D — USB Mass Storage (BOT + UAS)

- **D.1** — Map the `PageGrant` in the xHCI server and program bulk-endpoint TRBs; today the `UsbRequest::SubmitTransfer` page-grant transport exists but has no live consumer.
- **D.2** — Bulk-Only Transport (BOT) command block wrapper: `CBW` / `CSW` framing over bulk-out / bulk-in endpoint pairs.
- **D.3** — USB Attached SCSI (UAS) for higher-throughput devices: stream IDs + task management.
- **D.4** — `RemoteBlockDevice`-style facade: expose each mass-storage LUN as a block device; the VFS mounts USB sticks under `/mnt/usb<n>`.

### Track E — Isochronous endpoints: USB Audio (UAC) and USB Video (UVC)

- **E.1** — UAC isochronous PCM-out: schedule isochronous TRBs on the xHCI event ring; expose a PCM sink to `audio_server` alongside the existing AC'97 path.
- **E.2** — UVC isochronous frame capture: bulk or isochronous transfer to a framebuffer; expose frames to a new `camera_server` IPC surface.

### Track F — Multi-controller concurrency

- **F.1** — Run an independent event-loop thread per xHCI controller; today only the primary controller's event ring is serviced (documented in `main.rs`).
- **F.2** — Concurrent IRQ routing: each controller owns its MSI-X vector and wakes its own loop without serialising through a single handler.

## Important Components and How They Work

### Hub enumeration and slot assignment

A USB hub is itself a USB device — it enumerates like any other, then the host driver reads its Hub Descriptor to learn how many downstream ports it has. The hub drives power and reset to each port; the host then assigns a new slot to each downstream device exactly as it would for a root-hub port. The result is a device tree of arbitrary depth (up to the USB spec's 7-tier limit). The Phase 78b xHCI server only calls `Enable Slot` for root-hub ports; Phase 90 generalises slot assignment so `usbhub` can trigger it for tier-2+ devices via IPC.

### HID Report vs. Boot Protocol

Boot Protocol is a fixed 8-byte keyboard report and a fixed 3-byte mouse report — simple enough to parse in BIOS firmware. Report Protocol uses a descriptor that encodes each field's usage, size, and count; a touchpad might report X/Y/pressure as separate signed 16-bit fields plus multi-touch contact IDs. `parse_report_descriptor` in `kernel-core` already handles the descriptor language; Phase 90 connects its output to the running input pipeline so class drivers can drive arbitrary HID devices.

### Bulk-Only Transport and the block-device facade

BOT wraps SCSI commands in a 31-byte Command Block Wrapper sent over the bulk-out endpoint and reads status from a 13-byte Command Status Wrapper on bulk-in. The `SubmitTransfer` page-grant mechanism in the `usb` IPC service can carry the data phase; Phase 90 programs the TRBs on the xHCI side and surfaces the result as a `RemoteBlockDevice` so the VFS mount path needs no modification.

## How This Builds on Earlier Phases

- Extends the Phase 78a xHCI host driver with multi-controller concurrency and isochronous TRB scheduling.
- Extends the Phase 78b root-hub enumerator and `usbhub` stub into a live hub walker.
- Extends the Phase 78c `usb` IPC service (`GetDescriptors`, `ControlRequest`, `SubmitTransfer`) with live consumer paths that were unreachable at 1.0.
- Reuses the `RemoteBlockDevice` facade pattern from the Phase 77 userspace NVMe driver — USB mass storage gets the same ring-3 hosting model.
- Plugs into the Phase 69 `audio_server` PCM-out path for UAC without kernel changes.

## Implementation Outline

1. Wire `usbhub` as a live IPC consumer; implement hub bring-up and downstream slot assignment (Track A).
2. Extend the xHCI server to publish hub-class devices and serve `GetDescriptors` / `ControlRequest` live.
3. Wire HID Report Protocol in `kbd_server` / `mouse_server`; validate with a gaming mouse and a touchpad (Track B).
4. Implement the Port Status Change → `AttachNotice` live pipeline and hot-plug re-enumeration (Track C).
5. Implement BOT mass storage and the `RemoteBlockDevice` facade; mount a USB stick under `/mnt/usb0` (Track D).
6. Implement per-controller event-loop threads for multi-controller concurrency (Track F).
7. Implement UAC isochronous PCM-out; validate audio playback from a USB speaker (Track E.1).
8. Implement UVC frame capture and `camera_server` IPC surface (Track E.2).
9. Bump kernel to the next post-1.0 minor version.

## Acceptance Criteria

- A USB flash drive attached behind a 4-port USB hub enumerates and mounts; `ls /mnt/usb` lists its files.
- Disconnecting and reconnecting the flash drive triggers clean detach and re-enumeration without restarting any daemon.
- A HID Report Protocol gaming mouse reports correct X/Y axes and additional buttons through `mouse_server`; Caps Lock toggles the keyboard LED via `SET_REPORT`.
- A second xHCI controller (a second QEMU `-device qemu-xhci` instance) enumerates its devices independently and concurrently with the primary controller.
- USB audio (UAC) plays a PCM stream; `audio_server` lists the USB sink alongside AC'97.
- No regression in the Phase 78c Boot-Protocol keyboard or mouse, or in any smoke / regression gate.

## Companion Task List

- [Phase 90 Task List](./tasks/90-usb-class-expansion-tasks.md) — to be authored when implementation planning begins.

## How Real OS Implementations Differ

- Linux's `usbcore` + `hub.c` handle arbitrary hub depth natively; the driver framework makes multi-tier enumeration transparent. m3OS at Phase 90 wires this explicitly through the `usb` IPC service boundary.
- Linux's HID subsystem uses Report Descriptors universally — Boot Protocol is only a BIOS fallback. m3OS ships Boot Protocol at 1.0 because it covers the 99% case with zero descriptor-parsing risk at bring-up.
- Real OS hot-plug is interrupt-driven and fully asynchronous at every layer. m3OS at Phase 90 adds the Port Status Change → event surface path but may still serialize some re-enumeration steps through the xHCI server's single-threaded IPC loop.
- BOT is considered legacy in the USB 3.x era; UAS is the preferred SCSI transport for USB 3.0 drives. m3OS ships both and selects UAS when the device advertises it.
- Production OSes support UAC 2.0 and UAC 3.0 (high-speed isochronous, multiple sampling rates, feedback endpoints). m3OS at this phase targets UAC 1.0 full-speed isochronous only.
- UVC device profiles, format negotiation, and compressed streams (H.264, MJPEG) — deferred.

## Deferred Until Later

- USB-C alt-mode negotiation (DisplayPort, Thunderbolt)
- USB Power Delivery (PD) — charging policy, power roles
- xHCI Debug Capability (DbC) — hardware-level USB debug channel
- UAC 2.0 / 3.0 feedback-endpoint and high-speed isochronous audio
- UVC compressed formats (MJPEG, H.264)
- USB Bluetooth class (wireless HID)
- USB CDC / Ethernet gadget (USB networking)
- USB OTG / dual-role
- Per-tier hub TT (Transaction Translator) bandwidth accounting for full/low-speed devices behind USB 2.0 hubs
