//! Phase 68 Track A — control-socket subscription registry and event push.
//!
//! `display_server` records `ControlEvent` messages in a per-subscriber
//! ring when state changes (a surface is created, focus moves, a bind
//! fires, …). [`flush_subscriber_ring`] is the publish-side companion
//! that drains those rings through a transport callback. Phase 56
//! shipped the registry but not the flush step; Phase 68 closes that
//! gap so subscribed clients actually observe state-change events
//! rather than the events sitting in memory until the server exits.
//!
//! ## Why this lives in `kernel-core`
//!
//! `display_server` is `no_std` + `no_main` — its crate root cannot
//! be compiled with the host `cargo test` harness. Putting the
//! pure-logic registry, the flush function, and the
//! `EventKind` <-> index mapping in `kernel-core` lets the host tests
//! exercise the publish → flush → subscriber-receives flow without a
//! QEMU boot.
//!
//! ## What still lives in `display_server`
//!
//! The convenience `publish_surface_created` / `publish_layer_event`
//! / … wrappers stay in `userspace/display_server/src/control.rs`
//! because they look up surface roles from the compositor's
//! `SurfaceRegistry` (which is `display_server`-internal). They
//! delegate the heavy lifting to [`publish_to_subscribers`] below.

extern crate alloc;

use alloc::collections::{BTreeMap, VecDeque};
use alloc::vec::Vec;

use crate::display::protocol::{ControlErrorCode, ControlEvent, EventKind};

/// Stable identifier for one connected control client. Phase 56 has a
/// single in-process connection; the subscription registry is keyed on
/// this so a future multi-client world can keep the API shape.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub struct ClientId(pub u32);

/// Number of subscribable [`EventKind`] variants Phase 68 supports.
///
/// Phase 56 shipped four (`SurfaceCreated`, `SurfaceDestroyed`,
/// `FocusChanged`, `BindTriggered`). Phase 68 adds two more
/// (`LayerEvent`, `CursorEvent`).
pub const NUM_SUBSCRIBABLE_KINDS: usize = 6;

/// Stable list of subscribable event kinds in the same order as the
/// [`event_kind_index`] mapping. Exposed for docs / future iteration
/// helpers; the publish path indexes directly via [`event_kind_index`].
pub const SUBSCRIBABLE_EVENT_KINDS: [EventKind; NUM_SUBSCRIBABLE_KINDS] = [
    EventKind::SurfaceCreated,
    EventKind::SurfaceDestroyed,
    EventKind::FocusChanged,
    EventKind::BindTriggered,
    EventKind::LayerEvent,
    EventKind::CursorEvent,
];

/// Convert an [`EventKind`] to a small index into the subscription
/// table. The mapping is stable as long as the Phase 56 / Phase 68
/// wire format is stable. The match is exhaustive within this crate;
/// adding a new `EventKind` variant requires updating this function in
/// the same change set so the table layout stays consistent.
pub fn event_kind_index(kind: EventKind) -> Option<usize> {
    match kind {
        EventKind::SurfaceCreated => Some(0),
        EventKind::SurfaceDestroyed => Some(1),
        EventKind::FocusChanged => Some(2),
        EventKind::BindTriggered => Some(3),
        EventKind::LayerEvent => Some(4),
        EventKind::CursorEvent => Some(5),
    }
}

/// Map a [`ControlEvent`] to its corresponding [`EventKind`], or
/// `None` for non-subscribable variants (replies).
pub fn event_kind_of(event: &ControlEvent) -> Option<EventKind> {
    match event {
        ControlEvent::SurfaceCreated { .. } => Some(EventKind::SurfaceCreated),
        ControlEvent::SurfaceDestroyed { .. } => Some(EventKind::SurfaceDestroyed),
        ControlEvent::FocusChanged { .. } => Some(EventKind::FocusChanged),
        ControlEvent::BindTriggered { .. } => Some(EventKind::BindTriggered),
        ControlEvent::LayerEvent { .. } => Some(EventKind::LayerEvent),
        ControlEvent::CursorEvent { .. } => Some(EventKind::CursorEvent),
        ControlEvent::VersionReply { .. }
        | ControlEvent::SurfaceListReply { .. }
        | ControlEvent::Ack
        | ControlEvent::Error { .. }
        | ControlEvent::FrameStatsReply { .. }
        | ControlEvent::PixelReply { .. } => None,
    }
}

