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
| B | HID Report Protocol — wire `parse_report_descriptor` live, multi-axis/buttons/scroll, consumer keys, LED `SET_REPORT` | H | **B.1 landed** — `usb-hid` reads + parses the HID Report descriptor over EP0 at bind and stores the `ReportField` layout per device (`USB_HID:report-parsed` in `usb-smoke`); B.2/B.3 host-logic landed (47 hid + 38 keymap tests). **B.2-live decode / B.3-live consumer routing / B.4 LED `SET_REPORT` → Phase 92b** |
| C | Live hot-plug event surface — Port Status Change → `AttachNotice` push, detach (`attached:false`), dynamic re-enumeration, Disable Slot reclamation | — | C.1–C.3 + server-side C.4 landed (`usb-hotplug-smoke` 3-cycle PASS); **class-driver-side C.4 release scheduled per driver — usb-storage→92a, usb-hid→92b, usb-net→92e** |
| D | USB Mass Storage — BOT CBW/CSW on the Phase 96 inline bulk path, SCSI subset, UAS, `RemoteBlockDevice` facade + `/mnt/usb<n>`, page-grant overflow | C, H | D.1/D.2 transport + **D.3 UAS codec** + **D.4 mount landed (Phase 92a)**: the resident `usb-storage` daemon registers `usb0.block` and serves the block protocol; the kernel multi-device registry + VFS secondary-mount table mount it at `/mnt/usb0`. Live-validated by `usb-mount-smoke` (mount + ls + read + overwrite-readback). **D.5 page-grant overflow + live UAS → deferred follow-up.** |
| E | Isochronous endpoints — UAC PCM-out to `audio_server`, UVC frame capture + `camera_server`, controller isoch TRB scheduling | F | **→ Phase 92c** (deep isoch TRB scheduling; UVC bare-metal-only) |
| F | Multi-controller concurrency — per-controller bound IRQ + event-loop thread, concurrent MSI-X routing | — | **→ Phase 92d** (ring-3 driver threading; risk-isolated from the working single-loop server) |
| G | Host-side USB-Ethernet class drivers — generic CDC-ECM/NCM `RemoteNic`, fold the Phase 96 vendor `ure` into a shared device-match registry | C | G.1/G.2 host-logic landed (`cdc.rs`: CDC functional-descriptor parse + NTB-16 framing round-trip, 23 host tests); **live `usb-net` daemon → Phase 92e** (bare-metal/VFIO-gated — no QEMU CDC-ECM model) |
| H | Foundation & carry-over hardening — live `GetDescriptors` (large reads), control-transfer event capture, Disable Slot, page-grant `SubmitTransfer` | — | H.1–H.3 landed; **H.6 length bounds landed (Phase 92a)** (host-tested; caught a real chunk-overflow integration bug). **H.4 page-grant → deferred with D.5.** |
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
- ⏳ **D.5 + H.4** (page-grant `SubmitTransfer` overflow) — **deferred** to a focused follow-up: a throughput optimization for >7-sector transfers (and the enabler for 4096-block filesystems). The validated mount uses a 1024-block ext2 over the inline transport, so the functional requirement is met without it.
- *The hard USB data path was already done (D.1/D.2); this delivered the tier-2 enumeration + the kernel multi-mount integration.*

**Phase 92b — HID Report Protocol live decode.**
- **B.2-live** (data-driven multi-axis/scroll/button decode + usage→event mapping), **B.3-live** (consumer-key routing to `audio_server`; the consumer-keycode host-logic is already landed), **B.4** (keyboard LED `SET_REPORT`).
- **C.4 (usb-hid arm)** — `usb-hid` releases its per-device state on an `attached:false` notice.
- **H.2 remaining item** — the live "control transfer interleaved with an armed interrupt endpoint drops no report" assertion rides B.4's `SET_REPORT`-during-HID-polling path (line 87).
- **Gate (I.2 Report-Protocol arm):** a `usb-tablet` QMP-abs-input arm → `USB_HID:mouse`/`HID_REPORT:*` (extra axes/buttons). *Host-logic + the stored `ReportField` layout (B.1) are ready.*

**Phase 92c — USB isochronous (UAC / UVC).**
- **E.3** (controller isochronous-TRB scheduling — frame interval, bandwidth reservation, no-retry), **E.1** (UAC PCM-out to `audio_server`, CI-viable via `-device usb-audio`), **E.2** (UVC frame capture + `camera_server`, bare-metal/VFIO-only).
- **Gate (I.3 audio half):** `usb-audio-smoke` (non-silent PCM, `AUDIO:usb-sink`). AGENTS.md: `M3OS_USB_AUDIO_REGRESSION`. *Deepest controller work in the phase.*

