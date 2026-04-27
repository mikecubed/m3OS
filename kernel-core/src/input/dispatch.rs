//! Phase 56 Track D.3 — input dispatcher with focus-aware routing.
//!
//! Pure-logic policy that turns a raw [`KeyEvent`] / [`PointerEvent`] plus
//! a borrowed snapshot of the compositor state into a [`RouteDecision`]
//! (or [`PointerRouteDecision`] for pointer events). Owns *no* compositor
//! state — every call site supplies a [`CompositorState<'a>`] view.
//!
//! ## Decision order
//!
//! Key events (see [`InputDispatcher::route_key_event`]):
//!   1. [`BindTable::match_bind`] hit on a [`KeyEventKind::Down`] →
//!      [`GrabState::start_grab`] + return [`RouteDecision::Grab`].
//!      The matching `Repeat`/`Up` for that keycode are suppressed by
//!      step (2) below — clients never see half a chord.
//!   2. [`GrabState::is_grabbed`] hit (any kind) → suppress; for
//!      [`KeyEventKind::Up`] additionally call
//!      [`GrabState::clear_on_keyup`].
//!   3. `active_exclusive_layer` set → `DeliverTo(layer)`.
//!   4. `focused` set → `DeliverTo(focused)`.
//!   5. otherwise → [`RouteDecision::Drop`].
//!
//! Pointer events (see [`InputDispatcher::route_pointer_event`]):
//!   * Hit-test against `surface_geometry` (top-of-stack first).
//!     Boundary: **top-left-inclusive, bottom-right-exclusive** — a
//!     point `(x, y)` is inside `Rect { x: rx, y: ry, w, h }` iff
//!     `rx <= x < rx + w && ry <= y < ry + h`.
//!   * Motion that crosses a surface boundary emits a
//!     [`EnterOrLeave::Leave`] for the previous hovered surface
//!     followed by an [`EnterOrLeave::Enter`] for the new hovered
//!     surface, then delivers the event to the new surface.
//!   * [`PointerButton::Down`] on a [`SurfaceRole::Toplevel`] requests
//!     a focus change to that surface, *unless* an
//!     [`crate::display::protocol::KeyboardInteractivity::Exclusive`]
//!     layer is active.
//!
//! ## Resource discipline
//!
//! `surface_geometry` is a borrowed slice; the dispatcher does not own
//! or copy it. The enter/leave effect buffer is a fixed-capacity inline
//! array (max two effects per pointer event — at most one leave for the
//! prior hovered surface and one enter for the new hovered surface).
//!
//! ## `InputSource` trait
//!
//! Producer-side abstraction. The real `display_server` wires two impls
//! — one for `kbd_server` and one for `mouse_server`. Tests substitute a
//! [`MockInputSource`] that scripts events for assertions about routing.
//! Defining producer behaviour as a trait keeps the dispatcher pure
//! logic and lets the same code drive both real services and test
//! doubles.
//!
//! Spec: `docs/roadmap/tasks/56-display-and-input-architecture-tasks.md`
//! § D.3 (lines ~588–605).

use crate::display::protocol::{LayerConfig, Rect, SurfaceId, SurfaceRole};
use crate::input::bind_table::{BindId, BindTable, GrabState};
use crate::input::events::{KeyEvent, KeyEventKind, PointerButton, PointerEvent};

/// Per-decision routing outcome produced by
/// [`InputDispatcher::route_key_event`].
///
/// `#[non_exhaustive]` so future variants (e.g. `BoundCursorOnly` for
/// grab-hold pointer routing in later phases) can be added without
/// breaking downstream matchers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum RouteDecision {
    /// Deliver the event verbatim to the named surface.
    DeliverTo(SurfaceId),
    /// The event matched a registered keybind grab; `BindId` identifies
    /// the registered handler. The dispatcher already updated
    /// [`GrabState`] so the matching `Repeat` / `Up` will be
    /// suppressed.
    Grab(BindId),
    /// Suppress the event entirely (no client receives it). Used for
    /// post-grab `Repeat`/`Up`, and for the no-focus / no-layer case.
    Drop,
}

/// `PointerEnter`/`PointerLeave` direction tag. Distinct from
/// [`RouteDecision`] because pointer events may emit *both* an
/// enter/leave pair *and* a delivery in one decision.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EnterOrLeave {
    Leave,
    Enter,
}

/// Bounded effect-buffer capacity for [`EnterLeaveBuf`]. At most two:
/// one leave for the prior hovered surface and one enter for the new
/// hovered surface, in that order.
pub const MAX_ENTER_LEAVE: usize = 2;

/// Inline effect buffer carried alongside a pointer-route decision.
/// Bounded to [`MAX_ENTER_LEAVE`] entries — no allocation.
#[derive(Clone, Copy, Debug, Default)]
pub struct EnterLeaveBuf {
    entries: [Option<(SurfaceId, EnterOrLeave)>; MAX_ENTER_LEAVE],
}

impl EnterLeaveBuf {
    /// Construct an empty buffer.
    pub const fn new() -> Self {
        Self {
            entries: [None; MAX_ENTER_LEAVE],
        }
    }

    /// Push an effect; ignored if the buffer is full. The dispatcher
    /// never emits more than two effects per pointer event so this
    /// path is unreachable in practice; we silently drop rather than
    /// panic to keep the dispatcher allocation- and panic-free.
    fn push(&mut self, surface: SurfaceId, kind: EnterOrLeave) {
        for slot in self.entries.iter_mut() {
            if slot.is_none() {
                *slot = Some((surface, kind));
                return;
            }
        }
    }

