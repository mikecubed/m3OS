# Phase 78c — USB Host Foundation: HID + Integration + Release: Task List

**Status:** In Progress
**Source Ref:** phase-78c
**Depends on:** Phase 78b (USB Enumeration + Hub) ✅ merged, Phase 78a ✅ merged, Phase 56 (Display and Input Architecture) ✅, Phase 74 (IPC Capability Grants) ✅
**Goal:** Complete the USB milestone — a USB keyboard and mouse drive m3OS. Add the `usb-hid` Boot-Protocol class driver, inject its events into the Phase 56 `kbd_server`/`mouse_server` input path (leaving the dispatcher unchanged), land the full `usb-smoke` QMP gate (keystroke → prompt), write the Phase 78 learning doc, and cut `0.78.2` with the new USB capability inventory entry. Final of three Phase 78 sub-phases ([78a](../78a-xhci-host-bringup.md) → [78b](../78b-usb-enumeration-hub.md) → [78c](../78c-usb-hid-and-release.md)).

> **Source-verified (2026-05-30):** `KeyEvent`/`PointerEvent` already exist with stable 20-/37-byte codecs (`kernel-core/src/input/events.rs:146`/`:199`); the input syscalls are `SYS_READ_SCANCODE` (`0x1007`) / `SYS_READ_MOUSE_PACKET` (`0x1015`); the dispatcher is `InputDispatcher::route_key_event`/`route_pointer_event` (`kernel-core/src/input/dispatch.rs:304`/`:379`); `kbd_server`/`mouse_server` are synchronous single-endpoint pull loops with **no** pending-event buffer (so the inject is a real change, not just a label); there is **no** `qemu-xhci`/`usb-kbd` in xtask today.

## Post-Merge Validation & Architecture Decision (2026-05-30, after 78a + 78b merged)

A full source audit of the merged 78a/78b tree changed the scope of this phase. **Both 78a and 78b are merged.** Key findings:

1. **The live xHCI IPC server does NOT exist yet — it was deferred from 78b to here.** `userspace/drivers/xhci/src/main.rs` enumerates the device once at bring-up, prints the descriptor tree, then enters `controller.event_loop()` (`controller.rs:546`) which **discards every interrupt-IN transfer event silently** (`controller.rs:583`). It never registers a service, never publishes `AttachNotice`, never serves `UsbRequest`. `usb-core/src/protocol.rs:196` says verbatim *"the `sys_ipc_call` plumbing is **not** implemented here"* — `UsbClient` is a request-*builder* only, with no wire codec and nothing serving it. `usbhub/src/main.rs:14` confirms: *"the live `AttachNotice` IPC path … is deferred to **Phase 78c**."* **This server + transport work is new and was unbudgeted by the original task list.**
2. **Architecture chosen (developer decision): Full IPC.** Build the xHCI IPC server + a separate `/drivers/usb-hid` daemon (rejected alternative: in-process HID inside the xhci driver). This is captured as a **new Track A0** below.
3. **The IRQ↔IPC multiplex is a solved problem in m3OS.** `sys_notif_bind` (`0x1111`) binds an IRQ notification into an IPC endpoint; `ipc_recv_msg` then wakes on *either* a message *or* the IRQ (`RECV_KIND_NOTIFICATION = 1`). The e1000 driver already uses this exact pattern — the xHCI server reuses it (single-threaded; no `sys_clone` needed).
4. **HID reports are tiny** (8-byte kbd / ≤4-byte mouse, ≤64-byte descriptors), so control/interrupt-IN data returns **inline via `ipc_store_reply_bulk`** (the `UsbReply::ControlData` pattern). Cross-process `PageGrant` transfer (Phase 74) stays for future bulk endpoints — **deferred**, not used by the 1.0 HID path.
5. **B.2 drift corrected:** `DECLARED_SESSION_STEP_NAMES` (`session_supervisor.rs:89`) does **not** contain `xhci_driver`/`usbhub` — they are plain service-config daemons (`type=daemon`, `restart=on-failure`, `depends=`). `usb-hid` follows the **same daemon model** (config in `xtask::populate_ext2_files` + `init` `KNOWN_CONFIGS`, `depends=xhci_driver`), **not** an addition to `DECLARED_SESSION_STEP_NAMES`. The original B.2 acceptance referencing the session step list and `kernel/initrd/etc/services.d/usb-hid.conf` is superseded accordingly.
6. The xhci driver **already has every in-process primitive** the server needs: `control_transfer` (SETUP/DATA/STATUS, `controller.rs:723`), `alloc_interrupt_ep_ring` (`:1061`), `wait_for_transfer_event`. New controller code needed: enqueue a **Normal TRB** on an interrupt-IN ring + ring its doorbell + decode the resulting Transfer Event (slot/dci/residual).

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A0 | **(new)** USB IPC transport: `usb-core` wire codec for `AttachNotice`/`UsbRequest`/`UsbReply`; xHCI IPC server (register `usb`, IRQ-bound `ipc_recv_msg` multiplex, device table, control + interrupt-IN serving, deferred reply); controller interrupt-IN Normal-TRB enqueue/decode | Phase 78b ✅ | In Progress |
| A | HID decode core (host-tested): `kernel-core/src/usb/hid.rs` usage→keycode + boot report decode, `hid_report.rs` skeleton | Phase 78b ✅ | In Progress |
| B1 | Input integration: bounded inject queue + `KBD_EVENT_INJECT`/`MOUSE_EVENT_INJECT` in `kbd_server`/`mouse_server` (dispatcher unchanged) | Phase 56 ✅ | In Progress |
| A-hid | `usb-hid` daemon (ring 3): lookup `usb` service, `SET_PROTOCOL(0)`/`SET_IDLE(0)`, poll interrupt-IN, decode via Track A, inject via Track B1 | A0, A, B1 | Planned |
| B2 | Build + ramdisk + service wiring for `usb-hid` (`/drivers/usb-hid`, `depends=xhci_driver`) | A-hid | Planned |
| B3 | `usb-smoke` QMP gate (`qemu-xhci`+`usb-kbd`+`usb-mouse`, keystroke→prompt), opt-in `M3OS_USB_REGRESSION=1` | all above | Planned |
| C | Documentation + release: learning doc, `0.78.2` bump + capability entry | all above | Planned |

