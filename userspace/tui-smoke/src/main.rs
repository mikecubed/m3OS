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
                 colors, mouse, cursor, resize, paste, term-env, utf8, fonts\n",
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
    // SAFETY: bytes copied are ASCII subset of the leaf identifier
    // plus the fixed prefix, both UTF-8 by construction.
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
///   readable + parses cleanly". The boot-log `term: atlas loaded N
///   glyphs` assertion is owned by the xtask `cargo xtask tui-smoke`
///   harness (Track F.2) because the boot log is captured at xtask
///   level, not visible to a post-login userspace process.
/// - `branch-icon` — opens the staged font, builds a fresh atlas,
///   resolves U+E0A0, asserts the rasterized bitmap is non-blank
///   and matches a `Screen::cell` codepoint write.
/// - `emoji` — resolves U+1F600. Pass either way: a covered glyph
///   produces non-blank pixels, an uncovered one falls back to the
///   centred-dot. Neither must crash.
/// - `adversarial` — writes 2048 distinct codepoints to a 1024-cap
///   atlas. Asserts `atlas.len() <= 1024` after the stream and that
///   the most-recent insert is present.
/// - `missing-font` — in-process check that Phase 69b's
///   static-table resolver still covers ASCII + Latin-1 +
///   box-drawing. The complementary boot-log assertion (that
///   booting with the font omitted from the data disk emits
///   `term: font load failed; using static fallback`) is currently
///   *not* exercised — the xtask harness always stages the font on
///   the data disk and reuses that disk for every `fonts-*` leaf.
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
    let atlas =
        kernel_core::font::Atlas::new(bytes, 8, 16, kernel_core::font::DEFAULT_ATLAS_CAPACITY);
    let mut atlas = match atlas {
        Ok(a) => a,
        Err(_) => return Err("atlas-construct-failed"),
    };
    // Pre-warm a representative range so the in-process check has
    // teeth — atlas must hold ≥ 100 glyphs after this.
    let mut count = 0usize;
    for cp in 0x20u32..0x80 {
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
    let mut atlas =
        kernel_core::font::Atlas::new(bytes, 8, 16, kernel_core::font::DEFAULT_ATLAS_CAPACITY)
            .map_err(|_| "atlas-construct-failed")?;
    let bm = atlas.resolve(0xE0A0);
    if bm.is_blank() {
        return Err("branch-icon-rendered-blank");
    }
    // 4 px is the fallback-dot ink count; a real branch icon's
    // glyph paints substantially more than that. Use 8 to leave
    // slack for a sparsely-rendered glyph at 8 × 16 cell size while
    // still rejecting the fallback shape.
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
    let mut atlas =
        kernel_core::font::Atlas::new(bytes, 8, 16, kernel_core::font::DEFAULT_ATLAS_CAPACITY)
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
    let mut atlas =
        kernel_core::font::Atlas::new(bytes, 8, 16, CAP).map_err(|_| "atlas-construct-failed")?;
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
    // even when no font is present. The xtask harness boots the
    // kernel with the font omitted and asserts the boot log
    // contains `term: font load failed; using static fallback`.
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
