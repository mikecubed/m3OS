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

use crate::screen::ViewCmd;
use kernel_core::input::events::{KeyEvent, KeyEventKind, MOD_CTRL, MOD_SHIFT, ModifierState};
use kernel_core::input::keymap::{
    KEYSYM_DOWN, KEYSYM_END, KEYSYM_HOME, KEYSYM_LEFT, KEYSYM_PAGEDOWN, KEYSYM_PAGEUP,
    KEYSYM_RIGHT, KEYSYM_UP,
};

/// Phase 112 Track A.3 — what one translated key did.
///
/// `translate` used to return `()`, which could express neither "this key
/// was consumed locally, write nothing to the PTY" (needed for the
/// Shift+PageUp viewport binds) nor "this key produced PTY bytes" (the
/// snap-to-bottom trigger). `InputHandler` deliberately holds no `Screen`
/// reference — it stays host-testable in isolation — so the outcome is
/// handed back to the main loop, which owns both the screen and the PTY.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyOutcome {
    /// The event produced no output (an `Up` edge, a modifier-only event,
    /// or an unmapped private-use keysym).
    None,
    /// Bytes were written to the PTY. The main loop snaps the scrollback
    /// viewport back to the live tail so the user never types into
    /// history.
    WroteBytes,
    /// A scrollback viewport movement, consumed locally — no PTY bytes.
    View(ViewCmd),
}

/// Phase 112 Track A.3 — a non-printable key's meaning, resolved against
/// the live modifier state.
enum SpecialKey {
    /// A VT100/CSI sequence destined for the PTY.
    Bytes(&'static [u8]),
    /// A scrollback viewport command handled inside `term`.
    View(ViewCmd),
}

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
    ///
    /// Phase 112 Track A.3 — returns a [`KeyOutcome`] so the caller can
    /// distinguish "wrote to the PTY" (snap the scrollback view back to
    /// the live tail) from "consumed locally as a viewport command".
    pub fn translate<W: PtyWriter>(&mut self, event: &KeyEvent, writer: &mut W) -> KeyOutcome {
        // Only down / repeat events produce input.  Up events update
        // modifier state in `kbd_server`; clients see the latched
        // snapshot on the next down event.
        match event.kind {
            KeyEventKind::Down | KeyEventKind::Repeat => {}
            KeyEventKind::Up => return KeyOutcome::None,
        }

        let symbol = event.symbol;
        let modifiers = event.modifiers;

        // Phase 70 — `kbd_server` now surfaces modifier-key edges
        // (`KEY_LCTRL`, `KEY_RSHIFT`, ...) as `KeyEvent`s with
        // `symbol = 0` so DOOM-style "Ctrl is the Fire button" clients
        // can match by keycode. Term has no use for a stand-alone
        // modifier event: the modifier snapshot is folded into the
        // next non-modifier event's `modifiers` field via the chord
        // path below. Drop these here so we don't emit a NUL byte
        // through the `symbol <= 0x7F` clause at the bottom.
        if symbol == 0 {
            return KeyOutcome::None;
        }

        // Special keys live in the private-use area (0xE000+);
        // printable ASCII in [0x20..=0x7E] flows through verbatim.
        match special_key_sequence(symbol, modifiers) {
            Some(SpecialKey::Bytes(seq)) => {
                writer.write(seq);
                return KeyOutcome::WroteBytes;
            }
            Some(SpecialKey::View(cmd)) => return KeyOutcome::View(cmd),
            None => {}
        }

        // Backspace (0x08) → DEL (0x7F).
        if symbol == 0x08 {
            writer.write(&[0x7F]);
            return KeyOutcome::WroteBytes;
        }

        // Ctrl + letter → control code.
        if modifiers.contains(MOD_CTRL) {
            // Ctrl maps the printable letter range A..Z / a..z to
            // 0x01..0x1A.  Other Ctrl-combinations (Ctrl-Space,
            // Ctrl-[, etc.) are deferred — Phase 57 only ships the
            // shell-essential subset.
            if let Some(c) = ctrl_byte(symbol) {
                writer.write(&[c]);
                return KeyOutcome::WroteBytes;
            }
        }

        // Printable ASCII or control bytes that flow through.
        if symbol <= 0x7F {
            writer.write(&[symbol as u8]);
            return KeyOutcome::WroteBytes;
        }
        KeyOutcome::None
    }
}

