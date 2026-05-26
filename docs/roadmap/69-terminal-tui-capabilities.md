# Phase 69 - Terminal Contract Foundations

**Status:** Planned
**Source Ref:** phase-69
**Depends on:** Phase 22b (ANSI Escape) ✅, Phase 29 (PTY Subsystem) ✅, Phase 56 (Display and Input Architecture) ✅, Phase 57 (Audio and Local Session) ✅, Phase 68 (Display Server Closeout) ✅
**Builds on:** Extends the Phase 57 `term` graphical terminal emulator with the terminal-contract features required for real TUI applications; extends the Phase 22b ANSI parser in `kernel_core::fb` with new private-mode and extended-SGR vocabulary; extends the Phase 56 focus-aware `PointerEvent` dispatch to carry mouse events through the PTY master; closes the Phase 29 SIGWINCH deferral by wiring the display-server resize event to the existing kernel `TIOCSWINSZ` handler
**Primary Components:** kernel-core/fb (ANSI parser), userspace/term, userspace/init (env), xtask (terminfo staging), kernel/src/arch/x86_64/syscall (TIOCSWINSZ — already complete; verification only)

## Milestone Goal

Phase 69 lands the **terminal contract foundation**: a published `m3os-term` terminfo entry, alternate-screen buffer, 256-color and truecolor SGR, X10/SGR mouse reporting, DECSCUSR cursor-shape sequences, bracketed paste, and SIGWINCH propagation from the display-server resize event. Validation is byte-level via a new hand-rolled `tui-smoke` binary — application-level validation against real TUIs (nvim, tmux, htop, less, mc) lands in Phase 69d after termios (69a), UTF-8 + bitmap font expansion (69b), TTF/Nerd Font infrastructure (69c), and an `ncurses` port (69d) are in place.

Following TDD discipline, every new escape-sequence parser arm is host-tested first in `kernel-core/src/fb.rs` (the existing `AnsiParser` home) via `cargo test -p kernel-core` before the `term` integration is written. Applying DRY, 256-color indexed and 24-bit truecolor SGR share a single `color_to_bgra` resolver in the renderer, and alt-screen enter/exit share a `buffer_swap` helper rather than duplicating state management in both `?1049` and `?47` code paths.

## Why This Phase Exists

The Phase 57 `term` emulator was built to prove the local-session concept, not to run third-party TUI software. The post-Phase-57 evaluation in `docs/research/post-phase-57 evaluation/04-tui-and-neovim-roadmap.md` enumerates six terminal-contract gaps; this phase closes the half that are pure escape-sequence and signal-routing work:

| Gap | Phase |
|---|---|
| Alternate-screen buffer | **69** |
| 256-color and truecolor SGR | **69** |
| Cursor modes (DECSCUSR) | **69** |
| Mouse reporting (X10 / button / SGR) | **69** |
| SIGWINCH propagation | **69** |
| Published terminfo entry | **69** |
| Bracketed paste | **69** (folded in) |
| Raw/cbreak termios modes | 69a |
| UTF-8 wire decoding | 69b |
| Latin-1 + box-drawing glyph coverage | 69b |
| TTF/Nerd Font rendering | 69c |
| `ncurses` port + first real TUI app validators | 69d |

Without these, applications that call `setupterm()` either fall back to `xterm`/`vt100` and send sequences `term` does not handle, or refuse to start at all. This phase closes the gap between "a terminal that runs the built-in shell" and "a terminal that speaks the wire protocol modern TUI apps expect."

## Learning Goals

- Understand how terminfo entries describe terminal capabilities and how applications query them via `setupterm()` / `tigetstr()`.
- Learn how alternate-screen buffer state is independent of scrollback state and why editors require it.
- See how 256-color and truecolor SGR parameters extend the base 16-color ANSI model.
- Understand how SIGWINCH is generated and propagated through the kernel to a foreground process group.
- Learn how X10 and SGR mouse-reporting modes translate display-server pointer events into PTY byte streams.
- See how DECSCUSR cursor-shape sequences interact with the compose-loop cursor render path.
- Understand bracketed paste (`?2004`) as a wire-level safety contract between terminal and editor.

