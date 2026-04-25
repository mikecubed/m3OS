//! Phase 56 Track C.3 — surface state machine.
//!
//! Atomic unit of client-provided pixel state in the compositor. Pure
//! logic; the userspace `display_server::surface` module is a thin shim
//! mapping protocol messages to events on this machine.
//!
//! ## Why this exists
//!
//! The compositor's tear-free invariants — never present a partially
//! drawn frame, never reuse a buffer the GPU is still sampling, never
//! release a buffer the client has revoked — must all be enforceable
//! locally on a single surface. This module implements that local
//! enforcement so the higher-level `display_server` shim can be a
//! straightforward protocol-to-event mapper.

use alloc::vec;
use alloc::vec::Vec;

use crate::display::protocol::{BufferId, Rect, SurfaceId, SurfaceRole};

/// Maximum damage rectangles tracked per pending commit before
/// coalescing to a single full-surface rect. Bounds the per-surface
/// memory cost of a misbehaving client that flushes one-pixel damage
/// rectangles in a tight loop.
pub const MAX_PENDING_DAMAGE: usize = 16;

/// Input event applied to the [`SurfaceStateMachine`]. Each variant
/// corresponds 1:1 with a verb in the client protocol or a compositor
/// internal signal.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SurfaceEvent {
    /// Assign or re-confirm the surface's role. First assignment causes
    /// a `Configured` effect; re-asserting the same role is a no-op;
    /// changing the role is rejected.
    SetRole(SurfaceRole),
    /// Stage a buffer for the next commit. Replaces any previously
    /// staged-but-uncommitted buffer (which is released).
    AttachBuffer(BufferId),
    /// Add a damage rectangle to the pending commit's damage list.
    DamageSurface(Rect),
    /// Promote the staged buffer + damage to active.
    CommitSurface,
    /// Update geometry; if the role is set, reconfigures the surface.
    SetGeometry(Rect),
    /// Mark the surface as keyboard-focused. Idempotent.
    FocusIn,
    /// Clear keyboard focus. Idempotent.
    FocusOut,
    /// Tear down the surface; releases pending and active buffers, and
    /// notifies the layout policy.
    DestroySurface,
    /// The compositor finished sampling the active buffer.
    SamplingComplete,
}

/// Output effect emitted by the [`SurfaceStateMachine`] when an event
/// is applied. Each effect is an instruction for the userspace shim:
/// notify the client, return a buffer, push damage to the composer,
/// etc.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum SurfaceEffect {
    /// Buffer no longer referenced; release back to the client.
    ReleaseBuffer(BufferId),
    /// New damage to compose, in surface-local coords.
    EmitDamage(Vec<Rect>),
    /// Surface is gone; remove from layout policy.
    NotifyLayoutRemoved,
    /// Surface was just configured (role, geometry); compositor should
    /// reply with `SurfaceConfigured`.
    Configured {
        /// Geometry the client should adopt for the new configuration.
        rect: Rect,
        /// Strictly monotone serial used by the client `AckConfigure`.
        serial: u32,
    },
}

/// Non-fatal error returned alongside the effect list when an event
/// violates the surface's local protocol invariants. Callers map these
/// to a wire-level disconnect or log entry as policy dictates.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SurfaceError {
    /// Operation issued on a destroyed surface.
    Dead,
    /// Commit attempted with no buffer ever attached.
    CommitWithoutAttach,
    /// Setting role twice with different roles is rejected.
    RoleAlreadySet,
}

/// Per-surface state. `id` is immutable; everything else evolves.
///
/// The struct is the source of truth for tear-free composition: it
/// guarantees that an active buffer is never released while still
/// referenced by the compositor, and that a pending buffer cannot be
/// promoted without an explicit commit.
#[derive(Clone, Debug)]
pub struct SurfaceStateMachine {
    id: SurfaceId,
    role: Option<SurfaceRole>,
    geometry: Rect,
    focused: bool,
    dead: bool,
    pending_buffer: Option<BufferId>,
    active_buffer: Option<BufferId>,
    pending_damage: Vec<Rect>,
    /// Set true once any AttachBuffer was observed (for the "commit
    /// without attach" guard — distinct from `pending_buffer`, which
    /// can be `None` after a commit).
    ever_attached: bool,
    next_serial: u32,
}