> **Inject-label contract (pinned for parallel tracks):** `KBD_EVENT_INJECT = 5` on the `kbd` endpoint, `MOUSE_EVENT_INJECT = 3` on the `mouse` endpoint. Payload = the existing 20-byte `KeyEvent` / 37-byte `PointerEvent` wire form as IPC bulk; reply label `0` = enqueued OK, `u64::MAX` = queue full/error. Drain priority on `*_EVENT_PULL`: injected (USB) events drain **before** the PS/2 stream.

---

## Track A0 — USB IPC Transport (new; unblocks the separate `usb-hid` daemon)

### A0.1 — `usb-core` wire codec

**Files:** `userspace/lib/usb-core/src/protocol.rs` (+ host tests)
**Symbol:** `AttachNotice::{encode,decode}`, `UsbRequest::{encode,decode}`, `UsbReply::{encode,decode}`, IPC label constants, `USB_SERVICE_NAME = "usb"`
**Why it matters:** The types exist but have no byte transport (`protocol.rs:196`). Without a host-tested codec there is nothing to send over `ipc_call_buf`/`ipc_store_reply_bulk`.

**Acceptance:**
- [ ] `encode`/`decode` for `AttachNotice`, `UsbRequest`, `UsbReply` round-trip in host tests (incl. the `ControlData`/inline-report variants and an `Error` variant)
- [ ] IPC label constants + `USB_SERVICE_NAME` defined and host-asserted
- [ ] No `PageGrant` required on the live HID path (inline-bulk return documented; grant variant retained but marked deferred)

### A0.2 — xHCI IPC server loop

**Files:** `userspace/drivers/xhci/src/main.rs`, new `userspace/drivers/xhci/src/server.rs`, `userspace/drivers/xhci/src/controller.rs`
**Symbol:** `run_server`, `DeviceTable`, interrupt-IN `enqueue_normal_trb`/`ring_ep_doorbell`/transfer-event decode
**Why it matters:** Turns the driver from "enumerate once and discard events" into a live request/reply server that `usb-hid` (and later `usbhub`) drive.