## Feature Scope

### Terminfo entry (Track A)

Publish `m3os-term` as a compiled terminfo entry installed at `/usr/share/terminfo/m/m3os-term`. The entry declares exactly what Phase 69 `term` implements — no aspirational capabilities. The existing `TERM=m3os` literal in `userspace/init/src/main.rs:77` (`ENV_TERM`) is renamed to `TERM=m3os-term`; the matching references in `userspace/login/src/main.rs:132`, `userspace/shell/src/main.rs:419`, and `userspace/pty-test/src/main.rs:95` are updated in lock-step. Applications that call `setupterm()` or `tigetstr()` get accurate capability data rather than falling back to `xterm` and sending sequences `term` may not handle.

### Alternate-screen buffer (Track B)

Implement `\x1b[?1049h` (save cursor, switch to alternate screen) and `\x1b[?1049l` (restore cursor, switch back to primary screen) by extending the `ConsoleCmd` enum in `kernel_core::fb` with a `DecPrivateMode { code, set }` variant and matching it in `userspace/term/src/screen.rs::Screen::feed`. The older `\x1b[?47h` / `\x1b[?47l` forms (alternate screen without cursor save/restore) are handled as aliases. The alternate screen is a second cell grid, independently scrollable; entering it does not destroy primary-screen scrollback. Exiting restores the primary screen's cursor position and content exactly.

This is semantically distinct from the Tier 2 fb-takeover trigger described in `docs/appendix/fb-takeover-tiers.md` § "Tier 2" — that Tier used the alt-screen sequence as a takeover signal. Phase 69 implements alt-screen as a proper terminal feature with no framebuffer-ownership side effect.

### 256-color and truecolor SGR (Track C)

Extend the SGR parser in `kernel_core::fb` to handle the extended color parameter forms: `\x1b[38;5;<n>m` and `\x1b[48;5;<n>m` (256-color indexed foreground/background) and `\x1b[38;2;<r>;<g>;<b>m` / `\x1b[48;2;<r>;<g>;<b>m` (24-bit truecolor). The Phase 22b parser handles only the 8 standard and 8 bright colors and ignores any SGR parameter beyond that. The extended forms require the parser to consume additional parameter fields after a `38` or `48` prefix. Palette-to-BGRA8888 conversion feeds the existing Phase 57 glyph-render path.

### SIGWINCH propagation (Track D)

When the display server reports that a `term` surface has been resized, `term` must update its row/column state, commit the new `TIOCSWINSZ` to the PTY slave, and let the kernel generate SIGWINCH for the foreground process group. The kernel side is already complete — `kernel/src/arch/x86_64/syscall/mod.rs:11398` (the `TIOCSWINSZ` branch) updates `tty.winsize` and calls `send_signal_to_group(fg, SIGWINCH)`. The missing piece is the `term`-side wiring: extending the `PulledEvent` enum in `userspace/term/src/main.rs:461` with a `SurfaceResized { width, height }` variant, plumbing through `Screen::resize`, and calling `syscall_lib::ioctl(slave_fd, TIOCSWINSZ, ...)`. This track closes the Phase 29 SIGWINCH deferral noted at `docs/roadmap/29-pty-subsystem.md:123`.

### Mouse reporting (Track E)

Implement X10 mode (`\x1b[?9h`), button-event mode (`\x1b[?1000h`), and SGR mouse encoding (`\x1b[?1006h`). Phase 56 `PointerEvent` messages arrive at `term`'s surface input hook but are currently dropped silently at `userspace/term/src/main.rs:510` ("Pointer / Welcome / FocusIn / FocusOut / SurfaceConfigured / SurfaceDestroyed / BufferReleased: not load-bearing"). Track E.2 extends the `PulledEvent` enum with a `Pointer` variant, and Track E.1 introduces a new `userspace/term/src/mouse.rs` module that translates each event into the active reporting mode's byte sequence and writes it to the PTY primary fd via the existing `syscall_lib::write` path. Disable reporting on `\x1b[?9l`, `\x1b[?1000l`, `\x1b[?1006l`. Motion-tracking modes 1002 and 1003 are deferred — see "Deferred Until Later".

