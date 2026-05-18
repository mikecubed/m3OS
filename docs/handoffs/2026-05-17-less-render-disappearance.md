---
status: closed
branch: feat/phase-69d-tui-app-foundation (PR #176) — fix landed
last-known-good-commit: 7e654a4  # NUL-byte / control-char filter in parser
fix-commits:
  - 8497e0c  # double-buffer SHM + Clear-only defer (race scaffolding, partial)
  - d899f73  # always refresh back-from-front (closes regression from 8497e0c)
  - 0d0940f  # DA / DSR responses (lets less query the terminal properly)
  - 8c9bba7  # CSI completion (csr / vpa / su/sd / il/dl / ich/dch/ech) — cherry-picked from feat/term-csi-completion
  - 7e654a4  # NUL + spec-defined control bytes ignored (THE actual root cause)
date: 2026-05-17 / 2026-05-18 closeout
component: kernel-core/fb (parser) — root cause; userspace/term, userspace/display_server (scaffolding)
related:
  - docs/roadmap/69d-tui-app-foundation.md
  - docs/handoffs/2026-05-16-phase-69d-100-percent-followups.md
diag-branches:
  - diag/less-render-trace          # TT:* / PI:* / EV:* / PO:* traces + TIOCSWINSZ-at-init fix
  - feat/term-csi-completion         # csr / vpa / su/sd / il / dl / ich / dch / ech dispatches  (merged via cherry-pick)
  - diag/csi-completion-test         # both stacked
ruled-out-hypotheses:
  - cursor-trail clear in compose.rs (EV:Ptr = 0; symptom is black not teal)
  - 500 ms blink-tick wipe (Probe A: cursor default → SteadyBlock; bug still fires within 60 ms)
  - LLVM / store-visibility race (Probe B: SeqCst fence before publish_frame; bug still fires)
  - DECSTBM/csr not implemented (added 8c9bba7; less is not actually using csr)
  - snapshot-during-write race (real bug, fixed by 8497e0c; not the residual cause)
new-tooling:
  - cargo xtask less-render-probe  # QMP send-key + screendump; captures the bug pixel-state
---

## 2026-05-18 closeout — root cause was NUL-byte padding fill

**TL;DR.** The residual flake from `d899f73` was caused by `AnsiParser::process_normal`
treating NUL (0x00) — and every other ECMA-48 "ignored when standalone" control
byte — as `PutChar(0)`. Less sends NUL bytes as terminfo padding fill (default
`pc` = 0), and each NUL walked the cursor across the freshly-painted screen,
painting a blank cell at every position. With ~hundreds of padding NULs after
every `\E[2J\E[H<content>` header, the entire screen was systematically
overwritten with blanks before the next user keystroke.

The fix at `7e654a4` adds a single match arm to `process_normal` mapping the
spec-defined no-op control bytes (NUL plus SOH..ACK, SO/SI, DLE..ETB, CAN/SUB,
FS..US, DEL) to `ConsoleCmd::Nop`. Three host tests pin the new behaviour.

### How the trace probes found it

The compose-outcome probe (added in `394a512`, reverted in `bd7e8db` after the
fix landed) showed composes with `clears=0 puts=256 scrolls=2 submitted=1`
publishing all-zero buffers. The `TC:put-extent` probe narrowed the puts to
`rows=23..24 cols=0..79` (status-bar area). Decoding the PTY-byte trace
(`PI:<hex>`) showed less emitting `\E[2J\E[H<content>` followed by many `<00>`
bytes — the padding fill.

Without the parser fix, every NUL became a `PutChar(0) → put_glyph(...,
codepoint=0, ...)` that filled a cell with `bg` (default 0 = black) and
advanced the cursor. Less's emit-many-NULs-as-padding pattern therefore
walked the cursor across all 25 rows × 80 cols of the screen, blanking
every cell.

### Companion improvements landed alongside the root-cause fix

The hypothesis chain was wrong twice before finding NUL, but the failed
hypotheses produced two independently-useful improvements:

* **DA / DSR responses** (`0d0940f`). `\E[c` (Primary DA) now answers
  `\E[?6c` (VT102); `\E[5n` answers `\E[0n`; `\E[6n` answers
  `\E[<row>;<col>R`. These are terminal-contract obligations the
  m3os-term terminfo already advertised (`u8` / `u9` / `u7`); without
  them, some TUI apps fall back to "dumb terminal" full-repaint mode.
  Did not visibly move the flake but a real correctness gap.

* **CSI completion** (`8c9bba7`, cherry-picked from
  `feat/term-csi-completion`). DECSTBM (`csr`), VPA (`d`), SU (`S`) /
  SD (`T`), IL (`L`) / DL (`M`), ICH (`@`) / DCH (`P`), ECH (`X`) are
  now dispatched through full-fidelity screen state changes (scroll
  region, region-aware line_feed, insert/delete lines and chars).
  Less in this scenario does not actually emit any of these — it
  uses `\E[H\E[J<content>` with NUL padding — but every other TUI
  app will benefit, and the parser was already advertising these
  capabilities in terminfo.

### Verification

`cargo xtask less-render-probe` across 3 runs after `7e654a4`:

| event | run 1 | run 2 | run 3 |
|---|---|---|---|
| less-opened (1500 ms) | 0.0036 ✓ | 0.0036 ✓ | 0.0036 ✓ |
| after-down (1500 ms) | 0.0022 ✓ | 0.0022 ✓ | 0.0042 ✓ |
| after-up (1500 ms) | 0.0056 ✓ | 0.0057 ✓ | 0.0036 ✓ |

All 9 events show visible content at 1500 ms (vs the d899f73 baseline of
"3 of 4 runs show ALL three events settling"). Quality gates green at
`bd7e8db`: `cargo xtask check`, `cargo xtask tui-smoke`,
`cargo xtask tui-app-smoke`.

### Branches to dispose

| Branch | Disposition |
|---|---|
| `feat/term-csi-completion` | Cherry-picked at `8c9bba7`. Branch can be deleted. |
| `diag/csi-completion-test` | Discard — pure instrumentation, all useful changes folded into main fix. |
| `diag/less-render-trace` | Discard — TIOCSWINSZ-at-init fix not yet land; the diff is small enough that a fresh PR is the right way to merge it (terminfo claims 25 rows; kernel default is 24). |

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

### 2026-05-18 (always-refresh follow-up) — bug mostly closed

Re-ran the compose-timing trace from the original
`0bdcd62`-style probe instrumentation under the post-`8497e0c`
double-buffer fix, plus added a new `DC:snap-nonzero sid=N nz=X/Y`
trace counting non-zero bytes in each `pixels_snapshot` result.
The trace data immediately exposed the actual residual bug:

| metric | value |
|---|---|
| total composes | 70 |
| composes that deferred a Clear-only queue | **0** |
| composes that force-drained a Clear-only queue | **0** |
| display_server snapshots | 76 |
| snapshots showing `nz=0` (committed buffer is all-zero) | **33 of 76 (43%)** |

The `Clear-only defer` mechanism added in `8497e0c` was never
exercised — the after-up flake had nothing to do with that path. The
all-zero snapshots came from a different bug **introduced by that
same commit**: `DisplayClient::submit` skipped the front-to-back
memcpy on the first publish, leaving the new back at SHM-create
zeros. The very next compose drained its `[Put×N]`-only queue
(no Clear) into that zero buffer, so the published frame only had
the cells explicitly re-painted this frame — every cell from the
first frame dropped to black. Subsequent refreshes propagated this
zero-then-partial state forward.

**Fix landed at `d899f73`:** always run `refresh_back_from_front`
on every submit, including the first. Costs an extra ~4 MiB memcpy
per frame (~250 MiB/s at 60 Hz) — well within budget on QEMU TCG.

**Verification across 4 render-probe runs**: 3 of 4 runs show ALL
three events settling with content visible at 1500 ms (drop =
+0.0000). One run still has both keystrokes drop to black, so the
dominant symptom (every keypress wipes the screen) is gone but a
residual flake remains on a separate path.

Quality gates green at `d899f73`: `cargo xtask check`,
`cargo xtask tui-smoke`, `cargo xtask tui-app-smoke`.

### Host test coverage for the FramebufferOwner contract

The `DisplayClient` impl itself is not host-testable (it uses real
`syscall_lib::shm_*` and `ipc_call_buf`), but the *contract* it must
uphold is. `userspace/term/src/render.rs::tests::StatefulFakeFb` is
a single-buffer mock of that contract — it tracks an 8×4 cell grid
that the renderer paints into, simulates the same `clear` /
`put_glyph` / `scroll` mutations as a real surface, and records a
snapshot on every `submit`. Three tests drive the renderer through
real less-style compose cycles:

* `published_frames_accumulate_state_across_incremental_composes`:
  the bug shape that 8497e0c → d899f73 fixed. First compose drains
  `[Clear, Put×N]`, subsequent composes drain `[Put×M]` only. Asserts
  every published snapshot contains every cell that was ever painted,
  not just the cells touched in the most recent compose. Catches any
  future regression to "incremental ops must paint on top of the
  previously-published frame".
* `scroll_operates_on_previously_published_content`: shell-newline
  pattern — `[Scroll, Put]` between Clear-painted frames. Verifies
  scroll shifts the *real* rows up and the Put paints the new bottom
  row.
* `blink_only_submit_republishes_existing_buffer_unchanged`: the
  pre-d899f73 user-visible symptom — when only `mark_damaged` fires
  on an empty queue, submit must re-publish the existing buffer
  unchanged. Catches any regression that zeroes the buffer on submit.

These tests don't drive `DisplayClient` directly — that still
requires the in-QEMU `cargo xtask less-render-probe` integration
gate — but they pin the renderer-side semantics and serve as a
reference for any future FramebufferOwner implementation.

### Remaining residual flake — for the next investigator

A small fraction of probe runs still show after-down / after-up
dropping to black at 1500 ms. The diagnostic instrumentation that
caught the always-refresh bug needs to be re-armed to capture one of
those failing runs:

1. **Reinstate the trace probes.** The shape is well-contained:
   `TC:compose-start` / `TC:compose-end` brackets around
   `renderer.compose()` in `userspace/term/src/main.rs`;
   `DC:snap-start` / `DC:snap-end` + `DC:snap-nonzero` around the
   two `pixels_snapshot` call sites in
   `userspace/display_server/src/compose.rs`; and a `ComposeOutcome`
   getter on `Renderer` reporting `clears` / `puts` / `scrolls` /
   `deferred` / `force_drained` / `drained` flags. Both blocks have
   landed and been reverted twice in this investigation; the
   commit history at `d899f73~` carries the diff.
2. **Run the render-probe until a fail reproduces** and inspect the
   serial log around the failure. If `snap-nonzero` reaches zero
   while no compose has a Clear, the same class of bug as `d899f73`
   is back — look for a code path that leaves a buffer at zero
   without going through `refresh_back_from_front`.
3. **If Clear-only IS the signature**, bump `MAX_CLEAR_ONLY_DEFER`
   from 8 to 16, or switch to a PTY-idleness-gated defer (the
   count-based heuristic is fragile under TCG-slow PTY transfers).

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
