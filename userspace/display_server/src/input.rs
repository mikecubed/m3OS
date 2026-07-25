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
//! `MOUSE_EVENT_PULL = 1` — picked by D.2's `mouse_server`. (D.1's
//! `kbd_server` uses `KBD_EVENT_PULL = 2` because it also serves the
//! legacy `KBD_READ = 1` text-mode label on the same endpoint;
//! `mouse_server` has no legacy label, so it starts at 1.)
//!
//! ## Reply-bulk drain (Phase 56 close-out)
//!
//! The kernel transfers the typed-event bulk to the caller's
//! `pending_bulk` slot when the server `ipc_reply`s with
//! `ipc_store_reply_bulk`. The Phase 56 close-out adds
//! `syscall_lib::ipc_take_pending_bulk` (kernel syscall `0x1112`) which
//! drains that slot into a user-supplied buffer. `KbdInputSource::poll_key`
//! and `MouseInputSource::poll_pointer` use it: send `KBD_EVENT_PULL` /
//! `MOUSE_EVENT_PULL` via plain `ipc_call`, observe the reply label, then
//! drain the bulk into a fixed-size buffer matching the wire layout
//! (`KEY_EVENT_WIRE_SIZE = 19`, `POINTER_EVENT_WIRE_SIZE = 37`).
//!
//! `MockInputSource` (in `kernel-core::input::dispatch`) still drives the
//! host-side dispatcher tests against the same trait. The two impls are
//! Liskov-substitutable.

extern crate alloc;

use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

/// Diagnostic counter — total `KeyEvent`s drained from `kbd_server`
/// since boot. Incremented in [`InputWiring::drain_inner`] each time
/// `kbd.poll_key()` returns `Some`.
pub static DIAG_KEY_DRAINS_SOME: AtomicU64 = AtomicU64::new(0);
/// Diagnostic counter — `kbd.poll_key()` calls that returned `None`.
pub static DIAG_KEY_DRAINS_NONE: AtomicU64 = AtomicU64::new(0);
/// Diagnostic counter — total `PointerEvent`s drained from
/// `mouse_server` since boot.
pub static DIAG_PTR_DRAINS_SOME: AtomicU64 = AtomicU64::new(0);
/// Diagnostic counter — `mouse.poll_pointer()` calls that returned
/// `None`.
pub static DIAG_PTR_DRAINS_NONE: AtomicU64 = AtomicU64::new(0);

use kernel_core::display::protocol::{ServerMessage, SurfaceId};
use kernel_core::input::dispatch::{
    CompositorState, EnterOrLeave, InputDispatcher, InputSource, PointerRouteDecision,
    RouteDecision, SurfaceGeometry,
};
use kernel_core::input::events::{
    KEY_EVENT_WIRE_SIZE, KeyEvent, POINTER_EVENT_WIRE_SIZE, PointerEvent,
};

// ---------------------------------------------------------------------------
// Service / wire constants
// ---------------------------------------------------------------------------

/// Service-registry name for the keyboard service. Set by `kbd_server`
/// (D.1) at startup.
pub const KBD_SERVICE_NAME: &str = "kbd";

/// IPC label `kbd_server` accepts for typed `KeyEvent` pulls (D.1).
pub const KBD_EVENT_PULL: u64 = 2;

/// Reply label that `kbd_server` returns for the bounded-wait
/// "no event this tick" path. Phase 56 follow-up: the timeout
/// reply was previously `u64::MAX`, which collides with the IPC
/// transport-error sentinel returned by `ipc_call*`. The new
/// dedicated label keeps polite "no event yet" replies
/// distinguishable from real syscall failures, so future
/// observability can count them separately. Must be kept in sync
/// with `KBD_EVENT_NONE` in `userspace/kbd_server/src/main.rs`.
pub const KBD_EVENT_NONE: u64 = 3;

/// Service-registry name for the pointer/mouse service. Set by
/// `mouse_server` (D.2) at startup.
pub const MOUSE_SERVICE_NAME: &str = "mouse";

/// IPC label `mouse_server` accepts for typed `PointerEvent` pulls.
/// Picked by D.2 — `mouse_server` has no legacy text-mode label so it
/// starts at 1 (vs `kbd_server` which reserves 1 for legacy
/// `KBD_READ`).
pub const MOUSE_EVENT_PULL: u64 = 1;

