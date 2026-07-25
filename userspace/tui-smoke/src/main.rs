//! Phase 69 Track H — `tui-smoke` validator.
//!
//! Run from the post-login shell. Each subcommand exercises one
//! Phase 69 capability and prints a structured pass/fail line:
//!
//! ```text
//! TUI_SMOKE:<name>:ok
//! TUI_SMOKE:<name>:fail <reason>
//! ```
//!
//! The xtask `tui-smoke` gate drives every subcommand and asserts
//! that all of them print `:ok`.
//!
//! Subcommands:
//!
//! - `alt-screen` — `Screen::feed` of `?1049h` activates the alt
//!   buffer; `?1049l` restores primary cells + colours.
//! - `colors` — 256-color SGR and truecolor RGB map to the
//!   expected BGRA8888 pixel.
//! - `mouse` — `MouseReporter` in SGR mode encodes a left-press at
//!   `(col=10, row=5)` as `\x1b[<0;11;6M`.
//! - `cursor` — DECSCUSR transitions update `Screen::cursor_shape`.
//! - `resize` — `Screen::resize` reshapes the grid; verifies clamp.
//!   When stdin is a PTY, also exercises `ioctl(TIOCSWINSZ)` and
//!   checks that `TIOCGWINSZ` reports the new rows/cols.
//! - `paste` — `wrap_paste(b"abc", true)` returns
//!   `\x1b[200~abc\x1b[201~`; passthrough when disabled.
//! - `term-env` — `getenv("TERM") == "m3os-term"`.
//! - `mouse-live` — the only *interactive* subcommand. It puts stdin in
//!   raw mode, enters the alternate screen, turns on `?1000` tracking +
//!   `?1006` SGR encoding, and then decodes the reports `term` writes
//!   back on the PTY. Each wheel notch (`\x1b[<64;Px;PyM` up,
//!   `\x1b[<65;Px;PyM` down) repaints half the screen in a solid colour
//!   and mirrors a `cb=` sentinel to the serial console. `q` tears the
//!   modes down and reports `:ok`.

#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

use core::alloc::Layout;

use alloc::vec::Vec;
use syscall_lib::heap::BrkAllocator;
use syscall_lib::{STDIN_FILENO, STDOUT_FILENO};

use kernel_core::session::{FALLBACK_DOT_GLYPH, GLYPH_TABLE_BOX_DRAWING, resolve_glyph};
use kernel_core::tty::{EditBuffer, IUTF8};

use term::input::wrap_paste;
use term::mouse::{Mode as MouseMode, MouseReporter};
use term::screen::{
    CursorShape, REPLACEMENT_CHARACTER, RenderCommand, Screen, ScreenSelect, XTERM_256_PALETTE,
};

#[global_allocator]
static ALLOCATOR: BrkAllocator = BrkAllocator::new();

#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "tui-smoke: alloc error\n");
    syscall_lib::exit(99)
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "tui-smoke: PANIC\n");
    syscall_lib::exit(101)
}

syscall_lib::entry_point_with_env!(program_main);

fn program_main(args: &[&str], env: &[&str]) -> i32 {
    // args[0] is the program name; args[1] is the subcommand.
    let sub = args.get(1).copied().unwrap_or("");
    // Phase 69c Track F.1 — `fonts` has per-leaf subcommands. We
    // build the report name from `fonts-<leaf>` so the xtask gate
    // can match each leaf separately.
    let mut composite_buf = [0u8; 64];
    let report_name: &str = if sub == "fonts" {
        let leaf = args.get(2).copied().unwrap_or("");
        compose_fonts_name(&mut composite_buf, leaf)
    } else {
        sub
    };
    let result = match sub {
        "alt-screen" => run_alt_screen(),
        "colors" => run_colors(),
        "mouse" => run_mouse(),
        "mouse-live" => run_mouse_live(),
        "cursor" => run_cursor(),
        "resize" => run_resize(),
        "paste" => run_paste(),
        "term-env" => run_term_env(env),
        "utf8" => run_utf8(),
        "fonts" => {
            let leaf = args.get(2).copied().unwrap_or("");
            run_fonts(leaf)
        }
        "" => {
            syscall_lib::write_str(
                STDOUT_FILENO,
                "tui-smoke: missing subcommand. Use one of: alt-screen, \
                 colors, mouse, mouse-live, cursor, resize, paste, term-env, \
                 utf8, fonts\n",
            );
            return 2;
        }
        _ => {
            ok_or_fail("unknown", Err("subcommand not recognised"));
            return 2;
        }
    };
    ok_or_fail(report_name, result);
    if result.is_ok() { 0 } else { 1 }
}

fn compose_fonts_name<'a>(buf: &'a mut [u8; 64], leaf: &str) -> &'a str {
    const PREFIX: &[u8] = b"fonts-";
    let mut i = 0;
    for &b in PREFIX {
        if i < buf.len() {
            buf[i] = b;
            i += 1;
        }
    }
    for &b in leaf.as_bytes() {
        if i < buf.len() {
            buf[i] = b;
            i += 1;
        }
    }
    // The fixed `fonts-` prefix is ASCII, and `leaf.as_bytes()`
    // preserves whatever UTF-8 the caller already had (argv is
    // UTF-8, not necessarily ASCII). Concatenating valid UTF-8
    // with a leading ASCII run is still valid UTF-8, so the
    // `from_utf8` conversion below is safe by construction —
    // the fallback `"fonts"` only triggers if a future caller
    // hands us malformed UTF-8.
    core::str::from_utf8(&buf[..i]).unwrap_or("fonts")
}

fn ok_or_fail(name: &str, result: Result<(), &'static str>) {
    let mut line: [u8; 96] = [0; 96];
    let prefix = b"TUI_SMOKE:";
    let mut len = 0;
    for &b in prefix {
        if len < line.len() {
            line[len] = b;
            len += 1;
        }
    }
    for &b in name.as_bytes() {
        if len < line.len() {
            line[len] = b;
            len += 1;
        }
    }
    match result {
        Ok(()) => {
            for &b in b":ok\n" {
                if len < line.len() {
                    line[len] = b;
                    len += 1;
                }
            }
        }
        Err(reason) => {
            for &b in b":fail " {
                if len < line.len() {
                    line[len] = b;
                    len += 1;
                }
            }
            for &b in reason.as_bytes() {
                if len < line.len() {
                    line[len] = b;
                    len += 1;
                }
            }
            if len < line.len() {
                line[len] = b'\n';
                len += 1;
            }
        }
    }
    let _ = syscall_lib::write(STDOUT_FILENO, &line[..len]);
}

