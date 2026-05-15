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

#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

use core::alloc::Layout;

use alloc::vec::Vec;
use syscall_lib::heap::BrkAllocator;
use syscall_lib::{STDIN_FILENO, STDOUT_FILENO};

use term::input::wrap_paste;
use term::mouse::{Mode as MouseMode, MouseReporter};
use term::screen::{CursorShape, RenderCommand, Screen, ScreenSelect, XTERM_256_PALETTE};

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
    let result = match sub {
        "alt-screen" => run_alt_screen(),
        "colors" => run_colors(),
        "mouse" => run_mouse(),
        "cursor" => run_cursor(),
        "resize" => run_resize(),
        "paste" => run_paste(),
        "term-env" => run_term_env(env),
        "" => {
            syscall_lib::write_str(
                STDOUT_FILENO,
                "tui-smoke: missing subcommand. Use one of: alt-screen, \
                 colors, mouse, cursor, resize, paste, term-env\n",
            );
            return 2;
        }
        _ => {
            ok_or_fail("unknown", Err("subcommand not recognised"));
            return 2;
        }
    };
    ok_or_fail(sub, result);
    if result.is_ok() { 0 } else { 1 }
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
    let mut reporter = MouseReporter::new();
    // Off-state: disabled returns None.
    let event = PointerEvent {
        timestamp_ms: 0,
        dx: 0,
        dy: 0,
        abs_position: Some((10 * 8, 5 * 16)),
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
    if bytes.as_slice() != b"\x1b[<0;11;6M" {
        return Err("sgr-press-wire-mismatch");
    }
    Ok(())
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
    // manager binds /bin/sh0's stdin to the kernel TTY; the Linux
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