/// Reply label that `mouse_server` returns for the bounded-wait
/// "no event this tick" path (mirrors `KBD_EVENT_NONE`). Must be
/// kept in sync with `MOUSE_EVENT_NONE` in
/// `userspace/mouse_server/src/main.rs`.
pub const MOUSE_EVENT_NONE: u64 = 2;

/// Throttle for the lazy reconnect path inside `KbdInputSource` /
/// `MouseInputSource`: the source retries `ipc_lookup_service` every
/// `LAZY_RECONNECT_DRAIN_INTERVAL` drain calls when its handle is
/// `None` (initial 40 ms boot lookup raced and lost, or the server
/// has not yet registered, or the server crashed and was restarted by
/// the supervisor). The display_server main loop drains roughly
/// 100 times per second when idle (each pull-poll waits up to 5 ms),
/// so 100 → ~1 lookup/second — cheap, and the reconnect lands within
/// a second of the service appearing.
pub const LAZY_RECONNECT_DRAIN_INTERVAL: u32 = 100;

// ---------------------------------------------------------------------------
// Real `InputSource` impls
// ---------------------------------------------------------------------------

/// Real keyboard input source. Pulls `KeyEvent`s from `kbd_server` over
/// the `KBD_EVENT_PULL` label.
///
/// Phase 56 close-out — the bulk-drain syscall (`SYS_IPC_TAKE_PENDING_BULK
/// = 0x1112`) makes server→client reply payloads visible to userspace. The
/// pull pattern is now end-to-end:
///
/// 1. `ipc_call(handle, KBD_EVENT_PULL, 0)` — label-only request; the
///    kernel still transfers any bulk the server staged via
///    `ipc_store_reply_bulk` to this task's `pending_bulk` slot regardless
///    of whether the request itself carried a bulk payload.
/// 2. Inspect the reply label: `KBD_EVENT_PULL` = success (event
///    pending); `u64::MAX` = kbd_server's bounded-wait timeout sentinel
///    OR transport error. Both timeout and transport error are returned
///    as `None` from `poll_key`; the dispatcher just retries next tick.
/// 3. `ipc_take_pending_bulk(&mut buf)` drains the staged 19-byte
///    `KeyEvent` wire frame into the caller's buffer.
/// 4. `KeyEvent::decode(&buf)` parses the wire frame.
pub struct KbdInputSource {
    handle: Option<u32>,
    /// Drain-pass counter used to throttle the lazy reconnect path. Reset
    /// each time a successful lookup populates `handle`. See
    /// [`LAZY_RECONNECT_DRAIN_INTERVAL`].
    drains_since_last_lookup: u32,
}

impl KbdInputSource {
    /// Start disconnected; the drain path reconnects lazily without delaying
    /// display registration or contending with init's service-start burst.
    pub fn new_lazy() -> Self {
        Self {
            handle: None,
            drains_since_last_lookup: LAZY_RECONNECT_DRAIN_INTERVAL,
        }
    }

    /// True iff the service is currently connected. Useful for boot-log
    /// visibility — the wiring's correctness does not depend on this.
    pub fn is_connected(&self) -> bool {
        self.handle.is_some()
    }

    /// Try a single, non-blocking `ipc_lookup_service` to claim the
    /// service handle if we don't have one yet. Throttled by
    /// `drains_since_last_lookup` so this does not spam on every drain.
    /// On success, logs once so the operator can see when the late
    /// connection landed (the startup log line said "unavailable" if
    /// we got here).
    fn try_lazy_reconnect(&mut self) {
        if self.handle.is_some() {
            return;
        }
        if self.drains_since_last_lookup < LAZY_RECONNECT_DRAIN_INTERVAL {
            self.drains_since_last_lookup += 1;
            return;
        }
        self.drains_since_last_lookup = 0;
        let raw = syscall_lib::ipc_lookup_service(KBD_SERVICE_NAME);
        if raw != u64::MAX {
            self.handle = Some(raw as u32);
            syscall_lib::write_str(
                syscall_lib::STDOUT_FILENO,
                "display_server: kbd service connected (lazy)\n",
            );
        }
    }
}

