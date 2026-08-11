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
//! Phase 68 Track A closed the structural gap: `flush_subscriber_ring`
//! now exists in `kernel_core::display::subscription` and the
//! `publish_*` helpers below call it after each publish. The default
//! transport in `display_server::main` is
//! [`null_subscriber_sender`] (every event drops with an `EAGAIN` /
//! `WouldBlock`, observed via the `events_dropped` counter) until a
//! cap-transfer at subscribe time lands.

extern crate alloc;

use kernel_core::display::control::{
    ControlCommand, ControlError, ControlErrorCode, ControlEvent, FrameStatSample,
    PROTOCOL_VERSION, SurfaceId, SurfaceRoleTag, encode_event,
};
use kernel_core::display::protocol::{KeyboardInteractivity, SurfaceRole};
use kernel_core::display::stats::FrameStatsRing;
pub use kernel_core::display::subscription::{
    ClientId, ControlSubscriptions, FlushError, null_subscriber_sender, publish_to_subscribers,
};
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

// Subscription registry, `EventKind` indexing, and `flush_subscriber_ring`
// live in `kernel_core::display::subscription`. The types are re-exported
// at the top of this module so existing callers continue to import them
// from `crate::control::*`. Phase 68 Track A added `LayerEvent` and
// `CursorEvent` to the kind set and a `flush_subscriber_ring` helper that
// the `publish_*` wrappers below call once per push.

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
///
/// The parameter list is the control protocol's whole authority surface, not an
/// accidental accumulation: every verb needs the command and its client, the
/// three pieces of server state a verb may read or mutate (`registry`,
/// `bind_table`, `subscriptions`), the observability ring, one gate per
/// privileged verb family (`debug_crash` / `readback` / `inject_key_policy` —
/// each defaults to disabled and is enabled independently), the two effect
/// callbacks that keep this function pure with respect to the framebuffer and
/// the input pipeline, and the caller-owned reply buffer. Bundling them into a
/// context struct would only move the same twelve bindings to the single call
/// site in `serve_one_control_request`, and would hand the dispatcher a struct
/// it could mutate wholesale in place of the deliberate `&`/`&mut` split.
#[allow(clippy::too_many_arguments)]
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
        ControlCommand::EventPull => {
            // Phase 72b Track K.8 — drain one queued subscription event
            // for the calling client. Returning the event as the reply
            // bulk is the simplest possible delivery model: the
            // subscriber polls in a loop, the dispatcher pops, no
            // separate push transport needed. An empty queue yields
            // an `Ack` so the caller can distinguish "no events" from
            // an error.
            match subscriptions.drain_one(client) {
                Some(evt) => evt,
                None => ControlEvent::Ack,
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
                use kernel_core::input::events::{
                    KeyEvent, KeyEventKind, ModifierSide, ModifierState,
                };
                match *kind {
                    0..=2 => {
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
                            // Phase 68 Track C — `InjectKey` does not
                            // carry side information; the helper derives
                            // it from the injected keycode (left vs.
                            // right modifier or `Either` for everything
                            // else).
                            modifier_side: ModifierSide::for_keycode(*keycode),
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

// ---------------------------------------------------------------------------
// Phase 68 Track A — registry-aware publish helpers
// ---------------------------------------------------------------------------
//
// `flush_subscriber_ring` + the pure-logic publish path live in
// `kernel_core::display::subscription`. The wrappers below add the
// `SurfaceRegistry`-aware role lookup (for `SurfaceCreated`) and pin the
// per-event variant shape so callers can pass the relevant fields
// directly. Each one calls `publish_to_subscribers` to enqueue the event
// for every subscriber of the matching kind and then flush each
// subscriber's ring through `send_fn`.

/// Publish a `SurfaceCreated` event and flush each subscriber's ring.
/// `send_fn` is the per-subscriber transport callback (returns
/// `Err(FlushError::WouldBlock)` for `-EAGAIN`). Looks up the role
/// from the registry so the wire tag mirrors the actual role.
pub fn publish_surface_created<F>(
    subs: &mut ControlSubscriptions,
    registry: &SurfaceRegistry,
    surface_id: SurfaceId,
    send_fn: F,
) where
    F: FnMut(ClientId, &ControlEvent) -> Result<(), FlushError>,
{
    let role_tag = registry
        .surface_role(surface_id)
        .map(role_tag_for)
        .unwrap_or(SurfaceRoleTag::Toplevel);
    publish_to_subscribers(
        subs,
        ControlEvent::SurfaceCreated {
            surface_id,
            role: role_tag,
        },
        send_fn,
    );
}

/// Publish a `SurfaceDestroyed` event and flush each subscriber's ring.
pub fn publish_surface_destroyed<F>(
    subs: &mut ControlSubscriptions,
    surface_id: SurfaceId,
    send_fn: F,
) where
    F: FnMut(ClientId, &ControlEvent) -> Result<(), FlushError>,
{
    publish_to_subscribers(subs, ControlEvent::SurfaceDestroyed { surface_id }, send_fn);
}

/// Publish a `FocusChanged` event and flush each subscriber's ring.
pub fn publish_focus_changed<F>(
    subs: &mut ControlSubscriptions,
    focused: Option<SurfaceId>,
    send_fn: F,
) where
    F: FnMut(ClientId, &ControlEvent) -> Result<(), FlushError>,
{
    publish_to_subscribers(subs, ControlEvent::FocusChanged { focused }, send_fn);
}

/// Publish a `BindTriggered` event and flush each subscriber's ring.
/// The `(mask, keycode)` pair on the wire matches the registration the
/// bind originated from.
pub fn publish_bind_triggered<F>(
    subs: &mut ControlSubscriptions,
    modifier_mask: u16,
    keycode: u32,
    send_fn: F,
) where
    F: FnMut(ClientId, &ControlEvent) -> Result<(), FlushError>,
{
    publish_to_subscribers(
        subs,
        ControlEvent::BindTriggered {
            modifier_mask,
            keycode,
        },
        send_fn,
    );
}

/// Phase 68 Track A.3 — publish a `LayerEvent` describing a
/// `Layer`-role surface (re)configuration. Subscribers re-layout
/// around the new anchor / exclusive-zone / keyboard-interactivity
/// bits.
///
/// No caller yet. `main.rs` wires the sibling publishers
/// (`publish_surface_created` / `_destroyed` / `publish_focus_changed` /
/// `publish_bind_triggered`) but has no Layer-role reconfiguration hook to
/// publish from — see the report accompanying this lint pass. Kept because it
/// is the encode half of a `ControlEvent` variant the protocol already defines.
#[allow(dead_code)]
pub fn publish_layer_event<F>(
    subs: &mut ControlSubscriptions,
    surface_id: SurfaceId,
    anchor_mask: u8,
    exclusive_zone: u32,
    keyboard_interactivity: KeyboardInteractivity,
    send_fn: F,
) where
    F: FnMut(ClientId, &ControlEvent) -> Result<(), FlushError>,
{
    publish_to_subscribers(
        subs,
        ControlEvent::LayerEvent {
            surface_id,
            anchor_mask,
            exclusive_zone,
            keyboard_interactivity,
        },
        send_fn,
    );
}

/// Phase 68 Track A.3 — publish a `CursorEvent` describing a pointer
/// cursor visibility or hotspot transition.
///
/// No caller yet, for the same reason as `publish_layer_event`: nothing in
/// `main.rs` observes cursor visibility/hotspot changes as a publishable event.
#[allow(dead_code)]
pub fn publish_cursor_event<F>(
    subs: &mut ControlSubscriptions,
    visible: bool,
    hot_x: i32,
    hot_y: i32,
    send_fn: F,
) where
    F: FnMut(ClientId, &ControlEvent) -> Result<(), FlushError>,
{
    publish_to_subscribers(
        subs,
        ControlEvent::CursorEvent {
            visible,
            hot_x,
            hot_y,
        },
        send_fn,
    );
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
