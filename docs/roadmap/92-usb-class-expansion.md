# Phase 92 - USB Class Expansion

**Status:** Complete (92a–92e landed + the C.4 unmount-on-detach / D.4 multi-stick follow-ups; kernel `0.92.5`) — fully validated except **live UAS command/data driving** (the UAS codec + UAS-vs-BOT detection ship; the live IU datapath is a hardware-only deferral, see *Deferred Until Later*). C.4 (`usb-unmount-smoke`) and D.4 (`usb-storage-dual-smoke`) are gated.
**Source Ref:** phase-92
**Depends on:** Phase 78a (xHCI Host Bring-Up) ✅, Phase 78b (USB Enumeration + Hub) ✅, Phase 78c (HID Boot Protocol + `usb` IPC service) ✅, Phase 74 (IPC Capability Grants — the page-grant transport) ✅, Phase 83 (Release 1.0 Gate) ✅, Phase 96 (Bare-Metal USB-Ethernet) ✅ — the USB **bulk-endpoint transport** + **multi-controller handle codec** this phase's bulk-class drivers build on landed ahead of Phase 92 via PR 248 (host-stack robustness) and PR 237 (the `ure` driver)
**Builds on:** Extends the Phase 78 USB foundation — xHCI host driver, root-hub enumeration, HID Boot-Protocol keyboard + mouse, and the `usb` IPC service — with the USB class features explicitly deferred from 78c. It builds **directly on the Phase 96 bulk-endpoint substrate** (landed ahead of this phase): the `PollBulkIn` / `SubmitBulkOut` / `BulkData` inline bulk transport, the `ControlWrite` OUT-with-data control path, `USB_MSG_MAX` raised 1024 → 4096, the multi-controller `handle.rs` slot codec, and the three Phase 78c carry-over hardening fixes (see *Carry-over hardening*). Phase 92 adds the class drivers on top rather than re-implementing transport.
**Primary Components:** `userspace/drivers/usbhub` (live hub walker), `userspace/drivers/xhci` (the `usb` IPC server in `src/server.rs` — live `GetDescriptors`, dynamic/tier-2 slot assignment, isochronous scheduling, per-controller event loops), `userspace/lib/usb-core/src/protocol.rs` (the shared IPC protocol surface), `kernel-core/src/usb/{hid_report,hub,enumerate,descriptor}.rs` (Report Protocol + hub topology + enumeration logic), `userspace/drivers/usb-hid` (Report Protocol wiring), `userspace/drivers/usb-storage` (new — BOT + UAS facade), `userspace/drivers/usb-net` (new — generic CDC-ECM/NCM class driver that generalizes the Phase 96 vendor `ure` RemoteNic), `userspace/drivers/usb-audio` (new — UAC isochronous PCM), `userspace/drivers/usb-video` (new — UVC isochronous frames)

## Milestone Goal

m3OS supports the full USB class ecosystem deferred from the 1.0 release: multi-tier hubs enumerate devices behind them, HID Report Protocol enables touchpads and gaming mice, hot-plug attach/detach events reach userspace dynamically, USB flash drives mount as block devices, isochronous endpoints carry USB audio and video class streams, and a generic CDC-ECM/NCM driver brings up arbitrary USB-Ethernet dongles (generalizing the Phase 96 vendor `ure` proof). Multiple xHCI controllers are serviced concurrently, each on its own interrupt. The phase closes by bumping the kernel to `0.92.5` (the `0.92.0` core milestone plus the `0.92.1`–`0.92.5` sub-phase patch releases) and shipping the Phase 92 learning doc.

## Why This Phase Exists

Phase 78a/78b/78c were deliberately scoped to "minimum credible USB at 1.0": xHCI bring-up, root-hub enumeration, and HID Boot Protocol for one keyboard and one mouse. Every item in this phase is documented in the 78c design doc or task list with an explicit "→ Phase 92" deferral. Phase 92 makes good on those deferrals after the 1.0 gate, bringing m3OS to the USB class coverage expected of a general-purpose OS.

The phase ordering is deliberate but not strictly numeric: **Phase 96 (Bare-Metal USB-Ethernet) landed before Phase 92's implementation began.** Driving a real RTL8156 dongle on bare metal forced the USB bulk-endpoint primitives, the control-OUT-with-data path, the `USB_MSG_MAX` bump, the multi-controller slot codec, and all three of the 78c carry-over hardening fixes into `main` ahead of schedule (PR 248 + PR 237). Phase 92 therefore inherits a hardened bulk substrate and spends its budget on *class drivers* (hubs, Report Protocol, hot-plug, mass storage, audio/video, the generic Ethernet class) rather than transport.

