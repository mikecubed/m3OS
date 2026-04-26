//! Phase 56 Track D.3 — input wiring shim.
//!
//! Thin wiring layer between the userspace input services (`kbd_server`
//! D.1, `mouse_server` D.2) and the pure-logic [`InputDispatcher`] that
//! lives in `kernel-core::input::dispatch`. Per the engineering-discipline
//! rule "no policy in the userspace shim", this module is intentionally
//! mechanical: it owns service-handle lookups, drains events from each
//! [`InputSource`], hands them to the dispatcher, and translates the
//! decisions into outbound `ServerMessage`s and focus-state updates.
//!
//! ## Architecture
//!
//! ```text
//!           ┌─────────────────┐
//!           │  kbd_server     │  ─── KBD_EVENT_PULL (D.1) ──┐
//!           │  (D.1 service)  │                              │
//!           └─────────────────┘                              │
//!                                                            ▼
//!  ┌───────────────────┐    ┌────────────────────────────────────┐
//!  │  KbdInputSource   │ ─► │   InputDispatcher (kernel-core)    │
//!  │  MouseInputSource │ ─► │   route_key_event/route_pointer    │
//!  └───────────────────┘    └────────────────────────────────────┘
//!                                          │
//!                                          ▼
//!           ┌─────────────────────────────────────────────┐
//!           │   InputWiring                               │
//!           │   - translates RouteDecision::DeliverTo →   │
//!           │     ServerMessage::Key/Pointer (outbound)   │
//!           │   - translates RouteDecision::Grab →        │
//!           │     control-socket BindTriggered (E.4)      │
//!           │   - applies PointerRouteDecision focus      │
//!           │     change to display_server's focus state  │
//!           └─────────────────────────────────────────────┘
//! ```
//!
//! ## Wire constants
//!
//! `MOUSE_EVENT_PULL = 2` — assumed to mirror D.1's `KBD_EVENT_PULL = 2`
//! on the `"mouse"` service. If D.2 picks a different label this is a
//! one-line constant change.
//!
//! ## Reply-bulk plumbing (deferred)
//!
//! The kernel already transfers the typed-event bulk to the caller's
//! `pending_bulk` slot when the server `ipc_reply`s with
//! `ipc_store_reply_bulk`. Userspace cannot yet drain that slot; the
//! existing kernel-side `RemoteBlockDevice` path uses
//! `scheduler::take_bulk_data` directly, which has no userspace
//! syscall analogue today.
//!
//! This shim therefore wires the dispatcher correctly but the real
//! `KbdInputSource::poll_key` / `MouseInputSource::poll_pointer` impls
//! return `None` until the userspace bulk-reply visibility lands (a
//! sibling task to C.5's per-client out-of-band send-cap work). The
//! `MockInputSource` (in `kernel-core::input::dispatch`) drives the
//! routing tests against the same trait, so the dispatcher exercise is
//! complete on the host.

extern crate alloc;

use alloc::vec::Vec;

use kernel_core::display::protocol::{ServerMessage, SurfaceId};
use kernel_core::input::dispatch::{
    CompositorState, EnterOrLeave, InputDispatcher, InputSource, PointerRouteDecision,
    RouteDecision, SurfaceGeometry,
};
use kernel_core::input::events::{KeyEvent, PointerEvent};

// ---------------------------------------------------------------------------
// Service / wire constants
// ---------------------------------------------------------------------------

/// Service-registry name for the keyboard service. Set by `kbd_server`
/// (D.1) at startup.
pub const KBD_SERVICE_NAME: &str = "kbd";

/// IPC label `kbd_server` accepts for typed `KeyEvent` pulls (D.1).
pub const KBD_EVENT_PULL: u64 = 2;

/// Service-registry name for the pointer/mouse service. Set by
/// `mouse_server` (D.2) at startup.
pub const MOUSE_SERVICE_NAME: &str = "mouse";

/// IPC label `mouse_server` is expected to accept for typed
/// `PointerEvent` pulls. Mirrors D.1's `KBD_EVENT_PULL = 2` shape; if
/// D.2 picks a different label this is a one-line constant change.
pub const MOUSE_EVENT_PULL: u64 = 2;

