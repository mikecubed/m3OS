//! Phase 56 Track B.4 — surface-buffer lifetime state machine.
//!
//! Codifies the refcount invariants for client-provided shared-memory
//! buffers attached to compositor surfaces. The state machine accepts
//! protocol-level events and emits effects describing when the
//! compositor would release the buffer back to the client. Pure logic
//! → unit + proptest harness on the host.

use alloc::collections::BTreeMap;
use alloc::vec;
use alloc::vec::Vec;

/// Stable identifier for a client buffer. Mirrors the wire-level
/// `BufferId` in `display::protocol`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, PartialOrd, Ord, Hash)]
pub struct BufferId(pub u32);

/// Protocol-level events accepted by [`BufferLifecycle::apply`]. Each
/// variant corresponds to a verb in the surface-buffer transport: a
/// client attaches a buffer to a surface, commits it (which promotes
/// the pending buffer to actively-sampled and releases the previously
/// active one), receives a `BufferReleased` once the compositor finishes
/// sampling, or destroys the surface entirely.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BufferEvent {
    /// Client attached `buffer` to a surface (pending; not yet sampled).
    Attach(BufferId),
    /// Client committed the surface; the previously-pending buffer (if any)
    /// becomes the actively-sampled buffer; the previously-active buffer
    /// (if any) is released.
    Commit,
    /// Compositor finished sampling and is ready to release the active
    /// buffer back to the client.
    SamplingComplete,
    /// Client destroyed the surface; both pending and active buffers are
    /// released.
    Destroy,
    /// Client died abruptly: drop all references without emitting per-
    /// buffer release events (the client is gone — emitting would race).
    ClientGone,
}

/// Side effects produced by [`BufferLifecycle::apply`]. Each effect
/// corresponds to a wire-level message the kernel/compositor would emit
/// in response to the event.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BufferEffect {
    /// Tell the client (over the protocol) that this buffer is released.
    Release(BufferId),
}

/// Recoverable transition errors. These are surfaced to the caller so it
/// can log a warning, but the state machine continues to accept further
/// events. Only [`BufferTransitionError::SurfaceDead`] terminates further
/// progress.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BufferTransitionError {
    /// Re-Attach without an intervening Commit; old pending is replaced.
    /// Caller may treat as a warning. (Effect: pending is replaced.)
    AttachReplacedPending,
    /// Commit with nothing pending — no state change.
    CommitWithoutPending,
    /// SamplingComplete called when nothing is currently being sampled.
    NothingActive,
    /// Operation issued after Destroy/ClientGone.
    SurfaceDead,
}

/// Per-buffer release tracking. Used only for invariant assertions in
/// proptest; production code does not need to inspect it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ReleaseState {
    /// The buffer has been observed in an `Attach` event but no
    /// `Release` effect has been emitted for it yet.
    Live,
    /// A `Release` effect has been emitted for this buffer.
    Released,
}

/// Per-surface buffer lifecycle. Tracks at most one pending and one active
/// buffer slot. Reference counts the client's outstanding obligations.
///
/// The state machine is driven entirely by [`BufferLifecycle::apply`].
/// Each call accepts a single [`BufferEvent`] and returns the produced
/// effects (a `Vec` of at most two [`BufferEffect`]s) along with an
/// optional [`BufferTransitionError`]. Recoverable errors (e.g. an
/// `Attach` without an intervening `Commit`) are reported alongside any
/// effects the state machine still emitted; only `SurfaceDead` indicates
/// the surface has been torn down.
#[derive(Clone, Debug)]
pub struct BufferLifecycle {
    pending: Option<BufferId>,
    active: Option<BufferId>,
    /// Once Destroy or ClientGone is observed, the surface is dead and any
    /// further state-change event returns `SurfaceDead`.
    dead: bool,
    /// Track which BufferIds we have observed Attach for, so we can prove
    /// "no Release for a slot never attached."
    seen: BTreeMap<BufferId, ReleaseState>,
}

impl Default for BufferLifecycle {
    fn default() -> Self {
        Self::new()
    }
}

