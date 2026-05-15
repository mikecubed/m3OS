# `m3os-term` — Supported Escape Sequences

This appendix catalogues every ANSI/VT escape sequence that
`userspace/term` implements after Phase 69 (kernel v0.69.0).
`xtask/terminfo/m3os-term.ti` is the machine-readable source of truth;
this file is the human-readable companion.

Sequence notation:

- `ESC` denotes the byte `0x1B`.
- `CSI` denotes the two-byte introducer `ESC [`.
- `<n>` is a decimal number; `SP` is a literal `0x20` space byte.

## Cursor movement (Phase 22b baseline)

| Sequence | Name | Effect |
|---|---|---|
| `CSI <n> A` | CUU | Move cursor up `n` rows. |
| `CSI <n> B` | CUD | Move cursor down `n` rows. |
| `CSI <n> C` | CUF | Move cursor forward `n` cols. |
| `CSI <n> D` | CUB | Move cursor back `n` cols. |
| `CSI <n> G` | CHA | Move to column `n` (1-based). |
| `CSI <r> ; <c> H` | CUP | Move to row `r`, column `c` (1-based). |
| `CSI <n> J` | ED | Erase in display: `0` cursor→end, `1` start→cursor, `2` whole screen. |
| `CSI <n> K` | EL | Erase in line: `0` cursor→eol, `1` sol→cursor, `2` whole line. |

## SGR — Select Graphic Rendition

`CSI <p1> ; <p2> ; … m`. Phase 69 added the 256-color (`38 ; 5 ; <n>`),
truecolor (`38 ; 2 ; <r> ; <g> ; <b>`), and bright-8-color (`90..=97` /
`100..=107`) sub-grammars. Decorative attributes (bold, italic,
underline) are parsed via `SgrOp::Bold` / `Underline` / `Reverse` and
ignored by the current renderer — the 8×16 bitmap font has no
variant glyphs.

| SGR code | `SgrOp` variant | Renderer effect |
|---|---|---|
| `0` | `Reset` | Reset fg/bg to defaults. |
| `1` | `Bold` | Recorded; no glyph variant today. |
| `4` | `Underline` | Recorded; not painted today. |
| `7` | `Reverse` | Recorded; not painted today. |
| `22` / `24` / `27` | `NoBold` / `NoUnderline` / `NoReverse` | Recorded; no-op. |
| `30..=37` | `Fg8(0..=7)` | Standard 8-color foreground. |
| `39` | `FgDefault` | Reset fg to default. |
| `40..=47` | `Bg8(0..=7)` | Standard 8-color background. |
| `49` | `BgDefault` | Reset bg to default. |
| `90..=97` | `FgBright8(0..=7)` | Bright 8-color foreground. |
| `100..=107` | `BgBright8(0..=7)` | Bright 8-color background. |
| `38 ; 5 ; <n>` | `FgIndexed(n)` | 256-color indexed foreground. |
| `48 ; 5 ; <n>` | `BgIndexed(n)` | 256-color indexed background. |
| `38 ; 2 ; <r> ; <g> ; <b>` | `FgRgb(r,g,b)` | 24-bit RGB foreground. |
| `48 ; 2 ; <r> ; <g> ; <b>` | `BgRgb(r,g,b)` | 24-bit RGB background. |

Indexed values are resolved against
`userspace/term/src/screen.rs::XTERM_256_PALETTE` (standard xterm
layout: 0..=7 base, 8..=15 bright, 16..=231 6×6×6 cube, 232..=255
greyscale ramp).

## DEC private modes

`CSI ? <code> h` (set) / `CSI ? <code> l` (reset). Phase 69 introduced
the typed `ConsoleCmd::DecPrivateMode { codes: [u16; MAX_PARAMS], count, set }`
arm — a single CSI may carry multiple semicolon-separated codes
(e.g. `CSI ? 1006 ; 1000 h` from the terminfo `XM` capability) and
consumers iterate `codes[..count]`. Single-code payloads can be
constructed via the `ConsoleCmd::dec_private_single` helper.
Consumers that do not recognise a code drop it silently.

| Code | Name | Effect in `term` |
|---|---|---|
| `25` | DECTCEM | Show/hide cursor — accepted; no-op (cursor rendering is a `render::FramebufferOwner` concern). |
| `47` | Legacy alt-screen | Aliased to `?1049` — both route through the same `switch_to_alt` / `switch_to_primary` path (save cursor + colours on enter, restore on exit). A true `?47` no-save/restore semantic is deferred. |
| `1049` | Alt-screen + cursor save | Switch active grid + save/restore cursor and colours. |
| `9` | X10 mouse | Press-only mouse reporting via `MouseReporter`. |
| `1000` | Button-event mouse | Press + release via `MouseReporter`. |
| `1002` | Button-event with motion | Mapped to `1000` mode today; motion tracking is deferred. |
| `1003` | Any-event with motion | Mapped to `1000` mode today; motion tracking is deferred. |
| `1006` | SGR mouse | SGR-encoded `\x1b[<Pb;Px;Py M` / `m`. |
| `2004` | Bracketed paste | Toggles `Screen::bracketed_paste_enabled`; `term::input::wrap_paste` wraps payloads in `\x1b[200~ … \x1b[201~`. |