/// Service-lookup retry attempts before [`lookup_with_backoff`] gives
/// up.
pub const SERVICE_LOOKUP_ATTEMPTS: u32 = 8;

/// Backoff between service-lookup attempts (5 ms).
pub const SERVICE_LOOKUP_BACKOFF_NS: u32 = 5_000_000;

// ---------------------------------------------------------------------------
// Service-handle lookup
// ---------------------------------------------------------------------------

/// Bounded-retry service lookup. Mirrors `gfx-demo`'s
/// `lookup_display_with_backoff`: up to [`SERVICE_LOOKUP_ATTEMPTS`]
/// tries with [`SERVICE_LOOKUP_BACKOFF_NS`] between them. Returns
/// `Some(handle)` on success or `None` if the service never appears
/// (in which case the caller falls back to a no-op input source so the
/// compositor still composes its background and any test client).
pub fn lookup_with_backoff(name: &str) -> Option<u32> {
    for attempt in 0..SERVICE_LOOKUP_ATTEMPTS {
        let raw = syscall_lib::ipc_lookup_service(name);
        if raw != u64::MAX {
            return Some(raw as u32);
        }
        if attempt + 1 == SERVICE_LOOKUP_ATTEMPTS {
            return None;
        }
        let _ = syscall_lib::nanosleep_for(0, SERVICE_LOOKUP_BACKOFF_NS);
    }
    None
}

// ---------------------------------------------------------------------------
// Real `InputSource` impls
// ---------------------------------------------------------------------------

/// Real keyboard input source. Pulls `KeyEvent`s from `kbd_server` over
/// the `KBD_EVENT_PULL` label.
///
/// Phase 56 Track D.3: the pull mechanism is wired but the userspace
/// reply-bulk drain is a deferred sibling to C.5; until that lands,
/// `poll_key` returns `None` and the dispatcher is exercised by
/// `MockInputSource` in tests. Documented as a TODO so the integration
/// PR knows where to plug the real reader.
pub struct KbdInputSource {
    handle: Option<u32>,
}

impl KbdInputSource {
    /// Look up the `"kbd"` service with backoff. Returns a source whose
    /// `poll_key` will be wired to the real service when the bulk-reply
    /// drain lands; otherwise returns a source whose `poll_key` always
    /// yields `None`. Either shape keeps the dispatcher loop running.
    pub fn lookup_with_backoff() -> Self {
        Self {
            handle: lookup_with_backoff(KBD_SERVICE_NAME),
        }
    }

    /// True iff the service was found at startup. Useful for boot-log
    /// visibility — the wiring's correctness does not depend on this.
    pub fn is_connected(&self) -> bool {
        self.handle.is_some()
    }
}

impl InputSource for KbdInputSource {
    fn poll_key(&mut self) -> Option<KeyEvent> {
        // TODO(C.5-bulk-drain): wire the real KBD_EVENT_PULL → bulk
        // reply path once userspace can drain its own
        // `pending_bulk` slot. Today the kernel transfers the
        // KeyEvent bulk to the caller via `deliver_bulk`, but the
        // userspace `ipc_call_buf` syscall returns only the reply
        // label (no bulk-out). The same gap blocks the gfx-demo
        // server→client reply path; both are sibling work to D.3.
        //
        // Minimum acceptable wiring: send a label-only request, await
        // the reply label, and decode the bulk that the kernel
        // already delivered to this task's pending slot via a new
        // `syscall_lib::ipc_take_pending_bulk` (or equivalent).
        let _ = self.handle;
        None
    }

    fn poll_pointer(&mut self) -> Option<PointerEvent> {
        None
    }
}

/// Real pointer input source. Pulls `PointerEvent`s from
/// `mouse_server` over the `MOUSE_EVENT_PULL` label. Same deferred-wire
/// note as [`KbdInputSource`].
pub struct MouseInputSource {
    handle: Option<u32>,
}

impl MouseInputSource {
    /// Look up the `"mouse"` service with backoff.
    pub fn lookup_with_backoff() -> Self {
        Self {
            handle: lookup_with_backoff(MOUSE_SERVICE_NAME),
        }
    }