    /// Iterate effects in insertion order.
    pub fn iter(&self) -> impl Iterator<Item = (SurfaceId, EnterOrLeave)> + '_ {
        self.entries.iter().copied().flatten()
    }

    /// Number of recorded effects.
    pub fn len(&self) -> usize {
        self.entries.iter().filter(|e| e.is_some()).count()
    }

    /// True iff no effects were recorded.
    pub fn is_empty(&self) -> bool {
        self.entries.iter().all(|e| e.is_none())
    }
}

/// Per-pointer-event routing decision.
///
/// Carries any [`EnterOrLeave`] effects (`enter_leave`) emitted by the
/// motion crossing a surface boundary, plus an optional delivery target
/// (`deliver_to`) and an optional focus-change request
/// (`focus_change`) triggered by a button-down on a `Toplevel`.
///
/// `#[non_exhaustive]` so later phases (e.g. drag-and-drop sources,
/// pointer locks) can extend it.
#[derive(Clone, Copy, Debug, Default)]
#[non_exhaustive]
pub struct PointerRouteDecision {
    /// `PointerLeave(prev)` then `PointerEnter(new)` effects in
    /// insertion order. Maximum two entries.
    pub enter_leave: EnterLeaveBuf,
    /// Surface to deliver the event to; `None` means "suppress" (the
    /// cursor is over no surface).
    pub deliver_to: Option<SurfaceId>,
    /// If `Some`, the dispatcher requests a focus change to this
    /// surface. Only emitted on `PointerButton::Down` on a `Toplevel`
    /// when no `Exclusive` layer is active.
    pub focus_change: Option<SurfaceId>,
}

impl PointerRouteDecision {
    /// Construct an empty decision (no effects, no delivery, no focus
    /// change).
    pub const fn new() -> Self {
        Self {
            enter_leave: EnterLeaveBuf::new(),
            deliver_to: None,
            focus_change: None,
        }
    }
}

/// Borrow-only view of compositor state used by [`InputDispatcher`].
///
/// Lifetime `'a` ties the borrows to the caller's owning state. Tests
/// substitute a script-driven mock; the real `display_server` shim
/// wraps its registry + focus tracker in this view per call.
pub struct CompositorState<'a> {
    /// Currently keyboard-focused surface (any role).
    pub focused: Option<SurfaceId>,
    /// Surface that owns exclusive keyboard focus while mapped (a
    /// `Layer` with [`crate::display::protocol::KeyboardInteractivity::Exclusive`]).
    /// When `Some`, all key events route here regardless of `focused`.
    pub active_exclusive_layer: Option<SurfaceId>,
    /// Pointer position, in output-local coordinates.
    pub pointer_position: (i32, i32),
    /// Stacked surface geometry; **front-of-slice = bottom-of-stack**,
    /// **end-of-slice = top-of-stack**. Hit-testing iterates in
    /// reverse so the top-most surface wins.
    pub surface_geometry: &'a [SurfaceGeometry],
    /// Bind-table reference; the dispatcher consults
    /// [`BindTable::match_bind`] on every `KeyDown` and never mutates
    /// it.
    pub bind_table: &'a BindTable,
    /// Per-keycode grab state owned by the caller. The dispatcher
    /// mutates it (start/clear) but never holds it across calls.
    pub grab_state: &'a mut GrabState,
}

/// One entry in the dispatcher's `surface_geometry` slice. Carries the
/// fields needed to drive hit-testing and keyboard-routing decisions.
///
/// Distinct from [`crate::display::compose::ComposeSurface`] because
/// the dispatcher cares about role, not pixels.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SurfaceGeometry {
    pub id: SurfaceId,
    pub rect: Rect,
    pub role: SurfaceRole,
}

impl SurfaceGeometry {
    /// Convenience: create a `Toplevel` entry.
    pub const fn toplevel(id: SurfaceId, rect: Rect) -> Self {
        Self {
            id,
            rect,
            role: SurfaceRole::Toplevel,
        }
    }

    /// Convenience: create a `Layer` entry from a layer config.
    pub const fn layer(id: SurfaceId, rect: Rect, cfg: LayerConfig) -> Self {
        Self {
            id,
            rect,
            role: SurfaceRole::Layer(cfg),
        }
    }

    /// True iff `(x, y)` is inside `self.rect` under the
    /// **top-left-inclusive, bottom-right-exclusive** convention.
    pub fn contains(&self, x: i32, y: i32) -> bool {
        rect_contains(self.rect, x, y)
    }
}

/// `Rect` hit-test under the dispatcher's chosen boundary convention:
/// **top-left-inclusive, bottom-right-exclusive**. A 0-width or
/// 0-height rect contains nothing.
///
/// Width and height arithmetic is widened to `i64` so the rect's right
/// and bottom edges cannot overflow `i32` and silently wrap; an
/// overflow returns `false`.
pub fn rect_contains(r: Rect, x: i32, y: i32) -> bool {
    if r.w == 0 || r.h == 0 {
        return false;
    }
    let xi = x as i64;
    let yi = y as i64;
    let left = r.x as i64;
    let top = r.y as i64;
    let right = match left.checked_add(r.w as i64) {
        Some(v) => v,
        None => return false,
    };
    let bottom = match top.checked_add(r.h as i64) {
        Some(v) => v,
        None => return false,
    };
    xi >= left && xi < right && yi >= top && yi < bottom
}

/// Pure-logic input dispatcher. Owns no compositor state; every method
/// takes a [`CompositorState`] view.
///
/// Dispatcher state that is *not* compositor state — the previously
/// hovered pointer surface, used to detect motion crossings — lives
/// here. This is the dispatcher's *own* memory and survives between
/// calls.
#[derive(Default)]
pub struct InputDispatcher {
    /// Surface currently hovered by the pointer (most-recent enter, no
    /// matching leave yet). `None` means the pointer is not over any
    /// surface or the dispatcher has not yet seen a pointer event.
    hovered: Option<SurfaceId>,
}