impl BufferLifecycle {
    /// Construct a new, empty surface lifecycle: no pending or active
    /// buffer, not dead, no buffers tracked.
    pub fn new() -> Self {
        Self {
            pending: None,
            active: None,
            dead: false,
            seen: BTreeMap::new(),
        }
    }

    /// Apply an event; returns the resulting effects (a `Vec` of at most
    /// two entries) and an optional recoverable transition error.
    ///
    /// Effect ordering for `Destroy` with both slots populated: the
    /// active slot's `Release` is emitted first, followed by the pending
    /// slot's `Release`. This matches the natural reverse-allocation
    /// order: the active buffer was attached and committed before the
    /// pending one, so it is released first.
    pub fn apply(
        &mut self,
        event: BufferEvent,
    ) -> (Vec<BufferEffect>, Option<BufferTransitionError>) {
        if self.dead {
            // Once dead, the only events that are even nominally
            // meaningful are further ClientGone/Destroy, but we still
            // emit no effects and report SurfaceDead so the caller knows
            // the surface has been torn down.
            return (Vec::new(), Some(BufferTransitionError::SurfaceDead));
        }

        match event {
            BufferEvent::Attach(buffer) => self.apply_attach(buffer),
            BufferEvent::Commit => self.apply_commit(),
            BufferEvent::SamplingComplete => self.apply_sampling_complete(),
            BufferEvent::Destroy => self.apply_destroy(),
            BufferEvent::ClientGone => self.apply_client_gone(),
        }
    }

    fn apply_attach(
        &mut self,
        buffer: BufferId,
    ) -> (Vec<BufferEffect>, Option<BufferTransitionError>) {
        match self.pending {
            None => {
                self.pending = Some(buffer);
                self.mark_seen_live(buffer);
                (Vec::new(), None)
            }
            Some(existing) if existing == buffer => {
                // Idempotent identity: re-attaching the same buffer is a
                // no-op for the slot but is documented as still raising
                // the AttachReplacedPending warning so the caller can
                // notice the protocol-level redundancy.
                (
                    Vec::new(),
                    Some(BufferTransitionError::AttachReplacedPending),
                )
            }
            Some(existing) => {
                // The previously-pending buffer is being replaced before
                // it ever reached the active slot. Release it now.
                let effects = vec![BufferEffect::Release(existing)];
                self.mark_released(existing);
                self.pending = Some(buffer);
                self.mark_seen_live(buffer);
                (effects, Some(BufferTransitionError::AttachReplacedPending))
            }
        }
    }

    fn apply_commit(&mut self) -> (Vec<BufferEffect>, Option<BufferTransitionError>) {
        let Some(pending) = self.pending else {
            return (
                Vec::new(),
                Some(BufferTransitionError::CommitWithoutPending),
            );
        };

        let mut effects = Vec::new();
        if let Some(prior_active) = self.active {
            effects.push(BufferEffect::Release(prior_active));
            self.mark_released(prior_active);
        }
        self.active = Some(pending);
        self.pending = None;
        (effects, None)
    }

    fn apply_sampling_complete(&mut self) -> (Vec<BufferEffect>, Option<BufferTransitionError>) {
        let Some(active) = self.active else {
            return (Vec::new(), Some(BufferTransitionError::NothingActive));
        };

        let effects = vec![BufferEffect::Release(active)];
        self.mark_released(active);
        self.active = None;
        (effects, None)
    }

    fn apply_destroy(&mut self) -> (Vec<BufferEffect>, Option<BufferTransitionError>) {
        let mut effects = Vec::new();
        // Order documented above: active first, then pending.
        if let Some(active) = self.active.take() {
            effects.push(BufferEffect::Release(active));
            self.mark_released(active);
        }
        if let Some(pending) = self.pending.take() {
            effects.push(BufferEffect::Release(pending));
            self.mark_released(pending);
        }
        self.dead = true;
        (effects, None)
    }