    /// True iff the service was found at startup.
    pub fn is_connected(&self) -> bool {
        self.handle.is_some()
    }
}

impl InputSource for MouseInputSource {
    fn poll_key(&mut self) -> Option<KeyEvent> {
        None
    }

    fn poll_pointer(&mut self) -> Option<PointerEvent> {
        // TODO(C.5-bulk-drain): see KbdInputSource::poll_key; same
        // gap, same fix lands together.
        let _ = self.handle;
        None
    }
}

// ---------------------------------------------------------------------------
// Dispatcher wiring
// ---------------------------------------------------------------------------

/// Outbound effect produced by [`InputWiring::drain_one_pass`].
///
/// The shim lives between the [`InputDispatcher`] (pure logic, no I/O)
/// and the IPC / control-socket transports. Each variant maps to a
/// concrete observable effect; `main.rs` handles the actual
/// transmission so the wiring stays host-testable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputEffect {
    /// Push a `Key` or `Pointer` `ServerMessage` onto the per-client
    /// outbound queue. Phase 56 has a single client; multi-client
    /// routing is C.5 follow-up work.
    Outbound(ServerMessage),
    /// A registered keybind fired. Surface as a control-socket
    /// `BindTriggered` event in E.4; until the control socket is up,
    /// `main.rs` logs the event for diagnostic visibility.
    BindTriggered {
        id: u32,
    },
    /// The dispatcher requests the named surface become keyboard-focus.
    /// The shim updates `display_server`'s focus-tracker; the
    /// compositor's next compose pass observes the change.
    FocusChanged(SurfaceId),
    /// PointerEnter / PointerLeave for hover tracking. Phase 56 emits
    /// these as `Pointer` `ServerMessage`s once the protocol grows the
    /// dedicated wire ops; until then they are surfaced for logging
    /// and test visibility.
    PointerEnter(SurfaceId),
    PointerLeave(SurfaceId),
}

/// Owns the [`InputDispatcher`] plus the source-side wiring (`kbd`,
/// `mouse`) and a per-frame-tick drain helper.
pub struct InputWiring {
    pub dispatcher: InputDispatcher,
    pub kbd: KbdInputSource,
    pub mouse: MouseInputSource,
}

impl Default for InputWiring {
    fn default() -> Self {
        Self::new()
    }
}

impl InputWiring {
    /// Construct the wiring with both real input sources. Either
    /// service may be unavailable at startup; the source returns
    /// `None` from its poll methods and the dispatcher idles.
    pub fn new() -> Self {
        Self {
            dispatcher: InputDispatcher::new(),
            kbd: KbdInputSource::lookup_with_backoff(),
            mouse: MouseInputSource::lookup_with_backoff(),
        }
    }

    /// Drain both sources once and feed every event through the
    /// dispatcher. Intended to be called once per frame-tick / once
    /// per main-loop iteration.
    ///
    /// Returns the [`InputEffect`]s the caller is responsible for
    /// transporting (queueing onto the per-client outbound channel,
    /// updating focus state, logging bind triggers, etc.).
    ///
    /// Pure host-testable: the dispatcher is consumed via
    /// [`InputSource`], and a `MockInputSource` can be substituted in
    /// tests by calling [`Self::drain_with`].
    pub fn drain_one_pass(
        &mut self,
        focused: Option<SurfaceId>,
        active_exclusive_layer: Option<SurfaceId>,
        pointer_position: (i32, i32),
        surface_geometry: &[SurfaceGeometry],
        bind_table: &kernel_core::input::bind_table::BindTable,
        grab_state: &mut kernel_core::input::bind_table::GrabState,
    ) -> Vec<InputEffect> {
        // Borrow our two sources through the trait so the body of
        // drain_with is independent of which concrete type we hold.
        // The borrow-checker forbids holding two `&mut dyn` borrows
        // out of `self` at once, so we pass them as separate
        // arguments and swap order between drains internally.
        Self::drain_inner(
            &mut self.dispatcher,
            &mut self.kbd,
            &mut self.mouse,
            focused,
            active_exclusive_layer,
            pointer_position,
            surface_geometry,
            bind_table,
            grab_state,
        )
    }

