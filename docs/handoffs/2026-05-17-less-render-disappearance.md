---
status: open
branch: feat/phase-69d-tui-app-foundation (PR #176) — investigation surface
last-known-good-commit: adb9527
date: 2026-05-17
component: userspace/term + kernel-core/fb + userspace/display_server (compose)
related:
  - docs/roadmap/69d-tui-app-foundation.md
  - docs/handoffs/2026-05-16-phase-69d-100-percent-followups.md
diag-branches:
  - diag/less-render-trace          # TT:* / PI:* / EV:* / PO:* traces + TIOCSWINSZ-at-init fix
  - feat/term-csi-completion         # csr / vpa / su/sd / il / dl / ich / dch / ech dispatches
  - diag/csi-completion-test         # both stacked
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

## Next-session diagnostic plan

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
