# Fullscreen-takeover tiers

**Status:** Tier 1 landed (Phase 57d follow-up). Tiers 2 and 3 deferred.

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