**Acceptance:**
- [ ] After bring-up + enumeration, registers service `usb`, binds the controller IRQ into the endpoint (`sys_notif_bind`), and runs an `ipc_recv_msg` loop multiplexing IRQ + IPC (e1000 pattern), replacing the discard-only `event_loop`
- [ ] Holds a device table of enumerated devices (slot_id, interface class/sub/proto, interrupt-IN dci + mps + interval); serves an attach-pull request returning `AttachNotice` for already-present HID devices
- [ ] Serves `ControlRequest` (via `control_transfer`, inline data reply) and an interrupt-IN read; the interrupt-IN read **defers** its reply — the reply cap is stashed and answered when the matching Transfer Event arrives off the IRQ-drained event ring; endpoint is re-armed after each report
- [ ] New `Controller` methods enqueue a Normal TRB on an interrupt-IN endpoint ring, ring the endpoint doorbell, and decode the Transfer Event (slot/dci/residual) — host-tested where the logic is pure (`kernel-core/src/usb/xhci`)
- [ ] `xhci-bringup-smoke` and `xhci-enum-smoke` still PASS (no regression to the 78a/78b sentinels)

---

## Track A — HID Class Driver (ring 3)

### A.1 — Boot-Protocol keyboard → `KeyEvent`

**Files:**
- `userspace/drivers/usb-hid/` (new crate)
- `kernel-core/src/usb/hid.rs` (new — `hid_usage_to_keycode` table + report decode, host-tested)

**Symbol:** `set_protocol`, `set_idle`, `parse_boot_keyboard_report`, `hid_usage_to_keycode`
**Why it matters:** This is the actual input path. `SET_PROTOCOL(0)` puts the device in Boot Protocol (no report-descriptor parsing needed); `SET_IDLE(0)` suppresses duplicate/streamed reports; the interrupt-IN endpoint must be brought into the controller via `Configure Endpoint` (78b) and polled with Normal TRBs at `bInterval`.

**Acceptance:**
- [ ] Registers for `bInterfaceClass 0x03` / `SubClass 0x01` / `Protocol 0x01`; issues `SET_PROTOCOL(0)` (`bmRequestType 0x21`, `bRequest 0x0B`, `wValue 0`) and `SET_IDLE` with `wValue = (0 << 8) | 0` (duration 0 = report only on change, report ID 0 = all reports) so the keyboard does not stream duplicate reports
- [ ] Polls the interrupt-IN endpoint with Normal TRBs at `bInterval`; decodes the **first 8 bytes** of the boot report `[modifier][reserved][keycode0..keycode5]` (HID Usage IDs), handling the rollover/`0x01` error code
- [ ] HID Usage ID → `KeyEvent` (keycode/symbol/modifiers/`kind`) via the host-tested `hid_usage_to_keycode` table
- [ ] The `KeyEvent` is encoded with the existing `kernel-core` codec (`KEY_EVENT_WIRE_SIZE` = 20 bytes) — no new wire format introduced

### A.2 — Boot-Protocol mouse → `PointerEvent`

**File:** `userspace/drivers/usb-hid/src/mouse.rs`; `kernel-core/src/usb/hid.rs`
**Symbol:** `parse_boot_mouse_report`
**Why it matters:** The 3-byte boot mouse report maps directly to the Phase 56 relative-pointer model.

**Acceptance:**
- [ ] Registers for `bInterfaceClass 0x03` / `Protocol 0x02`; reads the endpoint's `wMaxPacketSize` bytes and decodes the **first 3 bytes** `[button bitfield][signed dx][signed dy]`, **ignoring any trailing bytes** (real boot mice often send 4+ bytes with a wheel in byte 4; the Boot Protocol only guarantees the first-3-byte layout, so the driver must accept a report `>= 3` bytes, not assume exactly 3)
- [ ] Produces a `PointerEvent` (relative `dx`/`dy` + button bitfield) via the existing `kernel-core` codec (`POINTER_EVENT_WIRE_SIZE` = 37 bytes)
- [ ] Host tests cover report decode including sign extension, button-bit mapping, **and a 4-byte report decoding to the same `PointerEvent` as its 3-byte prefix**

### A.3 — Report-Protocol skeleton (deferred from live use)

