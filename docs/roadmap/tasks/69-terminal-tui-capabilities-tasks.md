# Phase 69 — Terminal Contract Foundations: Task List

**Status:** Complete
**Source Ref:** phase-69
**Depends on:** Phase 22b (ANSI Escape) ✅, Phase 29 (PTY Subsystem) ✅, Phase 56 (Display and Input Architecture) ✅, Phase 57 (Audio and Local Session) ✅, Phase 68 (Display Server Closeout) ✅
**Goal:** Land the terminal-contract foundation in `term` so the wire protocol is ready for real TUI applications: a published `m3os-term` terminfo entry, alternate-screen buffer, 256-color and truecolor SGR, SIGWINCH propagation on resize, X10/SGR mouse reporting, DECSCUSR cursor styling, bracketed paste, and a hand-rolled `tui-smoke` byte-level validator. Application-level validation against real apps moves to Phase 69d after termios (69a), UTF-8 + glyphs (69b), and font infrastructure (69c) land.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Terminfo entry: `m3os-term.ti` source, xtask compile + stage, `ENV_TERM` rename in `init` + sister sites | None | Complete |
| B | Alternate-screen buffer: dual cell-grid in `Screen`, `DecPrivateMode` variant in `ConsoleCmd`, `?1049` and `?47` arms | A | Complete |
| C | 256-color and truecolor SGR: extended parameter parsing in `kernel-core::fb`, shared `color_to_bgra` resolver | A | Complete |
| D | SIGWINCH propagation: `SurfaceResized` PulledEvent variant, `Screen::resize`, `ioctl(TIOCSWINSZ)` call site | None | Complete |
| E | Mouse reporting: `mouse.rs` X10 / button / SGR encoder, `Pointer` PulledEvent variant, parser arms | B | Complete |
| F | Cursor styling: DECSCUSR state, cursor-render shape selection, blink-tick in event loop | B | Complete |
| G | Bracketed paste: `?2004` mode bit, write-wrap helper | B | Complete |
| H | Validation: `userspace/tui-smoke` binary + `cargo xtask tui-smoke` gate | B, C, D, E, F, G | Complete |
| I | Documentation: Phase 22b / 29 / 57 cross-refs; appendix escape-sequence reference; kernel version bump to 0.69.0 | H | Complete |

---

## Track A — Terminfo Entry

### A.1 — Author `m3os-term.ti` terminfo source

**File:** `xtask/terminfo/m3os-term.ti`
**Symbol:** N/A (terminfo source file)
**Why it matters:** Without a published terminfo entry, applications that call `setupterm()` or `tigetstr()` cannot learn what escape sequences `term` supports and will fall back to `xterm` or `vt100` assumptions that may be wrong.

**Acceptance:**
- [x] `m3os-term.ti` is present under `xtask/terminfo/`.
- [x] Entry declares only Phase 57-era capabilities `term` currently implements (8-color SGR 30–37 / 40–47, basic cursor movement, ED, EL, bold/reverse/underline).
- [x] Entry will be extended in A.4 after Tracks B, C, E, F, G are implemented.

### A.2 — Compile and stage terminfo in the xtask image build

**Files:**
- `xtask/src/main.rs`
- `xtask/terminfo/m3os-term.ti`

**Symbol:** `populate_ext2_files`
**Why it matters:** The terminfo binary must be present at `/usr/share/terminfo/m/m3os-term` inside the ext2 disk image before `setupterm()` can read it at runtime.

**Acceptance:**
- [x] `xtask image` invokes `tic -o <staging-dir>/usr/share/terminfo xtask/terminfo/m3os-term.ti`.
- [x] xtask returns a clear, actionable error if `tic` is not on `PATH` (build host requirement; documented in `AGENTS.md`).
- [x] `/usr/share/terminfo/m/m3os-term` is present in the built data disk.
- [x] `cargo xtask check` passes after the xtask change.

### A.3 — Rename `ENV_TERM` to `m3os-term` across the four call sites

**Files:**
- `userspace/init/src/main.rs`
- `userspace/login/src/main.rs`
- `userspace/shell/src/main.rs`
- `userspace/pty-test/src/main.rs`

**Symbol:** `ENV_TERM` (and inline `b"TERM=m3os\0"` literals)
**Why it matters:** Applications read `TERM` from the environment to select which terminfo entry to load; if it is `m3os` (the existing literal) the new `m3os-term` entry is never opened.

