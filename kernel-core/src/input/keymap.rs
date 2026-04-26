//! Phase 56 Track D.1 — pure-logic keyboard scancode translation, US-QWERTY
//! keymap, modifier tracker, and key-repeat scheduler.
//!
//! Test-first scaffolding: this commit establishes the public surface and
//! the failing test suite. The next commit fills in the bodies. Stubs
//! return placeholder values so the tests *compile* but *fail* — the goal
//! of the test-first discipline is to make the failure state observable
//! before the implementation that turns the suite green.

pub use crate::input::events::{
    KeyEvent, KeyEventKind, MOD_ALL, MOD_ALT, MOD_CAPS, MOD_CTRL, MOD_NUM, MOD_SHIFT, MOD_SUPER,
    ModifierState,
};

// ---------------------------------------------------------------------------
// Public surface — keycodes and key symbols
// ---------------------------------------------------------------------------

/// Hardware-neutral keycode emitted by [`ScancodeDecoder`].
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash, Default)]
pub struct Keycode(pub u32);

/// Post-keymap key symbol.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash, Default)]
pub struct KeySym(pub u32);

/// Sentinel: no key symbol.
pub const KEYSYM_NONE: KeySym = KeySym(0);

// Named keycodes — stable across decoder/keymap/tracker.
pub const KEY_A: Keycode = Keycode(0x0001);
pub const KEY_B: Keycode = Keycode(0x0002);
pub const KEY_C: Keycode = Keycode(0x0003);
pub const KEY_D: Keycode = Keycode(0x0004);
pub const KEY_E: Keycode = Keycode(0x0005);
pub const KEY_F: Keycode = Keycode(0x0006);
pub const KEY_G: Keycode = Keycode(0x0007);
pub const KEY_H: Keycode = Keycode(0x0008);
pub const KEY_I: Keycode = Keycode(0x0009);
pub const KEY_J: Keycode = Keycode(0x000A);
pub const KEY_K: Keycode = Keycode(0x000B);
pub const KEY_L: Keycode = Keycode(0x000C);
pub const KEY_M: Keycode = Keycode(0x000D);
pub const KEY_N: Keycode = Keycode(0x000E);
pub const KEY_O: Keycode = Keycode(0x000F);
pub const KEY_P: Keycode = Keycode(0x0010);
pub const KEY_Q: Keycode = Keycode(0x0011);
pub const KEY_R: Keycode = Keycode(0x0012);
pub const KEY_S: Keycode = Keycode(0x0013);
pub const KEY_T: Keycode = Keycode(0x0014);
pub const KEY_U: Keycode = Keycode(0x0015);
pub const KEY_V: Keycode = Keycode(0x0016);
pub const KEY_W: Keycode = Keycode(0x0017);
pub const KEY_X: Keycode = Keycode(0x0018);
pub const KEY_Y: Keycode = Keycode(0x0019);
pub const KEY_Z: Keycode = Keycode(0x001A);
pub const KEY_1: Keycode = Keycode(0x0021);
pub const KEY_2: Keycode = Keycode(0x0022);
pub const KEY_3: Keycode = Keycode(0x0023);
pub const KEY_4: Keycode = Keycode(0x0024);
pub const KEY_5: Keycode = Keycode(0x0025);
pub const KEY_6: Keycode = Keycode(0x0026);
pub const KEY_7: Keycode = Keycode(0x0027);
pub const KEY_8: Keycode = Keycode(0x0028);
pub const KEY_9: Keycode = Keycode(0x0029);
pub const KEY_0: Keycode = Keycode(0x002A);
pub const KEY_SPACE: Keycode = Keycode(0x0040);
pub const KEY_ENTER: Keycode = Keycode(0x0041);
pub const KEY_BACKSPACE: Keycode = Keycode(0x0042);
pub const KEY_TAB: Keycode = Keycode(0x0043);
pub const KEY_ESC: Keycode = Keycode(0x0044);
pub const KEY_MINUS: Keycode = Keycode(0x0050);
pub const KEY_EQUALS: Keycode = Keycode(0x0051);
pub const KEY_LBRACKET: Keycode = Keycode(0x0052);
pub const KEY_RBRACKET: Keycode = Keycode(0x0053);
pub const KEY_BACKSLASH: Keycode = Keycode(0x0054);
pub const KEY_SEMICOLON: Keycode = Keycode(0x0055);
pub const KEY_APOSTROPHE: Keycode = Keycode(0x0056);
pub const KEY_GRAVE: Keycode = Keycode(0x0057);
pub const KEY_COMMA: Keycode = Keycode(0x0058);
pub const KEY_DOT: Keycode = Keycode(0x0059);
pub const KEY_SLASH: Keycode = Keycode(0x005A);
pub const KEY_LSHIFT: Keycode = Keycode(0x0080);
pub const KEY_RSHIFT: Keycode = Keycode(0x0081);
pub const KEY_LCTRL: Keycode = Keycode(0x0082);
pub const KEY_RCTRL: Keycode = Keycode(0x0083);
pub const KEY_LALT: Keycode = Keycode(0x0084);
pub const KEY_RALT: Keycode = Keycode(0x0085);
pub const KEY_LSUPER: Keycode = Keycode(0x0086);
pub const KEY_RSUPER: Keycode = Keycode(0x0087);
pub const KEY_CAPSLOCK: Keycode = Keycode(0x0088);
pub const KEY_NUMLOCK: Keycode = Keycode(0x0089);
pub const KEY_SCROLLLOCK: Keycode = Keycode(0x008A);
pub const KEY_LEFT: Keycode = Keycode(0x00A0);
pub const KEY_RIGHT: Keycode = Keycode(0x00A1);
pub const KEY_UP: Keycode = Keycode(0x00A2);
pub const KEY_DOWN: Keycode = Keycode(0x00A3);
pub const KEY_HOME: Keycode = Keycode(0x00A4);
pub const KEY_END: Keycode = Keycode(0x00A5);
pub const KEY_PAGEUP: Keycode = Keycode(0x00A6);
pub const KEY_PAGEDOWN: Keycode = Keycode(0x00A7);
pub const KEY_INSERT: Keycode = Keycode(0x00A8);
pub const KEY_DELETE: Keycode = Keycode(0x00A9);
pub const KEY_F1: Keycode = Keycode(0x00C0);
pub const KEY_F2: Keycode = Keycode(0x00C1);
pub const KEY_F3: Keycode = Keycode(0x00C2);
pub const KEY_F4: Keycode = Keycode(0x00C3);
pub const KEY_F5: Keycode = Keycode(0x00C4);
pub const KEY_F6: Keycode = Keycode(0x00C5);
pub const KEY_F7: Keycode = Keycode(0x00C6);
pub const KEY_F8: Keycode = Keycode(0x00C7);
pub const KEY_F9: Keycode = Keycode(0x00C8);
pub const KEY_F10: Keycode = Keycode(0x00C9);
pub const KEY_F11: Keycode = Keycode(0x00CA);
pub const KEY_F12: Keycode = Keycode(0x00CB);
pub const KEY_PAUSE: Keycode = Keycode(0x00E0);
pub const KEY_PRINTSCREEN: Keycode = Keycode(0x00E1);

