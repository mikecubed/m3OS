//! Phase 56 Track D.4 — Keybind grab table + per-keycode grab state.
//!
//! Pure-logic types consumed by the `display_server` input dispatcher
//! (Track D.3). [`BindTable`] owns the registration table keyed by
//! `(modifier_mask, keycode)` with **exact mask equality** matching;
//! [`GrabState`] tracks per-keycode grab presence so a `KeyDown` that hit
//! a bind suppresses the matching `KeyRepeat`/`KeyUp` pair, ensuring
//! clients never see half a chord.
//!
//! Both types are independent — the dispatcher composes them. Storage is
//! fixed-capacity (no allocator); `register` past [`MAX_BINDS`] returns
//! [`BindError::TableFull`] and `start_grab` past `MAX_GRABS` is a no-op
//! that returns `false`.
//!
//! Spec: `docs/roadmap/tasks/56-display-and-input-architecture-tasks.md` § D.4 and § A.5.

// Stub-only file used to commit failing tests before the implementation.
// Replaced wholesale by the green-test commit.

#![allow(dead_code, unused_variables)]

/// Maximum number of registered binds. Phase 56 chose 64; recorded by H.1.
pub const MAX_BINDS: usize = 64;

/// Maximum number of concurrently active key-down grabs. Far smaller than
/// `MAX_BINDS` — a user only ever has a handful of keys held at once.
pub const MAX_GRABS: usize = 8;

/// Registration key — `(modifier_mask, keycode)` pair, where the mask is
/// the same `MOD_*` bitfield carried on a [`crate::input::events::KeyEvent`].
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub struct BindKey {
    pub modifier_mask: u16,
    pub keycode: u32,
}

/// Opaque, stable handle returned by [`BindTable::register`]. Never reused
/// across the lifetime of a single [`BindTable`] instance.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub struct BindId(u32);

/// Errors returned by [`BindTable`]. `#[non_exhaustive]` so future variants
/// (e.g. invalid modifier bits) can be added without breaking matchers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum BindError {
    /// `register` was called after [`MAX_BINDS`] entries were already live.
    TableFull,
    /// `unregister` was called with a [`BindId`] that is not present in the table.
    UnknownBind,
}

/// Pure-logic registration table. Independent of [`GrabState`].
pub struct BindTable {
    _stub: (),
}

impl Default for BindTable {
    fn default() -> Self {
        Self::new()
    }
}

impl BindTable {
    pub const fn new() -> Self {
        Self { _stub: () }
    }

    pub fn register(&mut self, key: BindKey) -> Result<BindId, BindError> {
        // Stub — fails tests until D.4's green commit lands.
        Err(BindError::TableFull)
    }

    pub fn unregister(&mut self, id: BindId) -> Result<(), BindError> {
        Err(BindError::UnknownBind)
    }

    pub fn match_bind(&self, modifier_mask: u16, keycode: u32) -> Option<BindId> {
        None
    }

    pub fn len(&self) -> usize {
        0
    }

    pub fn is_empty(&self) -> bool {
        true
    }
}

/// Per-keycode grab suppression policy. Independent of [`BindTable`].
pub struct GrabState {
    _stub: (),
}

impl Default for GrabState {
    fn default() -> Self {
        Self::new()
    }
}

impl GrabState {
    pub const fn new() -> Self {
        Self { _stub: () }
    }

    pub fn start_grab(&mut self, keycode: u32, bind: BindId) -> bool {
        false
    }

    pub fn is_grabbed(&self, keycode: u32) -> Option<BindId> {
        None
    }

    pub fn clear_on_keyup(&mut self, keycode: u32) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::events::{MOD_ALT, MOD_CTRL, MOD_SHIFT, MOD_SUPER};
    use proptest::prelude::*;

    fn key(modifier_mask: u16, keycode: u32) -> BindKey {
        BindKey {
            modifier_mask,
            keycode,
        }
    }

    // ---- BindTable smoke ----------------------------------------------------

    #[test]
    fn register_then_match_then_unregister_then_no_match() {
        let mut t = BindTable::new();
        let k = key(MOD_SUPER, b'q' as u32);
        let id = t.register(k).expect("first register succeeds");
        assert_eq!(t.match_bind(MOD_SUPER, b'q' as u32), Some(id));
        t.unregister(id).expect("unregister succeeds");
        assert_eq!(t.match_bind(MOD_SUPER, b'q' as u32), None);
    }