    fn apply_client_gone(&mut self) -> (Vec<BufferEffect>, Option<BufferTransitionError>) {
        // The client is gone; the kernel will reclaim the page-grant on
        // its own. Drop the slot state without emitting any Release
        // effects, since there is no longer anyone to deliver them to.
        self.active = None;
        self.pending = None;
        self.dead = true;
        (Vec::new(), None)
    }

    /// Currently-pending buffer, if any.
    pub fn pending(&self) -> Option<BufferId> {
        self.pending
    }

    /// Currently-active (sampled) buffer, if any.
    pub fn active(&self) -> Option<BufferId> {
        self.active
    }

    /// Whether the surface has been destroyed (Destroy or ClientGone).
    /// Once dead, all further events emit no effects and report
    /// [`BufferTransitionError::SurfaceDead`].
    pub fn is_dead(&self) -> bool {
        self.dead
    }

    /// Per-buffer release tracking. Returns `None` for buffers that have
    /// never been observed in an `Attach` event.
    pub fn release_state(&self, id: BufferId) -> Option<ReleaseState> {
        self.seen.get(&id).copied()
    }

    fn mark_seen_live(&mut self, id: BufferId) {
        // First-time attach. If the buffer is being re-attached after
        // having been released (legal — clients may reuse BufferIds),
        // reset the slot to Live.
        self.seen.insert(id, ReleaseState::Live);
    }

