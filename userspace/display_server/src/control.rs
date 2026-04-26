//! Phase 56 Track E.4 — control-socket dispatcher + subscription registry.
//!
//! ## Architecture
//!
//! ```text
//!  m3ctl client                  display_server
//!  ────────────                  ──────────────
//!     │                                │
//!     │ ipc_call_buf("display-control")│
//!     │ label = LABEL_CTL_CMD          │
//!     │ bulk  = encode_command(...)    │
//!     │ ──────────────────────────────►│
//!     │                                │
//!     │                          ┌─────────────────────────┐
//!     │                          │  dispatch_command       │
//!     │                          │  (this module)          │
//!     │                          │  - routes by verb       │
//!     │                          │  - reads SurfaceRegistry│
//!     │                          │  - writes BindTable     │
//!     │                          │  - writes Subscriptions │
//!     │                          │  - reads FrameStatsRing │
//!     │                          └────────────┬────────────┘
//!     │                                       │
//!     │             reply: encoded ControlEvent (bulk-staged)
//!     │ ◄─────────────────────────────────────┘
//! ```
//!
//! The dispatcher itself owns no I/O. `main.rs` reads the IPC frame,
//! calls [`dispatch_command`], then sends the encoded reply back over
//! the implicit reply capability. Keeping the dispatcher I/O-free is
//! the same engineering discipline applied to `client.rs::dispatch` and
//! `surface.rs::SurfaceRegistry`: testable as pure logic, reuseable
//! across transports if the AF_UNIX pivot ever lands.
//!
//! ## H.1 hand-off note — filesystem permissions
//!
//! The original spec (A.8) calls for "owning-user-only" filesystem
//! permissions on a `/run/m3os/display-server.sock` AF_UNIX endpoint.
//! With the IPC-pivot transport (recorded in
//! `kernel_core::display::control`'s module docs), this becomes a NOP
//! at the protocol level: IPC service registration is process-scoped
//! and any client that can lookup `"display-control"` is on the same
//! machine. Future hardening that pins the lookup to the same UID as
//! the registering process lands in F-track / H-track work alongside
//! the broader m3OS service-ACL story.
//!
//! ## Subscription event delivery
//!
//! When `display_server` records a state change (SurfaceCreated /
//! SurfaceDestroyed / FocusChanged / BindTriggered), it iterates the
//! [`ControlSubscriptions`] registry and pushes a serialized
//! [`ControlEvent`] onto each subscribed connection's outbound channel.
//!
//! The Phase 56 close-out resolves the bulk-drain gap so request/reply
//! verbs (`m3ctl version`, `list-surfaces`, etc.) work end-to-end.
//! Server-initiated push of subscribed events to a connected client is
//! a separate deferral — it needs either a polling verb (`drain-events`
//! that the client periodically calls) or a cap-transfer at subscribe
//! time so the server holds a send-cap to the subscriber's endpoint.
//! See the `TODO(subscription-push)` markers on the `publish_*`
//! helpers. The subscription registry + event-push code is structurally
//! complete and host-tested; only the wire transmission remains.

extern crate alloc;

use alloc::collections::{BTreeMap, VecDeque};
use alloc::vec::Vec;

use kernel_core::display::control::{
    ControlCommand, ControlError, ControlErrorCode, ControlEvent, EventKind, FrameStatSample,
    PROTOCOL_VERSION, SurfaceId, SurfaceRoleTag, encode_event,
};
use kernel_core::display::protocol::SurfaceRole;
use kernel_core::display::stats::FrameStatsRing;
use kernel_core::input::bind_table::{BindError, BindKey, BindTable};

use crate::surface::SurfaceRegistry;

// ---------------------------------------------------------------------------
// Phase 56 Track F.2 — debug-crash policy
// ---------------------------------------------------------------------------

/// Runtime gate for the `ControlCommand::DebugCrash` verb.
///
/// `display_server` reads `M3OS_DISPLAY_SERVER_DEBUG_CRASH=1` from the
/// environment once at startup and constructs one of these. Production
/// boots leave it disabled; the F.2 regression-test boot path (init
/// passes the env var through when `/etc/m3os-smoke-test-mode` is
/// present) enables it.
///
/// The dispatcher consults this on every `DebugCrash` verb. Disabled
/// shadows the verb back to `ControlError::UnknownVerb` so a hostile
/// or misconfigured client cannot crash the compositor on a production
/// build.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct DebugCrashPolicy {
    enabled: bool,
}