/// Map an arrow / navigation keysym to its CSI escape sequence, or — for
/// the Shift-modified navigation cluster — to a scrollback viewport
/// command handled inside `term`.
///
/// Phase 112 Track A.3 filled in the PageUp/PageDown/Home/End rows. Before
/// this phase `term` mapped the four arrows only, so plain PageUp/Home/End
/// produced **nothing at all** and paging inside `less`/`htop` was broken.
/// The unshifted sequences below are the same ones
/// `kernel_core::input::hid_poll::key_event_to_stdin` emits for the USB
/// HID path; the `hid_poll_*` tests below pin them together so the two
/// tables cannot drift.
///
/// Shift is the xterm convention for "talk to the terminal, not the
/// application", which is why the viewport binds live there and the
/// unshifted keys still reach the app.
fn special_key_sequence(symbol: u32, modifiers: ModifierState) -> Option<SpecialKey> {
    let shift = modifiers.contains(MOD_SHIFT);
    if symbol == KEYSYM_UP.0 {
        Some(SpecialKey::Bytes(b"\x1b[A"))
    } else if symbol == KEYSYM_DOWN.0 {
        Some(SpecialKey::Bytes(b"\x1b[B"))
    } else if symbol == KEYSYM_RIGHT.0 {
        Some(SpecialKey::Bytes(b"\x1b[C"))
    } else if symbol == KEYSYM_LEFT.0 {
        Some(SpecialKey::Bytes(b"\x1b[D"))
    } else if symbol == KEYSYM_PAGEUP.0 {
        Some(if shift {
            SpecialKey::View(ViewCmd::PageUp)
        } else {
            SpecialKey::Bytes(b"\x1b[5~")
        })
    } else if symbol == KEYSYM_PAGEDOWN.0 {
        Some(if shift {
            SpecialKey::View(ViewCmd::PageDown)
        } else {
            SpecialKey::Bytes(b"\x1b[6~")
        })
    } else if symbol == KEYSYM_HOME.0 {
        Some(if shift {
            SpecialKey::View(ViewCmd::Oldest)
        } else {
            SpecialKey::Bytes(b"\x1b[H")
        })
    } else if symbol == KEYSYM_END.0 {
        Some(if shift {
            SpecialKey::View(ViewCmd::Live)
        } else {
            SpecialKey::Bytes(b"\x1b[F")
        })
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

/// Phase 69 Track G — wrap a paste payload in bracketed-paste
/// markers when the mode is enabled.
///
/// Bracketed-paste protocol:
///   start: `ESC [ 200 ~` (6 bytes)
///   end:   `ESC [ 201 ~` (6 bytes)
///
/// Editors that opt in (`vim`, `nvim`, `emacs`) use the brackets to
/// distinguish typed input from pasted input — pasted text bypasses
/// per-line autoindent and history events.
///
/// When `enabled == false`, returns `payload` verbatim with no
/// brackets.
pub fn wrap_paste(payload: &[u8], enabled: bool) -> alloc::vec::Vec<u8> {
    if !enabled {
        return payload.to_vec();
    }
    let mut out = alloc::vec::Vec::with_capacity(payload.len() + 12);
    out.extend_from_slice(b"\x1b[200~");
    out.extend_from_slice(payload);
    out.extend_from_slice(b"\x1b[201~");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;
    use kernel_core::input::events::{MOD_CTRL, MOD_SHIFT, ModifierState};
    use kernel_core::input::keymap::{
        KEYSYM_DOWN, KEYSYM_END, KEYSYM_HOME, KEYSYM_LEFT, KEYSYM_PAGEDOWN, KEYSYM_PAGEUP,
        KEYSYM_RIGHT, KEYSYM_UP,
    };

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
            modifier_side: kernel_core::input::events::ModifierSide::Either,
        }
    }

    fn key_up(symbol: u32) -> KeyEvent {
        KeyEvent {
            timestamp_ms: 0,
            keycode: 0,
            symbol,
            modifiers: ModifierState(0),
            kind: KeyEventKind::Up,
            modifier_side: kernel_core::input::events::ModifierSide::Either,
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

    /// Phase 69 Track G.1 — `wrap_paste(payload, true)` wraps the
    /// payload in `\x1b[200~ … \x1b[201~`. Empty payload still emits
    /// the brackets so the receiver can distinguish "paste of empty
    /// string" from "no paste".
    #[test]
    fn wrap_paste_wraps_when_enabled() {
        let wrapped = wrap_paste(b"hello", true);
        assert_eq!(wrapped, b"\x1b[200~hello\x1b[201~");

        let empty = wrap_paste(b"", true);
        assert_eq!(empty, b"\x1b[200~\x1b[201~");
    }

    /// Phase 69 Track G.1 — when disabled, `wrap_paste` returns the
    /// payload verbatim.
    #[test]
    fn wrap_paste_passthrough_when_disabled() {
        let raw = wrap_paste(b"hello\nworld", false);
        assert_eq!(raw, b"hello\nworld");
    }

    /// Phase 69 Track G.1 — payload containing the close sequence as
    /// data passes through verbatim; the protocol does not require
    /// in-band escaping. Receivers terminate on the first `\x1b[201~`
    /// they see; the start/end framing is the only guarantee the
    /// helper provides.
    #[test]
    fn wrap_paste_does_not_escape_close_sequence_in_payload() {
        let wrapped = wrap_paste(b"a\x1b[201~b", true);
        assert_eq!(wrapped, b"\x1b[200~a\x1b[201~b\x1b[201~");
    }

    // -----------------------------------------------------------------
    // Phase 112 Track A.3 — key outcomes, page keys, viewport binds
    // -----------------------------------------------------------------

    /// Drive one key and return `(outcome, bytes written)`.
    fn run_key(symbol: u32, modifiers: u16) -> (KeyOutcome, Vec<u8>) {
        let mut h = InputHandler::new();
        let mut w = FakeWriter::new();
        let outcome = h.translate(&key_down(symbol, modifiers), &mut w);
        (outcome, w.bytes)
    }

    /// A.3 acceptance: **unshifted** PageUp/PageDown/Home/End emit the
    /// standard VT sequences. Before Phase 112 these produced no bytes at
    /// all, so `less`/`htop` could not page.
    #[test]
    fn unshifted_page_keys_emit_vt_sequences() {
        let cases: &[(u32, &[u8])] = &[
            (KEYSYM_PAGEUP.0, b"\x1b[5~"),
            (KEYSYM_PAGEDOWN.0, b"\x1b[6~"),
            (KEYSYM_HOME.0, b"\x1b[H"),
            (KEYSYM_END.0, b"\x1b[F"),
        ];
        for (symbol, expected) in cases {
            let (outcome, bytes) = run_key(*symbol, 0);
            assert_eq!(outcome, KeyOutcome::WroteBytes, "symbol {symbol:#x}");
            assert_eq!(&bytes[..], *expected, "symbol {symbol:#x}");
        }
    }

    /// A.3 acceptance: `term`'s unshifted navigation table must stay
    /// byte-identical to the authoritative one in
    /// `kernel_core::input::hid_poll`, which the USB HID path uses. Two
    /// copies of a VT table is exactly the kind of thing that silently
    /// drifts, so pin them together.
    #[test]
    fn unshifted_sequences_match_hid_poll_table() {
        for symbol in [
            KEYSYM_UP.0,
            KEYSYM_DOWN.0,
            KEYSYM_LEFT.0,
            KEYSYM_RIGHT.0,
            KEYSYM_PAGEUP.0,
            KEYSYM_PAGEDOWN.0,
            KEYSYM_HOME.0,
            KEYSYM_END.0,
        ] {
            let mut expected = Vec::new();
            kernel_core::input::hid_poll::key_event_to_stdin(symbol, 0, 0, |b| expected.push(b));
            let (outcome, got) = run_key(symbol, 0);
            assert_eq!(outcome, KeyOutcome::WroteBytes, "symbol {symbol:#x}");
            assert_eq!(got, expected, "term vs hid_poll drift at {symbol:#x}");
        }
    }

    /// A.3 acceptance: Shift + the navigation cluster is consumed locally
    /// as a viewport command and writes **no** PTY bytes.
    #[test]
    fn shift_page_keys_drive_the_viewport_without_pty_bytes() {
        let cases: &[(u32, ViewCmd)] = &[
            (KEYSYM_PAGEUP.0, ViewCmd::PageUp),
            (KEYSYM_PAGEDOWN.0, ViewCmd::PageDown),
            (KEYSYM_HOME.0, ViewCmd::Oldest),
            (KEYSYM_END.0, ViewCmd::Live),
        ];
        for (symbol, cmd) in cases {
            let (outcome, bytes) = run_key(*symbol, MOD_SHIFT);
            assert_eq!(outcome, KeyOutcome::View(*cmd), "symbol {symbol:#x}");
            assert!(bytes.is_empty(), "viewport binds write nothing to the PTY");
        }
    }

    /// A.3 acceptance: Shift+arrow is *not* a viewport bind — the arrows
    /// keep their CSI sequences so shift-select in an app still works.
    #[test]
    fn shift_arrows_still_emit_csi_sequences() {
        let cases: &[(u32, &[u8])] = &[
            (KEYSYM_UP.0, b"\x1b[A"),
            (KEYSYM_DOWN.0, b"\x1b[B"),
            (KEYSYM_RIGHT.0, b"\x1b[C"),
            (KEYSYM_LEFT.0, b"\x1b[D"),
        ];
        for (symbol, expected) in cases {
            let (outcome, bytes) = run_key(*symbol, MOD_SHIFT);
            assert_eq!(outcome, KeyOutcome::WroteBytes);
            assert_eq!(&bytes[..], *expected);
        }
    }

    /// A.3 acceptance: printable keys and Ctrl+letter still report
    /// `WroteBytes` (the snap-to-bottom trigger), and `Up` edges / bare
    /// modifier events report `None`.
    #[test]
    fn outcome_table_for_ordinary_keys() {
        let (outcome, bytes) = run_key(b'a' as u32, 0);
        assert_eq!(outcome, KeyOutcome::WroteBytes);
        assert_eq!(bytes, b"a");

        // Ctrl+C is unchanged: SIGINT's 0x03, not a clipboard bind.
        let (outcome, bytes) = run_key(b'c' as u32, MOD_CTRL);
        assert_eq!(outcome, KeyOutcome::WroteBytes);
        assert_eq!(bytes, &[0x03]);

        // Modifier-only event (symbol 0) writes nothing.
        let (outcome, bytes) = run_key(0, MOD_SHIFT);
        assert_eq!(outcome, KeyOutcome::None);
        assert!(bytes.is_empty());

        // Unmapped private-use keysym writes nothing.
        let (outcome, bytes) = run_key(0xE0FF, 0);
        assert_eq!(outcome, KeyOutcome::None);
        assert!(bytes.is_empty());

        // Up edges never produce output.
        let mut h = InputHandler::new();
        let mut w = FakeWriter::new();
        assert_eq!(
            h.translate(&key_up(b'a' as u32), &mut w),
            KeyOutcome::None,
            "key-up produces no outcome"
        );
        assert!(w.bytes.is_empty());
    }
}