## Learning Goals

- Understand how external USB hubs extend the root-hub topology: the Hub Descriptor drives per-port power sequencing and reset; downstream devices get their own slot assignments from the xHCI controller just like root-port devices, addressed through the xHCI 20-bit **route string**
- See how HID Report Descriptors encode arbitrary input events (axes, buttons, LED outputs) and how Boot Protocol trades expressiveness for simplicity
- Learn how hot-plug differs from static enumeration: Port Status Change events arrive on the xHCI event ring and must trigger dynamic slot assignment or teardown (Enable Slot ↔ Disable Slot)
- Understand Bulk-Only Transport (BOT) and UAS for mass storage: command block wrappers, bulk-in/out endpoint pairs, and how a USB stick becomes a block device — and how the same bulk transport already proven by the Phase 96 `ure` NIC carries SCSI
- See why isochronous endpoints have fixed bandwidth reservations and why they have no retry — USB audio and video trade guaranteed delivery for timing
- See how a vendor-specific driver (Phase 96 `ure`) generalizes into a class-compliant one (CDC-ECM/NCM) once the bulk + `RemoteNic` facade are in place

## Feature Scope

### Track A — Multi-tier hub enumeration

- **A.1** — Promote `usbhub` from a stub that classifies and exits to a live IPC consumer: walk `NextAttach` for `CLASS_HUB` (0x09) devices and remain resident.
- **A.2** — Hub bring-up: issue `GET_DESCRIPTOR(Hub)` over the live `ControlRequest` IPC path to read the hub descriptor; drive `SET_FEATURE(PORT_POWER)` and `SET_FEATURE(PORT_RESET)` per downstream port, honoring `bPwrOn2PwrGood`.
- **A.3** — Downstream device reporting: notify the xHCI server of devices behind the hub (with the computed route string + root-hub port) so they receive Enable Slot / Address Device / Configure Endpoint sequences.
- **A.4** — Wire the xHCI server to surface hub-class devices to `usbhub` and assign tier-2+ slots (today `device_info_from_ctx` skips `CLASS_HUB` and `scan_ports` only enumerates root-hub ports).
- **A.5** — Drive the `kernel-core/src/usb/hub.rs` `PortTopology` arena live: `add_root_port` / `add_child_port` / `route_string` / `root_hub_port` feed the Slot Context for tier-2+ devices (host-tested today, no live caller).

### Track B — HID Report Protocol

- **B.1** — Wire `kernel-core/src/usb/hid_report.rs::parse_report_descriptor` into the running `usb-hid` → `kbd_server` / `mouse_server` path; today the parser is host-tested-only (zero call sites) and 1.0 ships Boot Protocol only.
- **B.2** — Touchpad and gaming-mouse support: multi-axis relative movement, additional buttons, scroll wheels — decode variable-format reports by the parsed `ReportField` layout (requires the parser to handle Usage Min/Max ranges + Report IDs, both skeleton-limited today).
- **B.3** — Consumer-control keys (media keys, brightness) via Report Protocol Usage Page 0x0C.
- **B.4** — Keyboard LED output: `SET_REPORT` control transfers (via the live `ControlWrite` path) for Caps Lock / Num Lock / Scroll Lock LEDs.

### Track C — Live hot-plug event surface

- **C.1** — Replace the static device table (built once at bring-up) with a live Port Status Change → `AttachNotice` push pipeline (the `PortStatusChangeEvent` decoder + `Portsc` RW1C accessors already exist; no live handler reacts to them).
- **C.2** — Send `AttachNotice { attached: false }` on disconnect (the `attached` bool already rides the wire but is never set false at 1.0).
- **C.3** — Dynamic re-enumeration on attach: repeat the Address Device / Configure Endpoint sequence for newly appeared ports without restarting the xHCI server.
- **C.4** — Propagate detach to class drivers (`usbhub`, `usb-hid`, `usb-storage`, `usb-net`) so they release capabilities cleanly, and reclaim the slot via **Disable Slot** (no slot reclamation exists today → slot leak).

### Track D — USB Mass Storage (BOT + UAS)