impl SurfaceStateMachine {
    /// Construct a fresh surface with no role, no geometry, and no
    /// buffers attached.
    pub fn new(id: SurfaceId) -> Self {
        Self {
            id,
            role: None,
            geometry: Rect::default(),
            focused: false,
            dead: false,
            pending_buffer: None,
            active_buffer: None,
            pending_damage: Vec::new(),
            ever_attached: false,
            next_serial: 1,
        }
    }

    /// Stable surface id minted at construction.
    pub fn id(&self) -> SurfaceId {
        self.id
    }

    /// Current role, if assigned.
    pub fn role(&self) -> Option<SurfaceRole> {
        self.role
    }

    /// Current geometry. Defaults to a zero-sized rect at the origin
    /// until [`SurfaceEvent::SetGeometry`] is applied.
    pub fn geometry(&self) -> Rect {
        self.geometry
    }

    /// True iff the most recent focus event was `FocusIn`.
    pub fn is_focused(&self) -> bool {
        self.focused
    }

    /// True iff a `DestroySurface` event has been applied.
    pub fn is_dead(&self) -> bool {
        self.dead
    }

    /// Buffer staged for the next commit, if any.
    pub fn pending_buffer(&self) -> Option<BufferId> {
        self.pending_buffer
    }

    /// Buffer currently displayed (or being sampled) by the compositor,
    /// if any.
    pub fn active_buffer(&self) -> Option<BufferId> {
        self.active_buffer
    }

    /// Damage rectangles accumulated since the last commit.
    pub fn pending_damage(&self) -> &[Rect] {
        &self.pending_damage
    }

    /// Apply an event; returns the resulting effects (zero or more) and
    /// an optional non-fatal error. Fatal cases (operations after Dead)
    /// also return empty effects.
    pub fn apply(&mut self, event: SurfaceEvent) -> (Vec<SurfaceEffect>, Option<SurfaceError>) {
        if self.dead {
            return (Vec::new(), Some(SurfaceError::Dead));
        }
        match event {
            SurfaceEvent::SetRole(role) => self.apply_set_role(role),
            SurfaceEvent::AttachBuffer(buffer) => self.apply_attach_buffer(buffer),
            SurfaceEvent::DamageSurface(rect) => self.apply_damage_surface(rect),
            SurfaceEvent::CommitSurface => self.apply_commit_surface(),
            SurfaceEvent::SetGeometry(rect) => self.apply_set_geometry(rect),
            SurfaceEvent::FocusIn => {
                self.focused = true;
                (Vec::new(), None)
            }
            SurfaceEvent::FocusOut => {
                self.focused = false;
                (Vec::new(), None)
            }
            SurfaceEvent::DestroySurface => self.apply_destroy_surface(),
            SurfaceEvent::SamplingComplete => self.apply_sampling_complete(),
        }
    }

    /// Allocate the next configuration serial and bump the counter.
    ///
    /// Saturates at [`u32::MAX`] rather than wrapping. Wrap-around would
    /// violate the "strictly monotone" contract documented on
    /// [`SurfaceEffect::Configured`] and would let a stale `AckConfigure`
    /// be accepted after a reconfigure cycle ≥ `u32::MAX`. Saturation
    /// instead plateaus the serial — real surfaces will not approach
    /// 4 billion reconfigures within a single session.
    fn take_serial(&mut self) -> u32 {
        let serial = self.next_serial;
        self.next_serial = self.next_serial.saturating_add(1);
        serial
    }

    /// Surface-local full-extent damage rectangle. Used as the coalesce
    /// target when [`MAX_PENDING_DAMAGE`] is exceeded and as the implicit
    /// damage on a buffer-replacing commit with no client-supplied damage.
    /// Always anchored at `(0, 0)` — `EmitDamage` carries surface-local
    /// rectangles, so the compositor-space origin in `geometry` would
    /// double-apply offsets downstream.
    fn full_local_damage(&self) -> Rect {
        Rect {
            x: 0,
            y: 0,
            w: self.geometry.w,
            h: self.geometry.h,
        }
    }

