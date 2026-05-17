---
status: partially-fixed-still-racy
branch: feat/phase-69d-tui-app-foundation (PR #176) — investigation surface
last-known-good-commit: 8497e0c  # double-buffer + Clear-only defer
fix-commit: 8497e0c
date: 2026-05-17
component: userspace/term + kernel-core/fb + userspace/display_server (compose)
related:
  - docs/roadmap/69d-tui-app-foundation.md
  - docs/handoffs/2026-05-16-phase-69d-100-percent-followups.md
diag-branches:
  - diag/less-render-trace          # TT:* / PI:* / EV:* / PO:* traces + TIOCSWINSZ-at-init fix
  - feat/term-csi-completion         # csr / vpa / su/sd / il / dl / ich / dch / ech dispatches
  - diag/csi-completion-test         # both stacked
ruled-out-hypotheses:
  - cursor-trail clear in compose.rs (EV:Ptr = 0; symptom is black not teal)
  - 500 ms blink-tick wipe (Probe A: cursor default → SteadyBlock; bug still fires within 60 ms)
  - LLVM / store-visibility race (Probe B: SeqCst fence before publish_frame; bug still fires)
new-tooling:
  - cargo xtask less-render-probe  # QMP send-key + screendump; captures the bug pixel-state
---

# Handoff — `less` content disappears between events inside `term`

## User-reported symptom

Running `cargo xtask run-gui` (default mode, `display_server` active),
opening a small file with `less /readme.txt`, and pressing Down or Up:

> The text flashes on screen then disappears when pressing up or down.

The content briefly appears, stays for a moment, then the screen goes
black until the next keypress.  The `cargo xtask tui-app-smoke` gate
passes — it only inspects the serial stream, not the framebuffer, so
this regression is invisible to CI.

## Investigation summary

Three diagnostic passes against the running guest, each captured via
the `M3OS_SMOKE_SERIAL_DUMP` mechanism extended with userspace
serial-tag traces (see the diag branches above).

### Pass 1 — kernel framebuffer console (TTY0) yields to display_server

`kernel/src/fb/mod.rs:964` early-returns from `fb::write_str` when
`CONSOLE_YIELDED=true`, which `display_server` sets at startup via
`try_yield_console`.  All TTY0 output (the shell, `less` if it ran on
TTY0) is dropped silently on the framebuffer while still reaching
serial.  The smoke gate sees `root:` on the serial mirror and passes;
the framebuffer never receives the bytes.

This finding rules out the *TTY0 render path* as the cause of the
user-visible bug — the user is in `term` (the userspace terminal
emulator), not on TTY0.  But it is a real "smoke gate blind to
rendering" hole worth fixing on its own: the gate should assert pixel
state via QMP `screendump`, not only serial text.

### Pass 2 — confirm `less` is repainting fully on every event

The diag branch `diag/less-render-trace` adds four serial-tag traces
inside `term`:

- `TT:*` — every `RenderCommand` term applies (`Put` / `Clear` /
  `Scroll` / `Move` / `Color` / `Bell` / `Mouse`).
- `PI:*` — every byte term reads from the PTY primary (i.e., bytes
  `less` writes).  Per-digit `PI:0`..`PI:9` so `\E[<n>J` parameters
  are recoverable.
- `EV:*` — every `PulledEvent` (`Key` / `Pointer` / `Resize` /
  `Disconnect`) `display_server` sends to term.
- `PO:*` — every byte term writes back to the PTY primary (i.e.,
  bytes `less` reads).

Three captures, each via `cargo xtask run-gui 2>&1 | tee
/tmp/term-trace.log` and a `less /readme.txt` → Down x2 → q
interaction:

| Branch | TT:Clear | PI:2 | PI:J | EV:Key | EV:Ptr | EV:Resize | PO:^L | Notes |
|---|---|---|---|---|---|---|---|---|
| Baseline (PR 176 foundation) | 7 | 7 | 7 | 46 | 0 | 0 | 0 | 7-cycle redraw, ~4170 events per cycle |
| `diag/less-render-trace` (TIOCSWINSZ fix at term init) | 5 | 5 | 5 | 46 | 0 | 0 | 0 | 5-cycle redraw, ~8266 events per cycle ("stays a little longer") |
| `diag/csi-completion-test` (TIOCSWINSZ + CSI completion) | 5 | 5 | 5 | 36 | 0 | 0 | 0 | Same 5-cycle redraw — CSI completion did not change `less` behavior |

**`PI:ESC PI:[ PI:2 PI:J PI:ESC PI:[ PI:H` immediately precedes every
`TT:Clear`** — `less` is using `\E[2J\E[H<content>` for every screen
update.  It is *not* using `csr` / `vpa` / `su`/`sd` / `il`/`dl` /
`ich`/`dch`/`ech`, even with all of them dispatched correctly by the
parser on `feat/term-csi-completion`.  `less` chose the full-repaint
path at startup and never reconsiders.

`EV:Resize = 0` everywhere — `display_server` never sends
`SurfaceResized`, so `term`'s `handle_surface_resize` never runs.  The
"`less` getting SIGWINCH" hypothesis is ruled out.

`PO:^L = 0` everywhere — nothing is delivering Ctrl-L to `less`.

### Pass 3 — `less` repaints atomically, so the wipe is downstream

Each `\E[2J\E[H<content>` arrives at `term` as one or a few PTY chunks.
The `term` main loop drains the entire chunk through `screen.feed`,
queues `Clear + many Put` into the renderer, then calls
`renderer.compose` once per throttle window (16 ms ≈ 60 Hz).  Compose
drains the queue in order and calls `fb.submit()` exactly once,
publishing one full damage rect of the surface.

So `less`'s full repaints are a single atomic frame from the user's
viewpoint — they should produce a momentary refresh, not a flicker.
Yet the user reports the content disappears *between* events.

That points the remaining work at **`userspace/display_server/src/
compose.rs`**.

## Production-fix candidates already on branches

### `diag/less-render-trace` — TIOCSWINSZ-at-init

The kernel default for new PTYs is `Winsize::default_console() = 24
× 80` (`kernel-core/src/tty.rs:232`).  `term::Screen::new()` defaults
to `25 × 80` (`userspace/term/src/lib.rs:104-108`).  Without an
explicit `TIOCSWINSZ` at `term` init, the shell and `less` query
`TIOCGWINSZ` and see `24 × 80` — off by one from `term`'s grid.

The fix sits right after `pty.open_and_spawn()`:

```rust
let ws = syscall_lib::Winsize {
    ws_row: term::DEFAULT_ROWS,
    ws_col: term::DEFAULT_COLS,
    ws_xpixel: 0,
    ws_ypixel: 0,
};
syscall_lib::ioctl(primary_fd, syscall_lib::TIOCSWINSZ,
                   &ws as *const _ as usize);
```

Did not fix the user-visible bug on its own, but it is a real
correctness gap that any future TUI app will rely on.  Worth landing.

### `feat/term-csi-completion` — incremental-repaint CSI set

New `ConsoleCmd` variants in `kernel-core/src/fb.rs`:

- `SetScrollRegion { top, bottom }` for DECSTBM (`CSI <t>;<b> r`)
- `VerticalPositionAbsolute(n)` for VPA (`CSI <n> d`)
- `ScrollUp(n)` / `ScrollDown(n)` for SU/SD (`CSI <n> S` / `CSI <n> T`)
- `InsertLines(n)` / `DeleteLines(n)` for IL/DL (`CSI <n> L` / `CSI <n> M`)
- `InsertChars(n)` / `DeleteChars(n)` for ICH/DCH (`CSI <n> @` / `CSI <n> P`)
- `EraseChars(n)` for ECH (`CSI <n> X`)

`dispatch_csi` wires each final byte.  `userspace/term/src/screen.rs`
gains scroll-region state (`scroll_top` / `scroll_bottom`,
re-clamped on `resize`), region-aware `line_feed`,
`scroll_region_up` / `scroll_region_down` with full-screen fast path,
and the IL/DL/ICH/DCH/ECH implementations.
`kernel/src/fb/mod.rs` and `userspace/console_server/src/main.rs`
accept the new variants and no-op them (with cheap VPA + ECH
honoured) so the wire protocol is honest across all consumers.

`cargo xtask check` passes.

Did not fix the user-visible bug — `less` continues to emit
`\E[2J\E[H<content>` rather than the new sequences.  Still worth
landing as a terminal-contract completion: any future TUI app that
*does* attempt incremental updates will now reach a real handler.

## Where the bug actually lives — hypothesis for the next session

Reading `userspace/display_server/src/compose.rs` lines 290-306:

```rust
} else if cursor_motion
    && let (Some(prev_pos), Some(prev_size)) = (ctx.prev_pointer, ctx.prev_cursor_size)
{
    // Cursor-trail fix (Phase 56 follow-up) ...
    let damage = cursor_damage(prev_pos, prev_size, pointer_position, cursor_size);
    for rect in damage {
        clear_rect_to_background(owner, rect)?;
    }
}
```

When the cursor moves and we are not on the first compose, the union
of (old cursor box + new cursor box) is wiped to `BG_PIXEL` *before*
the surface-blit pass runs.  Inside mapped-surface bounds the clear
is overpainted by the surface-blit pass below — but that
overpainting depends on `compose_frame` re-blitting the cursor
damage area through `compose.damage` rects translated into surface-
local coordinates.

Suspected failure mode: when `term`'s surface fully covers the
output (which it does in `run-gui`'s default 1280×800 layout), and
the user has moved the mouse over the term window, the cursor-trail
clear writes `BG_PIXEL` *inside* term's surface area, then
`compose_frame` is supposed to re-blit term's pixels through the
damage rect — but the snapshot it reads (`entry.buf.pixels_snapshot()`
at line 381) and/or the per-surface damage list (lines 357-377) may
not fully cover what was wiped.  Result: holes show through to
background, and on a uniform-content surface (less's mostly-blank
file display) those holes look like the whole screen has gone dark.

## 2026-05-17 follow-up — render-probe tool + ruled-out hypotheses

Added a QMP-based pixel-state probe so the bug stops being invisible
to CI, then used it to rule out two leading hypotheses.

### Tool: `cargo xtask less-render-probe`

Commit `0bdcd62` adds the probe. It boots m3OS with a VNC-backed
display (the default `-display none` returns an empty screendump
surface — `query-display-options` reports `{"type":"none"}`), waits
for the `display.input-owner` + `TERM_SMOKE:prompt-ready` serial
markers, then drives less via PS/2 through the QMP `send-key`
verb. After each keystroke it burst-captures PPM screendumps at
20 / 60 / 120 / 200 / 400 / 800 / 1500 ms and reports per-frame
hash + non-black-pixel ratio + non-black row/col spread plus the
peak-vs-settled diff.

Components:
- `xtask/src/qmp.rs` — minimal QMP client (`UnixStream` + JSON,
  sequential request/reply; `send-key`, `screendump`, ASCII-to-qkey
  translator).
- `xtask/src/ppm.rs` — P6 reader + FNV-1a hash + black-ratio +
  non-black-spread heuristics. No image-crate dependency.
- `xtask/src/main.rs` — `less-render-probe` subcommand.

Usage:

```bash
cargo xtask less-render-probe --timeout 240 --out /tmp/probe
```

Output goes into the `--out` directory as `00-baseline.ppm`,
`{event}-{offset}ms.ppm`, plus `serial.log`. Default out dir is
`$TMPDIR/m3os-less-render-probe`.

### What the probe captured

The bug fires on most keystrokes. Per-event signature:

* **content briefly paints** — peak frame at 20 – 400 ms after the
  keystroke; non-black pixels spread across ~40 – 60 rows × ~370 –
  490 cols (real rendered content, not OVMF startup chrome).
* **wipe to all-black** — by 60 – 800 ms the surface drops to
  ~16 × 10 non-black pixels (residual OVMF chrome only; spread
  collapses into the top-left corner).

When a single frame *does* survive past 1500 ms, only the
*most-recently-painted* bytes survive — for less that's the bottom
status bar (rows 768 – 799 of an 800-row screen). Anything painted
earlier in the same repaint sequence is gone. That "only the last
paint survives" pattern is the most informative new fact and it
points strongly at an active producer/consumer race during the
repaint, not a passive timer wipe.

### Probes that were ruled out

**Probe A — disable the 500 ms blink tick.**
Set `Screen`'s default `cursor_shape` to `CursorShape::SteadyBlock`
(`userspace/term/src/screen.rs:360`) so `is_blinking()` returns
false from boot and the `term/src/main.rs:378` blink path is dead.
Two runs of `less-render-probe`:

- Run 1: less-opened still wiped (peak at 400 ms → black at 800 ms),
  `after-up` wiped at 60 ms, `after-down` retained content.
- Run 2: `after-down` instantly wiped (no peak captured), `after-up`
  retained content, less-opened still wiped.

The blink-tick hypothesis is wrong: the bug fires far faster than
500 ms, and disabling the tick does not change the rate.

**Probe B — `SeqCst` fence before `publish_frame`.**
Added `core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst)`
as the first line of `publish_frame` in `userspace/term/src/display.rs`
so every prior pixel store is globally ordered before the
`DamageSurface` IPC. If the bug were "store-visibility / LLVM
write-reorder," the fence would close the window. Two runs:

- Run 1: less-opened retained content, `after-down` wiped instantly,
  `after-up` wiped at 60 ms after a 20 ms peak.
- Run 2: less-opened wiped at 800 ms, `after-down` wiped instantly,
  `after-up` wiped at 60 ms.

No improvement over baseline. The LLVM/store-visibility hypothesis
is wrong (or at least not sufficient on its own).

### Where the bug actually lives — refined hypothesis

Both passive-wipe theories failed, so the remaining shape that fits
"only the last paint survives" is a **snapshot-during-write race**
in the SHM compose path:

1. Term enters `renderer.compose` (`userspace/term/src/render.rs:231`).
2. Drain queue: `fb.clear()` → writes `0x00` across all ~4 MiB of
   SHM. Then `put_glyph` × N writes the glyph cells. Then `submit()`
   sends `DamageSurface` + `CommitSurface` (publish step).
3. **In between** clear and the final puts, a frame tick in
   `display_server` fires and `run_compose` runs. `registry.has_damage()`
   is `true` because a *previous* commit set it. `pixels_snapshot`
   (`userspace/display_server/src/surface.rs:241`) does a raw-pointer
   memcpy of the surface; the snapshot lands somewhere between
   "all-cleared" and "fully painted." Display_server blits that
   torn snapshot to the framebuffer.
4. When term finishes painting (status bar last), the *next*
   display_server tick snapshots a coherent surface and blits real
   content — but only briefly, until the next paint cycle starts
   the race again.

The SeqCst fence in Probe B doesn't help because the race is not
about write-ordering; it's about display_server *reading* concurrently
with term writing. There is no happens-before edge between term's
pixel writes and display_server's snapshot read except the kernel
IPC system-call boundary, which is too coarse: the surface gets
`dirty=true` long before term has finished writing the next frame.

### Structural fix candidates

Both fix candidates eliminate the snapshot-during-write race:

1. **Double-buffering with atomic buffer-id swap.** Term allocates
   two SHM regions (front + back), writes pixels to the back buffer,
   and the `CommitSurface` verb names which buffer is now front.
   `display_server` only ever snapshots the buffer named on the most
   recent commit, so it can never see a half-written frame. Touches
   the protocol surface (`AttachSharedBuffer` would need to carry
   two buffer ids, or `CommitSurface` would need a buffer-id field).
   Closes the race definitively.

2. **Compose-lock IPC verb.** Term acquires a "publish lock" before
   starting a compose pass; display_server refuses to snapshot
   while the lock is held; term releases it after `submit`. Smaller
   protocol footprint but adds RTT latency to every frame and a
   potential deadlock surface (term crash with lock held).

Option 1 is what real wayland / sway-style compositors do and is
the principled close-out. It is multi-day work; the current
single-buffer SHM was a documented Phase 57d shortcut.

### Next-session diagnostic plan (revised)

Before committing to double-buffering, one cheap diagnostic step
confirms the race shape:

1. **Compose-timing trace.** Instrument both sides with
   `monotonic_micros()` brackets and log:
   - `term:compose-start <us>` at top of `Renderer::compose`
   - `term:compose-end <us>` after the final `fb.submit()`
   - `dispsrv:snapshot-start <sid> <us>` at line 381 of `compose.rs`
   - `dispsrv:snapshot-end <us>` after `pixels_snapshot()` returns
   Run `cargo xtask less-render-probe` and grep the `serial.log` for
   overlaps. If `dispsrv:snapshot-start` falls *between* a
   `term:compose-start` and the matching `term:compose-end` for the
   same surface — race confirmed.

2. If overlaps observed → **implement double-buffering**. Carrying
   the new buffer-id through `CommitSurface` is the lighter protocol
   change than re-attaching every frame.

3. If overlaps *not* observed → the race is somewhere else; capture
   the gap that doesn't fit and re-open the hypothesis search.

### 2026-05-17 (compose-timing trace ran) — race confirmed

Added `TC:compose-start` / `TC:compose-end` markers around
`renderer.compose()` in `userspace/term/src/main.rs:406` and
`DC:snap-start` / `DC:snap-end` markers around the two
`pixels_snapshot()` call sites in
`userspace/display_server/src/compose.rs` (fast path + general path).
Ran `cargo xtask less-render-probe` to capture
`/tmp/m3os-trace-probe/serial.log` and parsed it.

Numbers from one run (203 trace events):

| metric | value |
|---|---|
| term compose intervals captured | 55 |
| term compose duration (p50 / p90 / max) | 4.9 ms / 11 ms / 15 ms |
| display_server snapshot intervals captured | 45 |
| snapshot duration (p50 / p90 / max) | 1.6 ms / 1.9 ms / 5.5 ms |
| **snapshot intervals overlapping with a term compose interval** | **14 / 45 (31%)** |

Sample overlap (microsecond timestamps from the run):

```
snap [6112136..6113704] (1568 us)
  vs compose [6109377..6122117] (12740 us)
  snap-start - compose-start = +2759 us
```

`display_server`'s `pixels_snapshot` started **2.8 ms after term began
its compose pass** and completed before term's compose was done.
The snapshot was reading the SHM region while term was mid-`fb.clear`
/ `put_glyph` writes. This is exactly the snapshot-during-write race.

The race fires on roughly one third of all compose ticks under
the render-probe's workload. That matches the user's
"sometimes flashes" symptom and the probe's per-frame variance.

### 2026-05-17 (fix landed at 8497e0c) — partial repair, race remains

Implemented option 1 from the structural-fix candidates below plus a
single-renderer-side workaround for a secondary cause the trace
probe didn't surface:

1. **Double-buffered SHM publish** in `userspace/term/src/display.rs`.
   Two SHM regions + two `BufferId`s, alternating each frame. The
   kernel-core surface state machine's
   `pending_buffer → committed_buffer` move on `CommitSurface`
   (`userspace/display_server/src/surface.rs:677`) is now the atomic
   publication point: display_server's `pixels_snapshot` reads
   only `committed_buffer`, and term writes only to the *other*
   buffer until the next commit. A post-swap memcpy of the
   just-committed front into the new back preserves the published
   state for incremental ops (scroll, partial put).

2. **Clear-only defer in the renderer** in `userspace/term/src/render.rs`.
   Even with double-buffering, a `\E[2J` whose follow-up
   `\E[H<content>` lands in a later PTY chunk would queue
   `[Clear]` only and `Renderer::compose` would drain it,
   producing an all-zero back buffer that then propagates to both
   SHM buffers via `refresh_back_from_front`. The renderer now
   defers a Clear-only queue for up to `MAX_CLEAR_ONLY_DEFER = 8`
   composes (~128 ms grace period) waiting for follow-up `Put`
   ops, then drains anyway so a legit `clear` shell command isn't
   permanently deferred. Three new host tests cover the defer /
   drain / counter-reset transitions.

**Verification via `cargo xtask less-render-probe`**:

| event | pre-fix peak | pre-fix settled (1500 ms) | post-fix settled (1500 ms) |
|---|---|---|---|
| after-down | 0.0022 (26×377) | 0.0000 (16×10, all-black) | 0.0020 (62×357, **real less content**) — *most runs* |
| after-up | 0.0022 (26×377) | 0.0000 (16×10, all-black) | 0.0036 (48×389) then 0.0000 — *still racy* |

Two of three runs show `after-down` retaining content past
1500 ms. `after-up` still goes black ~80 – 200 ms after the
keystroke in most runs. The user-visible improvement is real but
the bug is not fully closed: less in TCG sometimes takes longer
than the 128 ms defer window to send follow-up content after a
`\E[2J`, and the renderer drains the deferred Clear into an
all-zero publish at that point.

Quality gates pass: `cargo xtask check`, `cargo xtask smoke-test`,
`cargo xtask tui-smoke`, `cargo xtask tui-app-smoke`.

### Open follow-ups for the next session

* **Investigate why `after-up` is more flaky than `after-down`.**
  The compose-timing trace from `0bdcd62` would reveal whether the
  remaining `after-up` flakes are still snapshot-during-write (i.e.
  the double-buffer didn't help that case) or a different
  Clear-only sequence (longer defer might fix it).
* **Consider extending `MAX_CLEAR_ONLY_DEFER`** beyond 8 frames
  (e.g., to 16 = 256 ms) if the next-session probe shows the bug
  fires beyond the current 128 ms window.
* **Or replace the count-based defer with an activity-based one**:
  defer until either a Put op arrives in the queue OR the PTY has
  been idle for N ms. The current count is a proxy for "more PTY
  bytes coming"; observing PTY idleness directly avoids the
  arbitrary cap.
* **Audit DA / DSR query responses.** If less is sending `\E[c`
  (DA) or `\E[6n` (DSR) and waiting on a timeout because
  m3os-term doesn't respond, the wait stretches the Clear-only
  window. Implementing the responses would shorten less's
  per-keystroke paint cycle and reduce defer pressure.

### Structural fix candidates (confirmed shape) — pre-fix snapshot

Both fix candidates eliminate the snapshot-during-write race:

1. **Double-buffering with atomic buffer-id swap (recommended).**
   Term allocates two SHM regions and two `BufferId`s. Each compose:
   * write pixels into the back buffer,
   * send `AttachSharedBuffer { surface_id, buffer_id = back, shm_id, ... }`
     so display_server installs the new buffer as `pending_buffer`,
   * send `DamageSurface(full)`,
   * send `CommitSurface`. The kernel-core surface state machine
     already moves `pending_buffer` → `committed_buffer` on commit;
     display_server's `pixels_snapshot` reads `committed_buffer` only.
   * swap roles so the next compose writes to the now-released front.
   The old `committed_buffer`'s SHM mapping has to stay alive at
   least until the *next* commit lands so an in-flight compose-pass
   snapshot doesn't dangle on a half-unmapped region — easiest fix
   is to drop only after the next commit lands (one frame of
   pinning).

2. **Compose-lock IPC verb.** Smaller protocol surface but adds
   round-trip latency and a deadlock surface (term crash mid-paint
   leaves the lock held). Not recommended.

Option 1 is what wayland and sway-style compositors do and is the
principled close-out. The single-buffer SHM was a documented
Phase 57d shortcut; this is the natural Phase 70 work.

## Next-session diagnostic plan (original, partially superseded)

1. **Reproduce on `diag/csi-completion-test`** with one more trace
   pass — instrument `display_server/src/compose.rs` with:
   - `CC:Clear { rect }` at every `clear_rect_to_background` call.
   - `CC:Blit { sid, rect }` at every `compose_frame` invocation
     (or, more precisely, every per-surface `local_damages` entry
     it iterates).
   - `CC:Submit` at every `owner.present()` call.
   Capture the trace and look at the sequence between successive
   term `submit` calls: any `CC:Clear` not followed by a matching
   `CC:Blit` over the same rect is the bug.

2. **Static check on the cursor-only fast path**: confirm that for
   a single full-output surface (term in `run-gui`), `compose_frame`
   with the cursor-rect damage list does in fact re-blit every pixel
   the cursor-trail clear just wiped.  If `compose_frame` drops the
   damage because the cursor doesn't actually overlap the surface
   bounds (off-by-one on `surface_screen_rect` vs `output` bounds),
   that would explain the wipe.

3. **Workaround if (2) is the issue**: gate the cursor-trail clear
   on `pointer is OUTSIDE every mapped surface`.  Inside a surface
   the surface-blit pass already overpaints; outside it we need the
   clear to remove the cursor's previous frame.

## Out of scope for this handoff but worth noting

- `cargo xtask tui-app-smoke` PASSES under all three branches — the
  smoke gate is *structurally* blind to a render disappearance
  because it only checks serial.  Fixing the gate is a separate
  follow-up (`SCREEN_DUMP` via QMP after each interactive step).
- `less` repaints fully on every event regardless of the new CSI
  dispatches.  Why `less` chose the full-repaint path at startup
  is not understood; the working theory is that `less` queries
  `\E[c` (DA, terminfo `u9`) or `\E[6n` (DSR, terminfo `u7`),
  expects a response that never comes back, and falls back to
  "dumb terminal" mode.  Validating that requires more trace
  bandwidth than I had time for; it is independent of the
  display_server wipe (`less` doing more repaints just makes the
  wipe more visible).

## Three branches to either land or discard

| Branch | Tip | Verdict |
|---|---|---|
| `diag/less-render-trace` | `0b176d7` | Strip the `TT:*` / `PI:*` / `EV:*` / `PO:*` traces; the TIOCSWINSZ-at-init change is a real fix. Open a clean PR. |
| `feat/term-csi-completion` | `3ddf90a` | Already clean. Open a PR. Real terminal-contract completion. |
| `diag/csi-completion-test` | `01441c2` | Discard after the next-session diagnostic pass — pure instrumentation. |