    #[test]
    fn empty_table_never_matches() {
        let t = BindTable::new();
        assert_eq!(t.match_bind(0, 0), None);
        assert_eq!(t.match_bind(MOD_SUPER, b'q' as u32), None);
    }

    // ---- Exact-mask matching (A.5 critical correctness) --------------------

    #[test]
    fn exact_mask_super_q_does_not_match_super_shift_q() {
        let mut t = BindTable::new();
        let id_super_q = t.register(key(MOD_SUPER, b'q' as u32)).unwrap();
        // Pressing Super+Shift+Q must NOT trigger the Super+Q bind.
        assert_eq!(t.match_bind(MOD_SUPER | MOD_SHIFT, b'q' as u32), None);
        // Sanity check: Super+Q alone still matches.
        assert_eq!(t.match_bind(MOD_SUPER, b'q' as u32), Some(id_super_q));
    }

    #[test]
    fn two_binds_differing_only_in_mask_are_distinct() {
        let mut t = BindTable::new();
        let id_sq = t.register(key(MOD_SUPER, b'q' as u32)).unwrap();
        let id_ssq = t.register(key(MOD_SUPER | MOD_SHIFT, b'q' as u32)).unwrap();
        assert_ne!(id_sq, id_ssq);
        assert_eq!(t.match_bind(MOD_SUPER, b'q' as u32), Some(id_sq));
        assert_eq!(
            t.match_bind(MOD_SUPER | MOD_SHIFT, b'q' as u32),
            Some(id_ssq)
        );
    }

    #[test]
    fn no_modifier_bind_does_not_match_with_any_modifier_pressed() {
        let mut t = BindTable::new();
        let _id = t.register(key(0, b'a' as u32)).unwrap();
        assert_eq!(t.match_bind(MOD_SHIFT, b'a' as u32), None);
        assert_eq!(t.match_bind(MOD_SUPER, b'a' as u32), None);
        assert_eq!(t.match_bind(0, b'a' as u32), Some(_id));
    }

    // ---- Double-register contract: idempotent ------------------------------

    #[test]
    fn double_register_returns_same_id_idempotent() {
        let mut t = BindTable::new();
        let k = key(MOD_CTRL | MOD_ALT, 0xFF);
        let first = t.register(k).unwrap();
        let second = t.register(k).unwrap();
        assert_eq!(first, second);
        // Table did not grow.
        assert_eq!(t.len(), 1);
        // And one unregister releases the slot.
        t.unregister(first).unwrap();
        assert_eq!(t.match_bind(MOD_CTRL | MOD_ALT, 0xFF), None);
    }

    // ---- Unregister error path ---------------------------------------------

    #[test]
    fn unregister_unknown_id_returns_typed_error_no_panic() {
        let mut t = BindTable::new();
        let bogus = BindId(0xDEAD_BEEF);
        assert_eq!(t.unregister(bogus), Err(BindError::UnknownBind));
    }

    #[test]
    fn unregister_after_unregister_is_unknown_bind() {
        let mut t = BindTable::new();
        let id = t.register(key(0, 1)).unwrap();
        t.unregister(id).unwrap();
        assert_eq!(t.unregister(id), Err(BindError::UnknownBind));
    }

    // ---- Capacity ----------------------------------------------------------

    #[test]
    fn table_full_at_max_binds_plus_one() {
        let mut t = BindTable::new();
        for i in 0..MAX_BINDS as u32 {
            t.register(key(0, i))
                .unwrap_or_else(|_| panic!("register {} of {} should succeed", i, MAX_BINDS));
        }
        // One past the limit fails.
        assert_eq!(
            t.register(key(0, MAX_BINDS as u32)),
            Err(BindError::TableFull)
        );
    }

    #[test]
    fn full_table_still_supports_unregister_and_reregister() {
        let mut t = BindTable::new();
        let mut ids = [None; MAX_BINDS];
        for i in 0..MAX_BINDS as u32 {
            ids[i as usize] = Some(t.register(key(0, i)).unwrap());
        }
        // Free a slot.
        t.unregister(ids[5].unwrap()).unwrap();
        // A new register succeeds and yields a fresh id (no reuse).
        let new_id = t.register(key(0, 999)).unwrap();
        assert_ne!(Some(new_id), ids[5]);
        assert_eq!(t.match_bind(0, 999), Some(new_id));
    }

