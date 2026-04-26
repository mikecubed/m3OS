//! Phase 56 Track D.1 — pure-logic keyboard scancode translation, US-QWERTY
//! keymap, modifier tracker, and key-repeat scheduler.
//!
//! Layered, single-concern types — composed at the `kbd_server` shim:
//!
//! * [`ScancodeDecoder`] — translates AT-style **set-1** scancode bytes
//!   (with `0xE0` extended prefixes, `0xE1` pause prefix, and break-code
//!   release semantics) into a stream of `(Keycode, KeyKind)` events. No
//!   modifier or symbol logic — pure mechanical decoding.
//!
//! * [`Keymap`] — pure data: maps `(Keycode, ModifierState)` to a [`KeySym`]
//!   (printable Unicode scalar, or a private-use tag for non-printable
//!   keys: arrows, function keys, navigation, modifiers themselves).
//!
//! * [`ModifierTracker`] — held + locked (CAPS/NUM) modifier tracking with
//!   correct latch semantics. Owns nothing else; consumes `(Keycode,
//!   KeyKind)` and emits a fresh [`ModifierState`] snapshot.
//!
//! * [`KeyRepeatScheduler`] — pure scheduler. Held-key state + tick
//!   timestamp ⇒ optional `KeyEvent { kind: Repeat }`. Bounded held-key
//!   table (8 simultaneously-held non-modifier keys); over-cap evicts
//!   the oldest and surfaces a typed [`KeymapError::HeldKeyTableOverflow`]
//!   so the caller can log a structured warning.
//!
//! All four types are allocation-free, host-testable, and intentionally
//! decoupled. The `kbd_server` userspace shim wires them together: feed
//! kernel scancode bytes into the decoder, push decoded edges into the
//! tracker, look up symbols via the keymap, and ask the scheduler for
//! repeat events on each timer tick.
//!
//! Re-exports [`KeyEvent`], [`KeyEventKind`], and [`ModifierState`] from
//! [`crate::input::events`] — Phase 56 single-source-of-truth discipline.

pub use crate::input::events::{
    KeyEvent, KeyEventKind, MOD_ALL, MOD_ALT, MOD_CAPS, MOD_CTRL, MOD_NUM, MOD_SHIFT, MOD_SUPER,
    ModifierState,
};

// ---------------------------------------------------------------------------
// Public surface — keycodes and key symbols
// ---------------------------------------------------------------------------

/// Hardware-neutral keycode emitted by [`ScancodeDecoder`].
///
/// The numeric range is opaque — the decoder, the keymap, and the modifier
/// tracker all agree on it internally. Consumers should use the named
/// constants (e.g. [`KEY_A`], [`KEY_LSHIFT`], [`KEY_LEFT`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash, Default)]
pub struct Keycode(pub u32);

/// Post-keymap key symbol.
///
/// Printable keys: Unicode scalar value (e.g. `0x61` for lowercase `a`).
/// Non-printable keys: Unicode private-use range — see [`KEYSYM_LEFT`],
/// [`KEYSYM_F1`], etc.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash, Default)]
pub struct KeySym(pub u32);

/// Sentinel: no key symbol.
pub const KEYSYM_NONE: KeySym = KeySym(0);

// ---- Named keycodes -------------------------------------------------------

/// Letter `A` keycode.
pub const KEY_A: Keycode = Keycode(0x0001);
/// Letter `B` keycode.
pub const KEY_B: Keycode = Keycode(0x0002);
/// Letter `C` keycode.
pub const KEY_C: Keycode = Keycode(0x0003);
/// Letter `D` keycode.
pub const KEY_D: Keycode = Keycode(0x0004);
/// Letter `E` keycode.
pub const KEY_E: Keycode = Keycode(0x0005);
/// Letter `F` keycode.
pub const KEY_F: Keycode = Keycode(0x0006);
/// Letter `G` keycode.
pub const KEY_G: Keycode = Keycode(0x0007);
/// Letter `H` keycode.
pub const KEY_H: Keycode = Keycode(0x0008);
/// Letter `I` keycode.
pub const KEY_I: Keycode = Keycode(0x0009);
/// Letter `J` keycode.
pub const KEY_J: Keycode = Keycode(0x000A);
/// Letter `K` keycode.
pub const KEY_K: Keycode = Keycode(0x000B);
/// Letter `L` keycode.
pub const KEY_L: Keycode = Keycode(0x000C);
/// Letter `M` keycode.
pub const KEY_M: Keycode = Keycode(0x000D);
/// Letter `N` keycode.
pub const KEY_N: Keycode = Keycode(0x000E);
/// Letter `O` keycode.
pub const KEY_O: Keycode = Keycode(0x000F);
/// Letter `P` keycode.
pub const KEY_P: Keycode = Keycode(0x0010);
/// Letter `Q` keycode.
pub const KEY_Q: Keycode = Keycode(0x0011);
/// Letter `R` keycode.
pub const KEY_R: Keycode = Keycode(0x0012);
/// Letter `S` keycode.
pub const KEY_S: Keycode = Keycode(0x0013);
/// Letter `T` keycode.
pub const KEY_T: Keycode = Keycode(0x0014);
/// Letter `U` keycode.
pub const KEY_U: Keycode = Keycode(0x0015);
/// Letter `V` keycode.
pub const KEY_V: Keycode = Keycode(0x0016);
/// Letter `W` keycode.
pub const KEY_W: Keycode = Keycode(0x0017);
/// Letter `X` keycode.
pub const KEY_X: Keycode = Keycode(0x0018);
/// Letter `Y` keycode.
pub const KEY_Y: Keycode = Keycode(0x0019);
/// Letter `Z` keycode.
pub const KEY_Z: Keycode = Keycode(0x001A);

/// Digit `1` keycode.
pub const KEY_1: Keycode = Keycode(0x0021);
/// Digit `2` keycode.
pub const KEY_2: Keycode = Keycode(0x0022);
/// Digit `3` keycode.
pub const KEY_3: Keycode = Keycode(0x0023);
/// Digit `4` keycode.
pub const KEY_4: Keycode = Keycode(0x0024);
/// Digit `5` keycode.
pub const KEY_5: Keycode = Keycode(0x0025);
/// Digit `6` keycode.
pub const KEY_6: Keycode = Keycode(0x0026);
/// Digit `7` keycode.
pub const KEY_7: Keycode = Keycode(0x0027);
/// Digit `8` keycode.
pub const KEY_8: Keycode = Keycode(0x0028);
/// Digit `9` keycode.
pub const KEY_9: Keycode = Keycode(0x0029);
/// Digit `0` keycode.
pub const KEY_0: Keycode = Keycode(0x002A);

/// Spacebar keycode.
pub const KEY_SPACE: Keycode = Keycode(0x0040);
/// Return / Enter keycode.
pub const KEY_ENTER: Keycode = Keycode(0x0041);
/// Backspace keycode.
pub const KEY_BACKSPACE: Keycode = Keycode(0x0042);
/// Tab keycode.
pub const KEY_TAB: Keycode = Keycode(0x0043);
/// Escape keycode.
pub const KEY_ESC: Keycode = Keycode(0x0044);

