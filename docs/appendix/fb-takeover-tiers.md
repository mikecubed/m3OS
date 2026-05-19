# Fullscreen-takeover tiers

**Status:** Tier 1 retained as fallback for non-`display_server` boots
(headless / serial-only mode). Tier 3 landed in Phase 70 — DOOM is now
a regular `display_server` client via `display_client_ffi` and no
longer requires the fb-takeover wrapper. The second-takeover-hang and
mouse-pointer-reset residuals are resolved structurally by Tier 3
(there is no longer a takeover, so there is nothing to reclaim).
Tier 2 remains deferred.

**Branch:** `feat/57d-voluntary-preemption` — head at `8619928`.

**Source ref:** Discussion during the 57d-graphical-boot-debugging session,
prompted by doom hanging silently because `display_server` owned the
framebuffer while doom tried to draw to it.

This document captures the three tiers we considered for letting a
legacy fullscreen program (e.g. doom) take over the screen while
`display_server` is running, the trade-offs of each, and what was built
in Tier 1.

## Problem statement

A program written against the kernel's `sys_framebuffer_*` syscalls
expects exclusive ownership of the framebuffer pages. When
`display_server` is the framebuffer owner, the program's
`sys_framebuffer_acquire` call returns `EBUSY`, the program either
crashes or runs blind, and the user sees nothing happen — the screen
keeps showing the compositor's last frame.

Real graphical sessions on Linux solve this with a per-protocol mode
(VT switching, Wayland's session-lock, Xorg's DRM master). m3OS doesn't
need the full mechanism for the toy boot, but it does need *some*
protocol so doom can run.

## Tier 1 — Wrapper-driven yield/reclaim (LANDED)

**Mechanism:** A userspace wrapper (`/bin/fb-takeover`) sends explicit
`YieldFb` / `ReclaimFb` verbs on the existing `display-control`
socket. `display_server` drops framebuffer ownership via a new
`SYS_FB_YIELD` syscall, pauses its compose loop, then reclaims via
`SYS_FB_REACQUIRE` and marks every surface dirty so the next compose
pass redraws the screen.

**Components landed:**
- Kernel: `SYS_FB_YIELD` (`0x101C`) and `SYS_FB_REACQUIRE` (`0x101D`)
  in `kernel/src/arch/x86_64/syscall/mod.rs`.
- syscall-lib wrappers: `syscall_lib::fb_yield()` and
  `syscall_lib::fb_reacquire()`.
- Protocol opcodes: `OP_CTL_YIELD_FB` (`0x020B`) and
  `OP_CTL_RECLAIM_FB` (`0x020C`) in
  `kernel-core/src/display/protocol.rs`, with matching
  `ControlCommand::YieldFb` / `ControlCommand::ReclaimFb` enum
  variants and encode / decode arms.
- `display_server` interception: `handle_fb_yield_request` /
  `handle_fb_reclaim_request` peek the decoded command before the
  generic `serve_control_iter` path so the syscall side effects and
  compose-loop gate don't leak into the pure-logic dispatcher.
- `SurfaceRegistry::mark_all_dirty` companion to `mark_clean` — the
  reclaim path forces a full repaint because cached damage state is
  meaningless after another process drew over the framebuffer.
- Wrapper: `userspace/fb-takeover/` — service-lookup with backoff,
  `YieldFb`, `fork()` + `execve()`, `waitpid()`, `ReclaimFb`.

**Usage:** `fb-takeover /bin/doom -warp 1 1` (or any other fullscreen
program that talks to the kernel `sys_framebuffer_*` API).

**Pros:**
- Minimal moving parts; the wrapper owns the handshake exclusively.
- The takeover program is unmodified — doom does not need to know
  anything about `display_server`.