impl DebugCrashPolicy {
    /// Disabled — the production default. `DebugCrash` short-circuits
    /// to `UnknownVerb`.
    pub const fn disabled() -> Self {
        Self { enabled: false }
    }

    /// Enabled — `DebugCrash` is honored: the dispatcher logs a
    /// structured intent line and `panic!()`s. Used only by the F.2
    /// regression test.
    pub const fn enabled() -> Self {
        Self { enabled: true }
    }

    /// Whether the verb is honored.
    pub const fn is_enabled(self) -> bool {
        self.enabled
    }
}

/// Phase 56 close-out (G.1 regression) — runtime gate for
/// `ControlCommand::ReadBackPixel`. Mirror shape of [`DebugCrashPolicy`]:
/// codec round-trips unconditionally; the dispatcher honors the verb
/// only when the env var `M3OS_DISPLAY_SERVER_READBACK=1` was set at
/// startup. Production boots leave this disabled; the multi-client-
/// coexistence regression flips a marker file (`/etc/display_server.readback`)
/// in the disk image to enable it.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct ReadBackPolicy {
    enabled: bool,
}

impl ReadBackPolicy {
    pub const fn disabled() -> Self {
        Self { enabled: false }
    }

    pub const fn enabled() -> Self {
        Self { enabled: true }
    }

    pub const fn is_enabled(self) -> bool {
        self.enabled
    }
}

/// Phase 56 close-out (G.2 regression) — runtime gate for
/// `ControlCommand::InjectKey`. Same shape as the other test-only
/// policy gates. Production boots leave this disabled; the grab-hook
/// regression flips `/etc/display_server.inject-key` so init
/// propagates `M3OS_DISPLAY_SERVER_INJECT_KEY=1`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct InjectKeyPolicy {
    enabled: bool,
}

impl InjectKeyPolicy {
    pub const fn disabled() -> Self {
        Self { enabled: false }
    }

    pub const fn enabled() -> Self {
        Self { enabled: true }
    }

    pub const fn is_enabled(self) -> bool {
        self.enabled
    }
}

// `EventKind` lives in protocol.rs and intentionally does not derive
// `Ord` (it is a stable wire-format enum). The subscription registry
// maps `EventKind` → `Vec<ClientId>` via a fixed-size array indexed by
// the variant discriminant; the helpers below convert in both
// directions. `SUBSCRIBABLE_EVENT_KINDS` is the slice we iterate over
// for "publish to all subscribers of this kind" and for the
// `subscriber_count` accessor.
const NUM_SUBSCRIBABLE_KINDS: usize = 4;
/// Stable list of subscribable event kinds in the same order as the
/// [`event_kind_index`] mapping. Exposed for docs / future iteration
/// helpers; not currently consumed at runtime (the publish path
/// indexes directly via `event_kind_index`).
#[allow(dead_code)]
const SUBSCRIBABLE_EVENT_KINDS: [EventKind; NUM_SUBSCRIBABLE_KINDS] = [
    EventKind::SurfaceCreated,
    EventKind::SurfaceDestroyed,
    EventKind::FocusChanged,
    EventKind::BindTriggered,
];