/// Maximum number of subscribers per `EventKind`. Over-cap is rejected
/// with [`ControlErrorCode::ResourceExhausted`] rather than allowing
/// the registry to grow unboundedly.
pub const MAX_SUBSCRIBERS_PER_KIND: usize = 16;

/// Maximum number of pending events per subscriber's outbound queue.
/// Over-cap drops the *oldest* queued event so the queue stays bounded
/// without dropping the newest event the client is most likely to
/// care about.
pub const MAX_OUTBOUND_PER_SUBSCRIBER: usize = 32;

/// Outcome of a single sender callback invocation inside
/// [`flush_subscriber_ring`].
///
/// `WouldBlock` mirrors the kernel-level `-EAGAIN` return from
/// `sys_send`: the event cannot be transmitted on this transport right
/// now. [`flush_subscriber_ring`] drops the offending event,
/// increments [`ControlSubscriptions::events_dropped`], and continues
/// with the next event in the queue.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FlushError {
    /// The transport refused the event (`sys_send` returned `-EAGAIN`).
    WouldBlock,
}

/// Registry of which [`ClientId`] is subscribed to which [`EventKind`].
///
/// Keyed by `EventKind` (via a fixed-size array indexed by variant
/// discriminant — `EventKind` deliberately does not derive `Ord`
/// because it is a stable wire-format enum) so the publish side
/// ("push this event to all subscribers of `SurfaceCreated`") is
/// O(subscribers), not O(clients). Each subscriber has its own
/// pending-event queue so a slow drain on one client cannot block
/// another.
pub struct ControlSubscriptions {
    /// Per-kind subscriber lists. `subscribers[i]` is the list for
    /// [`SUBSCRIBABLE_EVENT_KINDS`]`[i]`. Each list is bounded by
    /// [`MAX_SUBSCRIBERS_PER_KIND`].
    subscribers: [Vec<ClientId>; NUM_SUBSCRIBABLE_KINDS],
    pending_events: BTreeMap<ClientId, VecDeque<ControlEvent>>,
    /// Phase 68 Track A.1 — cumulative count of events dropped during
    /// a [`flush_subscriber_ring`] call because the transport reported
    /// [`FlushError::WouldBlock`].
    events_dropped: u64,
}

impl Default for ControlSubscriptions {
    fn default() -> Self {
        Self::new()
    }
}

impl ControlSubscriptions {
    /// Construct an empty registry.
    pub fn new() -> Self {
        Self {
            subscribers: [
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
            ],
            pending_events: BTreeMap::new(),
            events_dropped: 0,
        }
    }

    /// Read the cumulative dropped-event counter.
    pub fn events_dropped(&self) -> u64 {
        self.events_dropped
    }

    /// Subscribe `client` to events of `kind`. Idempotent: a second
    /// call for the same `(client, kind)` pair is a no-op (returns
    /// `Ok(())`). Over-cap returns
    /// `Err(ControlErrorCode::ResourceExhausted)`. A future
    /// `EventKind` variant that is not yet supported returns
    /// `Err(ControlErrorCode::BadArgs)` (the verb is known; the
    /// argument is not).
    pub fn subscribe(&mut self, client: ClientId, kind: EventKind) -> Result<(), ControlErrorCode> {
        let idx = match event_kind_index(kind) {
            Some(i) => i,
            None => return Err(ControlErrorCode::BadArgs),
        };
        let list = &mut self.subscribers[idx];
        if list.contains(&client) {
            return Ok(());
        }
        if list.len() >= MAX_SUBSCRIBERS_PER_KIND {
            return Err(ControlErrorCode::ResourceExhausted);
        }
        list.push(client);
        self.pending_events.entry(client).or_default();
        Ok(())
    }

    /// Remove `client` from the subscriber list of `kind`. Idempotent.
    pub fn unsubscribe(&mut self, client: ClientId, kind: EventKind) {
        if let Some(idx) = event_kind_index(kind) {
            self.subscribers[idx].retain(|c| *c != client);
        }
    }

    /// Forget `client` entirely. Removes the per-client queue and
    /// every subscription. Used when a control connection closes.
    pub fn forget_client(&mut self, client: ClientId) {
        self.pending_events.remove(&client);
        for list in self.subscribers.iter_mut() {
            list.retain(|c| *c != client);
        }
    }