// KeySym constants — Unicode private-use range so they can never collide
// with valid Unicode scalars.
const KEYSYM_PRIVATE_BASE: u32 = 0xE000;
pub const KEYSYM_LEFT: KeySym = KeySym(KEYSYM_PRIVATE_BASE + 0x10);
pub const KEYSYM_RIGHT: KeySym = KeySym(KEYSYM_PRIVATE_BASE + 0x11);
pub const KEYSYM_UP: KeySym = KeySym(KEYSYM_PRIVATE_BASE + 0x12);
pub const KEYSYM_DOWN: KeySym = KeySym(KEYSYM_PRIVATE_BASE + 0x13);
pub const KEYSYM_HOME: KeySym = KeySym(KEYSYM_PRIVATE_BASE + 0x14);
pub const KEYSYM_END: KeySym = KeySym(KEYSYM_PRIVATE_BASE + 0x15);
pub const KEYSYM_PAGEUP: KeySym = KeySym(KEYSYM_PRIVATE_BASE + 0x16);
pub const KEYSYM_PAGEDOWN: KeySym = KeySym(KEYSYM_PRIVATE_BASE + 0x17);
pub const KEYSYM_INSERT: KeySym = KeySym(KEYSYM_PRIVATE_BASE + 0x18);
pub const KEYSYM_DELETE: KeySym = KeySym(KEYSYM_PRIVATE_BASE + 0x19);
pub const KEYSYM_PAUSE: KeySym = KeySym(KEYSYM_PRIVATE_BASE + 0x1A);
pub const KEYSYM_PRINTSCREEN: KeySym = KeySym(KEYSYM_PRIVATE_BASE + 0x1B);
pub const KEYSYM_F1: KeySym = KeySym(KEYSYM_PRIVATE_BASE + 0x40);
pub const KEYSYM_F2: KeySym = KeySym(KEYSYM_PRIVATE_BASE + 0x41);
pub const KEYSYM_F3: KeySym = KeySym(KEYSYM_PRIVATE_BASE + 0x42);
pub const KEYSYM_F4: KeySym = KeySym(KEYSYM_PRIVATE_BASE + 0x43);
pub const KEYSYM_F5: KeySym = KeySym(KEYSYM_PRIVATE_BASE + 0x44);
pub const KEYSYM_F6: KeySym = KeySym(KEYSYM_PRIVATE_BASE + 0x45);
pub const KEYSYM_F7: KeySym = KeySym(KEYSYM_PRIVATE_BASE + 0x46);
pub const KEYSYM_F8: KeySym = KeySym(KEYSYM_PRIVATE_BASE + 0x47);
pub const KEYSYM_F9: KeySym = KeySym(KEYSYM_PRIVATE_BASE + 0x48);
pub const KEYSYM_F10: KeySym = KeySym(KEYSYM_PRIVATE_BASE + 0x49);
pub const KEYSYM_F11: KeySym = KeySym(KEYSYM_PRIVATE_BASE + 0x4A);
pub const KEYSYM_F12: KeySym = KeySym(KEYSYM_PRIVATE_BASE + 0x4B);

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum KeymapError {
    HeldKeyTableOverflow,
    UnmappedKeycode,
}

