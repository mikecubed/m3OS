# Phase 69 — Terminal TUI Capabilities: Task List

**Status:** Planned
**Source Ref:** phase-69
**Depends on:** Phase 22b (ANSI Escape) ✅, Phase 29 (PTY Subsystem) ✅, Phase 56 (Display and Input Architecture) ✅, Phase 57 (Audio and Local Session) ✅
**Goal:** Extend the Phase 57 `term` graphical terminal emulator with the terminal-contract features required for real TUI applications: a published `m3os-term` terminfo entry, alternate-screen buffer, 256-color and truecolor SGR, SIGWINCH propagation on resize, X10/SGR mouse reporting, cursor-shape sequences, and a validation pass against nvim, tmux, htop, less, and mc.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Terminfo entry: `m3os-term.ti` source, xtask compile + stage, `TERM` env set by session_manager | None | Planned |
| B | Alternate-screen buffer: dual cell-grid in `screen.rs`, `\x1b[?1049h/l` and `\x1b[?47h/l` parser arms | A | Planned |
| C | 256-color and truecolor SGR: extended parameter parsing, palette-to-BGRA8888 | A | Planned |
| D | SIGWINCH propagation: `term` resize event → `TIOCSWINSZ` → kernel SIGWINCH path | None | Planned |
| E | Mouse reporting: `mouse.rs`, X10 / button-event / SGR encoding, PointerEvent routing | B | Planned |
| F | Cursor styling: DECSCUSR state, cursor-render shape selection | B | Planned |
| G | Validation: nvim, tmux, htop, less, mc integration smoke | B, C, D, E, F | Planned |
| H | Documentation updates: Phase 22b, 29, 57 cross-refs; appendix escape-sequence reference | G | Planned |

---

## Track A — Terminfo Entry

### A.1 — Author `m3os-term.ti` terminfo source

**File:** `xtask/terminfo/m3os-term.ti`
**Symbol:** N/A (terminfo source file)
**Why it matters:** Without a published terminfo entry, applications that call `setupterm()` or `tigetstr()` cannot learn what escape sequences `term` supports and will fall back to `xterm` or `vt100` assumptions that may be wrong.

**Acceptance:**
- [ ] `m3os-term.ti` is present under `xtask/terminfo/`.
- [ ] Entry declares only Phase 57-era capabilities that `term` actually implements (colors 8, cursor movement, clear, bold/reverse/underline SGR).
- [ ] Entry will be extended in A.3 after Tracks B, C, E, F are implemented.

### A.2 — Compile and stage terminfo in xtask image build

**Files:**
- `xtask/src/main.rs`
- `xtask/terminfo/m3os-term.ti`

**Symbol:** `populate_ext2_files`
**Why it matters:** The terminfo binary must be present at `/usr/share/terminfo/m/m3os-term` inside the ext2 disk image before `setupterm()` can read it at runtime.

**Acceptance:**
- [ ] `xtask image` invokes `tic -o <staging-dir>/usr/share/terminfo xtask/terminfo/m3os-term.ti` (or equivalent compile step).
- [ ] `/usr/share/terminfo/m/m3os-term` is present in the built disk image.
- [ ] `cargo xtask check` passes after the xtask change.

### A.3 — Set `TERM=m3os-term` in `session_manager` before spawning user shell

**File:** `userspace/session_manager/src/boot.rs`
**Symbol:** `spawn_session`
**Why it matters:** Applications read `TERM` from the environment to select which terminfo entry to load; if it is unset or set to `xterm`, they send sequences `term` may not support.

**Acceptance:**
- [ ] `session_manager` sets `TERM=m3os-term` in the environment of every process it spawns in the graphical session.
- [ ] `echo $TERM` inside `term` reports `m3os-term`.

---

## Track B — Alternate-Screen Buffer

### B.1 — Dual cell-grid in `screen.rs`

**File:** `userspace/term/src/screen.rs`
**Symbol:** `ScreenState`, `switch_to_alt`, `switch_to_primary`
**Why it matters:** Without an alternate screen, full-screen TUI applications like nvim and htop overwrite shell scrollback, and restoring the previous display state on exit is impossible.

**Acceptance:**
- [ ] `ScreenState` holds a primary grid and an alternate grid; only one is active at a time.
- [ ] `switch_to_alt()` saves the primary cursor position and activates the alternate grid.
- [ ] `switch_to_primary()` restores the saved cursor position and activates the primary grid.
- [ ] The compositor path reads only the currently active grid.
- [ ] Unit tests in `kernel-core` or `userspace/term/tests/` cover: enter alt, write cells, exit alt, verify primary content unchanged.

### B.2 — ANSI parser arms for `\x1b[?1049h/l` and `\x1b[?47h/l`

**File:** `userspace/term/src/parser.rs`
**Symbol:** `handle_dec_private_mode`
**Why it matters:** These are the two historical alternate-screen sequences; nvim uses `?1049h/l` and older applications use `?47h/l`.

