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
the typed `ConsoleCmd::DecPrivateMode { code, set }` arm; consumers
that do not recognise a code drop it silently.

| Code | Name | Effect in `term` |
|---|---|---|
| `25` | DECTCEM | Show/hide cursor — accepted; no-op (cursor rendering is a `render::FramebufferOwner` concern). |
| `47` | Legacy alt-screen | Alias for `?1049` — switch active grid without saving cursor. |
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
the cursor blinks even when the PTY is idle.

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
via `DecPrivateMode { code: 2004, .. }`.

## Surface resize → SIGWINCH

When `display_server` sends `ServerMessage::SurfaceResized { width,
height }`, `term`'s main loop converts to cell coordinates
(`cols = width / 8`, `rows = height / 16`), calls `Screen::resize`,
and issues `ioctl(TIOCSWINSZ)` on the PTY primary fd. The kernel
TIOCSWINSZ branch (`kernel/src/arch/x86_64/syscall/mod.rs:11398`) then
sends `SIGWINCH` to the foreground process group.

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