fn feed(screen: &mut Screen, bytes: &[u8]) -> Vec<RenderCommand> {
    let mut out = Vec::new();
    for &b in bytes {
        screen.feed(b, &mut out);
    }
    out
}

fn run_alt_screen() -> Result<(), &'static str> {
    let mut s = Screen::with_geometry(10, 4);
    let _ = feed(&mut s, b"AB");
    if s.cell(0, 0).map(|c| c.codepoint).unwrap_or(0) != b'A' as u32 {
        return Err("primary-cell-zero-wrong-before-alt");
    }
    let _ = feed(&mut s, b"\x1b[?1049h");
    if !matches!(s.active(), ScreenSelect::Alt) {
        return Err("not-alt-after-1049h");
    }
    let _ = feed(&mut s, b"Z");
    if s.cell(0, 0).map(|c| c.codepoint).unwrap_or(0) != b'Z' as u32 {
        return Err("alt-cell-zero-not-z");
    }
    if s.cell_primary(0, 0).map(|c| c.codepoint).unwrap_or(0) != b'A' as u32 {
        return Err("primary-cell-zero-corrupted-by-alt");
    }
    let _ = feed(&mut s, b"\x1b[?1049l");
    if !matches!(s.active(), ScreenSelect::Primary) {
        return Err("not-primary-after-1049l");
    }
    if s.cell(0, 0).map(|c| c.codepoint).unwrap_or(0) != b'A' as u32 {
        return Err("primary-cell-zero-not-restored");
    }
    Ok(())
}

fn run_colors() -> Result<(), &'static str> {
    let mut s = Screen::with_geometry(10, 2);
    let _ = feed(&mut s, b"\x1b[38;5;208m");
    let (fg, _) = s.colors();
    if fg != XTERM_256_PALETTE[208] {
        return Err("indexed-208-mismatch");
    }
    let _ = feed(&mut s, b"\x1b[38;2;1;2;3m");
    let (fg, _) = s.colors();
    let expected_rgb: u32 = 0xFF00_0000 | (1 << 16) | (2 << 8) | 3;
    if fg != expected_rgb {
        return Err("rgb-1-2-3-mismatch");
    }
    let _ = feed(&mut s, b"\x1b[0m");
    let (fg, bg) = s.colors();
    if fg != term::screen::DEFAULT_FG || bg != term::screen::DEFAULT_BG {
        return Err("sgr-0-did-not-reset");
    }
    Ok(())
}

fn run_mouse() -> Result<(), &'static str> {
    use kernel_core::input::events::{ModifierState, PointerButton, PointerEvent};
    // Phase 112 — read the cell size from `term`'s crate root, the same
    // constants `term::mouse::compute_cell_position` projects with.
    //
    // These used to be private `16` / `32` literals duplicated here.
    // Phase 73 moved the real cell to 24×48 and this copy was missed, so
    // the synthetic pixel input was projected onto a grid 1.5× too fine
    // and the expected wire string below only held by coincidence.
    // Sourcing them makes the `(11, 6)` assertion true by construction at
    // any cell size.
    const CELL_W_PX: i32 = term::CELL_WIDTH as i32;
    const CELL_H_PX: i32 = term::CELL_HEIGHT as i32;
    let mut reporter = MouseReporter::new();
    // Off-state: disabled returns None.
    let event = PointerEvent {
        timestamp_ms: 0,
        dx: 0,
        dy: 0,
        abs_position: Some((10 * CELL_W_PX, 5 * CELL_H_PX)),
        button: PointerButton::Down(0),
        wheel_dx: 0,
        wheel_dy: 0,
        modifiers: ModifierState::default(),
    };
    if reporter.encode(&event, 80, 25).is_some() {
        return Err("disabled-returned-some");
    }
    reporter.enable(MouseMode::Sgr);
    let bytes = match reporter.encode(&event, 80, 25) {
        Some(b) => b,
        None => return Err("sgr-encode-returned-none"),
    };
    // Cell origin is 1-based: a click at pixel `(10*W, 5*H)` lands on
    // cell `(11, 6)` regardless of the cell pixel size.
    if bytes.as_slice() != b"\x1b[<0;11;6M" {
        return Err("sgr-press-wire-mismatch");
    }
    Ok(())
}

// ===========================================================================
// `mouse-live` — alternate-screen wheel-tracking probe
// ===========================================================================
//
// The Phase 112 framebuffer gate needs an application that (a) runs on the
// alternate screen, (b) has grabbed the mouse, and (c) reacts visibly to a
// wheel notch. A ported TUI is a poor fit: htop's process list on m3OS is
// short enough that a scroll can legitimately change zero pixels, and nothing
// guarantees ncurses turns xterm mouse tracking on under `TERM=m3os-term`.
// This probe enables tracking itself and repaints half the screen per notch,
// so both the "did term report it" and "did the app see it" halves of the
// contract are observable — the first on serial, the second on the
// framebuffer.

/// Sequences the probe writes on entry: alternate screen, button-event
/// tracking, SGR encoding, cursor home, erase. `?1000` (which events are
/// reported) and `?1006` (how they are encoded) are independent modes in
/// `term`'s reporter, so both are required — `?1000h` alone would emit the
/// legacy `\x1b[M` form this decoder does not accept.
const MOUSE_LIVE_SETUP: &[u8] = b"\x1b[?1049h\x1b[?1000h\x1b[?1006h\x1b[0m\x1b[H\x1b[2J";

/// The exact inverse of [`MOUSE_LIVE_SETUP`], in reverse order: stop
/// reporting before dropping the encoding, then leave the alternate screen so
/// the primary buffer (and the shell prompt on it) comes back intact. The
/// gate asserts the primary screen is restored, so this must run on *every*
/// exit path, including the timeout one.
const MOUSE_LIVE_TEARDOWN: &[u8] = b"\x1b[0m\x1b[?1000l\x1b[?1006l\x1b[?1049l";

