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
//! [`BindError::TableFull`] and `start_grab` past [`MAX_GRABS`] is a no-op
//! that returns `false`.
//!
//! Spec: `docs/roadmap/tasks/56-display-and-input-architecture-tasks.md`
//! § D.4 (lines ~607–625) and § A.5 (keybind grab semantics).
//!
//! ### Double-register contract
//!
//! [`BindTable::register`] is **idempotent**: re-registering an existing
//! `(modifier_mask, keycode)` pair returns the existing [`BindId`] rather
//! than allocating a new slot or returning an error. This matches the
//! semantics of X11 `XGrabKey` and most window managers, and it lets the
//! Phase 56 control socket (E.4) call `register_bind` on every config
//! reload without extra dedup logic at the call site.
//!
//! ### Invariants (also enforced by tests)
//!
//! * Every issued [`BindId`] is unique within the lifetime of a single
//!   [`BindTable`] instance — even after `unregister`, that id is never
//!   reused for a later registration.
//! * [`BindTable::match_bind`] only returns ids that are *currently*
//!   registered; an id observed for an entry that was later unregistered
//!   is no longer returned.
//! * After every `start_grab(k) → clear_on_keyup(k)` pair, `is_grabbed(k)`
//!   is `None`.

/// Maximum number of registered binds. Phase 56 chose 64 so the table fits
/// in a single CPU cache line's worth of pointers without dynamic memory;
/// [H.1's bookkeeping](../../../docs/roadmap/tasks/56-display-and-input-architecture-tasks.md)
/// records the choice for the resource-budget table.
pub const MAX_BINDS: usize = 64;

/// Maximum number of concurrently active key-down grabs. A user only ever
/// has a handful of keys held at once, and each entry costs 12 bytes, so 8
/// is generous without being wasteful.
pub const MAX_GRABS: usize = 8;

/// Registration key — `(modifier_mask, keycode)` pair, where the mask is
/// the same `MOD_*` bitfield carried on a [`crate::input::events::KeyEvent`].
///
/// Matching is exact-mask: pressing `SUPER+SHIFT+Q` does **not** trigger a
/// bind registered for `SUPER+Q`. This is the A.5 decision and is enforced
/// by `BindTable::match_bind`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub struct BindKey {
    pub modifier_mask: u16,
    pub keycode: u32,
}

/// Opaque, stable handle returned by [`BindTable::register`].
///
/// Within the lifetime of a single [`BindTable`] instance, ids are never
/// reused — even after `unregister(id)`, the same id will never be issued
/// to a later registration. The dispatcher (D.3) and the control socket
/// (E.4) hold these as plain values; nothing they do can forge an id that
/// later collides with a real registration.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub struct BindId(u32);

impl BindId {
    /// Raw monotonic counter value. Useful for logging only — clients must
    /// **not** synthesize a [`BindId`] from a numeric value they did not
    /// receive from [`BindTable::register`].
    pub const fn raw(self) -> u32 {
        self.0
    }
}

/// Errors returned by [`BindTable`]. `#[non_exhaustive]` so future variants
/// (e.g. invalid modifier bits) can be added without breaking matchers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum BindError {
    /// `register` was called after [`MAX_BINDS`] entries were already live.
    TableFull,
    /// `unregister` was called with a [`BindId`] that is not present in the
    /// table — either it was never issued, or it was already unregistered.
    UnknownBind,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct BindEntry {
    id: BindId,
    key: BindKey,
}

/// Pure-logic registration table. Independent of [`GrabState`]; the
/// dispatcher composes them.
///
/// Internally a fixed-size `[Option<BindEntry>; MAX_BINDS]` plus a
/// monotonic id counter. Lookup is a linear scan over occupied slots — at
/// 64 entries this is on the order of dozens of cache-resident reads,
/// well below 1 µs even on the slowest target hardware.
pub struct BindTable {
    slots: [Option<BindEntry>; MAX_BINDS],
    next_id: u32,
    occupied: usize,
}

impl Default for BindTable {
    fn default() -> Self {
        Self::new()
    }
}

impl BindTable {
    /// Construct an empty table.
    pub const fn new() -> Self {
        Self {
            slots: [None; MAX_BINDS],
            next_id: 0,
            occupied: 0,
        }
    }

    /// Register a new bind, or return the existing [`BindId`] if the same
    /// `(modifier_mask, keycode)` pair is already registered (idempotent
    /// contract — see module docs).
    ///
    /// Errors:
    /// * [`BindError::TableFull`] when [`MAX_BINDS`] entries are live and
    ///   the key is not already present.
    pub fn register(&mut self, key: BindKey) -> Result<BindId, BindError> {
        // Idempotent: existing key returns its existing id.
        if let Some(existing) = self.find_by_key(&key) {
            return Ok(existing);
        }
        // Find the first free slot.
        let free_idx = match self.slots.iter().position(Option::is_none) {
            Some(i) => i,
            None => return Err(BindError::TableFull),
        };
        // Allocate the next monotonic id. Saturating add prevents wrap on
        // the (theoretical) 4-billionth registration; we degrade by
        // refusing further registrations rather than reusing ids.
        if self.next_id == u32::MAX {
            return Err(BindError::TableFull);
        }
        let id = BindId(self.next_id);
        self.next_id = self.next_id.wrapping_add(1);
        self.slots[free_idx] = Some(BindEntry { id, key });
        self.occupied = self.occupied.saturating_add(1);
        Ok(id)
    }

