//! Pure-logic bound-notification state table.
//!
//! Tracks the 1:1 binding between a [`NotifId`] (notification object) and a
//! [`TaskId`] (TCB). A notification may be bound to at most one TCB; a TCB may
//! be bound to at most one notification. Binding the same pair twice is
//! idempotent.
//!
//! This module contains no unsafe code, no kernel-only types, and no I/O — it
//! is intentionally host-testable via `cargo test -p kernel-core`.

use alloc::vec::Vec;

use crate::types::{NotifId, TaskId};

/// Maximum number of notification slots tracked by the table.
///
/// Mirrors `MAX_NOTIFS` used by the kernel's `BOUND_TCB` array (Track B).
pub const MAX_NOTIFS: usize = 64;

/// Error returned when [`BoundNotifTable::bind`] cannot proceed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindError {
    /// The notification is already bound to a different TCB, or the TCB is
    /// already bound to a different notification.
    Busy,
}

/// Pure-logic model of the bound-notification state machine.
///
/// # Invariants
///
/// - A [`NotifId`] is bound to at most one [`TaskId`].
/// - A [`TaskId`] is bound to at most one [`NotifId`].
/// - Binding the same `(notif, tcb)` pair twice returns `Ok(())` (idempotent).
///
/// The backing array is indexed by `notif.0`; the kernel's ISR-safe version
/// uses `AtomicI32` in the same indexed position (Track B). Keeping the same
/// indexing scheme lets tests on the pure-logic model predict kernel behaviour.
pub struct BoundNotifTable {
    /// `slots[notif.0]` is the TCB currently bound to that notification object,
    /// or `None` if the slot is unbound.
    slots: [Option<TaskId>; MAX_NOTIFS],
}

impl BoundNotifTable {
    /// Create a new empty table (all slots unbound).
    pub const fn new() -> Self {
        Self {
            slots: [None; MAX_NOTIFS],
        }
    }

    /// Bind `notif` to `tcb`.
    ///
    /// Returns `Ok(())` on success or if the same pair is already bound.
    /// Returns [`BindError::Busy`] if:
    /// - `notif` is already bound to a *different* TCB, or
    /// - `tcb` is already bound to a *different* notification.
    pub fn bind(&mut self, notif: NotifId, tcb: TaskId) -> Result<(), BindError> {
        let idx = notif.0 as usize;
        match self.slots[idx] {
            Some(bound) if bound == tcb => return Ok(()), // idempotent
            Some(_) => return Err(BindError::Busy),       // notif taken by different TCB
            None => {}
        }
        // Enforce the TCB side of the 1:1 constraint.
        for (i, slot) in self.slots.iter().enumerate() {
            if i == idx {
                continue;
            }
            if *slot == Some(tcb) {
                return Err(BindError::Busy); // TCB already bound to a different notif
            }
        }
        self.slots[idx] = Some(tcb);
        Ok(())
    }

    /// Remove the binding for `notif` and return the `TaskId` that was bound,
    /// or `None` if the slot was already unbound.
    pub fn unbind(&mut self, notif: NotifId) -> Option<TaskId> {
        self.slots[notif.0 as usize].take()
    }

    /// Remove all bindings owned by `tcb` and return the freed [`NotifId`]s.
    ///
    /// Called during TCB teardown to ensure no dangling notification bindings
    /// remain.
    pub fn unbind_tcb(&mut self, tcb: TaskId) -> Vec<NotifId> {
        let mut freed = Vec::new();
        for (i, slot) in self.slots.iter_mut().enumerate() {
            if *slot == Some(tcb) {
                *slot = None;
                freed.push(NotifId(i as u8));
            }
        }
        freed
    }

    /// Treat `notif` as destroyed: clear its binding and return the `TaskId`
    /// that was bound, if any.
    ///
    /// Semantically equivalent to [`Self::unbind`]; provided as a distinct
    /// entry point so the kernel can call it from the notif-free path without
    /// ambiguity.
    pub fn notif_free(&mut self, notif: NotifId) -> Option<TaskId> {
        self.unbind(notif)
    }

    /// Return the `TaskId` currently bound to `notif`, or `None`.
    pub fn lookup(&self, notif: NotifId) -> Option<TaskId> {
        self.slots[notif.0 as usize]
    }
}