### Cursor styling (Track F)

Implement `\x1b[ q` (DECSCUSR) for cursor shape: 0/1 blinking block, 2 steady block, 3 blinking underline, 4 steady underline, 5 blinking bar, 6 steady bar. `term`'s cursor-render module honors the current shape at each compose frame. Because blinking variants need to repaint on a fixed cadence even when the PTY is idle, the event loop in `userspace/term/src/main.rs` gains an explicit blink-tick: when the cursor shape is a blinking variant, an idle-tick (`COMPOSE_INTERVAL_MS * N`) forces `renderer.damaged() = true` and triggers a compose pass at the 500 ms cadence. Steady shapes leave the existing damage-driven path untouched.

### Bracketed paste (Track G)

Implement `\x1b[?2004h` (enable) and `\x1b[?2004l` (disable). When enabled, any pointer-driven paste (or future clipboard insertion path) wraps its byte payload in `\x1b[200~` ... `\x1b[201~` before writing to the PTY master. The Phase 69 surface integration is minimal — the clipboard hook is not landed yet — but the parser and writer-wrap helper are in place so a 69d-era ncurses paste binding works out of the box.

### Validation (Track H)

A new userspace binary, `userspace/tui-smoke/src/main.rs`, is the Phase 69 acceptance vehicle. It emits each escape sequence and asserts on observable state via syscall checks:

- enter/exit alt-screen, verify primary-screen cell snapshot is restored;
- emit `\x1b[38;5;208m` and `\x1b[38;2;128;64;255m`, render a glyph, verify the renderer's last-pushed `(fg, bg)` matches the expected BGRA;
- emit `\x1b[ 6 q`, verify the screen's recorded cursor shape;
- emit `\x1b[?1006h` then synthesize a `PointerEvent::Button` via an in-test hook, verify the bytes written to the PTY master match the SGR encoding;
- send `SurfaceResized`, verify `stty size` (via a subprocess) reports the new geometry and that SIGWINCH was delivered to a self-installed handler;
- bracketed paste enable + simulated paste, verify the `\x1b[200~` / `\x1b[201~` wrap.

No nvim, tmux, htop, less, or mc dependency. Real-app validation moves to Phase 69d.

### Documentation updates (Track I)

Phase 22b, Phase 29, and Phase 57 design docs cross-reference Phase 69 capabilities. The supported-escape-sequence reference is added to `docs/appendix/`.

## Important Components and How They Work

### `kernel-core/src/fb.rs` — ANSI parser

The Phase 22b parser lives here as `AnsiParser` producing `ConsoleCmd` values consumed by both the kernel framebuffer console and `userspace/term/src/screen.rs`. After Phase 69:

- `ConsoleCmd` gains `DecPrivateMode { code: u16, set: bool }` (carries `?1049` / `?47` / `?9` / `?1000` / `?1006` / `?2004`) and `CursorShape { shape: u8 }`.
- `SgrParams` parsing extends to recognize the `38;5;<n>` / `48;5;<n>` / `38;2;<r>;<g>;<b>` / `48;2;<r>;<g>;<b>` shapes; the result feeds back through `Sgr(SgrParams)` with new typed variants for `IndexedFg(u8)`, `IndexedBg(u8)`, `RgbFg(u8,u8,u8)`, `RgbBg(u8,u8,u8)`.

### `userspace/term/src/screen.rs` — screen state machine

`Screen` is extended with a second cell grid (`alt_buf: Vec<Cell>`) and a `screen_active: ScreenSelect` discriminant. `switch_to_alt()` saves the primary cursor state and activates the alternate grid; `switch_to_primary()` restores it. The `cell()` / `feed()` / `cursor()` surface is unchanged from a caller's perspective; the only new method is `active_grid_id()` for the renderer to key its damage tracking on the active grid.