    /// Publish an event to every subscriber of its `kind`. Each
    /// subscriber receives a copy on its own outbound queue.
    ///
    /// Per-queue cap [`MAX_OUTBOUND_PER_SUBSCRIBER`] is enforced by
    /// dropping the *oldest* queued event before pushing the new one.
    pub fn publish(&mut self, event: ControlEvent) {
        let kind = match event_kind_of(&event) {
            Some(k) => k,
            None => return,
        };
        let idx = match event_kind_index(kind) {
            Some(i) => i,
            None => return,
        };
        let targets: Vec<ClientId> = self.subscribers[idx].clone();
        for client in targets {
            let queue = self.pending_events.entry(client).or_default();
            if queue.len() >= MAX_OUTBOUND_PER_SUBSCRIBER {
                queue.pop_front();
            }
            queue.push_back(event.clone());
        }
    }

    /// Drain the next pending event for `client`, if any. Returned in
    /// FIFO order. Public so the IPC pull verb (if any) can drain
    /// without going through [`flush_subscriber_ring`].
    pub fn drain_one(&mut self, client: ClientId) -> Option<ControlEvent> {
        self.pending_events.get_mut(&client)?.pop_front()
    }

    /// Number of pending events queued for `client`.
    pub fn pending_count(&self, client: ClientId) -> usize {
        self.pending_events
            .get(&client)
            .map(|q| q.len())
            .unwrap_or(0)
    }

    /// Number of subscribers registered for `kind`.
    pub fn subscriber_count(&self, kind: EventKind) -> usize {
        match event_kind_index(kind) {
            Some(i) => self.subscribers[i].len(),
            None => 0,
        }
    }

    /// List the subscribers currently registered for `kind`. Used by
    /// the publish path to iterate matching clients.
    pub fn subscribers_for(&self, kind: EventKind) -> &[ClientId] {
        match event_kind_index(kind) {
            Some(i) => &self.subscribers[i],
            None => &[],
        }
    }

    fn record_drop(&mut self) {
        self.events_dropped = self.events_dropped.saturating_add(1);
    }
}

/// Drain `client`'s pending-event ring into the transport `send_fn`.
///
/// Iterates the per-client ring in FIFO order. For each event:
/// * `Ok(())` — the transport accepted the event; continue to the next.
/// * `Err(FlushError::WouldBlock)` — the transport rejected the event
///   (`sys_send` returned `-EAGAIN`). The event is dropped, the
///   `events_dropped` counter on `subs` is incremented by one, and the
///   loop continues with the next event.
///
/// Returns the number of events that drained successfully. The dropped
/// count is observable via [`ControlSubscriptions::events_dropped`].
pub fn flush_subscriber_ring<F>(
    subs: &mut ControlSubscriptions,
    client: ClientId,
    mut send_fn: F,
) -> usize
where
    F: FnMut(&ControlEvent) -> Result<(), FlushError>,
{
    let mut sent = 0usize;
    while let Some(event) = subs.drain_one(client) {
        match send_fn(&event) {
            Ok(()) => {
                sent += 1;
            }
            Err(FlushError::WouldBlock) => {
                subs.record_drop();
            }
        }
    }
    sent
}

/// Enqueue `event` for every subscriber of its kind, then immediately
/// flush each subscriber's ring through `send_fn`.
///
/// The publish path is "queue, then drain" rather than "send directly"
/// so the bounded-queue drop-oldest behaviour applies even when the
/// transport accepts every event — a slow consumer cannot wedge a
/// fast publisher.
pub fn publish_to_subscribers<F>(
    subs: &mut ControlSubscriptions,
    event: ControlEvent,
    mut send_fn: F,
) where
    F: FnMut(ClientId, &ControlEvent) -> Result<(), FlushError>,
{
    let Some(kind) = event_kind_of(&event) else {
        return;
    };
    let Some(idx) = event_kind_index(kind) else {
        return;
    };
    subs.publish(event);
    let targets: Vec<ClientId> = subs.subscribers[idx].clone();
    for client in targets {
        flush_subscriber_ring(subs, client, |evt| send_fn(client, evt));
    }
}