/// Prefix shared by every serial line this subcommand emits. `ok_or_fail`
/// prints the same shape to stdout, but stdout here is `term`'s PTY — it
/// renders to the framebuffer and never reaches the console the gate greps.
const MOUSE_LIVE_PREFIX: &str = "TUI_SMOKE:mouse-live:";

/// Idle polls tolerated before the probe gives up. Each read is a 100 ms
/// VTIME poll, so this is ~30 s of silence — long enough for the harness to
/// boot a viewer and inject, short enough that a broken gate fails rather
/// than hanging until the outer QEMU timeout.
const MOUSE_LIVE_IDLE_POLLS: u32 = 300;

/// Hard ceiling on read iterations regardless of whether they carried data.
/// Guards against a peer that streams bytes forever without ever sending `q`.
const MOUSE_LIVE_READ_BUDGET: u32 = 6000;

/// Fixed-capacity ASCII line builder. The probe formats a handful of short
/// sentinels and cannot use `format!` (no `std`, and the serial call takes a
/// `&str`), so numbers are rendered by hand into this buffer.
struct LineBuf {
    bytes: [u8; 96],
    len: usize,
}

impl LineBuf {
    fn new() -> Self {
        Self {
            bytes: [0; 96],
            len: 0,
        }
    }

    fn push_str(&mut self, s: &str) {
        for &b in s.as_bytes() {
            if self.len < self.bytes.len() {
                self.bytes[self.len] = b;
                self.len += 1;
            }
        }
    }

    fn push_u32(&mut self, value: u32) {
        // `u32::MAX` is 10 digits; emit least-significant first into a
        // scratch array, then reverse it into the line.
        let mut digits = [0u8; 10];
        let mut n = value;
        let mut count = 0usize;
        loop {
            digits[count] = b'0' + (n % 10) as u8;
            n /= 10;
            count += 1;
            if n == 0 {
                break;
            }
        }
        while count > 0 {
            count -= 1;
            if self.len < self.bytes.len() {
                self.bytes[self.len] = digits[count];
                self.len += 1;
            }
        }
    }

    /// Everything pushed above is ASCII by construction, so the conversion
    /// cannot fail; the empty fallback keeps the helper panic-free anyway.
    fn as_str(&self) -> &str {
        core::str::from_utf8(&self.bytes[..self.len]).unwrap_or("")
    }
}

/// Emit `TUI_SMOKE:mouse-live:<tag><value>` on the serial console.
fn mouse_live_serial(tag: &str, value: u32) {
    let mut line = LineBuf::new();
    line.push_str(MOUSE_LIVE_PREFIX);
    line.push_str(tag);
    line.push_u32(value);
    line.push_str("\n");
    syscall_lib::serial_print(line.as_str());
}

/// Emit `TUI_SMOKE:mouse-live:<text>` on the serial console.
fn mouse_live_serial_str(text: &str) {
    let mut line = LineBuf::new();
    line.push_str(MOUSE_LIVE_PREFIX);
    line.push_str(text);
    syscall_lib::serial_print(line.as_str());
}

/// Which half of the alternate screen a wheel notch paints, and in which
/// colour. The two arms differ in *both* the region covered and the colour so
/// a PPM frame diff can tell scroll-up from scroll-down without decoding any
/// text out of the framebuffer.
#[derive(Clone, Copy)]
enum WheelBand {
    Up,
    Down,
}

/// Decoder states for the SGR mouse reports arriving on stdin.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SgrState {
    /// Between sequences — ordinary keystrokes (e.g. `q`) land here.
    Ground,
    /// Saw `ESC`.
    Esc,
    /// Saw `ESC [`; the next byte must be `<` for a mouse report.
    Csi,
    /// Inside `Pb ; Px ; Py`, waiting for the `M`/`m` final byte.
    Params,
}

/// One decoded `\x1b[<Pb;Px;Py M|m` report.
struct SgrMouseReport {
    button: u32,
    /// Lower-case `m` final byte — a button release. The wheel never
    /// produces one (a notch has no release edge), so the probe drops these.
    release: bool,
}

/// Incremental decoder for `term`'s SGR mouse reports. Byte-at-a-time and
/// allocation-free: a malformed or unrecognised run resets to `Ground` rather
/// than accumulating, so a stray escape sequence on stdin cannot wedge it.
struct SgrMouseDecoder {
    state: SgrState,
    params: [u32; 3],
    param_idx: usize,
}

impl SgrMouseDecoder {
    fn new() -> Self {
        Self {
            state: SgrState::Ground,
            params: [0; 3],
            param_idx: 0,
        }
    }

    /// True when no escape sequence is in flight, i.e. the next byte is a
    /// plain keystroke. The quit key is only honoured here so that a `q`
    /// appearing inside a (hypothetical) escape sequence is not mistaken for
    /// one.
    fn is_ground(&self) -> bool {
        self.state == SgrState::Ground
    }

    fn feed(&mut self, byte: u8) -> Option<SgrMouseReport> {
        match self.state {
            SgrState::Ground => {
                if byte == 0x1b {
                    self.state = SgrState::Esc;
                }
                None
            }
            SgrState::Esc => {
                self.state = match byte {
                    b'[' => SgrState::Csi,
                    // A second ESC restarts the sequence rather than
                    // discarding the one that is about to begin.
                    0x1b => SgrState::Esc,
                    _ => SgrState::Ground,
                };
                None
            }
            SgrState::Csi => {
                match byte {
                    b'<' => {
                        self.params = [0; 3];
                        self.param_idx = 0;
                        self.state = SgrState::Params;
                    }
                    0x1b => self.state = SgrState::Esc,
                    // Any other CSI (cursor reports, DA replies, …) is not
                    // ours; drop it and wait for the next ESC.
                    _ => self.state = SgrState::Ground,
                }
                None
            }
            SgrState::Params => match byte {
                b'0'..=b'9' => {
                    if self.param_idx < self.params.len() {
                        let slot = &mut self.params[self.param_idx];
                        *slot = slot
                            .saturating_mul(10)
                            .saturating_add(u32::from(byte - b'0'));
                    }
                    None
                }
                b';' => {
                    // Clamp instead of wrapping: a report with more than the
                    // three xterm parameters keeps the ones we already have.
                    if self.param_idx + 1 < self.params.len() {
                        self.param_idx += 1;
                    }
                    None
                }
                b'M' | b'm' => {
                    let report = SgrMouseReport {
                        button: self.params[0],
                        release: byte == b'm',
                    };
                    self.state = SgrState::Ground;
                    Some(report)
                }
                0x1b => {
                    self.state = SgrState::Esc;
                    None
                }
                _ => {
                    self.state = SgrState::Ground;
                    None
                }
            },
        }
    }
}