    // ---- BindId stability --------------------------------------------------

    #[test]
    fn bind_ids_are_never_reused_across_unregister_register_cycle() {
        let mut t = BindTable::new();
        let id1 = t.register(key(0, 1)).unwrap();
        t.unregister(id1).unwrap();
        let id2 = t.register(key(0, 1)).unwrap();
        assert_ne!(id1, id2, "BindId must be stable / never reused");
    }

    // ---- GrabState ---------------------------------------------------------

    #[test]
    fn grab_state_starts_empty() {
        let g = GrabState::new();
        assert_eq!(g.is_grabbed(b'q' as u32), None);
        assert_eq!(g.is_grabbed(0), None);
    }

    #[test]
    fn start_grab_then_is_grabbed_returns_bind() {
        let mut g = GrabState::new();
        let bind = BindId(7);
        assert!(g.start_grab(b'q' as u32, bind));
        assert_eq!(g.is_grabbed(b'q' as u32), Some(bind));
        assert_eq!(g.is_grabbed(b'a' as u32), None);
    }

    #[test]
    fn clear_on_keyup_for_grabbed_keycode_returns_true_and_clears() {
        let mut g = GrabState::new();
        let bind = BindId(11);
        g.start_grab(b'q' as u32, bind);
        assert!(g.clear_on_keyup(b'q' as u32));
        assert_eq!(g.is_grabbed(b'q' as u32), None);
    }

    #[test]
    fn clear_on_keyup_for_non_grabbed_keycode_returns_false() {
        let mut g = GrabState::new();
        assert!(!g.clear_on_keyup(b'q' as u32));
        // And it's idempotent — repeated calls keep returning false.
        assert!(!g.clear_on_keyup(b'q' as u32));
    }

    #[test]
    fn clear_on_keyup_after_clear_is_false() {
        let mut g = GrabState::new();
        let bind = BindId(3);
        g.start_grab(b'q' as u32, bind);
        assert!(g.clear_on_keyup(b'q' as u32));
        // Second call: nothing to clear.
        assert!(!g.clear_on_keyup(b'q' as u32));
    }

    #[test]
    fn key_repeat_simulation_grabbed_keycode_stays_grabbed_until_keyup() {
        // Simulate D.3's planned dispatcher behavior:
        //   on KeyDown match  -> start_grab(keycode, bind)
        //   on KeyRepeat      -> if is_grabbed(keycode).is_some() suppress
        //   on KeyUp          -> clear_on_keyup(keycode); suppress if was grabbed
        let mut g = GrabState::new();
        let bind = BindId(42);
        g.start_grab(b'q' as u32, bind);

        // Five repeats: each should still see an active grab.
        for _ in 0..5 {
            assert_eq!(g.is_grabbed(b'q' as u32), Some(bind));
        }
        // The release clears.
        assert!(g.clear_on_keyup(b'q' as u32));
        assert_eq!(g.is_grabbed(b'q' as u32), None);
    }

    #[test]
    fn multiple_keycodes_grabbed_independently() {
        let mut g = GrabState::new();
        let b1 = BindId(1);
        let b2 = BindId(2);
        g.start_grab(b'q' as u32, b1);
        g.start_grab(b'w' as u32, b2);
        assert_eq!(g.is_grabbed(b'q' as u32), Some(b1));
        assert_eq!(g.is_grabbed(b'w' as u32), Some(b2));
        // Releasing q does not affect w.
        assert!(g.clear_on_keyup(b'q' as u32));
        assert_eq!(g.is_grabbed(b'q' as u32), None);
        assert_eq!(g.is_grabbed(b'w' as u32), Some(b2));
    }

    #[test]
    fn re_grab_same_keycode_overwrites_bind() {
        // If a key is pressed twice without a keyup in between (debounce
        // glitch / two binds matching same keycode at different masks), the
        // newest grab wins. The dispatcher will only ever start_grab from a
        // KeyDown, so the older grab is conceptually orphaned anyway.
        let mut g = GrabState::new();
        g.start_grab(b'q' as u32, BindId(1));
        g.start_grab(b'q' as u32, BindId(2));
        assert_eq!(g.is_grabbed(b'q' as u32), Some(BindId(2)));
    }

