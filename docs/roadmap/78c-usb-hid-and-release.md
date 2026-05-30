# Phase 78c - USB Host Foundation: HID + Integration + Release

**Status:** Complete
**Source Ref:** phase-78c
**Depends on:** Phase 78b (USB Enumeration + Hub), Phase 56 (Display and Input Architecture) ✅, Phase 74 (IPC Capability Grants) ✅
**Builds on:** Final sub-phase of the [Phase 78](./78-usb-host-foundation.md) USB theme. On top of the enumerating stack from [78b](./78b-usb-enumeration-hub.md), adds the HID class driver (Boot-Protocol keyboard + mouse), injects its events into the Phase 56 `kbd_server` / `mouse_server` input path, lands the full `usb-smoke` QMP gate, writes the learning doc, and cuts the `0.78.2` release with the new USB capability inventory entry. Completes the milestone: a USB keyboard types into the m3OS shell.
**Primary Components:** `userspace/drivers/usb-hid/` (new), `kernel-core/src/usb/hid.rs` (new — usage→keycode + report decode, host-tested), `userspace/kbd_server/` + `userspace/mouse_server/` (new inject path), `xtask/src/main.rs` (the `usb-smoke` QMP gate), `docs/78-usb-host-foundation.md` (learning doc)

## Milestone Goal

A USB keyboard and mouse drive m3OS. The `usb-hid` driver puts each device into Boot Protocol, polls its interrupt-IN endpoint, decodes the 8-byte keyboard / 3-byte mouse reports into the Phase 56 `KeyEvent` / `PointerEvent` types, and injects them into `kbd_server` / `mouse_server` — which merge USB with PS/2 into the same pull stream `display_server` already drains, leaving the focus-aware dispatcher untouched. The `usb-smoke` gate proves a QMP-injected keystroke travels USB → `usb-hid` → `kbd_server` → the login prompt. Kernel cut to `0.78.2` with a "USB host stack" capability entry.

## Why This Phase Exists

78a/78b produce a controller that enumerates devices but does nothing user-visible. 78c is where USB becomes the point of the whole theme: real keyboard and mouse input on a PS/2-less machine. It is deliberately the last sub-phase because the HID-input integration touches the Phase 56 servers (a real change — they are synchronous pull loops today with no event buffer), needs the enumerated interrupt-IN endpoint from 78b, and the learning doc + capability cut can only be honest once the actual mechanisms (sentinel-BDF vs class enumeration, MSI-X, the inject path) are settled.

## Learning Goals

- See how HID report descriptors are simplified away by the keyboard/mouse Boot Protocols (`SET_PROTOCOL(0)` + `SET_IDLE`), and why the interrupt-IN endpoint is polled with Normal TRBs at `bInterval`.
- Understand how a new input device becomes an additional *producer* on an existing input path without changing the focus-aware dispatcher.
- Learn why asserting "the screen/prompt shows X" requires QMP injection + framebuffer/serial inspection, not a serial sentinel that only proves a program ran.

## Feature Scope

### Track A — HID class driver (ring 3)

`usb-hid`: Boot-Protocol keyboard (`SET_PROTOCOL(0)` + `SET_IDLE(0)`, interrupt-IN polling, 8-byte report → `KeyEvent`), Boot-Protocol mouse (3-byte report → `PointerEvent`, accepting reports ≥3 bytes), and a host-tested HID usage→keycode + report-decode layer in `kernel-core/src/usb/hid.rs`. A Report-Protocol descriptor-parser skeleton is host-tested only and deferred from live use.

### Track B — Input integration + smoke gate

Inject `usb-hid` events into `kbd_server` / `mouse_server` (a real change: add a bounded pending-event queue to those synchronous servers and define the inject/PULL interleaving), stage `usb-hid` under `/drivers/`, and land the full `usb-smoke` QMP gate (QEMU `-device qemu-xhci -device usb-kbd -device usb-mouse`) asserting a causally-ordered keystroke-to-prompt path.

### Track C — Documentation + release

The Phase 78 learning doc (`docs/78-usb-host-foundation.md`), the kernel `0.78.2` bump, and the `AGENTS.md` "USB host stack" capability-inventory entry.

## Important Components and How They Work

### Boot Protocol HID parsing

For a HID interface (`bInterfaceClass 0x03`), `SET_PROTOCOL(0)` selects fixed boot reports (no report-descriptor parsing) and `SET_IDLE(0)` suppresses duplicate reports. The interrupt-IN endpoint (configured into the controller in 78b) is polled with Normal TRBs at `bInterval`. Keyboard reports are 8 bytes (modifier + reserved + 6 keycodes, HID Usage IDs); mouse reports are at least 3 bytes (buttons + signed `dx` + signed `dy`, trailing wheel bytes ignored).

### The inject path (Phase 56 unchanged)