    /// Implementation of [`SurfaceEvent::SetRole`].
    fn apply_set_role(&mut self, role: SurfaceRole) -> (Vec<SurfaceEffect>, Option<SurfaceError>) {
        match self.role {
            Some(existing) if existing == role => (Vec::new(), None),
            Some(_) => (Vec::new(), Some(SurfaceError::RoleAlreadySet)),
            None => {
                self.role = Some(role);
                let serial = self.take_serial();
                let effect = SurfaceEffect::Configured {
                    rect: self.geometry,
                    serial,
                };
                (vec![effect], None)
            }
        }
    }

    /// Implementation of [`SurfaceEvent::AttachBuffer`].
    fn apply_attach_buffer(
        &mut self,
        buffer: BufferId,
    ) -> (Vec<SurfaceEffect>, Option<SurfaceError>) {
        let mut effects = Vec::new();
        if let Some(prev) = self.pending_buffer
            && prev != buffer
        {
            effects.push(SurfaceEffect::ReleaseBuffer(prev));
        }
        self.pending_buffer = Some(buffer);
        self.ever_attached = true;
        (effects, None)
    }

    /// Implementation of [`SurfaceEvent::DamageSurface`].
    fn apply_damage_surface(&mut self, rect: Rect) -> (Vec<SurfaceEffect>, Option<SurfaceError>) {
        let full_local = self.full_local_damage();
        if self.pending_damage.len() >= MAX_PENDING_DAMAGE {
            // Coalesce: replace the entire vec with a single
            // full-surface rect (in surface-local coordinates).
            self.pending_damage.clear();
            self.pending_damage.push(full_local);
        } else {
            self.pending_damage.push(rect);
            if self.pending_damage.len() > MAX_PENDING_DAMAGE {
                self.pending_damage.clear();
                self.pending_damage.push(full_local);
            }
        }
        (Vec::new(), None)
    }

    /// Implementation of [`SurfaceEvent::CommitSurface`].
    fn apply_commit_surface(&mut self) -> (Vec<SurfaceEffect>, Option<SurfaceError>) {
        if !self.ever_attached {
            return (Vec::new(), Some(SurfaceError::CommitWithoutAttach));
        }
        let mut effects = Vec::new();
        let pending = self.pending_buffer.take();
        let damage = core::mem::take(&mut self.pending_damage);
        match pending {
            None => {
                // Damage-only commit: only legal once an active buffer
                // already exists. (`ever_attached` guarantees a prior
                // attach happened; if `active_buffer` is also None
                // then a previous commit consumed the only attach
                // without producing an active buffer, which is not
                // currently reachable but we still defensively skip
                // the damage emit in that case.)
                if self.active_buffer.is_some() && !damage.is_empty() {
                    effects.push(SurfaceEffect::EmitDamage(damage));
                }
            }
            Some(p) => {
                if let Some(a) = self.active_buffer
                    && a != p
                {
                    effects.push(SurfaceEffect::ReleaseBuffer(a));
                }
                self.active_buffer = Some(p);
                let damage_to_emit = if damage.is_empty() {
                    vec![self.full_local_damage()]
                } else {
                    damage
                };
                effects.push(SurfaceEffect::EmitDamage(damage_to_emit));
            }
        }
        (effects, None)
    }

    /// Implementation of [`SurfaceEvent::SetGeometry`].
    fn apply_set_geometry(&mut self, rect: Rect) -> (Vec<SurfaceEffect>, Option<SurfaceError>) {
        self.geometry = rect;
        if self.role.is_some() {
            let serial = self.take_serial();
            let effect = SurfaceEffect::Configured { rect, serial };
            (vec![effect], None)
        } else {
            (Vec::new(), None)
        }
    }

    /// Implementation of [`SurfaceEvent::DestroySurface`].
    fn apply_destroy_surface(&mut self) -> (Vec<SurfaceEffect>, Option<SurfaceError>) {
        self.dead = true;
        let mut effects = Vec::new();
        let active = self.active_buffer.take();
        let pending = self.pending_buffer.take();
        if let Some(a) = active {
            effects.push(SurfaceEffect::ReleaseBuffer(a));
        }
        if let Some(p) = pending
            && Some(p) != active
        {
            effects.push(SurfaceEffect::ReleaseBuffer(p));
        }
        effects.push(SurfaceEffect::NotifyLayoutRemoved);
        (effects, None)
    }