/// Append `value` as decimal ASCII to `out`.
fn push_u16_decimal(out: &mut Vec<u8>, value: u16) {
    let mut digits = [0u8; 5];
    let mut n = value;
    let mut count = 0usize;
    loop {
        digits[count] = b'0' + (n % 10) as u8;
        n /= 10;
        count += 1;
        if n == 0 {
            break;
        }
    }
    while count > 0 {
        count -= 1;
        out.push(digits[count]);
    }
}

/// Write every byte of `bytes` to `fd`, looping over short writes.
///
/// A full-screen repaint is a couple of kilobytes — larger than the PTY's
/// slave-to-master ring — so a single `write` can legitimately place fewer
/// bytes than asked and leave the band half-painted. The spin bound stops a
/// wedged reader (one that keeps accepting zero bytes) from hanging the gate.
fn write_all(fd: i32, bytes: &[u8]) -> bool {
    let mut offset = 0usize;
    let mut stalls = 0u32;
    while offset < bytes.len() {
        let n = syscall_lib::write(fd, &bytes[offset..]);
        if n < 0 {
            return false;
        }
        if n == 0 {
            stalls += 1;
            if stalls > 10_000 {
                return false;
            }
            continue;
        }
        stalls = 0;
        offset += n as usize;
    }
    true
}

/// Repaint the alternate screen for one wheel notch: erase, then flood half
/// the grid with spaces on a saturated background colour.
///
/// Spaces are used deliberately — the renderer fills each cell's background
/// before blitting its glyph, so a run of coloured spaces is a solid block of
/// pixels and the frame diff is a large contiguous region rather than a few
/// stroke pixels.
fn paint_wheel_band(band: WheelBand, rows: u16, cols: u16) -> bool {
    let mut out: Vec<u8> = Vec::new();
    // Reset SGR *before* erasing: `ED 2` blanks with the currently selected
    // background, so clearing while the previous band's colour is still
    // active would repaint the whole screen in it instead of wiping it.
    out.extend_from_slice(b"\x1b[0m\x1b[H\x1b[2J");
    let half = (rows / 2).max(1);
    // Black on saturated red / blue. The foreground is set explicitly only
    // because the renderer falls back to a default background whenever a
    // cell's fg and bg are equal; black can never collide with either band.
    let (first_row, color) = match band {
        WheelBand::Up => (1u16, &b"\x1b[38;2;0;0;0m\x1b[48;2;255;0;0m"[..]),
        WheelBand::Down => (
            rows.saturating_sub(half).saturating_add(1),
            &b"\x1b[38;2;0;0;0m\x1b[48;2;0;0;255m"[..],
        ),
    };
    out.extend_from_slice(color);
    // Stop one column short of the right edge. Writing the final column arms
    // the wrap path, and a wrap on the last row would scroll the band out
    // from under itself.
    let width = cols.saturating_sub(1);
    let mut row = first_row;
    while row < first_row.saturating_add(half) {
        out.extend_from_slice(b"\x1b[");
        push_u16_decimal(&mut out, row);
        out.extend_from_slice(b";1H");
        out.extend(core::iter::repeat_n(b' ', usize::from(width)));
        row += 1;
    }
    // Park the cursor at home with default colours so the next repaint (and
    // the teardown) start from a known state.
    out.extend_from_slice(b"\x1b[0m\x1b[H");
    write_all(STDOUT_FILENO, &out)
}

/// Body of the probe, run with stdin already in raw mode. Split out from
/// [`run_mouse_live`] so every early return still passes through the mode
/// teardown there.
fn mouse_live_session(rows: u16, cols: u16) -> Result<(), &'static str> {
    if !write_all(STDOUT_FILENO, MOUSE_LIVE_SETUP) {
        return Err("setup-write-failed");
    }
    // The harness waits for this before injecting: a wheel event delivered
    // before `?1000h` reaches `term` would scroll `term`'s own scrollback
    // viewport instead of being reported to us.
    mouse_live_serial_str("ready\n");

    let mut decoder = SgrMouseDecoder::new();
    let mut up = 0u32;
    let mut down = 0u32;
    let mut idle = 0u32;
    let mut buf = [0u8; 64];
    for _ in 0..MOUSE_LIVE_READ_BUDGET {
        let n = syscall_lib::read(STDIN_FILENO, &mut buf);
        if n < 0 {
            return Err("stdin-read-failed");
        }
        if n == 0 {
            // Zero bytes means either the VTIME poll expired or the PTY
            // master closed; neither is distinguishable here and both are
            // covered by the same budget.
            idle += 1;
            if idle >= MOUSE_LIVE_IDLE_POLLS {
                return Err("no-input-before-deadline");
            }
            continue;
        }
        idle = 0;
        for &byte in &buf[..n as usize] {
            if decoder.is_ground() && (byte == b'q' || byte == b'Q') {
                mouse_live_serial("up=", up);
                mouse_live_serial("down=", down);
                return Ok(());
            }
            let report = match decoder.feed(byte) {
                Some(r) => r,
                None => continue,
            };
            if report.release {
                continue;
            }
            let band = if report.button == u32::from(term::mouse::WHEEL_UP) {
                up += 1;
                WheelBand::Up
            } else if report.button == u32::from(term::mouse::WHEEL_DOWN) {
                down += 1;
                WheelBand::Down
            } else {
                // A real button press (selection drag, click) — not what this
                // probe measures, and repainting on it would fight the
                // selection highlight.
                continue;
            };
            if !paint_wheel_band(band, rows, cols) {
                return Err("band-write-failed");
            }
            // Emitted after the repaint so a sentinel on serial implies the
            // framebuffer has already been asked to change.
            mouse_live_serial("cb=", report.button);
        }
    }
    Err("read-budget-exhausted")
}