impl InputDispatcher {
    /// Construct a dispatcher in the initial state (no hovered
    /// surface).
    pub const fn new() -> Self {
        Self { hovered: None }
    }

    /// Reset hover tracking. Call this when the compositor knows the
    /// previously hovered surface has been destroyed; the next pointer
    /// event will then emit the correct `PointerEnter` for whatever
    /// surface (if any) the pointer is now over.
    pub fn forget_hovered(&mut self) {
        self.hovered = None;
    }

    /// True iff the dispatcher currently believes the pointer is
    /// hovering the named surface. Exposed for tests and for the
    /// `display_server` shim's surface-destroyed cleanup path.
    pub fn hovered(&self) -> Option<SurfaceId> {
        self.hovered
    }

    /// Route a key event. See module docs for the exact decision
    /// order.
    pub fn route_key_event(
        &mut self,
        ev: &KeyEvent,
        state: &mut CompositorState<'_>,
    ) -> RouteDecision {
        match ev.kind {
            KeyEventKind::Down => self.route_key_down(ev, state),
            KeyEventKind::Repeat => self.route_key_repeat(ev, state),
            KeyEventKind::Up => self.route_key_up(ev, state),
        }
    }

    fn route_key_down(&mut self, ev: &KeyEvent, state: &mut CompositorState<'_>) -> RouteDecision {
        // (1) Bind-table match wins over any focus routing.
        if let Some(id) = state.bind_table.match_bind(ev.modifiers.bits(), ev.keycode) {
            // Record the grab so the matching Repeat/Up are suppressed.
            // If the grab table is full, `start_grab` returns false; we
            // still return `Grab(id)` — the bind fires once. Subsequent
            // repeats will then *not* be suppressed by the grab path
            // and will fall through to focus routing in step (3) / (4).
            // D.4 documents this capacity-overrun degradation.
            let _ = state.grab_state.start_grab(ev.keycode, id);
            return RouteDecision::Grab(id);
        }

        // (3) Exclusive layer wins over normal focus.
        if let Some(layer) = state.active_exclusive_layer {
            return RouteDecision::DeliverTo(layer);
        }
        // (4) Normal focus.
        if let Some(focused) = state.focused {
            return RouteDecision::DeliverTo(focused);
        }
        // (5) Otherwise drop.
        RouteDecision::Drop
    }

    fn route_key_repeat(
        &mut self,
        ev: &KeyEvent,
        state: &mut CompositorState<'_>,
    ) -> RouteDecision {
        // (2) If the keycode is currently grabbed, suppress the repeat.
        if state.grab_state.is_grabbed(ev.keycode).is_some() {
            return RouteDecision::Drop;
        }
        // No grab match — fall through to focus routing.
        if let Some(layer) = state.active_exclusive_layer {
            return RouteDecision::DeliverTo(layer);
        }
        if let Some(focused) = state.focused {
            return RouteDecision::DeliverTo(focused);
        }
        RouteDecision::Drop
    }

    fn route_key_up(&mut self, ev: &KeyEvent, state: &mut CompositorState<'_>) -> RouteDecision {
        // (2) If the keycode is currently grabbed, clear it and
        // suppress.
        if state.grab_state.is_grabbed(ev.keycode).is_some() {
            let _ = state.grab_state.clear_on_keyup(ev.keycode);
            return RouteDecision::Drop;
        }
        // No grab — fall through to focus routing.
        if let Some(layer) = state.active_exclusive_layer {
            return RouteDecision::DeliverTo(layer);
        }
        if let Some(focused) = state.focused {
            return RouteDecision::DeliverTo(focused);
        }
        RouteDecision::Drop
    }

    /// Route a pointer event. See module docs for the hit-test
    /// convention, enter/leave emission, and click-to-focus rule.
    pub fn route_pointer_event(
        &mut self,
        ev: &PointerEvent,
        state: &mut CompositorState<'_>,
    ) -> PointerRouteDecision {
        let mut decision = PointerRouteDecision::new();

        // 1. Hit-test the *current* pointer position against all
        //    surfaces, top-of-stack first (= reverse iteration).
        let hit = hit_test(state.surface_geometry, state.pointer_position);

        // 2. If the hovered-surface tracking changed since the last
        //    call, emit Leave(prev) then Enter(new) in that order.
        if self.hovered != hit {
            if let Some(prev) = self.hovered {
                decision.enter_leave.push(prev, EnterOrLeave::Leave);
            }
            if let Some(new) = hit {
                decision.enter_leave.push(new, EnterOrLeave::Enter);
            }
            self.hovered = hit;
        }

        // 3. Delivery target: the hovered surface, if any.
        decision.deliver_to = hit;

        // 4. Click-to-focus: a button-down on a Toplevel requests
        //    focus change, unless an exclusive layer is active.
        if state.active_exclusive_layer.is_none()
            && matches!(ev.button, PointerButton::Down(_))
            && let Some(target) = hit
            && let Some(geom) = find_geometry(state.surface_geometry, target)
            && matches!(geom.role, SurfaceRole::Toplevel)
        {
            decision.focus_change = Some(target);
        }

        decision
    }
}

/// Top-of-stack-first hit-test. The dispatcher's slice convention is
/// "front = bottom-of-stack, end = top-of-stack" (mirrors compose layer
/// ordering), so the iterator runs in reverse.
fn hit_test(geom: &[SurfaceGeometry], (x, y): (i32, i32)) -> Option<SurfaceId> {
    for entry in geom.iter().rev() {
        if entry.contains(x, y) {
            return Some(entry.id);
        }
    }
    None
}

