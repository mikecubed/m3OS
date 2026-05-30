//! HID Boot-Protocol decode core (Phase 78c Track A).
//!
//! Pure logic: no syscalls, no hardware, no IPC. This module provides
//! stateless conversion helpers and a stateful boot-keyboard decoder for the
//! two HID boot-class devices defined in the USB HID specification Appendix B:
//!
//! * **Boot Keyboard** (subclass 1, protocol 1) — 8-byte interrupt-IN reports:
//!   `[modifier][reserved][usage0]..[usage5]`. The report is a *snapshot* of
//!   all currently-held keys, so edges (press/release) are derived by diffing
//!   against the previous report. See [`BootKeyboardDecoder`].
//!
//! * **Boot Mouse** (subclass 1, protocol 2) — at-minimum-3-byte reports:
//!   `[buttons][dx i8][dy i8]`. See [`parse_boot_mouse_report`].
//!
//! # Key-space contract
//!
//! [`hid_usage_to_keycode`] maps HID Usage Page 0x07 (Keyboard/Keypad) usage
//! IDs to the hardware-neutral `Keycode` values defined in
//! [`crate::input::keymap`]. The consumer (`usb-hid`) will later look up
//! symbols via [`crate::input::keymap::Keymap`] and fill [`KeyEvent::symbol`];
//! this module is deliberately keymap-free.
//!
//! # Modifier-to-keycode mapping
//!
//! The modifier byte bit-positions defined by the HID boot protocol are:
//!
//! | Bit | HID name  | Keycode constant |
//! |-----|-----------|-----------------|
//! | 0   | Left Ctrl  | `KEY_LCTRL`    |
//! | 1   | Left Shift | `KEY_LSHIFT`   |
//! | 2   | Left Alt   | `KEY_LALT`     |
//! | 3   | Left GUI   | `KEY_LSUPER`   |
//! | 4   | Right Ctrl | `KEY_RCTRL`    |
//! | 5   | Right Shift| `KEY_RSHIFT`   |
//! | 6   | Right Alt  | `KEY_RALT`     |
//! | 7   | Right GUI  | `KEY_RSUPER`   |

extern crate alloc;

use crate::input::events::{KeyEventKind, MOD_ALT, MOD_CTRL, MOD_SHIFT, MOD_SUPER, ModifierState};
use crate::input::keymap::{
    KEY_0, KEY_1, KEY_2, KEY_3, KEY_4, KEY_5, KEY_6, KEY_7, KEY_8, KEY_9, KEY_A, KEY_APOSTROPHE,
    KEY_B, KEY_BACKSLASH, KEY_BACKSPACE, KEY_C, KEY_CAPSLOCK, KEY_COMMA, KEY_D, KEY_DELETE,
    KEY_DOT, KEY_DOWN, KEY_E, KEY_END, KEY_ENTER, KEY_EQUALS, KEY_ESC, KEY_F, KEY_F1, KEY_F2,
    KEY_F3, KEY_F4, KEY_F5, KEY_F6, KEY_F7, KEY_F8, KEY_F9, KEY_F10, KEY_F11, KEY_F12, KEY_G,
    KEY_GRAVE, KEY_H, KEY_HOME, KEY_I, KEY_INSERT, KEY_J, KEY_K, KEY_L, KEY_LALT, KEY_LBRACKET,
    KEY_LCTRL, KEY_LEFT, KEY_LSHIFT, KEY_LSUPER, KEY_M, KEY_MINUS, KEY_N, KEY_NUMLOCK, KEY_O,
    KEY_P, KEY_PAGEDOWN, KEY_PAGEUP, KEY_PAUSE, KEY_PRINTSCREEN, KEY_Q, KEY_R, KEY_RALT,
    KEY_RBRACKET, KEY_RCTRL, KEY_RIGHT, KEY_RSHIFT, KEY_RSUPER, KEY_S, KEY_SCROLLLOCK,
    KEY_SEMICOLON, KEY_SLASH, KEY_SPACE, KEY_T, KEY_TAB, KEY_U, KEY_UP, KEY_V, KEY_W, KEY_X, KEY_Y,
    KEY_Z,
};

// ---------------------------------------------------------------------------
// Public constants
// ---------------------------------------------------------------------------

/// Boot keyboard report length (modifier + reserved + 6 usage IDs).
///
/// USB HID specification §B.1: the boot-keyboard report is exactly 8 bytes:
/// `[modifier_byte][reserved][usage0]..[usage5]`.
pub const HID_KBD_REPORT_LEN: usize = 8;