**File:** `kernel-core/src/usb/hid_report.rs` (new)
**Symbol:** `parse_report_descriptor`
**Why it matters:** Report-Protocol parsing unlocks touchpads, gaming mice, and multi-touch — but Boot Protocol is sufficient for every 1.0 keyboard and mouse, so this is genuinely deferrable.

**Acceptance:**
- [ ] A minimal report-descriptor item parser (Input items, Usage Page, Usage, Report Size, Report Count) deriving field bit-offsets — **host-tested only**
- [ ] Explicitly **not** wired to any live device for 1.0; the design-doc "Deferred Until Later" entry for Report Protocol is honored

---

## Track B — Input Integration + Smoke Gate

### B.1 — Inject USB input into `kbd_server` / `mouse_server` (Phase 56 dispatch unchanged)

**Files:**
- `userspace/kbd_server/src/main.rs`
- `userspace/mouse_server/src/main.rs`

**Symbol:** new inbound IPC labels `KBD_EVENT_INJECT` / `MOUSE_EVENT_INJECT`; a bounded pending-event queue in `KeyboardPipeline` / the mouse pipeline; merge into the existing `KBD_EVENT_PULL` (label 2) / `MOUSE_EVENT_PULL` (label 1) replies
**Why it matters:** Making USB an additional **producer** keeps `display_server`'s `InputWiring` and the `InputDispatcher` (`kernel-core/src/input/dispatch.rs:304`/`:379`) completely unchanged — USB and PS/2 merge into the same pull stream the compositor already drains. **Source-verified scope note:** `kbd_server`/`mouse_server` today are strictly **synchronous single-endpoint loops** (`ipc_recv` → match label → `ipc_reply`) with **no pending-event buffer** — there is nowhere for an asynchronously injected event to land. So this task is a real change to those servers, not just "add a label": it must add a bounded pending-event queue and define how an inject `ipc_call` interleaves with `PULL` waiters on the single endpoint.

**Acceptance:**
- [ ] `kbd_server` gains a **bounded pending-`KeyEvent` queue** in `KeyboardPipeline`; a new `KBD_EVENT_INJECT` handler enqueues pushed `KeyEvent`s from `usb-hid`, and `handle_kbd_event_pull` drains that queue **and** the PS/2 (`SYS_READ_SCANCODE`, `0x1007`) stream into each `KBD_EVENT_PULL` reply, with a defined drain priority (injected vs PS/2) and a defined inject reply contract
- [ ] `mouse_server` gains the analogous bounded pending-`PointerEvent` queue + `MOUSE_EVENT_INJECT` handler, merged into `MOUSE_EVENT_PULL` replies alongside the PS/2 packet (`SYS_READ_MOUSE_PACKET`, `0x1015`) stream
- [ ] The single-endpoint interleaving of an inject `ipc_call` with `PULL` waiters is defined (no reply-cap collision, no dropped events under a full queue) and documented
- [ ] `InputDispatcher::route_key_event` / `route_pointer_event` and `display_server/src/input.rs::InputWiring` are unchanged (verified by diff)
- [ ] PS/2 input still works under QEMU's i8042 emulation — both producers coexist (no regression)
- [ ] The rejected alternative (`usb-hid` as a third direct `display_server` `InputSource`) is documented with the reason (would fork focus/grab routing outside the single dispatcher)

### B.2 — Build + ramdisk + service wiring for `usb-hid`

**Files:**
- `Cargo.toml` (workspace `members`)
- `xtask/src/main.rs` (`build_userspace_bins`, line 795; `bins` array, line 800; `populate_ext2_files`, ~line 12597)
- `kernel/src/fs/ramdisk.rs` (`DRIVERS_ENTRIES`, line 1150)
- `userspace/init/src/main.rs` (`KNOWN_CONFIGS`, lines 185–230)
- `kernel/initrd/etc/services.d/usb-hid.conf` (new)

**Symbol:** `bins` tuple `(pkg, bin, needs_alloc=true)`, `DRIVERS_ENTRIES`, `usb-hid.conf`
**Why it matters:** `usb-hid` is a ring-3 driver that must be staged under `/drivers/` (not `/bin/`) or the `is_authorized_driver_process` gate (`device_host.rs:126`) denies `sys_device_claim`. It is a static daemon receiving device-attach notifications over IPC (the 78b A.3 lifecycle model), not forked per device.