/// Convert an [`EventKind`] to a small index into the subscription
/// table. The mapping is stable as long as Phase 56's wire format is
/// stable; future variants on the `#[non_exhaustive]` enum default to
/// `None` so the publish path silently ignores them rather than
/// panicking.
fn event_kind_index(kind: EventKind) -> Option<usize> {
    match kind {
        EventKind::SurfaceCreated => Some(0),
        EventKind::SurfaceDestroyed => Some(1),
        EventKind::FocusChanged => Some(2),
        EventKind::BindTriggered => Some(3),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// IPC labels for the control endpoint
// ---------------------------------------------------------------------------

/// IPC label `display_server` accepts on the `"display-control"`
/// endpoint when the bulk carries an encoded [`ControlCommand`].
///
/// `#[allow(dead_code)]` is set because the per-iteration recv on the
/// control endpoint is gated on the C.5-bulk-drain follow-up; the
/// constant is consumed by `serve_control_iter` once that lands.
#[allow(dead_code)]
pub const LABEL_CTL_CMD: u64 = 1;

/// IPC reply label `display_server` returns when the dispatched verb
/// produced an encoded [`ControlEvent`] in the reply bulk.
#[allow(dead_code)]
pub const LABEL_CTL_REPLY: u64 = 2;

/// Maximum bulk size accepted on the control endpoint. Matches the
/// kernel's `MAX_BULK_LEN`.
#[allow(dead_code)]
pub const MAX_BULK_BYTES: usize = 4096;

// ---------------------------------------------------------------------------
// Resource bounds (engineering-discipline rule)
// ---------------------------------------------------------------------------

/// Maximum number of subscribers per `EventKind`. Over-cap is rejected
/// with a `ControlEvent::Error { code: ResourceExhausted }` reply
/// rather than allowing the registry to grow unboundedly.
pub const MAX_SUBSCRIBERS_PER_KIND: usize = 16;

/// Maximum number of pending events per subscriber's outbound queue.
/// Over-cap drops the oldest queued event so the queue stays bounded
/// without dropping the newest event the client is most likely to
/// care about.
pub const MAX_OUTBOUND_PER_SUBSCRIBER: usize = 32;

// ---------------------------------------------------------------------------
// Subscription registry
// ---------------------------------------------------------------------------

/// Stable identifier for one connected control client. Phase 56 has a
/// single in-process connection; the subscription registry is keyed on
/// this so a future multi-client world can keep the API shape.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub struct ClientId(pub u32);

/// Registry of which `ClientId` is subscribed to which `EventKind`.
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
    /// `SUBSCRIBABLE_EVENT_KINDS[i]`. Each list is bounded by
    /// [`MAX_SUBSCRIBERS_PER_KIND`].
    subscribers: [Vec<ClientId>; NUM_SUBSCRIBABLE_KINDS],
    pending_events: BTreeMap<ClientId, VecDeque<ControlEvent>>,
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
            subscribers: [Vec::new(), Vec::new(), Vec::new(), Vec::new()],
            pending_events: BTreeMap::new(),
        }
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
        // Ensure the per-client outbound queue exists so push paths
        // never silently drop on the very first event.
        self.pending_events.entry(client).or_default();
        Ok(())
    }

    /// Remove `client` from the subscriber list of `kind`. Idempotent:
    /// a non-subscribed client is a no-op. Used by the connection-
    /// teardown path (a subscriber that disconnects clears its slot).
    #[allow(dead_code)]
    pub fn unsubscribe(&mut self, client: ClientId, kind: EventKind) {
        if let Some(idx) = event_kind_index(kind) {
            self.subscribers[idx].retain(|c| *c != client);
        }
    }

    /// Forget `client` entirely. Removes the per-client queue and
    /// every subscription. Used when a control connection closes.
    #[allow(dead_code)]
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
    /// dropping the *oldest* queued event before pushing the new one;
    /// the newest event is the most likely to be relevant to a client
    /// that is currently draining.
    pub fn publish(&mut self, event: ControlEvent) {
        let kind = match event_kind_of(&event) {
            Some(k) => k,
            // Replies (VersionReply, Ack, Error, etc.) are not
            // subscribable — they only come back via the request/reply
            // channel. Ignoring them here is correct, not a bug.
            None => return,
        };
        let idx = match event_kind_index(kind) {
            Some(i) => i,
            None => return,
        };
        // Snapshot the subscriber list so the borrow on `subscribers`
        // is released before we mutate `pending_events`.
        let targets: Vec<ClientId> = self.subscribers[idx].clone();
        for client in targets {
            let queue = self.pending_events.entry(client).or_default();
            if queue.len() >= MAX_OUTBOUND_PER_SUBSCRIBER {
                // Bounded queue — drop oldest. The control socket has
                // no back-pressure surface today; preferring to drop
                // the oldest event keeps the registry steady-state.
                queue.pop_front();
            }
            queue.push_back(event.clone());
        }
    }

    /// Drain the next pending event for `client`, if any. Returned in
    /// FIFO order. The runtime transport is the C.5 bulk-drain seam;
    /// this method exposes the registry's queue shape for the
    /// transport wiring to consume.
    #[allow(dead_code)]
    pub fn drain_one(&mut self, client: ClientId) -> Option<ControlEvent> {
        self.pending_events.get_mut(&client)?.pop_front()
    }

    /// Number of pending events queued for `client`. Used by tests.
    #[allow(dead_code)]
    pub fn pending_count(&self, client: ClientId) -> usize {
        self.pending_events
            .get(&client)
            .map(|q| q.len())
            .unwrap_or(0)
    }

    /// Number of subscribers registered for `kind`. Used by tests.
    #[allow(dead_code)]
    pub fn subscriber_count(&self, kind: EventKind) -> usize {
        match event_kind_index(kind) {
            Some(i) => self.subscribers[i].len(),
            None => 0,
        }
    }
}

