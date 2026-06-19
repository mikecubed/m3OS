# Phase 92 — USB Class Expansion: Task List

**Status:** In Progress
**Source Ref:** phase-92
**Depends on:** Phase 78a (xHCI Host Bring-Up) ✅, Phase 78b (USB Enumeration + Hub) ✅, Phase 78c (HID Boot Protocol + `usb` IPC service) ✅, Phase 74 (IPC Capability Grants — page-grant transport) ✅, Phase 77 (ring-3 `RemoteBlockDevice` hosting) ✅, Phase 79 (`RemoteNic` facade) ✅, Phase 96 (Bare-Metal USB-Ethernet — USB bulk-endpoint transport + multi-controller handle codec) ✅
**Goal:** Deliver every USB class feature deferred from Phase 78c — multi-tier hub enumeration, live HID Report Protocol, USB hot-plug, USB mass storage (BOT + UAS), isochronous USB audio/video, a generic CDC-ECM/NCM USB-Ethernet class driver, and per-controller concurrency — building on the Phase 96 bulk-endpoint substrate (`PollBulkIn`/`SubmitBulkOut`/`BulkData`/`ControlWrite`, `USB_MSG_MAX`=4096, the `handle.rs` multi-controller codec) rather than re-implementing transport. Closes with the kernel version bump (`0.91.0` → `0.92.0`) and the Phase 92 learning doc (`docs/92-usb-class-expansion.md`).

> **Authored ahead of implementation.** Every acceptance item below is intentionally unchecked `[ ]`; it records the planned, measurable result, not a delivered one. Symbols and line references are anchored to the current tree (`main`, post-PR-248) so each task names a concrete extension point rather than a green-field design.

> **Substrate honesty (what Phase 96 already landed on `main`).** PR 248 (host-stack robustness) + PR 237 (the `ure` driver) put the bulk transport in place ahead of this phase. **LIVE** today: `UsbRequest::{NextAttach, ControlRequest, ControlWrite, PollInterruptIn, PollBulkIn, SubmitBulkOut, Topology}` and `UsbReply::{Attach, ControlData, InterruptReport, BulkData, TransferComplete, Topology, Error}` (`userspace/lib/usb-core/src/protocol.rs`); the controller bulk methods `arm_ring_in`/`arm_bulk_in`/`take_bulk_report`/`submit_bulk_out`/`wait_for_bulk_out_event` (`userspace/drivers/xhci/src/controller.rs`); `USB_MSG_MAX = 4096`; the multi-controller `pack_handle`/`unpack_handle` codec + `owner!` macro (`userspace/drivers/xhci/src/{handle,server}.rs`); and all three Phase 78c carry-over hardening fixes (persistent per-slot `ep0_data_buf`; bulk-OUT concurrent-IN capture; the `USB_MSG_MAX` bump). **Still `ENOSYS` by design:** `UsbRequest::{GetDescriptors, ConfigureEndpoints, SubmitTransfer}` — descriptors are pre-resolved into `AttachNotice` at enumeration (`device_info_from_ctx`, `server.rs:60-152`) and endpoints are configured via xHCI commands during bring-up, so most class drivers do **not** need them. Track H lights up only the residual paths a class driver actually requires.

> **Scope honesty (CI-viable vs hardware-only).** QEMU emulates `usb-hub`, `usb-storage`, `usb-audio`, a second `qemu-xhci`, and QMP `device_add`/`device_del` hot-plug — so Tracks A, C, D, E.1, F are CI-testable headlessly via the Phase 78 `usb-smoke` QMP/PPM plumbing (`xtask/src/{qmp,ppm}.rs`). UVC capture (E.2), real CDC-ECM/NCM dongles (G — QEMU has no CDC-ECM model; the `ure` arm stays the Phase 96 passthrough/VFIO path), UAC feedback endpoints, and TT bandwidth accounting are bare-metal/VFIO-only and follow the established opt-in `*_NET`/skip-with-reason pattern (mirroring `usb-eth-smoke`, `wifi-smoke`).

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Multi-tier hub enumeration — live `usbhub` walker, hub descriptor + per-port power/reset, tier-2+ slot assignment via the route string | H | **A.1–A.5 landed (Phase 92a)** — server surfaces `CLASS_HUB`; the resident `usbhub` walker binds a hub, reads its descriptor, drives per-port `PORT_POWER`/`PORT_RESET`, and **tier-2-enumerates a device behind the hub via the route string** (`UsbRequest::EnumerateChild` + `PortTopology`). Live-validated: `usb-hub-smoke` asserts `XHCI_HUB:child-enumerated` for a full-speed HID device behind the hub. |
| B | HID Report Protocol — wire `parse_report_descriptor` live, multi-axis/buttons/scroll, consumer keys, LED `SET_REPORT` | H | **B.1–B.4 landed (Phase 92b)** — `usb-hid` reads + parses the Report descriptor at bind (at its `wDescriptorLength`), classifies a non-boot HID pointer as `ReportPointer`, and decodes its reports with `decode_pointer_report` (multi-axis/scroll/buttons) into `mouse_server`; consumer-key decode (`decode_consumer_usages`) routes media keys; Caps/Num/Scroll Lock LEDs ride `SET_REPORT`. **Live-validated:** `usb-report-smoke` (`HID_REPORT:pointer` + `USB_HID:led` + H.2 no-drop). B.3-live consumer *routing* is bare-metal (no QEMU consumer device); its decode is host-tested. |
| C | Live hot-plug event surface — Port Status Change → `AttachNotice` push, detach (`attached:false`), dynamic re-enumeration, Disable Slot reclamation | — | C.1–C.3 + server-side C.4 landed (`usb-hotplug-smoke` 3-cycle PASS); class-driver-side C.4: **usb-storage→92a ✅, usb-hid→92b ✅** (`reconcile_attachments` releases on `attached:false`, gate-asserted), **usb-net→92e** |
| D | USB Mass Storage — BOT CBW/CSW on the Phase 96 inline bulk path, SCSI subset, UAS, `RemoteBlockDevice` facade + `/mnt/usb<n>`, page-grant overflow | C, H | D.1/D.2 transport + **D.3 UAS codec** + **D.4 mount landed (Phase 92a)**: the resident `usb-storage` daemon registers `usb0.block` and serves the block protocol; the kernel multi-device registry + VFS secondary-mount table mount it at `/mnt/usb0`. Live-validated by `usb-mount-smoke` (mount + ls + read + overwrite-readback). **D.5 zero-copy overflow landed** (shm-DMA, `USB_STORAGE:shm-dma-ok`); **live UAS → deferred follow-up.** |
| E | Isochronous endpoints — UAC PCM-out to `audio_server`, UVC frame capture + `camera_server`, controller isoch TRB scheduling | F | **→ Phase 92c** (deep isoch TRB scheduling; UVC bare-metal-only) |
| F | Multi-controller concurrency — secondary-controller IRQs multiplexed into the primary's bound notification (single event loop), concurrent MSI-X routing | — | **F.1/F.2 landed (Phase 92d)** — each secondary controller's MSI-X IRQ is subscribed into the primary's bound notification at a distinct bit (kernel-tested primitive), so the single server loop wakes on **any** controller's interrupt; a `Notification(bits)` wake drains only the controller(s) that fired. Implemented as the m3OS single-event-loop pattern (not per-controller threads — the native `BrkAllocator` is single-threaded by design and the ring-drain path allocates, so a service thread would race the heap). Live-validated: `usb-multi-controller-smoke` asserts `XHCI:controller-1:ready` + a controller-1 `usb-mouse` decode. |
| G | Host-side USB-Ethernet class drivers — generic CDC-ECM/NCM `RemoteNic`, fold the Phase 96 vendor `ure` into a shared device-match registry | C | G.1/G.2 host-logic landed (`cdc.rs`: CDC functional-descriptor parse + NTB-16 framing round-trip, 23 host tests); **live `usb-net` daemon → Phase 92e** (bare-metal/VFIO-gated — no QEMU CDC-ECM model) |
| H | Foundation & carry-over hardening — live `GetDescriptors` (large reads), control-transfer event capture, Disable Slot, zero-copy `SubmitTransfer` | — | H.1–H.3 landed; **H.6 length bounds + H.4 zero-copy DMA landed (Phase 92a)**. H.4 delivered as a kernel IOMMU-map-shm syscall (`SYS_DEVICE_DMA_MAP_SHM`) + `SubmitShmTransfer`, validated by `USB_STORAGE:shm-dma-ok` (8192-byte zero-copy round-trip). |
| I | Validation, kernel version bump & learning docs — new acceptance gates, AGENTS.md rows, version bumps, Phase 92 learning doc | A–H | **I.4 done** (`0.92.0` core, then **`0.92.1`** for 92a). **I.1 done** — `usb-mount-smoke` gate (mount + ls + read + rw). **I.6 done** — Phase 92 USB gates wired into `M3OS_USB_REGRESSION` + AGENTS.md. **I.5 learning doc → last sub-phase.** |

---

## Sub-Phase Schedule

Phase 92 was split (per the `flow:parallel-impl` breadth-first run) so each chunk
lands and is **independently verified** rather than waiting on the whole, deep
USB-class surface. The **core** (CI-verifiable, validated this run) is below; the
deeper / kernel-invasive / hardware-only remainder is scheduled as numbered
sub-phases so they can be scheduled and verified one at a time.

**Phase 92 core — landed + validated (always-on QEMU gates + host tests):**

- **H** (foundation) — `GetDescriptors` cache, control-event capture, Disable Slot.
- **C** (hot-plug) — Port-Status-Change → `AttachNotice`/detach/Disable-Slot (`usb-hotplug-smoke`).
- **D.1/D.2** (mass storage) — BOT CBW/CSW + SCSI codec + **the data-IN phase** (the synchronous single-TRB `SubmitBulkIn` closing the Phase 96 streaming-path STALL) + a WRITE(10)/READ(10) sector round-trip (`usb-storage-smoke` → `USB_MASS_STORAGE:ready` + `USB_STORAGE:rw-ok`).
- **A.1/A.2/A.3** (hub discovery) — server surfaces `CLASS_HUB`; the live `usbhub` walker reads the hub descriptor + powers/resets ports (`usb-hub-smoke`).
- **B.1** (HID Report) — live Report-descriptor read + parse at bind (`usb-smoke` → `USB_HID:report-parsed`).
- **B.2/B.3, G.1/G.2** host-logic — Report-descriptor Usage ranges + Report IDs, consumer keycodes, CDC functional-descriptor parse + NTB-16 framing (host tests).

Each sub-phase below lists **every** open task ID it owns (so no item is orphaned), its validation gate, and its AGENTS.md row. The mapping is exhaustive: every `[ ]`/`[~]` acceptance item in this doc belongs to exactly one sub-phase (see the coverage table at the end).