**Phase 92d — Multi-controller concurrency.**
- **F.1** (per-controller bound IRQ + event-loop thread), **F.2** (concurrent MSI-X routing).
- **Gate (I.2 multi-controller arm):** `usb-multi-controller-smoke` (second `qemu-xhci`, `XHCI:controller-1:ready`). *Ring-3 driver threading, deliberately risk-isolated from the validated single-loop server.*

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
| B.1-decode (197), B.2-live (212–213), B.3-live (225–226), B.4 (238–240), H.2-test (87) | 92b |
| C.4 usb-hid detach (293) | 92b |
| I.2 Report-Protocol arm (501) | 92b |
| E.1 (378–380), E.2 (392–394), E.3 (403–405) | 92c |
| I.3 audio gate (510, 512) | 92c |
| F.1 (421–423), F.2 (432–433) | 92d |
| I.2 multi-controller arm (500) | 92d |
| G.1 (449–450), G.2 (461), G.3 (473–475) | 92e |
| C.4 usb-net detach (293) | 92e |
| I.3 CDC-ECM arm (511) | 92e |
| I.4 version bump (524–526) | **done — `0.92.0` with the core** |
| I.5 learning doc (539–541) | Track I close-out (lands with the last sub-phase) |
| *PR #252 readiness follow-ups (task IDs below):* | |
| H.6 length bounds, D.2 rw-ok comment, I.6 gate regression wiring | 92a |
| B.1 readiness items (wDescriptorLength read, doc header, hostile-count test) | 92b |
| H.5 slot-reclaim on unpackable handle | 92d |

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
- [ ] A test (or instrumented run) issuing a control transfer while an interrupt endpoint is armed shows no dropped report and no un-rearmed endpoint. — structurally mirrors the proven bulk-OUT path; a dedicated live no-drop assertion is exercised by B.4 (`SET_REPORT` interleaved with HID polling).
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

> **Sequencing:** deferred to land with **Track D.5** (its only consumer). Inline `SubmitBulkOut`/`PollBulkIn` cover ≤ 4096-byte data phases, so D.1–D.4 do not need it; it is implemented when the multi-sector overflow path is built. **Status: deferred past the Phase 92a core PR.** The validated D.4 mount path uses the inline transport with a 1024-byte-block ext2 (every block I/O is a single ≤2-sector transfer, well within `USB_MSG_MAX`). H.4/D.5 (the page-grant path for >4092-byte / multi-sector transfers) is a throughput optimization — and the enabler for 4096-block filesystems — scheduled as a focused follow-up; the functional read/write/mount requirement is met without it. **Note:** H.6 revealed the precise inline budget is 7 sectors (3584 B), not 8 (4096 B), since the reply carries 3 bytes of wire overhead.

**Files:**
- `userspace/drivers/xhci/src/server.rs`
- `userspace/drivers/xhci/src/controller.rs`

**Symbol:** `UsbRequest::SubmitTransfer { slot_id, dci, grant: PageGrant }` (today the `ENOSYS` default), `PageGrant` (`protocol.rs`)
**Why it matters:** inline `SubmitBulkOut`/`PollBulkIn` cap a data phase at `USB_MSG_MAX`=4096. A multi-sector mass-storage READ(10)/WRITE(10) (D.5) can exceed that; the latent page-grant transport maps a shared buffer and programs bulk TRBs directly against it, avoiding per-chunk IPC.

**Acceptance:**
- [ ] `SubmitTransfer` maps the `PageGrant`, programs Normal TRBs (IOC on the last), rings the doorbell, and completes off the Transfer Event — returning `UsbReply::TransferComplete { transferred, completion_code }`.
- [ ] A > 4096-byte transfer (e.g. an 8-sector READ(10)) completes via the page-grant path; a ≤ 4096-byte transfer still uses the inline path.
- [ ] The grant is unmapped on completion (no IOVA leak across repeated transfers), reusing the H.1/PR-248 buffer-lifetime discipline.

### H.5 — Reclaim the slot when a hot-plug handle can't be packed (PR #252 readiness)