/// Map a [`ControlEvent`] to its corresponding [`EventKind`], or
/// `None` for non-subscribable variants (replies).
fn event_kind_of(event: &ControlEvent) -> Option<EventKind> {
    match event {
        ControlEvent::SurfaceCreated { .. } => Some(EventKind::SurfaceCreated),
        ControlEvent::SurfaceDestroyed { .. } => Some(EventKind::SurfaceDestroyed),
        ControlEvent::FocusChanged { .. } => Some(EventKind::FocusChanged),
        ControlEvent::BindTriggered { .. } => Some(EventKind::BindTriggered),
        // Reply-only events (not subscribable).
        ControlEvent::VersionReply { .. }
        | ControlEvent::SurfaceListReply { .. }
        | ControlEvent::Ack
        | ControlEvent::Error { .. }
        | ControlEvent::FrameStatsReply { .. } => None,
        // `ControlEvent` is `#[non_exhaustive]`; future variants
        // default to "not subscribable" so the publish path stays
        // safe.
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Verb dispatcher
// ---------------------------------------------------------------------------

/// Dispatch a single decoded [`ControlCommand`] against the compositor
/// state and return an encoded reply payload.
///
/// Returns `Ok(Some(bytes))` for verbs that produce a reply (Version,
/// ListSurfaces, FrameStats, plus the synthesized Ack for Focus /
/// RegisterBind / UnregisterBind / Subscribe). The bytes are the
/// encoded `ControlEvent` and the caller transmits them as the reply
/// bulk.
///
/// `Ok(None)` is reserved for fire-and-forget verbs that have no
/// reply (Phase 56 has none — every implemented verb either produces
/// a typed reply or an `Ack`).
///
/// `Err(ControlError)` indicates the dispatcher itself failed (e.g.
/// encoding into the reply buffer), distinct from the wire-level
/// errors the codec returns.
///
/// # Subscriber publication side-effects
///
/// `dispatch_command` is the *receive-side* path. It reads from the
/// registry, mutates the bind-table or subscription registry, and
/// composes a reply. The *publish-side* (state-change → publish to
/// subscribers) lives in `main.rs`, which observes outbound
/// `ServerMessage` traffic and the registry's surface delta and calls
/// [`ControlSubscriptions::publish`] directly.
///
/// # Buffer ownership
///
/// The reply is encoded into `reply_buf`. The function returns the
/// number of bytes written (or `None`). The caller is responsible for
/// staging that slice as the IPC reply bulk.
pub fn dispatch_command<F, I>(
    cmd: &ControlCommand,
    client: ClientId,
    registry: &SurfaceRegistry,
    bind_table: &mut BindTable,
    subscriptions: &mut ControlSubscriptions,
    frame_stats: &FrameStatsRing,
    debug_crash: DebugCrashPolicy,
    readback: ReadBackPolicy,
    inject_key_policy: InjectKeyPolicy,
    pixel_reader: F,
    inject_key_sink: I,
    reply_buf: &mut [u8],
) -> Result<Option<usize>, ControlError>
where
    F: FnOnce(u32, u32) -> Option<u32>,
    I: FnOnce(kernel_core::input::events::KeyEvent),
{
    let evt = match cmd {
        ControlCommand::Version => ControlEvent::VersionReply {
            protocol_version: PROTOCOL_VERSION,
        },
        ControlCommand::ListSurfaces => ControlEvent::SurfaceListReply {
            ids: registry.surface_ids(),
        },
        ControlCommand::Focus { surface_id } => {
            // Phase 56 Focus verb: validate that the surface exists in
            // the registry. The actual focus update lives in `main.rs`
            // (which owns the `focused: Option<SurfaceId>` tracker);
            // this dispatcher returns a typed Ack on success and
            // `Error { UnknownSurface }` on a stale id. The caller
            // (main.rs) consults the same registry post-dispatch and
            // applies the focus change there.
            if registry.surface_role(*surface_id).is_some()
                || registry.surface_ids().contains(surface_id)
            {
                ControlEvent::Ack
            } else {
                ControlEvent::Error {
                    code: ControlErrorCode::UnknownSurface,
                }
            }
        }
        ControlCommand::RegisterBind {
            modifier_mask,
            keycode,
        } => match bind_table.register(BindKey {
            modifier_mask: *modifier_mask,
            keycode: *keycode,
        }) {
            Ok(_id) => ControlEvent::Ack,
            Err(BindError::TableFull) => ControlEvent::Error {
                code: ControlErrorCode::ResourceExhausted,
            },
            // `BindError` is `#[non_exhaustive]`; future variants
            // (e.g. invalid modifier bits) map to `BadArgs` so the
            // dispatcher never panics on an unhandled variant.
            Err(_) => ControlEvent::Error {
                code: ControlErrorCode::BadArgs,
            },
        },
        ControlCommand::UnregisterBind {
            modifier_mask,
            keycode,
        } => {
            // The protocol carries the (mask, keycode) pair, but
            // `BindTable::unregister` takes a `BindId` — we look up
            // the existing registration via `match_bind` and then
            // unregister by id. A non-registered pair returns
            // `Error { UnknownVerb }` to mirror the symmetry of the
            // verb space (the verb is known; the *target* is not).
            // We use `UnknownSurface` here because it's the closest
            // semantic in the existing error code space; the H.1
            // doc records this mapping.
            match bind_table.match_bind(*modifier_mask, *keycode) {
                Some(id) => match bind_table.unregister(id) {
                    Ok(()) => ControlEvent::Ack,
                    Err(BindError::UnknownBind) => ControlEvent::Error {
                        code: ControlErrorCode::UnknownSurface,
                    },
                    Err(_) => ControlEvent::Error {
                        code: ControlErrorCode::BadArgs,
                    },
                },
                None => ControlEvent::Error {
                    code: ControlErrorCode::UnknownSurface,
                },
            }
        }
        ControlCommand::Subscribe { event_kind } => {
            match subscriptions.subscribe(client, *event_kind) {
                Ok(()) => ControlEvent::Ack,
                Err(code) => ControlEvent::Error { code },
            }
        }
        ControlCommand::FrameStats => ControlEvent::FrameStatsReply {
            samples: frame_stats.snapshot_newest_first(),
        },
        // Phase 56 Track F.2 — debug-only crash trigger.
        //
        // The codec round-trips this verb unconditionally, but the
        // dispatcher honors it only when the runtime debug flag is
        // set (env var `M3OS_DISPLAY_SERVER_DEBUG_CRASH=1` checked
        // once at startup and stored in `DebugCrashPolicy`). When
        // disabled, the verb shadows back to a typed
        // `Error { UnknownVerb }` reply so a hostile or misconfigured
        // client cannot crash the compositor on a production build.
        // When enabled, the dispatcher logs a structured intent line
        // and `panic!()`s; the kernel reclaims the framebuffer (the
        // userspace panic handler calls `framebuffer_release`, and
        // the kernel additionally invokes `restore_console` on
        // process death — see kernel/src/fb/mod.rs::restore_console),
        // and the supervisor restarts the service per
        // `etc/services.d/display_server.conf`'s `max_restart=5`.
        ControlCommand::DebugCrash => {
            if debug_crash.is_enabled() {
                // Structured intent line so the F.2 regression can
                // assert the controlled-crash entry point fired
                // before the panic-handler banner.
                syscall_lib::write_str(
                    syscall_lib::STDOUT_FILENO,
                    "display_server: intentional crash for F.2 regression\n",
                );
                #[allow(clippy::panic)]
                {
                    panic!("F.2 debug-crash verb");
                }
            } else {
                ControlEvent::Error {
                    code: ControlErrorCode::UnknownVerb,
                }
            }
        }
        // Phase 56 close-out (G.1 regression) — test-only pixel
        // readback. Honors the verb only when the runtime debug flag
        // is set; production boots short-circuit to `UnknownVerb`.
        ControlCommand::ReadBackPixel { x, y } => {
            if readback.is_enabled() {
                match pixel_reader(*x, *y) {
                    Some(color) => ControlEvent::PixelReply { color },
                    None => ControlEvent::Error {
                        code: ControlErrorCode::BadArgs,
                    },
                }
            } else {
                ControlEvent::Error {
                    code: ControlErrorCode::UnknownVerb,
                }
            }
        }
        // Phase 56 close-out (G.2 regression) — test-only synthetic
        // key injection.
        ControlCommand::InjectKey {
            modifier_mask,
            keycode,
            kind,
        } => {
            if inject_key_policy.is_enabled() {
                use kernel_core::input::events::{KeyEvent, KeyEventKind, ModifierState};
                match *kind {
                    0 | 1 | 2 => {
                        let kind_enum = match *kind {
                            0 => KeyEventKind::Down,
                            1 => KeyEventKind::Up,
                            _ => KeyEventKind::Repeat,
                        };
                        inject_key_sink(KeyEvent {
                            timestamp_ms: 0,
                            keycode: *keycode,
                            symbol: *keycode,
                            modifiers: ModifierState(*modifier_mask),
                            kind: kind_enum,
                        });
                        ControlEvent::Ack
                    }
                    _ => ControlEvent::Error {
                        code: ControlErrorCode::BadArgs,
                    },
                }
            } else {
                ControlEvent::Error {
                    code: ControlErrorCode::UnknownVerb,
                }
            }
        }
        // `ControlCommand` is `#[non_exhaustive]`; unknown future
        // variants surface as `Error { UnknownVerb }`. The codec layer
        // already rejects unknown opcodes via `ControlError::UnknownVerb`,
        // so this branch is reached only on a future-protocol command
        // we've decoded but not yet wired.
        _ => ControlEvent::Error {
            code: ControlErrorCode::UnknownVerb,
        },
    };
    let n = encode_event(&evt, reply_buf)?;
    Ok(Some(n))
}

// ---------------------------------------------------------------------------
// Subscription event push helpers (called from main.rs's main loop)
// ---------------------------------------------------------------------------

/// Translate a registry [`SurfaceRole`] into the wire-only
/// [`SurfaceRoleTag`]. Used when emitting a `SurfaceCreated` event so
/// the wire payload mirrors the registered role rather than a default
/// guess.
pub fn role_tag_for(role: SurfaceRole) -> SurfaceRoleTag {
    match role {
        SurfaceRole::Toplevel => SurfaceRoleTag::Toplevel,
        SurfaceRole::Layer(_) => SurfaceRoleTag::Layer,
        SurfaceRole::Cursor(_) => SurfaceRoleTag::Cursor,
    }
}

/// Convenience: publish a `SurfaceCreated` event. Looks up the role
/// from the registry so the wire tag mirrors the actual role.
pub fn publish_surface_created(
    subs: &mut ControlSubscriptions,
    registry: &SurfaceRegistry,
    surface_id: SurfaceId,
) {
    // TODO(subscription-push): server-initiated push of subscribed events
    // back to the m3ctl client. Phase 56 close-out resolved the bulk-
    // drain gap (replies work end-to-end) but server-initiated push to
    // a subscriber needs either a polling verb (`drain-events`) or a
    // cap-transfer at subscribe time so the server holds a send-cap to
    // the subscriber's endpoint. Distinct from the bulk-drain gap; this
    // is a separate deferral. The subscription registry below is
    // structurally complete and exercised by host tests.
    let role_tag = registry
        .surface_role(surface_id)
        .map(role_tag_for)
        .unwrap_or(SurfaceRoleTag::Toplevel);
    subs.publish(ControlEvent::SurfaceCreated {
        surface_id,
        role: role_tag,
    });
}

/// Convenience: publish a `SurfaceDestroyed` event.
pub fn publish_surface_destroyed(subs: &mut ControlSubscriptions, surface_id: SurfaceId) {
    // TODO(subscription-push): see publish_surface_created.
    subs.publish(ControlEvent::SurfaceDestroyed { surface_id });
}

/// Convenience: publish a `FocusChanged` event.
pub fn publish_focus_changed(subs: &mut ControlSubscriptions, focused: Option<SurfaceId>) {
    // TODO(subscription-push): see publish_surface_created.
    subs.publish(ControlEvent::FocusChanged { focused });
}

/// Convenience: publish a `BindTriggered` event. The `(mask, keycode)`
/// pair on the wire matches the registration the bind originated from.
pub fn publish_bind_triggered(subs: &mut ControlSubscriptions, modifier_mask: u16, keycode: u32) {
    // TODO(subscription-push): see publish_surface_created.
    subs.publish(ControlEvent::BindTriggered {
        modifier_mask,
        keycode,
    });
}

/// Push a freshly-measured frame compose sample onto the
/// observability ring. Called once per `compose_frame` from
/// `main.rs`.
pub fn record_frame_sample(ring: &mut FrameStatsRing, frame_index: u64, compose_micros: u32) {
    ring.push(FrameStatSample {
        frame_index,
        compose_micros,
    });
}