### `userspace/term/src/mouse.rs` (new)

Pure-logic encoder. `MouseReporter::new()` starts in `Mode::Disabled`; `enable(mode)` / `disable()` toggle state from the parser's `DecPrivateMode` arm. `encode(event)` produces `Option<Vec<u8>>` — `None` when reporting is disabled — and `main.rs` writes the bytes via `syscall_lib::write(primary_fd, …)`. Host-testable via `cargo test -p term --target x86_64-unknown-linux-gnu --lib`.

### `userspace/term/src/main.rs` — event loop

The `PulledEvent` enum gains two variants: `Pointer(PointerEvent)` and `SurfaceResized { width: u32, height: u32 }`. The match in the main loop routes `Pointer` into `MouseReporter::encode` and routes `SurfaceResized` into `Screen::resize` followed by `syscall_lib::ioctl(slave_fd, TIOCSWINSZ, ...)`. The cursor-blink tick is a small `last_blink_ms` field that synthesizes damage every 500 ms when the current shape is a blinking variant.

### `kernel/src/arch/x86_64/syscall/mod.rs` — `TIOCSWINSZ`

Already complete from Phase 29. The branch at line 11398 updates `tty.winsize` and calls `send_signal_to_group(fg, SIGWINCH)`. Phase 69 does not modify the kernel side; Track D.2 is verification-only and adds the missing userspace-end-to-end test.

### Terminfo entry

Source: `xtask/terminfo/m3os-term.ti`. Compiled by host-side `tic` during `xtask image`, staged into the ext2 data disk at `/usr/share/terminfo/m/m3os-term`. The entry covers the capabilities Phase 69 actually implements — 256 colors, alt-screen, mouse-1006, DECSCUSR, bracketed paste — and intentionally omits anything 69a/b/c/d will add later (raw-mode flags, UTF-8 glyph cells, etc.).

## How This Builds on Earlier Phases

- Extends the Phase 22b ANSI parser (`kernel_core::fb::AnsiParser`) with DEC private modes, extended SGR color forms, and cursor-shape sequences.
- Closes the Phase 29 SIGWINCH deferral by wiring `term`'s surface-resize callback to the existing kernel `TIOCSWINSZ` handler.
- Extends Phase 56's focus-aware `PointerEvent` dispatch to flow into `term`'s mouse-reporting encoder rather than being dropped at the surface boundary.
- Extends Phase 57 `term`'s single cell-grid model with a dual-grid alternate-screen implementation.
- Builds on Phase 68's `PROTOCOL_VERSION` 2 (`ModifierSide` field) — `term` is already at v2.

## Implementation Outline

1. Extend `ConsoleCmd` + `AnsiParser` in `kernel-core/src/fb.rs` with `DecPrivateMode`, `CursorShape`, and extended SGR color variants; add host tests.
2. Author `xtask/terminfo/m3os-term.ti` covering only the Phase 57-era capabilities; compile + stage via `xtask image`.
3. Rename `ENV_TERM` in `userspace/init/src/main.rs:77` to `TERM=m3os-term` and update the three sister sites (`login`, `shell`, `pty-test`).
4. Add alternate-screen dual-grid in `userspace/term/src/screen.rs`; wire `DecPrivateMode` arms for `?1049` / `?47`.
5. Extend SGR handling in `screen.rs::apply_sgr` for 256-color and truecolor; share `color_to_bgra` with the renderer.
6. Add the `Pointer` + `SurfaceResized` variants to `PulledEvent` in `main.rs`; route both.
7. Create `userspace/term/src/mouse.rs` with X10 / button / SGR encoders; wire `?9` / `?1000` / `?1006` parser arms.
8. Add DECSCUSR cursor-shape state in `Screen`; extend the renderer's cursor draw; add the blink-tick to the event loop.
9. Add bracketed-paste `?2004` mode bit + write-wrap helper.
10. Wire `SurfaceResized` to `ioctl(TIOCSWINSZ)`; expand the terminfo entry to cover everything implemented.
11. Build `userspace/tui-smoke/src/main.rs`; register it in the four-place pipeline; gate via a new `cargo xtask tui-smoke`.
12. Update Phase 22b, Phase 29, Phase 57 design docs; author `docs/appendix/term-escape-sequences.md`.