**Acceptance:**
- [x] `userspace/init/src/main.rs:77` `ENV_TERM = b"TERM=m3os-term\0"`.
- [x] `userspace/login/src/main.rs:132` `env_term = b"TERM=m3os-term\0"`.
- [x] `userspace/shell/src/main.rs:419` `env_term = b"TERM=m3os-term\0"`.
- [x] `userspace/pty-test/src/main.rs:95` `env_term = b"TERM=xterm\0"` — leave unchanged; this is an upstream-compat fixture.
- [x] A new `userspace/tui-smoke` binary verifies `getenv("TERM") == "m3os-term"` (covered by Track H).

### A.4 — Re-publish the terminfo entry with the Phase 69 capability set

**File:** `xtask/terminfo/m3os-term.ti`
**Symbol:** N/A
**Why it matters:** After Tracks B–G land, the entry must describe what `term` actually supports.

**Acceptance:**
- [x] Entry declares 256 colors (`colors#256`), `setaf`/`setab` for indexed and truecolor SGR (`38;5;`, `38;2;`, etc.).
- [x] Entry declares `smcup`/`rmcup` for alt-screen (`\x1b[?1049h/l`).
- [x] Entry declares `kmous` / `XM` for mouse-1006 reporting.
- [x] Entry declares the six `Ss` cursor shapes (DECSCUSR 0–6).
- [x] Entry declares `BE`/`BD` for bracketed paste.

---

## Track B — Alternate-Screen Buffer

### B.1 — Extend `ConsoleCmd` with `DecPrivateMode`

**File:** `kernel-core/src/fb.rs`
**Symbol:** `ConsoleCmd`, `AnsiParser::process_csi`
**Why it matters:** The Phase 22b parser handles standard CSI sequences but discards `?`-prefixed DEC private modes. Every Phase 69 mode (alt-screen, mouse, bracketed paste) is a `?` mode.

**Acceptance:**
- [x] `ConsoleCmd` gains `DecPrivateMode { codes: [u16; MAX_PARAMS], count: usize, set: bool }` (multi-code form widened in PR 168 round-2 to support terminfo patterns like `?1006;1000h`); a `ConsoleCmd::dec_private_single` helper wraps the single-code construction path used by tests and synthesizers.
- [x] `AnsiParser` recognizes `\x1b[?<n>h` / `\x1b[?<n>l` *and* the multi-parameter form `\x1b[?<n1>;<n2>;...h/l`, and emits a single `DecPrivateMode` whose `codes[..count]` carries every parsed parameter.
- [x] Unrecognized codes are preserved in `codes[..count]` and dropped silently by callers (no parser crash).
- [x] Host tests in `kernel-core` cover: `?1049h`, `?1049l`, `?47h`, `?47l`, `?9h`, `?1000h`, `?1006h`, `?2004h`, `?2004l`, one bogus code, plus `test_dec_private_multi_param` for `?1006;1000h` and `?1000;1006;2004h`.

### B.2 — Dual cell-grid in `Screen`

**File:** `userspace/term/src/screen.rs`
**Symbol:** `Screen`, `switch_to_alt`, `switch_to_primary`, `ScreenSelect`
**Why it matters:** Without an alternate screen, full-screen TUI applications overwrite shell scrollback and cannot restore the prior display state on exit.

**Acceptance:**
- [x] `Screen` holds a primary `Vec<Cell>` and an alternate `Vec<Cell>`; only one is "active" at a time per `ScreenSelect`.
- [x] `switch_to_alt()` saves the primary cursor position + active colours into a `SavedCursor` field and activates the alternate grid (cleared on first entry).
- [x] `switch_to_primary()` restores the saved cursor + colours and activates the primary grid.
- [x] `Screen::feed` routes all writes to whichever grid is active.
- [x] Host tests cover: enter alt, write cells, exit alt, verify primary content unchanged; nested alt-enter is a no-op; exit when not in alt is a no-op.

### B.3 — Wire `?1049` and `?47` arms in `Screen::feed`

**File:** `userspace/term/src/screen.rs`
**Symbol:** `Screen::feed` (`DecPrivateMode` match arm)
**Why it matters:** The parser emits `DecPrivateMode`; the screen state machine must act on it.