impl InputSource for KbdInputSource {
    fn poll_key(&mut self) -> Option<KeyEvent> {
        self.try_lazy_reconnect();
        let handle = self.handle?;

        // Label-only request. kbd_server's `KBD_EVENT_PULL` arm pumps
        // the keymap pipeline, encodes a `KeyEvent` into the reply
        // bulk via `ipc_store_reply_bulk`, and replies with the same
        // label on success, `KBD_EVENT_NONE` on bounded-wait timeout
        // (no event this tick), or `u64::MAX` on a real failure
        // (encode failure, bulk-store failure, or IPC transport
        // error). All non-success cases yield `None` to the
        // dispatcher loop; future observability can count them
        // separately because the labels are now distinct.
        let label = syscall_lib::ipc_call(handle, KBD_EVENT_PULL, 0);
        // Drain the kernel-staged reply bulk regardless of label. The kernel's
        // `endpoint::reply` calls `transfer_bulk(server, caller)` unconditionally —
        // any byte still parked in `kbd_server`'s `pending_bulk` slot at reply time
        // gets moved into the caller's slot even when the reply was a
        // `KBD_EVENT_NONE` sentinel that does not stage fresh bulk. Returning
        // early without draining left those stale bytes in `display_server`'s
        // `pending_bulk` slot, where a subsequent `ipc_try_recv_msg` whose
        // sender has no fresh bulk to overwrite the slot would surface the
        // stale bytes as a malformed verb frame to the dispatcher
        // (`reason=verb-decode opcode=304` = `OP_SERVER_KEY_EVENT` traced back
        // to this leak). Always drain so the slot is empty before the next
        // syscall path can read it.
        let mut buf = [0u8; KEY_EVENT_WIRE_SIZE];
        let n = syscall_lib::ipc_take_pending_bulk(&mut buf);
        if label != KBD_EVENT_PULL {
            // KBD_EVENT_NONE (no event this tick) or `u64::MAX` (transport
            // error). Slot is now drained; surface no event to the loop.
            return None;
        }
        if n != KEY_EVENT_WIRE_SIZE as u64 {
            // u64::MAX = drain error; any other mismatch = protocol
            // violation. Drop the event silently to keep the
            // dispatcher loop pumping; a future control-socket
            // observability verb can surface the count.
            return None;
        }

        KeyEvent::decode(&buf).ok().map(|(ev, _)| ev)
    }

    fn poll_pointer(&mut self) -> Option<PointerEvent> {
        None
    }
}

/// Real pointer input source. Pulls `PointerEvent`s from `mouse_server`
/// over the `MOUSE_EVENT_PULL` label. Same drain pattern as
/// [`KbdInputSource`] — see that type's docs for the four-step pull flow.
pub struct MouseInputSource {
    handle: Option<u32>,
    /// See [`KbdInputSource::drains_since_last_lookup`].
    drains_since_last_lookup: u32,
}

impl MouseInputSource {
    /// Start disconnected; the drain path reconnects lazily without delaying
    /// display registration or contending with init's service-start burst.
    pub fn new_lazy() -> Self {
        Self {
            handle: None,
            drains_since_last_lookup: LAZY_RECONNECT_DRAIN_INTERVAL,
        }
    }

    /// True iff the service is currently connected.
    pub fn is_connected(&self) -> bool {
        self.handle.is_some()
    }

    /// See [`KbdInputSource::try_lazy_reconnect`].
    fn try_lazy_reconnect(&mut self) {
        if self.handle.is_some() {
            return;
        }
        if self.drains_since_last_lookup < LAZY_RECONNECT_DRAIN_INTERVAL {
            self.drains_since_last_lookup += 1;
            return;
        }
        self.drains_since_last_lookup = 0;
        let raw = syscall_lib::ipc_lookup_service(MOUSE_SERVICE_NAME);
        if raw != u64::MAX {
            self.handle = Some(raw as u32);
            syscall_lib::write_str(
                syscall_lib::STDOUT_FILENO,
                "display_server: mouse service connected (lazy)\n",
            );
        }
    }
}

impl InputSource for MouseInputSource {
    fn poll_key(&mut self) -> Option<KeyEvent> {
        None
    }