/// Phase 112 — `tui-smoke mouse-live`.
///
/// Proves the wheel reaches a mouse-tracking application on the alternate
/// screen: `term` must project the pointer onto the cell grid, encode the
/// notch as the xterm 64/65 pseudo-button, and deliver it on the PTY instead
/// of consuming it for its own scrollback viewport.
fn run_mouse_live() -> Result<(), &'static str> {
    let saved = match syscall_lib::tcgetattr(STDIN_FILENO) {
        Ok(t) => t,
        Err(_) => return Err("stdin-not-a-tty"),
    };
    let mut raw = saved;
    syscall_lib::cfmakeraw(&mut raw);
    // `cfmakeraw` leaves VMIN=1 / VTIME=0, i.e. a read with no input parks
    // forever — which would wedge the gate whenever the harness fails to
    // inject. VMIN=0 / VTIME=1 makes each read a 100 ms poll that returns 0
    // on expiry, which is what gives the idle budget something to count.
    raw.c_cc[syscall_lib::VMIN] = 0;
    raw.c_cc[syscall_lib::VTIME] = 1;
    if syscall_lib::tcsetattr(STDIN_FILENO, &raw).is_err() {
        return Err("raw-mode-set-failed");
    }
    // Fall back to the classic 80×25 when stdin cannot report a size, or
    // reports a degenerate one (a PTY whose winsize was never set reads back
    // as 0×0). A smaller-than-real band still paints thousands of pixels; a
    // zero-sized one would paint nothing and the gate would see no change.
    let (rows, cols) = match syscall_lib::get_window_size(STDIN_FILENO) {
        Ok((r, c)) if r > 0 && c > 1 => (r, c),
        _ => (25, 80),
    };

    let outcome = mouse_live_session(rows, cols);

    let _ = write_all(STDOUT_FILENO, MOUSE_LIVE_TEARDOWN);
    let _ = syscall_lib::tcsetattr(STDIN_FILENO, &saved);

    // Mirror the verdict to serial in the same shape `ok_or_fail` uses. The
    // caller's stdout copy renders into `term`, so without this the gate
    // would have no machine-readable outcome to wait on.
    match outcome {
        Ok(()) => mouse_live_serial_str("ok\n"),
        Err(reason) => {
            let mut line = LineBuf::new();
            line.push_str(MOUSE_LIVE_PREFIX);
            line.push_str("fail ");
            line.push_str(reason);
            line.push_str("\n");
            syscall_lib::serial_print(line.as_str());
        }
    }
    outcome
}

fn run_cursor() -> Result<(), &'static str> {
    let mut s = Screen::with_geometry(10, 2);
    if !matches!(s.cursor_shape(), CursorShape::BlinkingBlock) {
        return Err("default-not-blinking-block");
    }
    let _ = feed(&mut s, b"\x1b[5 q");
    if !matches!(s.cursor_shape(), CursorShape::BlinkingBar) {
        return Err("decscusr-5-not-blinking-bar");
    }
    let _ = feed(&mut s, b"\x1b[2 q");
    if !matches!(s.cursor_shape(), CursorShape::SteadyBlock) {
        return Err("decscusr-2-not-steady-block");
    }
    // Out-of-range filtered → no state change.
    let _ = feed(&mut s, b"\x1b[9 q");
    if !matches!(s.cursor_shape(), CursorShape::SteadyBlock) {
        return Err("decscusr-9-changed-shape");
    }
    Ok(())
}

fn run_resize() -> Result<(), &'static str> {
    let mut s = Screen::with_geometry(8, 4);
    let _ = feed(&mut s, b"abcdef");
    let mut out: Vec<RenderCommand> = Vec::new();
    s.resize(4, 2, &mut out);
    if s.cols() != 4 || s.rows() != 2 {
        return Err("resize-cols-rows-mismatch");
    }
    if s.cell(0, 0).map(|c| c.codepoint).unwrap_or(0) != b'a' as u32 {
        return Err("resize-cell-zero-overwritten");
    }
    if s.cell(0, 3).map(|c| c.codepoint).unwrap_or(0) != b'd' as u32 {
        return Err("resize-cell-three-overwritten");
    }
    // Now exercise TIOCSWINSZ on stdin if it is a TTY. The session
    // manager binds /bin/sh0's stdin to the kernel TTY; the m3OS
    // kernel TIOCSWINSZ branch sends SIGWINCH to the foreground
    // process group, and updates `tty.winsize`. The smoke gate
    // simply asserts the ioctl returns 0 and a follow-up
    // `TIOCGWINSZ` reports the same dimensions.
    let ws = syscall_lib::Winsize {
        ws_row: 32,
        ws_col: 100,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let rc = syscall_lib::ioctl(
        STDIN_FILENO,
        syscall_lib::TIOCSWINSZ,
        &ws as *const syscall_lib::Winsize as usize,
    );
    if rc < 0 {
        // Not fatal — some shells inherit a non-TTY stdin (e.g. when
        // tui-smoke is piped to). The cell-grid resize already passed.
        return Ok(());
    }
    match syscall_lib::get_window_size(STDIN_FILENO) {
        Ok((rows, cols)) => {
            if rows != 32 || cols != 100 {
                return Err("tiocgwinsz-did-not-reflect-set");
            }
        }
        Err(_) => {
            // TIOCGWINSZ failed; the set was accepted (rc == 0), so
            // we treat this as Ok — the read is a defence-in-depth
            // check only.
        }
    }
    Ok(())
}

fn run_paste() -> Result<(), &'static str> {
    let wrapped = wrap_paste(b"abc", true);
    if wrapped.as_slice() != b"\x1b[200~abc\x1b[201~" {
        return Err("wrap-enabled-mismatch");
    }
    let raw = wrap_paste(b"abc", false);
    if raw.as_slice() != b"abc" {
        return Err("wrap-disabled-not-passthrough");
    }
    let empty = wrap_paste(b"", true);
    if empty.as_slice() != b"\x1b[200~\x1b[201~" {
        return Err("wrap-empty-payload-mismatch");
    }
    Ok(())
}