**Acceptance:**
- [x] `DecPrivateMode { code: 1049, set: true }` calls `switch_to_alt()` (with cursor save).
- [x] `DecPrivateMode { code: 1049, set: false }` calls `switch_to_primary()` (with cursor restore).
- [x] `DecPrivateMode { code: 47, set: true|false }` aliases without cursor save/restore.
- [x] Unrecognized codes are forwarded to `MouseReporter` (Track E) and to the bracketed-paste handler (Track G); anything still unrecognized is silently ignored.

---

## Track C — 256-Color and Truecolor SGR

### C.1 — Extend SGR parameter parser

**File:** `kernel-core/src/fb.rs`
**Symbol:** `SgrParams`, `AnsiParser::parse_sgr`
**Why it matters:** Omarchy-class themes and nvim color schemes use 256-color indexed and 24-bit truecolor; the Phase 22b parser handles only 8 standard ANSI colors and ignores any parameter beyond `0..=47`.

**Acceptance:**
- [x] `SgrParams` gains `IndexedFg(u8)`, `IndexedBg(u8)`, `RgbFg(u8,u8,u8)`, `RgbBg(u8,u8,u8)` variants (or an equivalent flattened representation in the existing `params` buffer plus a typed accessor — implementer's call).
- [x] `\x1b[38;5;<n>m` produces `IndexedFg(n)`; `\x1b[48;5;<n>m` produces `IndexedBg(n)`.
- [x] `\x1b[38;2;<r>;<g>;<b>m` produces `RgbFg(r,g,b)`; `\x1b[48;2;<r>;<g>;<b>m` produces `RgbBg(r,g,b)`.
- [x] Sequences combining a recognised extended-color SGR with other SGR codes in the same `m` (e.g. `\x1b[1;38;5;208;4m`) are parsed correctly — the extended-color subgroup consumes exactly its parameters.
- [x] Host tests cover: round-trip parse for all four forms; boundary values (index 0, 255, r/g/b 0, 255); mixed SGR (`\x1b[1;38;5;208m`).

### C.2 — Palette-to-BGRA8888 conversion

**File:** `userspace/term/src/screen.rs` (palette) + `userspace/term/src/render.rs` (resolver)
**Symbol:** `color_to_bgra`, `XTERM_256_PALETTE`
**Why it matters:** The Phase 57 glyph-render path produces BGRA8888 pixels; 256-color palette entries and truecolor RGB must be converted to the same format before blending.

**Acceptance:**
- [x] `XTERM_256_PALETTE: [u32; 256]` (standard xterm 6×6×6 cube + 24 greyscale ramp + 16 base colors) is stored as a `const` in `screen.rs` (or `kernel-core::fb` if shared with the kernel framebuffer console).
- [x] `color_to_bgra` handles `Color::Indexed(n)`, `Color::Rgb(r, g, b)`, and the existing 8-color SGR path.
- [x] No per-pixel allocation; conversion is O(1).
- [x] `Screen::apply_sgr` consumes the new `SgrParams` extended-color variants and updates `self.fg` / `self.bg` via `color_to_bgra`.

---

## Track D — SIGWINCH Propagation

### D.1 — `SurfaceResized` PulledEvent variant

**File:** `userspace/term/src/main.rs`
**Symbol:** `PulledEvent`, `pull_one_event`
**Why it matters:** Phase 56 emits a `SurfaceResized` `ServerMessage` that `term` currently drops silently; without routing it, the cell grid stays at boot-time geometry forever.

**Acceptance:**
- [x] `PulledEvent` enum gains `SurfaceResized { width: u32, height: u32 }`.
- [x] `pull_one_event` decodes `ServerMessage::SurfaceResized` and emits the new variant.
- [x] The main loop's match handles the variant: compute `cols = width / glyph_w`, `rows = height / glyph_h`, call `Screen::resize(cols, rows)`, then `ioctl(TIOCSWINSZ)` (D.2).

### D.2 — `Screen::resize` + `ioctl(TIOCSWINSZ)` call site

**Files:**
- `userspace/term/src/screen.rs`
- `userspace/term/src/main.rs`

**Symbol:** `Screen::resize`, `handle_surface_resize`
**Why it matters:** The kernel TTY layer's `TIOCSWINSZ` handler (already complete from Phase 29 at `kernel/src/arch/x86_64/syscall/mod.rs:11398`) updates `tty.winsize` and sends SIGWINCH to the foreground process group — but only if userspace calls it.

**Acceptance:**
- [x] `Screen::resize(cols, rows)` reallocates both the primary and alternate grids, clamps the cursor to the new bounds, and re-emits a full-redraw damage hint.
- [x] `handle_surface_resize` calls `syscall_lib::ioctl(slave_fd, TIOCSWINSZ, &Winsize { ws_row: rows, ws_col: cols, … })`.
- [x] `tui-smoke resize` (Track H) verifies a self-installed SIGWINCH handler runs after the resize call.
- [x] `stty size` inside `term` after a resize reports the updated rows and columns (covered by `tui-smoke`).

### D.3 — Kernel SIGWINCH path verification

**File:** `kernel/src/arch/x86_64/syscall/mod.rs`
**Symbol:** TIOCSWINSZ branch at line 11398
**Why it matters:** Phase 29 deferred SIGWINCH; the kernel path was wired in a later phase. This task is a verification-only audit to confirm it still fires end-to-end and to add the missing user-end integration test.

**Acceptance:**
- [x] No kernel-side changes required (handler already calls `send_signal_to_group(fg, SIGWINCH)`).
- [x] A QEMU integration test (or the `tui-smoke resize` flow in Track H) observes a SIGWINCH signal received by a userspace process after `TIOCSWINSZ`.
- [x] A short note in `docs/roadmap/29-pty-subsystem.md` flips the SIGWINCH deferral line to `(closed in Phase 69)`.

---

## Track E — Mouse Reporting

### E.1 — `mouse.rs` PointerEvent-to-PTY encoder

**File:** `userspace/term/src/mouse.rs` (new)
**Symbol:** `MouseReporter`, `Mode`, `encode_x10`, `encode_button`, `encode_sgr`
**Why it matters:** Mouse-aware TUIs (nvim, mc, lazygit) rely on click events arriving on the PTY; without this routing the mouse is visible on screen but invisible to applications.

**Acceptance:**
- [x] `MouseReporter::new()` starts in `Mode::Disabled`.
- [x] `enable(mode)` / `disable()` are driven by parser `DecPrivateMode` arms for `?9` (X10), `?1000` (button-event), `?1006` (SGR).
- [x] `encode(event, cols, rows)` returns `Option<heapless::Vec<u8, 16>>` (or equivalent stack-bounded buffer) — `None` when disabled.
- [x] SGR encoding matches xterm: press at `(col=10, row=5)` for left button produces `\x1b[<0;11;6M`; release produces `\x1b[<0;11;6m`.
- [x] X10 encoding uses the legacy 6-byte form: `\x1b[M Cb Cx Cy` with the standard `+32` offset.
- [x] Coordinates outside the grid are clamped to `(1, 1)..=(cols, rows)`.
- [x] Host tests cover: X10 press, button-event press + release, SGR press + release, mode transitions, disabled state returns `None`.

### E.2 — Wire `Pointer` PulledEvent variant + parser arms

**File:** `userspace/term/src/main.rs`
**Symbol:** `PulledEvent::Pointer`, `pull_one_event`, main-loop match
**Why it matters:** The current code drops `ServerMessage::Pointer` silently at line 510; the new variant must thread through to `MouseReporter`.

**Acceptance:**
- [x] `PulledEvent` enum gains `Pointer(PointerEvent)`.
- [x] `pull_one_event` decodes `ServerMessage::Pointer` into the new variant.
- [x] The main-loop match routes `Pointer` into `MouseReporter::encode`; on `Some(bytes)`, writes via `syscall_lib::write(primary_fd, …)`.
- [x] When reporting is disabled, `MouseReporter::encode` returns `None` and the main loop drops the event silently.
- [x] No `Pointer` processing happens when `term` is not the focused surface (the display server already filters; this is a defence-in-depth assert).

---

## Track F — Cursor Styling

### F.1 — DECSCUSR parser + state

**Files:**
- `kernel-core/src/fb.rs` (parser)
- `userspace/term/src/screen.rs` (state)

**Symbol:** `ConsoleCmd::CursorShape`, `Screen::cursor_shape`
**Why it matters:** Editors like nvim change cursor shape between normal and insert mode; without DECSCUSR the cursor stays as a fixed block regardless of mode.

**Acceptance:**
- [x] `AnsiParser` recognizes `\x1b[<n> q` for `n` ∈ 0..=6 and emits `ConsoleCmd::CursorShape { shape: n }`.
- [x] `Screen` carries a `CursorShape` enum: `BlinkingBlock` (0/1), `SteadyBlock` (2), `BlinkingUnderline` (3), `SteadyUnderline` (4), `BlinkingBar` (5), `SteadyBar` (6).
- [x] Default cursor shape is `BlinkingBlock` to match xterm.
- [x] Host tests cover: each of the seven valid codes; an out-of-range code is ignored (no parser crash, no state change).

### F.2 — Cursor render + blink tick

**Files:**
- `userspace/term/src/render.rs`
- `userspace/term/src/main.rs`

**Symbol:** `Renderer::render_cursor`, `BlinkTick`
**Why it matters:** The visual cursor must match the shape the application requested; blinking variants must visibly blink even when the PTY is idle.

**Acceptance:**
- [~] `render_cursor` reads `Screen::cursor_shape()` and draws the appropriate glyph: full-cell inverted fill for block, bottom 2 rows for underline, left 2 columns for bar. **Partial — Phase 69 lands `Screen::cursor_shape` state, the DECSCUSR parser, and the blink-tick `mark_damaged()` call, but the actual cursor *pixel* draw (block / underline / bar fill on the framebuffer) is deferred to a follow-up phase. `RenderCommand::MoveCursor` currently routes to a documented no-op; `tui-smoke cursor` asserts shape-state transitions but not pixel output.**
- [x] When the current shape is a blinking variant, `main.rs` synthesizes damage every 500 ms via a `last_blink_ms` field and a forced `renderer.mark_damaged()` call; the compose throttle (`COMPOSE_INTERVAL_MS`) still applies so the upload is at most one frame per 16 ms.
- [x] Steady shapes leave the existing damage-driven path untouched (no idle compose).

---

## Track G — Bracketed Paste

### G.1 — `?2004` mode bit + write-wrap helper

**Files:**
- `userspace/term/src/screen.rs` (state)
- `userspace/term/src/input.rs` (helper)

**Symbol:** `Screen::bracketed_paste_enabled`, `wrap_paste`
**Why it matters:** Editors use bracketed paste to distinguish typed input from pasted input — without it, a multi-line paste into vim triggers per-line autoindent and mangles the content.

**Acceptance:**
- [x] `DecPrivateMode { code: 2004, set: true|false }` toggles `Screen::bracketed_paste_enabled`.
- [x] `wrap_paste(bytes) -> heapless::Vec` (or fallback `Vec`) returns `\x1b[200~ <bytes> \x1b[201~` when enabled; returns the raw bytes when disabled.
- [x] No automatic paste source is wired in Phase 69 (clipboard support is later); the helper is callable by future code paths and is exercised by `tui-smoke paste`.
- [x] Host tests cover: enable + disable transitions; wrap with empty payload; wrap with a payload containing the close sequence as data (no special escaping required by the protocol — assert the byte stream is exactly `start + payload + end`).

---

## Track H — Validation

### H.1 — `userspace/tui-smoke` binary

**Files:**
- `userspace/tui-smoke/Cargo.toml` (new)
- `userspace/tui-smoke/src/main.rs` (new)
- `Cargo.toml` (workspace member)
- `xtask/src/main.rs` (`build_userspace_bins` bins list)
- `kernel/src/fs/ramdisk.rs` (`include_bytes!` + `BIN_ENTRIES`)

**Symbol:** `program_main`
**Why it matters:** Phase 69 acceptance is byte-level. `tui-smoke` is the single binary the gate runs to confirm each escape-sequence path is wired end-to-end without depending on any ported third-party application.

**Acceptance:**
- [x] Binary follows the four-place pipeline (workspace member, xtask `bins` array with `needs_alloc = true`, ramdisk `BIN_ENTRIES` entry, no service-conf needed since it is not a daemon).
- [x] Subcommands: `alt-screen`, `colors`, `mouse`, `cursor`, `resize`, `paste`. Each prints `TUI_SMOKE:<name>:ok` on success and `TUI_SMOKE:<name>:fail <reason>` on failure, exiting with the matching status.
- [x] Each subcommand asserts on observable state (cell snapshot, recorded `(fg,bg)`, recorded cursor shape, PTY echo bytes, `getenv("TERM")`, `stty size`-equivalent ioctl, SIGWINCH handler counter) — not just "no crash."
- [x] Binary uses `syscall_lib::heap::BrkAllocator` per the four-place rule.

### H.2 — `cargo xtask tui-smoke` gate

**File:** `xtask/src/main.rs`
**Symbol:** `tui_smoke` subcommand
**Why it matters:** A reproducible CI gate is the load-bearing acceptance signal for the phase; without it, regressions creep back.

**Acceptance:**
- [x] `cargo xtask tui-smoke` boots the kernel under QEMU, waits for the `TERM_SMOKE:ready` sentinel, then drives `tui-smoke <subcmd>` for each of the six subcommands via the existing PTY-driver shape used by `smoke-test`.
- [x] The gate asserts all six subcommands print `TUI_SMOKE:<name>:ok` and exit zero.
- [x] Total runtime under 90 s on a developer laptop; the gate is added to the pre-push hook behind `M3OS_TUI_REGRESSION=1` (matching the existing optional-regression-gate pattern).

---

## Track I — Documentation and Release

### I.1 — Cross-reference new capabilities in Phase 22b, 29, 57 docs

**Files:**
- `docs/roadmap/22b-ansi-parser-enhancement.md`
- `docs/roadmap/29-pty-subsystem.md`
- `docs/roadmap/57-audio-and-local-session.md`

**Symbol:** N/A
**Why it matters:** Phase 22b and Phase 29 each deferred capabilities that Phase 69 implements; those docs must note the deferral was resolved.

**Acceptance:**
- [x] Phase 22b doc notes that DEC private modes, 256-color/truecolor SGR, and DECSCUSR were added in Phase 69.
- [x] Phase 29 doc's `Deferred Until Later` line for SIGWINCH (line 123) is updated to `(closed in Phase 69)`.
- [x] Phase 57 doc notes that `term`'s terminal contract was extended in Phase 69 and that termios/UTF-8/Nerd Font are scoped to 69a/b/c.

### I.2 — Supported escape sequence reference in `docs/appendix/`

**File:** `docs/appendix/term-escape-sequences.md` (new)
**Symbol:** N/A
**Why it matters:** A canonical list of which sequences `term` supports prevents future phases from inadvertently regressing documented behavior.

**Acceptance:**
- [x] File lists every escape sequence `term` implements after Phase 69, grouped by category (cursor movement, SGR, DEC private modes, OSC, mouse, bracketed paste).
- [x] File cross-references `m3os-term.ti` as the machine-readable source of truth.
- [x] Deferred sequences (Kitty keyboard protocol, sixel, motion mouse modes 1002/1003) are listed with a note pointing to the relevant deferral phase.

### I.3 — Kernel version bump to 0.69.0

**Files:**
- `kernel/Cargo.toml`
- `Cargo.lock`
- `AGENTS.md`
- `docs/roadmap/README.md`

**Symbol:** `version` field in `kernel/Cargo.toml` `[package]`
**Why it matters:** Project convention bumps the kernel minor version by 1 per shipped phase; AGENTS.md's version cursor must stay accurate.

**Acceptance:**
- [x] `kernel/Cargo.toml` `version = "0.69.0"`.
- [x] `Cargo.lock` regenerated to reflect the new version.
- [x] `AGENTS.md` "Kernel v0.X.0" reference updated to `v0.69.0`.
- [x] `docs/roadmap/README.md` row for Phase 69 updated to reflect Completed status at ship.
- [x] `cargo xtask check` passes after the version bump.
- [x] Git tag `v0.69.0` recommended at phase merge.

---

## Documentation Notes

- The alternate-screen implementation replaces the fb-takeover Tier 2 trigger described in `docs/appendix/fb-takeover-tiers.md` § "Tier 2". Tier 2 proposed hijacking the alt-screen sequence for FB ownership; Phase 69 implements alt-screen as a proper terminal feature with no FB side effect. Update `fb-takeover-tiers.md` to note this resolution.
- The ANSI parser lives in `kernel-core/src/fb.rs` (not `kernel-core::ansi` — there is no such module). Earlier drafts of this doc cited a fictional path; the parser is `kernel_core::fb::AnsiParser` and is reused by `userspace/term/src/screen.rs::Screen::feed`.
- `TERM=m3os-term` is set by `init` via the `ENV_TERM` constant + `build_service_envp`, **not** by `session_manager` (`session_manager` does not fork/exec; `init` does).
- The kernel `TIOCSWINSZ` branch already sends SIGWINCH; Track D adds only the `term`-side call site and an end-to-end test. No kernel changes are required.
- Mouse reporting routing flows through the Phase 56 surface input hook into a new `Pointer` variant of `PulledEvent`; no new syscalls.
- Real-application validation (nvim, tmux, htop, less, mc) is **not** in Phase 69 acceptance — it moves to Phase 69d after ncurses + the first quality TUI app port.
