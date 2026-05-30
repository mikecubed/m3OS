# Phase 78c — USB Host Foundation: HID + Integration + Release: Task List

**Status:** Planned
**Source Ref:** phase-78c
**Depends on:** Phase 78b (USB Enumeration + Hub), Phase 56 (Display and Input Architecture) ✅, Phase 74 (IPC Capability Grants) ✅
**Goal:** Complete the USB milestone — a USB keyboard and mouse drive m3OS. Add the `usb-hid` Boot-Protocol class driver, inject its events into the Phase 56 `kbd_server`/`mouse_server` input path (leaving the dispatcher unchanged), land the full `usb-smoke` QMP gate (keystroke → prompt), write the Phase 78 learning doc, and cut `0.78.2` with the new USB capability inventory entry. Final of three Phase 78 sub-phases ([78a](../78a-xhci-host-bringup.md) → [78b](../78b-usb-enumeration-hub.md) → [78c](../78c-usb-hid-and-release.md)).

> **Source-verified (2026-05-30):** `KeyEvent`/`PointerEvent` already exist with stable 20-/37-byte codecs (`kernel-core/src/input/events.rs:146`/`:199`); the input syscalls are `SYS_READ_SCANCODE` (`0x1007`) / `SYS_READ_MOUSE_PACKET` (`0x1015`); the dispatcher is `InputDispatcher::route_key_event`/`route_pointer_event` (`kernel-core/src/input/dispatch.rs:304`/`:379`); `kbd_server`/`mouse_server` are synchronous single-endpoint pull loops with **no** pending-event buffer (so the inject is a real change, not just a label); there is **no** `qemu-xhci`/`usb-kbd` in xtask today.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | HID class driver (ring 3): Boot keyboard, Boot mouse, Report-Protocol skeleton | Phase 78b | Planned |
| B | Input integration + smoke: inject into `kbd_server`/`mouse_server`, stage `usb-hid`, full `usb-smoke` QMP gate | A, Phase 56 ✅ | Planned |
| C | Documentation + release: learning doc, `0.78.2` bump + capability entry | A, B | Planned |

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
- [ ] `usb-hid` added as a Cargo `member` + `bins` entry with `needs_alloc = true`
- [ ] `usb-hid` binary embedded in `DRIVERS_ENTRIES` (`ramdisk.rs`) at `/drivers/usb-hid`
- [ ] `usb-hid.conf` added to `kernel/initrd/etc/services.d/` **and** `init` `KNOWN_CONFIGS`; uses `command=/drivers/usb-hid`, `type=daemon`, `restart=on-failure`, `depends=xhci`
- [ ] `session_manager` start sequence (`DECLARED_SESSION_STEP_NAMES`, `kernel-core/src/session_supervisor.rs:89`) brings `usb-hid` up before `greeter`; on attach, `xhci` sends a device-attach IPC notification to the running `usb-hid` daemon (not forked per device, per the userspace-first rule)
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