/// Minimum boot mouse report length (buttons + dx + dy).
///
/// USB HID specification §B.2: the first three bytes are `[buttons][dx i8]
/// [dy i8]`. Trailing bytes (e.g. a wheel byte) are ignored by this decoder.
pub const HID_MOUSE_REPORT_MIN_LEN: usize = 3;

// ---------------------------------------------------------------------------
// Modifier bit-to-keycode table
// ---------------------------------------------------------------------------

/// Mapping from modifier-byte bit index (0..=7) to the corresponding
/// hardware-neutral keycode value (as a raw `u32` for the HID layer).
///
/// HID boot-keyboard modifier byte (USB HID §B.1 Table B-1):
/// bit 0 = Left Ctrl, bit 1 = Left Shift, bit 2 = Left Alt, bit 3 = Left GUI,
/// bit 4 = Right Ctrl, bit 5 = Right Shift, bit 6 = Right Alt, bit 7 = Right GUI.
const MODIFIER_KEYCODES: [u32; 8] = [
    KEY_LCTRL.0,  // bit 0
    KEY_LSHIFT.0, // bit 1
    KEY_LALT.0,   // bit 2
    KEY_LSUPER.0, // bit 3
    KEY_RCTRL.0,  // bit 4
    KEY_RSHIFT.0, // bit 5
    KEY_RALT.0,   // bit 6
    KEY_RSUPER.0, // bit 7
];

// ---------------------------------------------------------------------------
// hid_usage_to_keycode
// ---------------------------------------------------------------------------