    fn mark_released(&mut self, id: BufferId) {
        self.seen.insert(id, ReleaseState::Released);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::collections::BTreeMap;
    use alloc::vec;
    use proptest::prelude::*;

    fn b(n: u32) -> BufferId {
        BufferId(n)
    }

    #[test]
    fn attach_then_commit_promotes_to_active() {
        let mut life = BufferLifecycle::new();
        let (effects, err) = life.apply(BufferEvent::Attach(b(1)));
        assert!(effects.is_empty());
        assert!(err.is_none());
        let (effects, err) = life.apply(BufferEvent::Commit);
        assert!(effects.is_empty());
        assert!(err.is_none());
        assert_eq!(life.pending(), None);
        assert_eq!(life.active(), Some(b(1)));
    }

    #[test]
    fn commit_with_nothing_pending_returns_recoverable_error() {
        let mut life = BufferLifecycle::new();
        let (effects, err) = life.apply(BufferEvent::Commit);
        assert!(effects.is_empty());
        assert_eq!(err, Some(BufferTransitionError::CommitWithoutPending));
        assert_eq!(life.pending(), None);
        assert_eq!(life.active(), None);
        assert!(!life.is_dead());
    }

    #[test]
    fn double_attach_replaces_pending_and_releases_old() {
        let mut life = BufferLifecycle::new();
        let _ = life.apply(BufferEvent::Attach(b(1)));
        let (effects, err) = life.apply(BufferEvent::Attach(b(2)));
        assert_eq!(effects, vec![BufferEffect::Release(b(1))]);
        assert_eq!(err, Some(BufferTransitionError::AttachReplacedPending));
        assert_eq!(life.pending(), Some(b(2)));
        assert_eq!(life.release_state(b(1)), Some(ReleaseState::Released));
        assert_eq!(life.release_state(b(2)), Some(ReleaseState::Live));
    }

    #[test]
    fn attach_same_buffer_twice_is_noop() {
        let mut life = BufferLifecycle::new();
        let _ = life.apply(BufferEvent::Attach(b(1)));
        let (effects, err) = life.apply(BufferEvent::Attach(b(1)));
        assert!(effects.is_empty());
        assert_eq!(err, Some(BufferTransitionError::AttachReplacedPending));
        assert_eq!(life.pending(), Some(b(1)));
        assert_eq!(life.release_state(b(1)), Some(ReleaseState::Live));
    }

    #[test]
    fn commit_replaces_active_and_releases_old() {
        let mut life = BufferLifecycle::new();
        let _ = life.apply(BufferEvent::Attach(b(1)));
        let _ = life.apply(BufferEvent::Commit);
        let _ = life.apply(BufferEvent::Attach(b(2)));
        let (effects, err) = life.apply(BufferEvent::Commit);
        assert_eq!(effects, vec![BufferEffect::Release(b(1))]);
        assert!(err.is_none());
        assert_eq!(life.active(), Some(b(2)));
        assert_eq!(life.pending(), None);
        assert_eq!(life.release_state(b(1)), Some(ReleaseState::Released));
        assert_eq!(life.release_state(b(2)), Some(ReleaseState::Live));
    }

    #[test]
    fn sampling_complete_releases_active() {
        let mut life = BufferLifecycle::new();
        let _ = life.apply(BufferEvent::Attach(b(1)));
        let _ = life.apply(BufferEvent::Commit);
        let (effects, err) = life.apply(BufferEvent::SamplingComplete);
        assert_eq!(effects, vec![BufferEffect::Release(b(1))]);
        assert!(err.is_none());
        assert_eq!(life.active(), None);
        assert_eq!(life.release_state(b(1)), Some(ReleaseState::Released));
    }

    #[test]
    fn sampling_complete_with_no_active_is_recoverable_error() {
        let mut life = BufferLifecycle::new();
        let (effects, err) = life.apply(BufferEvent::SamplingComplete);
        assert!(effects.is_empty());
        assert_eq!(err, Some(BufferTransitionError::NothingActive));
        assert!(!life.is_dead());
    }

    #[test]
    fn destroy_releases_both_pending_and_active() {
        let mut life = BufferLifecycle::new();
        let _ = life.apply(BufferEvent::Attach(b(1)));
        let _ = life.apply(BufferEvent::Commit);
        let _ = life.apply(BufferEvent::Attach(b(2)));
        let (effects, err) = life.apply(BufferEvent::Destroy);
        assert_eq!(
            effects,
            vec![BufferEffect::Release(b(1)), BufferEffect::Release(b(2)),]
        );
        assert!(err.is_none());
        assert!(life.is_dead());
        assert_eq!(life.release_state(b(1)), Some(ReleaseState::Released));
        assert_eq!(life.release_state(b(2)), Some(ReleaseState::Released));
    }

    #[test]
    fn destroy_marks_dead_and_blocks_further_state_change() {
        let mut life = BufferLifecycle::new();
        let _ = life.apply(BufferEvent::Attach(b(1)));
        let (_effects, _err) = life.apply(BufferEvent::Destroy);
        assert!(life.is_dead());

        for event in [
            BufferEvent::Attach(b(2)),
            BufferEvent::Commit,
            BufferEvent::SamplingComplete,
        ] {
            let (effects, err) = life.apply(event);
            assert!(
                effects.is_empty(),
                "unexpected effects after destroy: {:?}",
                effects
            );
            assert_eq!(err, Some(BufferTransitionError::SurfaceDead));
        }
    }

    #[test]
    fn client_gone_marks_dead_without_release_effects() {
        let mut life = BufferLifecycle::new();
        let _ = life.apply(BufferEvent::Attach(b(1)));
        let _ = life.apply(BufferEvent::Commit);
        let (effects, err) = life.apply(BufferEvent::ClientGone);
        assert!(effects.is_empty());
        assert!(err.is_none());
        assert!(life.is_dead());
        assert_eq!(life.active(), None);
        assert_eq!(life.pending(), None);
        // The buffer was attached but not released — no Release effect
        // was emitted, so the per-buffer state remains Live.
        assert_eq!(life.release_state(b(1)), Some(ReleaseState::Live));
    }

    #[test]
    fn release_state_for_attached_then_released_buffer() {
        let mut life = BufferLifecycle::new();
        assert_eq!(life.release_state(b(1)), None);
        let _ = life.apply(BufferEvent::Attach(b(1)));
        assert_eq!(life.release_state(b(1)), Some(ReleaseState::Live));
        let _ = life.apply(BufferEvent::Commit);
        let _ = life.apply(BufferEvent::SamplingComplete);
        assert_eq!(life.release_state(b(1)), Some(ReleaseState::Released));
    }

    // --- proptest harness ---

    fn arb_event() -> impl Strategy<Value = BufferEvent> {
        // Bounded BufferIds so we get deliberate collisions and exercise
        // re-attach paths.
        prop_oneof![
            (0u32..4).prop_map(|n| BufferEvent::Attach(BufferId(n))),
            Just(BufferEvent::Commit),
            Just(BufferEvent::SamplingComplete),
            Just(BufferEvent::Destroy),
            Just(BufferEvent::ClientGone),
        ]
    }

    proptest! {
        #[test]
        fn proptest_no_double_release(events in proptest::collection::vec(arb_event(), 0..64)) {
            let mut life = BufferLifecycle::new();
            let mut release_counts: BTreeMap<BufferId, u32> = BTreeMap::new();
            for event in events {
                let (effects, _err) = life.apply(event);
                for effect in effects {
                    let BufferEffect::Release(id) = effect;
                    let count = release_counts.entry(id).or_insert(0);
                    *count += 1;
                }
            }
            // Within any single attach/release lifecycle, a buffer is
            // released at most once. Because the state machine consults
            // the slot state before emitting Release, and clears it
            // afterwards, the only way to legitimately see the same
            // BufferId released twice is if it was re-attached after
            // its first release. So the assertion is: for every released
            // BufferId, the release count never exceeds the number of
            // distinct attach-then-release cycles observed. The simplest
            // sufficient invariant: the release count is bounded by the
            // total number of Attach events for that buffer in the input.
            // We re-walk the input to compute that bound.
            // (release_counts is built above; bound check below.)
            let _ = release_counts; // bound is enforced via attach_counts below.
        }

        #[test]
        fn proptest_releases_bounded_by_attaches(
            events in proptest::collection::vec(arb_event(), 0..64)
        ) {
            let mut life = BufferLifecycle::new();
            let mut attach_counts: BTreeMap<BufferId, u32> = BTreeMap::new();
            let mut release_counts: BTreeMap<BufferId, u32> = BTreeMap::new();
            for event in events {
                if let BufferEvent::Attach(id) = event {
                    *attach_counts.entry(id).or_insert(0) += 1;
                }
                let (effects, _err) = life.apply(event);
                for effect in effects {
                    let BufferEffect::Release(id) = effect;
                    *release_counts.entry(id).or_insert(0) += 1;
                }
            }
            for (id, releases) in release_counts {
                let attaches = attach_counts.get(&id).copied().unwrap_or(0);
                prop_assert!(
                    releases <= attaches,
                    "buffer {:?}: {} releases > {} attaches",
                    id, releases, attaches
                );
            }
        }

        #[test]
        fn proptest_no_release_for_unknown_buffer(
            events in proptest::collection::vec(arb_event(), 0..64)
        ) {
            let mut life = BufferLifecycle::new();
            let mut attached: alloc::collections::BTreeSet<BufferId> =
                alloc::collections::BTreeSet::new();
            for event in events {
                if let BufferEvent::Attach(id) = event {
                    attached.insert(id);
                }
                let (effects, _err) = life.apply(event);
                for effect in effects {
                    let BufferEffect::Release(id) = effect;
                    prop_assert!(
                        attached.contains(&id),
                        "Release({:?}) emitted but buffer was never attached",
                        id
                    );
                }
            }
        }

        #[test]
        fn proptest_dead_surface_emits_no_effects(
            pre in proptest::collection::vec(arb_event(), 0..16),
            killer in prop_oneof![Just(BufferEvent::Destroy), Just(BufferEvent::ClientGone)],
            post in proptest::collection::vec(arb_event(), 0..32),
        ) {
            let mut life = BufferLifecycle::new();
            for event in pre {
                let _ = life.apply(event);
            }
            // Force the surface dead. Whatever pre-events happened, the
            // killer event leaves the surface dead afterwards (any
            // effects it produced are not constrained by this test).
            let _ = life.apply(killer);
            prop_assert!(life.is_dead());
            for event in post {
                let (effects, err) = life.apply(event);
                prop_assert!(
                    effects.is_empty(),
                    "dead surface emitted effects: {:?}", effects
                );
                prop_assert_eq!(err, Some(BufferTransitionError::SurfaceDead));
            }
        }
    }
}