`usb-hid` decodes reports into `KeyEvent` / `PointerEvent` using the existing 20-/37-byte `kernel-core` codecs and pushes them into `kbd_server` / `mouse_server` over a new inbound IPC label. Those servers — synchronous single-endpoint pull loops today — gain a bounded pending-event queue that is drained into their existing `*_EVENT_PULL` replies alongside the PS/2 stream. The `InputDispatcher` and `display_server` `InputWiring` are untouched; PS/2 and USB coexist as parallel producers.

## How This Builds on Earlier Phases

- Consumes the enumerated, Configure-Endpoint'd interrupt-IN endpoint from 78b via the `usb-core` `UsbClient`.
- Reuses Phase 56's `KeyEvent` / `PointerEvent` wire formats (`kernel-core/src/input/events.rs`) verbatim — no new wire format.
- Ships `usb-hid` as a static service-config daemon (`command=/drivers/usb-hid`, `type=daemon`, `restart=on-failure`, `depends=xhci_driver`), wired through `xtask::populate_ext2_files` + init `KNOWN_CONFIGS` like the other USB drivers — **not** a `DECLARED_SESSION_STEP_NAMES` session step. It discovers its bound devices by walking the xHCI server's `NextAttach` cursor (a pull), not a push notification channel.

## Implementation Outline

1. `usb-hid` Boot keyboard: `SET_PROTOCOL(0)` + `SET_IDLE(0)`, poll interrupt-IN, decode → `KeyEvent`.
2. `usb-hid` Boot mouse: decode → `PointerEvent`.
3. Add the inject path to `kbd_server` / `mouse_server` (bounded pending-event queue + new IPC label).
4. Stage `usb-hid` under `/drivers/`; wire its service config + init `KNOWN_CONFIGS` ordering (`depends=xhci_driver`).
5. Land the `usb-smoke` QMP gate (inject key → Transfer event → prompt).
6. Report-Protocol skeleton (host-tested, deferred from live use).
7. Write the learning doc; bump kernel to `0.78.2` + add the `AGENTS.md` capability entry.

## Acceptance Criteria

- `cargo xtask run` with `-device qemu-xhci -device usb-kbd -device usb-mouse` types into the m3OS login prompt over USB.
- A `cargo xtask usb-smoke` gate asserts, in causal order: a real `Enable Slot` Command Completion event; a QMP `send-key` injection; the resulting boot-keyboard Transfer event decoded to a `KeyEvent`; and the keystroke reaching the prompt (QMP `screendump`/serial). Opt-in pre-push `M3OS_USB_REGRESSION=1`. A serial sentinel alone is not sufficient.
- HID usage→keycode + report decode (incl. a 4-byte mouse report decoding to the same `PointerEvent`) are host-tested in `kernel-core/src/usb/hid.rs`.
- `InputDispatcher` and `display_server` `InputWiring` are unchanged (verified by diff); PS/2 input still works (no regression).
- The Phase 78 learning doc exists at `docs/78-usb-host-foundation.md`.
- Kernel bumped to `0.78.2` with a "USB host stack" `AGENTS.md` capability entry.

## Companion Task List

- [Phase 78c Task List](./tasks/78c-usb-hid-and-release-tasks.md)

## How Real OS Implementations Differ

- Linux's HID stack has thousands of quirk entries and full Report-Protocol support; m3OS at 1.0 ships zero quirks and Boot Protocol only.
- Real stacks route HID through an evdev/input-subsystem abstraction; m3OS injects directly into the existing `kbd_server`/`mouse_server` producers.
- Production input handles N-key rollover, consumer-control keys, and multi-touch; m3OS at 1.0 handles the boot keyboard 6-key rollover and a 3-button mouse.

## Deferred Until Later

All deferrals below are assigned to **[Phase 90 — USB Class Expansion](./90-usb-class-expansion.md)** (post-1.0). The original 78c deltas (single-slot controller, host-test-only mouse, kbd_server-only smoke) were **closed** in the 78c "100%" pass — multi-slot now serves a keyboard **and** mouse simultaneously, live mouse is asserted end-to-end, and the `usb-smoke` gate screendump-verifies a typed USB key rendering at the focused term prompt.

- USB Report Protocol live use (touchpads, gaming mice, multi-touch, keyboard LEDs) — skeleton host-tested only → **Phase 90**
- External-hub multi-tier enumeration (devices behind a `usb-hub`); `usbhub` becomes a live `usb`-service consumer → **Phase 90**
- USB hot-plug event surface to userspace (`AttachNotice attached=false`, dynamic re-enumeration) → **Phase 90**
- USB mass storage (BBB/UAS bulk via the `PageGrant` transport), audio (UAC — HDA is the 1.0 audio bet, Phase 80), video (UVC) → **Phase 90**
- Multi-controller concurrent IRQ servicing → **Phase 90**
- USB-C / Power Delivery / DisplayPort alternate mode; xHCI Debug Capability → **Phase 90 (Deferred Until Later)**