    fn poll_pointer(&mut self) -> Option<PointerEvent> {
        self.try_lazy_reconnect();
        let handle = self.handle?;

        // mouse_server replies with `MOUSE_EVENT_PULL` on success,
        // `MOUSE_EVENT_NONE` on bounded-wait timeout (no event this
        // tick), or `u64::MAX` on a real failure (encode failure,
        // bulk-store failure, or IPC transport error). All
        // non-success cases yield `None` to the dispatcher loop;
        // labels are now distinct so future observability can count
        // them separately.
        let label = syscall_lib::ipc_call(handle, MOUSE_EVENT_PULL, 0);
        // Drain pending_bulk regardless of label — see [`KbdInputSource::poll_key`]
        // for the matching kernel-side rationale: `endpoint::reply` always
        // transfers any bytes parked in `mouse_server`'s `pending_bulk` slot,
        // including across NONE-sentinel replies that did not stage fresh
        // bulk. Returning early without draining left stale bytes in
        // display_server's slot that later surfaced as bogus verb frames.
        let mut buf = [0u8; POINTER_EVENT_WIRE_SIZE];
        let n = syscall_lib::ipc_take_pending_bulk(&mut buf);
        if label != MOUSE_EVENT_PULL {
            return None;
        }
        if n != POINTER_EVENT_WIRE_SIZE as u64 {
            return None;
        }

        PointerEvent::decode(&buf).ok().map(|(ev, _)| ev)
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
    /// outbound queue. Phase 70 tags the target surface id (decided by
    /// `route_key_event` / `route_pointer_event`) so the PULL handler
    /// can filter per-client and not deliver focused-client events to
    /// the wrong PULLer. Phase 71 will replace the shared queue with a
    /// per-client queue.
    Outbound(SurfaceId, ServerMessage),
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
    /// New compositor-maintained absolute pointer position after
    /// integrating a relative `PointerEvent`'s `dx` / `dy` into the
    /// previous position. Always emitted by `drain_one_pass` for
    /// every drained `PointerEvent`, independent of whether the
    /// dispatcher routed the event to a surface — the cursor blit
    /// in `compose.rs` needs to follow physical motion even when
    /// the pointer is over no mapped surface (e.g., bare boot
    /// before any client maps a `Toplevel`). `main.rs` consumes
    /// this to update its own `pointer_position` local.
    CursorMoved((i32, i32)),
}

/// Owns the [`InputDispatcher`] plus the source-side wiring (`kbd`,
/// `mouse`) and a per-frame-tick drain helper.
pub struct InputWiring {
    pub dispatcher: InputDispatcher,
    pub kbd: KbdInputSource,
    pub mouse: MouseInputSource,
    /// Phase 56 close-out (G.2 regression) — test-only injected key
    /// queue. Populated by the `ControlCommand::InjectKey` dispatcher
    /// arm (gated by `M3OS_DISPLAY_SERVER_INJECT_KEY=1`); drained by
    /// `drain_one_pass` before the real `kbd` source so synthesized
    /// chord-style events flow through the same routing pipeline as
    /// real PS/2 input. Production boots leave this empty.
    injected_keys: Vec<KeyEvent>,
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
            kbd: KbdInputSource::new_lazy(),
            mouse: MouseInputSource::new_lazy(),
            injected_keys: Vec::new(),
        }
    }

    /// Phase 56 close-out (G.2) — append a synthesized `KeyEvent` to
    /// the injected-key queue. Used by the `ControlCommand::InjectKey`
    /// dispatcher arm to drive grab-hook regressions without needing
    /// real PS/2 hardware events. Production boots leave the verb
    /// disabled and never call this.
    pub fn inject_key(&mut self, ev: KeyEvent) {
        self.injected_keys.push(ev);
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
        // Phase 56 close-out (G.2) — drain any injected keys *before*
        // the real input sources so test-driven chords route through
        // the same dispatcher path as real PS/2 events.
        let injected = core::mem::take(&mut self.injected_keys);
        let mut effects = Vec::new();
        for ev in injected {
            let mut state = CompositorState {
                focused,
                active_exclusive_layer,
                pointer_position,
                surface_geometry,
                bind_table,
                grab_state,
            };
            match self.dispatcher.route_key_event(&ev, &mut state) {
                RouteDecision::DeliverTo(target_id) => {
                    effects.push(InputEffect::Outbound(target_id, ServerMessage::Key(ev)));
                }
                RouteDecision::Grab(id) => {
                    effects.push(InputEffect::BindTriggered { id: id.raw() });
                }
                RouteDecision::Drop => {}
                _ => {}
            }
        }

        // Borrow our two sources through the trait so the body of
        // drain_with is independent of which concrete type we hold.
        // The borrow-checker forbids holding two `&mut dyn` borrows
        // out of `self` at once, so we pass them as separate
        // arguments and swap order between drains internally.
        let mut source_effects = Self::drain_inner(
            &mut self.dispatcher,
            &mut self.kbd,
            &mut self.mouse,
            focused,
            active_exclusive_layer,
            pointer_position,
            surface_geometry,
            bind_table,
            grab_state,
        );
        effects.append(&mut source_effects);
        effects
    }

    /// Test-shaped variant: drains arbitrary sources (typically a pair
    /// of `MockInputSource`s) through the dispatcher. Mirrors
    /// [`Self::drain_one_pass`] but accepts the sources by reference
    /// so tests can construct + script them inline.
    #[allow(clippy::too_many_arguments)]
    #[allow(dead_code)]
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
        // Per-pass drain cap. Previously this fn drained the full kbd
        // queue in a `while let Some(...)` loop *before* polling the
        // mouse at all. While a key was held the autorepeat stream kept
        // `kbd.poll_key()` returning Some, so the inner loop never
        // exited and the pointer drain was starved — the visible
        // symptom was "mouse cursor freezes while a key is held, jumps
        // to the new position on release". Bounding each source at
        // [`MAX_DRAINS_PER_PASS`] events per pass and interleaving the
        // two polls means the pointer is always serviced once per
        // iteration of this loop, regardless of how busy the keyboard
        // is. The main loop iterates roughly 1 kHz, so the cap still
        // sustains thousands of events per second per source, well
        // above PS/2's physical throughput.
        const MAX_DRAINS_PER_PASS: usize = 8;

        let mut effects = Vec::new();
        let mut current_pointer = pointer_position;

        for _ in 0..MAX_DRAINS_PER_PASS {
            let key_ev = kbd.poll_key();
            let ptr_ev = mouse.poll_pointer();
            if key_ev.is_some() {
                DIAG_KEY_DRAINS_SOME.fetch_add(1, Ordering::Relaxed);
            } else {
                DIAG_KEY_DRAINS_NONE.fetch_add(1, Ordering::Relaxed);
            }
            if ptr_ev.is_some() {
                DIAG_PTR_DRAINS_SOME.fetch_add(1, Ordering::Relaxed);
            } else {
                DIAG_PTR_DRAINS_NONE.fetch_add(1, Ordering::Relaxed);
            }
            if key_ev.is_none() && ptr_ev.is_none() {
                break;
            }

            if let Some(ev) = key_ev {
                let mut state = CompositorState {
                    focused,
                    active_exclusive_layer,
                    pointer_position: current_pointer,
                    surface_geometry,
                    bind_table,
                    grab_state,
                };
                match dispatcher.route_key_event(&ev, &mut state) {
                    RouteDecision::DeliverTo(target_id) => {
                        effects.push(InputEffect::Outbound(target_id, ServerMessage::Key(ev)));
                    }
                    RouteDecision::Grab(id) => {
                        effects.push(InputEffect::BindTriggered { id: id.raw() });
                    }
                    RouteDecision::Drop => {
                        // Suppressed (post-grab Repeat/Up, or no focus).
                    }
                    // `RouteDecision` is `#[non_exhaustive]` so future
                    // variants (e.g. `BoundCursorOnly`) do not break
                    // the match. Treat them as suppressed until
                    // specific wiring lands.
                    _ => {}
                }
            }

            if let Some(mut ev) = ptr_ev {
                // Integrate relative `dx` / `dy` into a new absolute
                // position. Keep `abs_position` if the source already
                // carries one (future absolute pointer like USB tablet).
                let abs = match ev.abs_position {
                    Some(p) => p,
                    None => (
                        current_pointer.0.saturating_add(ev.dx),
                        current_pointer.1.saturating_add(ev.dy),
                    ),
                };
                // Stamp the compositor-maintained absolute position back
                // into the event so the dispatcher hit-tests against the
                // same coordinates the cursor is drawn at. This value is
                // output-local; the Outbound branch below rebases it to
                // surface-local before handing the event to a client.
                ev.abs_position = Some(abs);
                // The compositor is the only process that sees both the
                // keyboard and the pointer stream, so it is the only one
                // that can tell a client whether a click was a plain
                // click or a Shift/Ctrl/Alt chord. Every pointer producer
                // (`mouse_server`, `usb-hid`) hardcodes empty modifiers
                // because none of them read the keyboard; stamp the
                // dispatcher's live snapshot over that placeholder.
                ev.modifiers = dispatcher.modifiers();
                current_pointer = abs;
                let mut state = CompositorState {
                    focused,
                    active_exclusive_layer,
                    pointer_position: abs,
                    surface_geometry,
                    bind_table,
                    grab_state,
                };
                let decision: PointerRouteDecision =
                    dispatcher.route_pointer_event(&ev, &mut state);
                // Emit the new compositor-maintained position
                // regardless of routing. The cursor blit in
                // `compose.rs` needs this to follow motion when the
                // pointer is over no surface.
                effects.push(InputEffect::CursorMoved(abs));
                for (sid, kind) in decision.enter_leave.iter() {
                    effects.push(match kind {
                        EnterOrLeave::Enter => InputEffect::PointerEnter(sid),
                        EnterOrLeave::Leave => InputEffect::PointerLeave(sid),
                    });
                }
                if let Some(target_id) = decision.deliver_to {
                    // Clients think in their own surface's coordinate
                    // space: `term` divides by its cell size to find the
                    // clicked glyph, `m3ui` compares against widget rects
                    // laid out from (0,0). Rebase the screen-absolute
                    // position onto the hit surface's origin so a window
                    // that is not at the top-left corner of the output
                    // (i.e. every window — the bar's exclusive zone alone
                    // pushes tiles down 48 px) hit-tests correctly.
                    //
                    // The origin is the surface's *geometry* rect, which
                    // for a Toplevel is its tile. `surface_screen_rect`
                    // letterbox-centres a client's pixels inside that
                    // tile when the buffer is smaller than the tile, so
                    // between a `SurfaceResized` and the client's realloc
                    // the transform is off by the centring delta. In
                    // steady state the client has resized to the tile and
                    // the delta is zero. Layer surfaces carry their paint
                    // rect as their geometry, so they are always exact.
                    //
                    // `current_pointer` / `InputEffect::CursorMoved` stay
                    // screen-absolute — they drive the cursor blit and
                    // the next pass's hit-test, both of which work in
                    // output coordinates.
                    if let Some((ox, oy)) = decision.deliver_origin {
                        ev.abs_position =
                            Some((abs.0.saturating_sub(ox), abs.1.saturating_sub(oy)));
                    }
                    effects.push(InputEffect::Outbound(target_id, ServerMessage::Pointer(ev)));
                }
                if let Some(target) = decision.focus_change {
                    // C.2 bare-metal sentinel: greppable evidence that a
                    // pointer event changed compositor focus, proving
                    // focus-follows-click over the dock-hub topology.
                    syscall_lib::write_str(
                        syscall_lib::STDOUT_FILENO,
                        "INPUT:pointer-focus-change surface=",
                    );
                    write_surface_id_dec(target.0);
                    syscall_lib::write_str(syscall_lib::STDOUT_FILENO, "\n");
                    effects.push(InputEffect::FocusChanged(target));
                }
            }
        }

        effects
    }
}

/// Write a `u32` surface id as decimal digits to STDOUT using the existing
/// `syscall_lib::write_str` idiom.  Used by the C.2 pointer-focus-change
/// sentinel.  No `alloc::format!` or new formatting machinery required.
fn write_surface_id_dec(mut value: u32) {
    let mut buf = [0u8; 10]; // u32::MAX is 10 decimal digits
    let mut idx = buf.len();
    if value == 0 {
        idx -= 1;
        buf[idx] = b'0';
    } else {
        while value != 0 {
            idx -= 1;
            buf[idx] = b'0' + (value % 10) as u8;
            value /= 10;
        }
    }
    // SAFETY: buf[idx..] contains only ASCII digit bytes.
    let s = unsafe { core::str::from_utf8_unchecked(&buf[idx..]) };
    syscall_lib::write_str(syscall_lib::STDOUT_FILENO, s);
}

// NB: `display_server` is a `no_std` + `no_main` binary crate, so the
// std `test` harness cannot compile it. The pure-logic invariants of
// the dispatcher are covered exhaustively in the `kernel_core::input::dispatch`
// host tests (22 tests); end-to-end wiring is the Phase 56 G.1
// regression test in QEMU.