    /// Remove the registration with the given id.
    ///
    /// Errors:
    /// * [`BindError::UnknownBind`] when the id is not currently registered
    ///   — either it was never issued, or it was already unregistered.
    pub fn unregister(&mut self, id: BindId) -> Result<(), BindError> {
        let idx = match self.find_slot_by_id(id) {
            Some(i) => i,
            None => return Err(BindError::UnknownBind),
        };
        self.slots[idx] = None;
        self.occupied = self.occupied.saturating_sub(1);
        Ok(())
    }

    /// Return the [`BindId`] whose key exactly matches `(modifier_mask,
    /// keycode)`, or `None` if no registered key matches.
    ///
    /// Matching is **exact mask equality**: a registration for `SUPER+Q`
    /// only fires when the modifier bitmask at event time is exactly
    /// `MOD_SUPER`; pressing `SUPER+SHIFT+Q` returns `None`.
    pub fn match_bind(&self, modifier_mask: u16, keycode: u32) -> Option<BindId> {
        let needle = BindKey {
            modifier_mask,
            keycode,
        };
        self.find_by_key(&needle)
    }

    /// Number of currently registered binds (`0..=MAX_BINDS`).
    pub fn len(&self) -> usize {
        self.occupied
    }

    /// True when no binds are registered.
    pub fn is_empty(&self) -> bool {
        self.occupied == 0
    }

    fn find_by_key(&self, key: &BindKey) -> Option<BindId> {
        for slot in &self.slots {
            if let Some(entry) = slot
                && entry.key == *key
            {
                return Some(entry.id);
            }
        }
        None
    }

    fn find_slot_by_id(&self, id: BindId) -> Option<usize> {
        for (i, slot) in self.slots.iter().enumerate() {
            if let Some(entry) = slot
                && entry.id == id
            {
                return Some(i);
            }
        }
        None
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct GrabEntry {
    keycode: u32,
    bind: BindId,
}

/// Per-keycode grab suppression policy. Independent of [`BindTable`]; the
/// dispatcher composes them.
///
/// Intent (D.3 dispatcher):
/// * On a `KeyDown` event whose `(modifier_mask, keycode)` matches a
///   registered bind, the dispatcher calls `start_grab(keycode, bind_id)`
///   *and* delivers the bind to the server-side handler — clients see no
///   key event for that down.
/// * On every subsequent `KeyRepeat` for that same keycode, the dispatcher
///   checks `is_grabbed(keycode).is_some()` and suppresses the event.
/// * On a `KeyUp` for that keycode the dispatcher calls
///   `clear_on_keyup(keycode)`; if it returned `true`, the up event is
///   suppressed (clients never saw the down, so they must not see the up).
///
/// Storage is a fixed `[Option<GrabEntry>; MAX_GRABS]` array; `start_grab`
/// past the cap returns `false` without displacing earlier grabs.
pub struct GrabState {
    grabs: [Option<GrabEntry>; MAX_GRABS],
}

impl Default for GrabState {
    fn default() -> Self {
        Self::new()
    }
}

impl GrabState {
    /// Construct an empty grab table.
    pub const fn new() -> Self {
        Self {
            grabs: [None; MAX_GRABS],
        }
    }

    /// Record a new key-down grab. Returns `true` on success.
    ///
    /// If `keycode` is already grabbed, the existing grab is **overwritten**
    /// — see the `re_grab_same_keycode_overwrites_bind` test for the
    /// rationale (a keycode can only have one outstanding grab at a time;
    /// the most recent `KeyDown` wins).
    ///
    /// If [`MAX_GRABS`] grabs are already active *and* `keycode` is not
    /// among them, returns `false` and emits no event. The dispatcher must
    /// treat that as "the grab did not take" and let the event flow
    /// through normal focus routing.
    pub fn start_grab(&mut self, keycode: u32, bind: BindId) -> bool {
        // If the keycode is already grabbed, overwrite the bind.
        for slot in &mut self.grabs {
            if let Some(entry) = slot
                && entry.keycode == keycode
            {
                entry.bind = bind;
                return true;
            }
        }
        // Otherwise find the first free slot.
        for slot in &mut self.grabs {
            if slot.is_none() {
                *slot = Some(GrabEntry { keycode, bind });
                return true;
            }
        }
        // Capacity exceeded.
        false
    }

    /// Returns `Some(bind)` if `keycode` currently has an active grab.
    pub fn is_grabbed(&self, keycode: u32) -> Option<BindId> {
        for slot in &self.grabs {
            if let Some(entry) = slot
                && entry.keycode == keycode
            {
                return Some(entry.bind);
            }
        }
        None
    }

    /// Clear any grab for `keycode`. Returns `true` if a grab was cleared,
    /// `false` if the keycode was not grabbed (idempotent on the
    /// no-grab-exists path).
    pub fn clear_on_keyup(&mut self, keycode: u32) -> bool {
        for slot in &mut self.grabs {
            if let Some(entry) = slot
                && entry.keycode == keycode
            {
                *slot = None;
                return true;
            }
        }
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
        let id = t.register(key(0, b'a' as u32)).unwrap();
        assert_eq!(t.match_bind(MOD_SHIFT, b'a' as u32), None);
        assert_eq!(t.match_bind(MOD_SUPER, b'a' as u32), None);
        assert_eq!(t.match_bind(0, b'a' as u32), Some(id));
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
                        if !still_present.is_empty() {
                            let pos = idx as usize % still_present.len();
                            let id = still_present[pos];
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
