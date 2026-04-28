//! Phase 57 Track G.5 — `KeyEvent` → PTY byte translation.
//!
//! `InputHandler` consumes typed [`KeyEvent`]s from
//! `display_server`'s focus-aware dispatcher (which receives them
//! from `kbd_server`), applies the keymap, and writes shell-relevant
//! byte sequences to the PTY:
//!
//! - Printable ASCII flows through verbatim.
//! - Ctrl + letter produces the corresponding control code (Ctrl-A =
//!   0x01, Ctrl-C = 0x03, Ctrl-D = 0x04, ...).
//! - Arrow keys produce CSI sequences (`ESC [ A/B/C/D`).
//! - Backspace produces 0x7F (DEL) so the shell's cooked-mode line
//!   editor erases.
//! - Carriage return produces 0x0D so the PTY's line discipline can
//!   translate it to LF in cooked mode.
//! - Unknown private-use keysyms write nothing.  No worker threads —
//!   the binary's main loop drives this synchronously.

use kernel_core::input::events::{KeyEvent, KeyEventKind, MOD_CTRL};
use kernel_core::input::keymap::{KEYSYM_DOWN, KEYSYM_LEFT, KEYSYM_RIGHT, KEYSYM_UP};

/// Pluggable PTY-write seam. Production wraps `syscall_lib::write`;
/// host tests record byte slices.
pub trait PtyWriter {
    fn write(&mut self, bytes: &[u8]);
}

/// Input handler.  Consumes `KeyEvent`s, applies the keymap, writes
/// shell-relevant byte sequences to the PTY.
///
/// Stateless — every event is translated independently.  Future
/// tracks may add modal state (e.g. dead keys, IME) here.
pub struct InputHandler;

impl InputHandler {
    pub const fn new() -> Self {
        Self
    }

    /// Translate one event into PTY bytes; the writer is called once
    /// per event with the bytes (or not at all for events that do not
    /// produce output).
    pub fn translate<W: PtyWriter>(&mut self, event: &KeyEvent, writer: &mut W) {
        // Only down / repeat events produce input.  Up events update
        // modifier state in `kbd_server`; clients see the latched
        // snapshot on the next down event.
        match event.kind {
            KeyEventKind::Down | KeyEventKind::Repeat => {}
            KeyEventKind::Up => return,
        }

        let symbol = event.symbol;
        let modifiers = event.modifiers;

        // Special keys live in the private-use area (0xE000+);
        // printable ASCII in [0x20..=0x7E] flows through verbatim.
        if let Some(seq) = special_key_sequence(symbol) {
            writer.write(seq);
            return;
        }

        // Backspace (0x08) → DEL (0x7F).
        if symbol == 0x08 {
            writer.write(&[0x7F]);
            return;
        }

        // Ctrl + letter → control code.
        if modifiers.contains(MOD_CTRL) {
            // Ctrl maps the printable letter range A..Z / a..z to
            // 0x01..0x1A.  Other Ctrl-combinations (Ctrl-Space,
            // Ctrl-[, etc.) are deferred — Phase 57 only ships the
            // shell-essential subset.
            if let Some(c) = ctrl_byte(symbol) {
                writer.write(&[c]);
                return;
            }
        }

        // Printable ASCII or control bytes that flow through.
        if symbol <= 0x7F {
            writer.write(&[symbol as u8]);
        }
    }
}

/// Map an arrow / function-key keysym to its CSI escape sequence.
fn special_key_sequence(symbol: u32) -> Option<&'static [u8]> {
    if symbol == KEYSYM_UP.0 {
        Some(b"\x1b[A")
    } else if symbol == KEYSYM_DOWN.0 {
        Some(b"\x1b[B")
    } else if symbol == KEYSYM_RIGHT.0 {
        Some(b"\x1b[C")
    } else if symbol == KEYSYM_LEFT.0 {
        Some(b"\x1b[D")
    } else {
        None
    }
}

/// Map an ASCII letter codepoint to its Ctrl-modifier byte.
/// `'a' / 'A'` → 0x01, `'b' / 'B'` → 0x02, ..., `'z' / 'Z'` → 0x1A.
///
/// Returns `None` for any non-ASCII keysym (`symbol > 0x7F`) so a
/// private-use codepoint with Ctrl held does not silently truncate
/// to an ASCII control code.
fn ctrl_byte(symbol: u32) -> Option<u8> {
    if symbol > 0x7F {
        return None;
    }
    let c = symbol as u8;
    let lower = c.to_ascii_lowercase();
    if lower.is_ascii_lowercase() {
        Some(lower - b'a' + 1)
    } else {
        None
    }
}

impl Default for InputHandler {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;
    use kernel_core::input::events::{MOD_CTRL, ModifierState};
    use kernel_core::input::keymap::{KEYSYM_DOWN, KEYSYM_LEFT, KEYSYM_RIGHT, KEYSYM_UP};

    struct FakeWriter {
        bytes: Vec<u8>,
    }

    impl FakeWriter {
        fn new() -> Self {
            Self { bytes: Vec::new() }
        }
    }

    impl PtyWriter for FakeWriter {
        fn write(&mut self, bytes: &[u8]) {
            self.bytes.extend_from_slice(bytes);
        }
    }

    fn key_down(symbol: u32, modifiers: u16) -> KeyEvent {
        KeyEvent {
            timestamp_ms: 0,
            keycode: 0,
            symbol,
            modifiers: ModifierState(modifiers),
            kind: KeyEventKind::Down,
        }
    }