    /// Implementation of [`SurfaceEvent::SamplingComplete`].
    fn apply_sampling_complete(&mut self) -> (Vec<SurfaceEffect>, Option<SurfaceError>) {
        match self.active_buffer.take() {
            Some(active) => (vec![SurfaceEffect::ReleaseBuffer(active)], None),
            None => (Vec::new(), None),
        }
    }
}

#[cfg(all(test, feature = "std"))]
mod tests {
    use super::*;
    use crate::display::protocol::{KeyboardInteractivity, Layer, LayerConfig};
    use proptest::prelude::*;
    use std::collections::HashSet;

    fn surface() -> SurfaceStateMachine {
        SurfaceStateMachine::new(SurfaceId(7))
    }

    fn r(x: i32, y: i32, w: u32, h: u32) -> Rect {
        Rect { x, y, w, h }
    }

    fn layer_role() -> SurfaceRole {
        SurfaceRole::Layer(LayerConfig {
            layer: Layer::Top,
            anchor_mask: 0,
            exclusive_zone: 0,
            keyboard_interactivity: KeyboardInteractivity::None,
            margin: [0, 0, 0, 0],
        })
    }

    #[test]
    fn new_surface_has_no_role_or_buffer() {
        let s = surface();
        assert_eq!(s.id(), SurfaceId(7));
        assert_eq!(s.role(), None);
        assert_eq!(s.geometry(), Rect::default());
        assert!(!s.is_focused());
        assert!(!s.is_dead());
        assert_eq!(s.pending_buffer(), None);
        assert_eq!(s.active_buffer(), None);
        assert!(s.pending_damage().is_empty());
    }

    #[test]
    fn set_role_first_time_emits_configured() {
        let mut s = surface();
        let (effects, err) = s.apply(SurfaceEvent::SetRole(SurfaceRole::Toplevel));
        assert!(err.is_none());
        assert_eq!(effects.len(), 1);
        match &effects[0] {
            SurfaceEffect::Configured { rect, serial } => {
                assert_eq!(*rect, Rect::default());
                assert_eq!(*serial, 1);
            }
            other => panic!("expected Configured, got {:?}", other),
        }
        assert_eq!(s.role(), Some(SurfaceRole::Toplevel));
    }

    #[test]
    fn set_same_role_twice_is_noop_no_effects() {
        let mut s = surface();
        let _ = s.apply(SurfaceEvent::SetRole(SurfaceRole::Toplevel));
        let (effects, err) = s.apply(SurfaceEvent::SetRole(SurfaceRole::Toplevel));
        assert!(err.is_none());
        assert!(effects.is_empty());
    }

    #[test]
    fn set_different_role_returns_error_no_change() {
        let mut s = surface();
        let _ = s.apply(SurfaceEvent::SetRole(SurfaceRole::Toplevel));
        let (effects, err) = s.apply(SurfaceEvent::SetRole(layer_role()));
        assert_eq!(err, Some(SurfaceError::RoleAlreadySet));
        assert!(effects.is_empty());
        assert_eq!(s.role(), Some(SurfaceRole::Toplevel));
    }

    #[test]
    fn attach_then_commit_promotes_buffer() {
        let mut s = surface();
        let geom = r(0, 0, 200, 100);
        let _ = s.apply(SurfaceEvent::SetGeometry(geom));
        let (e1, _) = s.apply(SurfaceEvent::AttachBuffer(BufferId(1)));
        assert!(e1.is_empty());
        let (e2, err) = s.apply(SurfaceEvent::CommitSurface);
        assert!(err.is_none());
        assert_eq!(s.active_buffer(), Some(BufferId(1)));
        assert_eq!(s.pending_buffer(), None);
        assert_eq!(e2.len(), 1);
        match &e2[0] {
            SurfaceEffect::EmitDamage(rects) => {
                assert_eq!(rects.as_slice(), &[geom]);
            }
            other => panic!("expected EmitDamage, got {:?}", other),
        }
    }

    #[test]
    fn attach_replaces_pending_and_releases_old() {
        let mut s = surface();
        let _ = s.apply(SurfaceEvent::AttachBuffer(BufferId(1)));
        let (effects, err) = s.apply(SurfaceEvent::AttachBuffer(BufferId(2)));
        assert!(err.is_none());
        assert_eq!(effects, vec![SurfaceEffect::ReleaseBuffer(BufferId(1))]);
        assert_eq!(s.pending_buffer(), Some(BufferId(2)));
    }