fn find_geometry(geom: &[SurfaceGeometry], id: SurfaceId) -> Option<&SurfaceGeometry> {
    geom.iter().find(|g| g.id == id)
}

// ---------------------------------------------------------------------------
// `InputSource` trait + mock implementation.
// ---------------------------------------------------------------------------

/// Producer-side abstraction for one stream of input events.
///
/// The real `display_server` wires two impls — one for `kbd_server`
/// and one for `mouse_server`. Tests substitute a [`MockInputSource`]
/// that scripts events for assertions about routing.
pub trait InputSource {
    /// Return the next available [`KeyEvent`], or `None` if the source
    /// has nothing pending. Must be non-blocking; the dispatcher's
    /// main loop drains until both sources return `None`.
    fn poll_key(&mut self) -> Option<KeyEvent>;

    /// Return the next available [`PointerEvent`], or `None` if the
    /// source has nothing pending. Must be non-blocking.
    fn poll_pointer(&mut self) -> Option<PointerEvent>;
}

/// Test-only scripted input source. Pushes events into FIFO queues;
/// the dispatcher's main loop drains them via
/// [`InputSource::poll_key`] / [`InputSource::poll_pointer`].
#[cfg(any(test, feature = "std"))]
pub struct MockInputSource {
    keys: alloc::collections::VecDeque<KeyEvent>,
    pointers: alloc::collections::VecDeque<PointerEvent>,
}

#[cfg(any(test, feature = "std"))]
impl Default for MockInputSource {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(any(test, feature = "std"))]
impl MockInputSource {
    /// Construct an empty source.
    pub fn new() -> Self {
        Self {
            keys: alloc::collections::VecDeque::new(),
            pointers: alloc::collections::VecDeque::new(),
        }
    }

    /// Schedule a key event.
    pub fn push_key(&mut self, ev: KeyEvent) {
        self.keys.push_back(ev);
    }

    /// Schedule a pointer event.
    pub fn push_pointer(&mut self, ev: PointerEvent) {
        self.pointers.push_back(ev);
    }
}

#[cfg(any(test, feature = "std"))]
impl InputSource for MockInputSource {
    fn poll_key(&mut self) -> Option<KeyEvent> {
        self.keys.pop_front()
    }