    fn key_up(symbol: u32) -> KeyEvent {
        KeyEvent {
            timestamp_ms: 0,
            keycode: 0,
            symbol,
            modifiers: ModifierState(0),
            kind: KeyEventKind::Up,
        }
    }

    /// Phase 57 G.5 acceptance: key-down with a printable ASCII
    /// symbol writes one byte to the PTY.
    #[test]
    fn ascii_down_writes_one_byte() {
        let mut h = InputHandler::new();
        let mut w = FakeWriter::new();
        h.translate(&key_down(b'a' as u32, 0), &mut w);
        assert_eq!(w.bytes, b"a");
    }

    /// Phase 57 G.5 acceptance: key-up does NOT write to the PTY.
    /// Only the down/repeat edge produces input.
    #[test]
    fn key_up_writes_nothing() {
        let mut h = InputHandler::new();
        let mut w = FakeWriter::new();
        h.translate(&key_up(b'a' as u32), &mut w);
        assert!(w.bytes.is_empty());
    }

    /// Phase 57 G.5 acceptance: Ctrl-C → 0x03.
    #[test]
    fn ctrl_c_writes_etx() {
        let mut h = InputHandler::new();
        let mut w = FakeWriter::new();
        h.translate(&key_down(b'c' as u32, MOD_CTRL), &mut w);
        assert_eq!(w.bytes, &[0x03]);
    }

    /// Phase 57 G.5 acceptance: Ctrl-D → 0x04.
    #[test]
    fn ctrl_d_writes_eot() {
        let mut h = InputHandler::new();
        let mut w = FakeWriter::new();
        h.translate(&key_down(b'd' as u32, MOD_CTRL), &mut w);
        assert_eq!(w.bytes, &[0x04]);
    }

    /// Phase 57 G.5 acceptance: arrow keys produce CSI sequences.
    /// Up = ESC [ A, Down = ESC [ B, Right = ESC [ C, Left = ESC [ D.
    #[test]
    fn arrow_up_writes_csi_a() {
        let mut h = InputHandler::new();
        let mut w = FakeWriter::new();
        h.translate(&key_down(KEYSYM_UP.0, 0), &mut w);
        assert_eq!(w.bytes, b"\x1b[A");
    }

    #[test]
    fn arrow_down_writes_csi_b() {
        let mut h = InputHandler::new();
        let mut w = FakeWriter::new();
        h.translate(&key_down(KEYSYM_DOWN.0, 0), &mut w);
        assert_eq!(w.bytes, b"\x1b[B");
    }

    #[test]
    fn arrow_right_writes_csi_c() {
        let mut h = InputHandler::new();
        let mut w = FakeWriter::new();
        h.translate(&key_down(KEYSYM_RIGHT.0, 0), &mut w);
        assert_eq!(w.bytes, b"\x1b[C");
    }

    #[test]
    fn arrow_left_writes_csi_d() {
        let mut h = InputHandler::new();
        let mut w = FakeWriter::new();
        h.translate(&key_down(KEYSYM_LEFT.0, 0), &mut w);
        assert_eq!(w.bytes, b"\x1b[D");
    }

    /// Phase 57 G.5 acceptance: enter (\r) writes a carriage return
    /// — pty layer translates to LF in cooked mode.
    #[test]
    fn enter_writes_cr() {
        let mut h = InputHandler::new();
        let mut w = FakeWriter::new();
        h.translate(&key_down(b'\r' as u32, 0), &mut w);
        assert_eq!(w.bytes, b"\r");
    }

    /// Phase 57 G.5 acceptance: backspace → 0x7F (DEL) so the shell's
    /// cooked-mode line editor erases.
    #[test]
    fn backspace_writes_del() {
        let mut h = InputHandler::new();
        let mut w = FakeWriter::new();
        h.translate(&key_down(0x08, 0), &mut w);
        assert_eq!(w.bytes, &[0x7F]);
    }

    /// Phase 57 G.5 acceptance: a key with an unknown special-key
    /// symbol (private-use codepoint outside the supported set)
    /// writes nothing rather than panicking.
    #[test]
    fn unknown_keysym_writes_nothing() {
        let mut h = InputHandler::new();
        let mut w = FakeWriter::new();
        // Random private-use codepoint.
        h.translate(&key_down(0xE000 + 0xFF, 0), &mut w);
        assert!(w.bytes.is_empty());
    }

    /// Ctrl + non-ASCII keysym must not emit a truncated control byte.
    /// A private-use codepoint whose low byte happens to land in the
    /// ASCII letter range (`0xE061` → low byte `0x61` = `'a'`) used
    /// to silently produce `Ctrl-A` (0x01) before the bound was added.
    #[test]
    fn ctrl_with_non_ascii_keysym_writes_nothing() {
        let mut h = InputHandler::new();
        let mut w = FakeWriter::new();
        h.translate(&key_down(0xE061, MOD_CTRL), &mut w);
        assert!(w.bytes.is_empty());
        // Same shape with a high codepoint whose low byte is `'c'`.
        h.translate(&key_down(0xE063, MOD_CTRL), &mut w);
        assert!(w.bytes.is_empty());
    }

    /// Phase 57 G.5 acceptance: a key-repeat event behaves like a
    /// key-down (autorepeat from kbd_server is the source).
    #[test]
    fn key_repeat_is_treated_like_down() {
        let mut h = InputHandler::new();
        let mut w = FakeWriter::new();
        let mut event = key_down(b'a' as u32, 0);
        event.kind = KeyEventKind::Repeat;
        h.translate(&event, &mut w);
        assert_eq!(w.bytes, b"a");
    }
}
