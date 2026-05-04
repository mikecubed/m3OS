# virtio-input Migration — Plan & Handoff

**Status:** Plan only. No implementation work has started. All current code
is on `feat/57d-voluntary-preemption` at `ae45431` (the typematic-revert
commit) — PS/2 input is intact and functional, with one known limitation
documented in [Symptom](#symptom) below.

**Branch to start from:** `feat/57d-voluntary-preemption` head (`ae45431`).
This branch already carries useful defensive work that should stay
regardless of transport (interleaved key/pointer drains in `display_server`,
shared kbd+mouse i8042 drain helper). It also carries some leftover
diagnostic scaffolding flagged for cleanup in
[Phase 4](#phase-4--cleanup) below.

**Estimated effort:** ~1.5 days of focused work, broken into four phases
sized for reviewable PRs.

---

## TL;DR for next session

1. **Symptom we're fixing:** holding a keyboard key freezes the mouse
   cursor in QEMU until the key is released. Diagnosed (with counters
   pushed in `d2f6978`) to QEMU's i8042 PS/2 emulation arbitrating
   kbd-priority — while the kbd FIFO has any byte, mouse bytes never
   get promoted to the controller's `OUTPUT_FULL`. Confirmed by
   counters: 6 s of held key (213 IRQ1 entries) produced **zero** IRQ12
   entries and **zero** mouse bytes drained, even with a 100 Hz
   timer-ISR backstop polling the controller. Real hardware does not
   exhibit this; it is a QEMU-emulation artifact.
2. **Why we're not patching PS/2 further:** all software workarounds
   (Linux-style "drain both byte types from each ISR", timer-driven
   poll, slowing hardware autorepeat to 2 Hz) either don't help or
   change behavior in ways the operator dislikes. QEMU's pckbd
   arbitration is the bottleneck and we can't reach it from the guest.
3. **Why virtio-input over USB HID:** USB HID would need a USB host
   controller stack (UHCI / EHCI / xHCI) plus a HID class driver — a
   phase-sized project. virtio-input slots into the existing
   `virtio_net.rs` / `virtio_blk.rs` shape (~500–800 lines per device),
   has no shared-bus arbitration, and works in any modern VMM. The
   only tradeoff is virtio-input is VM-only — PS/2 stays as the real
   hardware fallback.
4. **Approach: kernel-side translation, not protocol replacement.**
   The kernel virtio-input driver translates `(type, code, value)`
   events into the existing `SCANCODE_BUF` / `MOUSE_PACKET_RING`
   formats. `kbd_server` and `mouse_server` see no change — same
   syscalls, same wire formats, just a different producer. Smallest
   blast radius and keeps every userspace test untouched.
5. **Probe-time priority:** if virtio-input enumerates successfully at
   boot, mark the PS/2 ISRs dormant (early-return). If virtio-input is
   absent (real hardware, older QEMU), PS/2 stays active. No runtime
   switching once the decision is made.

---

## Symptom

Reproducer: boot the GUI (`cargo xtask run-gui --fresh`), wait for
`session.boot: state=running`, focus the term window, hold any
printable key, and move the mouse. The cursor remains frozen at its
last position until the key is released, at which point a burst of
queued mouse motion catches up.

Diagnostic counters (in `display_server`'s `compose#…` log line) make
this unambiguous. Representative window from `m3os.log` over a 6 s
held-key period:

```
compose#180 keys=14  ptrs=86  pos=520,459 irq1=14  irq12=130 mbytes=270
compose#540 keys=205 ptrs=86  pos=520,459 irq1=205 irq12=130 mbytes=270
                                                  ↑ unchanged ↑ unchanged
```

`irq1` ticked 191× (normal autorepeat); `irq12` and `mbytes` are
completely flat. The mouse hardware was generating motion the whole
time — bytes just stayed in QEMU's internal AUX queue, never reaching
the controller's `OUTPUT_FULL` for our ISR to read.

After release (1 s later):

```
compose#600 keys=206 ptrs=178 pos=405,551 irq1=205 irq12=260 mbytes=546
                                                  ↑ +130    ↑ +276
```

130 mouse IRQs and 276 byte deliveries arrive in a single 1 s window
— the queued motion floods through.

---

## Why each rejected workaround was rejected

| Attempt | Commit | Result |
|---|---|---|
| Bound per-pass key vs pointer drain in `display_server` | `becba8e` | Defensive but didn't address the symptom — mouse polls were already running, just returning `None` |
| ISR drains both kbd + mouse bytes (Linux-style) | `156bb9c` | No change — bytes still never reach `OUTPUT_FULL` |
| Diagnostic counters via `sys_ps2_diag_counter` | `d2f6978` | Pinned the bottleneck to below our ISR. Keep until virtio-input verified, strip after |
| Timer-ISR polls i8042 at 100 Hz | `7b305c7` | No change — empty buffer at every tick during keyhold |
| Slow hardware typematic to 2 Hz + decouple software repeat | `f3fc49e` | Reverted in `ae45431` — operator did not want hardware-state workaround for QEMU bug |

---

## Plan

### Phase 1 — kernel virtio-input driver (~6–8 h)

**Files**

- `kernel/src/input/virtio_input.rs` — new, ~600–800 lines. Modeled on
  `kernel/src/net/virtio_net.rs` for the queue/IRQ shape and
  `kernel/src/blk/virtio_blk.rs` for the lock discipline. Both are
  bare-virtio over PCI; copy-with-rename starts the file.
- `kernel/src/input/mod.rs` — add `pub mod virtio_input;` (module
  exists or create).
- `kernel/src/main.rs` — add a probe call in the same spot where
  `virtio_net::probe_and_init` and `virtio_blk::probe_and_init` are
  called today (search `probe_and_init` in `kernel_main`).

**PCI enumeration**

- Vendor `0x1AF4` (Red Hat / virtio).
- Legacy device id `0x1052`.
- Modern device id `0x1040 + 18 = 0x1052` *(coincidentally same — but the
  device id space encodes type via the PCI Subsystem ID for legacy and via
  the Device ID itself for modern; double-check with `lspci` on a
  virtio-input-enabled QEMU before finalising)*.
- Two devices (kbd + mouse) advertise separately — enumerate both,
  initialize each independently, store as `Option<VirtioInput>` per role
  in a static.

**Virtqueue setup**

- One rx (event) queue, 32–64 entries.
- Each entry holds a 16-byte `virtio_input_event { type: u16, code: u16,
  value: u32 }` struct (with padding to 16 bytes per the spec).
- Pre-fill the queue at init with 32 descriptors all pointing into a
  static event-buffer pool. On used-ring drain, copy the event out and
  re-add the descriptor to the available ring.

**IRQ handler**

- Acknowledge interrupt status (legacy: ISR register at PCI BAR; modern:
  ISR config cap).
- Drain the used ring. For each event, translate to legacy format:
  - `type=EV_KEY` (`0x01`) + `code` (Linux keycode) + `value` (0 = up, 1
    = down, 2 = repeat) → derive PS/2 set-1 scancode using a static
    Linux-keycode → PS/2 lookup table → push to `SCANCODE_BUF` (TTY) or
    `RAW_SCANCODE_BUF` (raw input owner) per existing
    `RAW_INPUT_ROUTER` policy.
  - `type=EV_REL` (`0x02`) + `code=REL_X|REL_Y|REL_WHEEL` → accumulate
    into a per-device `MousePacket` builder. Push the packet on the
    next `EV_SYN` (`type=0x00, code=SYN_REPORT`).
  - `type=EV_KEY` + button code (`BTN_LEFT=0x110`, `BTN_RIGHT=0x111`,
    `BTN_MIDDLE=0x112`) → record button state into the
    `MousePacket` builder; pushed on `EV_SYN`.
- Signal the corresponding IRQ notification (`signal_irq(1)` for kbd,
  `signal_irq(12)` for mouse) so `kbd_server` / `mouse_server` wake.

**PS/2 ISR dormancy**

- Add a `pub static VIRTIO_INPUT_ACTIVE: AtomicBool` somewhere central
  (perhaps `kernel/src/input/mod.rs`).
- `keyboard_handler` and `mouse_handler` early-return on entry when this
  flag is set, leaving any stray bytes in the i8042 buffer harmlessly.
- Set the flag on a successful `virtio_input::probe_and_init` for
  *both* kbd and mouse. If only one of the two virtio-input devices
  enumerates, leave PS/2 active (mixed mode is not worth the complexity).

**Acceptance criteria**

- `cargo xtask check` clean.
- Boot in QEMU with `-device virtio-keyboard-pci -device
  virtio-mouse-pci`. Log line `[virtio-input] kbd ready (queue_size=N)`
  and matching mouse line at boot.
- Type into term, see characters echo. Move the mouse, see cursor track.
- Hold a key + move mouse — cursor moves smoothly the whole time. The
  reproducer that motivated this work no longer triggers.

### Phase 2 — xtask QEMU args (~30 min)

**Files**

- `xtask/src/main.rs` — `qemu_args_with_devices_resolved` (~line 2091).
  Add `-device virtio-keyboard-pci` and `-device virtio-mouse-pci` to
  the unconditional device list. Keep PS/2 emulation (default in q35 /
  i440fx — no flag needed).

**Acceptance criteria**

- `cargo xtask test` (full QEMU test harness) passes, including any
  existing input-related tests.
- The two `-device virtio-…-pci` strings are in the args printed by
  `cargo xtask run-gui --print-args` (or whatever the local
  print-only path is).

### Phase 3 — strip diagnostic scaffolding (~30 min)

These were added during the post-mortem and should come out once
virtio-input is verified working.

**Files / commits to revert or trim**

- `kernel/src/arch/x86_64/ps2.rs` — remove `MOUSE_BYTES_SEEN`,
  `MOUSE_PACKETS_PRODUCED`, `MOUSE_RING_DROPS`, `IRQ1_ENTRIES`,
  `IRQ12_ENTRIES` and their increment sites.
- `kernel/src/arch/x86_64/syscall/mod.rs` — remove `PS2_DIAG_COUNTER`
  syscall (`0x101E`) and `sys_ps2_diag_counter`.
- `userspace/syscall-lib/src/lib.rs` — remove `SYS_PS2_DIAG_COUNTER`
  and `ps2_diag_counter`.
- `userspace/display_server/src/main.rs` — remove the `irq1=…
  irq12=… mbytes=… mpkts=… mdrops=…` portion of the compose log line.
  Keep the userspace-side `keys=… ptrs=… pos=…` counters (they're
  cheap and useful for routine input observability).
- `kernel/src/arch/x86_64/interrupts.rs` — remove the timer-ISR call
  to `ps2_drain_all_bytes` (commit `7b305c7`). Keep the shared
  `ps2_drain_all_bytes` helper and the per-IRQ drain that uses it
  (commit `156bb9c`) — that's defensive in its own right (matches the
  Linux i8042 driver shape and protects against any other arbitration
  quirk we haven't characterised).

**Keep**

- `userspace/display_server/src/input.rs` interleaved per-pass
  `MAX_DRAINS_PER_PASS` drain (commit `becba8e`). This is defensive
  and useful regardless of transport.
- The `DIAG_KEY_DRAINS_*` / `DIAG_PTR_DRAINS_*` counters in
  `userspace/display_server/src/input.rs` (also from `becba8e`/diag).
  Cheap, useful for observability.

**Acceptance criteria**

- Workspace clean of `MOUSE_BYTES_SEEN` / `IRQ12_ENTRIES` /
  `sys_ps2_diag_counter` / `ps2_diag_counter` references.
- `cargo xtask check` clean.
- The compose log line is shorter (no PS/2-specific fields).

### Phase 4 — fallback validation (~1 h)

**What to test**

- Boot with `-no-ps2` (or whatever QEMU flag suppresses the legacy
  i8042) in addition to the virtio devices — verify input still works
  and the kernel doesn't log spurious i8042 init failures.
- Boot WITHOUT the virtio devices (revert xtask change locally).
  Verify PS/2 still works as today; the original cursor-freeze
  symptom is back, but typing and pointer otherwise function.
- One real-hardware boot (USB stick to a physical machine) if
  feasible — confirms the PS/2 fallback path still exists.

---

## Open questions

1. **Modern vs legacy virtio split.** Modern virtio (1.0+) uses a
   different config layout (PCI capability list with multiple
   capabilities). Legacy is BAR0 + IO ports. Look at `virtio_net.rs`
   to see which we currently support — if it's modern-only, stay
   modern-only for input. If both, mirror.
2. **Linux-keycode → PS/2-scancode translation table size.** A 256-entry
   lookup per scancode set covers everything we need; can be a
   `[u8; 256]` static, ~256 bytes. Verify the table against
   `kernel-core::input::keymap::Keycode` definitions so the round-trip
   (virtio → PS/2 set 1 → existing `ScancodeDecoder` →
   `keymap::Keycode`) preserves identity.
3. **Where does the translation table live?** Probably
   `kernel-core/src/input/scancode_translation.rs` so it's host-testable;
   the kernel imports from there. Match the pattern in
   `kernel-core/src/input/mouse.rs` (pure logic, decoder + encoder, host
   tests).
4. **EV_REL clamping.** PS/2 mouse packets have 9-bit signed dx/dy
   (with overflow flags). virtio-input EV_REL events are i32. Decide
   whether to clamp + set overflow flag, or split a large delta across
   multiple PS/2 packets. Splitting is cleaner; clamping matches the
   PS/2 hardware shape exactly.

---

## Out of scope

- No userspace changes to `kbd_server` / `mouse_server`. They keep
  reading the same syscalls.
- No phase doc per `docs/roadmap/` conventions. This is a regression
  fix, not a new feature track.
- USB HID. Documented as the rejected alternative above; revisit only
  if virtio-input proves insufficient (would not).
- Removing PS/2 entirely. Keep as fallback for real hardware and for
  mixed-input boots where one of the virtio devices fails to
  enumerate.

---

## Reference: relevant commits on `feat/57d-voluntary-preemption`

| Commit | Subject | Status |
|---|---|---|
| `becba8e` | display_server interleaved drain, MAX_DRAINS_PER_PASS | Keep |
| `156bb9c` | Shared `ps2_drain_all_bytes` ISR helper | Keep |
| `d2f6978` | `sys_ps2_diag_counter` + IRQ counters | Strip in Phase 3 |
| `7b305c7` | Timer-ISR `ps2_drain_all_bytes` call | Strip in Phase 3 |
| `f3fc49e` | 2 Hz typematic + soft-repeat decouple | **Reverted** in `ae45431` |
| `ae45431` | Revert `f3fc49e` | Current branch head |
