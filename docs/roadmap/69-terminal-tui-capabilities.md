# Phase 69 - Terminal TUI Capabilities

**Status:** Planned
**Source Ref:** phase-69
**Depends on:** Phase 22b (ANSI Escape) ✅, Phase 29 (PTY Subsystem) ✅, Phase 56 (Display and Input Architecture) ✅, Phase 57 (Audio and Local Session) ✅
**Builds on:** Extends the Phase 57 `term` graphical terminal emulator with the terminal-contract features required for real TUI applications; extends Phase 56 focus-aware input dispatch to carry mouse events through the PTY master; extends Phase 29 SIGWINCH handling deferred at Phase 29 close
**Primary Components:** userspace/term, kernel-core/ansi, kernel/src/signal, kernel/src/tty, docs/appendix/terminfo

## Milestone Goal

TUI applications — nvim, tmux, htop, less, midnight commander — run correctly inside the Phase 57 `term` emulator. The terminal publishes a `m3os-term` terminfo entry, supports alternate-screen, 256-color and truecolor SGR, mouse reporting, SIGWINCH on resize, and cursor-shape sequences. The Phase 57 `term` is promoted from demo-class to daily-driver-class.

Following TDD discipline, the `kernel-core::ansi` ANSI parser is host-tested first — 256-color round-trip and alternate-screen state transitions are verified with `cargo test -p kernel-core` before the `term` integration is written. Applying SRP, each ANSI escape category (SGR color, DEC private modes, mouse reporting, DECSCUSR) lives in its own parser submodule so changes to one category cannot regress another. Applying DRY, 256-color indexed and 24-bit truecolor SGR share a single `color_to_bgra` resolver, and alt-screen enter/exit share a `buffer_swap` helper rather than duplicating state management in both `?1049` and `?47` code paths.

## Why This Phase Exists

The Phase 57 `term` emulator was built to prove the local-session concept, not to run arbitrary third-party TUI software. Per the post-Phase-57 evaluation in `docs/research/post-phase-57 evaluation/04-tui-and-neovim-roadmap.md` and audit finding B8, several mandatory terminal features were left unimplemented: alternate-screen buffer (required by every full-screen editor), 256-color and truecolor SGR (required by Omarchy-class themes), SIGWINCH propagation (required by tmux and nvim resize), mouse reporting (required by mouse-aware TUIs), and a published terminfo entry (required by any application that calls `setupterm()`). Without these, TUI applications either refuse to start, render incorrectly, or hang on termcap negotiation.

This phase exists to close the gap between "a terminal that can run the built-in shell" and "a terminal that can run real developer tools."

## Learning Goals

- Understand how terminfo entries describe terminal capabilities and how applications query them.
- Learn how alternate-screen buffer state is separate from the scrollback buffer state and why editors require it.
- See how 256-color and truecolor SGR parameters extend the base 16-color ANSI model.
- Understand how SIGWINCH is generated and propagated through the kernel to a foreground process group.
- Learn how X10 and SGR mouse-reporting modes translate display-server pointer events into PTY byte streams.
- See how cursor shape sequences interact with the compositor's cursor-render path.

## Feature Scope

### Terminfo entry (Track A)

Publish `m3os-term` as a compiled terminfo entry installed at
`/usr/share/terminfo/m/m3os-term`. The entry declares exactly what the Phase 69
`term` emulator implements — no aspirational capabilities. `TERM=m3os-term` is
set in the user environment by `session_manager` before spawning the user shell.
Applications that call `setupterm()` or `tigetstr()` get accurate capability data
rather than falling back to `xterm` or `vt100` and sending sequences `term` may
not handle.

### Alternate-screen buffer (Track B)

Implement `\x1b[?1049h` (save cursor, switch to alternate screen) and `\x1b[?1049l`
(restore cursor, switch back to primary screen) in the term ANSI parser. The older
`\x1b[?47h` / `\x1b[?47l` forms (alternate screen without cursor save/restore) are
handled as aliases. The alternate screen is a second cell grid, independently
scrollable; entering it does not destroy primary-screen scrollback. Exiting restores
the primary screen's cursor position and content exactly.