    #[test]
    fn grab_state_capacity_full_returns_false() {
        let mut g = GrabState::new();
        for i in 0..MAX_GRABS as u32 {
            assert!(g.start_grab(i, BindId(i)));
        }
        // One past the limit: false, no panic, no displacement.
        assert!(!g.start_grab(MAX_GRABS as u32, BindId(99)));
        assert_eq!(g.is_grabbed(MAX_GRABS as u32), None);
        // The earlier grabs are still intact.
        assert_eq!(g.is_grabbed(0), Some(BindId(0)));
    }

    // ---- Property tests ----------------------------------------------------

    #[derive(Debug, Clone)]
    enum Op {
        Register { mask: u16, keycode: u32 },
        UnregisterById(u8), // index into "ids ever issued" list
        Match { mask: u16, keycode: u32 },
    }

    fn arb_op() -> impl Strategy<Value = Op> {
        prop_oneof![
            (0u16..=0x3F, 0u32..16).prop_map(|(mask, keycode)| Op::Register { mask, keycode }),
            (0u8..32).prop_map(Op::UnregisterById),
            (0u16..=0x3F, 0u32..16).prop_map(|(mask, keycode)| Op::Match { mask, keycode }),
        ]
    }

    proptest! {
        #[test]
        fn prop_bind_ids_never_double_issued(
            ops in proptest::collection::vec(arb_op(), 0..200)
        ) {
            let mut t = BindTable::new();
            let mut issued: alloc::vec::Vec<BindId> = alloc::vec::Vec::new();
            let mut still_present: alloc::vec::Vec<BindId> = alloc::vec::Vec::new();
            for op in ops {
                match op {
                    Op::Register { mask, keycode } => {
                        if let Ok(id) = t.register(BindKey { modifier_mask: mask, keycode }) {
                            // Idempotent contract: a duplicate register may yield
                            // an id we already know about. Otherwise it must be new.
                            let already_present = still_present.contains(&id);
                            let already_issued_then_freed =
                                issued.contains(&id) && !still_present.contains(&id);
                            prop_assert!(
                                !already_issued_then_freed,
                                "BindId {:?} was reused after being freed",
                                id
                            );
                            if !already_present {
                                issued.push(id);
                                still_present.push(id);
                            }
                        }
                    }
                    Op::UnregisterById(idx) => {
                        if let Some(&id) = still_present.get(idx as usize % still_present.len().max(1))
                            .filter(|_| !still_present.is_empty())
                        {
                            t.unregister(id).expect("present in still_present");
                            still_present.retain(|x| *x != id);
                        }
                    }
                    Op::Match { mask, keycode } => {
                        let m = t.match_bind(mask, keycode);
                        if let Some(id) = m {
                            // Returned id must be currently registered.
                            prop_assert!(
                                still_present.contains(&id),
                                "match returned {:?} but it isn't currently registered",
                                id
                            );
                        }
                    }
                }
            }
        }

        #[test]
        fn prop_match_never_returns_id_for_never_registered_bind(
            mask in 0u16..=0x3F,
            keycode in 0u32..256,
        ) {
            let t = BindTable::new();
            // Empty table — no match can possibly be valid.
            prop_assert_eq!(t.match_bind(mask, keycode), None);
        }

        #[test]
        fn prop_grab_clear_pairs_leave_no_residual(
            keycodes in proptest::collection::vec(0u32..16, 0..50)
        ) {
            let mut g = GrabState::new();
            for (i, &k) in keycodes.iter().enumerate() {
                let id = BindId(i as u32);
                if g.start_grab(k, id) {
                    prop_assert_eq!(g.is_grabbed(k), Some(id));
                    let cleared = g.clear_on_keyup(k);
                    prop_assert!(cleared);
                    prop_assert_eq!(g.is_grabbed(k), None);
                }
            }
        }

        #[test]
        fn prop_grab_clear_idempotent(
            keycodes in proptest::collection::vec(0u32..16, 0..30)
        ) {
            let mut g = GrabState::new();
            for &k in &keycodes {
                // Without a prior start_grab, every clear returns false.
                let cleared = g.clear_on_keyup(k);
                prop_assert!(!cleared);
            }
        }
    }
}