**Acceptance:**
- [ ] `usb-hid` added as a Cargo `member` + `bins` entry with `needs_alloc = true` (mirrors the `("xhci_driver", …, true)` / `("usbhub", …, true)` entries)
- [ ] `usb-hid` binary embedded in `DRIVERS_ENTRIES` (`ramdisk.rs`) at `/drivers/usb-hid` so the `is_authorized_driver_process` gate (`device_host.rs:126`, `/drivers/` prefix) admits it for `sys_device_*`
- [ ] **(corrected)** service config follows the `xhci_driver`/`usbhub` precedent: written in `xtask::populate_ext2_files` (ext2 data disk) **and** added to `init` `KNOWN_CONFIGS`; uses `command=/drivers/usb-hid`, `type=daemon`, `restart=on-failure`, `depends=xhci_driver`. (The original `kernel/initrd/etc/services.d/usb-hid.conf` location is superseded — the existing USB drivers use the ext2 path.)
- [ ] **(corrected)** `usb-hid` is a plain `depends=xhci_driver` daemon. It is **not** added to `DECLARED_SESSION_STEP_NAMES` (`session_supervisor.rs:89` contains neither `xhci_driver` nor `usbhub`). It is a static daemon that looks up the `usb` service and receives `AttachNotice` over IPC (not forked per device, per the userspace-first rule)
- [ ] `cargo xtask clean` run after adding the config (forces ext2 disk recreation)

### B.3 — `usb-smoke` acceptance gate (QMP + serial; asserts a real keystroke-to-prompt)

**Files:**
- `xtask/src/main.rs` (new `cmd_usb_smoke`; QEMU arg additions; smoke step)
- `userspace/drivers/usb-hid/` (PASS sentinel) and/or `userspace/smoke-runner/src/main.rs`