- **D.1** — Build the Bulk-Only Transport data path on the **existing Phase 96 inline bulk primitives** (`PollBulkIn` / `SubmitBulkOut` / `BulkData`, controller `arm_bulk_in` / `take_bulk_report` / `submit_bulk_out`) — *not* a re-implemented transport. The 31-byte CBW + 13-byte CSW + per-chunk data phases all fit the `USB_MSG_MAX` = 4096 inline budget.
- **D.2** — BOT framing: `CBW` / `CSW` over the bulk-out / bulk-in endpoint pair, the `GET_MAX_LUN` class control-IN (over the live `ControlRequest`), and the SCSI command subset (TEST UNIT READY, INQUIRY, READ CAPACITY(10), READ(10) / WRITE(10), REQUEST SENSE) parsed in the ring-3 `usb-storage` daemon so the kernel stays SCSI-unaware.
- **D.3** — USB Attached SCSI (UAS) for higher-throughput USB 3.0 devices that advertise it: stream IDs + task management.
- **D.4** — `RemoteBlockDevice`-style facade: expose each mass-storage LUN as a block device over the shared block protocol (reusing the Phase 77 ring-3 NVMe hosting pattern); the VFS mounts USB sticks under `/mnt/usb<n>`.
- **D.5** — Page-grant `SubmitTransfer` TRB programming: implement the latent `UsbRequest::SubmitTransfer` path (maps a `PageGrant` and programs bulk TRBs) **only** for data phases that exceed the 4096-byte inline ceiling (large multi-sector transfers); it still returns `ENOSYS` today. Inline `SubmitBulkOut` is the default; this is the overflow path.

### Track E — Isochronous endpoints: USB Audio (UAC) and USB Video (UVC)

- **E.1** — UAC isochronous PCM-out: schedule isochronous TRBs (new EP type for the controller) and service their completions on the xHCI event ring; expose a PCM sink to `audio_server` alongside the existing AC'97 / HDA paths.
- **E.2** — UVC isochronous frame capture: isochronous (or bulk) transfer to a frame buffer; expose frames to a new `camera_server` IPC surface.
- **E.3** — Isochronous scheduling primitives in the controller: bandwidth reservation, no-retry semantics, and frame/microframe interval handling (interrupt + bulk share `arm_ring_in` today; isoch needs its own TRB shape and scheduling).

### Track F — Multi-controller concurrency

- **F.1** — Today (Phase 96 / PR 248) every controller *is* serviced, but only the **primary** controller's MSI-X IRQ wakes the server loop; secondary controllers are drained opportunistically on each message/notification wake, with the `handle.rs` slot codec multiplexing requests to the right `(controller, irq)`. Track F gives each controller its own bound IRQ + event-loop thread so a device on a secondary controller wakes the server on its own interrupt instead of waiting for the next message.
- **F.2** — Concurrent MSI-X routing: each controller owns its vector and re-arms its own ring without serializing through the primary loop.

### Track G — Host-side USB-Ethernet class drivers (CDC-ECM / NCM)

- **G.1** — Generalize the Phase 96 vendor `ure` (Realtek RTL8156) `RemoteNic` to the standard **CDC-ECM** class (`bInterfaceClass = 0x02` communications + `0x0a` CDC-data): parse the CDC functional descriptors, select the data alt-setting, drive the data-interface bulk IN/OUT pair, and present an L2 `RemoteNic` exactly as `ure` does — reusing the same bulk primitives and the bus-agnostic Phase 79 NIC facade.
- **G.2** — CDC-NCM (the framed/aggregated NTB variant) for higher-throughput dongles.
- **G.3** — Adopt the Phase 96 `ure` driver into the USB class-driver family: a shared device-match registry routes a `VID:PID`/class triple to the vendor `ure` driver or the generic CDC-ECM/NCM driver, both presenting the same `RemoteNic`. RNDIS (Windows-proprietary Ethernet-over-USB) stays deferred.

## Important Components and How They Work

### Hub enumeration and slot assignment

A USB hub is itself a USB device — it enumerates like any other, then the host driver reads its Hub Descriptor to learn how many downstream ports it has. The hub drives power and reset to each port; the host then assigns a new slot to each downstream device exactly as it would for a root-hub port, but addresses it through the xHCI 20-bit **route string** (`kernel-core/src/usb/hub.rs::PortTopology::route_string`, with the root-hub port number going in the Slot Context separately). The result is a device tree of arbitrary depth (up to the xHCI 5-tier route-string limit). The Phase 78b xHCI server only calls `Enable Slot` for root-hub ports discovered by `scan_ports`; Phase 92 generalises slot assignment so `usbhub` can trigger it for tier-2+ devices via IPC.

### HID Report vs. Boot Protocol