**File:** `userspace/drivers/xhci/src/server.rs`
**Symbol:** `process_port_events` (the attach arm, ~`server.rs:233`, where `pack_handle` returns `None`)
**Why it matters:** on hot-plug attach the server runs Enable Slot (allocating a hardware slot + `SlotContext`) **before** `pack_handle(ctrl_idx, slot_id)`. If `pack_handle` returns `None` (`ctrl_idx > 3`, i.e. ≥5 controllers, or hw slot > 63) the code logs "unpackable handle" and continues without `disable_slot` — leaking the very slot H.3 set out to reclaim. Reachable only in the multi-controller regime, so it pairs with Track F / Phase 92d, but it is live code today. (The identical bring-up-path case at ~`server.rs:300` predates this phase.)

**Acceptance:**
- [ ] When `pack_handle` returns `None` on the attach path, the server issues `disable_slot` for the just-enabled slot before dropping the device (no slot leak) and logs the drop.
- [ ] An instrumented run in the ≥5-controller / slot>63 regime shows no slot-pool leak across repeated unpackable attaches.

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
- [~] A Report-Protocol device's reports decode by the parsed field layout (not the boot 8-byte/3-byte assumption). — **Phase 92b** (B.2-live): the layout is stored; the data-driven decode + usage→event mapping + a `usb-tablet` QMP-abs-input gate arm are scheduled there.
- [x] The existing Boot-Protocol keyboard/mouse path is unchanged (`usb-smoke` still PASSES). — `usb-smoke` PASSES (kbd+mouse decode live + render); the boot decode path is untouched.
- [ ] **(PR #252 readiness, 92b)** `usb-hid` reads the HID descriptor's `wDescriptorLength` instead of the hard-coded `REQ_LEN = 256` (`fetch_report_fields`), so the Report descriptor is not over-read into zero padding that parses as spurious trailing zero-width fields.
- [ ] **(PR #252 readiness, 92b)** The stale `hid_report.rs` module doc header ("not wired to any live device") is corrected — the parser is now called live at bind (B.1).
- [ ] **(PR #252 readiness, 92b)** A host test feeds `parse_report_descriptor` a hostile Report Count/Size (e.g. a 4-byte `0xFFFFFFFF` count) to lock in the saturating/clamped (≤65536 fields) behavior now that the parser sees live device input.

### B.2 — Multi-axis / extra-button / scroll decode (touchpad + gaming mouse)

**Files:**
- `kernel-core/src/usb/hid_report.rs`
- `kernel-core/src/usb/hid.rs`
- `userspace/drivers/usb-hid/src/main.rs`

**Symbol:** `parse_report_descriptor` (enhance: Usage Min/Max ranges + Report IDs — skeleton-limited to one Usage and no Report ID today); a data-driven report decoder mirroring `BootKeyboardDecoder`; `PointerEvent` (extend axes)
**Why it matters:** a gaming mouse reports a scroll wheel + extra buttons; a touchpad reports X/Y/pressure + contact IDs. The parser must emit multiple `ReportField`s for Usage ranges and respect Report IDs, and `usb-hid` must unpack arbitrary bit fields and map usages (X=0x01:0x30, Y=0x01:0x31, buttons=0x09:0x01..) to `mouse_server` events.

**Acceptance:**
- [x] `parse_report_descriptor` emits a `ReportField` per usage for a Usage Min/Max range and tags fields with their Report ID; host tests cover both. — `kernel_core::usb::hid_report` Usage-Min/Max range expansion + per-Report-ID `report_id` tagging + offset reset; host tests `usage_min_max_range_expands_to_one_field_per_usage` + `two_report_ids_tag_fields_and_reset_offset`. (The live `usb-hid` decode using this — B.2 remainder — is pending.)
- [ ] A Report-Protocol gaming mouse delivers correct X/Y + a scroll axis + ≥4 buttons through `mouse_server` (`USB_HID:mouse` sentinels reflect the extra axes/buttons).
- [ ] A single-pointer touchpad maps to pointer motion (multi-touch contact tracking is explicitly deferred — see design doc).

### B.3 — Consumer-control keys (media / brightness)

**Files:**
- `kernel-core/src/usb/hid.rs`
- `kernel-core/src/input/keymap.rs`

**Symbol:** `hid_usage_to_keycode` (extend to Usage Page 0x0C, Consumer); `Keycode` enum (add consumer slots)
**Why it matters:** media keys (volume up/down/mute, play/pause) and brightness live on HID Usage Page 0x0C, which Boot Protocol cannot express. B.3 maps Consumer usages to keycodes so `display_server` can route them to `audio_server` / brightness control.

**Acceptance:**
- [ ] `hid_usage_to_keycode` maps the Consumer page (volume up/down/mute at minimum) to distinct keycodes; host-tested.
- [ ] A Report-Protocol keyboard's volume keys are decoded and routed (volume keys reach `audio_server`).

### B.4 — Keyboard LED output via `SET_REPORT`

**Files:**
- `userspace/drivers/usb-hid/src/main.rs`
- `userspace/kbd_server/src/main.rs`

**Symbol:** new `SET_REPORT` issuance over `UsbRequest::ControlWrite` (bmRequestType `0x21`, bRequest `0x09`, wValue = report-type/ID); `kbd_server` LED-state tracking
**Why it matters:** Boot keyboards are input-only; Report-Protocol keyboards expose OUTPUT items for Caps/Num/Scroll Lock LEDs. B.4 tracks lock state in `kbd_server` and issues `SET_REPORT` (over the live `ControlWrite` path) with the LED bitfield — the one Track-B path that writes back to the device.

**Acceptance:**
- [ ] Toggling Caps Lock updates `kbd_server` LED state and issues a `SET_REPORT` `ControlWrite` carrying the LED bitfield.
- [ ] The transfer uses the H.2-hardened control path (a concurrent interrupt report is not dropped during the `SET_REPORT`).
- [ ] Boot keyboards (no OUTPUT items) are unaffected — no `SET_REPORT` issued, no error.

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
- [ ] `usb-hid`/`usb-storage`/`usb-net` each release their per-device state on an `attached: false` notice for a slot they own. — **server-side teardown done**; the class-driver-side release is scheduled per driver alongside that driver's resident-state work: **usb-storage → Phase 92a**, **usb-hid → Phase 92b**, **usb-net → Phase 92e** (see the Sub-Phase Schedule coverage map).
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
- [x] An ext2 USB stick mounts under `/mnt/usb0`; `ls /mnt/usb0` lists its files and a written file reads back byte-identical. — **`usb-mount-smoke` validates this end-to-end**: `mount("/dev/usb0","/mnt/usb0","ext2")` → `USB_MASS_STORAGE:mounted`, `getdents64` lists the seeded `hello.txt` (`USB_MOUNT:ls-ok`), read matches the seed (`USB_MOUNT:read-ok`), and an overwrite reads back byte-identical (`USB_MOUNT:rw-ok`) — through the kernel VFS secondary-mount routing (`USB_MOUNTS` table + `dev_id`-aware `Ext2Volume`, root path byte-identical). The gate uses a **1024-byte-block ext2** image so every block I/O is a single ≤2-sector inline BOT transfer; a 4096-block fs's 8-sector I/O needs the page-grant overflow path (D.5, deferred). FAT-on-USB is not wired (ext2 only).
- [~] A second LUN / second stick mounts at `/mnt/usb1` independently. — the registry supports up to 4 devices and `/mnt/usb1` is pre-created + routable (`mount /dev/usb1 /mnt/usb1`), but a two-stick gate is not yet wired (single-stick validated).

### D.5 — Page-grant overflow path

**File:** `userspace/drivers/usb-storage/src/main.rs` (consumes H.4)
**Symbol:** `UsbRequest::SubmitTransfer { grant: PageGrant }` (H.4)
**Why it matters:** a multi-sector transfer larger than the 4096-byte inline budget should use the page-grant `SubmitTransfer` path (H.4) rather than many inline chunks — fewer IPC round-trips for large reads.

**Acceptance:**
- [ ] Transfers ≤ 4096 bytes use inline `SubmitBulkOut`/`PollBulkIn`; transfers above it use the H.4 page-grant path.
- [ ] A large sequential read (e.g. a multi-MiB file copy off `/mnt/usb0`) completes and verifies byte-identical.

---

## Track E — Isochronous Endpoints: USB Audio (UAC) + USB Video (UVC)

### E.1 — UAC isochronous PCM-out to `audio_server`

**Files:**
- `userspace/drivers/usb-audio/src/main.rs` (new crate)
- `userspace/audio_server/src/main.rs`

**Symbol:** isochronous TRB scheduling (E.3); a PCM sink registered with `audio_server` alongside the AC'97 / HDA sinks (`driver_ipc::audio` seam)
**Why it matters:** USB speakers/headsets carry PCM over a full-speed isochronous OUT endpoint. E.1 schedules isoch TRBs and forwards `audio_server`'s mixed PCM to the device, presenting a USB sink through the same policy/mixer seam the on-board codecs use.

**Acceptance:**
- [ ] `usb-audio` binds a `CLASS_AUDIO` (0x01) streaming interface, sets the active alt-setting (sample rate), and schedules isochronous OUT TRBs.
- [ ] `audio_server` lists the USB sink alongside AC'97/HDA; a PCM stream plays through it.
- [ ] Targets UAC 1.0 full-speed isochronous only (feedback endpoints / UAC 2.0 deferred — design doc).

### E.2 — UVC isochronous frame capture + `camera_server`

**Files:**
- `userspace/drivers/usb-video/src/main.rs` (new crate)
- `userspace/camera_server/src/main.rs` (new IPC surface)

**Symbol:** UVC probe/commit format negotiation (uncompressed/YUY2 only); isochronous (or bulk) IN frame transfer; a `camera_server` IPC surface delivering frames
**Why it matters:** a webcam streams frames over an isochronous IN endpoint after a probe/commit negotiation. E.2 captures uncompressed frames and exposes them to a new `camera_server` so clients can read frames. Compressed formats (MJPEG/H.264) are explicitly deferred.

**Acceptance:**
- [ ] `usb-video` binds a `CLASS_VIDEO` (0x0E) streaming interface, completes probe/commit for an uncompressed format, and captures frames over the isoch IN endpoint.
- [ ] `camera_server` delivers a captured frame to a client over IPC (frame dimensions match the negotiated format).
- [ ] Validation is bare-metal/VFIO-gated (no QEMU UVC model) with skip-with-reason in CI.

### E.3 — Isochronous TRB scheduling primitives in the controller

**File:** `userspace/drivers/xhci/src/controller.rs`
**Symbol:** new isochronous-endpoint support (Isoch TRB shape, frame/microframe interval, bandwidth reservation, no-retry); distinct from the shared `arm_ring_in` (interrupt + bulk)
**Why it matters:** interrupt and bulk endpoints share `arm_ring_in` today; isochronous endpoints have a different TRB type, a fixed per-(micro)frame schedule, reserved bandwidth, and no retry on error. E.3 is the controller-side primitive E.1/E.2 stand on.

**Acceptance:**
- [ ] The controller programs isochronous TRBs with the correct frame ID / interval and reserves bandwidth at Configure Endpoint.
- [ ] Isoch completions are serviced on the event ring without the bulk/interrupt re-arm assumptions (no retry on a missed frame; underrun handled gracefully).
- [ ] No regression to the interrupt (HID) or bulk (NIC/storage) paths sharing the event ring.

---

## Track F — Multi-Controller Concurrency

### F.1 — Per-controller bound IRQ + event-loop thread

**Files:**
- `userspace/drivers/xhci/src/main.rs`
- `userspace/drivers/xhci/src/server.rs`

**Symbol:** `bring_up_controller` (`main.rs:170-360`); `server::run` (`server.rs:158-173` — documents that only the **primary** controller's IRQ wakes the loop; secondaries are drained opportunistically on each message wake); `controllers: Vec<ControllerCtx>`
**Why it matters:** PR 248's `handle.rs` codec already multiplexes requests to the right controller, but only the primary controller has a bound IRQ — a device on a secondary controller is serviced only on the next inbound message, not on its own interrupt. F.1 binds each controller's IRQ and runs a per-controller event loop so secondary devices wake the server on their own interrupt.

**Acceptance:**
- [ ] Each brought-up controller binds its own MSI-X IRQ and runs an event-loop thread (not just the primary).
- [ ] A device on a second `qemu-xhci` controller delivers interrupt/bulk completions without waiting for traffic on the primary — proven by a second-controller HID/NIC event arriving while the primary is idle.
- [ ] The `owner!`/`unpack_handle` request routing is unchanged (no cross-controller misroute).

### F.2 — Concurrent MSI-X routing

**File:** `userspace/drivers/xhci/src/controller.rs`
**Symbol:** per-controller `service_interrupt_events` (`controller.rs:1383-1448`) driven from each controller's own IRQ; per-controller ring re-arm
**Why it matters:** with per-controller loops (F.1), each controller must service and re-arm its own event ring concurrently without serializing through a single shared handler.

**Acceptance:**
- [ ] Two controllers service their event rings concurrently (a device on each enumerates and polls in parallel).
- [ ] Simultaneous input on both controllers (QMP keyboard on one, mouse on the other) is observed without one controller starving the other.

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
- [ ] Multi-controller gate: a device on the second controller enumerates on its own IRQ (`XHCI:controller-1:ready`) concurrently with the primary.
- [ ] Report-Protocol arm: a gaming-mouse report with extra axes/buttons renders through `mouse_server` (PPM `changed_rows_in_band` or `USB_HID:mouse` sentinel).

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