**Symbol:** `cmd_usb_smoke`, QEMU `-device qemu-xhci -device usb-kbd -device usb-mouse`, `SMOKE:usb:PASS`
**Why it matters:** A serial `[xhci] N ports detected` sentinel proves only that the daemon ran — not that the event ring and interrupter delivered a real HID report and it reached the prompt. Per the AGENTS.md headless-framebuffer guidance, real input must be asserted via QMP, not a serial wait. (78a's `xhci-bringup-smoke` covers the controller; this gate covers the full input chain.)

**Acceptance:**
- [ ] QEMU launched with `-device qemu-xhci` plus `-device usb-kbd` and `-device usb-mouse` (extends the 78a QEMU arg builder)
- [ ] The gate asserts, **in causal order** (the emulated `usb-kbd` only emits an interrupt-IN report in response to an injected key — so injection precedes the Transfer-event observation, never after): (1) an `Enable Slot` Command Completion event is observed; (2) a QMP `send-key` is injected into the emulated `usb-kbd`; (3) the resulting interrupt-IN **Transfer event** carrying the 8-byte boot report is observed and decoded to a `KeyEvent`; (4) the keystroke reaches the login/shell prompt (USB → `usb-hid` → `kbd_server` → prompt), verified via QMP `screendump` (PPM occupancy) or a serial echo
- [ ] Mouse path: a QMP `input-send-event` relative mouse motion is injected into `usb-mouse` and the resulting `PointerEvent` is asserted to reach `mouse_server` (or, if `input-send-event` mouse injection proves unreachable in the harness, the A.2 mouse path is explicitly marked host-test-only and the gate does not imply live mouse verification)
- [ ] A serial sentinel alone (e.g. `[xhci] N ports detected`) is explicitly **not** sufficient for PASS
- [ ] Wired as `cargo xtask usb-smoke` with the opt-in pre-push gate `M3OS_USB_REGRESSION=1` (mirrors the heavyweight `htop-render-probe` / `compositor-stress` gates and the AGENTS.md hooks table)
- [ ] PS/2 i8042 input still passes its existing `smoke-test` coverage (no regression)

---

## Track C — Documentation + Release

### C.1 — Create the Phase 78 learning doc

**File:** `docs/78-usb-host-foundation.md`
**Symbol:** N/A
**Why it matters:** A learner-friendly doc scoped to the whole Phase 78 USB stack consolidates the bring-up story — TRB rings, the event-ring/interrupter completion model, descriptor-tree enumeration, and HID Boot Protocol — so readers do not reconstruct it from three sub-phases. Follows the "aligned legacy learning doc" template in `docs/appendix/doc-templates.md`.

**Acceptance:**
- [ ] File exists at `docs/78-usb-host-foundation.md`
- [ ] Required template fields populated: `**Aligned Roadmap Phase:** Phase 78`, `**Status:**`, `**Source Ref:** phase-78`, `**Supersedes Legacy Doc:** new`
- [ ] Overview explains, learner-first, why USB-HID is the 1.0 real-hardware unblocker (modern laptops have no PS/2 port) and how a ring-3 + IOMMU-DMA driver issues hardware transfers safely
- [ ] "What This Doc Covers" walks TRB rings, the event ring + interrupter (why an IRQ, not a poll, signals completion), the enumeration descriptor walk, and the HID boot-report layouts
- [ ] Key Files table cites the **real** files (`userspace/drivers/xhci`, `userspace/drivers/usbhub`, `userspace/drivers/usb-hid`, `userspace/lib/usb-core`, `kernel-core/src/usb/`, `kernel/src/syscall/device_host.rs`, the `kernel-core/src/input` codecs)
- [ ] "How This Phase Differs From Later USB Work" notes the deferrals (mass storage, UVC, USB audio, Report Protocol, hot-plug surface)
- [ ] Related Roadmap Docs links the three sub-phase design docs + their task lists
- [ ] Authored **after** 78a/78b/78c implementation so it cites the actual mechanism chosen (sentinel-BDF + class enumeration, MSI-X, BME, the `kbd_server`/`mouse_server` inject path)

### C.2 — Bump kernel version to `0.78.2` + add the capability entry

**Files:**
- `kernel/Cargo.toml`
- `Cargo.lock`
- `AGENTS.md`
- `docs/roadmap/README.md`

**Symbol:** `version` in `kernel/Cargo.toml` `[package]` (will be `0.78.1` after 78b)
**Why it matters:** 78c is the sub-phase where USB becomes a user-visible capability, so this is where the `AGENTS.md` capability inventory gains its "USB host stack" entry (per the file's keep-it-small policy — one bullet for a genuinely new capability class). The `0.78.2` cut closes the Phase 78 theme.

**Acceptance:**
- [ ] `kernel/Cargo.toml` `version = "0.78.2"`
- [ ] `Cargo.lock` regenerated (via `cargo xtask check`)
- [ ] `AGENTS.md` kernel version updated to `v0.78.2` and a new **"USB host stack"** capability-class bullet added (xHCI host driver + USB core/hub + HID — modern PS/2-less machines get keyboard/mouse input; detailed record stays in `docs/roadmap/`)
- [ ] `docs/roadmap/README.md` Phase 78 (umbrella) + 78a/78b/78c rows Status updated to "Complete"; the three sub-phase design-doc + task-doc Status headers set to Complete
- [ ] `cargo xtask check` passes
- [ ] Git tag `v0.78.2` — recommended at sub-phase merge (left to the merge step)

---

## Documentation Notes

- **The HID-input integration keeps Phase 56 untouched** by making `usb-hid` an injector into `kbd_server`/`mouse_server` (B.1) rather than a new dispatcher client — the `InputDispatcher` and `display_server` `InputWiring` are unchanged, and PS/2 and USB coexist as parallel producers. But B.1 is a **real change** to those servers (a bounded pending-event queue), not just a new IPC label.
- **Acceptance is QMP-driven, not serial-only** (B.3). A `[xhci] N ports detected` line proves the daemon ran; the gate must assert a QMP-injected keystroke reaching the prompt.
- **The learning doc (C.1) is authored last** so it cites the real mechanisms (sentinel-BDF + class enumeration, MSI-X, BME, the inject path), not the planning-doc assumptions.
- **The capability cut (C.2) lands here**, not in 78a/78b, because USB only becomes user-visible when HID input works.
- After adding the `usb-hid.conf` service config (B.2), run `cargo xtask clean` to force ext2 disk recreation.