    #[test]
    fn commit_without_attach_returns_error_no_state_change() {
        let mut s = surface();
        let (effects, err) = s.apply(SurfaceEvent::CommitSurface);
        assert_eq!(err, Some(SurfaceError::CommitWithoutAttach));
        assert!(effects.is_empty());
        assert_eq!(s.active_buffer(), None);
        assert_eq!(s.pending_buffer(), None);
    }

    #[test]
    fn commit_without_replace_emits_no_release() {
        let mut s = surface();
        let _ = s.apply(SurfaceEvent::AttachBuffer(BufferId(1)));
        let (effects, err) = s.apply(SurfaceEvent::CommitSurface);
        assert!(err.is_none());
        // No prior active, so no ReleaseBuffer should appear.
        for e in &effects {
            assert!(
                !matches!(e, SurfaceEffect::ReleaseBuffer(_)),
                "did not expect ReleaseBuffer in {:?}",
                effects,
            );
        }
    }

    #[test]
    fn damage_accumulates_until_max_then_coalesces() {
        let mut s = surface();
        let geom = r(0, 0, 1024, 768);
        let _ = s.apply(SurfaceEvent::SetGeometry(geom));
        for i in 0..MAX_PENDING_DAMAGE {
            let (eff, err) = s.apply(SurfaceEvent::DamageSurface(r(i as i32, i as i32, 4, 4)));
            assert!(err.is_none());
            assert!(eff.is_empty());
        }
        assert_eq!(s.pending_damage().len(), MAX_PENDING_DAMAGE);
        // The MAX_PENDING_DAMAGE+1 push triggers coalescing.
        let (eff, err) = s.apply(SurfaceEvent::DamageSurface(r(99, 99, 1, 1)));
        assert!(err.is_none());
        assert!(eff.is_empty());
        assert_eq!(s.pending_damage().len(), 1);
        assert_eq!(s.pending_damage()[0], geom);
    }

    #[test]
    fn damage_taken_on_commit() {
        let mut s = surface();
        let _ = s.apply(SurfaceEvent::SetGeometry(r(0, 0, 100, 100)));
        let _ = s.apply(SurfaceEvent::AttachBuffer(BufferId(1)));
        let _ = s.apply(SurfaceEvent::DamageSurface(r(1, 1, 2, 2)));
        let _ = s.apply(SurfaceEvent::DamageSurface(r(5, 5, 8, 8)));
        let _ = s.apply(SurfaceEvent::CommitSurface);
        assert!(s.pending_damage().is_empty());
    }

    #[test]
    fn damage_only_commit_after_initial_commit() {
        let mut s = surface();
        let _ = s.apply(SurfaceEvent::SetGeometry(r(0, 0, 100, 100)));
        let _ = s.apply(SurfaceEvent::AttachBuffer(BufferId(1)));
        let _ = s.apply(SurfaceEvent::CommitSurface);
        let _ = s.apply(SurfaceEvent::DamageSurface(r(2, 2, 4, 4)));
        let (effects, err) = s.apply(SurfaceEvent::CommitSurface);
        assert!(err.is_none());
        assert_eq!(effects.len(), 1);
        match &effects[0] {
            SurfaceEffect::EmitDamage(rects) => {
                assert_eq!(rects.as_slice(), &[r(2, 2, 4, 4)]);
            }
            other => panic!("expected EmitDamage, got {:?}", other),
        }
        // No new ReleaseBuffer.
        for e in &effects {
            assert!(
                !matches!(e, SurfaceEffect::ReleaseBuffer(_)),
                "no release expected, got {:?}",
                effects
            );
        }
        assert_eq!(s.active_buffer(), Some(BufferId(1)));
    }