## DECSCUSR cursor shapes

`CSI <n> SP q` for `n` ∈ 0..=6. Out-of-range codes are filtered by the
parser. Mapping in `userspace/term/src/screen.rs::CursorShape::from_code`:

| Code | Shape |
|---|---|
| `0` / `1` | `BlinkingBlock` (xterm default). |
| `2` | `SteadyBlock`. |
| `3` | `BlinkingUnderline`. |
| `4` | `SteadyUnderline`. |
| `5` | `BlinkingBar`. |
| `6` | `SteadyBar`. |

Blinking variants drive a 500 ms damage tick in `term`'s main loop so
the cursor *would* blink even when the PTY is idle.

> **Phase 69 scope note.** The DECSCUSR parser, `Screen::cursor_shape`
> state, and the 500 ms `mark_damaged()` blink-tick all ship in Phase 69
> and are observable via `tui-smoke cursor`. The actual cursor *pixel*
> render (block / underline / bar fill on the framebuffer) is deferred —
> `RenderCommand::MoveCursor` is currently a documented no-op so the
> blink tick repaints an identical frame. A follow-up phase will land
> the visible cursor glyph; until then, cursor styling is wire-correct
> but not yet user-visible.

## Mouse reporting wire format

`userspace/term/src/mouse.rs::MouseReporter` produces:

- X10 (`?9`): `\x1b[M Cb Cx Cy` — single 6-byte sequence per press,
  with each coordinate offset by `+32`. No release events.
- Button-event (`?1000`): same wire form as X10, but release events
  emit `Cb = 3 + 32` (`#`).
- SGR (`?1006`): `\x1b[<Pb;Px;Py M` for press and
  `\x1b[<Pb;Px;Py m` (lower-case `m`) for release. Coordinates are
  1-based, not offset by 32.

The pointer's pixel position is divided by the 8×16 glyph metrics to
produce 1-based cell coordinates, clamped into `1..=cols` / `1..=rows`.

## Bracketed paste

`term::input::wrap_paste(payload, enabled)` returns either:

- `\x1b[200~ <payload> \x1b[201~` when `enabled` is `true`, or
- the payload verbatim when `enabled` is `false`.

The mode bit is owned by `Screen::bracketed_paste_enabled` and toggled
via a `DecPrivateMode { codes, count, set }` payload whose
`codes[..count]` slice contains `2004`. (Constructed by the parser when
it sees `CSI ? 2004 h/l`, or by callers via
`ConsoleCmd::dec_private_single(2004, …)`.)

## Surface resize → SIGWINCH

When `display_server` sends `ServerMessage::SurfaceResized { width,
height }`, `term`'s main loop converts to cell coordinates
(`cols = width / 8`, `rows = height / 16`), calls `Screen::resize`,
and issues `ioctl(TIOCSWINSZ)` on the PTY primary fd. The kernel
TIOCSWINSZ branch (`kernel/src/arch/x86_64/syscall/mod.rs:11398`) then
sends `SIGWINCH` to the foreground process group.

## Termios contract (Phase 69a)

The full POSIX termios surface lives in `kernel-core/src/tty.rs` and is
consumed via the userspace `tcgetattr` / `tcsetattr` / `cfmakeraw`
helpers in `userspace/syscall-lib/src/lib.rs`.  Each flag is honoured
on both the kernel TTY0 ldisc and the per-PTY-pair ldisc; numeric
values match Linux/musl bit positions so future C ports compile
unchanged.

### `c_iflag` — input mode

| Bit | Behaviour |
|---|---|
| `IGNBRK = 0o000001` | Ignore BREAK condition (no serial driver yet). |
| `BRKINT = 0o000002` | BREAK → SIGINT (no serial driver yet). |
| `PARMRK = 0o000010` | Mark parity errors (no serial driver yet). |
| `INPCK  = 0o000020` | Enable parity checking (no serial driver yet). |
| `ISTRIP = 0o000040` | Strip 8th bit (no serial driver yet). |
| `INLCR  = 0o000100` | Map incoming `\n` → `\r`. |
| `IGNCR  = 0o000200` | Drop incoming `\r`. |
| `ICRNL  = 0o000400` | Map incoming `\r` → `\n`. |
| `IXON   = 0o002000` | VSTOP suspends output; VSTART resumes. |
| `IXOFF  = 0o010000` | Emit XOFF when input buffer ≥ 80 % full. |
| `IUTF8  = 0o040000` | Round-trips today; full effect lands in Phase 69b. |

### `c_oflag` — output mode