- Atomic yield → run → reclaim sequence: a crash in the wrapper
  itself still runs the reclaim (because the kernel exit-cleanup path
  clears `FB_OWNER_PID` when the takeover child exits, and the
  wrapper's reclaim is best-effort even if `waitpid` fails).
- Reuses the existing `display-control` socket; no new IPC service.

**Cons / trade-offs:**
- The whole compositor pauses for the duration. Any layer-shell
  surfaces (status bars, lockscreens) freeze on screen instead of
  hiding cleanly.
- The screen flashes between the compositor's last frame, doom's
  output, and the recomposed compositor view. No fade or animation.
- No input multiplexing: while doom is running, kbd/mouse events
  still flow through `display_server`'s dispatcher and are
  effectively dropped. Doom polls the keyboard via the same
  `kbd_server` IPC service it always has.
- A misbehaving takeover program that calls
  `sys_framebuffer_acquire` *without* the wrapper still races
  `display_server`'s compose loop. The wrapper is the only sanctioned
  path; manual `m3ctl yield-fb` is intentionally not exposed.

## Tier 2 — Term-aware fullscreen handoff (DEFERRED)

**Mechanism:** `term` (the graphical terminal emulator that owns the
PTY) recognises a fullscreen-mode escape sequence (e.g.
`ESC[?1049h` — the alt-screen DECSET), or a custom OSC sequence, and
performs the yield → run → reclaim handshake itself. The user types
`doom` at the shell, the shell `exec`s doom inside the PTY, doom
emits the alt-screen sequence as part of its terminal init, `term`
sees the sequence and yields the framebuffer, and doom's subsequent
`sys_framebuffer_acquire` succeeds. On doom exit, the PTY EOF or a
matching `ESC[?1049l` triggers reclaim.

**Components needed:**
- ANSI parser extension in `term` to recognise the
  takeover-trigger sequence (alt-screen or a custom OSC).
- Hand-off state machine in `term`'s main loop: pause the screen-
  buffer write path while in takeover mode; resume on reclaim.
- Either the alt-screen sequence as the trigger (simple but
  ambiguous: alt-screen is a normal terminal feature), or a new
  m3OS-specific OSC (`ESC]52;m3os-fb-takeover\x07` or similar).
- Coordination with the shell: `term` needs to know whether the
  child has exited so it can reclaim. PTY EOF is a cleaner signal
  than a teardown OSC.
- Surface lifecycle on `term`'s side: while yielded, the toplevel
  surface is implicitly hidden; on reclaim, mark its buffer dirty
  and re-commit so the compositor repaints.

**Pros:**
- No wrapper required. `doom` just runs in the terminal.
- Shell pipelines work: `cat /etc/doom-args | xargs doom` no longer
  needs the wrapper.
- The handshake is invisible to the program — doom thinks it's
  running on a real fullscreen terminal.

**Cons / trade-offs:**
- Couples `term`'s ANSI parser to the framebuffer-ownership
  protocol. Two unrelated state machines now share state.
- Conflict with the alt-screen feature: if `term` later wants to
  implement alt-screen as a normal text-mode feature (so `vim`
  doesn't pollute the scrollback), the takeover trigger must be a
  different sequence.
- Programs that are not run from `term` (e.g. launched directly by
  `init` or `session_manager` for kiosk-style boots) still need the
  wrapper.
- The PTY-EOF reclaim path doesn't fire if doom crashes hard or
  forks a daemon child — `term` would need a watchdog timeout.

**When to revisit:** When we have more than one fullscreen program
and the wrapper invocation becomes ergonomically painful, or when
`term` already needs alt-screen for `vim` so the parser-extension
cost is amortised across both features.

## Tier 3 — Doom as a `display_server` client (DEFERRED)

**Mechanism:** Doom is rewritten (via a `dg_m3os.c` rewrite) to
allocate a Phase 56 `Toplevel` surface, render its frames into the
shared-memory buffer, and submit damage rects through the SHM
transport. `display_server` composes doom's surface alongside
everything else; there is no framebuffer handoff.

**Components needed:**
- Rewrite `userspace/doom/dg_m3os.c` to use the surface-buffer
  protocol (`SurfaceCreate` → `BufferCreate` → `Commit` →
  `DamageBuffer`).
- Doom-side support for the m3OS pixel format / surface size
  semantics (today doom expects to write directly into the kernel
  framebuffer's `bgra8888` linear scanline).
- Input wiring: doom currently polls keyboard events on its own;
  Phase 56 routes input through the focus-aware dispatcher. Doom
  becomes a regular focusable client.
- Audio still goes through `audio_client` — already independent
  from the framebuffer.

**Pros:**
- "Correct" architecturally — every userspace program is a
  display-server client. No mode switching, no flicker.
- Doom's window can be moved, resized, and overlapped with other
  surfaces. Status bars and the cursor stay visible.
- Multiple instances of doom can run simultaneously.
- Removes `SYS_FB_YIELD` / `SYS_FB_REACQUIRE` from the kernel ABI
  surface (purely internal compositor concerns).

**Cons / trade-offs:**
- Significant rewrite of doom's rendering layer.
- The surface-buffer protocol is the same one we're still
  hardening for `term`; bugs in the SHM bring-up affect doom too.
- For a toy OS demo, this is overkill — doom-on-the-framebuffer is
  the iconic outcome we're chasing in the first place.
- Doesn't help any other legacy fullscreen program; each one needs
  the same rewrite.

**When to revisit:** When the SHM transport has been stable for at
least one full phase, and when we want a long-term home for
graphical programs that doesn't involve mode switching. Useful as a
forcing function to find protocol bugs that single-client `term` +
`gfx-demo` don't exercise.

## Known Tier 1 reclaim residuals

**Resolution (Phase 70):** Both residuals below — the
second-consecutive-takeover hang and the mouse-pointer-reset on
reclaim — are now structurally inapplicable. Phase 70 turned DOOM
into a `display_server` client via `display_client_ffi`, eliminating
the yield/reclaim cycle that was the root cause of both bugs. The
prose below is preserved as a historical post-mortem of the Tier 1
implementation.

Tier 1 ships fixes for the most-visible reclaim issues, but two
input-state-machine corners are tracked here for follow-up:

### Stuck Enter (and other modifier keys) after reclaim — fixed

When the user types `fb-takeover doom` and presses Enter to invoke,
the Enter press routes to TTY (`kbd_server` sees it, schedules a
repeat). When `display_server` yields, scancodes route to the RAW
buffer; the eventual Enter release goes to the takeover program (or
nowhere) instead of `kbd_server`. After reclaim, `kbd_server`'s
tracker still believes Enter is held; the repeat scheduler emits
ENTER repeats forever.

**Fix landed:** the kernel's `try_yield_console`, when transferring
the console to a non-raw owner (i.e. `raw_input_enabled = false`),
injects break codes for Enter / Shift / Ctrl / Alt / Space into the
TTY scancode buffer via `inject_release_all_held_modifiers`.
`kbd_server` processes them as ordinary key releases and clears the
held state. The injection is wrapped in `without_interrupts` so it
isn't racy against the keyboard ISR (regression caught in
`c643ab1`).

The set of injected break codes is hand-curated for the common
stuck-key cases. If a future use-case surfaces a held letter or
function key, the list grows; doing the full 102-key sweep was
deemed overkill.

**Empirical quirk — keep the boot-time fire:** `try_yield_console`
is called twice in a normal session — once at boot when
`display_server` does its initial `sys_fb_acquire`, and again on
every `sys_fb_reacquire` after a takeover. The boot-time call is
*semantically* a no-op (no keys held, `kbd_server`'s tracker is
empty), but commit `8619928` established that removing the
boot-time inject — even with the inject still firing on every
reclaim — regresses the *first* `fb-takeover doom` invocation in
the session: doom mmaps the FB, prints its banner, then wedges in
`BlockedOnReply` on its next IPC. Restoring the boot-time fire
makes the first invocation work again. The actual upstream cause
of why exercising the kbd_server → display_server input pipeline
at boot matters is unresolved; investigation lead below under
"Second consecutive `fb-takeover doom` hangs".

### Second consecutive `fb-takeover doom` hangs — open

The first `fb-takeover doom` invocation in a session works end-to-end
(framebuffer mmap, WAD load, gameplay, exit, reclaim). A second
invocation in the same session hangs: doom (the new pid) maps the FB
successfully, prints its DG_Init / GPL banner over the PTY, then stays
in `BlockedOnReply` indefinitely on its next kernel-internal IPC.
The watchdog's 30-second "no waker registered" warning fires
repeatedly (200+ times in some runs) with the same `stuck-since`
counter incrementing monotonically — i.e. doom is genuinely wedged
on a single call, not just slow.

The disk subsystem is alive at the time:
- In some runs a `virtio-blk completion poll + queue notify after
  request timeout owner_pid=11 type=0 sector=N completed=true`
  warning indicates the reply request did finish (the polling path
  caught a missed IRQ) but the requester chain didn't propagate the
  wake.
- In other runs (the most recent test against `8619928`) there are
  *no* virtio-blk timeouts — the hang is purely IPC-wake propagation,
  not disk I/O.

The same boot-time-inject quirk noted above (removing the boot-time
inject regresses the *first* takeover) almost certainly shares an
upstream cause with this second-takeover hang: both are about
something in the kbd_server → display_server input pipeline that
needs to have been "exercised" before doom can complete its first
non-FB IPC, and the once-only exercise from boot doesn't carry over
to a second takeover in the same session.

Hypotheses to investigate (in priority order):
- The reclaim path doesn't re-arm whatever the boot-time path arms.
  Compare the post-`sys_fb_reacquire` state of `kbd_server`'s
  endpoint, `display_server`'s bind tables, and any cross-process
  notification subscriptions against the post-boot state. A
  diff that's empty at boot but non-empty after the first reclaim
  is the smoking gun.
- Stale `Reply` capability or pending-message slot left over from
  the first session that displaces the second session's reply
  delivery. doom's exit cleanup may not be tearing down its
  endpoint state cleanly enough that the next takeover starts with
  a clean inbox.
- virtio-blk waiter slot or `ACTIVE_REQUEST_*` state not fully
  cleared between sessions, causing the post-completion wake to
  target the wrong (now-gone) task. Ruled less likely by the
  no-virtio-blk-timeout run, but waiter-slot leaks could still
  affect the wake-propagation chain via vfs_server.
- Task-ID recycling across sessions interacting with cached IPC
  state somewhere (display_server's bind tables, vfs_server's
  open-handle map, etc.).
- doom-side: doom's per-process state (NCURSES tty mode, signal
  handlers from the previous run's exit path) somehow persists in a
  shared resource. Less likely — doom is fork+exec'd as a fresh
  process each time — but worth ruling out by capturing the second
  doom's `/proc`-equivalent state at the hang point.

Suggested next investigation step: instrument the kernel scheduler
to dump every `BlockedOnReply` task's `pending_msg`, its endpoint
binding, and the corresponding endpoint's wait queue when the
watchdog fires. The wedge is on a *specific* IPC reply that never
lands; identifying which endpoint and which expected reply is the
fastest path to the root cause.

Workaround: reboot between doom sessions. The first run still
works.

### Mouse pointer resets to top-left after reclaim — open

After reclaim, moving the mouse repaints the screen but the cursor
position keeps resetting to the top-left corner. The compositor's
`pointer_position` accumulator likely gets reset (or the mouse-event
stream emits an absolute-zero event) somewhere in the FB-ownership
transition path. Needs investigation. Workaround: keep moving the
mouse until the surface repaints; the cursor will track normally on
the next event chain.

Hypothesis: PS/2 mouse decoder is delta-only, so `pointer_position`
is summed in userspace. During yield no events arrive at
`mouse_server` (events route somewhere else), and on reclaim the
first event's delta lands at whatever `pointer_position` happens to
be. If something else clears `pointer_position` to `(0, 0)` on
reclaim (cursor renderer reset?), every fresh delta starts from
zero.

## Decision log

- **2026-05-02:** Tier 1 selected for immediate implementation.
  Rationale: smallest landed surface, doom runs unmodified, the
  wrapper is a 200-line crate, no rework when Tiers 2 or 3 land
  (they replace the wrapper, they don't conflict with it).
- **2026-05-02:** Tier 2 deferred. Reason: `term`'s ANSI parser is
  still being shaken out (Phase 57d cursor-rendering and bottom-row
  glyph fixes); piling on a second state machine before the parser
  is stable is asking for trouble.
- **2026-05-02:** Tier 3 deferred. Reason: out of scope for the
  current debug session; the goal was "make doom work today", not
  "make doom architecturally correct".
- **2026-05-03:** Boot-time `inject_release_all_held_modifiers`
  retained as a workaround. Removing it (commit `c643ab1`) regressed
  the first-takeover path; restoring it (commit `8619928`) fixes
  first-takeover but does not address the second-takeover hang.
  The injection is treated as load-bearing until the upstream
  IPC-wake-propagation cause is identified.

## Commit log (current branch)

| Commit | Subject | Notes |
|---|---|---|
| `43fa68e` | feat(display): Tier 1 fullscreen takeover via yield/reclaim FB | Original Tier 1 landing. |
| `1445c04` | fix(doom): exit with clear error when framebuffer unavailable | Replaces the silent doom-runs-blind failure mode. |
| `6423e9e` | fix(input): stop dropping 0xAA — it's the LSHIFT break code | Decoder no longer eats LSHIFT releases. |
| `4a3cddd` | fix(fb-takeover): resolve relative names + route diagnostics to serial | `/bin/` prefix + serial_print so yield-time diagnostics survive. |
| `6ebd713` | fix(fb-takeover): full background fill + clear stuck modifiers on reclaim | First version of the modifier-release inject. |
| `58b5a70` | fix(display): tolerate stale-tail bulks + surveil the IPC desync | Loosened protocol decoder: `consumed > bulk.len()` instead of `!=`. |
| `c643ab1` | fix(ipc): revert deliver_bulk warning + harden modifier-release inject | Reverts a `log::warn!` while holding `scheduler_lock` that starved IPC; wraps the inject in `without_interrupts`. **Also moved the inject out of `try_yield_console`'s boot path — this is the regression `8619928` undoes.** |
| `0968c0c` | fix(ipc): drop false-positive bulk_mismatch warnings; doc 2nd-doom hang | Removes `log_bulk_mismatch` (false positives on VFS_READ where `data[1]=offset`). |
| `8619928` | fix(fb): restore boot-time inject so first doom takeover works again | Current head. Restores the working `6ebd713` pattern of inject inside `try_yield_console` while keeping the `c643ab1` IRQ-safety wrapping. |