/// Sender callback that never transmits: every event is reported as
/// [`FlushError::WouldBlock`], which causes [`flush_subscriber_ring`]
/// to drop the event and bump the dropped-event counter. Useful as
/// the default wiring in `display_server::main` while per-subscriber
/// transport caps are not yet captured at `Subscribe` time.
pub fn null_subscriber_sender(_client: ClientId, _event: &ControlEvent) -> Result<(), FlushError> {
    Err(FlushError::WouldBlock)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display::protocol::{KeyboardInteractivity, SurfaceId, SurfaceRoleTag};
    use alloc::vec;
    use core::cell::RefCell;

    fn surface_created(id: u32) -> ControlEvent {
        ControlEvent::SurfaceCreated {
            surface_id: SurfaceId(id),
            role: SurfaceRoleTag::Toplevel,
        }
    }

    #[test]
    fn flush_drains_in_fifo_order() {
        let mut subs = ControlSubscriptions::new();
        let client = ClientId(7);
        subs.subscribe(client, EventKind::SurfaceCreated).unwrap();
        subs.publish(surface_created(1));
        subs.publish(surface_created(2));
        subs.publish(surface_created(3));
        let received: RefCell<Vec<u32>> = RefCell::new(Vec::new());
        let sent = flush_subscriber_ring(&mut subs, client, |evt| {
            if let ControlEvent::SurfaceCreated { surface_id, .. } = evt {
                received.borrow_mut().push(surface_id.0);
            }
            Ok(())
        });
        assert_eq!(sent, 3);
        assert_eq!(*received.borrow(), vec![1, 2, 3]);
        assert_eq!(subs.pending_count(client), 0);
        assert_eq!(subs.events_dropped(), 0);
    }

    #[test]
    fn flush_drops_on_would_block_and_increments_counter() {
        let mut subs = ControlSubscriptions::new();
        let client = ClientId(7);
        subs.subscribe(client, EventKind::SurfaceCreated).unwrap();
        subs.publish(surface_created(1));
        subs.publish(surface_created(2));
        let sent = flush_subscriber_ring(&mut subs, client, |_evt| Err(FlushError::WouldBlock));
        assert_eq!(sent, 0);
        assert_eq!(subs.pending_count(client), 0);
        assert_eq!(subs.events_dropped(), 2);
    }

    #[test]
    fn flush_continues_after_individual_drop() {
        let mut subs = ControlSubscriptions::new();
        let client = ClientId(7);
        subs.subscribe(client, EventKind::SurfaceCreated).unwrap();
        subs.publish(surface_created(1));
        subs.publish(surface_created(2));
        subs.publish(surface_created(3));
        let received: RefCell<Vec<u32>> = RefCell::new(Vec::new());
        let call_count: RefCell<u32> = RefCell::new(0);
        let sent = flush_subscriber_ring(&mut subs, client, |evt| {
            let mut count = call_count.borrow_mut();
            *count += 1;
            // Reject the second event; accept the others.
            if *count == 2 {
                return Err(FlushError::WouldBlock);
            }
            if let ControlEvent::SurfaceCreated { surface_id, .. } = evt {
                received.borrow_mut().push(surface_id.0);
            }
            Ok(())
        });
        assert_eq!(sent, 2);
        assert_eq!(*received.borrow(), vec![1, 3]);
        assert_eq!(subs.events_dropped(), 1);
    }

    #[test]
    fn publish_to_subscribers_delivers_to_all_subscribers_of_kind() {
        let mut subs = ControlSubscriptions::new();
        let a = ClientId(1);
        let b = ClientId(2);
        subs.subscribe(a, EventKind::SurfaceCreated).unwrap();
        subs.subscribe(b, EventKind::SurfaceCreated).unwrap();
        let received: RefCell<Vec<(u32, u32)>> = RefCell::new(Vec::new());
        publish_to_subscribers(&mut subs, surface_created(42), |client, evt| {
            if let ControlEvent::SurfaceCreated { surface_id, .. } = evt {
                received.borrow_mut().push((client.0, surface_id.0));
            }
            Ok(())
        });
        let mut got = received.borrow().clone();
        got.sort();
        assert_eq!(got, vec![(1, 42), (2, 42)]);
        assert_eq!(subs.events_dropped(), 0);
    }

    #[test]
    fn publish_to_subscribers_skips_non_subscribers() {
        let mut subs = ControlSubscriptions::new();
        let a = ClientId(1);
        subs.subscribe(a, EventKind::FocusChanged).unwrap();
        // Publish a SurfaceCreated; A is subscribed only to FocusChanged.
        let received: RefCell<Vec<u32>> = RefCell::new(Vec::new());
        publish_to_subscribers(&mut subs, surface_created(99), |_client, evt| {
            if let ControlEvent::SurfaceCreated { surface_id, .. } = evt {
                received.borrow_mut().push(surface_id.0);
            }
            Ok(())
        });
        assert!(received.borrow().is_empty());
        assert_eq!(subs.pending_count(a), 0);
    }

    #[test]
    fn publish_to_subscribers_delivers_layer_event() {
        let mut subs = ControlSubscriptions::new();
        let a = ClientId(1);
        subs.subscribe(a, EventKind::LayerEvent).unwrap();
        let received: RefCell<Option<ControlEvent>> = RefCell::new(None);
        publish_to_subscribers(
            &mut subs,
            ControlEvent::LayerEvent {
                surface_id: SurfaceId(5),
                anchor_mask: 0x0F,
                exclusive_zone: 24,
                keyboard_interactivity: KeyboardInteractivity::Exclusive,
            },
            |_client, evt| {
                *received.borrow_mut() = Some(evt.clone());
                Ok(())
            },
        );
        let got = received.borrow().clone().expect("layer event delivered");
        match got {
            ControlEvent::LayerEvent {
                surface_id,
                anchor_mask,
                exclusive_zone,
                keyboard_interactivity,
            } => {
                assert_eq!(surface_id.0, 5);
                assert_eq!(anchor_mask, 0x0F);
                assert_eq!(exclusive_zone, 24);
                assert_eq!(keyboard_interactivity, KeyboardInteractivity::Exclusive);
            }
            other => panic!("expected LayerEvent, got {other:?}"),
        }
    }

    #[test]
    fn publish_to_subscribers_delivers_cursor_event() {
        let mut subs = ControlSubscriptions::new();
        let a = ClientId(1);
        subs.subscribe(a, EventKind::CursorEvent).unwrap();
        let received: RefCell<Option<ControlEvent>> = RefCell::new(None);
        publish_to_subscribers(
            &mut subs,
            ControlEvent::CursorEvent {
                visible: false,
                hot_x: 4,
                hot_y: 4,
            },
            |_client, evt| {
                *received.borrow_mut() = Some(evt.clone());
                Ok(())
            },
        );
        let got = received.borrow().clone().expect("cursor event delivered");
        match got {
            ControlEvent::CursorEvent {
                visible,
                hot_x,
                hot_y,
            } => {
                assert!(!visible);
                assert_eq!(hot_x, 4);
                assert_eq!(hot_y, 4);
            }
            other => panic!("expected CursorEvent, got {other:?}"),
        }
    }

    #[test]
    fn subscriber_count_reports_each_kind_independently() {
        let mut subs = ControlSubscriptions::new();
        subs.subscribe(ClientId(1), EventKind::LayerEvent).unwrap();
        subs.subscribe(ClientId(2), EventKind::LayerEvent).unwrap();
        subs.subscribe(ClientId(3), EventKind::CursorEvent).unwrap();
        assert_eq!(subs.subscriber_count(EventKind::LayerEvent), 2);
        assert_eq!(subs.subscriber_count(EventKind::CursorEvent), 1);
        assert_eq!(subs.subscriber_count(EventKind::SurfaceCreated), 0);
    }

    #[test]
    fn subscribe_caps_at_max_per_kind() {
        let mut subs = ControlSubscriptions::new();
        for i in 0..MAX_SUBSCRIBERS_PER_KIND as u32 {
            subs.subscribe(ClientId(i), EventKind::SurfaceCreated)
                .unwrap();
        }
        let err = subs
            .subscribe(ClientId(99), EventKind::SurfaceCreated)
            .unwrap_err();
        assert_eq!(err, ControlErrorCode::ResourceExhausted);
    }

    #[test]
    fn null_sender_drops_every_event() {
        let mut subs = ControlSubscriptions::new();
        let client = ClientId(1);
        subs.subscribe(client, EventKind::SurfaceCreated).unwrap();
        publish_to_subscribers(&mut subs, surface_created(1), null_subscriber_sender);
        publish_to_subscribers(&mut subs, surface_created(2), null_subscriber_sender);
        assert_eq!(subs.pending_count(client), 0);
        assert_eq!(subs.events_dropped(), 2);
    }
}