    fn poll_pointer(&mut self) -> Option<PointerEvent> {
        self.pointers.pop_front()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display::protocol::{
        KeyboardInteractivity, Layer, LayerConfig, Rect, SurfaceId, SurfaceRole,
    };
    use crate::input::bind_table::{BindKey, BindTable, GrabState};
    use crate::input::events::{
        KeyEvent, KeyEventKind, MOD_SUPER, ModifierState, PointerButton, PointerEvent,
    };
    use proptest::prelude::*;

    fn surf(id: u32) -> SurfaceId {
        SurfaceId(id)
    }

    fn rect(x: i32, y: i32, w: u32, h: u32) -> Rect {
        Rect { x, y, w, h }
    }

    fn empty_pointer(x: i32, y: i32) -> PointerEvent {
        PointerEvent {
            timestamp_ms: 0,
            dx: 0,
            dy: 0,
            abs_position: Some((x, y)),
            button: PointerButton::None,
            wheel_dx: 0,
            wheel_dy: 0,
            modifiers: ModifierState::empty(),
        }
    }

    fn key_down(modifiers: u16, keycode: u32) -> KeyEvent {
        KeyEvent {
            timestamp_ms: 0,
            keycode,
            symbol: 0,
            modifiers: ModifierState(modifiers),
            kind: KeyEventKind::Down,
        }
    }

    fn key_repeat(modifiers: u16, keycode: u32) -> KeyEvent {
        KeyEvent {
            timestamp_ms: 0,
            keycode,
            symbol: 0,
            modifiers: ModifierState(modifiers),
            kind: KeyEventKind::Repeat,
        }
    }

    fn key_up(modifiers: u16, keycode: u32) -> KeyEvent {
        KeyEvent {
            timestamp_ms: 0,
            keycode,
            symbol: 0,
            modifiers: ModifierState(modifiers),
            kind: KeyEventKind::Up,
        }
    }

    fn layer_cfg(interactivity: KeyboardInteractivity) -> LayerConfig {
        LayerConfig {
            layer: Layer::Top,
            anchor_mask: 0,
            exclusive_zone: 0,
            keyboard_interactivity: interactivity,
            margin: [0; 4],
        }
    }

    fn build_state<'a>(
        focused: Option<SurfaceId>,
        active_exclusive_layer: Option<SurfaceId>,
        pointer_position: (i32, i32),
        surface_geometry: &'a [SurfaceGeometry],
        bind_table: &'a BindTable,
        grab_state: &'a mut GrabState,
    ) -> CompositorState<'a> {
        CompositorState {
            focused,
            active_exclusive_layer,
            pointer_position,
            surface_geometry,
            bind_table,
            grab_state,
        }
    }

    // --- Key decision order -------------------------------------------------

    #[test]
    fn key_down_with_no_focus_drops() {
        let bt = BindTable::new();
        let mut gs = GrabState::new();
        let geom: [SurfaceGeometry; 0] = [];
        let mut state = build_state(None, None, (0, 0), &geom, &bt, &mut gs);
        let mut d = InputDispatcher::new();
        let result = d.route_key_event(&key_down(0, 1), &mut state);
        assert_eq!(result, RouteDecision::Drop);
    }

    #[test]
    fn key_down_with_focused_toplevel_routes_to_focused() {
        let bt = BindTable::new();
        let mut gs = GrabState::new();
        let geom = [SurfaceGeometry::toplevel(surf(7), rect(0, 0, 10, 10))];
        let mut state = build_state(Some(surf(7)), None, (0, 0), &geom, &bt, &mut gs);
        let mut d = InputDispatcher::new();
        let result = d.route_key_event(&key_down(0, 1), &mut state);
        assert_eq!(result, RouteDecision::DeliverTo(surf(7)));
    }

    #[test]
    fn key_down_with_active_exclusive_layer_routes_to_layer_even_with_focus() {
        let bt = BindTable::new();
        let mut gs = GrabState::new();
        let geom = [
            SurfaceGeometry::toplevel(surf(7), rect(0, 0, 10, 10)),
            SurfaceGeometry::layer(
                surf(99),
                rect(0, 0, 10, 10),
                layer_cfg(KeyboardInteractivity::Exclusive),
            ),
        ];
        let mut state = build_state(Some(surf(7)), Some(surf(99)), (0, 0), &geom, &bt, &mut gs);
        let mut d = InputDispatcher::new();
        let result = d.route_key_event(&key_down(0, 1), &mut state);
        assert_eq!(result, RouteDecision::DeliverTo(surf(99)));
    }

    #[test]
    fn key_down_with_matching_bind_returns_grab_and_starts_grab_state() {
        let mut bt = BindTable::new();
        let bid = bt
            .register(BindKey {
                modifier_mask: MOD_SUPER,
                keycode: b'q' as u32,
            })
            .expect("register");
        let mut gs = GrabState::new();
        let geom = [SurfaceGeometry::toplevel(surf(1), rect(0, 0, 10, 10))];
        let mut state = build_state(Some(surf(1)), None, (0, 0), &geom, &bt, &mut gs);
        let mut d = InputDispatcher::new();
        let result = d.route_key_event(&key_down(MOD_SUPER, b'q' as u32), &mut state);
        assert_eq!(result, RouteDecision::Grab(bid));
        assert_eq!(gs.is_grabbed(b'q' as u32), Some(bid));
    }

    #[test]
    fn key_repeat_for_grabbed_keycode_drops_and_keeps_grab() {
        let mut bt = BindTable::new();
        let bid = bt
            .register(BindKey {
                modifier_mask: MOD_SUPER,
                keycode: b'q' as u32,
            })
            .unwrap();
        let mut gs = GrabState::new();
        let geom = [SurfaceGeometry::toplevel(surf(1), rect(0, 0, 10, 10))];
        let mut d = InputDispatcher::new();
        // Down → Grab + start_grab.
        {
            let mut state = build_state(Some(surf(1)), None, (0, 0), &geom, &bt, &mut gs);
            assert_eq!(
                d.route_key_event(&key_down(MOD_SUPER, b'q' as u32), &mut state),
                RouteDecision::Grab(bid)
            );
        }
        // Repeat → Drop, grab still alive.
        {
            let mut state = build_state(Some(surf(1)), None, (0, 0), &geom, &bt, &mut gs);
            assert_eq!(
                d.route_key_event(&key_repeat(MOD_SUPER, b'q' as u32), &mut state),
                RouteDecision::Drop
            );
        }
        assert_eq!(gs.is_grabbed(b'q' as u32), Some(bid));
    }

    #[test]
    fn key_up_for_grabbed_keycode_clears_grab_and_drops() {
        let mut bt = BindTable::new();
        let _bid = bt
            .register(BindKey {
                modifier_mask: MOD_SUPER,
                keycode: b'q' as u32,
            })
            .unwrap();
        let mut gs = GrabState::new();
        let geom = [SurfaceGeometry::toplevel(surf(1), rect(0, 0, 10, 10))];
        let mut d = InputDispatcher::new();
        // Down → Grab.
        {
            let mut state = build_state(Some(surf(1)), None, (0, 0), &geom, &bt, &mut gs);
            d.route_key_event(&key_down(MOD_SUPER, b'q' as u32), &mut state);
        }
        // Up → Drop, grab cleared.
        {
            let mut state = build_state(Some(surf(1)), None, (0, 0), &geom, &bt, &mut gs);
            assert_eq!(
                d.route_key_event(&key_up(MOD_SUPER, b'q' as u32), &mut state),
                RouteDecision::Drop
            );
        }
        assert_eq!(gs.is_grabbed(b'q' as u32), None);
    }

    #[test]
    fn key_up_for_non_grabbed_keycode_routes_to_focused() {
        let bt = BindTable::new();
        let mut gs = GrabState::new();
        let geom = [SurfaceGeometry::toplevel(surf(7), rect(0, 0, 10, 10))];
        let mut state = build_state(Some(surf(7)), None, (0, 0), &geom, &bt, &mut gs);
        let mut d = InputDispatcher::new();
        let result = d.route_key_event(&key_up(0, 0x99), &mut state);
        assert_eq!(result, RouteDecision::DeliverTo(surf(7)));
    }

    // --- Hit-test cases (4 boundary cases + miss + stacking) ---------------

    #[test]
    fn hit_test_interior_returns_surface() {
        let geom = [SurfaceGeometry::toplevel(surf(1), rect(10, 20, 30, 40))];
        assert_eq!(hit_test(&geom, (15, 25)), Some(surf(1)));
    }

    #[test]
    fn hit_test_top_left_corner_inclusive() {
        let geom = [SurfaceGeometry::toplevel(surf(1), rect(10, 20, 30, 40))];
        assert_eq!(hit_test(&geom, (10, 20)), Some(surf(1)));
    }

    #[test]
    fn hit_test_bottom_right_corner_exclusive() {
        let geom = [SurfaceGeometry::toplevel(surf(1), rect(10, 20, 30, 40))];
        // Right edge: (10 + 30) = 40, point at x=40 is outside.
        assert_eq!(hit_test(&geom, (40, 25)), None);
        // Bottom edge: (20 + 40) = 60, point at y=60 is outside.
        assert_eq!(hit_test(&geom, (15, 60)), None);
        // Just inside both (one less than right/bottom).
        assert_eq!(hit_test(&geom, (39, 59)), Some(surf(1)));
    }

    #[test]
    fn hit_test_miss_outside_all_returns_none() {
        let geom = [SurfaceGeometry::toplevel(surf(1), rect(10, 20, 30, 40))];
        assert_eq!(hit_test(&geom, (0, 0)), None);
        assert_eq!(hit_test(&geom, (100, 100)), None);
    }

    #[test]
    fn hit_test_stacked_surfaces_top_of_stack_wins() {
        // Front-of-slice = bottom; end-of-slice = top.
        let geom = [
            SurfaceGeometry::toplevel(surf(1), rect(0, 0, 100, 100)),
            SurfaceGeometry::toplevel(surf(2), rect(20, 20, 30, 30)),
        ];
        // Inside both: surf(2) wins (top of stack).
        assert_eq!(hit_test(&geom, (25, 25)), Some(surf(2)));
        // Inside only the bottom: surf(1).
        assert_eq!(hit_test(&geom, (5, 5)), Some(surf(1)));
    }

    #[test]
    fn hit_test_zero_sized_rect_never_hits() {
        let geom = [SurfaceGeometry::toplevel(surf(1), rect(10, 20, 0, 0))];
        assert_eq!(hit_test(&geom, (10, 20)), None);
    }

    // --- Pointer enter/leave on motion crossing ----------------------------

    #[test]
    fn pointer_motion_into_surface_emits_enter() {
        let bt = BindTable::new();
        let mut gs = GrabState::new();
        let geom = [SurfaceGeometry::toplevel(surf(1), rect(10, 10, 30, 30))];
        let mut d = InputDispatcher::new();
        // First move: pointer is at (5,5) — outside any surface.
        let mut state = build_state(None, None, (5, 5), &geom, &bt, &mut gs);
        let r1 = d.route_pointer_event(&empty_pointer(5, 5), &mut state);
        assert_eq!(r1.deliver_to, None);
        assert!(r1.enter_leave.is_empty());
        // Second move: pointer is at (15,15) — inside surf(1).
        let mut state2 = build_state(None, None, (15, 15), &geom, &bt, &mut gs);
        let r2 = d.route_pointer_event(&empty_pointer(15, 15), &mut state2);
        assert_eq!(r2.deliver_to, Some(surf(1)));
        let effects: alloc::vec::Vec<_> = r2.enter_leave.iter().collect();
        assert_eq!(effects, alloc::vec![(surf(1), EnterOrLeave::Enter)]);
    }

    #[test]
    fn pointer_motion_across_surface_boundary_emits_leave_then_enter() {
        let bt = BindTable::new();
        let mut gs = GrabState::new();
        let geom = [
            SurfaceGeometry::toplevel(surf(1), rect(0, 0, 50, 50)),
            SurfaceGeometry::toplevel(surf(2), rect(100, 0, 50, 50)),
        ];
        let mut d = InputDispatcher::new();
        // Step 1: pointer at (10,10) — over surf(1).
        {
            let mut state = build_state(None, None, (10, 10), &geom, &bt, &mut gs);
            let r = d.route_pointer_event(&empty_pointer(10, 10), &mut state);
            assert_eq!(r.deliver_to, Some(surf(1)));
            let effects: alloc::vec::Vec<_> = r.enter_leave.iter().collect();
            assert_eq!(effects, alloc::vec![(surf(1), EnterOrLeave::Enter)]);
        }
        // Step 2: pointer moves to (110,10) — over surf(2).
        {
            let mut state = build_state(None, None, (110, 10), &geom, &bt, &mut gs);
            let r = d.route_pointer_event(&empty_pointer(110, 10), &mut state);
            assert_eq!(r.deliver_to, Some(surf(2)));
            let effects: alloc::vec::Vec<_> = r.enter_leave.iter().collect();
            assert_eq!(
                effects,
                alloc::vec![
                    (surf(1), EnterOrLeave::Leave),
                    (surf(2), EnterOrLeave::Enter)
                ]
            );
        }
    }

    #[test]
    fn pointer_motion_out_of_all_surfaces_emits_only_leave() {
        let bt = BindTable::new();
        let mut gs = GrabState::new();
        let geom = [SurfaceGeometry::toplevel(surf(1), rect(0, 0, 50, 50))];
        let mut d = InputDispatcher::new();
        // Inside.
        {
            let mut state = build_state(None, None, (10, 10), &geom, &bt, &mut gs);
            let _ = d.route_pointer_event(&empty_pointer(10, 10), &mut state);
        }
        // Outside.
        {
            let mut state = build_state(None, None, (200, 200), &geom, &bt, &mut gs);
            let r = d.route_pointer_event(&empty_pointer(200, 200), &mut state);
            assert_eq!(r.deliver_to, None);
            let effects: alloc::vec::Vec<_> = r.enter_leave.iter().collect();
            assert_eq!(effects, alloc::vec![(surf(1), EnterOrLeave::Leave)]);
        }
    }

    #[test]
    fn pointer_motion_within_same_surface_emits_no_enter_leave() {
        let bt = BindTable::new();
        let mut gs = GrabState::new();
        let geom = [SurfaceGeometry::toplevel(surf(1), rect(0, 0, 50, 50))];
        let mut d = InputDispatcher::new();
        // Two moves both inside surf(1).
        let _ = d.route_pointer_event(
            &empty_pointer(10, 10),
            &mut build_state(None, None, (10, 10), &geom, &bt, &mut gs),
        );
        let r = d.route_pointer_event(
            &empty_pointer(20, 20),
            &mut build_state(None, None, (20, 20), &geom, &bt, &mut gs),
        );
        assert_eq!(r.deliver_to, Some(surf(1)));
        assert!(r.enter_leave.is_empty());
    }

    // --- Click-to-focus ----------------------------------------------------

    #[test]
    fn pointer_button_down_on_toplevel_requests_focus_change() {
        let bt = BindTable::new();
        let mut gs = GrabState::new();
        let geom = [SurfaceGeometry::toplevel(surf(7), rect(0, 0, 100, 100))];
        let mut d = InputDispatcher::new();
        let click = PointerEvent {
            timestamp_ms: 0,
            dx: 0,
            dy: 0,
            abs_position: Some((50, 50)),
            button: PointerButton::Down(0),
            wheel_dx: 0,
            wheel_dy: 0,
            modifiers: ModifierState::empty(),
        };
        let mut state = build_state(None, None, (50, 50), &geom, &bt, &mut gs);
        let r = d.route_pointer_event(&click, &mut state);
        assert_eq!(r.focus_change, Some(surf(7)));
        assert_eq!(r.deliver_to, Some(surf(7)));
    }

    #[test]
    fn pointer_button_down_with_active_exclusive_layer_does_not_change_focus() {
        let bt = BindTable::new();
        let mut gs = GrabState::new();
        let geom = [
            SurfaceGeometry::toplevel(surf(7), rect(0, 0, 100, 100)),
            SurfaceGeometry::layer(
                surf(99),
                rect(0, 0, 100, 100),
                layer_cfg(KeyboardInteractivity::Exclusive),
            ),
        ];
        let mut d = InputDispatcher::new();
        let click = PointerEvent {
            timestamp_ms: 0,
            dx: 0,
            dy: 0,
            abs_position: Some((50, 50)),
            button: PointerButton::Down(0),
            wheel_dx: 0,
            wheel_dy: 0,
            modifiers: ModifierState::empty(),
        };
        let mut state = build_state(Some(surf(7)), Some(surf(99)), (50, 50), &geom, &bt, &mut gs);
        let r = d.route_pointer_event(&click, &mut state);
        assert_eq!(r.focus_change, None);
    }

    #[test]
    fn pointer_button_down_on_layer_does_not_change_focus() {
        let bt = BindTable::new();
        let mut gs = GrabState::new();
        let geom = [SurfaceGeometry::layer(
            surf(50),
            rect(0, 0, 100, 100),
            layer_cfg(KeyboardInteractivity::OnDemand),
        )];
        let mut d = InputDispatcher::new();
        let click = PointerEvent {
            timestamp_ms: 0,
            dx: 0,
            dy: 0,
            abs_position: Some((50, 50)),
            button: PointerButton::Down(0),
            wheel_dx: 0,
            wheel_dy: 0,
            modifiers: ModifierState::empty(),
        };
        let mut state = build_state(None, None, (50, 50), &geom, &bt, &mut gs);
        let r = d.route_pointer_event(&click, &mut state);
        // Click-to-focus is Toplevel-only.
        assert_eq!(r.focus_change, None);
    }

    // --- MockInputSource ---------------------------------------------------

    #[test]
    fn mock_input_source_drains_in_fifo_order() {
        let mut src = MockInputSource::new();
        src.push_key(key_down(0, 1));
        src.push_key(key_down(0, 2));
        src.push_pointer(empty_pointer(1, 1));
        assert_eq!(src.poll_key().map(|e| e.keycode), Some(1));
        assert_eq!(src.poll_key().map(|e| e.keycode), Some(2));
        assert_eq!(src.poll_key(), None);
        assert!(src.poll_pointer().is_some());
        assert!(src.poll_pointer().is_none());
    }

    // --- Property tests ----------------------------------------------------

    #[derive(Debug, Clone)]
    enum Op {
        Key {
            modifiers: u16,
            keycode: u32,
            kind: KeyKindOp,
        },
        Pointer {
            x: i32,
            y: i32,
            button: ButtonOp,
        },
        AddSurface {
            id: u32,
            x: i32,
            y: i32,
            w: u32,
            h: u32,
        },
        RemoveSurface(u32),
        FocusChanged(Option<u32>),
    }

    #[derive(Debug, Clone)]
    enum KeyKindOp {
        Down,
        Repeat,
        Up,
    }

    #[derive(Debug, Clone)]
    enum ButtonOp {
        None,
        Down,
        Up,
    }

    fn arb_key_kind_op() -> impl Strategy<Value = KeyKindOp> {
        prop_oneof![
            Just(KeyKindOp::Down),
            Just(KeyKindOp::Repeat),
            Just(KeyKindOp::Up),
        ]
    }

    fn arb_button_op() -> impl Strategy<Value = ButtonOp> {
        prop_oneof![
            Just(ButtonOp::None),
            Just(ButtonOp::Down),
            Just(ButtonOp::Up)
        ]
    }

    fn arb_op() -> impl Strategy<Value = Op> {
        prop_oneof![
            (0u16..=0x3F, 0u32..8, arb_key_kind_op()).prop_map(|(modifiers, keycode, kind)| {
                Op::Key {
                    modifiers,
                    keycode,
                    kind,
                }
            }),
            (-50i32..200, -50i32..200, arb_button_op()).prop_map(|(x, y, button)| Op::Pointer {
                x,
                y,
                button
            }),
            (1u32..6, 0i32..100, 0i32..100, 1u32..50, 1u32..50)
                .prop_map(|(id, x, y, w, h)| Op::AddSurface { id, x, y, w, h }),
            (1u32..6).prop_map(Op::RemoveSurface),
            prop::option::of(1u32..6).prop_map(Op::FocusChanged),
        ]
    }

    proptest! {
        /// Five invariants:
        ///   1. No event is ever delivered to a destroyed surface.
        ///   2. Grab matches do not leak to clients on interleaved
        ///      key/pointer traffic.
        ///   3. PointerEnter/PointerLeave always come in balanced pairs
        ///      (no leave-without-enter, no double-enter).
        ///   4. KeyRepeat / KeyUp for a grabbed keycode are suppressed
        ///      until KeyUp clears the grab.
        ///   5. Focus changes only target live surfaces.
        #[test]
        fn prop_dispatcher_invariants(
            ops in proptest::collection::vec(arb_op(), 0..120)
        ) {
            let mut surfaces: alloc::vec::Vec<SurfaceGeometry> = alloc::vec::Vec::new();
            let mut focused: Option<SurfaceId> = None;
            let mut grab_state = GrabState::new();
            let mut bind_table = BindTable::new();
            let bid = bind_table
                .register(BindKey { modifier_mask: MOD_SUPER, keycode: 1 })
                .unwrap();
            let mut dispatcher = InputDispatcher::new();
            let mut hover_balance: alloc::collections::BTreeMap<SurfaceId, i32> =
                alloc::collections::BTreeMap::new();
            let mut pointer = (0i32, 0i32);

            for op in ops {
                match op {
                    Op::AddSurface { id, x, y, w, h } => {
                        if !surfaces.iter().any(|s| s.id == surf(id)) {
                            surfaces.push(SurfaceGeometry::toplevel(surf(id), rect(x, y, w, h)));
                        }
                    }
                    Op::RemoveSurface(id) => {
                        surfaces.retain(|s| s.id != surf(id));
                        if focused == Some(surf(id)) {
                            focused = None;
                        }
                        if dispatcher.hovered() == Some(surf(id)) {
                            let entry = hover_balance.entry(surf(id)).or_insert(0);
                            *entry -= 1;
                            dispatcher.forget_hovered();
                        }
                    }
                    Op::FocusChanged(maybe_id) => {
                        focused = match maybe_id {
                            Some(id) if surfaces.iter().any(|s| s.id == surf(id)) => Some(surf(id)),
                            _ => None,
                        };
                    }
                    Op::Key { modifiers, keycode, kind } => {
                        let ev = match kind {
                            KeyKindOp::Down => key_down(modifiers, keycode),
                            KeyKindOp::Repeat => key_repeat(modifiers, keycode),
                            KeyKindOp::Up => key_up(modifiers, keycode),
                        };
                        let mut state = CompositorState {
                            focused,
                            active_exclusive_layer: None,
                            pointer_position: pointer,
                            surface_geometry: &surfaces,
                            bind_table: &bind_table,
                            grab_state: &mut grab_state,
                        };
                        let r = dispatcher.route_key_event(&ev, &mut state);
                        match r {
                            RouteDecision::DeliverTo(id) => {
                                prop_assert!(
                                    surfaces.iter().any(|s| s.id == id),
                                    "Key delivery to destroyed surface {:?}", id
                                );
                            }
                            RouteDecision::Grab(got_id) => {
                                prop_assert_eq!(got_id, bid);
                                prop_assert_eq!(modifiers, MOD_SUPER);
                                prop_assert_eq!(keycode, 1u32);
                                prop_assert!(matches!(kind, KeyKindOp::Down));
                            }
                            RouteDecision::Drop => {
                                let _ = grab_state.is_grabbed(keycode);
                            }
                        }
                    }
                    Op::Pointer { x, y, button } => {
                        pointer = (x, y);
                        let button_enum = match button {
                            ButtonOp::None => PointerButton::None,
                            ButtonOp::Down => PointerButton::Down(0),
                            ButtonOp::Up => PointerButton::Up(0),
                        };
                        let ev = PointerEvent {
                            timestamp_ms: 0,
                            dx: 0,
                            dy: 0,
                            abs_position: Some((x, y)),
                            button: button_enum,
                            wheel_dx: 0,
                            wheel_dy: 0,
                            modifiers: ModifierState::empty(),
                        };
                        let mut state = CompositorState {
                            focused,
                            active_exclusive_layer: None,
                            pointer_position: pointer,
                            surface_geometry: &surfaces,
                            bind_table: &bind_table,
                            grab_state: &mut grab_state,
                        };
                        let r = dispatcher.route_pointer_event(&ev, &mut state);
                        if let Some(id) = r.deliver_to {
                            prop_assert!(
                                surfaces.iter().any(|s| s.id == id),
                                "Pointer delivery to destroyed surface {:?}", id
                            );
                        }
                        if let Some(id) = r.focus_change {
                            prop_assert!(
                                surfaces.iter().any(|s| s.id == id),
                                "Focus change to destroyed surface {:?}", id
                            );
                        }
                        for (id, kind) in r.enter_leave.iter() {
                            let entry = hover_balance.entry(id).or_insert(0);
                            match kind {
                                EnterOrLeave::Enter => {
                                    *entry += 1;
                                    prop_assert!(*entry == 1, "double-enter for {:?}", id);
                                }
                                EnterOrLeave::Leave => {
                                    *entry -= 1;
                                    prop_assert!(*entry == 0, "leave-without-enter for {:?}", id);
                                }
                            }
                        }
                    }
                }
            }
            for (id, &count) in hover_balance.iter() {
                prop_assert!(count == 0 || count == 1, "imbalanced hover for {:?}: {}", id, count);
            }
        }
    }
}