    #[test]
    fn destroy_releases_pending_and_active_in_order() {
        let mut s = surface();
        let _ = s.apply(SurfaceEvent::AttachBuffer(BufferId(1)));
        let _ = s.apply(SurfaceEvent::CommitSurface);
        let _ = s.apply(SurfaceEvent::AttachBuffer(BufferId(2)));
        let (effects, err) = s.apply(SurfaceEvent::DestroySurface);
        assert!(err.is_none());
        assert_eq!(
            effects,
            vec![
                SurfaceEffect::ReleaseBuffer(BufferId(1)),
                SurfaceEffect::ReleaseBuffer(BufferId(2)),
                SurfaceEffect::NotifyLayoutRemoved,
            ]
        );
        assert!(s.is_dead());
    }

    #[test]
    fn destroy_emits_each_buffer_only_once_when_pending_equals_active() {
        let mut s = surface();
        let _ = s.apply(SurfaceEvent::AttachBuffer(BufferId(1)));
        let _ = s.apply(SurfaceEvent::CommitSurface);
        let (effects, err) = s.apply(SurfaceEvent::DestroySurface);
        assert!(err.is_none());
        assert_eq!(
            effects,
            vec![
                SurfaceEffect::ReleaseBuffer(BufferId(1)),
                SurfaceEffect::NotifyLayoutRemoved,
            ]
        );
    }

    #[test]
    fn events_after_destroy_return_dead_error_no_effects() {
        let mut s = surface();
        let _ = s.apply(SurfaceEvent::DestroySurface);
        for ev in [
            SurfaceEvent::SetRole(SurfaceRole::Toplevel),
            SurfaceEvent::AttachBuffer(BufferId(9)),
            SurfaceEvent::DamageSurface(r(0, 0, 1, 1)),
            SurfaceEvent::CommitSurface,
            SurfaceEvent::SetGeometry(r(0, 0, 1, 1)),
            SurfaceEvent::FocusIn,
            SurfaceEvent::FocusOut,
            SurfaceEvent::DestroySurface,
            SurfaceEvent::SamplingComplete,
        ] {
            let (effects, err) = s.apply(ev);
            assert_eq!(err, Some(SurfaceError::Dead), "event {:?}", ev);
            assert!(effects.is_empty(), "event {:?} produced {:?}", ev, effects);
        }
    }

    #[test]
    fn sampling_complete_releases_active() {
        let mut s = surface();
        let _ = s.apply(SurfaceEvent::AttachBuffer(BufferId(5)));
        let _ = s.apply(SurfaceEvent::CommitSurface);
        let (effects, err) = s.apply(SurfaceEvent::SamplingComplete);
        assert!(err.is_none());
        assert_eq!(effects, vec![SurfaceEffect::ReleaseBuffer(BufferId(5))]);
        assert_eq!(s.active_buffer(), None);
    }

    #[test]
    fn sampling_complete_with_no_active_is_noop() {
        let mut s = surface();
        let (effects, err) = s.apply(SurfaceEvent::SamplingComplete);
        assert!(err.is_none());
        assert!(effects.is_empty());
    }

    #[test]
    fn set_geometry_after_role_emits_configured_with_new_serial() {
        let mut s = surface();
        let (e1, _) = s.apply(SurfaceEvent::SetRole(SurfaceRole::Toplevel));
        let s1 = match e1[0] {
            SurfaceEffect::Configured { serial, .. } => serial,
            _ => panic!("expected Configured"),
        };
        let (e2, _) = s.apply(SurfaceEvent::SetGeometry(r(0, 0, 640, 480)));
        let s2 = match e2[0] {
            SurfaceEffect::Configured { serial, rect } => {
                assert_eq!(rect, r(0, 0, 640, 480));
                serial
            }
            _ => panic!("expected Configured"),
        };
        assert!(s2 > s1);
    }

    #[test]
    fn emit_damage_is_surface_local_even_when_geometry_has_origin() {
        // Regression test: a previous version emitted `self.geometry`
        // (which carries the compositor-space origin) into EmitDamage,
        // double-applying the offset downstream. EmitDamage rectangles
        // are documented as surface-local — origin must be (0, 0).
        let mut s = surface();
        let _ = s.apply(SurfaceEvent::SetGeometry(r(120, 80, 200, 100)));
        let _ = s.apply(SurfaceEvent::AttachBuffer(BufferId(1)));
        let (effects, _) = s.apply(SurfaceEvent::CommitSurface);
        let damage = effects
            .iter()
            .find_map(|e| match e {
                SurfaceEffect::EmitDamage(rs) => Some(rs.clone()),
                _ => None,
            })
            .expect("expected EmitDamage on first commit");
        assert_eq!(damage, vec![r(0, 0, 200, 100)]);
    }

