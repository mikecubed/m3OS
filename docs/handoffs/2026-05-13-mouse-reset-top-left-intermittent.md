---
status: downgraded-post-1.0-followup
resolution: "Phase 77 Track G.5 (2026-05-28) — too rare to reproduce on demand (reboot-recoverable, intermittent). Explicitly downgraded to a known intermittent issue / post-1.0 follow-up per the G.5 acceptance. The serial transcript is retained across runs via M3OS_SMOKE_SERIAL_DUMP=<path> and the AGENTS.md headless-screenshot (QMP screendump) path, so the sticky-state can be analysed after the fact when it next occurs; no synchronous root-cause is blocking 1.0."
priority: medium (user-visible UX regression, intermittent, reboot-recoverable)
date: 2026-05-13
component: PS/2 mouse pipeline — kernel decoder ↔ `MOUSE_PACKET_RING` ↔ `mouse_server` ↔ `display_server` cursor integration
related:
  - kernel/src/arch/x86_64/ps2.rs
  - kernel-core/src/input/mouse.rs
  - userspace/mouse_server/src/main.rs
  - userspace/display_server/src/input.rs
  - userspace/display_server/src/main.rs
  - userspace/display_server/src/compose.rs
log: not captured yet — see [Reproduction](#reproduction) for what to collect on the next occurrence
---

# Handoff — mouse cursor intermittently sticks at top-left corner after slight motion

> **Bug shape from user report.** Sometimes after boot the mouse cursor
> behaves normally for a while, then enters a sticky bad state: tiny
> motions cause the cursor to **continuously reset to the top-left
> corner of the screen** (or stay pinned there). Rebooting clears the
> state and the mouse works normally again for some period. No log
> captured during the failure window yet.

## TL;DR

The PS/2 mouse pipeline has four hand-off boundaries where framing or
ring state can desync and stay desynced:

1. **IRQ12 → kernel decoder** (`kernel/src/arch/x86_64/ps2.rs::feed_byte_isr`):
   raw bytes from the AUX port are fed through `Ps2MouseDecoder::feed`.
   The decoder reports `DecoderEvent::Resync` when it sees a byte at
   `cursor=0` without the required PS/2 sync bit, but **the kernel
   currently does not count Resync events** — there is no
   `MOUSE_DECODER_RESYNCS` counter (only `MOUSE_BYTES_SEEN`,
   `MOUSE_PACKETS_PRODUCED`, `MOUSE_RING_DROPS`).
2. **Kernel decoder → packet ring** (`MOUSE_PACKET_RING`): packets push
   to a ring; on overflow the oldest is silently dropped and
   `MOUSE_RING_DROPS` bumps. Ring overflow itself is harmless to
   framing (the decoder is upstream), but is the most common
   symptom of downstream stalls.
3. **Kernel ring → `mouse_server`** (userspace): polls via syscall,
   emits relative-delta `PointerEvent`s with `abs_position: None`.
4. **`mouse_server` → `display_server`**: `display_server` integrates
   relative deltas into an absolute `pointer_position: (i32, i32)`
   via `saturating_add` in
   `userspace/display_server/src/input.rs:601-602`. Initial value at
   startup is `(0, 0)` (`userspace/display_server/src/main.rs:280`).

**Most likely root cause** (per pattern-matching the symptom):
boundary #1 produces a misframed packet — a byte that happens to have
bit 3 set is treated as a status byte mid-stream — yielding a packet
with garbage `dx` / `dy` whose sign bits push the integrated
`pointer_position` toward zero. Once `pointer_position` is at or near
(0, 0), every subsequent small motion is itself absorbed by the next
misframed packet that pulls it back to (0, 0), creating the "resets to
top-left" loop. Reboot resets the decoder state and the AUX port,
restoring sync.

Less likely but worth eliminating:

- **AUX ring overrun chain** (`MOUSE_RING_DROPS` jumping during the
  symptom). If the userspace `mouse_server` falls behind the kernel
  ring, dropped packets desync the userspace `ButtonTracker` from the
  hardware button state but should NOT cause position drift; the
  relative-delta integration is robust against dropped packets so long
  as packets themselves are well-formed.
- **`display_server` integer overflow**. `saturating_add` saturates at
  `i32::MIN` / `i32::MAX`. If a misframed packet produces
  `dx = i32::MIN`, `pointer_position.0` saturates to `i32::MIN`. The
  cursor blit at `userspace/display_server/src/compose.rs:387-388`
  uses `i64::saturating_sub` and would compute an `origin_x` far
  off-screen. The visible result would be **no cursor rendered**,
  not "cursor at (0, 0)" — so saturation alone doesn't match the
  symptom. But a combination of misframing + clamping at
  `userspace/display_server/src/main.rs:707`
  (`pointer_position.0.max(0) as u32` for client-event broadcast)
  could mask the real position from clients while compose still
  renders correctly. Worth checking on the next reproduction.

## Reproduction

User-observed pattern:

1. Boot to graphical session.
2. Use mouse normally for some period (minutes to hours; no
   deterministic trigger known).
3. Symptom onset: tiny mouse motion causes cursor to reset to top-left
   continuously.
4. Reboot restores normal operation.

**What to capture next time it happens** (in order of priority):

1. **Serial log snapshot** during the bad state. The kernel diagnostic
   counters live at:
   - `kernel::arch::x86_64::ps2::MOUSE_BYTES_SEEN`
   - `kernel::arch::x86_64::ps2::MOUSE_PACKETS_PRODUCED`
   - `kernel::arch::x86_64::ps2::MOUSE_RING_DROPS`
   - `kernel::arch::x86_64::ps2::IRQ12_ENTRIES`
   No syscall surfaces these today; instrument a temporary
   `serial_println!` dump on a magic key combo or a periodic ~1 Hz
   log of the four counters while debugging.
2. **`display_server`'s `pointer_position`** at the time of symptom.
   The value is local to `userspace/display_server/src/main.rs:280`
   and not currently printed; a temporary instrumentation logging the
   integrated value on each `InputEffect::CursorMoved` would show
   whether `pointer_position` is actually at `(0, 0)` or whether the
   compose path is misrendering.
3. **The raw byte stream from the AUX port** would be definitive but
   currently undumpable without instrumentation. Adding a small
   ring of the last 64 bytes seen by `feed_byte_isr` (stored in a
   `static [u8; 64]` ring with an `AtomicUsize` head, ISR-safe) and
   dumping it on the same magic-key combo would let the next session
   read the actual framing.

## Hypotheses (ranked by likelihood)

### Hypothesis A — sticky PS/2 framing loss inside `Ps2MouseDecoder` (**most likely**)

The decoder considers `cursor == 0` ∧ `(byte & STATUS_SYNC_BIT) == 0`
to be the misalignment signal (`kernel-core/src/input/mouse.rs:146`).
`STATUS_SYNC_BIT` is bit 3 of the PS/2 status byte, which must always
be 1 in a real status byte. But ~12% of arbitrary bytes have bit 3
set, so a misframed stream can land on a non-status byte that happens
to satisfy the predicate and parse the next two bytes as `dx` / `dy`
with the wrong interpretation.

What makes this "sticky": once the decoder is misframed, every
subsequent packet is *also* misframed at the same offset, because the
decoder advances `cursor` by 1 per byte and resets on packet-boundary
(`cursor = 0` after writing 3 bytes). Without a kernel-side resync
mechanism that hard-resets the AUX device, the misalignment persists
until reboot.

The Phase 56 reset path (`Ps2MouseDecoder::resync`) does not help
here — it's only called from task context inside
`kernel::arch::x86_64::ps2::init_mouse`, never from the ISR. The ISR
just swallows `DecoderEvent::Resync` and continues (`ps2.rs:210`).

**Fix sketch**: extend `feed_byte_isr` to count `DecoderEvent::Resync`
hits and, when N consecutive Resyncs are seen without an intervening
Packet, schedule a kernel-side re-init of the AUX port (the full
`init_mouse` sequence, gated through a deferred-task path so the ISR
isn't doing it inline). Threshold could be 4–8 consecutive Resyncs:
~3 bytes per packet × N misframed packets, well above transient noise
but well below the time the user perceives the bad state.

### Hypothesis B — AUX ring overrun during a kernel-side scheduling spike

If `MOUSE_PACKET_RING` fills because `mouse_server` couldn't drain it
fast enough (kernel stall, GC-like sweep, scheduler hiccup), the
`push_packet_isr` path silently overwrites the oldest packet
(`ps2.rs:218-227`). Dropped packets disturb the userspace
`ButtonTracker`'s button-edge detection but should not cause position
drift — relative deltas are still correct, just with some lost. So
this hypothesis is unlikely to explain the symptom *alone*, but it
could be a co-factor: a ring overflow might correlate with the
underlying scheduling event that also caused the framing loss.

**Action**: when the symptom recurs, check `MOUSE_RING_DROPS` against
its pre-symptom value. A jump implicates this path.

### Hypothesis C — `pointer_position` saturation in `display_server`

`current_pointer.0.saturating_add(ev.dx)` saturates at `i32::MIN` and
`i32::MAX`. If a misframed packet produces `dx = -2_147_483_648`, the
position saturates to `i32::MIN` permanently — every subsequent small
delta also saturates because `i32::MIN + ε` ≈ `i32::MIN`. The cursor
would be rendered far off-screen (compose.rs uses `i64::saturating_sub`
so the negative origin doesn't wrap), which matches "cursor not
visible at expected position." The `max(0) as u32` client-event path
would emit `0` to clients, which could explain "looks like (0, 0)
from a client's perspective" even though compose is correctly drawing
the cursor far off-screen.

**Fix sketch**: clamp `pointer_position` to `[0, screen_width) ×
[0, screen_height)` after each integration step, not after the fact
in the client-event path. Use `fb::console_text_size` or the
compose-time framebuffer extent to get bounds.

### Hypothesis D — `MouseDecoder` `wheel_mode` state mismatch

`init_mouse` documents "Step 4 — stay in the standard 3-byte packet
mode" (`ps2.rs:401-404`), so `wheel_mode` should be `false`. But if
some QEMU front-end or BIOS leaves the device in 4-byte (IntelliMouse)
mode and the decoder is in 3-byte mode, every 4th byte is misframed
and the rest of the bytes shift. The decoder would parse status bytes
from offsets that aren't packet boundaries.

The current init does `MOUSE_CMD_SET_DEFAULTS` (`ps2.rs:399`) which
*should* reset to 3-byte mode, but `MOUSE_CMD_SET_DEFAULTS` (0xF6)
sets sample rate / resolution / scaling but **does not necessarily
clear IntelliMouse mode** — that requires the documented sample-rate
sequence to *enter* IntelliMouse mode, but exit semantics are device-
dependent.

**Action**: before `MOUSE_CMD_ENABLE_STREAMING`, issue a hard
`MOUSE_CMD_RESET` (`0xFF`) and wait for the `0xAA` + `0x00` BAT
response. This forces the device into known 3-byte mode regardless
of prior state.

### Hypothesis E — race between `init_mouse` and the BIOS / firmware

If `init_mouse` runs while the BIOS is mid-packet (e.g. firmware
left a partial packet in the AUX FIFO that the `drain_output()`
call missed), the first user motion's bytes append to a partial
packet from before init, producing one malformed packet that throws
the decoder out of sync.

**Action**: increase `drain_output()` to a `drain_output_thoroughly()`
that polls the OBF for ~10 ms before declaring drained, OR rely on
the hard-reset fix from Hypothesis D.

## Code surface — what to read first

1. **`kernel/src/arch/x86_64/ps2.rs:199-212`** —
   `feed_byte_isr`. The bridge between IRQ bytes and the decoder.
   Add `MOUSE_DECODER_RESYNCS` counter here.
2. **`kernel-core/src/input/mouse.rs:135-195`** —
   `Ps2MouseDecoder::resync` and `feed`. The framing state machine.
   Consider adding a `consecutive_resyncs: u8` field that triggers
   a "force_resync" event after N consecutive Resyncs.
3. **`kernel/src/arch/x86_64/ps2.rs:379-413`** —
   `init_mouse`. Add `MOUSE_CMD_RESET` (0xFF) + BAT-response wait
   before `MOUSE_CMD_ENABLE_STREAMING`.
4. **`userspace/display_server/src/input.rs:594-625`** — relative-
   to-absolute integration. Consider clamping rather than
   saturating.
5. **`userspace/display_server/src/main.rs:280`** —
   `pointer_position` initial value. Consider seeding to screen
   center on startup; (0, 0) is a fragile initial value.

## Recommended investigation order

1. **Add `MOUSE_DECODER_RESYNCS` counter + periodic ~1 Hz serial
   dump of `(BYTES_SEEN, PACKETS_PRODUCED, RING_DROPS,
   DECODER_RESYNCS, IRQ12_ENTRIES)`** — call it `[ps2-mouse]`
   prefix. ~20 LoC in `ps2.rs`. Reproduce the bug and check
   whether `RESYNCS` is growing during the bad state. This single
   observation either confirms Hypothesis A or rules it out.
2. **If Hypothesis A confirmed**: add a kernel-side automatic
   re-init when the decoder reports `>= 4` consecutive Resyncs.
   Probably ~40 LoC: a deferred task (similar to the existing
   `tlb_shootdown` patterns) that runs `init_mouse` outside ISR
   context.
3. **In parallel** (orthogonal to A): change `init_mouse` to
   issue `MOUSE_CMD_RESET` (0xFF) before `SET_DEFAULTS`. Even if
   it doesn't fix the running-state bug, it improves boot
   robustness against BIOS-leftover state (Hypothesis E).
4. **In parallel**: clamp `pointer_position` to framebuffer bounds
   in `display_server` rather than saturating to `i32::MIN` /
   `i32::MAX`. Doesn't fix the root cause but prevents the visual
   "cursor disappears off-screen" symptom of any underlying drift.

## What this is NOT

- **Not related to PR #155** (page-fault re-entrance guard / kstack
  guard pages / pipe tripwire) or PR #156 (grow_heap TLB shootdown).
  Those PRs touch the kernel-stack and TLB paths, not PS/2 input.
- **Not related to the `reply_v2:deadline_expired_no_deadline`
  spurious wake** tracked in
  `docs/handoffs/2026-05-13-reply-v2-deadline-residual-race.md`.
  That's a scheduler diagnostic; mouse_server's reply paths haven't
  been observed in those warnings.
- **Not a "Phase 64 made it worse" symptom** — Phase 64 didn't touch
  PS/2 or display_server. Likely a pre-existing latent issue surfaced
  by longer-running sessions now that the kernel-pipe-table cascade
  no longer terminates the kernel early.

## References

| Resource | Where |
|---|---|
| IRQ12 → decoder bridge | `kernel/src/arch/x86_64/ps2.rs::feed_byte_isr` (line 199) |
| Decoder state machine | `kernel-core/src/input/mouse.rs::Ps2MouseDecoder::feed` (line 145) |
| Decoder resync entry point | `kernel-core/src/input/mouse.rs::Ps2MouseDecoder::resync` (line 135) |
| Packet ring | `kernel/src/arch/x86_64/ps2.rs::MOUSE_PACKET_RING` + `push_packet_isr` (line 214) |
| Mouse init sequence | `kernel/src/arch/x86_64/ps2.rs::init_mouse` (line 379) |
| Existing diagnostic counters | `MOUSE_BYTES_SEEN`, `MOUSE_PACKETS_PRODUCED`, `MOUSE_RING_DROPS`, `IRQ12_ENTRIES` (ps2.rs lines 114–119) |
| Relative-delta integration | `userspace/display_server/src/input.rs:594-625` |
| `pointer_position` ownership | `userspace/display_server/src/main.rs:280, 797, 855` |
| Cursor blit (compose) | `userspace/display_server/src/compose.rs::blit_cursor` (line 376) |
| Client-event clamp to ≥ 0 | `userspace/display_server/src/main.rs:707-709` |