/// Phase 69b Track H — `tui-smoke utf8`.
///
/// Drives the UTF-8 decode + glyph resolver + wide-cell accounting +
/// IUTF8 erase paths end-to-end on a host-mode `Screen` instance plus
/// a host-mode `EditBuffer`. The subcommand does not touch the kernel
/// TTY or PTY (the termios-smoke gate covers those paths) — its job is
/// to assert the byte-stream → cell-state → glyph-bitmap chain that
/// Phase 69b lands.
fn run_utf8() -> Result<(), &'static str> {
    // 1. A 3-byte UTF-8 sequence for U+2500 lands a single cell with
    //    codepoint 0x2500; the resolved glyph matches the
    //    box-drawing horizontal-line bitmap.
    let mut s = Screen::with_geometry(10, 2);
    let mut out: Vec<RenderCommand> = Vec::new();
    for &b in &[0xE2u8, 0x94, 0x80] {
        s.feed(b, &mut out);
    }
    let cell = match s.cell(0, 0) {
        Ok(c) => c,
        Err(_) => return Err("box-drawing-cell-out-of-bounds"),
    };
    if cell.codepoint != 0x2500 {
        return Err("box-drawing-cell-codepoint-mismatch");
    }
    if cell.wide_continuation {
        return Err("box-drawing-cell-flagged-wide");
    }
    let g = resolve_glyph(0x2500);
    let expected = &GLYPH_TABLE_BOX_DRAWING[0];
    if !core::ptr::eq(g.bitmap.as_ptr(), expected.bitmap.as_ptr()) {
        return Err("box-drawing-glyph-not-from-table");
    }
    // Inked-pixel sanity check: row 7 or row 8 must carry pixels.
    if g.bitmap[7] == 0 && g.bitmap[8] == 0 {
        return Err("box-drawing-glyph-empty-center");
    }

    // 2. A lone continuation byte yields U+FFFD; the rendered glyph
    //    is the fallback dot.
    let mut s2 = Screen::with_geometry(10, 2);
    let mut out2: Vec<RenderCommand> = Vec::new();
    s2.feed(0x80, &mut out2);
    let cell = match s2.cell(0, 0) {
        Ok(c) => c,
        Err(_) => return Err("invalid-cell-out-of-bounds"),
    };
    if cell.codepoint != REPLACEMENT_CHARACTER {
        return Err("invalid-cell-codepoint-not-replacement");
    }
    let g = resolve_glyph(REPLACEMENT_CHARACTER);
    if !core::ptr::eq(g.bitmap.as_ptr(), FALLBACK_DOT_GLYPH.bitmap.as_ptr()) {
        return Err("replacement-glyph-not-fallback");
    }

    // 3. A 3-byte CJK codepoint U+4E2D (UTF-8 bytes E4 B8 AD)
    //    occupies (0, 0) + (0, 1) as a wide cell pair; the renderer
    //    paints the fallback dot because CJK glyph tables are
    //    deferred to a later phase.
    let mut s3 = Screen::with_geometry(10, 2);
    let mut out3: Vec<RenderCommand> = Vec::new();
    for &b in &[0xE4u8, 0xB8, 0xAD] {
        s3.feed(b, &mut out3);
    }
    let lead = match s3.cell(0, 0) {
        Ok(c) => c,
        Err(_) => return Err("wide-lead-out-of-bounds"),
    };
    if lead.codepoint != 0x4E2D {
        return Err("wide-lead-codepoint-mismatch");
    }
    let trail = match s3.cell(0, 1) {
        Ok(c) => c,
        Err(_) => return Err("wide-trail-out-of-bounds"),
    };
    if !trail.wide_continuation {
        return Err("wide-trail-not-flagged");
    }
    let g = resolve_glyph(0x4E2D);
    if !core::ptr::eq(g.bitmap.as_ptr(), FALLBACK_DOT_GLYPH.bitmap.as_ptr()) {
        return Err("cjk-glyph-not-fallback");
    }

    // 4. With IUTF8 set, pushing a 2-byte Latin-1 sequence into a
    //    canonical-mode edit buffer and erasing once removes the
    //    whole codepoint.
    let mut buf = EditBuffer::new();
    for &b in &[0xC3u8, 0xA9] {
        if !buf.push(b) {
            return Err("edit-buf-push-failed");
        }
    }
    let _ = IUTF8; // touch the imported flag so its presence is part of the gate
    let removed = buf.erase_one_codepoint(true);
    if removed != 2 {
        return Err("iutf8-erase-did-not-remove-two-bytes");
    }
    if !buf.is_empty() {
        return Err("iutf8-erase-left-residue");
    }

    Ok(())
}

/// Phase 69c Track F.1 — `tui-smoke fonts <leaf>` checks.
///
/// Each leaf exercises one acceptance bullet from the
/// `Track F.1` task list:
///
/// - `startup` — the in-process check is "the staged font is
///   readable, parses cleanly, and a fresh in-process atlas warms
///   to at least 64 non-blank printable-ASCII glyphs". The
///   complementary boot-log assertion (that the parent `term`
///   process logs `term: atlas loaded N glyphs` for `N > 100`) is
///   **not** exercised by the current xtask harness — `tui_smoke_steps`
///   waits only for the `TUI_SMOKE:fonts-startup:ok` sentinel, not
///   the `term:` boot-log line. Wiring the harness to wait for the
///   boot-log line is a documented follow-up.
/// - `branch-icon` — opens the staged font, builds a fresh atlas,
///   confirms the font's cmap covers U+E0A0, and asserts the
///   rasterized bitmap has more ink than the 4-pixel fallback dot
///   so a non-Nerd-Font asset cannot silently pass. Also feeds the
///   UTF-8 bytes through `Screen::feed` and asserts the cell's
///   recorded codepoint.
/// - `emoji` — feeds the UTF-8 bytes for U+1F600 through `Screen::feed`
///   to exercise the 4-byte decode path, then resolves the codepoint.
///   Pass either way on the resolver: a covered glyph produces
///   non-blank pixels, an uncovered one falls back to the centred-dot
///   (also non-blank). A regression that returns a blank bitmap or
///   a wrong cell codepoint fails the gate.
/// - `adversarial` — writes 2 × CAP distinct codepoints into a
///   CAP-cap atlas. Asserts `atlas.len() == CAP` after the stream,
///   the first-inserted codepoint has been evicted, and the
///   most-recent insert is still cached.
/// - `missing-font` — in-process check that Phase 69b's
///   static-table resolver still covers ASCII + Latin-1 +
///   box-drawing. The complementary stripped-disk variant (boot
///   with the font omitted and watch for `term: font load failed;
///   using static fallback` in the kernel log) is currently *not*
///   exercised — the xtask harness always stages the font on the
///   data disk and reuses that disk for every `fonts-*` leaf.
///   Running the kernel against a stripped image for this single
///   step is a documented follow-up.
fn run_fonts(leaf: &str) -> Result<(), &'static str> {
    match leaf {
        "startup" => run_fonts_startup(),
        "branch-icon" => run_fonts_branch_icon(),
        "emoji" => run_fonts_emoji(),
        "adversarial" => run_fonts_adversarial(),
        "missing-font" => run_fonts_missing_font(),
        "" => Err("missing-leaf"),
        _ => Err("unknown-leaf"),
    }
}