Boot Protocol is a fixed 8-byte keyboard report and a fixed 3-byte mouse report — simple enough to parse in BIOS firmware, and what m3OS ships live at 1.0 (`BootKeyboardDecoder` / `parse_boot_mouse_report`). Report Protocol uses a descriptor that encodes each field's usage, size, and count; a touchpad might report X/Y/pressure as separate signed fields plus multi-touch contact IDs. `parse_report_descriptor` in `kernel-core` already handles the basic descriptor language (host-tested, but with zero call sites and skeleton limits — single Usage per field, Report IDs ignored). Phase 92 enhances it (Usage ranges, Report IDs) and connects its `ReportField` output to the running input pipeline so class drivers can drive arbitrary HID devices.

### Bulk-Only Transport and the block-device facade

BOT wraps SCSI commands in a 31-byte Command Block Wrapper sent over the bulk-out endpoint and reads status from a 13-byte Command Status Wrapper on bulk-in. **This is the same bulk transport the Phase 96 `ure` NIC already exercises** (`PollBulkIn` / `SubmitBulkOut` / `BulkData`, with the controller's `arm_bulk_in` / `take_bulk_report` / `submit_bulk_out`); Phase 92 adds only the class-specific CBW/CSW framing and SCSI command set in the ring-3 `usb-storage` daemon, then surfaces the result as a `RemoteBlockDevice` so the VFS mount path needs no modification. The page-grant `SubmitTransfer` mechanism remains available (latent) for the rare data phase larger than the 4096-byte inline budget.

### From vendor `ure` to a CDC-ECM/NCM class driver

Phase 96 proved the host-side USB-Ethernet path with a *vendor-specific* driver: `ure` claims a Realtek RTL8156 (class `0xFF`), tunnels OCP register access over `ControlRequest` / `ControlWrite`, prepends/strips the Realtek V1 TX/RX descriptors on the bulk pair, and registers an L2 `RemoteNic`. A CDC-ECM/NCM driver is the *class-compliant* generalization: the framing is the standard CDC functional-descriptor + Ethernet-frame-over-bulk convention rather than a vendor register map, so the same bulk primitives and `RemoteNic` facade light up arbitrary dongles. Track G factors `ure` and the new class driver behind one device-match registry.

## How This Builds on Earlier Phases

- Extends the Phase 78a xHCI host driver with per-controller event loops and isochronous TRB scheduling.
- Extends the Phase 78b root-hub enumerator and `usbhub` stub into a live multi-tier hub walker, driving the host-tested `PortTopology` route-string logic for the first time.
- Extends the Phase 78c `usb` IPC service (`GetDescriptors`, `ControlRequest`, `SubmitTransfer`) with live consumer paths that were unreachable at 1.0.
- **Builds on Phase 96**: reuses its USB bulk-endpoint transport (`PollBulkIn` / `SubmitBulkOut` / `BulkData`), the `ControlWrite` OUT-with-data path, the `USB_MSG_MAX` = 4096 budget, the multi-controller `handle.rs` slot codec, and the three carry-over hardening fixes — Phase 92 mass storage, audio, video, and the CDC-ECM/NCM class driver are all bulk-class drivers on that substrate, and Track G generalizes Phase 96's vendor `ure` NIC.
- Reuses the `RemoteBlockDevice` facade pattern from the Phase 77 userspace NVMe driver — USB mass storage gets the same ring-3 hosting model — and the bus-agnostic Phase 79 `RemoteNic` facade for USB-Ethernet.
- Plugs into the Phase 69 / 80 `audio_server` PCM-out path for UAC without kernel audio changes.

## Implementation Outline

1. Land the foundation/hardening prerequisites the class paths need (Track H): finish the residual 78c carry-over items not closed by PR 248, make `GetDescriptors` (large-descriptor reads) live, and add Disable Slot reclamation.
2. Wire `usbhub` as a live IPC consumer; implement hub bring-up and tier-2+ slot assignment via the `PortTopology` route string (Track A).
3. Wire HID Report Protocol in `usb-hid` → `kbd_server` / `mouse_server`; validate with a gaming mouse and a touchpad, and Caps Lock LED via `SET_REPORT` (Track B).
4. Implement the Port Status Change → `AttachNotice` live pipeline, detach (`attached: false`), hot-plug re-enumeration, and Disable Slot teardown (Track C).
5. Implement BOT mass storage on the existing inline bulk primitives + the `RemoteBlockDevice` facade; mount a USB stick under `/mnt/usb0`; add UAS and the page-grant overflow path (Track D).
6. Implement per-controller event-loop threads + concurrent MSI-X routing for multi-controller concurrency (Track F).
7. Implement the generic CDC-ECM/NCM class driver and fold the Phase 96 `ure` driver into the shared device-match registry (Track G).
8. Implement UAC isochronous PCM-out; validate audio playback from a USB speaker (Track E.1) and UVC frame capture + `camera_server` (Track E.2/E.3).
9. Add the Phase 92 acceptance gates, bump the kernel (`0.92.0` at the core milestone, then `0.92.1`–`0.92.5` across sub-phases 92a–92e), and ship the Phase 92 learning doc (Track I).

## Acceptance Criteria

- A USB flash drive enumerates and mounts; `ls /mnt/usb0` lists its files. (Direct-attach in QEMU — `usb-mount-smoke`. Tier-2 hub enumeration is CI-validated separately with a full-speed HID device behind the hub — `usb-hub-smoke` → `XHCI_HUB:child-enumerated`; a *high-speed mass-storage* device behind an external hub is bare-metal-only because QEMU's `usb-hub` is full-speed USB 1.1 and cannot carry it.)
- Disconnecting and reconnecting the flash drive triggers a clean detach (`AttachNotice { attached: false }` + Disable Slot) and re-enumeration without restarting any daemon.
- A HID Report Protocol gaming mouse reports correct X/Y axes and additional buttons through `mouse_server`; Caps Lock toggles the keyboard LED via `SET_REPORT`.
- A second xHCI controller (a second QEMU `-device qemu-xhci`) enumerates its devices on **its own bound IRQ / event loop**, concurrently with the primary controller (not merely polled on the next message).
- A generic CDC-ECM USB-Ethernet dongle brings up an L2 `RemoteNic` through the new `usb-net` class driver, and the Phase 96 vendor `ure` driver continues to bind the RTL8156 through the shared registry.
- USB audio (UAC) plays a PCM stream; `audio_server` lists the USB sink alongside AC'97 / HDA.
- No regression in the Phase 78c Boot-Protocol keyboard or mouse, the Phase 96 `usb-eth-smoke`, or any other smoke / regression gate.
- The kernel reports `0.92.5` (boot banner / `uname` — `0.92.0` at the core milestone, bumped to `0.92.5` across sub-phases 92a–92e), and the Phase 92 learning doc (`docs/92-usb-class-expansion.md`) ships, linked from `docs/README.md` and `docs/appendix/codebase-map.md`.

## Companion Task List

- [Phase 92 Task List](./tasks/92-usb-class-expansion-tasks.md)

## How Real OS Implementations Differ

- Linux's `usbcore` + `hub.c` handle arbitrary hub depth natively; the driver framework makes multi-tier enumeration transparent. m3OS at Phase 92 wires this explicitly through the `usb` IPC service boundary.
- Linux's HID subsystem uses Report Descriptors universally — Boot Protocol is only a BIOS fallback. m3OS ships Boot Protocol at 1.0 because it covers the 99% case with zero descriptor-parsing risk at bring-up.
- Real OS hot-plug is interrupt-driven and fully asynchronous at every layer. m3OS at Phase 92 adds the Port Status Change → event surface path but may still serialize some re-enumeration steps through the xHCI server's IPC loop.
- BOT is considered legacy in the USB 3.x era; UAS is the preferred SCSI transport for USB 3.0 drives. m3OS at Phase 92 ships **BOT** as the live transport plus the host-tested UAS Information-Unit codec and the UAS-vs-BOT device-detection/selection (`find_uas_interface` → `transport=uas|bot`); the **live UAS command/data driving path** (stream IDs, queued IUs) is deferred — see *Deferred Until Later*.
- Production OSes drive USB-Ethernet through a stack of class + vendor drivers (`cdc_ether`, `cdc_ncm`, `r8152`, `ax88179_178a`, …). m3OS at Phase 92 ships the generic CDC-ECM/NCM class driver plus the Phase 96 vendor `ure` driver behind one registry, leaving RNDIS and the long vendor tail deferred.
- Production OSes support UAC 2.0 and UAC 3.0 (high-speed isochronous, multiple sampling rates, feedback endpoints). m3OS at this phase targets UAC 1.0 full-speed isochronous only.
- UVC device profiles, format negotiation, and compressed streams (H.264, MJPEG) — deferred.

## Deferred Until Later

- USB **device / gadget mode** — m3OS presenting *as* a USB peripheral (CDC-ECM gadget, mass-storage gadget, HID gadget). Phase 92 is host-side only; Phase 96 + Track G are host-side USB-Ethernet, *not* the gadget side.
- RNDIS host class driver (Windows-proprietary Ethernet-over-USB) and the long USB-Ethernet vendor tail (ASIX, etc.) beyond `ure` (Realtek).
- USB-C alt-mode negotiation (DisplayPort, Thunderbolt)
- USB Power Delivery (PD) — charging policy, power roles
- xHCI Debug Capability (DbC) — hardware-level USB debug channel
- UAC 2.0 / 3.0 feedback-endpoint and high-speed isochronous audio
- UVC compressed formats (MJPEG, H.264)
- USB Bluetooth class (wireless HID)
- USB OTG / dual-role
- HID multi-touch contact tracking beyond single-pointer touchpad mapping
- Per-tier hub TT (Transaction Translator) bandwidth accounting for full/low-speed devices behind USB 2.0 hubs
- **Live UAS command/data driving** (Track D.3) — the UAS Information-Unit codec (`kernel_core::usb::mass_storage`) and UAS-vs-BOT device detection/selection (`find_uas_interface`, logging `transport=uas|bot`) are shipped + host-tested, but the live command/status/data path over stream IDs is deferred (BOT is the validated transport; QEMU's `usb-uas` chain is bare-metal-only here).

> **Closed as follow-ups (formerly deferred here):** **Mount-teardown-on-detach** (Track C.4) — the resident `usb-storage` loop now uses the Phase 87 `ipc_recv_msg_timeout` (no new IPC primitive was needed) to notice a hot-unplug and `umount("/mnt/usb<n>")`, the kernel `sys_linux_umount2` gaining a `/mnt/usb*` branch that frees the ext2 volume + `blk::remote` slot (gated by `usb-unmount-smoke`). **Multi-stick concurrent mounts** (Track D.4) — the daemon is now multi-device (`discover_storage_devices` + `run_multi_block_server_loop`, the single-event-loop pattern), serving `usb0.block`/`usb1.block`/… concurrently (gated by `usb-storage-dual-smoke`).

### Carry-over hardening from the Phase 78c review

The Phase 78c review flagged three latent issues in the xHCI server that the live HID-boot path never exercised. **PR 248 (Phase 96 host-stack robustness) resolved or substantially mitigated all three** while bringing the bulk path online; the residual cases below are tracked under Track H and only matter once `GetDescriptors` / repeated-control-read paths go live:

- **DMA-buffer lifetime for repeated control reads — RESOLVED (PR 248).** `control_transfer` previously allocated a fresh `DmaBuffer` per data-stage IN transfer and `DmaBuffer::drop` did not free the region, so a caller issuing many control IN reads in the never-exiting USB server would leak monotonically. PR 248 added a **persistent per-slot `ep0_data_buf` scratch buffer** (`SlotContext`), grown on demand and reused across control transfers — the leak is closed. Track H only needs to keep new call sites passing through it.
- **Interrupt reports lost during a blocking transfer — ADDRESSED for the bulk path (PR 248); finish for control.** PR 248's `wait_for_bulk_out_event` captures concurrent interrupt/bulk-IN completions via `capture_interrupt_report` while a bulk-OUT is in flight, so RX is not lost during TX. The analogous capture during a blocking *control* transfer interleaved with active interrupt polling (`drain_for_transfer_event` / `drain_for_command_completion` still discard non-matching events) should be confirmed/finished when `GetDescriptors` and repeated control reads go live (Track H / B.4 LED `SET_REPORT`).
- **Inline `ControlData` capacity — MITIGATED (PR 248); finish for large descriptors.** `USB_MSG_MAX` was raised 1024 → 4096, which fits frames and small control payloads inline. But `UsbReply::ControlData` is still clamped to ≤64 bytes inline, so a large descriptor read (a full configuration descriptor) via a live `GetDescriptors` / `ControlRequest` needs either a widened inline cap or the page-grant path (Track D.5 / H).

Inject-endpoint access control was **resolved in 78c** rather than deferred: `KBD_EVENT_INJECT` / `MOUSE_EVENT_INJECT` are gated by `sys_ipc_peer_is_driver` (syscall `0x111B`), which authenticates the caller via its reply cap and admits only driver-TCB processes (`exec_path` under `/drivers/`). Tightening this further to a single named injector, or to a true capability handed to `usb-hid` at spawn, waits on a ring-3 cap-distribution mechanism that does not yet exist — track it with the privileged-driver work.