**Phase 92a — USB tier-2 enumeration + mass-storage mount. — CORE LANDED + VALIDATED (PR #253).**
- ✅ **A.4/A.5/A.2-reset** — device-behind-hub enumeration via the xHCI route string + `PortTopology` → Slot Context. `UsbRequest::EnumerateChild` server arm + the `usbhub` walker; **live-validated** by `usb-hub-smoke` (a full-speed HID device behind the hub → `XHCI_HUB:child-enumerated`).
- ✅ **D.4** — kernel **multi-remote-block-device registry** (lifted `blk::remote` from singleton) + a **VFS secondary-mount table** (`USB_MOUNTS` + `dev_id`-aware `Ext2Volume`, root path byte-identical) + the resident `usb-storage` block-server daemon. **Live-validated** end-to-end by `usb-mount-smoke` (mount + ls + read + overwrite-readback). *(Discovery: the kernel had **no** VFS mount table — D.4 was a build-multi-mount effort spanning 33 `EXT2_VOLUME` sites + the `vfs_server` write authority; this PR adds a contained second-mount path. FAT-on-USB not wired.)*
- ✅ **D.3 codec** — UAS Information-Unit codec (`CommandIu`/`SenseIu`/`ResponseIu`/ready/`TaskMgmtIu`) host-tested; the live `usb-storage` daemon selects UAS vs BOT (BOT is the QEMU-validated path; live UAS is bare-metal — QEMU's `usb-uas` chain is untested here).
- ✅ **C.4 (usb-storage arm)** — the daemon has a `release_device` detach hook keyed on `attached:false`; full detach-during-serve needs non-blocking recv (TODO noted in-code).
- ✅ **H.6** — length bounds (caught a real chunk-overflow bug). ✅ **I.1** — `usb-mount-smoke` gate. ✅ **I.6** — gates wired into `M3OS_USB_REGRESSION` + AGENTS.md. ✅ **I.4** — kernel `0.92.0`→`0.92.1`.
- ✅ **Multi-sector bulk-OUT fix** — root-caused a recv-truncation bug (the xHCI server `recv`d with the 1522-byte Ethernet-MTU buffer, truncating >1522-byte `SubmitBulkOut`); fixed to `recv_with_capacity(USB_MSG_MAX)` (+ a SHORT_PACKET filter fix). **A real-world 4096-block ext2 now mounts + reads + writes over the inline path** (8-sector block I/O via 7+1 inline chunks).
- ✅ **D.5 + H.4 — zero-copy DMA (delivered).** New capability-gated kernel syscalls `SYS_DEVICE_DMA_MAP_SHM`/`UNMAP_SHM` IOMMU-map a **shared-memory** region (`sys_shm` — contiguous, shared by id, the right substrate vs the move-based page-grant) into a claimed device's domain; `UsbRequest::SubmitShmTransfer` programs one bulk TRB straight at it (no inline copy), freed on unmap + process exit. **Validated:** `usb-storage-smoke` `USB_STORAGE:shm-dma-ok` — a 16-sector (8192-byte, >`USB_MSG_MAX`) zero-copy WRITE+READ in single descriptors, byte-identical.
- *The hard USB data path was already done (D.1/D.2); this delivered the tier-2 enumeration + the kernel multi-mount integration.*

**Phase 92b — HID Report Protocol live decode. — CORE LANDED + VALIDATED (PR #254).**
- ✅ **B.2-live** — `usb-hid` classifies a non-boot HID pointer as `ReportPointer` and decodes its interrupt-IN reports with `kernel_core::usb::hid_report::decode_pointer_report` against the parsed `ReportField` layout (multi-axis abs/rel motion, signed wheel, up to 32 buttons), injecting motion + wheel + button edges into `mouse_server`. The parser gained `is_relative` + multi-usage-list (`Usage X; Usage Y; Input`) support. **Live-validated** by `usb-report-smoke` (a `usb-tablet` → `HID_REPORT:pointer`).
- ✅ **B.3 decode** — `decode_consumer_usages` (Usage Page 0x0C bitmap, host-tested) + `usb-hid` `poll_report_consumer` route media/volume keys (`hid_consumer_usage_to_keycode` → `kbd_server` → `audio_server`, `USB_HID:consumer`). The live routing is bare-metal/VFIO (no QEMU consumer device); the decode is host-tested.
- ✅ **B.4** — `usb-hid` tracks Caps/Num/Scroll Lock from the decoded boot-keyboard edges and issues `SET_REPORT(Output)` over the live `ControlWrite` EP0 path. **Live-validated** by `usb-report-smoke` → `USB_HID:led`.
- ✅ **C.4 (usb-hid arm)** — `reconcile_attachments` re-walks `NextAttach` (~200 ms) and releases a held device whose latest entry is `attached:false` (and binds hot-attached HID interfaces). **Live-validated** by `usb-hotplug-smoke` (`usb-hid: hot-attached`/`usb-hid: released` each of 3 cycles).
- ✅ **H.2 remaining item** — the no-drop assertion rides B.4: `usb-report-smoke` injects a key right after the `SET_REPORT` and asserts it still decodes (`USB_HID:key … sym=0x…62`).
- ✅ **B.1 readiness** — wDescriptorLength-driven Report read (`report_descriptor_len` via `GetDescriptors`), corrected `hid_report.rs` module doc header, hostile Report-Count host test (`MAX_REPORT_FIELDS`=65536 cap).
- ✅ **Gate (I.2 Report-Protocol arm):** new always-on `usb-report-smoke` (`usb-tablet` → `HID_REPORT:pointer` (B.2) + `caps_lock` → `USB_HID:led` (B.4) + post-write key decode (H.2)); wired into `M3OS_USB_REGRESSION` + the AGENTS.md row. ✅ **I.4** — kernel `0.92.1`→`0.92.2`.

**Phase 92c — USB isochronous (UAC / UVC). — LANDED + VALIDATED.**
- ✅ **E.3** — isochronous TRB scheduling primitives: `Trb::isoch` (TRB type 5, SIA/Frame-ID), `EP_TYPE_ISOCH_OUT`/`IN` + `EP_CERR_0`, the `build_configure_endpoint_ctx` fix typing isoch endpoints correctly (they previously fell through to the Interrupt-IN default), `Controller::submit_isoch_out` (one SIA Isoch TRB per `wMaxPacketSize` frame, batched, Ring-Underrun/Missed-Service treated as non-fatal), the `SubmitIsochOut` IPC protocol + server arm. Host-tested.
- ✅ **E.1** — UAC PCM-out: a new `usb-audio` ring-3 driver binds the AudioStreaming interface (now surfaced by the xHCI server), resolves the isoch OUT DCI via `GetDescriptors` + `kernel_core::usb::uac::find_isoch_out_stream`, `SET_INTERFACE(alt=1)`, registers `audio.hw`. **Live-validated** by `usb-audio-smoke` — `audio-demo` → `audio_server` mixer → USB sink → isoch OUT → QEMU `usb-audio` → **non-silent WAV** (loudest window 99% non-silent) + `frames_consumed != 0`.
- ✅ **E.2** — UVC frame capture: `usb-video` driver + `camera_server` + host-tested `kernel_core::usb::uvc` codec (probe/commit + `find_video_stream` + `camera_ipc`, 17 tests). Live capture **bare-metal/VFIO-only** (no QEMU UVC model — skip-with-reason); the server surfaces `CLASS_VIDEO` interfaces.
- ✅ **Gate (I.3 audio half):** `usb-audio-smoke` (non-silent WAV + `AUDIO_DEMO:PASS` + `frames_consumed!=0`); `M3OS_USB_AUDIO_REGRESSION` pre-push block + AGENTS.md row. ✅ **I.4** — kernel `0.92.2`→`0.92.3`. *The deepest controller work in the phase; the isoch-OUT delivery required splitting a payload into ≤mps per-frame TDs (a full-speed isoch TD carries ≤ mps/frame) — root-caused via QEMU `xhci`/WAV diagnosis.*

**Phase 92d — Multi-controller concurrency. — LANDED + VALIDATED.**
- ✅ **F.1/F.2 — multiplexed-interrupt multi-controller servicing.** A device on a *secondary* xHCI controller is now serviced on its **own interrupt**, not only when traffic happens on the primary. Delivered as the **m3OS single-event-loop pattern** rather than the task doc's literal per-controller threads: the native userspace heap (`BrkAllocator`) is single-threaded by design ("Safety: Single-threaded userspace processes") and the per-controller ring-drain path allocates, so a per-controller *service* thread would race the global allocator (the only existing threaded binary, `thread-test`, is careful never to allocate in a thread). Making malloc thread-safe would touch every native userspace binary — out of 92d scope. Instead each controller's interrupt source is added to the *one* bound event loop (the analog of adding all fds to an `epoll` set), which is the architecturally correct pattern for m3OS's single-threaded-event-loop driver model and keeps the validated single-controller path byte-identical. *(Design discussed + approved with the maintainer before implementation.)*
  - **Kernel:** `sys_device_irq_subscribe` accepts a `Capability::DeviceIrq` as `notification_arg` (via the existing, unit-tested `Capability::ipc_notification_id`), so a driver can subscribe a second device's IRQ into an already-subscribed controller's notification at a distinct bit. `kernel_owns_notif` stays false for the caller-provided path (no double-free). Covered by the existing `device_host_irq_subscribe_caller_provided_notif` test + the `ipc_notification_id_accepts_device_irq_caps` unit test.
  - **driver_runtime:** `IrqNotification::subscribe_into(device, into_notif_cap, bit_index)` + `IrqBackend::subscribe_into` — host-tested (`subscribe_into_targets_existing_notification_at_bit`, `subscribe_into_surfaces_backend_error`).
  - **xHCI driver:** `Controller::init_interrupter_into` + shared `enable_interrupter()`; `program_main` brings up controller 0 with a fresh notification (bit 0, bound to the recv loop) and multiplexes each secondary into it at `bit = controller index`, emitting `XHCI:controller-N:ready`. **Optimization A** (bit-directed draining): a `Notification(bits)` wake drains only the controller(s) whose bit is set — single-controller is bit 0 → controller 0, byte-identical to pre-92d; the Message arm still drains all as a safety net. **Optimization B**: `service_interrupt_events` skips the ERDP/IMAN MMIO write on an empty drain (mirrors the command-completion path guard), so the more-frequent multiplexed drains waste no MMIO.
- ✅ **H.5** — `process_port_events` attach arm reclaims the just-enabled slot via `disable_slot` when `pack_handle` returns `None` (no slot leak), mirroring the EnumerateChild arm.
- ✅ **Gate (I.2 multi-controller arm):** `usb-multi-controller-smoke` — a second `qemu-xhci,id=xhci1,addr=0x7` with a `usb-mouse` behind it; asserts `XHCI:controller-1:ready` (the multiplexed subscribe succeeded end-to-end: kernel + driver_runtime + driver — emitted only after `init_interrupter_into` succeeds) and that a QMP mouse-move on the controller-1 device decodes (`USB_HID:mouse`), proving controller 1 is fully enumerated, routed, and serviced. Wired into `M3OS_USB_REGRESSION` + the AGENTS.md row. ✅ **I.4** — kernel `0.92.3`→`0.92.4`.
- ✅ **Perf follow-ups (landed in 92d).** Two performance items the maintainer asked to fold in:
  - **Interrupt moderation (IMOD).** `Controller::set_interrupt_moderation` applies a 1 ms (`IMODI = 4000`, 250 ns units) interrupter-moderation interval **per controller in `server::run`** (bring-up keeps `IMOD = 0` for a prompt first interrupt, so the validated bring-up path is untouched). Coalesces bulk-completion storms into ≤1 interrupt/ms; class-driver delivery is poll-driven and unaffected; redundant wakes that find an empty ring already do no MMIO (Optimization B). 1 ms also matches the USB HID reporting interval.
  - **Non-blocking control transfers → interleaved-drain (robustness-preserving).** The control-transfer poll (`wait_for_transfer_event`) was found to **deliberately poll** (not `irq.wait()`) for bare-metal robustness ("a controller may deliver zero MSI/MSI-X interrupts" — a *full* async/IRQ-driven control transfer would regress that). So instead of making control transfers async, the bounded poll now **interleaves servicing the OTHER controllers' event rings** (`drain_others` callback threaded `control_request`/`control_write` → `control_transfer` → `wait_for_transfer_event`; the server splits the controller slice into target + others) so a slow/dead-device control transfer on one controller can't overflow a co-resident controller's finite event ring. The dead-device timeout is tightened 400 ms → 200 ms. Single-controller (empty others → no-op drain) and bring-up enumeration (serial, no co-resident controllers) are behaviourally unchanged. *(Design fork — interleaved-drain vs full-async — surfaced to + chosen by the maintainer; full async deferred because it trades away the intentional zero-interrupt-hardware robustness.)*
  - **Validated:** the full `M3OS_USB_REGRESSION` suite (9 gates incl. the control-heavy `usb-hub-smoke` + `usb-multi-controller-smoke`) is green with both changes.
- **Still deferred (own measured changes):** full async EP0 control transfers (would need a thread-safe-malloc-free deferred-reply path AND would regress the zero-interrupt-hardware polling robustness) and extending the interleaved-drain to the bulk-OUT/`SubmitBulkIn` poll path (`wait_for_bulk_out_event`, the same pattern for steady-state storage/NIC IO).
- *Honest gate-falsifiability note:* `XHCI:controller-1:ready` is the load-bearing proof of the multiplexed-IRQ substrate. The `USB_HID:mouse` decode proves controller 1 is fully functional; it does **not** isolate the bit-directed IRQ path from the Message-wake all-drain safety net (a client poll would also drain controller 1), since both are correct. The bit-directed optimization itself is validated by design + the kernel unit test.

**Phase 92e — USB-Ethernet class drivers (live).**
- **G.1/G.2/G.3** live `usb-net` (CDC-ECM/NCM `RemoteNic` + the shared `ure` device-match registry).
- **C.4 (usb-net arm)** — `usb-net` releases its per-device state on an `attached:false` notice.
- **Gate (I.3 ethernet half):** the CDC-ECM/NCM arm of `usb-eth-smoke` — bare-metal/VFIO, skip-with-reason in CI. *QEMU has no CDC-ECM model; host-logic (G.1/G.2) is done.*

**Track I** — **I.4 (version bump) is done**: bumped `0.91.0`→**`0.92.0`** with the Phase 92 core (this PR), the AGENTS.md "kernel v0.92.0" line, and the USB capability-bullet rewrite. **Sub-phases 92a–92e land as `0.92.x` patch releases.** **I.5** (the `docs/92-usb-class-expansion.md` learning doc + `docs/README.md`/`codebase-map.md` links + flipping the roadmap README Phase 92 row to `Complete`) remains, scheduled to land with the last sub-phase. *Note: each per-sub-phase AGENTS.md gate row above lands **with** its sub-phase.*

### Coverage map — every open acceptance item → its sub-phase

| Open items (task IDs / lines) | Owner |
|---|---|
| A.4 (167–169), A.5 (178–180), A.2-reset (147) | 92a |
| D.3 (336–338), D.4 (350–352), D.5 (361–362), H.4 (116–118) | 92a |
| C.4 usb-storage detach + unmount (293/295) | 92a |
| I.1 hub+mount gate (488–490), I.2 mount/unmount arm (499) | 92a |
| B.1-decode, B.2-live, B.3 decode, B.4, H.2-test, B.1 readiness items | **92b — done (PR #254)** |
| C.4 usb-hid detach | **92b — done (PR #254)** |
| I.2 Report-Protocol arm (`usb-report-smoke`) | **92b — done (PR #254)** |
| E.1 (378–380), E.2 (392–394), E.3 (403–405) | 92c |
| I.3 audio gate (510, 512) | 92c |
| F.1, F.2 | **92d — done** |
| I.2 multi-controller arm (`usb-multi-controller-smoke`) | **92d — done** |
| G.1 (449–450), G.2 (461), G.3 (473–475) | 92e |
| C.4 usb-net detach (293) | 92e |
| I.3 CDC-ECM arm (511) | 92e |
| I.4 version bump (524–526) | **done — `0.92.0` with the core** |
| I.5 learning doc (539–541) | Track I close-out (lands with the last sub-phase) |
| *PR #252 readiness follow-ups (task IDs below):* | |
| H.6 length bounds, D.2 rw-ok comment, I.6 gate regression wiring | 92a |
| B.1 readiness items (wDescriptorLength read, doc header, hostile-count test) | 92b |
| H.5 slot-reclaim on unpackable handle | **92d — done** |

---

## Track H — Foundation & Carry-Over Hardening

> Land first: the class tracks need these residual paths, and the carry-over items must be closed before `GetDescriptors`/repeated control reads go live.

### H.1 — Live `GetDescriptors` for large-descriptor reads

**Files:**
- `userspace/drivers/xhci/src/server.rs`
- `userspace/lib/usb-core/src/protocol.rs`

**Symbol:** `handle_request` (the `REQ_GET_DESCRIPTORS` arm, today the `ENOSYS` default at `server.rs:406`), `UsbReply::Descriptors`, `UsbReply::ControlData`
**Why it matters:** descriptors are pre-resolved into `AttachNotice` at enumeration, but a Report-Protocol HID device (B.1) and CDC-ECM (G.1) must read a full **configuration / Report / CDC functional descriptor** at bind time — these exceed the ≤64-byte inline `ControlData` clamp. `GetDescriptors` must return the cached device + config descriptor blobs (already parsed during enumeration) so class drivers stop being limited to what `AttachNotice` carries.

**Acceptance:**
- [x] `UsbRequest::GetDescriptors { slot_id }` returns `UsbReply::Descriptors { device, config }` from the enumeration-time cache (not a fresh control read) for an enumerated device. — server arm wired to `Controller::cached_descriptors`; `XhciHostOps::{get_device_descriptor(len≥18),get_config_full}` cache the raw blobs into the per-slot `SlotContext`. Live class-driver read exercised by B.1/G.1.
- [x] A configuration descriptor larger than 64 bytes round-trips intact (bounded by `USB_MSG_MAX`=4096), proven by a host test over the wire codec. — `protocol::tests::descriptors_large_config_roundtrip` (512-byte config) PASSES.
- [x] The arm no longer returns `Error { code: ENOSYS }`; the `ENOSYS` default remains for `ConfigureEndpoints`/`SubmitTransfer` until H.4.

### H.2 — Capture non-matching events during a blocking control transfer

**File:** `userspace/drivers/xhci/src/controller.rs`
**Symbol:** `drain_for_transfer_event` (`controller.rs:1045-1077`), `drain_for_command_completion` (`controller.rs:765-798`), `capture_interrupt_report` (`controller.rs:1456-1502`)
**Why it matters:** the bulk-OUT path already captures concurrent interrupt/bulk-IN completions (`wait_for_bulk_out_event`), but a blocking **control** transfer still discards non-matching `TransferEvent`s. Once control traffic (Report-Protocol descriptor reads, LED `SET_REPORT`, GET_MAX_LUN) interleaves with active interrupt/bulk polling, a report completing mid-transfer would be dropped and its endpoint left un-rearmed — the second 78c carry-over item.

**Acceptance:**
- [x] `drain_for_transfer_event` routes non-matching IN-endpoint completions through `capture_interrupt_report` + deferred re-arm (mirroring `wait_for_bulk_out_event`), instead of discarding them. — applied to **both** `drain_for_transfer_event` (EP0 control) and `drain_for_command_completion` (Configure/Disable Slot); callers `wait_for_transfer_event`/`issue_command_and_wait` re-arm at `armed_len`.
- [x] A test (or instrumented run) issuing a control transfer while an interrupt endpoint is armed shows no dropped report and no un-rearmed endpoint. — **`usb-report-smoke` (Phase 92b)**: a `caps_lock` press issues a `SET_REPORT(Output)` EP0 control write while the keyboard's interrupt-IN endpoint is armed; the gate then injects a normal key (`b`) and asserts it still decodes (`USB_HID:key … sym=0x…62`), proving the interleaved control transfer dropped no report and left the endpoint armed.
- [x] No regression in `usb-smoke` (HID boot path) or `usb-eth-smoke` (bulk path). — `usb-smoke` PASSES (kbd+mouse decoded live); `usb-eth-smoke` needs the absent `ure` crate so it is not present in-tree (Track G).

### H.3 — Disable Slot reclamation

**Files:**
- `userspace/drivers/xhci/src/controller.rs`
- `kernel-core/src/usb/xhci/trb.rs`

**Symbol:** new `disable_slot(slot_id)` controller method; `TRB_TYPE_DISABLE_SLOT` (constant exists in `trb.rs`, no builder); `alloc_slot_context` (`controller.rs:1083-1128`, the allocate side)
**Why it matters:** `Enable Slot` allocates a slot at enumeration but nothing reclaims it. Hot-plug detach (Track C) and re-enumeration cycles would leak slot IDs and DCBAA entries until the controller's slot pool exhausts. Disable Slot is the matching teardown.

**Acceptance:**
- [x] A `disable_slot` command is issued (and its Command Completion drained) when a device detaches, freeing the DCBAA entry and slot-context allocations. — `Controller::disable_slot` (issues `Trb::disable_slot`, drains via `issue_command_and_wait`, zeroes `DCBAA[slot]`, drops the `SlotContext`) + the `Trb::disable_slot` builder (host-tested `encode_disable_slot`). The detach trigger is wired in Track C's `process_port_events`; proven end-to-end by `usb-hotplug-smoke`.
- [x] A repeated attach/detach loop (QMP `device_add`/`device_del`, ≥ slot-pool-count iterations) does not exhaust slots — the Nth attach still enumerates. — `usb-hotplug-smoke` runs 3 attach/detach cycles, each Enable+Disable Slot; all attaches enumerate (no exhaustion).
- [x] Slot-handle packing (`pack_handle`) correctly reuses a reclaimed slot without misrouting (`unpack_handle` round-trip holds). — the gate's per-cycle re-attach routes correctly (the device is found + enumerated each cycle).

### H.4 — Page-grant `SubmitTransfer` for oversized bulk data phases

> **Status: DELIVERED (zero-copy via shared memory).** Implemented as **option (a)** — a new capability-gated kernel **IOMMU-map syscall** — but over `sys_shm` rather than the move-based page-grant.
>
> Two things landed:
> 1. **The multi-sector inline path** was the real blocker for 4096-block filesystems, and is fixed: a real-world 4096-byte-block ext2 USB stick mounts + reads + writes over `SubmitBulkIn`/`SubmitBulkOut`. The fix root-caused a **bulk-OUT recv-truncation bug** (the xHCI server `recv`d with the 1522-byte Ethernet-MTU buffer → truncated >1522-byte `SubmitBulkOut` → device timeout) + a SHORT_PACKET completion-filter inconsistency.
> 2. **True zero-copy DMA** — `SYS_DEVICE_DMA_MAP_SHM` (0x112A) / `SYS_DEVICE_DMA_UNMAP_SHM` (0x112B): a claimed device IOMMU-maps a **shared-memory** region's contiguous frame run into its domain (phys→phys; identity-fallback when not translating), returning the device IOVA. `sys_shm` is the right substrate (the existing page-grant is a *move*, ill-suited for device reads; shm is a true *share* by integer id — bidirectional, no cap transfer, physically contiguous → a single TRB). The new `UsbRequest::SubmitShmTransfer` arm maps the shm, programs one bulk TRB straight at it (no `USB_MSG_MAX` copy), and unmaps; the kernel frees the IOMMU entry + shm pin on process exit. **Validated end-to-end** by `usb-storage-smoke`'s `USB_STORAGE:shm-dma-ok`: a 16-sector (8192-byte, > inline budget) WRITE(10)+READ(10) zero-copy round-trip in single descriptors, byte-identical.
>
> **Note:** H.6 fixed the inline budget at 7 sectors (3584 B) — the reply carries 3 bytes of wire overhead.

**Files:**
- `userspace/drivers/xhci/src/server.rs`
- `userspace/drivers/xhci/src/controller.rs`

**Symbol:** `UsbRequest::SubmitTransfer { slot_id, dci, grant: PageGrant }` (today the `ENOSYS` default), `PageGrant` (`protocol.rs`)
**Why it matters:** inline `SubmitBulkOut`/`PollBulkIn` cap a data phase at `USB_MSG_MAX`=4096. A multi-sector mass-storage READ(10)/WRITE(10) (D.5) can exceed that; the latent page-grant transport maps a shared buffer and programs bulk TRBs directly against it, avoiding per-chunk IPC.

**Acceptance:**
- [x] `SubmitShmTransfer` maps the shared region (`sys_device_dma_map_shm`), programs a Normal TRB (IOC) straight at its device IOVA, rings the doorbell, and completes off the Transfer Event — returning `UsbReply::TransferComplete { transferred, completion_code }`. (Delivered over `sys_shm` rather than the move-based `PageGrant`; `controller::submit_bulk_iova` + the `SubmitShmTransfer` server arm.)
- [x] A > `USB_MSG_MAX` transfer (a 16-sector / 8192-byte WRITE(10)+READ(10)) completes via the zero-copy shm path in **one** descriptor (no inline chunking); ≤ budget transfers still use the inline path. — `usb-storage-smoke` `USB_STORAGE:shm-dma-ok`.
- [x] The mapping is torn down on completion (`sys_device_dma_unmap_shm`, keyed by IOVA) and on process exit (`release_shm_dma_maps_for_pid`, before claim release) — no IOMMU-entry or shm-ref leak across repeated transfers.

### H.5 — Reclaim the slot when a hot-plug handle can't be packed (PR #252 readiness)

**File:** `userspace/drivers/xhci/src/server.rs`
**Symbol:** `process_port_events` (the attach arm, ~`server.rs:233`, where `pack_handle` returns `None`)
**Why it matters:** on hot-plug attach the server runs Enable Slot (allocating a hardware slot + `SlotContext`) **before** `pack_handle(ctrl_idx, slot_id)`. If `pack_handle` returns `None` (`ctrl_idx > 3`, i.e. ≥5 controllers, or hw slot > 63) the code logs "unpackable handle" and continues without `disable_slot` — leaking the very slot H.3 set out to reclaim. Reachable only in the multi-controller regime, so it pairs with Track F / Phase 92d, but it is live code today. (The identical bring-up-path case at ~`server.rs:300` predates this phase.)

**Acceptance:**
- [x] When `pack_handle` returns `None` on the attach path, the server issues `disable_slot` for the just-enabled slot before dropping the device (no slot leak) and logs the drop. — `process_port_events`'s attach `None` arm now calls `c.disable_slot(irq, notice.slot_id)` (the real hardware slot, before it is overwritten with the packed handle) and logs `… unpackable handle (slot reclaimed)`, mirroring the EnumerateChild arm.
- [~] An instrumented run in the ≥5-controller / slot>63 regime shows no slot-pool leak across repeated unpackable attaches. — the reclaim is exercised on the code path; a ≥5-controller QEMU topology to drive the `pack_handle == None` branch live is not wired (QEMU multi-xHCI tops out well below the pathological regime), so the reclaim is verified by inspection + the shared `disable_slot` path the hot-plug gate already covers.

### H.6 — Bound device-controlled lengths against `USB_MSG_MAX` (PR #252 readiness)

**Files:**
- `userspace/drivers/xhci/src/server.rs`
- `kernel-core/src/usb/enumerate.rs`

**Symbol:** the cached config-descriptor read (uses the device's `wTotalLength`) + `cache_config_descriptor` + the `Descriptors` reply encode; the `SubmitBulkIn { len }` arm (~`server.rs:504`)
**Why it matters:** both paths trust a device-/caller-supplied length. A device reporting `wTotalLength` near 65535 makes the server hold a ~64 KiB blob and emit a `Descriptors` reply far over `USB_MSG_MAX` (4096); a `SubmitBulkIn { len }` near/over 4090 produces an oversized `BulkData` reply. Both **fail closed** today (the client's 4096-byte buffer truncates → length-prefixed `decode` returns `None`; no panic/OOB) and the only live callers stay well under budget — so this is hardening, not a live bug. An explicit server-side cap (reject/clamp + logged error) is clearer than silent truncation and removes the per-slot memory-amplification window from a compromised/buggy class driver.

**Acceptance:**
- [x] The server rejects (or clamps with a logged error) a cached config blob / `Descriptors` reply that would exceed `USB_MSG_MAX`, instead of relying on client-side truncation. — `GetDescriptors` arm returns `Error{EINVAL}` + logs when `device.len()+config.len()+5 > USB_MSG_MAX`.
- [x] `SubmitBulkIn { len }` that would overflow `USB_MSG_MAX` returns an explicit `UsbReply::Error` rather than a silently-truncated `BulkData`. — `SubmitBulkIn` arm returns `Error{EINVAL}` + logs when `len+4 > USB_MSG_MAX`. (This bound **caught a real integration bug**: the usb-storage daemon's initial 8-sector/4096-byte chunk overflowed the reply by 3 bytes; fixed to a 7-sector cap — see D.4.)
- [x] Test coverage asserts the over-budget case takes the error path (the under-budget BOT lengths 13/36/512 are unaffected). — `protocol::tests::{bulk_in_len_budget_matches_encoded_size, descriptors_over_budget_exceeds_usb_msg_max}`.

---

## Track A — Multi-Tier Hub Enumeration

### A.1 — Promote `usbhub` to a resident live IPC consumer

**File:** `userspace/drivers/usbhub/src/main.rs`
**Symbol:** `program_main` (today: logs `BOOT_LOG_MARKER`, calls `classify_hub_interface(0x09)`, returns 0); `classify_hub_interface` → `kernel_core::usb::hub::is_hub_interface`
**Why it matters:** at 1.0 the hub daemon exits immediately after proving the kernel-core link. Track A turns it into a resident process that waits on the `usb` service, walks `NextAttach` (cursor 0..until `notice.is_none()`), and filters for `interface_class == CLASS_HUB` (0x09) — the entry point for everything else in this track.

**Acceptance:**
- [x] `usbhub` no longer returns immediately from `program_main`; it waits on the `usb` service (`USB_SERVICE_NAME`) and walks the `NextAttach` cursor. — `program_main` rewritten as a resident walker.
- [x] It discovers a `CLASS_HUB` device via the `NextAttach` cursor walk and logs a hub-found marker. — server `device_info_from_ctx` now surfaces `CLASS_HUB`; the daemon logs `usbhub: bound hub slot=…` (proven by `usb-hub-smoke`).
- [x] No regression in `xhci-bringup-smoke` / `usb-smoke` (HID boot path) — `usb-smoke` PASSES; the surfaced hub does not disturb the HID/bulk class drivers (they filter by class).

### A.2 — Hub bring-up: descriptor read + per-port power/reset

**Files:**
- `userspace/drivers/usbhub/src/main.rs` (+ a new `hub_enumerate` module)
- `kernel-core/src/usb/hub.rs`

**Symbol:** `kernel_core::usb::hub::{get_hub_descriptor, HubDescriptor::parse, set_port_feature, clear_port_feature, enumerate_hub_ports, PORT_POWER, PORT_RESET}`; issued over `UsbRequest::ControlRequest`
**Why it matters:** all the hub-class control encoders are host-tested in `kernel-core` but have no live caller. A.2 issues `GET_DESCRIPTOR(Hub)` to learn `bNbrPorts`/`bPwrOn2PwrGood`, then `SET_FEATURE(PORT_POWER)` per port (waiting `bPwrOn2PwrGood × 2 ms`) and `SET_FEATURE(PORT_RESET)` on a detected connection — the standard hub power/reset sequence.

**Acceptance:**
- [x] `HubDescriptor::parse` is fed a live `GET_DESCRIPTOR(Hub)` reply; `bNbrPorts` drives the per-port loop. — `enumerate_hub` issues `get_hub_descriptor` over EP0 `ControlRequest`, parses, logs `XHCI_HUB:enumerated ports=N` (proven by `usb-hub-smoke`).
- [x] `SET_FEATURE(PORT_POWER, port)` is issued for every downstream port, honoring the `bPwrOn2PwrGood` settle delay. — per-port `set_port_feature(PORT_POWER, port)` + a `bPwrOn2PwrGood × 2 ms` settle.
- [x] On a port-connection, `SET_FEATURE(PORT_RESET, port)` is issued and the `C_PORT_RESET` change bit is acked with `CLEAR_FEATURE`. — **now exercised live against a connected downstream device**: `usb-hub-smoke` attaches a full-speed usb-mouse behind the hub (QEMU port-path `3.1`); `usbhub` sees `port_status_connected`, resets (`PORT_RESET` → poll-for-`PORT_ENABLE` → `CLEAR_FEATURE(C_PORT_RESET)`), then tier-2-enumerates it (`XHCI_HUB:child-enumerated`).

### A.3 — `GET_PORT_STATUS` encoder + port-status bitmap helpers

**File:** `kernel-core/src/usb/hub.rs`
**Symbol:** new `get_port_status(port)` SetupPacket encoder (bmRequestType `0xA3`, bRequest `0x00`, wIndex=port, wLength=4) + status/change bitmap helpers (CCS, PE, RESET, C_CONNECTION, C_RESET)
**Why it matters:** the hub state machine needs to read the 4-byte port status to detect connection (CCS), confirm enable (PE), and clear change bits (RW1C). The encoder and bit helpers are the only hub primitives `kernel-core` does not yet have; they belong next to `set_port_feature`/`clear_port_feature` and must be host-tested like the rest.

**Acceptance:**
- [x] `get_port_status` encodes the class GET_STATUS SetupPacket; host test asserts the exact 8 bytes. — `get_port_status(port)` (bmRequestType `0xA3`, bRequest `0x00`, wValue 0, wIndex=port, wLength=4); host tests `get_port_status_port{1,3}_encoding` assert the 8 bytes.
- [x] Bitmap helpers decode CCS/PE/RESET (bytes 0–1) and C_CONNECTION/C_RESET (bytes 2–3) per USB 2.0 §11.24.2.7; host-tested against known status words. — `port_status_{connected,enabled,resetting}` + `port_change_{connection,reset}` with named bit consts; short-slice inputs return false (no panic). 36 hub tests pass.
- [x] The hub state machine polls `GET_PORT_STATUS` after `PORT_RESET` until `PORT_ENABLE` is set. — consumed by the live `usbhub` walker (A.2 `enumerate_hub`).

### A.4 — Surface hubs + assign tier-2+ slots in the xHCI server

**File:** `userspace/drivers/xhci/src/server.rs`
**Symbol:** `device_info_from_ctx` (`server.rs:60-152`, currently skips `CLASS_HUB`); the `Enable Slot`/`Address Device` path (`enumerate.rs::XhciHostOps::{enable_slot, address_device}`, `controller.rs::alloc_slot_context`)
**Why it matters:** the server only `scan_ports`-enumerates root-hub ports and never surfaces `CLASS_HUB` interfaces to a class driver. A.4 publishes hub-class `AttachNotice`s to `usbhub` and accepts a "enumerate child device" request that runs Enable Slot / Address Device / Configure Endpoint for a device behind a hub, addressed by route string (A.5).

**Acceptance:**
- [x] `device_info_from_ctx` surfaces `CLASS_HUB` interfaces (a hub appears in the `NextAttach` walk). — landed in Track A.1; `usb-hub-smoke` asserts `usbhub: bound hub`.
- [x] A device behind the hub receives Enable Slot / Address Device / Configure Endpoint and reaches `Configured`, surfaced as its own `AttachNotice`. — new `UsbRequest::EnumerateChild` server arm + `enumerate_child` run the full sequence with the route string; **`usb-hub-smoke` now attaches a full-speed usb-mouse behind the hub and asserts `XHCI_HUB:child-enumerated class=3` live**.
- [x] The multi-controller `owner!`/`unpack_handle` routing is preserved for tier-2+ slots (no misroute). — `EnumerateChild` unpacks the parent handle for the controller index and `pack_handle`s the child slot; the existing multi-controller gates are unaffected.

### A.5 — Drive the `PortTopology` route string for tier-2+ addressing

**File:** `kernel-core/src/usb/hub.rs`
**Symbol:** `PortTopology::{add_root_port, add_child_port, route_string, root_hub_port, depth_of}`, `MAX_HUB_DEPTH` (5); `kernel-core/src/usb/xhci/context.rs::{slot_context_dword0, slot_context_dword1}`
**Why it matters:** the flat-arena topology tree + 20-bit route-string computation (xHCI §8.9) are fully host-tested but have **no live caller**. A.5 builds the tree as hubs/devices are discovered and feeds `route_string` into Slot Context dword0 and `root_hub_port` into dword1 — the addressing a device deeper than tier 1 requires.

**Acceptance:**
- [x] A device behind one hub gets a non-zero route string from `route_string`; the root-hub port goes in the Slot Context Root Hub Port Number field (not the route string). — `EnumContext.route_string` is threaded into `slot_context_dword0` (host test `slot_context_route_string_encoded_for_tier2_device`); `usbhub` computes it via `PortTopology::{add_root_port,add_child_port,route_string,root_hub_port}` and the live `usb-hub-smoke` enumerates the behind-hub device through it.
- [x] Nesting beyond `MAX_HUB_DEPTH` is rejected gracefully (`add_child_port` returns `None`, daemon logs + skips, no panic). — `usbhub` matches `add_child_port → None` and logs `nesting beyond MAX_HUB_DEPTH — skipping` (host-tested `PortTopology` depth limit).
- [x] Host tests cover a two-tier route-string value and the `root_hub_port` walk. — kernel-core `usb::hub` PortTopology tests + the enumerate.rs route-string slot-context test.

---

## Track B — HID Report Protocol

### B.1 — Wire `parse_report_descriptor` into the live input path

**Files:**
- `kernel-core/src/usb/hid_report.rs`
- `userspace/drivers/usb-hid/src/main.rs`

**Symbol:** `parse_report_descriptor` + `ReportField { usage_page, usage, bit_offset, bit_size }` (host-tested, **zero call sites** today); `usb-hid` device-bind path (`boot_protocol_init`, `poll_keyboard`, `poll_mouse`)
**Why it matters:** 1.0 ships Boot Protocol only; the Report Descriptor parser is dead code. B.1 reads the Report Descriptor at device bind (via `GetDescriptors`/`ControlRequest`, H.1), parses it into a `ReportField` array stored in per-device state, and decodes variable-format reports by that layout instead of the fixed boot offsets.

**Acceptance:**
- [x] `parse_report_descriptor` gains a live call site in `usb-hid` at device bind; the parsed `ReportField` array is stored per device. — `fetch_report_fields` issues `GET_DESCRIPTOR(Report)` over EP0 for each `CLASS_HID` interface, parses it, stores `HidDevice.report_fields`, and logs `USB_HID:report-parsed proto=P fields=N` (asserted by `usb-smoke`).
- [x] A Report-Protocol device's reports decode by the parsed field layout (not the boot 8-byte/3-byte assumption). — **Phase 92b (B.2-live)**: `usb-hid` classifies a non-boot HID pointer as `ReportPointer` and decodes its interrupt-IN reports with `decode_pointer_report` against the stored `ReportField` layout. Live-validated by `usb-report-smoke` (a `usb-tablet` → `HID_REPORT:pointer`).
- [x] The existing Boot-Protocol keyboard/mouse path is unchanged (`usb-smoke` still PASSES). — `usb-smoke` PASSES (kbd+mouse decode live + render); the boot decode path is untouched.
- [x] **(PR #252 readiness, 92b)** `usb-hid` reads the HID descriptor's `wDescriptorLength` instead of the hard-coded `REQ_LEN = 256` (`fetch_report_fields`), so the Report descriptor is not over-read into zero padding that parses as spurious trailing zero-width fields. — `report_descriptor_len` reads the config descriptor via `GetDescriptors` (H.1) and `hid_report_descriptor_len` scans it for the interface's HID-descriptor Report-entry `wDescriptorLength`; `fetch_report_fields` requests exactly that (falling back to 256 only if unavailable).
- [x] **(PR #252 readiness, 92b)** The stale `hid_report.rs` module doc header ("not wired to any live device") is corrected — the parser is now called live at bind (B.1). — header rewritten to "live, host-tested" describing the live bind-time read + `decode_pointer_report` use.
- [x] **(PR #252 readiness, 92b)** A host test feeds `parse_report_descriptor` a hostile Report Count/Size (e.g. a 4-byte `0xFFFFFFFF` count) to lock in the saturating/clamped (≤65536 fields) behavior now that the parser sees live device input. — `hid_report::tests::hostile_report_count_is_bounded` (Usage Min 1 / Max 0xFFFF range + a 4-byte `0xFFFFFFFF` Report Count) asserts `<= MAX_REPORT_FIELDS` (65536) and no panic.

### B.2 — Multi-axis / extra-button / scroll decode (touchpad + gaming mouse)

**Files:**
- `kernel-core/src/usb/hid_report.rs`
- `kernel-core/src/usb/hid.rs`
- `userspace/drivers/usb-hid/src/main.rs`

**Symbol:** `parse_report_descriptor` (enhance: Usage Min/Max ranges + Report IDs — skeleton-limited to one Usage and no Report ID today); a data-driven report decoder mirroring `BootKeyboardDecoder`; `PointerEvent` (extend axes)
**Why it matters:** a gaming mouse reports a scroll wheel + extra buttons; a touchpad reports X/Y/pressure + contact IDs. The parser must emit multiple `ReportField`s for Usage ranges and respect Report IDs, and `usb-hid` must unpack arbitrary bit fields and map usages (X=0x01:0x30, Y=0x01:0x31, buttons=0x09:0x01..) to `mouse_server` events.

**Acceptance:**
- [x] `parse_report_descriptor` emits a `ReportField` per usage for a Usage Min/Max range and tags fields with their Report ID; host tests cover both. — `kernel_core::usb::hid_report` Usage-Min/Max range expansion + per-Report-ID `report_id` tagging + offset reset; host tests `usage_min_max_range_expands_to_one_field_per_usage` + `two_report_ids_tag_fields_and_reset_offset`. **Phase 92b also added** `is_relative` + multi-usage-list parsing (`Usage X; Usage Y; Input`) + `decode_pointer_report`, and `usb-hid` decodes live against this layout (validated by `usb-report-smoke`).
- [x] A Report-Protocol gaming mouse delivers correct X/Y + a scroll axis + ≥4 buttons through `mouse_server` (`USB_HID:mouse` sentinels reflect the extra axes/buttons). — `decode_pointer_report` extracts X/Y (relative or absolute by the field's `is_relative` flag), the Generic-Desktop Wheel (signed), and up to 32 Button-page buttons against the parsed layout; `poll_report_pointer` injects motion + `wheel_dy` + per-button Down/Up edges into `mouse_server`. The decode is host-tested (gaming-mouse-style relative + button descriptors) and the live path is validated by `usb-report-smoke` via a `usb-tablet` (absolute X/Y + wheel + 3 buttons) → `HID_REPORT:pointer`. *(QEMU ships no multi-button gaming-mouse Report-Protocol model, so the ≥4-button case is host-tested + bare-metal; the tablet exercises the same decode/inject path live.)*
- [x] A single-pointer touchpad maps to pointer motion (multi-touch contact tracking is explicitly deferred — see design doc). — a single-pointer absolute device (the `usb-tablet`) maps to `PointerEvent { abs_position }` motion through `mouse_server`; multi-touch contact tracking remains deferred per the design doc.

### B.3 — Consumer-control keys (media / brightness)

**Files:**
- `kernel-core/src/usb/hid.rs`
- `kernel-core/src/input/keymap.rs`

**Symbol:** `hid_usage_to_keycode` (extend to Usage Page 0x0C, Consumer); `Keycode` enum (add consumer slots)
**Why it matters:** media keys (volume up/down/mute, play/pause) and brightness live on HID Usage Page 0x0C, which Boot Protocol cannot express. B.3 maps Consumer usages to keycodes so `display_server` can route them to `audio_server` / brightness control.

**Acceptance:**
- [x] `hid_usage_to_keycode` maps the Consumer page (volume up/down/mute at minimum) to distinct keycodes; host-tested. — `hid_consumer_usage_to_keycode` (Usage Page 0x0C) maps Mute (0xE2), Volume Increment (0xE9), Volume Decrement (0xEA), Play/Pause (0xCD) to distinct `KEY_MUTE`/`KEY_VOLUMEUP`/`KEY_VOLUMEDOWN`/`KEY_PLAYPAUSE`; host tests (`consumer_*`) assert the mappings + distinctness.
- [~] A Report-Protocol keyboard's volume keys are decoded and routed (volume keys reach `audio_server`). — **implemented + host-tested decode, live arm bare-metal.** `decode_consumer_usages` (Usage Page 0x0C bitmap, host-tested) + `usb-hid`'s `poll_report_consumer`/`inject_consumer_key` decode a Report-Protocol consumer interface and inject the mapped keycode (Down+Up) into `kbd_server` → `display_server` → `audio_server` (the proven keyboard inject path; `USB_HID:consumer`). QEMU emulates no device that emits HID consumer reports, so the live routing is bare-metal/VFIO-validated (skip-with-reason in CI), mirroring the established `usb-eth-smoke`/`wifi-smoke` pattern; the decode logic is host-tested.

### B.4 — Keyboard LED output via `SET_REPORT`

**Files:**
- `userspace/drivers/usb-hid/src/main.rs`
- `userspace/kbd_server/src/main.rs`

**Symbol:** new `SET_REPORT` issuance over `UsbRequest::ControlWrite` (bmRequestType `0x21`, bRequest `0x09`, wValue = report-type/ID); `kbd_server` LED-state tracking
**Why it matters:** Boot keyboards are input-only; Report-Protocol keyboards expose OUTPUT items for Caps/Num/Scroll Lock LEDs. B.4 tracks lock state in `kbd_server` and issues `SET_REPORT` (over the live `ControlWrite` path) with the LED bitfield — the one Track-B path that writes back to the device.

**Acceptance:**
- [x] Toggling Caps Lock updates LED state and issues a `SET_REPORT` `ControlWrite` carrying the LED bitfield. — `usb-hid` tracks Caps/Num/Scroll Lock per device (`maybe_update_leds` watches the decoded boot-keyboard lock-key Down edges) and `set_keyboard_leds` issues `SET_REPORT(Output)` (bmRequestType 0x21 / bRequest 0x09 / wValue 0x0200) over the live `ControlWrite` EP0 path. Live-validated by `usb-report-smoke` → `USB_HID:led`. *(LED authority lives in `usb-hid` rather than `kbd_server` for this sub-phase since `usb-hid` decodes the keyboard's own edges; cross-driver PS/2↔USB lock-LED coherence via `kbd_server` is a noted follow-up.)*
- [x] The transfer uses the H.2-hardened control path (a concurrent interrupt report is not dropped during the `SET_REPORT`). — `usb-report-smoke` injects a normal key immediately after the `SET_REPORT` and asserts it still decodes (`USB_HID:key … sym=0x…62`); the EP0 control write captured no interrupt-IN report and left the endpoint armed (the H.2 capture path).
- [x] Boot keyboards (no OUTPUT items) are unaffected — no `SET_REPORT` issued, no error. — `SET_REPORT` is issued only when a lock-key Down edge is decoded; mice/tablets and any non-keyboard interface decode no lock keys, so they never trigger a `SET_REPORT`. The existing `usb-smoke` (boot kbd/mouse, no lock keys typed) passes unchanged with no LED writes.

---

## Track C — Live Hot-Plug Event Surface

### C.1 — Port Status Change → `AttachNotice` live pipeline

**Files:**
- `userspace/drivers/xhci/src/server.rs`
- `userspace/drivers/xhci/src/controller.rs`
- `kernel-core/src/usb/xhci/{trb.rs, port.rs}`

**Symbol:** `parse_port_status_change` / `PortStatusChangeEvent` (`trb.rs`, decoder exists, **no live handler**); `Portsc` accessors `csc()`/`prc()`/`plc()` + `portsc_clear_change` (`port.rs`, RW1C-safe); `service_interrupt_events` (`controller.rs:1383-1448`)
**Why it matters:** at 1.0 the device table is built once at bring-up; Port Status Change events are decoded but nothing reacts. C.1 makes the server read PORTSC on a change event, classify CSC/PRC/PLC, and drive attach/detach — the foundation of hot-plug.

**Acceptance:**
- [x] A Port Status Change event on a root-hub port triggers a PORTSC read and a CSC/PRC/PLC classification (not just an event decode). — `on_port_status_change` (now `&mut self`) reads PORTSC and classifies CSC connect vs disconnect; PRC is acked in the `reset_port_with_speed` path. Proven by `usb-hotplug-smoke`.
- [x] A CSC attach runs `run_enumeration` for the new device and publishes its `AttachNotice` dynamically (no server restart). — `process_port_events` → `enumerate_port` → `served.push`; `USB_HOTPLUG:attached` asserted by the gate.
- [x] Change bits are acked via the RW1C-safe `portsc_clear_change` (no accidental status-bit clear). — unchanged RW1C ack in `on_port_status_change`.

### C.2 — Detach notification (`attached: false`)

**File:** `userspace/lib/usb-core/src/protocol.rs` (consumer in `userspace/drivers/xhci/src/server.rs`)
**Symbol:** `AttachNotice.attached` (bool, already on the 21-byte wire format; encoded `as u8`, decoded `!= 0` — never set false at 1.0)
**Why it matters:** the protocol already carries a detach flag, but nothing emits it. C.2 publishes `AttachNotice { attached: false, .. }` on a CSC-clear (device removed) so class drivers learn a device went away.

**Acceptance:**
- [x] A device removal produces an `AttachNotice` with `attached == false` carrying the departing device's slot/port/class. — `process_port_events` flips `served[pos].attached = false` for the departing (ctrl,port). Proven by `USB_HOTPLUG:detached`.
- [x] The `NextAttach` cursor walk and detach stream coexist without losing either (a removal is observed by a class driver polling `NextAttach`). — `served` is append-only so cursors stay stable; the detached entry remains visible with `attached: false`.

### C.3 — Dynamic re-enumeration on attach

**File:** `userspace/drivers/xhci/src/controller.rs`
**Symbol:** `run_enumeration` (`kernel-core/src/usb/enumerate.rs:377-548`) driven from the live event path; `scan_ports` (`controller.rs:1802-1823`, today bring-up-only)
**Why it matters:** the Enable Slot → Address Device → Configure Endpoint sequence runs once at bring-up. C.3 reuses the *stateless* enumeration state machine on demand when a port newly connects, so a freshly attached device is brought to `Configured` without re-running global bring-up.

**Acceptance:**
- [x] A device attached after boot (QMP `device_add`) is enumerated to `Configured` and surfaced via `AttachNotice` — no daemon restart. — `usb-hotplug-smoke` `device_add usb-mouse` → `USB_HOTPLUG:attached`.
- [x] Re-attaching after a detach re-enumerates cleanly (fresh slot via H.3), proven by an attach/detach/attach cycle. — the gate runs **3 attach/detach cycles**, each re-enumerating + reclaiming the slot (no exhaustion).

### C.4 — Propagate detach to class drivers + slot teardown

**Files:**
- `userspace/drivers/usbhub/src/main.rs`
- `userspace/drivers/usb-hid/src/main.rs`
- `userspace/drivers/usb-storage/src/main.rs` (new — see D)
- `userspace/drivers/usb-net/src/main.rs` (new — see G)

**Symbol:** per-driver detach handler keyed on `AttachNotice { attached: false }`; `disable_slot` (H.3)
**Why it matters:** a clean detach must release the class driver's capabilities and reclaim the slot, or a removed flash drive leaves a stale `/mnt/usb<n>` mount and a leaked slot. C.4 wires each class driver to drop its device state on detach and the server to Disable Slot.

**Acceptance:**
- [~] `usb-hid`/`usb-storage`/`usb-net` each release their per-device state on an `attached: false` notice for a slot they own. — **usb-hid (Phase 92b) + usb-storage (Phase 92a) done; usb-net → Phase 92e.** `usb-hid`'s `reconcile_attachments` re-walks the `NextAttach` table every ~200 ms and drops a held device whose latest entry is `attached: false` (resolving by the *latest* `(slot_id, interface_num)` entry so a reclaimed/re-packed slot is never confused with a stale detached one), logging `usb-hid: released slot=N`. Live-validated by `usb-hotplug-smoke` (3 cycles: `usb-hid: hot-attached` then `usb-hid: released` each cycle).
- [x] The server issues Disable Slot for the departed device (H.3) and the slot is reusable. — `process_port_events` calls `Controller::disable_slot`; the 3-cycle gate proves the slot is reused without exhaustion.
- [ ] Unplugging a mounted USB stick unmounts `/mnt/usb<n>` without wedging the VFS (Track D integration). — **Phase 92a** (pairs with the D.4 mount).

---

## Track D — USB Mass Storage (BOT + UAS)

### D.1 — BOT data path on the Phase 96 inline bulk primitives

**Files:**
- `userspace/drivers/usb-storage/src/main.rs` (new crate)
- `userspace/lib/usb-core/src/protocol.rs` (reused, unchanged)

**Symbol:** `UsbRequest::{PollBulkIn, SubmitBulkOut}` + `UsbReply::BulkData` (LIVE); controller `arm_bulk_in`/`take_bulk_report`/`submit_bulk_out`; `AttachNotice.{bulk_in_dci, bulk_in_mps, bulk_out_dci, bulk_out_mps}` (Phase 96 fields)
**Why it matters:** mass storage is a bulk-class device — the exact transport the Phase 96 `ure` NIC already proved. D.1 stands up a new `usb-storage` ring-3 driver that binds a `CLASS_MASS_STORAGE` (0x08) interface from the `NextAttach` walk and drives its bulk IN/OUT pair using the existing primitives — **no new transport**. (New crate ⇒ the four-place wiring from AGENTS.md: workspace member, `xtask` `bins`, ramdisk `BIN_ENTRIES`, service config.)

**Acceptance:**
- [x] `usb-storage` binds a `CLASS_MASS_STORAGE` interface (`bulk_in_dci`/`bulk_out_dci` from `AttachNotice`) and exchanges bytes over `SubmitBulkOut`/`PollBulkIn`. — the new `usb-storage` daemon binds the device and completes a **full BOT CBW-out + CSW-in round-trip** (`GET_MAX_LUN` class control + `TEST UNIT READY`) over the bulk pair on a real (SuperSpeed) device; `usb-storage-smoke` asserts `USB_STORAGE:bot-ok`.
- [x] The driver guards on `bulk_in_dci != 0` / `bulk_out_dci != 0` before issuing bulk requests (a device with no bulk pair is rejected, not crashed). — the `NextAttach` bind loop filters `interface_class == 0x08 && bulk_in_dci != 0 && bulk_out_dci != 0`.
- [x] The crate is wired in all four places and appears in the ramdisk (`execve` resolves it). — workspace member + `xtask` `bins` + ramdisk `DRIVERS_ENTRIES` (`/drivers/usb-storage`) + init `usb-storage.conf` (daemon, `depends=xhci_driver`); the daemon spawns + binds the device, proven by the gate.
- [x] **The bulk-IN data phase (INQUIRY + READ CAPACITY) round-trips.** — RESOLVED via a new synchronous, single-TRB `UsbRequest::SubmitBulkIn` / `Controller::submit_bulk_in` (one bulk-IN TRB per BOT phase, no streaming auto-re-arm). Root cause of the prior `cc=6` STALL: the Phase 96 streaming bulk-IN path arms `depth`=4 auto-re-armed TRBs (correct for a NIC), so after the device's data + CSW the surplus IN tokens — issued while the device is back in CBW-wait — made it STALL the endpoint. The synchronous path never issues a surplus IN token. `usb-storage-smoke` now asserts `USB_MASS_STORAGE:ready blocks=16384 bsize=512` (8 MiB scratch image). Unblocks **D.4**.

### D.2 — CBW/CSW framing + SCSI command subset + `GET_MAX_LUN`

**File:** `userspace/drivers/usb-storage/src/main.rs`
**Symbol:** `Cbw` (31-byte Command Block Wrapper), `Csw` (13-byte Command Status Wrapper); SCSI ops TEST UNIT READY, INQUIRY, READ CAPACITY(10), READ(10), WRITE(10), REQUEST SENSE; `GET_MAX_LUN` over `UsbRequest::ControlRequest`
**Why it matters:** BOT wraps each SCSI command in a CBW on bulk-out and reads a CSW on bulk-in. Parsing SCSI in the ring-3 daemon keeps the kernel SCSI-unaware. `GET_MAX_LUN` (a class control-IN, over the live `ControlRequest`) tells the driver how many logical units the device exposes.

> **Implementation note:** the pure BOT/SCSI codec lives in `kernel-core/src/usb/mass_storage.rs` (host-testable, the `hid_report`/`hub` pattern), consumed by the future `usb-storage` daemon — so the kernel stays SCSI-unaware (kernel-core is a shared lib, not called by the kernel binary).

**Acceptance:**
- [x] `Cbw`/`Csw` encode/decode are host-tested against known byte layouts (dCBWSignature `USBC`, dCSWSignature `USBS`). — `kernel_core::usb::mass_storage` `Cbw::encode`/`Csw::parse` + 31 host tests (signatures, tag/len LE, CDB pad, short-buffer rejection).
- [x] `INQUIRY` + `READ CAPACITY(10)` return device identity and block count; `READ(10)`/`WRITE(10)` move sectors; a failed command surfaces `REQUEST SENSE`. — codec host-tested (CDB builders big-endian, `InquiryData`/`ReadCapacity10` parsers) **and the live BOT data movement is proven**: the `usb-storage` daemon reads INQUIRY + READ CAPACITY, and a `WRITE(10)` + `READ(10)` sector round-trip verifies byte-identical (`USB_STORAGE:rw-ok` in `usb-storage-smoke`) — WRITE data-OUT over `SubmitBulkOut` + READ data-IN over `SubmitBulkIn`. (REQUEST SENSE builder host-tested; surfaced on a failed command.)
- [x] `GET_MAX_LUN` is issued over `ControlRequest`; a device reporting STALL is treated as single-LUN. — `get_max_lun(iface)` SetupPacket encoder host-tested (`A1 FE 00 00 iface 00 01 00`); live issuance + STALL→single-LUN policy is the daemon (D.1).
- [ ] **(PR #252 readiness, 92a)** A comment in `cmd_usb_storage_smoke` notes that the `USB_STORAGE:rw-ok` round-trip depends on the 8 MiB / 512-byte scratch geometry (a non-512 device safely skips the round-trip, so the gate times out rather than false-passing).

### D.3 — UAS (USB Attached SCSI)

**File:** `userspace/drivers/usb-storage/src/main.rs`
**Symbol:** UAS pipe set (command/status/data-in/data-out) + stream IDs + task-management; selected when the device advertises the UAS alt-setting
**Why it matters:** BOT is a single-command-in-flight legacy protocol; USB 3.0 drives advertise UAS for queued, higher-throughput SCSI over streams. D.3 selects UAS when present and falls back to BOT otherwise.

**Acceptance:**
- [ ] A device advertising the UAS alt-setting is driven over the UAS pipes with stream IDs; a BOT-only device falls back to D.2.
- [ ] Command queuing (≥2 in flight) is exercised on a UAS device; a task-management abort is issued and acknowledged.
- [ ] Selection is logged (`usb-storage: transport=uas|bot`).

### D.4 — `RemoteBlockDevice` facade + `/mnt/usb<n>` mount

**Files:**
- `userspace/drivers/usb-storage/src/main.rs`
- the shared block protocol (as used by `nvme`/`ahci` drivers)

**Symbol:** a `RemoteBlockDevice`-style facade per LUN (reusing the Phase 77 ring-3 NVMe hosting pattern); VFS mount under `/mnt/usb<n>`
**Why it matters:** exposing each LUN as a `RemoteBlockDevice` over the shared block protocol means the VFS mount path needs no modification — a USB stick mounts exactly like the SATA/NVMe disks.

**Acceptance:**
- [x] Each mass-storage LUN registers as a block device over the shared block protocol (`READ(10)`/`WRITE(10)` back the block read/write IPC). — the resident `usb-storage` daemon registers `usb0.block` and serves `BLK_READ`/`BLK_WRITE`/`BLK_FLUSH`/`BLK_STATUS`, chunking each into ≤7-sector BOT READ(10)/WRITE(10) transfers. The kernel `blk::remote` multi-device registry routes `dev_id>=1` to it.
- [x] An ext2 USB stick mounts under `/mnt/usb0`; `ls /mnt/usb0` lists its files and a written file reads back byte-identical. — **`usb-mount-smoke` validates this end-to-end with a real-world 4096-byte-block ext2**: `mount("/dev/usb0","/mnt/usb0","ext2")` → `USB_MASS_STORAGE:mounted`, `getdents64` lists the seeded `hello.txt` (`USB_MOUNT:ls-ok`), read matches the seed (`USB_MOUNT:read-ok`), and an overwrite reads back byte-identical (`USB_MOUNT:rw-ok`) — through the kernel VFS secondary-mount routing (`USB_MOUNTS` table + `dev_id`-aware `Ext2Volume`, root path byte-identical). Each 4096-byte (8-sector) block I/O is split into a 7-sector + 1-sector pair of inline BOT transfers. *(Fixing this required root-causing a **bulk-OUT recv-truncation bug**: the xHCI server `recv`d requests with the 1522-byte Ethernet-MTU buffer, truncating a >1522-byte `SubmitBulkOut` and wedging the multi-sector write — fixed to `recv_with_capacity(USB_MSG_MAX)`.)* FAT-on-USB is not wired (ext2 only).
- [~] A second LUN / second stick mounts at `/mnt/usb1` independently. — the registry supports up to 4 devices and `/mnt/usb1` is pre-created + routable (`mount /dev/usb1 /mnt/usb1`), but a two-stick gate is not yet wired (single-stick validated).

### D.5 — Page-grant overflow path

**File:** `userspace/drivers/usb-storage/src/main.rs` (consumes H.4)
**Symbol:** `UsbRequest::SubmitTransfer { grant: PageGrant }` (H.4)
**Why it matters:** a multi-sector transfer larger than the 4096-byte inline budget should use the page-grant `SubmitTransfer` path (H.4) rather than many inline chunks — fewer IPC round-trips for large reads.

**Acceptance:**
- [x] Transfers ≤ `USB_MSG_MAX` use inline `SubmitBulkOut`/`SubmitBulkIn`; transfers above it use the zero-copy shm path. — the inline path works for all block I/O (the multi-sector bulk-OUT recv-truncation bug is fixed; a 4096-block ext2 mounts via 7+1-sector inline chunks), and a >`USB_MSG_MAX` transfer goes through the H.4 zero-copy `SubmitShmTransfer` path in one descriptor (`USB_STORAGE:shm-dma-ok`).
- [x] A large transfer completes and verifies byte-identical via the zero-copy path. — `usb-storage-smoke` runs an 8192-byte (16-sector) WRITE(10)+READ(10) over an IOMMU-mapped shared-memory region the device DMAs directly, verified byte-identical (`USB_STORAGE:shm-dma-ok`).

---

## Track E — Isochronous Endpoints: USB Audio (UAC) + USB Video (UVC)

### E.1 — UAC isochronous PCM-out to `audio_server`

**Files:**
- `userspace/drivers/usb-audio/src/main.rs` (new crate)
- `userspace/audio_server/src/main.rs`

**Symbol:** isochronous TRB scheduling (E.3); a PCM sink registered with `audio_server` alongside the AC'97 / HDA sinks (`driver_ipc::audio` seam)
**Why it matters:** USB speakers/headsets carry PCM over a full-speed isochronous OUT endpoint. E.1 schedules isoch TRBs and forwards `audio_server`'s mixed PCM to the device, presenting a USB sink through the same policy/mixer seam the on-board codecs use.

**Acceptance:**
- [x] `usb-audio` binds a `CLASS_AUDIO` (0x01) streaming interface, sets the active alt-setting (sample rate), and schedules isochronous OUT TRBs. — the `usb-audio` daemon walks `NextAttach` for the `CLASS_AUDIO` interface the xHCI server now surfaces (it carries an isoch OUT endpoint), `GetDescriptors` + `kernel_core::usb::uac::find_isoch_out_stream` resolves the isoch OUT DCI/MPS, issues `SET_INTERFACE(alt=1)` + a best-effort UAC `SET_CUR(SAMPLING_FREQ_CONTROL)`, then forwards PCM as `SubmitIsochOut` → `Controller::submit_isoch_out`. **Live-validated** by `usb-audio-smoke`.
- [x] `audio_server` lists the USB sink alongside AC'97/HDA; a PCM stream plays through it. — `usb-audio` registers `audio.hw` exactly as the AC'97/HDA drivers do (the same single-backend `ipc_lookup_service("audio.hw")` seam), so it is a peer PCM-sink driver; on a USB-audio machine it is the active backend. **Live-validated** by `usb-audio-smoke`: `audio-demo` mixes a tone through `audio_server` → the USB sink → the isoch OUT endpoint, and a **non-silent WAV** is captured (loudest window 99% non-silent), with `frames_consumed != 0` (the `audio.hw`-fallback guard). *(audio_server has been single-active-backend since Phase 80; simultaneous multi-sink mixing is unchanged scope.)*
- [x] Targets UAC 1.0 full-speed isochronous only (feedback endpoints / UAC 2.0 deferred — design doc). — full-speed isoch OUT split into ≤`wMaxPacketSize` (192-byte) per-frame Isoch TRBs (a full-speed isoch TD carries ≤ mps/frame); feedback endpoints + UAC 2.0 deferred.

### E.2 — UVC isochronous frame capture + `camera_server`

**Files:**
- `userspace/drivers/usb-video/src/main.rs` (new crate)
- `userspace/camera_server/src/main.rs` (new IPC surface)

**Symbol:** UVC probe/commit format negotiation (uncompressed/YUY2 only); isochronous (or bulk) IN frame transfer; a `camera_server` IPC surface delivering frames
**Why it matters:** a webcam streams frames over an isochronous IN endpoint after a probe/commit negotiation. E.2 captures uncompressed frames and exposes them to a new `camera_server` so clients can read frames. Compressed formats (MJPEG/H.264) are explicitly deferred.

**Acceptance:**
- [~] `usb-video` binds a `CLASS_VIDEO` (0x0E) streaming interface, completes probe/commit for an uncompressed format, and captures frames over the isoch IN endpoint. — **implemented + host-tested; live capture bare-metal-only.** The xHCI server surfaces a `CLASS_VIDEO`/`VideoStreaming` interface carrying a capture IN endpoint; `usb-video` walks `NextAttach`, `GetDescriptors` + `kernel_core::usb::uvc::find_video_stream` resolves the IN endpoint (bulk preferred over isoch), `SET_INTERFACE(alt)`, runs the full UVC probe/commit handshake (`GET_MAX`/`SET_CUR`(`VS_PROBE_CONTROL`) → `GET_CUR` → `SET_CUR`(`VS_COMMIT_CONTROL`) via `UvcStreamingControl` 26-byte block), and captures frames over `SubmitBulkIn` (`CAMERA:frame seq=… len=…`). QEMU ships no UVC device model, so the live capture is **bare-metal/VFIO-only** (mirroring the Track G CDC-ECM pattern); the codec (`find_video_stream`, the streaming-control + probe/commit SETUP builders) is host-tested (`usb::uvc`, 17 tests). *(isoch-IN capture + full VS Format/Frame-descriptor parsing for YUY2/MJPEG selection are deferred — the driver prefers a bulk-IN alt-setting.)*
- [~] `camera_server` delivers a captured frame to a client over IPC (frame dimensions match the negotiated format). — **implemented + host-tested; full pixel delivery deferred.** `camera_server` registers the `camera` IPC service and serves the host-tested `camera_ipc` protocol (`QueryFormat` → `Format{width,height,fmt}`, `PushFrame{seq,len}` → `Ack`); `usb-video` pushes a per-frame `PushFrame` notification. The frame *pixel-data* transfer to a viewer client (via shared memory) is deferred to a later sub-phase; the notification protocol + format query are host-tested (`usb::uvc::camera_ipc`).
- [x] Validation is bare-metal/VFIO-gated (no QEMU UVC model) with skip-with-reason in CI. — no always-on QEMU gate is added (QEMU has no UVC device model); the CI-verifiable deliverable is the host-tested `kernel_core::usb::uvc` codec + both crates compiling for `x86_64-m3os` (via `cargo xtask check`), matching the established `usb-eth-smoke`/`wifi-smoke` skip-with-reason pattern.

### E.3 — Isochronous TRB scheduling primitives in the controller

**File:** `userspace/drivers/xhci/src/controller.rs`
**Symbol:** new isochronous-endpoint support (Isoch TRB shape, frame/microframe interval, bandwidth reservation, no-retry); distinct from the shared `arm_ring_in` (interrupt + bulk)
**Why it matters:** interrupt and bulk endpoints share `arm_ring_in` today; isochronous endpoints have a different TRB type, a fixed per-(micro)frame schedule, reserved bandwidth, and no retry on error. E.3 is the controller-side primitive E.1/E.2 stand on.

**Acceptance:**
- [x] The controller programs isochronous TRBs with the correct frame ID / interval and reserves bandwidth at Configure Endpoint. — `kernel_core::usb::xhci::trb::Trb::isoch` (TRB type 5, SIA/Frame-ID), `EP_TYPE_ISOCH_OUT`/`IN` + `EP_CERR_0` in `context.rs`, and `build_configure_endpoint_ctx` now types isoch endpoints correctly (previously they fell through to the Interrupt-IN default) with the correct FS interval and CErr=0. `Controller::submit_isoch_out` programs one SIA Isoch TRB per `wMaxPacketSize`-sized frame. **Live-validated** by `usb-audio-smoke` (non-silent WAV through the isoch OUT path). *(Bandwidth reservation on FS = mps/frame; HS/SS Max-Burst/ESIT-payload scaling is host-supported (`ep_context_dword1_burst`) but bare-metal-only.)*
- [x] Isoch completions are serviced on the event ring without the bulk/interrupt re-arm assumptions (no retry on a missed frame; underrun handled gracefully). — `submit_isoch_out` enqueues frames in bounded batches, rings the doorbell per batch, and drains completions treating `Ring Underrun` (the steady-state empty-ring event between batches) and `Missed Service` as non-fatal delivered intervals (no re-arm, no retry). Host tests cover the isoch TRB encode + the isoch EP-context encode + the enumerate isoch EP-type mapping.
- [x] No regression to the interrupt (HID) or bulk (NIC/storage) paths sharing the event ring. — non-isoch endpoints are unchanged (CErr stays `EP_CERR_3`; `device_info_from_ctx` only adds an audio branch gated on an isoch OUT endpoint being present); `usb-smoke` (HID boot kbd+mouse decode + render) passes, and `cargo xtask check` (kernel-core/usb-core/xhci_driver host tests) is green.

---

## Track F — Multi-Controller Concurrency

### F.1 — Per-controller bound IRQ + event-loop thread

**Files:**
- `userspace/drivers/xhci/src/main.rs`
- `userspace/drivers/xhci/src/server.rs`

**Symbol:** `bring_up_controller` (`main.rs:170-360`); `server::run` (`server.rs:158-173` — documents that only the **primary** controller's IRQ wakes the loop; secondaries are drained opportunistically on each message wake); `controllers: Vec<ControllerCtx>`
**Why it matters:** PR 248's `handle.rs` codec already multiplexes requests to the right controller, but only the primary controller has a bound IRQ — a device on a secondary controller is serviced only on the next inbound message, not on its own interrupt. F.1 binds each controller's IRQ and runs a per-controller event loop so secondary devices wake the server on their own interrupt.

**Acceptance:**
- [x] Each brought-up controller binds its own MSI-X IRQ and its interrupt wakes the server loop (not just the primary). — each controller still subscribes its own MSI-X vector (`init_interrupter`/`init_interrupter_into`); the **secondary** controllers subscribe **into the primary's bound notification** at a distinct bit (`Controller::init_interrupter_into` → `IrqNotification::subscribe_into` → the kernel's caller-provided-notification path) so the single bound recv loop wakes on any controller's interrupt. *(Delivered as the m3OS single-event-loop pattern rather than a per-controller OS thread: the native `BrkAllocator` is single-threaded by design and the ring-drain path allocates, so a per-controller service thread would race the heap — see the 92d schedule note. Multiplexing all IRQ sources into the bound notification is the event-loop-idiomatic equivalent.)*
- [x] A device on a second `qemu-xhci` controller delivers interrupt completions without waiting for traffic on the primary. — `usb-multi-controller-smoke`: a `usb-mouse` on the second controller (`xhci1`) decodes (`USB_HID:mouse`) and `XHCI:controller-1:ready` confirms its IRQ was subscribed into the primary's bound notification (so its interrupt wakes the loop independently). The mouse being the *sole* pointer device, on `xhci1`, makes this falsifiable.
- [x] The `owner!`/`unpack_handle` request routing is unchanged (no cross-controller misroute). — the request-routing path is untouched; the only server change is the `Notification(bits)` arm draining bit-directed instead of all (`controllers` index == notification bit == `unpack_handle` controller index).

### F.2 — Concurrent MSI-X routing

**File:** `userspace/drivers/xhci/src/controller.rs`
**Symbol:** per-controller `service_interrupt_events` (`controller.rs:1383-1448`) driven from each controller's own IRQ; per-controller ring re-arm
**Why it matters:** with per-controller loops (F.1), each controller must service and re-arm its own event ring concurrently without serializing through a single shared handler.

**Acceptance:**
- [~] Both controllers' event rings are serviced promptly, each woken by its own interrupt (no controller waits on another's traffic). — reworded from the original "service their event rings concurrently": in the m3OS single-event-loop model the rings are serviced by the *one* loop, but each controller's interrupt independently wakes it (multiplexed notification) and a `Notification(bits)` wake drains exactly the controller(s) that fired (Optimization A), so neither controller's servicing waits on the other's traffic. True multi-core *simultaneous* ring draining would require a per-controller thread (→ a thread-safe native allocator first; deferred — see the 92d schedule note); it buys nothing here (ring servicing is microseconds of MMIO) and the loop is the same single loop that already serializes all USB servicing.
- [x] Simultaneous input on both controllers is observed without one controller starving the other. — both controllers' interrupts wake the single loop and a Message-wake all-drain is the safety net, so a `usb-kbd` on `xhci0` and the `usb-mouse` on `xhci1` are both serviced; the gate exercises the `xhci1` pointer path (`USB_HID:mouse`) with the `xhci0` keyboard idle. The long-blocking-control-transfer head-of-line case (the one place a single loop could delay another controller) is now mitigated by the interleaved-drain follow-up (landed in 92d — see the schedule note): a control transfer's bounded poll services the other controllers' event rings so a co-resident controller can't overflow while one waits.

---

## Track G — Host-Side USB-Ethernet Class Drivers (CDC-ECM / NCM)

### G.1 — Generic CDC-ECM `RemoteNic`

**Files:**
- `userspace/drivers/usb-net/src/main.rs` (new crate)
- `userspace/lib/usb-core/src/protocol.rs` (reused)

**Symbol:** CDC functional-descriptor parse (read via `GetDescriptors`, H.1); data-interface alt-setting select + bulk IN/OUT pair (`AttachNotice.bulk_*`); an L2 `RemoteNic` registration (Phase 79 facade, mirroring `userspace/drivers/ure/src/net.rs::run_io_loop`)
**Why it matters:** Phase 96's `ure` is a *vendor* driver (Realtek register map). CDC-ECM is the *class-compliant* generalization — standard CDC framing + Ethernet-frame-over-bulk — so the same bulk primitives + `RemoteNic` facade bring up arbitrary dongles. This is the direct "align PR 237 with Phase 92" deliverable.

**Acceptance:**
- [ ] `usb-net` binds a CDC-ECM interface (`bInterfaceClass=0x02` + `0x0a` data), parses the CDC functional descriptors, selects the data alt-setting, and reads the MAC from the ECM MAC-address string descriptor.
- [ ] Ethernet frames move over the bulk pair (`PollBulkIn`/`SubmitBulkOut`) and the kernel net stack binds the `RemoteNic` (`[remote_nic] … registered ring-3 NIC driver`).
- [x] QEMU has no CDC-ECM model ⇒ the live arm is bare-metal/VFIO-gated with skip-with-reason (mirroring `usb-eth-smoke`); host tests cover the CDC descriptor parse + frame framing. — **host-logic landed**: `kernel_core::usb::cdc::{find_ethernet_functional_desc, has_ncm_functional_desc}` + 23 host tests cover the CDC functional-descriptor parse (the live `usb-net` bind is bare-metal/VFIO-only, pending).

### G.2 — CDC-NCM (framed/aggregated NTB)

**File:** `userspace/drivers/usb-net/src/main.rs`
**Symbol:** NCM Transfer Block (NTB) framing — NDP/datagram-pointer aggregation on top of the G.1 data path
**Why it matters:** CDC-NCM aggregates multiple Ethernet frames into one bulk transfer (NTB) for higher throughput than ECM's one-frame-per-transfer. G.2 adds NTB parse/build on the same bulk path.

**Acceptance:**
- [x] NTB encode/decode (NTH16 + NDP16) is host-tested against a known NTB carrying ≥2 datagrams. — `kernel_core::usb::cdc::{build_ntb16, parse_ntb16}` round-trip (`ntb16_round_trip_two_datagrams`) + malformed-NTB rejection tests.
- [ ] A CDC-NCM dongle (bare-metal) brings up a `RemoteNic` and aggregates TX frames into NTBs; RX NTBs are split back into frames.

### G.3 — Shared USB-Ethernet device-match registry (adopt `ure`)

**Files:**
- `userspace/drivers/usb-net/src/main.rs`
- `userspace/drivers/ure/` (folded under the registry)

**Symbol:** a `VID:PID`/class device-match table routing to the vendor `ure` driver (Realtek `0x0bda:0x8156` family) or the generic CDC-ECM/NCM driver; both present the same `RemoteNic`
**Why it matters:** Phase 96's `ure` and the new class driver should not be two unrelated binaries. G.3 factors a shared device-match registry so a USB-Ethernet `AttachNotice` (`vendor_id`/`product_id` + class) selects the right driver, leaving RNDIS deferred.

**Acceptance:**
- [ ] A Realtek `0x0bda:0x8156` device routes to `ure`; a class-`0x02/0x0a` device with no vendor match routes to CDC-ECM/NCM — selection logged.
- [ ] Both paths register an identical `RemoteNic` surface (the kernel net stack binds either without special-casing).
- [ ] No regression in the Phase 96 `usb-eth-smoke` gate.

---

## Track I — Validation, Kernel Version Bump & Learning Documentation

### I.1 — Multi-tier hub + mass-storage acceptance gate

**File:** `xtask/src/main.rs`
**Symbol:** new `cmd_usb_storage_smoke` (+ `usb_storage_smoke_steps`), modeled on `cmd_usb_smoke` (`main.rs:10161-10361`); QEMU `-device usb-hub,bus=xhci0.0` + `-device usb-storage,drive=<backend>,bus=usb-hub.0`; serial sentinels `XHCI_HUB:enumerated` + `USB_MASS_STORAGE:mounted`
**Why it matters:** the headline acceptance ("a USB flash drive behind a 4-port hub enumerates and mounts") needs an always-on, CI-viable gate reusing the Phase 78 `usb-smoke` QMP/serial scaffolding and a `usb-hub` + `usb-storage` QEMU topology.

**Acceptance:**
- [x] The gate boots m3OS with a USB mass-storage device, asserts `USB_MASS_STORAGE:mounted`, and `ls /mnt/usb0` lists the backing image's files (`USB_MOUNT:ls-ok`). — **`usb-mount-smoke`**. (The `usb-hub`-*carrying*-`usb-storage` topology is **bare-metal-only**: QEMU's `usb-hub` is full-speed USB 1.1 and cannot enumerate a high-speed mass-storage device behind it. Tier-2 enumeration is instead validated with a full-speed HID device behind the hub in `usb-hub-smoke` → `XHCI_HUB:child-enumerated`; the mount path uses a direct-attach `usb-storage`.)
- [x] A write to `/mnt/usb0` reads back byte-identical (BOT WRITE(10) round-trip). — `usb-mount-smoke` overwrites `/mnt/usb0/hello.txt` and re-reads it in a fresh open (`USB_MOUNT:rw-ok`).
- [x] The gate is wired into the pre-push opt-in table with the `M3OS_USB_REGRESSION` env var. — `usb-mount-smoke` (+ `usb-hotplug-smoke`/`usb-storage-smoke`/`usb-hub-smoke`) added to the `M3OS_USB_REGRESSION` block in `.githooks/pre-push` and the AGENTS.md row (I.6).

### I.2 — Hot-plug + HID Report + multi-controller gates

**File:** `xtask/src/main.rs`
**Symbol:** new `cmd_usb_hotplug_smoke` (QMP `device_add`/`device_del` mid-run; sentinels `USB_HOTPLUG:attached`/`USB_HOTPLUG:detached`), `cmd_usb_multi_controller_smoke` (second `-device qemu-xhci,id=xhci1,addr=0x7`; `XHCI:controller-1:ready`), a Report-Protocol arm (`HID_REPORT:gaming-mouse`) in/alongside `cmd_usb_smoke`; `DeviceSet` (`main.rs:370-408`) gains `usb_hub`/`mass_storage`/`usb_audio`/`dual_xhci` flags
**Why it matters:** Tracks B/C/F each need a falsifiable QEMU gate; the QMP `device_add`/`device_del` path and a second `qemu-xhci` instance are emulator-supported, so these can be CI-viable.

**Acceptance:**
- [ ] Hot-plug gate: a mid-run `device_add`/`device_del` produces `USB_HOTPLUG:attached` then `USB_HOTPLUG:detached`, and `/mnt/usb0` mounts then cleanly unmounts.
- [x] Multi-controller gate: a device on the second controller enumerates + is serviced via its own (multiplexed) IRQ (`XHCI:controller-1:ready`) alongside the primary. — **`usb-multi-controller-smoke`** (Phase 92d): a second `qemu-xhci,id=xhci1,addr=0x7` carries the sole `usb-mouse`; the gate asserts `XHCI:controller-1:ready` (the secondary's IRQ subscribed into the primary's bound notification) then injects a QMP mouse-move and asserts `USB_HID:mouse`. Wired into `M3OS_USB_REGRESSION` + the AGENTS.md row.
- [x] Report-Protocol arm: a Report-Protocol pointer with extra axes/buttons decodes through `mouse_server`. — **`usb-report-smoke` (Phase 92b)**: a `usb-tablet` (Report-Protocol absolute pointer, no Boot interface) is decoded against the parsed `ReportField` layout and emits `HID_REPORT:pointer` (B.2); the same gate covers the B.4 `USB_HID:led` `SET_REPORT` and the H.2 no-drop assertion. Wired into `M3OS_USB_REGRESSION` + the AGENTS.md row.

### I.3 — USB audio + USB-Ethernet class gates

**File:** `xtask/src/main.rs`
**Symbol:** `cmd_usb_audio_smoke` (`-device usb-audio`, non-silent PCM assertion reusing the `hda-smoke` WAV approach); the CDC-ECM/NCM arm of an extended `usb-eth-smoke` (bare-metal/VFIO-gated, skip-with-reason in CI)
**Why it matters:** Track E.1 is QEMU-testable (`usb-audio` model); Track G's class driver is not (no CDC-ECM model), so it follows the Phase 96 opt-in pattern. Both need explicit gates so the work does not ride unverified.

**Acceptance:**
- [ ] `usb-audio` gate plays a PCM stream and asserts a non-silent capture; `audio_server` lists the USB sink (`AUDIO:usb-sink`).
- [ ] The CDC-ECM/NCM arm registers a `RemoteNic` on real hardware/VFIO and **skips with reason** under plain QEMU.
- [ ] AGENTS.md gains `M3OS_USB_AUDIO_REGRESSION` (+ any new rows) describing each gate, matching the existing table's format.

### I.4 — Kernel version bump `0.91.0` → `0.92.0`

**Files:**
- `kernel/Cargo.toml`
- `AGENTS.md`

**Symbol:** `version = "0.91.0"` (`kernel/Cargo.toml:3`) → `"0.92.0"`; the boot banner reads it via `env!("CARGO_PKG_VERSION")` (`kernel/src/lib.rs`), so `uname` / `/proc/version` update with no manual string edit; the AGENTS.md "kernel **v0.91.0**" line in the Project Overview
**Why it matters:** every phase closes by bumping the kernel minor version so the boot banner, `uname`, and `/proc/version` report the phase that landed; the AGENTS.md overview line is the canonical human-readable version.

**Acceptance:**
- [x] `kernel/Cargo.toml` version is `0.92.0`; the boot banner / `uname` / `/proc/version` report `0.92.0` (all read `env!("CARGO_PKG_VERSION")`, so the bump propagates with no other string edits).
- [x] The AGENTS.md Project Overview line reads "kernel **v0.92.0**".
- [x] The AGENTS.md USB capability bullet is rewritten to add the new device classes (live hot-plug, USB mass storage, the resident hub walker) under the existing USB bullet — per the maintenance policy. Tier-2/mount/isoch/CDC-ECM are noted as sub-phases 92a–92e.

> **Sub-phase versioning.** The `0.92.0` bump lands **with the Phase 92 core** (this PR) to mark the milestone. Sub-phases **92a–92e land as `0.92.x` patch releases** (each bumps the patch when it merges); the `0.93.0` minor is the next *distinct* phase.

### I.5 — Phase 92 learning documentation

**Files:**
- `docs/92-usb-class-expansion.md` (new)
- `docs/README.md` (Phase-Aligned Learning Docs table)
- `docs/appendix/codebase-map.md`

**Symbol:** the "aligned legacy learning doc" template from `docs/appendix/doc-templates.md` (header: Aligned Roadmap Phase / Status / Source Ref / Supersedes; seven sections: Overview, What This Doc Covers, Core Implementation, Key Files, How This Phase Differs From Later USB Work, Related Roadmap Docs, Deferred or Later-Phase Topics)
**Why it matters:** every phase ships a learner-facing doc explaining the *why* (hub topology + route strings, Report vs Boot Protocol, BOT/UAS, isochronous bandwidth, vendor→class generalization) and linking the design + task docs. It must conform exactly to the template, mirroring `docs/91-ipv6-dhcpv6.md`.

**Acceptance:**
- [ ] `docs/92-usb-class-expansion.md` exists, follows the seven-section aligned-learning-doc template, and explains the route string, Report Protocol, BOT/UAS, isochronous endpoints, and the CDC-ECM-vs-vendor-`ure` generalization.
- [ ] It is linked from the `docs/README.md` Phase-Aligned Learning Docs table and referenced in `docs/appendix/codebase-map.md`.
- [ ] The `docs/roadmap/README.md` Phase 92 row Status flips `Planned` → `Complete` (and the Tasks cell links this task doc) when the phase lands.

### I.6 — Wire the Phase 92 core USB gates into the regression flag (PR #252 readiness)

**Files:**
- `.githooks/pre-push`
- `.github/workflows/` (the USB regression job)
- `AGENTS.md` (the `M3OS_USB_REGRESSION` gate row)

**Symbol:** the `M3OS_USB_REGRESSION` opt-in block (today runs only `xhci-bringup-smoke`/`xhci-enum-smoke`/`usb-smoke`)
**Why it matters:** the three new core gates (`usb-hotplug-smoke`, `usb-storage-smoke`, `usb-hub-smoke`) PASS when invoked but are not in `M3OS_USB_REGRESSION` or any CI workflow, so a regression in the **landed** hot-plug / mass-storage / hub paths would not be caught automatically. (The host-side `usb-core` codec tests were wired into `cargo xtask check` in PR #252; this is the QEMU-gate half. Near-term — schedule with 92a.)

**Acceptance:**
- [ ] `usb-hotplug-smoke`, `usb-storage-smoke`, and `usb-hub-smoke` run under `M3OS_USB_REGRESSION` (and/or the CI USB job) alongside `usb-smoke`.
- [ ] The AGENTS.md `M3OS_USB_REGRESSION` row lists the three added gates.

---

## Documentation Notes

- **Substrate reuse, not re-implementation.** The Phase 96 bulk transport (`PollBulkIn`/`SubmitBulkOut`/`BulkData`/`ControlWrite`, `USB_MSG_MAX`=4096, the `handle.rs` multi-controller codec) is the foundation; Tracks D/E/G are bulk/isoch **class drivers** on it. Do not duplicate the controller's bulk machinery — call `arm_bulk_in`/`take_bulk_report`/`submit_bulk_out` directly.
- **`ENOSYS` is by design, not a gap.** `GetDescriptors`/`ConfigureEndpoints`/`SubmitTransfer` return `ENOSYS` because descriptors are pre-resolved into `AttachNotice` and endpoints are configured during enumeration. Track H lights up only the residual paths a class driver genuinely needs (large-descriptor `GetDescriptors`, page-grant `SubmitTransfer`); leave the rest stubbed.
- **Carry-over hardening status.** PR 248 (Phase 96) already resolved the DMA-buffer-lifetime leak (persistent per-slot `ep0_data_buf`), captured concurrent IN completions during bulk-OUT, and raised `USB_MSG_MAX` to 4096. Track H finishes only the control-transfer event capture (H.2) and the large-descriptor / page-grant cases (H.1/H.4) — it does **not** re-do the fixed items.
- **Phase numbering.** Phase 96 (USB-Ethernet) landed before Phase 92; the `ure` driver lives in `userspace/drivers/ure/` and is adopted into the Track G registry rather than rewritten. Where the xHCI server comments say "Phase 90 (mass storage)" they mean this phase — fix the comment to "Phase 92" while implementing D/H.
- **New userspace crates** (`usb-storage`, `usb-net`, `usb-audio`, `usb-video`, `camera_server`) each require the four-place wiring from AGENTS.md (workspace member, `xtask` `bins`, ramdisk `BIN_ENTRIES`, and a service config for the daemons) — missing any one yields a silent build-omit or `ENOENT` at `execve`.
- **CI-viable vs hardware-only.** A/C/D/E.1/F have always-on QEMU gates (Track I). E.2 (UVC), G (CDC-ECM/NCM dongles), UAC feedback endpoints, and TT accounting are bare-metal/VFIO-only — gate them opt-in with skip-with-reason, exactly as `usb-eth-smoke` and `wifi-smoke` do.
- **Acceptance checkboxes** are intentionally `[ ]` (Planned); they flip to `[x]` only when the named gate or host test verifies the behavior on `main`.