impl Default for BoundNotifTable {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(id: u8) -> NotifId {
        NotifId(id)
    }
    fn t(id: u64) -> TaskId {
        TaskId(id)
    }

    // --- A.1 acceptance tests (committed red before implementation) ---

    #[test]
    fn bind_then_rebind_same_pair_is_idempotent() {
        let mut table = BoundNotifTable::new();
        assert!(table.bind(n(0), t(1)).is_ok());
        assert!(
            table.bind(n(0), t(1)).is_ok(),
            "second bind of the same pair must succeed"
        );
        assert_eq!(table.lookup(n(0)), Some(t(1)));
    }

    #[test]
    fn bind_same_notif_to_different_tcb_returns_busy() {
        let mut table = BoundNotifTable::new();
        table.bind(n(0), t(1)).unwrap();
        assert_eq!(table.bind(n(0), t(2)), Err(BindError::Busy));
    }

    #[test]
    fn bind_different_notif_to_same_tcb_returns_busy() {
        let mut table = BoundNotifTable::new();
        table.bind(n(0), t(1)).unwrap();
        assert_eq!(table.bind(n(1), t(1)), Err(BindError::Busy));
    }

    #[test]
    fn unbind_clears_slot() {
        let mut table = BoundNotifTable::new();
        table.bind(n(2), t(5)).unwrap();
        let was = table.unbind(n(2));
        assert_eq!(was, Some(t(5)));
        // Slot is now free; re-binding must succeed.
        assert!(table.bind(n(2), t(9)).is_ok());
    }

    #[test]
    fn tcb_drop_clears_all_bindings_owned_by_tcb() {
        let mut table = BoundNotifTable::new();
        table.bind(n(3), t(10)).unwrap();
        let freed = table.unbind_tcb(t(10));
        assert!(freed.contains(&n(3)));
        // Slot must be free now.
        assert!(table.bind(n(3), t(99)).is_ok());
    }

    #[test]
    fn notif_free_clears_binding_and_returns_tcb() {
        let mut table = BoundNotifTable::new();
        table.bind(n(5), t(20)).unwrap();
        let freed_tcb = table.notif_free(n(5));
        assert_eq!(freed_tcb, Some(t(20)));
        // Slot must be clear.
        assert!(table.bind(n(5), t(21)).is_ok());
    }

    // --- additional coverage ---

    #[test]
    fn unbind_on_unbound_slot_returns_none() {
        let mut table = BoundNotifTable::new();
        assert_eq!(table.unbind(n(0)), None);
    }

    #[test]
    fn lookup_returns_bound_tcb() {
        let mut table = BoundNotifTable::new();
        table.bind(n(7), t(42)).unwrap();
        assert_eq!(table.lookup(n(7)), Some(t(42)));
    }

    #[test]
    fn lookup_returns_none_after_unbind() {
        let mut table = BoundNotifTable::new();
        table.bind(n(1), t(3)).unwrap();
        table.unbind(n(1));
        assert_eq!(table.lookup(n(1)), None);
    }

    #[test]
    fn rebind_after_unbind_succeeds() {
        let mut table = BoundNotifTable::new();
        table.bind(n(0), t(1)).unwrap();
        table.unbind(n(0));
        // After unbind the TCB side is also free, so both must succeed.
        assert!(table.bind(n(0), t(2)).is_ok());
    }

    #[test]
    fn unbind_tcb_with_multiple_notifs_impossible() {
        // This tests that the 1:1 invariant is maintained across unbind_tcb:
        // a single TCB can only be bound to one notif at a time.
        let mut table = BoundNotifTable::new();
        table.bind(n(0), t(1)).unwrap();
        // Attempt to bind same TCB to another notif is rejected.
        assert_eq!(table.bind(n(1), t(1)), Err(BindError::Busy));
        // unbind_tcb still clears what is there.
        let freed = table.unbind_tcb(t(1));
        assert_eq!(freed, alloc::vec![n(0)]);
        assert_eq!(table.lookup(n(0)), None);
    }

    #[test]
    fn notif_free_on_unbound_slot_returns_none() {
        let mut table = BoundNotifTable::new();
        assert_eq!(table.notif_free(n(63)), None);
    }
}