/// `-` / `_` keycode.
pub const KEY_MINUS: Keycode = Keycode(0x0050);
/// `=` / `+` keycode.
pub const KEY_EQUALS: Keycode = Keycode(0x0051);
/// `[` / `{` keycode.
pub const KEY_LBRACKET: Keycode = Keycode(0x0052);
/// `]` / `}` keycode.
pub const KEY_RBRACKET: Keycode = Keycode(0x0053);
/// `\` / `|` keycode.
pub const KEY_BACKSLASH: Keycode = Keycode(0x0054);
/// `;` / `:` keycode.
pub const KEY_SEMICOLON: Keycode = Keycode(0x0055);
/// `'` / `"` keycode.
pub const KEY_APOSTROPHE: Keycode = Keycode(0x0056);
/// `` ` `` / `~` keycode.
pub const KEY_GRAVE: Keycode = Keycode(0x0057);
/// `,` / `<` keycode.
pub const KEY_COMMA: Keycode = Keycode(0x0058);
/// `.` / `>` keycode.
pub const KEY_DOT: Keycode = Keycode(0x0059);
/// `/` / `?` keycode.
pub const KEY_SLASH: Keycode = Keycode(0x005A);

/// Left Shift keycode.
pub const KEY_LSHIFT: Keycode = Keycode(0x0080);
/// Right Shift keycode.
pub const KEY_RSHIFT: Keycode = Keycode(0x0081);
/// Left Ctrl keycode.
pub const KEY_LCTRL: Keycode = Keycode(0x0082);
/// Right Ctrl keycode.
pub const KEY_RCTRL: Keycode = Keycode(0x0083);
/// Left Alt keycode.
pub const KEY_LALT: Keycode = Keycode(0x0084);
/// Right Alt (AltGr) keycode.
pub const KEY_RALT: Keycode = Keycode(0x0085);
/// Left Super (Meta / Windows) keycode.
pub const KEY_LSUPER: Keycode = Keycode(0x0086);
/// Right Super keycode.
pub const KEY_RSUPER: Keycode = Keycode(0x0087);
/// Caps Lock keycode.
pub const KEY_CAPSLOCK: Keycode = Keycode(0x0088);
/// Num Lock keycode.
pub const KEY_NUMLOCK: Keycode = Keycode(0x0089);
/// Scroll Lock keycode (tracked but not bound to a modifier bit).
pub const KEY_SCROLLLOCK: Keycode = Keycode(0x008A);

/// Left arrow keycode.
pub const KEY_LEFT: Keycode = Keycode(0x00A0);
/// Right arrow keycode.
pub const KEY_RIGHT: Keycode = Keycode(0x00A1);
/// Up arrow keycode.
pub const KEY_UP: Keycode = Keycode(0x00A2);
/// Down arrow keycode.
pub const KEY_DOWN: Keycode = Keycode(0x00A3);
/// Home keycode.
pub const KEY_HOME: Keycode = Keycode(0x00A4);
/// End keycode.
pub const KEY_END: Keycode = Keycode(0x00A5);
/// Page Up keycode.
pub const KEY_PAGEUP: Keycode = Keycode(0x00A6);
/// Page Down keycode.
pub const KEY_PAGEDOWN: Keycode = Keycode(0x00A7);
/// Insert keycode.
pub const KEY_INSERT: Keycode = Keycode(0x00A8);
/// Delete keycode.
pub const KEY_DELETE: Keycode = Keycode(0x00A9);

/// `F1` keycode.
pub const KEY_F1: Keycode = Keycode(0x00C0);
/// `F2` keycode.
pub const KEY_F2: Keycode = Keycode(0x00C1);
/// `F3` keycode.
pub const KEY_F3: Keycode = Keycode(0x00C2);
/// `F4` keycode.
pub const KEY_F4: Keycode = Keycode(0x00C3);
/// `F5` keycode.
pub const KEY_F5: Keycode = Keycode(0x00C4);
/// `F6` keycode.
pub const KEY_F6: Keycode = Keycode(0x00C5);
/// `F7` keycode.
pub const KEY_F7: Keycode = Keycode(0x00C6);
/// `F8` keycode.
pub const KEY_F8: Keycode = Keycode(0x00C7);
/// `F9` keycode.
pub const KEY_F9: Keycode = Keycode(0x00C8);
/// `F10` keycode.
pub const KEY_F10: Keycode = Keycode(0x00C9);
/// `F11` keycode.
pub const KEY_F11: Keycode = Keycode(0x00CA);
/// `F12` keycode.
pub const KEY_F12: Keycode = Keycode(0x00CB);

/// Pause / Break keycode (decoded from `E1 1D 45 E1 9D C5`).
pub const KEY_PAUSE: Keycode = Keycode(0x00E0);
/// Print Screen keycode (decoded from `E0 2A E0 37`; release: `E0 B7 E0 AA`).
pub const KEY_PRINTSCREEN: Keycode = Keycode(0x00E1);

// ---- KeySym constants for non-printable keys -----------------------------

const KEYSYM_PRIVATE_BASE: u32 = 0xE000;

/// `KeySym` for the left arrow key.
pub const KEYSYM_LEFT: KeySym = KeySym(KEYSYM_PRIVATE_BASE + 0x10);
/// `KeySym` for the right arrow key.
pub const KEYSYM_RIGHT: KeySym = KeySym(KEYSYM_PRIVATE_BASE + 0x11);
/// `KeySym` for the up arrow key.
pub const KEYSYM_UP: KeySym = KeySym(KEYSYM_PRIVATE_BASE + 0x12);
/// `KeySym` for the down arrow key.
pub const KEYSYM_DOWN: KeySym = KeySym(KEYSYM_PRIVATE_BASE + 0x13);
/// `KeySym` for Home.
pub const KEYSYM_HOME: KeySym = KeySym(KEYSYM_PRIVATE_BASE + 0x14);
/// `KeySym` for End.
pub const KEYSYM_END: KeySym = KeySym(KEYSYM_PRIVATE_BASE + 0x15);
/// `KeySym` for Page Up.
pub const KEYSYM_PAGEUP: KeySym = KeySym(KEYSYM_PRIVATE_BASE + 0x16);
/// `KeySym` for Page Down.
pub const KEYSYM_PAGEDOWN: KeySym = KeySym(KEYSYM_PRIVATE_BASE + 0x17);
/// `KeySym` for Insert.
pub const KEYSYM_INSERT: KeySym = KeySym(KEYSYM_PRIVATE_BASE + 0x18);
/// `KeySym` for Delete.
pub const KEYSYM_DELETE: KeySym = KeySym(KEYSYM_PRIVATE_BASE + 0x19);
/// `KeySym` for Pause / Break.
pub const KEYSYM_PAUSE: KeySym = KeySym(KEYSYM_PRIVATE_BASE + 0x1A);
/// `KeySym` for Print Screen.
pub const KEYSYM_PRINTSCREEN: KeySym = KeySym(KEYSYM_PRIVATE_BASE + 0x1B);
/// `KeySym` for `F1`.
pub const KEYSYM_F1: KeySym = KeySym(KEYSYM_PRIVATE_BASE + 0x40);
/// `KeySym` for `F2`.
pub const KEYSYM_F2: KeySym = KeySym(KEYSYM_PRIVATE_BASE + 0x41);
/// `KeySym` for `F3`.
pub const KEYSYM_F3: KeySym = KeySym(KEYSYM_PRIVATE_BASE + 0x42);
/// `KeySym` for `F4`.
pub const KEYSYM_F4: KeySym = KeySym(KEYSYM_PRIVATE_BASE + 0x43);
/// `KeySym` for `F5`.
pub const KEYSYM_F5: KeySym = KeySym(KEYSYM_PRIVATE_BASE + 0x44);
/// `KeySym` for `F6`.
pub const KEYSYM_F6: KeySym = KeySym(KEYSYM_PRIVATE_BASE + 0x45);
/// `KeySym` for `F7`.
pub const KEYSYM_F7: KeySym = KeySym(KEYSYM_PRIVATE_BASE + 0x46);
/// `KeySym` for `F8`.
pub const KEYSYM_F8: KeySym = KeySym(KEYSYM_PRIVATE_BASE + 0x47);
/// `KeySym` for `F9`.
pub const KEYSYM_F9: KeySym = KeySym(KEYSYM_PRIVATE_BASE + 0x48);
/// `KeySym` for `F10`.
pub const KEYSYM_F10: KeySym = KeySym(KEYSYM_PRIVATE_BASE + 0x49);
/// `KeySym` for `F11`.
pub const KEYSYM_F11: KeySym = KeySym(KEYSYM_PRIVATE_BASE + 0x4A);
/// `KeySym` for `F12`.
pub const KEYSYM_F12: KeySym = KeySym(KEYSYM_PRIVATE_BASE + 0x4B);

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors observable on the keymap public surface.
///
/// `#[non_exhaustive]` so future variants can be added without forcing
/// exhaustive matches at every call site.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum KeymapError {
    /// Held-key table for repeat scheduling is full; oldest was evicted.
    HeldKeyTableOverflow,
    /// Keycode was not bound in the active keymap.
    UnmappedKeycode,
}