/// Map a HID Usage ID (Usage Page 0x07, Keyboard/Keypad) to a kernel-core
/// hardware-neutral keycode value.
///
/// Returns `None` for:
/// * Usage 0x00 — "No event indicated".
/// * Usages 0x01..=0x03 — rollover and POST-fail error codes that appear in
///   the key-array when the device cannot report all held keys reliably.
/// * Any usage ID not covered by this table (reserved or out-of-range).
///
/// The returned `u32` is the `.0` field of the [`crate::input::keymap::Keycode`]
/// type; the consumer can store it directly in [`crate::input::events::KeyEvent::keycode`].
pub fn hid_usage_to_keycode(usage: u8) -> Option<u32> {
    // Usages 0x00–0x03 are reserved / error codes; never map them.
    if usage <= 0x03 {
        return None;
    }
    Some(match usage {
        // --- Letters (HID §10, Table 12: 0x04–0x1D) ---
        0x04 => KEY_A.0,
        0x05 => KEY_B.0,
        0x06 => KEY_C.0,
        0x07 => KEY_D.0,
        0x08 => KEY_E.0,
        0x09 => KEY_F.0,
        0x0A => KEY_G.0,
        0x0B => KEY_H.0,
        0x0C => KEY_I.0,
        0x0D => KEY_J.0,
        0x0E => KEY_K.0,
        0x0F => KEY_L.0,
        0x10 => KEY_M.0,
        0x11 => KEY_N.0,
        0x12 => KEY_O.0,
        0x13 => KEY_P.0,
        0x14 => KEY_Q.0,
        0x15 => KEY_R.0,
        0x16 => KEY_S.0,
        0x17 => KEY_T.0,
        0x18 => KEY_U.0,
        0x19 => KEY_V.0,
        0x1A => KEY_W.0,
        0x1B => KEY_X.0,
        0x1C => KEY_Y.0,
        0x1D => KEY_Z.0,

        // --- Digits (0x1E–0x27) ---
        0x1E => KEY_1.0,
        0x1F => KEY_2.0,
        0x20 => KEY_3.0,
        0x21 => KEY_4.0,
        0x22 => KEY_5.0,
        0x23 => KEY_6.0,
        0x24 => KEY_7.0,
        0x25 => KEY_8.0,
        0x26 => KEY_9.0,
        0x27 => KEY_0.0,

        // --- Common non-printable / editing keys ---
        0x28 => KEY_ENTER.0,
        0x29 => KEY_ESC.0,
        0x2A => KEY_BACKSPACE.0,
        0x2B => KEY_TAB.0,
        0x2C => KEY_SPACE.0,

        // --- Punctuation / symbols ---
        0x2D => KEY_MINUS.0,
        0x2E => KEY_EQUALS.0,
        0x2F => KEY_LBRACKET.0,
        0x30 => KEY_RBRACKET.0,
        0x31 => KEY_BACKSLASH.0,
        // 0x32 = Non-US # / ~ (ISO keyboard); no keymap constant — skip.
        0x33 => KEY_SEMICOLON.0,
        0x34 => KEY_APOSTROPHE.0,
        0x35 => KEY_GRAVE.0,
        0x36 => KEY_COMMA.0,
        0x37 => KEY_DOT.0,
        0x38 => KEY_SLASH.0,

        // --- Lock keys ---
        0x39 => KEY_CAPSLOCK.0,

        // --- Function keys (0x3A–0x45) ---
        0x3A => KEY_F1.0,
        0x3B => KEY_F2.0,
        0x3C => KEY_F3.0,
        0x3D => KEY_F4.0,
        0x3E => KEY_F5.0,
        0x3F => KEY_F6.0,
        0x40 => KEY_F7.0,
        0x41 => KEY_F8.0,
        0x42 => KEY_F9.0,
        0x43 => KEY_F10.0,
        0x44 => KEY_F11.0,
        0x45 => KEY_F12.0,

        // --- System / navigation keys ---
        0x46 => KEY_PRINTSCREEN.0,
        0x47 => KEY_SCROLLLOCK.0,
        0x48 => KEY_PAUSE.0,
        0x49 => KEY_INSERT.0,
        0x4A => KEY_HOME.0,
        0x4B => KEY_PAGEUP.0,
        0x4C => KEY_DELETE.0,
        0x4D => KEY_END.0,
        0x4E => KEY_PAGEDOWN.0,
        0x4F => KEY_RIGHT.0,
        0x50 => KEY_LEFT.0,
        0x51 => KEY_DOWN.0,
        0x52 => KEY_UP.0,

        // --- Keypad Num Lock / Clear ---
        0x53 => KEY_NUMLOCK.0,

        // Everything else is unmapped in this table.
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// hid_modifiers_to_state
// ---------------------------------------------------------------------------

/// Decode the boot-keyboard modifier byte (byte 0 of the report) into a
/// [`ModifierState`] bitmask.
///
/// The HID boot-protocol modifier byte combines left and right variants of
/// each modifier under a single logical bit in [`ModifierState`]:
///
/// * Bits 0 (LCtrl) and 4 (RCtrl) → [`MOD_CTRL`]
/// * Bits 1 (LShift) and 5 (RShift) → [`MOD_SHIFT`]
/// * Bits 2 (LAlt) and 6 (RAlt) → [`MOD_ALT`]
/// * Bits 3 (LGUI) and 7 (RGUI) → [`MOD_SUPER`]
///
/// Both sides set the same logical bit; the side distinction is available
/// through the modifier-bit edges emitted by [`BootKeyboardDecoder::decode`].
pub fn hid_modifiers_to_state(modifier_byte: u8) -> ModifierState {
    let mut bits: u16 = 0;
    // Ctrl: LCtrl (bit 0) | RCtrl (bit 4).
    if modifier_byte & 0b0001_0001 != 0 {
        bits |= MOD_CTRL;
    }
    // Shift: LShift (bit 1) | RShift (bit 5).
    if modifier_byte & 0b0010_0010 != 0 {
        bits |= MOD_SHIFT;
    }
    // Alt: LAlt (bit 2) | RAlt (bit 6).
    if modifier_byte & 0b0100_0100 != 0 {
        bits |= MOD_ALT;
    }
    // Super/GUI: LGUI (bit 3) | RGUI (bit 7).
    if modifier_byte & 0b1000_1000 != 0 {
        bits |= MOD_SUPER;
    }
    ModifierState(bits)
}

// ---------------------------------------------------------------------------
// KeyEdge
// ---------------------------------------------------------------------------

/// One decoded press/release edge produced by [`BootKeyboardDecoder`].
///
/// The `keycode` field holds the raw `u32` value of a
/// [`crate::input::keymap::Keycode`]. The consumer (`usb-hid`) enriches this
/// into a full [`crate::input::events::KeyEvent`] by calling
/// [`crate::input::keymap::Keymap::lookup`] for the symbol and
/// [`crate::input::events::ModifierSide::for_keycode`] for the side.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct KeyEdge {
    /// Hardware-neutral keycode (raw `Keycode` value).
    pub keycode: u32,
    /// Edge direction: [`KeyEventKind::Down`] for newly pressed,
    /// [`KeyEventKind::Up`] for newly released.
    pub kind: KeyEventKind,
    /// Modifier snapshot *after* applying this report (from byte 0 of the
    /// report; reflects the state at the time of the edge).
    pub modifiers: ModifierState,
}

// ---------------------------------------------------------------------------
// BootKeyboardDecoder
// ---------------------------------------------------------------------------

/// Stateful boot-keyboard report decoder.
///
/// Boot keyboard reports are *snapshots* of all currently held keys, so edges
/// (key-down / key-up) are derived by comparing each new report against the
/// previous one. This decoder maintains that previous report internally.
///
/// # Usage
///
/// ```rust,ignore
/// let mut decoder = BootKeyboardDecoder::new();
/// let mut edges = alloc::vec::Vec::new();
/// decoder.decode(&report, &mut edges);
/// for edge in &edges { /* ... */ }
/// ```
#[derive(Clone, Copy, Debug, Default)]
pub struct BootKeyboardDecoder {
    /// The most recently accepted report, used for diffing.
    prev: [u8; HID_KBD_REPORT_LEN],
}

impl BootKeyboardDecoder {
    /// Construct a decoder whose previous report is all-zeroes (no keys held).
    pub const fn new() -> Self {
        Self {
            prev: [0u8; HID_KBD_REPORT_LEN],
        }
    }

    /// Diff `report` against the stored previous report, push [`KeyEdge`]s
    /// into `out`, then store `report` as the new previous.
    ///
    /// # Edge generation rules
    ///
    /// 1. **Modifier edges** — for each of the 8 modifier bits in byte 0,
    ///    emit a [`KeyEventKind::Down`] edge when the bit transitions 0→1 and
    ///    a [`KeyEventKind::Up`] edge when it transitions 1→0. The keycode is
    ///    taken from the `MODIFIER_KEYCODES` table.
    ///
    /// 2. **Key-array edges** — bytes 2..=7 each hold a HID usage ID (or 0
    ///    for "no key"). For each usage ID present in the new report but not
    ///    in the previous report, emit a `Down` edge. For each usage ID
    ///    present in the previous report but not in the new report, emit an
    ///    `Up` edge.
    ///
    /// 3. **Rollover suppression** — if byte 2 of the new report equals 0x01
    ///    (Keyboard Error Roll Over), the key-array is ignored for this report
    ///    (the array values are unreliable). Modifier edges are still processed
    ///    normally.
    ///
    /// The `modifiers` field on every emitted [`KeyEdge`] is the
    /// [`ModifierState`] decoded from byte 0 of the *new* report.
    pub fn decode(
        &mut self,
        report: &[u8; HID_KBD_REPORT_LEN],
        out: &mut alloc::vec::Vec<KeyEdge>,
    ) {
        let new_mod = report[0];
        let old_mod = self.prev[0];
        let mods = hid_modifiers_to_state(new_mod);

        // --- 1. Modifier-bit edges ---
        let changed = new_mod ^ old_mod;
        for bit in 0u8..8 {
            if changed & (1 << bit) == 0 {
                continue;
            }
            let keycode = MODIFIER_KEYCODES[bit as usize];
            let kind = if new_mod & (1 << bit) != 0 {
                KeyEventKind::Down
            } else {
                KeyEventKind::Up
            };
            out.push(KeyEdge {
                keycode,
                kind,
                modifiers: mods,
            });
        }

        // --- 2. Key-array edges ---
        // Rollover: if the first usage byte is 0x01 (ErrorRollOver), the
        // array contents are unreliable — skip array diffing entirely.
        let rollover = report[2] == 0x01;

        if !rollover {
            let new_keys = &report[2..8];
            let old_keys = &self.prev[2..8];

            // Newly pressed: in new but not in old. A usage repeated across
            // multiple array slots within one (malformed) report must yield
            // only a single Down edge, so skip a slot whose usage already
            // appeared earlier in this report.
            for (i, &usage) in new_keys.iter().enumerate() {
                if usage == 0 {
                    continue;
                }
                if new_keys[..i].contains(&usage) {
                    continue;
                }
                if !old_keys.contains(&usage)
                    && let Some(keycode) = hid_usage_to_keycode(usage)
                {
                    out.push(KeyEdge {
                        keycode,
                        kind: KeyEventKind::Down,
                        modifiers: mods,
                    });
                }
            }

            // Newly released: in old but not in new. De-duplicate the same way
            // so a repeated usage in the prior report releases only once.
            for (i, &usage) in old_keys.iter().enumerate() {
                if usage == 0 {
                    continue;
                }
                if old_keys[..i].contains(&usage) {
                    continue;
                }
                if !new_keys.contains(&usage)
                    && let Some(keycode) = hid_usage_to_keycode(usage)
                {
                    out.push(KeyEdge {
                        keycode,
                        kind: KeyEventKind::Up,
                        modifiers: mods,
                    });
                }
            }
        }

        // Store the new report as the previous for the next diff. On a rollover
        // frame the key array is all-0x01 sentinels rather than real keys:
        // overwriting `prev` with it would erase the record of keys that were
        // held *before* the rollover, so their eventual release could never be
        // diffed and they would stick down forever. Preserve the previous key
        // array across a rollover and update only the modifier/reserved bytes
        // (the modifier byte was diffed normally above).
        if rollover {
            self.prev[0] = report[0];
            self.prev[1] = report[1];
        } else {
            self.prev = *report;
        }
    }
}

// ---------------------------------------------------------------------------
// MouseReport + parse_boot_mouse_report
// ---------------------------------------------------------------------------

/// Decoded boot mouse report: button state and sign-extended motion deltas.
///
/// Derived from a boot-mouse interrupt-IN report (USB HID §B.2):
/// `[buttons][dx i8][dy i8][…]`. Trailing bytes are ignored.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct MouseReport {
    /// Button state bitmask. Bit 0 = left button, bit 1 = right button,
    /// bit 2 = middle button. Higher bits are device-specific.
    pub buttons: u8,
    /// Signed horizontal motion delta. Sign-extended from the `i8` wire value.
    pub dx: i32,
    /// Signed vertical motion delta. Sign-extended from the `i8` wire value.
    /// Positive `dy` is typically downward (raw device convention).
    pub dy: i32,
}

/// Decode a boot mouse report from a byte slice.
///
/// The slice must be at least [`HID_MOUSE_REPORT_MIN_LEN`] bytes long.
/// Trailing bytes beyond the first three (e.g. a wheel byte) are silently
/// ignored — the caller receives only buttons, dx, and dy.
///
/// Returns `None` if `report.len() < 3`.
pub fn parse_boot_mouse_report(report: &[u8]) -> Option<MouseReport> {
    if report.len() < HID_MOUSE_REPORT_MIN_LEN {
        return None;
    }
    Some(MouseReport {
        buttons: report[0],
        dx: report[1] as i8 as i32,
        dy: report[2] as i8 as i32,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::events::{MOD_ALT, MOD_CTRL, MOD_SHIFT};
    use crate::input::keymap::{
        KEY_1, KEY_A, KEY_BACKSPACE, KEY_ENTER, KEY_ESC, KEY_SPACE, KEY_TAB,
    };

    // ---- hid_usage_to_keycode ---------------------------------------------

    /// HID usage 0x04 ('a') must map to KEY_A.
    #[test]
    fn usage_a_maps_to_key_a() {
        assert_eq!(hid_usage_to_keycode(0x04), Some(KEY_A.0));
    }

    /// HID usage 0x1E ('1') must map to KEY_1.
    #[test]
    fn usage_1_maps_to_key_1() {
        assert_eq!(hid_usage_to_keycode(0x1E), Some(KEY_1.0));
    }

    /// HID usage 0x28 (Enter) must map to KEY_ENTER.
    #[test]
    fn usage_enter_maps_to_key_enter() {
        assert_eq!(hid_usage_to_keycode(0x28), Some(KEY_ENTER.0));
    }

    /// HID usage 0x2C (Space) must map to KEY_SPACE.
    #[test]
    fn usage_space_maps_to_key_space() {
        assert_eq!(hid_usage_to_keycode(0x2C), Some(KEY_SPACE.0));
    }

    /// HID usage 0x29 (Esc) must map to KEY_ESC.
    #[test]
    fn usage_esc_maps_to_key_esc() {
        assert_eq!(hid_usage_to_keycode(0x29), Some(KEY_ESC.0));
    }

    /// HID usage 0x2A (Backspace) must map to KEY_BACKSPACE.
    #[test]
    fn usage_backspace_maps_to_key_backspace() {
        assert_eq!(hid_usage_to_keycode(0x2A), Some(KEY_BACKSPACE.0));
    }

    /// HID usage 0x2B (Tab) must map to KEY_TAB.
    #[test]
    fn usage_tab_maps_to_key_tab() {
        assert_eq!(hid_usage_to_keycode(0x2B), Some(KEY_TAB.0));
    }

    /// Usage 0x00 (No event) must return None.
    #[test]
    fn usage_zero_returns_none() {
        assert_eq!(hid_usage_to_keycode(0x00), None);
    }

    /// Usage 0x01 (Keyboard Error Roll Over) must return None.
    #[test]
    fn usage_rollover_returns_none() {
        assert_eq!(hid_usage_to_keycode(0x01), None);
    }

    /// Usage 0x02 (Keyboard POST Fail) must return None.
    #[test]
    fn usage_post_fail_returns_none() {
        assert_eq!(hid_usage_to_keycode(0x02), None);
    }

    /// Usage 0x03 (Keyboard Error Undefined) must return None.
    #[test]
    fn usage_error_undefined_returns_none() {
        assert_eq!(hid_usage_to_keycode(0x03), None);
    }

    // ---- hid_modifiers_to_state -------------------------------------------

    /// LShift only (bit 1 = 0x02) → MOD_SHIFT set, nothing else.
    #[test]
    fn modifier_lshift_sets_mod_shift() {
        let state = hid_modifiers_to_state(0x02);
        assert!(state.contains(MOD_SHIFT));
        assert!(!state.contains(MOD_CTRL));
        assert!(!state.contains(MOD_ALT));
    }

    /// LCtrl only (bit 0 = 0x01) → MOD_CTRL set.
    #[test]
    fn modifier_lctrl_sets_mod_ctrl() {
        let state = hid_modifiers_to_state(0x01);
        assert!(state.contains(MOD_CTRL));
        assert!(!state.contains(MOD_SHIFT));
        assert!(!state.contains(MOD_ALT));
    }

    /// RAlt (bit 6 = 0x40) → MOD_ALT set.
    #[test]
    fn modifier_ralt_sets_mod_alt() {
        let state = hid_modifiers_to_state(0x40);
        assert!(state.contains(MOD_ALT));
        assert!(!state.contains(MOD_SHIFT));
        assert!(!state.contains(MOD_CTRL));
    }

    /// LCtrl | LShift (0x03) → both MOD_CTRL and MOD_SHIFT set.
    #[test]
    fn modifier_combined_ctrl_shift() {
        let state = hid_modifiers_to_state(0x03);
        assert!(state.contains(MOD_CTRL));
        assert!(state.contains(MOD_SHIFT));
        assert!(!state.contains(MOD_ALT));
    }

    /// Zero modifier byte → empty state.
    #[test]
    fn modifier_zero_byte_is_empty() {
        let state = hid_modifiers_to_state(0x00);
        assert_eq!(state, ModifierState::empty());
    }

    /// RCtrl (bit 4 = 0x10) also sets MOD_CTRL (same logical modifier).
    #[test]
    fn modifier_rctrl_sets_mod_ctrl() {
        let state = hid_modifiers_to_state(0x10);
        assert!(state.contains(MOD_CTRL));
    }

    /// RShift (bit 5 = 0x20) also sets MOD_SHIFT.
    #[test]
    fn modifier_rshift_sets_mod_shift() {
        let state = hid_modifiers_to_state(0x20);
        assert!(state.contains(MOD_SHIFT));
    }

    // ---- BootKeyboardDecoder ----------------------------------------------

    fn empty_report() -> [u8; HID_KBD_REPORT_LEN] {
        [0u8; HID_KBD_REPORT_LEN]
    }

    /// Pressing 'a' from no prior keys → exactly one Down edge for KEY_A.
    #[test]
    fn press_a_yields_one_down_edge() {
        let mut dec = BootKeyboardDecoder::new();
        let mut edges = alloc::vec::Vec::new();

        // Report: modifier=0, reserved=0, usage=0x04 (A), rest=0.
        let report = [0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00];
        dec.decode(&report, &mut edges);

        assert_eq!(edges.len(), 1, "expected exactly one edge");
        assert_eq!(edges[0].keycode, KEY_A.0);
        assert_eq!(edges[0].kind, KeyEventKind::Down);
    }

    /// Releasing 'a' (empty report after 'a' pressed) → exactly one Up edge for KEY_A.
    #[test]
    fn release_a_yields_one_up_edge() {
        let mut dec = BootKeyboardDecoder::new();
        let mut edges = alloc::vec::Vec::new();

        // Press A.
        let press = [0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00];
        dec.decode(&press, &mut edges);
        edges.clear();

        // Release A (empty report).
        dec.decode(&empty_report(), &mut edges);
        assert_eq!(edges.len(), 1, "expected exactly one Up edge");
        assert_eq!(edges[0].keycode, KEY_A.0);
        assert_eq!(edges[0].kind, KeyEventKind::Up);
    }

    /// Holding Shift then pressing 'a': modifier byte 0x02 should produce a
    /// LShift Down edge AND the 'a' Down edge carries MOD_SHIFT in modifiers.
    #[test]
    fn shift_then_a_yields_shift_down_then_a_with_mod() {
        let mut dec = BootKeyboardDecoder::new();
        let mut edges = alloc::vec::Vec::new();

        // Report: LShift held, usage A.
        let report = [0x02, 0x00, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00];
        dec.decode(&report, &mut edges);

        // Expect: LShift Down edge + A Down edge (order: modifier first).
        assert!(
            edges.len() >= 2,
            "expected at least 2 edges, got {}",
            edges.len()
        );

        // Find LShift Down edge.
        let shift_edge = edges
            .iter()
            .find(|e| e.keycode == KEY_LSHIFT.0 && e.kind == KeyEventKind::Down);
        assert!(shift_edge.is_some(), "expected LShift Down edge");

        // Find A Down edge.
        let a_edge = edges
            .iter()
            .find(|e| e.keycode == KEY_A.0 && e.kind == KeyEventKind::Down);
        assert!(a_edge.is_some(), "expected A Down edge");

        // The A Down edge must carry MOD_SHIFT.
        let a_edge = a_edge.unwrap();
        assert!(
            a_edge.modifiers.contains(MOD_SHIFT),
            "A Down edge must carry MOD_SHIFT"
        );
    }

    /// Two keys held in one report → two Down edges; releasing one → one Up edge.
    #[test]
    fn two_keys_held_then_one_released() {
        let mut dec = BootKeyboardDecoder::new();
        let mut edges = alloc::vec::Vec::new();

        // Press A and B.
        let both = [0x00, 0x00, 0x04, 0x05, 0x00, 0x00, 0x00, 0x00];
        dec.decode(&both, &mut edges);
        assert_eq!(edges.len(), 2, "expected two Down edges");
        assert!(
            edges
                .iter()
                .any(|e| e.keycode == KEY_A.0 && e.kind == KeyEventKind::Down)
        );
        assert!(
            edges
                .iter()
                .any(|e| e.keycode == KEY_B.0 && e.kind == KeyEventKind::Down)
        );
        edges.clear();

        // Release A; B remains.
        let b_only = [0x00, 0x00, 0x05, 0x00, 0x00, 0x00, 0x00, 0x00];
        dec.decode(&b_only, &mut edges);
        assert_eq!(edges.len(), 1, "expected one Up edge for A");
        assert_eq!(edges[0].keycode, KEY_A.0);
        assert_eq!(edges[0].kind, KeyEventKind::Up);
    }

    /// Rollover report (first usage byte = 0x01) → no array edges, but
    /// modifier changes in byte 0 are still processed.
    #[test]
    fn rollover_suppresses_key_array_but_not_modifiers() {
        let mut dec = BootKeyboardDecoder::new();
        let mut edges = alloc::vec::Vec::new();

        // Rollover report with LCtrl bit set (byte 0 = 0x01).
        let rollover = [0x01, 0x00, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01];
        dec.decode(&rollover, &mut edges);

        // Key array must be suppressed — no KEY_LCTRL keycode confusion here
        // because 0x01 in usage array would be None anyway; but we also need
        // to verify no spurious usages were decoded.
        // The modifier bit 0 = LCtrl being set should yield a LCtrl Down edge.
        let ctrl_down = edges
            .iter()
            .filter(|e| e.keycode == KEY_LCTRL.0 && e.kind == KeyEventKind::Down)
            .count();
        assert_eq!(
            ctrl_down, 1,
            "expected one LCtrl Down edge from modifier byte"
        );

        // No array-derived edges should exist (all usages in array are 0x01 =
        // rollover error, which maps to None).
        let array_edges = edges.iter().filter(|e| e.keycode != KEY_LCTRL.0).count();
        assert_eq!(
            array_edges, 0,
            "no array-derived edges expected during rollover"
        );
    }

    /// A key held *before* a rollover frame must still produce an Up edge when
    /// it is later released. Regression: the decoder used to overwrite `prev`
    /// with the all-0x01 rollover sentinel, erasing the held-key record so the
    /// release was never diffed and the key stuck down.
    #[test]
    fn keys_held_before_rollover_release_correctly() {
        let mut dec = BootKeyboardDecoder::new();
        let mut edges = alloc::vec::Vec::new();

        // Hold A and B.
        let ab = [0x00, 0x00, 0x04, 0x05, 0x00, 0x00, 0x00, 0x00];
        dec.decode(&ab, &mut edges);
        assert_eq!(edges.len(), 2, "expected two Down edges for A and B");
        edges.clear();

        // Rollover frame (more keys than the array can report): all 0x01.
        let rollover = [0x00, 0x00, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01];
        dec.decode(&rollover, &mut edges);
        assert!(
            edges.is_empty(),
            "rollover frame must not emit any key edges"
        );
        edges.clear();

        // Release everything. A and B (held before the rollover) must each
        // produce exactly one Up edge.
        dec.decode(&empty_report(), &mut edges);
        assert_eq!(
            edges.len(),
            2,
            "expected two Up edges after rollover release"
        );
        assert!(
            edges
                .iter()
                .any(|e| e.keycode == KEY_A.0 && e.kind == KeyEventKind::Up),
            "A must release after a rollover"
        );
        assert!(
            edges
                .iter()
                .any(|e| e.keycode == KEY_B.0 && e.kind == KeyEventKind::Up),
            "B must release after a rollover"
        );
    }

    /// A malformed report listing the same usage in two array slots must yield
    /// only one Down edge (and, on release, only one Up edge).
    #[test]
    fn duplicate_usage_in_report_yields_single_edge() {
        let mut dec = BootKeyboardDecoder::new();
        let mut edges = alloc::vec::Vec::new();

        // Usage 0x04 (A) appears twice.
        let dup = [0x00, 0x00, 0x04, 0x04, 0x00, 0x00, 0x00, 0x00];
        dec.decode(&dup, &mut edges);
        let a_downs = edges
            .iter()
            .filter(|e| e.keycode == KEY_A.0 && e.kind == KeyEventKind::Down)
            .count();
        assert_eq!(a_downs, 1, "duplicate usage must yield a single Down edge");
        edges.clear();

        // Release: a single Up edge despite the duplicate in the prior report.
        dec.decode(&empty_report(), &mut edges);
        let a_ups = edges
            .iter()
            .filter(|e| e.keycode == KEY_A.0 && e.kind == KeyEventKind::Up)
            .count();
        assert_eq!(a_ups, 1, "duplicate usage must yield a single Up edge");
    }

    /// Identical reports produce no edges.
    #[test]
    fn identical_report_yields_no_edges() {
        let mut dec = BootKeyboardDecoder::new();
        let mut edges = alloc::vec::Vec::new();

        let report = [0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00];
        dec.decode(&report, &mut edges);
        edges.clear();

        // Same report again.
        dec.decode(&report, &mut edges);
        assert!(edges.is_empty(), "identical report must produce no edges");
    }

    // ---- parse_boot_mouse_report ------------------------------------------

    /// [0x01, 0x05, 0xFB] → left button, dx=+5, dy=-5.
    #[test]
    fn mouse_report_positive_dx_negative_dy() {
        let raw = [0x01u8, 0x05, 0xFB];
        let r = parse_boot_mouse_report(&raw).expect("must parse");
        assert_eq!(r.buttons, 1);
        assert_eq!(r.dx, 5);
        assert_eq!(r.dy, -5);
    }

    /// Fewer than 3 bytes → None.
    #[test]
    fn mouse_report_too_short_returns_none() {
        assert!(parse_boot_mouse_report(&[]).is_none());
        assert!(parse_boot_mouse_report(&[0x00]).is_none());
        assert!(parse_boot_mouse_report(&[0x00, 0x01]).is_none());
    }

    /// A 4-byte report and its 3-byte prefix must produce the same MouseReport
    /// (the trailing wheel byte is ignored).
    #[test]
    fn mouse_report_trailing_wheel_byte_ignored() {
        let four_byte = [0x00u8, 0x03, 0x02, 0x7F];
        let three_byte = [0x00u8, 0x03, 0x02];

        let r4 = parse_boot_mouse_report(&four_byte).expect("4-byte must parse");
        let r3 = parse_boot_mouse_report(&three_byte).expect("3-byte must parse");

        assert_eq!(r4, r3, "4-byte and 3-byte prefix must decode identically");
    }

    /// Zero-delta, no buttons.
    #[test]
    fn mouse_report_all_zeros() {
        let r = parse_boot_mouse_report(&[0x00, 0x00, 0x00]).unwrap();
        assert_eq!(r, MouseReport::default());
    }

    /// Right button only (bit 1 = 0x02).
    #[test]
    fn mouse_report_right_button() {
        let r = parse_boot_mouse_report(&[0x02, 0x00, 0x00]).unwrap();
        assert_eq!(r.buttons, 0x02);
    }
}