This is semantically distinct from the Tier 2 fb-takeover trigger described in
`docs/appendix/fb-takeover-tiers.md` § "Tier 2" — that Tier used the alt-screen
sequence as a takeover signal. Phase 69 implements alt-screen as a proper terminal
feature with no framebuffer-ownership side effect.

### 256-color and truecolor SGR (Track C)

Extend the ANSI SGR parser to handle the extended color parameter forms:
`\x1b[38;5;<n>m` and `\x1b[48;5;<n>m` (256-color indexed foreground/background)
and `\x1b[38;2;<r>;<g>;<b>m` / `\x1b[48;2;<r>;<g>;<b>m` (24-bit truecolor). The
Phase 22b parser handles only the 8 standard and 8 bright colors. The extended
forms require the parser to consume additional parameter fields after a `38` or `48`
prefix. Palette-to-BGRA8888 conversion feeds the existing Phase 57 glyph-render
path.

### SIGWINCH propagation (Track D)

When the display server reports that a `term` surface has been resized, `term` must
update its row/column state, commit the new `TIOCSWINSZ` to the PTY slave via the
kernel tty layer, and send SIGWINCH to the foreground process group. Today only
manual `TIOCSWINSZ` calls are supported; SIGWINCH is generated by the tty layer
but not wired from the display-server resize event through to the PTY. This track
closes the Phase 29 SIGWINCH deferral.

### Mouse reporting (Track E)

Implement X10 mode (`\x1b[?9h`), button-event mode (`\x1b[?1000h`), and SGR mouse
encoding (`\x1b[?1006h`). Phase 56 `PointerEvent` messages arrive at `term`'s
surface input hook. When mouse reporting is enabled, `term` encodes each event in
the active mode and writes the byte sequence to the PTY master, which the
application reads as ordinary input. Disable reporting on `\x1b[?9l`,
`\x1b[?1000l`, `\x1b[?1006l`.

### Cursor styling (Track F)

Implement `\x1b[ q` (DECSCUSR) for cursor shape: 0/1 blinking block, 2 steady
block, 3 blinking underline, 4 steady underline, 5 blinking bar, 6 steady bar.
`term`'s cursor-render module honors the current shape at each compose frame.

### Validation (Track G)

Full nvim session (open file, insert, save, quit), full tmux session (new session,
split pane, resize, detach), htop (full-color process list, resize), less (paging,
quit), mc (Midnight Commander, alt-screen enter/exit). These are the target
applications from `docs/research/post-phase-57 evaluation/04-tui-and-neovim-roadmap.md`.

### Documentation updates (Track H)

Phase 22b, Phase 29, and Phase 57 design docs updated to cross-reference Phase 69
capabilities. Terminfo entry and supported-escape-sequence list added to
`docs/appendix/`.

## Important Components and How They Work

### `userspace/term/src/parser.rs`

The ANSI escape sequence parser introduced in Phase 57, reusing `kernel-core::ansi`.
After Phase 69: handles `\x1b[?1049h/l`, `\x1b[?47h/l`, extended SGR (38;5, 48;5,
38;2, 48;2), `\x1b[?9h/l`, `\x1b[?1000h/l`, `\x1b[?1006h/l`, and `\x1b[ q`
cursor-shape commands. Mode state (alternate screen active, mouse reporting mode,
cursor shape) is tracked in a `TermState` struct that the renderer reads.

### `userspace/term/src/screen.rs`

The cell-grid renderer. After Phase 69: maintains two grids — primary and alternate.
`switch_to_alt()` copies cursor position to saved state and activates the alternate
grid. `switch_to_primary()` restores saved state and activates the primary grid. The
compositor path reads whichever grid is currently active.

### `userspace/term/src/mouse.rs` (new)

Translates Phase 56 `PointerEvent` values into PTY byte sequences. Tracks enabled
reporting mode and the active encoding (X10, button, SGR). Writes output to the PTY
master via the existing `pty.write_master()` helper.

### `kernel/src/tty/mod.rs` — SIGWINCH path