**Acceptance:**
- [ ] `\x1b[?1049h` calls `screen.switch_to_alt()` (with cursor save).
- [ ] `\x1b[?1049l` calls `screen.switch_to_primary()` (with cursor restore).
- [ ] `\x1b[?47h` calls `switch_to_alt()` without cursor save/restore.
- [ ] `\x1b[?47l` calls `switch_to_primary()` without cursor save/restore.
- [ ] Unrecognized private-mode codes are silently ignored (no parser crash).

---

## Track C — 256-Color and Truecolor SGR

### C.1 — Extend SGR parameter parser for 256-color and truecolor

**File:** `userspace/term/src/parser.rs` (and `kernel-core/src/ansi/sgr.rs` if the parser lives there)
**Symbol:** `parse_sgr_color`
**Why it matters:** Omarchy-class themes and nvim color schemes use 256-color indexed and 24-bit truecolor; the Phase 22b parser handles only 8 standard + 8 bright ANSI colors.

**Acceptance:**
- [ ] `\x1b[38;5;<n>m` sets foreground to the xterm 256-color palette index `n` (0–255).
- [ ] `\x1b[48;5;<n>m` sets background to palette index `n`.
- [ ] `\x1b[38;2;<r>;<g>;<b>m` sets foreground to the 24-bit RGB value.
- [ ] `\x1b[48;2;<r>;<g>;<b>m` sets background to the 24-bit RGB value.
- [ ] Unit tests in `kernel-core` cover: round-trip encode/decode for all four forms; boundary values (index 0, 255, r/g/b 0, 255).

### C.2 — Palette-to-BGRA8888 conversion

**File:** `userspace/term/src/render.rs`
**Symbol:** `color_to_bgra`
**Why it matters:** The Phase 57 glyph-render path produces BGRA8888 pixels; 256-color palette entries must be converted to the same format before blending.

**Acceptance:**
- [ ] 256-color palette (xterm standard) is stored as a `const` array of BGRA8888 values in `kernel-core`.
- [ ] `color_to_bgra` handles `Color::Indexed(n)`, `Color::Rgb(r, g, b)`, and the existing `Color::Ansi16(n)` variants.
- [ ] No per-pixel allocation; conversion is O(1).

---

## Track D — SIGWINCH Propagation

### D.1 — Wire `term` surface resize event to `TIOCSWINSZ`

**File:** `userspace/term/src/pty.rs`
**Symbol:** `handle_surface_resize`
**Why it matters:** Applications like nvim and tmux use SIGWINCH to reflow their layouts on resize; without this wiring the window can be resized visually but the application sees stale dimensions.

**Acceptance:**
- [ ] When `term` receives a `SurfaceResized { width, height }` event from the display server, it recalculates the cell grid dimensions (cols = width / glyph_w, rows = height / glyph_h).
- [ ] `term` calls `sys_ioctl(pty_slave_fd, TIOCSWINSZ, &new_winsize)` with the updated dimensions.
- [ ] The kernel tty layer's existing `TIOCSWINSZ` handler sends SIGWINCH to the foreground process group.
- [ ] `stty size` inside `term` after a resize reports the updated rows and columns.

### D.2 — Kernel SIGWINCH path audit

**File:** `kernel/src/tty/mod.rs`
**Symbol:** `tty_ioctl_tiocswinsz`
**Why it matters:** Phase 29 deferred SIGWINCH; the kernel path must be verified complete and not no-op before D.1 can rely on it.

**Acceptance:**
- [ ] `TIOCSWINSZ` handler updates `tty.winsize` and calls `send_signal_to_pgrp(tty.pgrp, SIGWINCH)`.
- [ ] A unit test or QEMU integration test verifies SIGWINCH is delivered after `TIOCSWINSZ`.

---

## Track E — Mouse Reporting

### E.1 — `mouse.rs` PointerEvent-to-PTY encoder

**File:** `userspace/term/src/mouse.rs` (new)
**Symbol:** `MouseReporter`, `encode_x10`, `encode_button`, `encode_sgr`
**Why it matters:** Mouse-aware TUIs (nvim, mc, lazygit) rely on click and motion events arriving in the PTY; without this routing the mouse is visible on screen but invisible to applications.

**Acceptance:**
- [ ] `MouseReporter` tracks the currently enabled reporting mode (None, X10, ButtonEvent, Sgr).
- [ ] Mode is updated by `enable_mode`/`disable_mode` calls from the parser when `\x1b[?9h`, `\x1b[?1000h`, `\x1b[?1006h` (and `l` variants) are received.
- [ ] On a `PointerEvent::Button` with a focused term surface, `encode_*` produces the correct byte sequence and writes it to the PTY master.
- [ ] Unit tests cover: X10 press encoding, SGR press + release encoding, mode transitions.

### E.2 — Wire `PointerEvent` from display server surface hook to `MouseReporter`

**File:** `userspace/term/src/main.rs`
**Symbol:** `handle_input_event`
**Why it matters:** Phase 56 routes `PointerEvent` to the focused surface's input hook; `term` must forward those events to `MouseReporter` rather than discarding them.

**Acceptance:**
- [ ] `PointerEvent` messages received on `term`'s input endpoint are forwarded to `mouse.rs` when mouse reporting is enabled.
- [ ] When reporting is disabled, `PointerEvent` messages are silently dropped (no PTY write).
- [ ] No `PointerEvent` processing when `term` is not the focused surface.