    /// Test-shaped variant: drains arbitrary sources (typically a pair
    /// of `MockInputSource`s) through the dispatcher. Mirrors
    /// [`Self::drain_one_pass`] but accepts the sources by reference
    /// so tests can construct + script them inline.
    #[allow(clippy::too_many_arguments)]
    pub fn drain_with<K: InputSource, M: InputSource>(
        dispatcher: &mut InputDispatcher,
        kbd: &mut K,
        mouse: &mut M,
        focused: Option<SurfaceId>,
        active_exclusive_layer: Option<SurfaceId>,
        pointer_position: (i32, i32),
        surface_geometry: &[SurfaceGeometry],
        bind_table: &kernel_core::input::bind_table::BindTable,
        grab_state: &mut kernel_core::input::bind_table::GrabState,
    ) -> Vec<InputEffect> {
        Self::drain_inner(
            dispatcher,
            kbd,
            mouse,
            focused,
            active_exclusive_layer,
            pointer_position,
            surface_geometry,
            bind_table,
            grab_state,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn drain_inner<K: InputSource + ?Sized, M: InputSource + ?Sized>(
        dispatcher: &mut InputDispatcher,
        kbd: &mut K,
        mouse: &mut M,
        focused: Option<SurfaceId>,
        active_exclusive_layer: Option<SurfaceId>,
        pointer_position: (i32, i32),
        surface_geometry: &[SurfaceGeometry],
        bind_table: &kernel_core::input::bind_table::BindTable,
        grab_state: &mut kernel_core::input::bind_table::GrabState,
    ) -> Vec<InputEffect> {
        let mut effects = Vec::new();

        // Drain all keyboard events first — they cannot move the
        // pointer, so order-relative-to-pointer does not matter for
        // correctness.
        while let Some(ev) = kbd.poll_key() {
            let mut state = CompositorState {
                focused,
                active_exclusive_layer,
                pointer_position,
                surface_geometry,
                bind_table,
                grab_state,
            };
            match dispatcher.route_key_event(&ev, &mut state) {
                RouteDecision::DeliverTo(_id) => {
                    effects.push(InputEffect::Outbound(ServerMessage::Key(ev)));
                }
                RouteDecision::Grab(id) => {
                    effects.push(InputEffect::BindTriggered { id: id.raw() });
                }
                RouteDecision::Drop => {
                    // Suppressed (post-grab Repeat/Up, or no focus).
                }
                // `RouteDecision` is `#[non_exhaustive]` so future
                // variants (e.g. `BoundCursorOnly`) do not break the
                // match. Treat them as suppressed until specific
                // wiring lands.
                _ => {}
            }
        }

        // Drain pointer events. Each event can produce an enter/leave
        // pair *and* a delivery, plus an optional focus change.
        while let Some(ev) = mouse.poll_pointer() {
            let abs = ev.abs_position.unwrap_or(pointer_position);
            let mut state = CompositorState {
                focused,
                active_exclusive_layer,
                pointer_position: abs,
                surface_geometry,
                bind_table,
                grab_state,
            };
            let decision: PointerRouteDecision = dispatcher.route_pointer_event(&ev, &mut state);
            for (sid, kind) in decision.enter_leave.iter() {
                effects.push(match kind {
                    EnterOrLeave::Enter => InputEffect::PointerEnter(sid),
                    EnterOrLeave::Leave => InputEffect::PointerLeave(sid),
                });
            }
            if decision.deliver_to.is_some() {
                effects.push(InputEffect::Outbound(ServerMessage::Pointer(ev)));
            }
            if let Some(target) = decision.focus_change {
                effects.push(InputEffect::FocusChanged(target));
            }
        }

        effects
    }
}

// NB: `display_server` is a `no_std` + `no_main` binary crate, so the
// std `test` harness cannot compile it. The pure-logic invariants of
// the dispatcher are covered exhaustively in the `kernel_core::input::dispatch`
// host tests (22 tests); end-to-end wiring is the Phase 56 G.1
// regression test in QEMU.