| Bit | Behaviour |
|---|---|
| `OPOST = 0o000001` | Enable output post-processing.  When cleared, write bytes pass through verbatim on both the kernel TTY0 path and the PTY slave→master path. |
| `ONLCR = 0o000004` | When `OPOST` is set, expand outgoing `\n` to `\r\n`. |

### `c_lflag` — local mode

| Bit | Behaviour |
|---|---|
| `ISIG    = 0o000001` | VINTR/VQUIT/VSUSP raise SIGINT/SIGQUIT/SIGTSTP via `send_signal_to_group`. |
| `ICANON  = 0o000002` | Cooked-mode line editor; clearing switches the PTY slave read path to byte-by-byte raw delivery. |
| `ECHO    = 0o000010` | Local echo of input bytes. |
| `ECHOE   = 0o000020` | VERASE prints `^H \b`. |
| `ECHOK   = 0o000040` | VKILL prints a `\n` after killing the line. |
| `ECHONL  = 0o000100` | Echo `\n` even when ECHO is off (canonical mode only). |
| `IEXTEN  = 0o100000` | Enables VLNEXT (literal-next) and VDISCARD. |

### `c_cc` — control characters

| Index | Symbol | Default | Role |
|---|---|---|---|
| 0  | `VINTR`    | `^C` (0x03) | Generates SIGINT when ISIG is on. |
| 1  | `VQUIT`    | `^\` (0x1C) | Generates SIGQUIT when ISIG is on. |
| 2  | `VERASE`   | `^?` (0x7F) | Erase one character (canonical). |
| 3  | `VKILL`    | `^U` (0x15) | Erase the whole line (canonical). |
| 4  | `VEOF`     | `^D` (0x04) | End-of-file on empty line; flush partial line otherwise. |
| 5  | `VTIME`    | 0           | Tenths of a second for raw-mode read timeout. |
| 6  | `VMIN`     | 1           | Minimum bytes for a raw-mode read. |
| 8  | `VSTART`   | `^Q` (0x11) | XON — resumes output when IXON is on. |
| 9  | `VSTOP`    | `^S` (0x13) | XOFF — suspends output when IXON is on. |
| 10 | `VSUSP`    | `^Z` (0x1A) | Generates SIGTSTP when ISIG is on. |
| 13 | `VDISCARD` | `^O` (0x0F) | Toggle output discard when IEXTEN is on. |
| 14 | `VWERASE`  | `^W` (0x17) | Word erase (canonical). |
| 15 | `VLNEXT`   | `^V` (0x16) | Deliver next byte literally when IEXTEN is on. |

### `tcgetattr` / `tcsetattr` semantics

| Ioctl | Userspace verb | Behaviour |
|---|---|---|
| `TCGETS  = 0x5401` | `tcgetattr(fd)` | Copy current termios to user. |
| `TCSETS  = 0x5402` | `tcsetattr_when(fd, TCSANOW, &t)` | Apply immediately. |
| `TCSETSW = 0x5403` | `tcsetattr_when(fd, TCSADRAIN, &t)` | Drain output, then apply. |
| `TCSETSF = 0x5404` | `tcsetattr_when(fd, TCSAFLUSH, &t)` | Drain output and flush input edit buffer, then apply. |

The PTY *master* fd is not a terminal device for the slave's termios; all
four ioctls return `-ENOTTY` against a master fd, matching Linux
`drivers/tty/n_tty.c`.

### VMIN / VTIME read semantics (raw mode only)

Computed inside `kernel-core::LineDiscipline::poll_read_ready`:

| `VMIN` | `VTIME` | Behaviour |
|---|---|---|
| `> 0` | `0`  | Block until ≥ `VMIN` bytes are available. |
| `0`   | `> 0`| Return immediately if any data; otherwise wait `VTIME × 100 ms`. |
| `> 0` | `> 0`| Inter-byte timer: arms on first byte, fires when `VMIN` reached or timer expires. |
| `0`   | `0`  | Polling: read returns whatever is buffered, including 0 bytes. |

The kernel slave-read path threads the deadline through `WaitQueue` via
`block_current_until` so the wait wakes on either a byte arrival or
timer expiry.  See `userspace/tcsmoke/src/main.rs` `vmin-vtime` for the
end-to-end check; the host-side equivalents live in
`kernel-core/src/tty.rs::tests::ldisc_vmin_*`.

## Deferred — not yet supported

- Kitty keyboard protocol (`CSI = …`).
- Sixel / DCS image protocols.
- Motion mouse modes `1002` / `1003` — currently mapped to `1000`.
- DEC scroll regions (`DECSTBM`, `CSI <t> ; <b> r`).
- DEC save/restore cursor (`ESC 7` / `ESC 8`).
- OSC sequences (window titles, hyperlinks).
- Italic / underline / strikethrough rendering — recorded as
  `SgrOp::Bold` / `SgrOp::Underline` / `SgrOp::Reverse` but the
  current renderer ignores them. Phase 69b will add variant glyphs.

`xtask/terminfo/m3os-term.ti` matches the live set; any new
capability should land here and there in lockstep.