// ---------------------------------------------------------------------------
// ScancodeDecoder — set-1 byte stream → (Keycode, KeyKind) edges
// ---------------------------------------------------------------------------

/// Pause-sequence prefix: `E1 1D 45 E1 9D C5`. Six bytes → one
/// `(KEY_PAUSE, Down)` event. There is no separate up edge.
const PAUSE_SEQUENCE: [u8; 6] = [0xE1, 0x1D, 0x45, 0xE1, 0x9D, 0xC5];

/// Print-Screen press sequence: `E0 2A E0 37`.
const PRINTSCREEN_DOWN: [u8; 4] = [0xE0, 0x2A, 0xE0, 0x37];
/// Print-Screen release sequence: `E0 B7 E0 AA`.
const PRINTSCREEN_UP: [u8; 4] = [0xE0, 0xB7, 0xE0, 0xAA];

/// Decoded scancode event surfaced by [`ScancodeDecoder::feed`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DecodedScancode {
    /// A complete `(keycode, kind)` event was decoded.
    Edge {
        /// Hardware-neutral keycode.
        keycode: Keycode,
        /// `Down` for press, `Up` for release.
        kind: KeyEventKind,
    },
    /// One or more bytes were consumed without producing an event yet
    /// (mid-sequence).
    InProgress,
    /// A byte was discarded as part of resync after an invalid prefix.
    /// The decoder always recovers within at most six bytes (the longest
    /// well-formed prefix it tracks — the pause sequence).
    Discarded,
}

/// State machine for AT-style set-1 scancode decoding.
///
/// Allocation-free; internal state is a fixed-size buffer plus a small
/// enum. Safe to drive from any single-threaded caller.
#[derive(Clone, Copy, Debug)]
pub struct ScancodeDecoder {
    state: DecoderState,
    /// Bytes consumed during the current multi-byte sequence (max 6).
    pending: [u8; 6],
    /// Number of bytes currently in `pending`.
    pending_len: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DecoderState {
    /// Awaiting the first byte of a fresh scancode.
    Idle,
    /// Saw `0xE0`; awaiting one more byte (or the start of a print-screen
    /// 4-byte sequence).
    Extended,
    /// Inside the multi-byte print-screen sequence (`E0 2A E0 37` or
    /// `E0 B7 E0 AA`).
    PrintScreen,
    /// Saw `0xE1`; awaiting the rest of the pause sequence.
    Pause,
}

impl Default for ScancodeDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl ScancodeDecoder {
    /// Construct an idle decoder.
    pub const fn new() -> Self {
        Self {
            state: DecoderState::Idle,
            pending: [0; 6],
            pending_len: 0,
        }
    }

    /// Reset the decoder to the idle state.
    pub fn reset(&mut self) {
        self.state = DecoderState::Idle;
        self.pending_len = 0;
    }

    /// Feed one scancode byte. Returns the decoded event (or
    /// `InProgress` / `Discarded`).
    pub fn feed(&mut self, byte: u8) -> DecodedScancode {
        match self.state {
            DecoderState::Idle => self.feed_idle(byte),
            DecoderState::Extended => self.feed_extended(byte),
            DecoderState::PrintScreen => self.feed_printscreen(byte),
            DecoderState::Pause => self.feed_pause(byte),
        }
    }

    fn record(&mut self, byte: u8) {
        if (self.pending_len as usize) < self.pending.len() {
            self.pending[self.pending_len as usize] = byte;
            self.pending_len = self.pending_len.saturating_add(1);
        }
    }

    fn feed_idle(&mut self, byte: u8) -> DecodedScancode {
        match byte {
            0xE0 => {
                self.state = DecoderState::Extended;
                self.pending_len = 0;
                self.record(byte);
                DecodedScancode::InProgress
            }
            0xE1 => {
                self.state = DecoderState::Pause;
                self.pending_len = 0;
                self.record(byte);
                DecodedScancode::InProgress
            }
            // 0x00 is the 8042 "buffer overflow / error" marker; common reply
            // bytes (ACK 0xFA, basic-assurance 0xAA, resend 0xFE, echo 0xEE,
            // device error 0xFF) are also dropped silently.
            0x00 | 0xFF | 0xAA | 0xFA | 0xFE | 0xEE => DecodedScancode::Discarded,
            _ => self.emit_simple_edge(byte),
        }
    }

    fn emit_simple_edge(&mut self, byte: u8) -> DecodedScancode {
        let is_break = (byte & 0x80) != 0;
        let make = byte & 0x7F;
        match map_simple_make(make) {
            Some(keycode) => {
                let kind = if is_break {
                    KeyEventKind::Up
                } else {
                    KeyEventKind::Down
                };
                self.state = DecoderState::Idle;
                self.pending_len = 0;
                DecodedScancode::Edge { keycode, kind }
            }
            None => {
                // Unknown make code — drop and resync.
                self.state = DecoderState::Idle;
                self.pending_len = 0;
                DecodedScancode::Discarded
            }
        }
    }

    fn feed_extended(&mut self, byte: u8) -> DecodedScancode {
        // After E0 we expect either 0x2A / 0xB7 (start of print-screen
        // 4-byte sequence) or a regular extended-key byte.
        match byte {
            0x2A | 0xB7 => {
                self.record(byte);
                self.state = DecoderState::PrintScreen;
                DecodedScancode::InProgress
            }
            _ => {
                let is_break = (byte & 0x80) != 0;
                let make = byte & 0x7F;
                match map_extended_make(make) {
                    Some(keycode) => {
                        self.state = DecoderState::Idle;
                        self.pending_len = 0;
                        let kind = if is_break {
                            KeyEventKind::Up
                        } else {
                            KeyEventKind::Down
                        };
                        DecodedScancode::Edge { keycode, kind }
                    }
                    None => {
                        // Unknown extended scancode — discard and reset.
                        self.state = DecoderState::Idle;
                        self.pending_len = 0;
                        DecodedScancode::Discarded
                    }
                }
            }
        }
    }

    fn feed_printscreen(&mut self, byte: u8) -> DecodedScancode {
        self.record(byte);
        if (self.pending_len as usize) < 4 {
            return DecodedScancode::InProgress;
        }
        let captured = &self.pending[..4];
        if captured == PRINTSCREEN_DOWN {
            self.state = DecoderState::Idle;
            self.pending_len = 0;
            DecodedScancode::Edge {
                keycode: KEY_PRINTSCREEN,
                kind: KeyEventKind::Down,
            }
        } else if captured == PRINTSCREEN_UP {
            self.state = DecoderState::Idle;
            self.pending_len = 0;
            DecodedScancode::Edge {
                keycode: KEY_PRINTSCREEN,
                kind: KeyEventKind::Up,
            }
        } else {
            self.state = DecoderState::Idle;
            self.pending_len = 0;
            DecodedScancode::Discarded
        }
    }