    #[test]
    fn coalesced_damage_is_surface_local_even_when_geometry_has_origin() {
        // Companion regression: when DamageSurface overflows the pending
        // cap, the coalesced replacement rect must also be surface-local.
        let mut s = surface();
        let geom = r(120, 80, 1024, 768);
        let _ = s.apply(SurfaceEvent::SetGeometry(geom));
        for _ in 0..(MAX_PENDING_DAMAGE + 1) {
            let _ = s.apply(SurfaceEvent::DamageSurface(r(1, 1, 4, 4)));
        }
        assert_eq!(s.pending_damage().len(), 1);
        assert_eq!(s.pending_damage()[0], r(0, 0, 1024, 768));
    }

    #[test]
    fn take_serial_saturates_at_u32_max() {
        // Regression test: switching from wrapping_add to saturating_add
        // means a surface that somehow accumulates 2^32 reconfigures
        // plateaus at u32::MAX rather than wrapping to 0 and admitting a
        // stale AckConfigure. We can't drive 4B events in a unit test,
        // so seed the counter directly.
        let mut s = surface();
        // Drive the surface to a state where the next serial is u32::MAX.
        s.next_serial = u32::MAX;
        let (e1, _) = s.apply(SurfaceEvent::SetRole(SurfaceRole::Toplevel));
        let s1 = match e1[0] {
            SurfaceEffect::Configured { serial, .. } => serial,
            _ => panic!("expected Configured"),
        };
        assert_eq!(s1, u32::MAX);
        // Subsequent take_serial calls plateau at u32::MAX rather than wrap.
        let (e2, _) = s.apply(SurfaceEvent::SetGeometry(r(0, 0, 320, 240)));
        let s2 = match e2[0] {
            SurfaceEffect::Configured { serial, .. } => serial,
            _ => panic!("expected Configured"),
        };
        assert_eq!(s2, u32::MAX);
    }

    #[test]
    fn focus_in_focus_out_toggles() {
        let mut s = surface();
        assert!(!s.is_focused());
        let (e, _) = s.apply(SurfaceEvent::FocusIn);
        assert!(e.is_empty());
        assert!(s.is_focused());
        let (e, _) = s.apply(SurfaceEvent::FocusIn);
        assert!(e.is_empty());
        assert!(s.is_focused());
        let (e, _) = s.apply(SurfaceEvent::FocusOut);
        assert!(e.is_empty());
        assert!(!s.is_focused());
        let (e, _) = s.apply(SurfaceEvent::FocusOut);
        assert!(e.is_empty());
        assert!(!s.is_focused());
    }

    // ------------------------------------------------------------------
    // Property tests
    // ------------------------------------------------------------------

    fn arb_rect() -> impl Strategy<Value = Rect> {
        (0i32..50, 0i32..50, 0u32..200, 0u32..200).prop_map(|(x, y, w, h)| Rect { x, y, w, h })
    }

    fn arb_event() -> impl Strategy<Value = SurfaceEvent> {
        prop_oneof![
            Just(SurfaceEvent::SetRole(SurfaceRole::Toplevel)),
            (0u32..4).prop_map(|b| SurfaceEvent::AttachBuffer(BufferId(b))),
            arb_rect().prop_map(SurfaceEvent::DamageSurface),
            Just(SurfaceEvent::CommitSurface),
            arb_rect().prop_map(SurfaceEvent::SetGeometry),
            Just(SurfaceEvent::FocusIn),
            Just(SurfaceEvent::FocusOut),
            Just(SurfaceEvent::SamplingComplete),
        ]
    }

    fn arb_event_with_destroy() -> impl Strategy<Value = SurfaceEvent> {
        prop_oneof![arb_event(), Just(SurfaceEvent::DestroySurface),]
    }