`TIOCSWINSZ` ioctl processing updates `tty.winsize` and calls `send_signal_to_pgrp(tty.pgrp, SIGWINCH)`. After Phase 69, `term` calls `sys_ioctl(TIOCSWINSZ)` on the PTY slave fd in response to a display-server resize event, triggering this path. The kernel side already supports `send_signal_to_pgrp`; the missing piece is the `term`-side call site.

### Terminfo entry

Compiled terminfo database for `m3os-term` staged into the ext2 data disk at
`/usr/share/terminfo/m/m3os-term`. The source file lives in
`xtask/terminfo/m3os-term.ti` and is compiled during the xtask image build. Entry
declares exactly the escape sequences Phase 69 `term` implements.

## How This Builds on Earlier Phases

- Extends Phase 22b's ANSI parser (in `kernel-core::ansi`) with 256-color/truecolor SGR parameter forms and new private-mode DEC sequences.
- Closes the Phase 29 SIGWINCH deferral by wiring a resize event from `term`'s surface callback through `TIOCSWINSZ` to the kernel tty signal path.
- Extends Phase 56's focus-aware `PointerEvent` dispatch to flow into `term`'s mouse-reporting encoder rather than being dropped at the surface boundary.
- Extends Phase 57 `term`'s single cell-grid model with a dual-grid alternate-screen implementation.

## Implementation Outline

1. Write `m3os-term.ti` terminfo source for current Phase 57 capabilities only; compile and stage.
2. Implement alternate-screen buffer in `screen.rs`; add `\x1b[?1049h/l` and `\x1b[?47h/l` parser arms.
3. Extend SGR parser for 256-color (38;5/48;5) and truecolor (38;2/48;2); update palette-to-BGRA8888 conversion.
4. Wire display-server resize event to `sys_ioctl(TIOCSWINSZ)` call in `term`; verify kernel SIGWINCH path fires.
5. Implement mouse-reporting mode tracking; add `mouse.rs` PointerEvent-to-PTY encoder for X10, button, SGR modes.
6. Implement DECSCUSR cursor-shape state; update cursor-render module to honor shape at each frame.
7. Update terminfo entry to include all newly implemented capabilities.
8. Validation pass: nvim, tmux, htop, less, mc.
9. Update Phase 22b, Phase 29, Phase 57 design docs; add supported-escape-sequence reference to `docs/appendix/`.

## Acceptance Criteria

- `TERM=m3os-term` is set by `session_manager`; `infocmp m3os-term` returns the installed entry without error.
- `nvim /tmp/test.txt` opens, renders with 256-color syntax highlighting, saves, and quits without corrupting the primary screen.
- `tmux new-session` creates a session, `split-window` produces a visible split, `resize-pane` reflows content, `detach` exits cleanly.
- `htop` renders a full-color process list and reflows on terminal resize.
- Mouse click in an `nvim` window positions the cursor at the clicked cell.
- SIGWINCH is received by the foreground process after `term`'s surface is resized; `stty size` reports the updated dimensions.

## Companion Task List

- [Phase 69 Task List](./tasks/69-terminal-tui-capabilities-tasks.md)

## How Real OS Implementations Differ

- Linux VTE and xterm-compatible terminals ship with upstream terminfo entries maintained by the ncurses project; m3OS must maintain its own.
- Wayland compositors pass resize events through `xdg_toplevel::configure`; m3OS uses its own typed `SurfaceResized` message on the Phase 56 control socket.
- Linux delivers SIGWINCH through the kernel TTY layer; m3OS follows the same model but the resize event originates from a userspace compositor rather than a VT switch.
- Production terminals implement the full XTGETTCAP / Kitty keyboard protocol; m3OS defers those to a later phase.

## Deferred Until Later

- Kitty keyboard protocol and Kitty graphics protocol
- Sixel graphics rendering inside `term`
- Bracketed paste mode (`\x1b[?2004h`)
- Terminal scrollback selection and clipboard integration
- Configurable fonts and font scaling
- IME / input method support
- Motion events in mouse-reporting mode (tracks 1002, 1003)
