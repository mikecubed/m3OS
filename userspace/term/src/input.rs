//! Phase 57 Track G.5 — `KeyEvent` → PTY byte translation.
//!
//! Red commit: declares the public seam (`InputHandler::translate`)
//! returning a stub byte slice; the green commit fills in the keymap.

use alloc::vec::Vec;

use kernel_core::input::events::{KeyEvent, KeyEventKind};

/// Pluggable PTY-write seam. Production wraps `syscall_lib::write`;
/// host tests record byte slices.
pub trait PtyWriter {
    fn write(&mut self, bytes: &[u8]);
}

/// Input handler.  Consumes `KeyEvent`s, applies the keymap, writes
/// shell-relevant byte sequences to the PTY.
pub struct InputHandler;

impl InputHandler {
    pub fn new() -> Self {
        Self
    }

    /// Translate one event into PTY bytes; the writer is called once
    /// per event with the bytes (or not at all for events that do not
    /// produce output, such as key-up events or unhandled symbols).
    pub fn translate<W: PtyWriter>(&mut self, _event: &KeyEvent, _writer: &mut W) {
        unimplemented!("G.5 green commit lands this")
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