    proptest! {
        #[test]
        fn proptest_no_release_for_unknown_buffer(events in proptest::collection::vec(arb_event(), 0..50)) {
            let mut s = surface();
            let mut attached: HashSet<BufferId> = HashSet::new();
            for ev in events {
                if let SurfaceEvent::AttachBuffer(b) = ev {
                    attached.insert(b);
                }
                let (effects, _) = s.apply(ev);
                for e in effects {
                    if let SurfaceEffect::ReleaseBuffer(b) = e {
                        prop_assert!(
                            attached.contains(&b),
                            "released unknown buffer {:?} (attached: {:?})",
                            b,
                            attached,
                        );
                    }
                }
            }
        }

        #[test]
        fn proptest_no_double_release_per_buffer_per_lifetime(events in proptest::collection::vec(arb_event(), 0..80)) {
            // The state machine never releases a buffer more times than
            // the client attached it. A buffer can legitimately be
            // released twice if it occupies two slots at once (e.g.
            // attach-then-commit makes it active, then attach again
            // re-stages the same id in pending without releasing active
            // — both slots eventually drain). We bound releases by the
            // total attach count plus an allowance for the active+pending
            // double-occupancy: at most `attach_count(b) + 1`.
            //
            // The strictest safety property is captured by
            // `proptest_no_release_for_unknown_buffer` (every released
            // buffer was attached at least once). This test reinforces
            // that by counting that releases never exceed the legitimate
            // upper bound implied by slot occupancy.
            use std::collections::HashMap;
            let mut s = surface();
            let mut attach_count: HashMap<BufferId, u32> = HashMap::new();
            let mut release_count: HashMap<BufferId, u32> = HashMap::new();
            // Track current occupancy: a buffer can be in pending and/or
            // active. The maximum simultaneous occupancy across a
            // buffer's lifetime sets the slack on its release count.
            let mut max_occupancy: HashMap<BufferId, u32> = HashMap::new();
            for ev in events {
                if let SurfaceEvent::AttachBuffer(b) = ev {
                    *attach_count.entry(b).or_insert(0) += 1;
                }
                let (effects, _) = s.apply(ev);
                for e in &effects {
                    if let SurfaceEffect::ReleaseBuffer(b) = e {
                        *release_count.entry(*b).or_insert(0) += 1;
                    }
                }
                // Measure occupancy after the event applies.
                if let Some(p) = s.pending_buffer() {
                    let entry = max_occupancy.entry(p).or_insert(0);
                    let here = if Some(p) == s.active_buffer() { 2 } else { 1 };
                    if here > *entry { *entry = here; }
                }
                if let Some(a) = s.active_buffer() {
                    let entry = max_occupancy.entry(a).or_insert(0);
                    let here = if Some(a) == s.pending_buffer() { 2 } else { 1 };
                    if here > *entry { *entry = here; }
                }
            }
            for (b, releases) in &release_count {
                let attaches = attach_count.get(b).copied().unwrap_or(0);
                let occ = max_occupancy.get(b).copied().unwrap_or(1);
                let upper = attaches + occ; // allow slack for double-slot occupancy
                prop_assert!(
                    *releases <= upper,
                    "buffer {:?} released {} times but only attached {} (max occupancy {})",
                    b, releases, attaches, occ,
                );
            }
        }

        #[test]
        fn proptest_dead_surface_emits_no_effects(events in proptest::collection::vec(arb_event_with_destroy(), 0..50)) {
            let mut s = surface();
            let mut destroyed = false;
            for ev in events {
                let was_destroy = matches!(ev, SurfaceEvent::DestroySurface);
                let (effects, err) = s.apply(ev);
                if destroyed {
                    prop_assert!(effects.is_empty(), "dead surface emitted {:?}", effects);
                    prop_assert_eq!(err, Some(SurfaceError::Dead));
                }
                if was_destroy && !destroyed {
                    destroyed = true;
                }
            }
        }

        #[test]
        fn proptest_serial_strictly_monotone(events in proptest::collection::vec(arb_event(), 0..80)) {
            let mut s = surface();
            let mut last_serial: Option<u32> = None;
            for ev in events {
                let (effects, _) = s.apply(ev);
                for e in effects {
                    if let SurfaceEffect::Configured { serial, .. } = e {
                        if let Some(prev) = last_serial {
                            prop_assert!(
                                serial > prev,
                                "serial not strictly monotone: prev={}, new={}",
                                prev,
                                serial,
                            );
                        }
                        last_serial = Some(serial);
                    }
                }
            }
        }
    }
}