const FONT_PATH: &[u8] = b"/usr/share/fonts/m3os/term.ttf\0";

fn load_font_bytes() -> Result<Vec<u8>, &'static str> {
    let fd = syscall_lib::open(FONT_PATH, syscall_lib::O_RDONLY, 0);
    if fd < 0 {
        return Err("open-failed");
    }
    let fd = fd as i32;
    let mut bytes: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let n = syscall_lib::read(fd, &mut chunk);
        if n < 0 {
            let _ = syscall_lib::close(fd);
            return Err("read-failed");
        }
        if n == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..n as usize]);
    }
    let _ = syscall_lib::close(fd);
    if bytes.is_empty() {
        return Err("font-empty");
    }
    Ok(bytes)
}

fn run_fonts_startup() -> Result<(), &'static str> {
    let bytes = load_font_bytes()?;
    if bytes.len() < 1024 {
        return Err("font-too-small");
    }
    // Cell metrics MUST match the production terminal's — rasterizing at
    // a different cell size exercises a different path than `term` and a
    // regression at the real size could pass here. Phase 112 moved the
    // constants to `term`'s crate root (ungated), so source them instead
    // of duplicating literals that silently went stale in Phase 73.
    const CELL_W: u8 = term::CELL_WIDTH;
    const CELL_H: u8 = term::CELL_HEIGHT;
    let atlas = kernel_core::font::Atlas::new(
        bytes,
        CELL_W,
        CELL_H,
        kernel_core::font::DEFAULT_ATLAS_CAPACITY,
    );
    let mut atlas = match atlas {
        Ok(a) => a,
        Err(_) => return Err("atlas-construct-failed"),
    };
    // Pre-warm printable ASCII (`U+0020..=U+007E`, 95 codepoints
    // inc. space). We exclude `U+007F` (DEL) because the atlas
    // classifies it as a blank control codepoint, so it can never
    // contribute to the non-blank count below. The threshold is 64
    // — not 95 — because some printable codepoints (space) may
    // legitimately map to a glyph the rasterizer renders as blank
    // even at 16 × 32, and the gate must be robust to that without
    // losing teeth.
    let mut count = 0usize;
    for cp in 0x20u32..0x7F {
        let bm = atlas.resolve(cp);
        if !bm.is_blank() {
            count += 1;
        }
    }
    if count < 64 {
        return Err("printable-ascii-mostly-blank");
    }
    if atlas.len() < 64 {
        return Err("atlas-len-too-small");
    }
    Ok(())
}

fn run_fonts_branch_icon() -> Result<(), &'static str> {
    let bytes = load_font_bytes()?;
    // The fallback dot is the centred 2 × 2 stamp `Atlas` returns
    // when the font's cmap does not cover a codepoint. A real Nerd
    // Font branch icon should both (a) have a `glyph_index` in the
    // font and (b) rasterize to substantially more ink than the
    // fallback's 4 pixels. Without the glyph-index check, a bare
    // JetBrainsMono with no Nerd Font patches would silently pass
    // because `resolve` returns the (non-blank) fallback dot.
    {
        let font = kernel_core::font::Font::open(&bytes).map_err(|_| "font-open-failed")?;
        if font.glyph_index(0xE0A0).is_none() {
            return Err("branch-icon-not-in-font-cmap");
        }
    }
    // Sourced from `term`'s crate root — see the `run_fonts_startup`
    // rationale comment for why the runtime metrics must match the
    // production terminal exactly.
    const CELL_W: u8 = term::CELL_WIDTH;
    const CELL_H: u8 = term::CELL_HEIGHT;
    let mut atlas = kernel_core::font::Atlas::new(
        bytes,
        CELL_W,
        CELL_H,
        kernel_core::font::DEFAULT_ATLAS_CAPACITY,
    )
    .map_err(|_| "atlas-construct-failed")?;
    let bm = atlas.resolve(0xE0A0);
    if bm.is_blank() {
        return Err("branch-icon-rendered-blank");
    }
    // The fallback dot paints exactly 4 px (a 2 × 2 stamp), so an
    // `ink_count() <= 4` rejection catches the fallback path
    // without rejecting a sparsely-rasterized real glyph at 16 × 32.
    // The 4 px threshold is independent of cell size — the fallback
    // stamp is a fixed 2 × 2 — so the check stays valid as the cell
    // metrics evolve.
    if bm.ink_count() <= 4 {
        return Err("branch-icon-rendered-as-fallback-dot");
    }
    // Also check that the screen state machine records the
    // codepoint at (0, 0) when the UTF-8 bytes for U+E0A0 land —
    // U+E0A0 → 0xEE 0x82 0xA0.
    let mut s = Screen::with_geometry(10, 2);
    let mut out: Vec<RenderCommand> = Vec::new();
    for &b in &[0xEEu8, 0x82, 0xA0] {
        s.feed(b, &mut out);
    }
    let cell = match s.cell(0, 0) {
        Ok(c) => c,
        Err(_) => return Err("branch-icon-cell-out-of-bounds"),
    };
    if cell.codepoint != 0xE0A0 {
        return Err("branch-icon-cell-codepoint-mismatch");
    }
    Ok(())
}