---

## Track F — Cursor Styling

### F.1 — DECSCUSR cursor-shape state

**File:** `userspace/term/src/parser.rs`
**Symbol:** `handle_decscusr`
**Why it matters:** Editors like nvim change cursor shape between normal and insert mode; without DECSCUSR the cursor stays as a fixed block regardless of mode.

**Acceptance:**
- [ ] `\x1b[ q` sequences 0–6 are parsed and stored in `TermState::cursor_shape`.
- [ ] Shape enum covers: BlinkingBlock (0/1), SteadyBlock (2), BlinkingUnderline (3), SteadyUnderline (4), BlinkingBar (5), SteadyBar (6).

### F.2 — Cursor-render shape selection

**File:** `userspace/term/src/render.rs`
**Symbol:** `render_cursor`
**Why it matters:** The visual cursor must match the shape the application requested.

**Acceptance:**
- [ ] `render_cursor` reads `TermState::cursor_shape` and draws the appropriate glyph: full-cell fill for block, bottom 2 rows for underline, left 2 columns for bar.
- [ ] Blinking variants toggle visibility at a fixed interval (500 ms on / 500 ms off) driven by a timer in the compose loop.

---

## Track G — Validation

### G.1 — nvim smoke

**Files:**
- `xtask/src/main.rs` (new `tui-smoke` gate or sub-test)
- `docs/research/post-phase-57 evaluation/04-tui-and-neovim-roadmap.md` (cross-ref)

**Symbol:** `cargo xtask tui-smoke`
**Why it matters:** nvim is the primary target application; a passing smoke test is the most concrete proof that Tracks B, C, D, and F all work together.

**Acceptance:**
- [ ] `nvim /tmp/test.txt` launches inside `term`, enters insert mode, inserts text, saves (`:w`), quits (`:q`), and returns to the shell prompt without PTY corruption.
- [ ] Syntax highlighting uses more than 8 colors (verified by inspecting rendered cells).
- [ ] Cursor changes shape on normal/insert mode transition.
- [ ] Resize while nvim is open causes nvim to reflow its layout.

### G.2 — tmux, htop, less, mc smoke

**File:** `xtask/src/main.rs`
**Symbol:** `cargo xtask tui-smoke`
**Why it matters:** Covers multiplexer, process-monitor, pager, and file-manager archetypes to validate the full Track B/C/D/E surface.

**Acceptance:**
- [ ] `tmux new-session` creates a session; `split-window` produces a visible pane split; `resize-pane` reflows content; `detach` exits cleanly.
- [ ] `htop` renders a full-color process list; resize causes htop to reflow.
- [ ] `less /etc/passwd` pages correctly; `q` returns to the shell with the primary screen restored.
- [ ] `mc` launches with its blue two-panel UI; mouse clicks (when mouse reporting enabled) navigate the panel.

---

## Track H — Documentation Updates

### H.1 — Cross-reference new capabilities in Phase 22b, Phase 29, and Phase 57 docs

**Files:**
- `docs/roadmap/22-tty-pty.md` (or `22b` variant)
- `docs/roadmap/29-pty-subsystem.md`
- `docs/roadmap/57-audio-and-local-session.md`

**Symbol:** N/A
**Why it matters:** Phase 22b and Phase 29 each deferred capabilities that Phase 69 implements; those docs must note the deferral was resolved.

**Acceptance:**
- [ ] Phase 22b doc notes that 256-color/truecolor SGR and mouse reporting were extended in Phase 69.
- [ ] Phase 29 doc notes that SIGWINCH propagation from display-server resize events was completed in Phase 69.
- [ ] Phase 57 doc notes that `term`'s terminal contract was extended in Phase 69.

### H.2 — Supported escape sequence reference in `docs/appendix/`

**File:** `docs/appendix/term-escape-sequences.md` (new)
**Symbol:** N/A
**Why it matters:** A canonical list of which sequences `term` supports prevents future phases from inadvertently regressing documented behavior.

**Acceptance:**
- [ ] File lists every escape sequence `term` implements after Phase 69, grouped by category (cursor movement, SGR, DEC private modes, OSC, mouse).
- [ ] File cross-references `m3os-term.ti` as the machine-readable source of truth.
- [ ] Deferred sequences (Kitty keyboard protocol, bracketed paste, sixel) are listed with a note.

---

## Documentation Notes

- The alternate-screen implementation replaces the fb-takeover Tier 2 trigger described in `docs/appendix/fb-takeover-tiers.md` § "Tier 2". Tier 2 proposed hijacking the alt-screen sequence for FB ownership; Phase 69 implements alt-screen as a proper terminal feature with no FB side effect. Update `fb-takeover-tiers.md` to note this resolution.
- The terminfo entry source lives in `xtask/terminfo/`, not in the ext2 data disk directly — the xtask build compiles it.
- Mouse reporting routing flows through the Phase 56 surface input hook, not through a new syscall.
- SIGWINCH relies on the kernel `TIOCSWINSZ` path that existed from Phase 29; Phase 69 adds only the `term`-side call site.