## Acceptance Criteria

- `TERM=m3os-term` is set on every supervised service spawned from `init`; `getenv("TERM")` inside `term`'s shell returns `m3os-term`.
- `infocmp m3os-term` (or an equivalent in-tree reader) returns the installed entry without error.
- `tui-smoke alt-screen` enters alt-screen, writes a known cell pattern, exits, and verifies the primary-screen cell snapshot is bit-identical to the pre-enter state.
- `tui-smoke colors` emits `\x1b[38;5;208m` and `\x1b[38;2;128;64;255m`, paints a glyph, and verifies the last-pushed `(fg, bg)` matches the expected BGRA8888 values.
- `tui-smoke mouse` enables `?1006`, synthesizes a `PointerEvent::Button` left-press at `(col=10, row=5)`, and verifies the PTY master sees `\x1b[<0;11;6M`.
- `tui-smoke cursor` emits `\x1b[ 6 q` and verifies `Screen::cursor_shape()` returns `CursorShape::SteadyBar`; the blink-tick observably toggles when shape is set to `BlinkingBar`.
- `tui-smoke resize` synthesizes a `SurfaceResized` to a smaller geometry, verifies `Screen::cols()`/`rows()` updated, verifies a self-installed SIGWINCH handler ran, and verifies `stty size` reports the new dimensions.
- `tui-smoke paste` enables `?2004` and verifies a paste write is wrapped in `\x1b[200~` / `\x1b[201~`.
- `cargo xtask check` and `cargo xtask test` both pass after the phase lands.

## Companion Task List

- [Phase 69 Task List](./tasks/69-terminal-tui-capabilities-tasks.md)

## How Real OS Implementations Differ

- Linux VTE and xterm-compatible terminals ship with upstream terminfo entries maintained by the ncurses project; m3OS must maintain its own.
- Wayland compositors pass resize events through `xdg_toplevel::configure`; m3OS uses its own typed `SurfaceResized` message on the Phase 56 control socket.
- Linux delivers SIGWINCH through the kernel TTY layer; m3OS follows the same model but the resize event originates from a userspace compositor rather than a VT switch.
- Production terminals implement the full XTGETTCAP / Kitty keyboard protocol; m3OS defers those to a later phase.

## Deferred Until Later

- **Termios raw/cbreak mode + line-discipline plumbing** → Phase 69a.
- **UTF-8 wire decoding + Latin-1 supplement + Unicode box-drawing glyphs** *(closed in Phase 69b — `kernel-core::utf8::Utf8Decoder` decodes bytes before the parser, `kernel-core::session::resolve_glyph` dispatches Latin-1 + box-drawing tables + a centred-dot fallback, and `kernel-core::session::width_of` powers wide-cell accounting in `Screen::put_char`. The `Screen::feed` extension point introduced in Phase 69 for the alternate-screen path now also hosts the UTF-8 decoder.)*
- **TTF/OTF font loader + glyph atlas + Nerd Font asset embedding** → Phase 69c.
- **`ncurses` port + first quality TUI app validators (`less`, `htop`, `tmux`)** → Phase 69d.
- **Neovim port** (libuv + Lua/LuaJIT-equivalent + tree-sitter) → dedicated phase after 69d.
- **`btop` port** (C++ toolchain dependency) → after Phase 85 cross-compiled toolchains.
- **Kitty keyboard protocol and Kitty graphics protocol**.
- **Sixel graphics rendering inside `term`**.
- **Terminal scrollback selection and clipboard integration**.
- **Configurable fonts and font scaling**.
- **IME / input method support**.
- **Motion events in mouse-reporting mode** (modes 1002, 1003).