fn run_fonts_emoji() -> Result<(), &'static str> {
    let bytes = load_font_bytes()?;
    // Drive the 4-byte UTF-8 decode path through `Screen::feed`
    // before checking the atlas — the documented smoke contract
    // says this leaf *writes* U+1F600, not just *resolves* it. A
    // regression in the terminal's 4-byte UTF-8 → cell pipeline
    // for emoji would otherwise sneak past the atlas-only check.
    // U+1F600 encodes as 0xF0 0x9F 0x98 0x80.
    let mut s = Screen::with_geometry(10, 2);
    let mut out: Vec<RenderCommand> = Vec::new();
    for &b in &[0xF0u8, 0x9F, 0x98, 0x80] {
        s.feed(b, &mut out);
    }
    let cell = match s.cell(0, 0) {
        Ok(c) => c,
        Err(_) => return Err("emoji-cell-out-of-bounds"),
    };
    if cell.codepoint != 0x1F600 {
        return Err("emoji-cell-codepoint-mismatch");
    }
    // Sourced from `term`'s crate root — see `run_fonts_startup` for
    // the rationale.
    const CELL_W: u8 = term::CELL_WIDTH;
    const CELL_H: u8 = term::CELL_HEIGHT;
    let mut atlas = kernel_core::font::Atlas::new(
        bytes,
        CELL_W,
        CELL_H,
        kernel_core::font::DEFAULT_ATLAS_CAPACITY,
    )
    .map_err(|_| "atlas-construct-failed")?;
    // Contract per Phase 69c: a covered emoji produces non-blank
    // ink; an uncovered one returns the centred-dot fallback (also
    // non-blank). Either way the bitmap must not be blank — a bug
    // that returned a blank bitmap for U+1F600 would be a silent
    // regression that "no crash" cannot catch.
    let bm = atlas.resolve(0x1F600);
    if bm.is_blank() {
        return Err("emoji-resolves-blank");
    }
    Ok(())
}

fn run_fonts_adversarial() -> Result<(), &'static str> {
    let bytes = load_font_bytes()?;
    const CAP: usize = 1024;
    // Walk the font's cmap to collect `2 × CAP` codepoints the
    // font actually covers. Resolving codepoints the font does not
    // cover takes the shared-fallback path and never inserts into
    // the cache, so streaming an arbitrary BMP range can fail to
    // saturate the cache (e.g. JetBrainsMono Nerd Font Mono leaves
    // wide stretches of BMP uncovered). Filtering through
    // `glyph_index` makes the eviction assertion font-agnostic.
    let mut covered: alloc::vec::Vec<u32> = alloc::vec::Vec::new();
    {
        let font = kernel_core::font::Font::open(&bytes).map_err(|_| "font-open-failed")?;
        for cp in 0x21u32..0x10000u32 {
            if covered.len() >= 2 * CAP {
                break;
            }
            // Skip codepoints the resolver treats as blank — they
            // would not exercise the rasterize-and-insert path.
            if cp == 0x7F || (0x80..=0x9F).contains(&cp) || cp == 0xA0 {
                continue;
            }
            if font.glyph_index(cp).is_some() {
                covered.push(cp);
            }
        }
    }
    if covered.len() < 2 * CAP {
        return Err("adversarial-font-too-sparse-for-eviction");
    }
    // Sourced from `term`'s crate root — see `run_fonts_startup` for
    // the rationale.
    const CELL_W: u8 = term::CELL_WIDTH;
    const CELL_H: u8 = term::CELL_HEIGHT;
    let mut atlas = kernel_core::font::Atlas::new(bytes, CELL_W, CELL_H, CAP)
        .map_err(|_| "atlas-construct-failed")?;
    for &cp in &covered {
        let _ = atlas.resolve(cp);
    }
    if atlas.len() > CAP {
        return Err("atlas-exceeded-capacity");
    }
    // Every codepoint in `covered` had a `glyph_index` hit, so
    // every `resolve` was a fresh insert (or hit on a still-cached
    // earlier insert). With `covered.len() == 2 * CAP`, the cache
    // must end fully saturated.
    if atlas.len() != CAP {
        return Err("atlas-not-saturated-after-overflow");
    }
    let first = covered[0];
    let last = covered[covered.len() - 1];
    // The first-inserted codepoint must have been pushed off the
    // tail by the subsequent `2 * CAP - 1` inserts. A broken
    // policy that evicts the newest entry would leave `first`
    // cached and lose `last` instead — both assertions catch the
    // regression.
    if atlas.contains(first) {
        return Err("adversarial-oldest-not-evicted");
    }
    if !atlas.contains(last) {
        return Err("adversarial-mru-not-cached");
    }
    Ok(())
}

fn run_fonts_missing_font() -> Result<(), &'static str> {
    // In-process side of the missing-font check: Phase 69b's static
    // tables must continue to resolve ASCII / Latin-1 / box-drawing
    // even when no font is present. The complementary stripped-disk
    // boot variant — boot the kernel with the font omitted and
    // assert the kernel log contains
    // `term: font load failed; using static fallback` — is
    // currently *not* exercised: the `tui-smoke` xtask harness
    // always stages the font on the data disk it builds. A
    // dedicated stripped-image boot for this single leaf is tracked
    // as a deferred follow-up.
    let ascii = resolve_glyph(b'A' as u32);
    if ascii.bitmap.iter().all(|&b| b == 0) {
        return Err("ascii-A-resolves-blank");
    }
    let latin1 = resolve_glyph(0xE9);
    if latin1.bitmap.iter().all(|&b| b == 0) {
        return Err("latin1-e-acute-resolves-blank");
    }
    let box_drawing = resolve_glyph(0x2500);
    if box_drawing.bitmap.iter().all(|&b| b == 0) {
        return Err("box-drawing-resolves-blank");
    }
    Ok(())
}

fn run_term_env(env: &[&str]) -> Result<(), &'static str> {
    for entry in env {
        if let Some(value) = entry.strip_prefix("TERM=") {
            if value == "m3os-term" {
                return Ok(());
            }
            return Err("term-not-m3os-term");
        }
    }
    Err("term-not-set")
}