    fn feed_pause(&mut self, byte: u8) -> DecodedScancode {
        self.record(byte);
        if (self.pending_len as usize) < PAUSE_SEQUENCE.len() {
            // Sanity-check the prefix; if it diverges, drop and resync.
            let len = self.pending_len as usize;
            if self.pending[..len] != PAUSE_SEQUENCE[..len] {
                self.state = DecoderState::Idle;
                self.pending_len = 0;
                return DecodedScancode::Discarded;
            }
            return DecodedScancode::InProgress;
        }
        let matched = self.pending[..PAUSE_SEQUENCE.len()] == PAUSE_SEQUENCE;
        self.state = DecoderState::Idle;
        self.pending_len = 0;
        if matched {
            DecodedScancode::Edge {
                keycode: KEY_PAUSE,
                kind: KeyEventKind::Down,
            }
        } else {
            DecodedScancode::Discarded
        }
    }
}

/// Map a 7-bit make code from the simple (non-extended) part of set-1
/// into a hardware-neutral keycode. `None` means the byte is not bound.
pub(crate) fn map_simple_make(make: u8) -> Option<Keycode> {
    Some(match make {
        0x01 => KEY_ESC,
        0x02 => KEY_1,
        0x03 => KEY_2,
        0x04 => KEY_3,
        0x05 => KEY_4,
        0x06 => KEY_5,
        0x07 => KEY_6,
        0x08 => KEY_7,
        0x09 => KEY_8,
        0x0A => KEY_9,
        0x0B => KEY_0,
        0x0C => KEY_MINUS,
        0x0D => KEY_EQUALS,
        0x0E => KEY_BACKSPACE,
        0x0F => KEY_TAB,
        0x10 => KEY_Q,
        0x11 => KEY_W,
        0x12 => KEY_E,
        0x13 => KEY_R,
        0x14 => KEY_T,
        0x15 => KEY_Y,
        0x16 => KEY_U,
        0x17 => KEY_I,
        0x18 => KEY_O,
        0x19 => KEY_P,
        0x1A => KEY_LBRACKET,
        0x1B => KEY_RBRACKET,
        0x1C => KEY_ENTER,
        0x1D => KEY_LCTRL,
        0x1E => KEY_A,
        0x1F => KEY_S,
        0x20 => KEY_D,
        0x21 => KEY_F,
        0x22 => KEY_G,
        0x23 => KEY_H,
        0x24 => KEY_J,
        0x25 => KEY_K,
        0x26 => KEY_L,
        0x27 => KEY_SEMICOLON,
        0x28 => KEY_APOSTROPHE,
        0x29 => KEY_GRAVE,
        0x2A => KEY_LSHIFT,
        0x2B => KEY_BACKSLASH,
        0x2C => KEY_Z,
        0x2D => KEY_X,
        0x2E => KEY_C,
        0x2F => KEY_V,
        0x30 => KEY_B,
        0x31 => KEY_N,
        0x32 => KEY_M,
        0x33 => KEY_COMMA,
        0x34 => KEY_DOT,
        0x35 => KEY_SLASH,
        0x36 => KEY_RSHIFT,
        0x38 => KEY_LALT,
        0x39 => KEY_SPACE,
        0x3A => KEY_CAPSLOCK,
        0x3B => KEY_F1,
        0x3C => KEY_F2,
        0x3D => KEY_F3,
        0x3E => KEY_F4,
        0x3F => KEY_F5,
        0x40 => KEY_F6,
        0x41 => KEY_F7,
        0x42 => KEY_F8,
        0x43 => KEY_F9,
        0x44 => KEY_F10,
        0x45 => KEY_NUMLOCK,
        0x46 => KEY_SCROLLLOCK,
        0x57 => KEY_F11,
        0x58 => KEY_F12,
        _ => return None,
    })
}

/// Map a 7-bit make code that arrived after `0xE0` into a hardware-neutral
/// keycode.
pub(crate) fn map_extended_make(make: u8) -> Option<Keycode> {
    Some(match make {
        0x1D => KEY_RCTRL,
        0x38 => KEY_RALT,
        0x47 => KEY_HOME,
        0x48 => KEY_UP,
        0x49 => KEY_PAGEUP,
        0x4B => KEY_LEFT,
        0x4D => KEY_RIGHT,
        0x4F => KEY_END,
        0x50 => KEY_DOWN,
        0x51 => KEY_PAGEDOWN,
        0x52 => KEY_INSERT,
        0x53 => KEY_DELETE,
        0x5B => KEY_LSUPER,
        0x5C => KEY_RSUPER,
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// ModifierTracker — held + locked state machine
// ---------------------------------------------------------------------------

/// Pure-logic modifier tracker.
///
/// Maintains held state for SHIFT/CTRL/ALT/SUPER (set on `Down`, cleared
/// on `Up`) and lock state for CAPS/NUM (toggled on each `Down` edge of
/// the lock key). Repeats do not change state.
#[derive(Clone, Copy, Debug, Default)]
pub struct ModifierTracker {
    /// Held bits for non-locking modifiers.
    held: u16,
    /// Latched bits for locking modifiers (CAPS, NUM).
    locked: u16,
}

impl ModifierTracker {
    /// Construct an empty tracker.
    pub const fn new() -> Self {
        Self { held: 0, locked: 0 }
    }

    /// Snapshot of the current modifier state.
    pub fn state(&self) -> ModifierState {
        ModifierState((self.held | self.locked) & MOD_ALL)
    }

    /// Apply a keycode edge to the tracker. Returns the post-edge snapshot.
    pub fn apply(&mut self, keycode: Keycode, kind: KeyEventKind) -> ModifierState {
        if let Some(b) = held_bit_for(keycode) {
            match kind {
                KeyEventKind::Down => self.held |= b,
                KeyEventKind::Up => self.held &= !b,
                KeyEventKind::Repeat => {
                    // Repeats don't change held state.
                }
            }
        } else if let Some(b) = lock_bit_for(keycode) {
            // Lock keys toggle on the Down edge only.
            if kind == KeyEventKind::Down {
                self.locked ^= b;
            }
        }
        self.state()
    }

    /// True if any modifier in `mask` is currently held or locked.
    pub fn has(&self, mask: u16) -> bool {
        ((self.held | self.locked) & mask) != 0
    }
}

pub(crate) fn held_bit_for(keycode: Keycode) -> Option<u16> {
    match keycode {
        KEY_LSHIFT | KEY_RSHIFT => Some(MOD_SHIFT),
        KEY_LCTRL | KEY_RCTRL => Some(MOD_CTRL),
        KEY_LALT | KEY_RALT => Some(MOD_ALT),
        KEY_LSUPER | KEY_RSUPER => Some(MOD_SUPER),
        _ => None,
    }
}

pub(crate) fn lock_bit_for(keycode: Keycode) -> Option<u16> {
    match keycode {
        KEY_CAPSLOCK => Some(MOD_CAPS),
        KEY_NUMLOCK => Some(MOD_NUM),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Keymap — pure data, US QWERTY
// ---------------------------------------------------------------------------

/// US-QWERTY keymap.
///
/// Pure data: lookup is `(Keycode, ModifierState) -> KeySym`. Caps Lock
/// affects letter case but not symbols; Shift affects both.
#[derive(Clone, Copy, Debug, Default)]
pub struct Keymap;

impl Keymap {
    /// Construct the US-QWERTY keymap.
    pub const fn us_qwerty() -> Self {
        Self
    }

    /// Look up the symbol for a keycode given the current modifier state.
    pub fn lookup(&self, keycode: Keycode, mods: ModifierState) -> Option<KeySym> {
        let shift = mods.contains(MOD_SHIFT);
        let caps = mods.contains(MOD_CAPS);
        Some(match keycode {
            KEY_A => letter(b'a', shift, caps),
            KEY_B => letter(b'b', shift, caps),
            KEY_C => letter(b'c', shift, caps),
            KEY_D => letter(b'd', shift, caps),
            KEY_E => letter(b'e', shift, caps),
            KEY_F => letter(b'f', shift, caps),
            KEY_G => letter(b'g', shift, caps),
            KEY_H => letter(b'h', shift, caps),
            KEY_I => letter(b'i', shift, caps),
            KEY_J => letter(b'j', shift, caps),
            KEY_K => letter(b'k', shift, caps),
            KEY_L => letter(b'l', shift, caps),
            KEY_M => letter(b'm', shift, caps),
            KEY_N => letter(b'n', shift, caps),
            KEY_O => letter(b'o', shift, caps),
            KEY_P => letter(b'p', shift, caps),
            KEY_Q => letter(b'q', shift, caps),
            KEY_R => letter(b'r', shift, caps),
            KEY_S => letter(b's', shift, caps),
            KEY_T => letter(b't', shift, caps),
            KEY_U => letter(b'u', shift, caps),
            KEY_V => letter(b'v', shift, caps),
            KEY_W => letter(b'w', shift, caps),
            KEY_X => letter(b'x', shift, caps),
            KEY_Y => letter(b'y', shift, caps),
            KEY_Z => letter(b'z', shift, caps),

            // Digits and shifted symbols (US QWERTY top row).
            KEY_1 => sym(b'1', b'!', shift),
            KEY_2 => sym(b'2', b'@', shift),
            KEY_3 => sym(b'3', b'#', shift),
            KEY_4 => sym(b'4', b'$', shift),
            KEY_5 => sym(b'5', b'%', shift),
            KEY_6 => sym(b'6', b'^', shift),
            KEY_7 => sym(b'7', b'&', shift),
            KEY_8 => sym(b'8', b'*', shift),
            KEY_9 => sym(b'9', b'(', shift),
            KEY_0 => sym(b'0', b')', shift),

            KEY_MINUS => sym(b'-', b'_', shift),
            KEY_EQUALS => sym(b'=', b'+', shift),
            KEY_LBRACKET => sym(b'[', b'{', shift),
            KEY_RBRACKET => sym(b']', b'}', shift),
            KEY_BACKSLASH => sym(b'\\', b'|', shift),
            KEY_SEMICOLON => sym(b';', b':', shift),
            KEY_APOSTROPHE => sym(b'\'', b'"', shift),
            KEY_GRAVE => sym(b'`', b'~', shift),
            KEY_COMMA => sym(b',', b'<', shift),
            KEY_DOT => sym(b'.', b'>', shift),
            KEY_SLASH => sym(b'/', b'?', shift),

            KEY_SPACE => KeySym(b' ' as u32),
            KEY_ENTER => KeySym(b'\n' as u32),
            KEY_BACKSPACE => KeySym(0x08),
            KEY_TAB => KeySym(b'\t' as u32),
            KEY_ESC => KeySym(0x1B),

            KEY_LEFT => KEYSYM_LEFT,
            KEY_RIGHT => KEYSYM_RIGHT,
            KEY_UP => KEYSYM_UP,
            KEY_DOWN => KEYSYM_DOWN,
            KEY_HOME => KEYSYM_HOME,
            KEY_END => KEYSYM_END,
            KEY_PAGEUP => KEYSYM_PAGEUP,
            KEY_PAGEDOWN => KEYSYM_PAGEDOWN,
            KEY_INSERT => KEYSYM_INSERT,
            KEY_DELETE => KEYSYM_DELETE,
            KEY_PAUSE => KEYSYM_PAUSE,
            KEY_PRINTSCREEN => KEYSYM_PRINTSCREEN,

            KEY_F1 => KEYSYM_F1,
            KEY_F2 => KEYSYM_F2,
            KEY_F3 => KEYSYM_F3,
            KEY_F4 => KEYSYM_F4,
            KEY_F5 => KEYSYM_F5,
            KEY_F6 => KEYSYM_F6,
            KEY_F7 => KEYSYM_F7,
            KEY_F8 => KEYSYM_F8,
            KEY_F9 => KEYSYM_F9,
            KEY_F10 => KEYSYM_F10,
            KEY_F11 => KEYSYM_F11,
            KEY_F12 => KEYSYM_F12,

            // Modifiers and lock keys carry no character symbol.
            KEY_LSHIFT | KEY_RSHIFT | KEY_LCTRL | KEY_RCTRL | KEY_LALT | KEY_RALT | KEY_LSUPER
            | KEY_RSUPER | KEY_CAPSLOCK | KEY_NUMLOCK | KEY_SCROLLLOCK => return None,
            _ => return None,
        })
    }
}

fn letter(lower: u8, shift: bool, caps: bool) -> KeySym {
    // Shift XOR Caps Lock controls letter case.
    let upper = shift ^ caps;
    let base = if upper {
        lower.to_ascii_uppercase()
    } else {
        lower
    };
    KeySym(base as u32)
}

fn sym(unshifted: u8, shifted: u8, shift: bool) -> KeySym {
    KeySym(if shift { shifted } else { unshifted } as u32)
}

// ---------------------------------------------------------------------------
// KeyRepeatScheduler — pure timer-driven repeat
// ---------------------------------------------------------------------------

/// Default initial delay before a held key starts repeating, in
/// milliseconds.
pub const DEFAULT_REPEAT_INITIAL_DELAY_MS: u64 = 500;

/// Default repeat rate in repeats per second after the initial delay.
pub const DEFAULT_REPEAT_RATE_HZ: u32 = 30;

/// Maximum number of simultaneously-held non-modifier keys tracked by the
/// scheduler. Over-cap evicts the oldest with
/// [`KeymapError::HeldKeyTableOverflow`].
pub const MAX_HELD_KEYS: usize = 8;

/// Pure-logic key-repeat scheduler.
///
/// Inputs are timestamps and keycode edges; outputs are optional repeat
/// edges. Owns no concept of time on its own; the caller drives `tick`
/// with a monotonic millisecond timestamp.
#[derive(Clone, Copy, Debug)]
pub struct KeyRepeatScheduler {
    /// Initial press-to-first-repeat delay in milliseconds.
    initial_delay_ms: u64,
    /// Steady-state interval between repeats in milliseconds.
    repeat_interval_ms: u64,
    /// Held-key table.
    slots: [HeldKeySlot; MAX_HELD_KEYS],
    /// Number of occupied slots (`<= MAX_HELD_KEYS`).
    held_count: u8,
    /// Snapshot of the modifier state at the most recent transition. If
    /// it changes, repeats are cancelled.
    last_mods: ModifierState,
}

#[derive(Clone, Copy, Debug, Default)]
struct HeldKeySlot {
    occupied: bool,
    keycode: Keycode,
    /// Timestamp the key was originally pressed (ms).
    pressed_at_ms: u64,
    /// Timestamp of the most recent emitted edge for this key
    /// (Down or Repeat).
    last_emit_ms: u64,
    /// Insertion index used to evict the oldest slot when the table is
    /// full.
    age: u32,
}

impl Default for KeyRepeatScheduler {
    fn default() -> Self {
        Self::new()
    }
}

impl KeyRepeatScheduler {
    /// Construct with the default delay/rate.
    pub const fn new() -> Self {
        Self::with_config(
            DEFAULT_REPEAT_INITIAL_DELAY_MS,
            DEFAULT_REPEAT_RATE_HZ as u64,
        )
    }

    /// Construct with a custom initial delay (ms) and steady-state rate (Hz).
    pub const fn with_config(initial_delay_ms: u64, rate_hz: u64) -> Self {
        // SAFETY: clamp `rate_hz` to ≥ 1 so the steady-state interval
        // computation cannot panic on a zero divisor — pure const fn,
        // no external inputs.
        let safe_rate = if rate_hz == 0 { 1 } else { rate_hz };
        let repeat_interval_ms = 1000 / safe_rate;
        Self {
            initial_delay_ms,
            repeat_interval_ms,
            slots: [HeldKeySlot {
                occupied: false,
                keycode: Keycode(0),
                pressed_at_ms: 0,
                last_emit_ms: 0,
                age: 0,
            }; MAX_HELD_KEYS],
            held_count: 0,
            last_mods: ModifierState::empty(),
        }
    }

    /// Reset the scheduler. All held-key state is dropped.
    pub fn reset(&mut self) {
        for slot in &mut self.slots {
            slot.occupied = false;
            slot.age = 0;
        }
        self.held_count = 0;
        self.last_mods = ModifierState::empty();
    }

    /// Number of held non-modifier keys currently tracked.
    pub fn held_count(&self) -> usize {
        self.held_count as usize
    }

    /// Currently configured initial delay in milliseconds.
    pub fn initial_delay_ms(&self) -> u64 {
        self.initial_delay_ms
    }

    /// Currently configured steady-state interval in milliseconds.
    pub fn repeat_interval_ms(&self) -> u64 {
        self.repeat_interval_ms
    }

    /// Notify the scheduler of a keycode edge and the current modifier
    /// snapshot. Modifier-key transitions cancel all pending repeats.
    pub fn observe(
        &mut self,
        keycode: Keycode,
        kind: KeyEventKind,
        mods: ModifierState,
        timestamp_ms: u64,
    ) -> Result<(), KeymapError> {
        // Modifier-state changes cancel all pending repeats so chord
        // changes don't see stale repeats.
        if mods != self.last_mods {
            self.cancel_all();
            self.last_mods = mods;
        }

        // Modifier or lock keycodes don't register holds — they cancel
        // any in-flight repeats but never occupy a slot.
        if held_bit_for(keycode).is_some() || lock_bit_for(keycode).is_some() {
            self.cancel_all();
            return Ok(());
        }

        match kind {
            KeyEventKind::Down => self.start_hold(keycode, timestamp_ms),
            KeyEventKind::Up => {
                self.cancel(keycode);
                Ok(())
            }
            KeyEventKind::Repeat => {
                if let Some(slot) = self.find_mut(keycode) {
                    slot.last_emit_ms = timestamp_ms;
                }
                Ok(())
            }
        }
    }

    /// Drive the scheduler with a monotonic timestamp. Returns at most one
    /// repeat event per call; callers should poll until `None` is returned
    /// to drain back-pressure.
    pub fn tick(&mut self, timestamp_ms: u64) -> Option<KeyEvent> {
        let mut best_idx: Option<usize> = None;
        let mut best_due_at: u64 = u64::MAX;
        for (idx, slot) in self.slots.iter().enumerate() {
            if !slot.occupied {
                continue;
            }
            let due_at = self.next_due_ms(slot);
            if timestamp_ms >= due_at && due_at < best_due_at {
                best_due_at = due_at;
                best_idx = Some(idx);
            }
        }
        let idx = best_idx?;
        let slot = &mut self.slots[idx];
        slot.last_emit_ms = timestamp_ms;
        Some(KeyEvent {
            timestamp_ms,
            keycode: slot.keycode.0,
            symbol: 0,
            modifiers: self.last_mods,
            kind: KeyEventKind::Repeat,
        })
    }

    fn next_due_ms(&self, slot: &HeldKeySlot) -> u64 {
        if slot.last_emit_ms == slot.pressed_at_ms {
            slot.pressed_at_ms.saturating_add(self.initial_delay_ms)
        } else {
            slot.last_emit_ms.saturating_add(self.repeat_interval_ms)
        }
    }

    fn cancel_all(&mut self) {
        for slot in &mut self.slots {
            slot.occupied = false;
            slot.age = 0;
        }
        self.held_count = 0;
    }

    fn cancel(&mut self, keycode: Keycode) {
        for slot in &mut self.slots {
            if slot.occupied && slot.keycode == keycode {
                slot.occupied = false;
                slot.age = 0;
                self.held_count = self.held_count.saturating_sub(1);
            }
        }
    }

    fn find_mut(&mut self, keycode: Keycode) -> Option<&mut HeldKeySlot> {
        self.slots
            .iter_mut()
            .find(|s| s.occupied && s.keycode == keycode)
    }

    fn start_hold(&mut self, keycode: Keycode, timestamp_ms: u64) -> Result<(), KeymapError> {
        // De-dup: pressing a key that's already held doesn't double-track.
        if let Some(slot) = self.find_mut(keycode) {
            slot.pressed_at_ms = timestamp_ms;
            slot.last_emit_ms = timestamp_ms;
            return Ok(());
        }

        let mut overflowed = false;
        let new_age = self.next_age();

        let target_idx = match self.slots.iter().position(|s| !s.occupied) {
            Some(i) => i,
            None => {
                overflowed = true;
                self.evict_oldest_idx()
            }
        };

        self.slots[target_idx] = HeldKeySlot {
            occupied: true,
            keycode,
            pressed_at_ms: timestamp_ms,
            last_emit_ms: timestamp_ms,
            age: new_age,
        };
        if !overflowed {
            self.held_count = self.held_count.saturating_add(1);
        }
        if overflowed {
            Err(KeymapError::HeldKeyTableOverflow)
        } else {
            Ok(())
        }
    }

    fn next_age(&self) -> u32 {
        let mut max_age = 0u32;
        for slot in &self.slots {
            if slot.occupied && slot.age > max_age {
                max_age = slot.age;
            }
        }
        max_age.saturating_add(1)
    }

    fn evict_oldest_idx(&self) -> usize {
        let mut oldest = 0usize;
        let mut oldest_age = u32::MAX;
        for (i, slot) in self.slots.iter().enumerate() {
            if slot.occupied && slot.age < oldest_age {
                oldest_age = slot.age;
                oldest = i;
            }
        }
        oldest
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn feed_all(decoder: &mut ScancodeDecoder, bytes: &[u8]) -> alloc::vec::Vec<DecodedScancode> {
        let mut events = alloc::vec::Vec::new();
        for &b in bytes {
            events.push(decoder.feed(b));
        }
        events
    }

    fn last_edge(events: &[DecodedScancode]) -> Option<(Keycode, KeyEventKind)> {
        for e in events.iter().rev() {
            if let DecodedScancode::Edge { keycode, kind } = e {
                return Some((*keycode, *kind));
            }
        }
        None
    }

    // ---- ScancodeDecoder ---------------------------------------------------

    #[test]
    fn decoder_plain_letter_a_down() {
        let mut d = ScancodeDecoder::new();
        let events = feed_all(&mut d, &[0x1E]);
        assert_eq!(last_edge(&events), Some((KEY_A, KeyEventKind::Down)));
    }

    #[test]
    fn decoder_plain_letter_a_up() {
        let mut d = ScancodeDecoder::new();
        let events = feed_all(&mut d, &[0x9E]);
        assert_eq!(last_edge(&events), Some((KEY_A, KeyEventKind::Up)));
    }

    #[test]
    fn decoder_extended_arrow_left() {
        let mut d = ScancodeDecoder::new();
        let events = feed_all(&mut d, &[0xE0, 0x4B]);
        assert_eq!(last_edge(&events), Some((KEY_LEFT, KeyEventKind::Down)));
    }

    #[test]
    fn decoder_extended_arrow_left_up() {
        let mut d = ScancodeDecoder::new();
        let events = feed_all(&mut d, &[0xE0, 0xCB]);
        assert_eq!(last_edge(&events), Some((KEY_LEFT, KeyEventKind::Up)));
    }

    #[test]
    fn decoder_pause_sequence_emits_pause_down() {
        let mut d = ScancodeDecoder::new();
        let events = feed_all(&mut d, &[0xE1, 0x1D, 0x45, 0xE1, 0x9D, 0xC5]);
        assert_eq!(last_edge(&events), Some((KEY_PAUSE, KeyEventKind::Down)));
    }

    #[test]
    fn decoder_printscreen_down_sequence() {
        let mut d = ScancodeDecoder::new();
        let events = feed_all(&mut d, &[0xE0, 0x2A, 0xE0, 0x37]);
        assert_eq!(
            last_edge(&events),
            Some((KEY_PRINTSCREEN, KeyEventKind::Down))
        );
    }

    #[test]
    fn decoder_printscreen_up_sequence() {
        let mut d = ScancodeDecoder::new();
        let events = feed_all(&mut d, &[0xE0, 0xB7, 0xE0, 0xAA]);
        assert_eq!(
            last_edge(&events),
            Some((KEY_PRINTSCREEN, KeyEventKind::Up))
        );
    }

    #[test]
    fn decoder_unknown_extended_byte_resyncs() {
        let mut d = ScancodeDecoder::new();
        let _ = feed_all(&mut d, &[0xE0, 0x00]);
        let events = feed_all(&mut d, &[0x1E]);
        assert_eq!(last_edge(&events), Some((KEY_A, KeyEventKind::Down)));
    }

    #[test]
    fn decoder_recovers_after_arbitrary_garbage() {
        let mut d = ScancodeDecoder::new();
        for b in [0x00, 0xFF, 0xAA, 0xFE, 0xEE, 0xFA] {
            let _ = d.feed(b);
        }
        let events = feed_all(&mut d, &[0x1E]);
        assert_eq!(last_edge(&events), Some((KEY_A, KeyEventKind::Down)));
    }

    // ---- Keymap ------------------------------------------------------------

    #[test]
    fn keymap_plain_letter_a_lowercase() {
        let km = Keymap::us_qwerty();
        let sym = km.lookup(KEY_A, ModifierState::empty()).expect("mapped");
        assert_eq!(sym, KeySym(b'a' as u32));
    }

    #[test]
    fn keymap_shifted_letter_a_uppercase() {
        let km = Keymap::us_qwerty();
        let sym = km.lookup(KEY_A, ModifierState(MOD_SHIFT)).expect("mapped");
        assert_eq!(sym, KeySym(b'A' as u32));
    }

    #[test]
    fn keymap_caps_lock_only_uppercases_letters() {
        let km = Keymap::us_qwerty();
        let mods = ModifierState(MOD_CAPS);
        assert_eq!(km.lookup(KEY_A, mods), Some(KeySym(b'A' as u32)));
        assert_eq!(km.lookup(KEY_1, mods), Some(KeySym(b'1' as u32)));
    }

    #[test]
    fn keymap_shift_xor_caps_for_letters() {
        let km = Keymap::us_qwerty();
        let both = ModifierState(MOD_SHIFT | MOD_CAPS);
        assert_eq!(km.lookup(KEY_A, both), Some(KeySym(b'a' as u32)));
    }

    #[test]
    fn keymap_arrow_left_resolves_to_keysym_left() {
        let km = Keymap::us_qwerty();
        let sym = km.lookup(KEY_LEFT, ModifierState::empty()).expect("mapped");
        assert_eq!(sym, KEYSYM_LEFT);
    }

    #[test]
    fn keymap_shifted_digit_emits_top_row_symbol() {
        let km = Keymap::us_qwerty();
        let mods = ModifierState(MOD_SHIFT);
        assert_eq!(km.lookup(KEY_1, mods), Some(KeySym(b'!' as u32)));
        assert_eq!(km.lookup(KEY_4, mods), Some(KeySym(b'$' as u32)));
    }

    #[test]
    fn keymap_modifier_keycodes_have_no_symbol() {
        let km = Keymap::us_qwerty();
        assert_eq!(km.lookup(KEY_LSHIFT, ModifierState::empty()), None);
        assert_eq!(km.lookup(KEY_CAPSLOCK, ModifierState::empty()), None);
    }

    // ---- ModifierTracker ---------------------------------------------------

    #[test]
    fn modifier_tracker_starts_empty() {
        let t = ModifierTracker::new();
        assert_eq!(t.state(), ModifierState::empty());
    }

    #[test]
    fn modifier_tracker_shift_tap_tracks_held_state() {
        let mut t = ModifierTracker::new();
        let st = t.apply(KEY_LSHIFT, KeyEventKind::Down);
        assert!(st.contains(MOD_SHIFT));
        let st = t.apply(KEY_LSHIFT, KeyEventKind::Up);
        assert!(!st.contains(MOD_SHIFT));
    }

    #[test]
    fn modifier_tracker_shift_hold_persists_across_other_keys() {
        let mut t = ModifierTracker::new();
        t.apply(KEY_LSHIFT, KeyEventKind::Down);
        t.apply(KEY_A, KeyEventKind::Down);
        t.apply(KEY_A, KeyEventKind::Up);
        assert!(t.state().contains(MOD_SHIFT));
        t.apply(KEY_LSHIFT, KeyEventKind::Up);
        assert_eq!(t.state(), ModifierState::empty());
    }

    #[test]
    fn modifier_tracker_caps_lock_toggles_on_each_press() {
        let mut t = ModifierTracker::new();
        let st = t.apply(KEY_CAPSLOCK, KeyEventKind::Down);
        assert!(st.contains(MOD_CAPS));
        let st = t.apply(KEY_CAPSLOCK, KeyEventKind::Up);
        assert!(st.contains(MOD_CAPS));
        let st = t.apply(KEY_CAPSLOCK, KeyEventKind::Down);
        assert!(!st.contains(MOD_CAPS));
    }

    #[test]
    fn modifier_tracker_num_lock_toggles_on_each_press() {
        let mut t = ModifierTracker::new();
        let st = t.apply(KEY_NUMLOCK, KeyEventKind::Down);
        assert!(st.contains(MOD_NUM));
        let st = t.apply(KEY_NUMLOCK, KeyEventKind::Down);
        assert!(!st.contains(MOD_NUM));
    }

    #[test]
    fn modifier_tracker_concurrent_modifiers_compose() {
        let mut t = ModifierTracker::new();
        t.apply(KEY_LSHIFT, KeyEventKind::Down);
        t.apply(KEY_LCTRL, KeyEventKind::Down);
        t.apply(KEY_LALT, KeyEventKind::Down);
        let st = t.state();
        assert!(st.contains(MOD_SHIFT));
        assert!(st.contains(MOD_CTRL));
        assert!(st.contains(MOD_ALT));
    }

    #[test]
    fn modifier_tracker_left_and_right_shift_share_bit() {
        // The wire format has one bit per modifier family — left and
        // right shifts share the SHIFT bit. Releasing one shift clears
        // the bit even if the other physical key remains pressed. Full
        // L/R chord tracking is deferred.
        let mut t = ModifierTracker::new();
        t.apply(KEY_LSHIFT, KeyEventKind::Down);
        t.apply(KEY_RSHIFT, KeyEventKind::Down);
        t.apply(KEY_LSHIFT, KeyEventKind::Up);
        assert!(!t.state().contains(MOD_SHIFT));
    }

    // ---- KeyRepeatScheduler ------------------------------------------------

    #[test]
    fn scheduler_no_repeat_until_initial_delay_elapses() {
        let mut s = KeyRepeatScheduler::new();
        let mods = ModifierState::empty();
        s.observe(KEY_A, KeyEventKind::Down, mods, 0)
            .expect("observe");
        assert!(s.tick(DEFAULT_REPEAT_INITIAL_DELAY_MS - 1).is_none());
        let ev = s.tick(DEFAULT_REPEAT_INITIAL_DELAY_MS).expect("repeat");
        assert_eq!(ev.kind, KeyEventKind::Repeat);
        assert_eq!(ev.keycode, KEY_A.0);
    }

    #[test]
    fn scheduler_emits_at_steady_rate_after_initial_delay() {
        let mut s = KeyRepeatScheduler::new();
        let mods = ModifierState::empty();
        s.observe(KEY_A, KeyEventKind::Down, mods, 0)
            .expect("observe");
        let _ = s.tick(DEFAULT_REPEAT_INITIAL_DELAY_MS).expect("first");
        let interval = s.repeat_interval_ms();
        assert!(interval > 0);
        assert!(
            s.tick(DEFAULT_REPEAT_INITIAL_DELAY_MS + interval - 1)
                .is_none()
        );
        assert!(s.tick(DEFAULT_REPEAT_INITIAL_DELAY_MS + interval).is_some());
    }

    #[test]
    fn scheduler_releasing_key_cancels_repeat() {
        let mut s = KeyRepeatScheduler::new();
        let mods = ModifierState::empty();
        s.observe(KEY_A, KeyEventKind::Down, mods, 0).expect("down");
        s.observe(KEY_A, KeyEventKind::Up, mods, 100).expect("up");
        assert!(s.tick(DEFAULT_REPEAT_INITIAL_DELAY_MS).is_none());
        assert_eq!(s.held_count(), 0);
    }

    #[test]
    fn scheduler_modifier_change_cancels_pending_repeats() {
        let mut s = KeyRepeatScheduler::new();
        s.observe(KEY_A, KeyEventKind::Down, ModifierState::empty(), 0)
            .expect("down");
        s.observe(KEY_A, KeyEventKind::Down, ModifierState(MOD_SHIFT), 10)
            .expect("down with shift");
        assert!(s.tick(DEFAULT_REPEAT_INITIAL_DELAY_MS).is_none());
        assert!(s.tick(10 + DEFAULT_REPEAT_INITIAL_DELAY_MS).is_some());
    }

    #[test]
    fn scheduler_overcap_evicts_oldest() {
        let mut s = KeyRepeatScheduler::new();
        let mods = ModifierState::empty();
        for (i, k) in [KEY_A, KEY_B, KEY_C, KEY_D, KEY_E, KEY_F, KEY_G, KEY_H]
            .iter()
            .enumerate()
        {
            s.observe(*k, KeyEventKind::Down, mods, i as u64)
                .expect("ok");
        }
        assert_eq!(s.held_count(), MAX_HELD_KEYS);
        let r = s.observe(KEY_I, KeyEventKind::Down, mods, 99);
        assert_eq!(r, Err(KeymapError::HeldKeyTableOverflow));
        assert_eq!(s.held_count(), MAX_HELD_KEYS);
    }

    #[test]
    fn scheduler_multiple_held_keys_repeat_independently() {
        let mut s = KeyRepeatScheduler::new();
        let mods = ModifierState::empty();
        s.observe(KEY_A, KeyEventKind::Down, mods, 0).expect("a");
        s.observe(KEY_B, KeyEventKind::Down, mods, 0).expect("b");
        let first = s.tick(DEFAULT_REPEAT_INITIAL_DELAY_MS).expect("first");
        let second = s.tick(DEFAULT_REPEAT_INITIAL_DELAY_MS).expect("second");
        let codes = [first.keycode, second.keycode];
        assert!(codes.contains(&KEY_A.0));
        assert!(codes.contains(&KEY_B.0));
    }

    #[test]
    fn scheduler_modifier_keycodes_do_not_register_holds() {
        let mut s = KeyRepeatScheduler::new();
        s.observe(KEY_LSHIFT, KeyEventKind::Down, ModifierState(MOD_SHIFT), 0)
            .expect("modifier");
        assert_eq!(s.held_count(), 0);
        assert!(s.tick(10_000).is_none());
    }

    // ---- Property tests ----------------------------------------------------

    proptest! {
        #[test]
        fn prop_decoder_does_not_panic_on_garbage(bytes in prop::collection::vec(any::<u8>(), 0..1024)) {
            let mut d = ScancodeDecoder::new();
            for b in bytes {
                let _ = d.feed(b);
            }
        }

        #[test]
        fn prop_decoder_recovers_within_bounded_bytes(
            garbage in prop::collection::vec(any::<u8>(), 0..32),
        ) {
            let mut d = ScancodeDecoder::new();
            for b in garbage {
                let _ = d.feed(b);
            }
            // After at most six bytes of "no-op" filler (longest tracked
            // prefix is the 6-byte pause sequence), the decoder must
            // recover. Feed six garbage bytes then a clean A-Down.
            for _ in 0..6 {
                let _ = d.feed(0x00);
            }
            let mut saw_a = false;
            for b in [0x1E] {
                if let DecodedScancode::Edge { keycode, kind } = d.feed(b) {
                    if keycode == KEY_A && kind == KeyEventKind::Down {
                        saw_a = true;
                    }
                }
            }
            prop_assert!(saw_a, "decoder must recover and decode A-Down after garbage");
        }

        #[test]
        fn prop_decoder_progress_on_valid_make_codes(byte in 0x01u8..=0x58u8) {
            let mut d = ScancodeDecoder::new();
            let ev = d.feed(byte);
            let ok = matches!(ev, DecodedScancode::Edge { .. } | DecodedScancode::Discarded);
            prop_assert!(ok);
        }

        #[test]
        fn prop_modifier_tracker_state_bits_within_mod_all(
            stream in prop::collection::vec((any::<u8>(), any::<bool>()), 0..256)
        ) {
            let mut t = ModifierTracker::new();
            let codes = [
                KEY_LSHIFT, KEY_RSHIFT, KEY_LCTRL, KEY_RCTRL, KEY_LALT, KEY_RALT,
                KEY_LSUPER, KEY_RSUPER, KEY_CAPSLOCK, KEY_NUMLOCK, KEY_A, KEY_1,
            ];
            for (sel, is_down) in stream {
                let kc = codes[(sel as usize) % codes.len()];
                let kind = if is_down { KeyEventKind::Down } else { KeyEventKind::Up };
                let st = t.apply(kc, kind);
                prop_assert_eq!(st.bits() & !MOD_ALL, 0);
            }
        }

        #[test]
        fn prop_scheduler_held_count_bounded(
            stream in prop::collection::vec((0u32..32u32, any::<bool>(), any::<u64>()), 0..256)
        ) {
            let mut s = KeyRepeatScheduler::new();
            let mods = ModifierState::empty();
            for (k, is_down, ts) in stream {
                let kc = Keycode(k);
                let kind = if is_down { KeyEventKind::Down } else { KeyEventKind::Up };
                let _ = s.observe(kc, kind, mods, ts);
                prop_assert!(s.held_count() <= MAX_HELD_KEYS);
            }
        }
    }
}