// ---------------------------------------------------------------------------
// ScancodeDecoder — set-1 byte stream → (Keycode, KeyKind) edges
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DecodedScancode {
    Edge {
        keycode: Keycode,
        kind: KeyEventKind,
    },
    InProgress,
    Discarded,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ScancodeDecoder;

impl ScancodeDecoder {
    pub const fn new() -> Self {
        Self
    }

    pub fn reset(&mut self) {}

    /// Stub — always returns `Discarded` so all tests fail.
    pub fn feed(&mut self, _byte: u8) -> DecodedScancode {
        DecodedScancode::Discarded
    }
}

// ---------------------------------------------------------------------------
// ModifierTracker — held + locked state
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Default)]
pub struct ModifierTracker;

impl ModifierTracker {
    pub const fn new() -> Self {
        Self
    }

    pub fn state(&self) -> ModifierState {
        ModifierState::empty()
    }

    /// Stub — always returns empty so toggle/hold tests fail.
    pub fn apply(&mut self, _keycode: Keycode, _kind: KeyEventKind) -> ModifierState {
        ModifierState::empty()
    }

    pub fn has(&self, _mask: u16) -> bool {
        false
    }
}

// ---------------------------------------------------------------------------
// Keymap — pure data, US QWERTY
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Default)]
pub struct Keymap;

impl Keymap {
    pub const fn us_qwerty() -> Self {
        Self
    }

    /// Stub — always returns `None` so lookup tests fail.
    pub fn lookup(&self, _keycode: Keycode, _mods: ModifierState) -> Option<KeySym> {
        None
    }
}

// ---------------------------------------------------------------------------
// KeyRepeatScheduler — pure timer-driven repeat
// ---------------------------------------------------------------------------

pub const DEFAULT_REPEAT_INITIAL_DELAY_MS: u64 = 500;
pub const DEFAULT_REPEAT_RATE_HZ: u32 = 30;
pub const MAX_HELD_KEYS: usize = 8;

#[derive(Clone, Copy, Debug, Default)]
pub struct KeyRepeatScheduler;

impl KeyRepeatScheduler {
    pub const fn new() -> Self {
        Self
    }

    pub const fn with_config(_initial_delay_ms: u64, _rate_hz: u64) -> Self {
        Self
    }

    pub fn reset(&mut self) {}

    pub fn held_count(&self) -> usize {
        0
    }

    pub fn initial_delay_ms(&self) -> u64 {
        DEFAULT_REPEAT_INITIAL_DELAY_MS
    }

    pub fn repeat_interval_ms(&self) -> u64 {
        1000 / DEFAULT_REPEAT_RATE_HZ as u64
    }

    /// Stub — never errors and never tracks anything; `tick` returns None.
    pub fn observe(
        &mut self,
        _keycode: Keycode,
        _kind: KeyEventKind,
        _mods: ModifierState,
        _timestamp_ms: u64,
    ) -> Result<(), KeymapError> {
        Ok(())
    }

    /// Stub — never produces a repeat event so timing tests fail.
    pub fn tick(&mut self, _timestamp_ms: u64) -> Option<KeyEvent> {
        None
    }
}

// ---------------------------------------------------------------------------
// Tests — written first per the test-first discipline
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
        // The modifier wire format has one bit per modifier family
        // (SHIFT/CTRL/ALT/SUPER), so left and right shifts share the
        // SHIFT bit. Releasing one shift clears the bit even if the
        // other physical key remains pressed. This matches the wire
        // contract and is the simpler-to-test invariant; full L/R chord
        // tracking is deferred.
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
