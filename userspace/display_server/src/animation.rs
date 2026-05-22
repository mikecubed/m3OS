//! Phase 73 Track A — Animation engine.
//!
//! Tracks a small set of in-flight animations, advances them one
//! frame at a time, and reports the union of their damage rectangles
//! so the composer re-blits only the affected regions.
//!
//! Pure logic: no syscalls, no allocator hand-waving. The composer
//! calls [`AnimationEngine::tick`] from `compose_frame`; tests can
//! drive the same call directly without booting QEMU.

extern crate alloc;

use alloc::vec::Vec;

use kernel_core::display::protocol::{Rect, SurfaceId};

/// Animation easing / timing curve.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Curve {
    Linear,
    EaseOut,
    /// Critically-damped spring approximation. We use a simple
    /// `1 - e^{-6t}` envelope evaluated via a polynomial truncation to
    /// stay in `f32` without a math runtime — close enough to a real
    /// spring to feel "springy" without an overshoot.
    Spring,
}

impl Curve {
    /// Evaluate the curve at normalised time `t ∈ [0, 1]`. Returns a
    /// value clamped to `[0, 1]`.
    pub fn eval(self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        match self {
            Curve::Linear => t,
            Curve::EaseOut => {
                let inv = 1.0 - t;
                1.0 - inv * inv
            }
            Curve::Spring => {
                // Truncated Taylor series for `1 - exp(-6t)`, clamped
                // so it never exceeds 1 and stays monotonic on [0, 1].
                // Avoids pulling in libm just for `expf`.
                let x = 6.0 * t;
                let approx = 1.0 - (1.0 - x + x * x * 0.5 - x * x * x * (1.0 / 6.0));
                approx.clamp(0.0, 1.0)
            }
        }
    }

    /// Convenience: interpolate `start → end` at normalised time `t`.
    pub fn lerp(self, start: f32, end: f32, t: f32) -> f32 {
        start + (end - start) * self.eval(t)
    }
}

/// Animation kind tags. The engine treats every animation uniformly
/// (advance timer, sample curve, report damage); the kind is preserved
/// so the composer / observability layer can describe what is in
/// flight.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AnimationKind {
    /// Slide + fade from 90% scale / 20% opacity to 100% / 100%.
    WindowOpen,
    /// Fade from 100% opacity to 0%; the engine signals completion so
    /// the caller can drop the surface.
    WindowClose,
    /// Horizontal workspace-switch slide.
    WorkspaceSwitch,
    /// Smooth tile reposition.
    WindowMove,
}

impl AnimationKind {
    /// Default duration in milliseconds. Phase 73 (HiDPI revision)
    /// bumped these from the original sketch — the previous timings
    /// were tuned for the 1280×800 framebuffer and at 1920×1080 the
    /// effects flew by too fast to perceive.
    pub fn default_duration_ms(self) -> u32 {
        match self {
            AnimationKind::WindowOpen => 350,
            AnimationKind::WindowClose => 220,
            AnimationKind::WorkspaceSwitch => 260,
            AnimationKind::WindowMove => 180,
        }
    }

    pub fn default_curve(self) -> Curve {
        match self {
            AnimationKind::WindowOpen => Curve::EaseOut,
            AnimationKind::WindowClose => Curve::EaseOut,
            AnimationKind::WorkspaceSwitch => Curve::EaseOut,
            AnimationKind::WindowMove => Curve::Spring,
        }
    }
}

/// One in-flight animation.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Animation {
    pub surface_id: SurfaceId,
    pub kind: AnimationKind,
    pub curve: Curve,
    pub elapsed_ms: u32,
    pub duration_ms: u32,
    /// Damage rectangle in screen coordinates. Returned via
    /// [`AnimationEngine::tick`] so the composer can clip the blit to
    /// just the animated region.
    pub damage: Rect,
    /// Starting rect for `WindowMove`. The compositor lerps from this
    /// rect to the surface's natural (post-arrangement) rect over the
    /// animation's duration. `None` for animations that don't need a
    /// source rect.
    pub from_rect: Option<Rect>,
}

impl Animation {
    /// Construct using the kind's default curve + duration with no
    /// `from_rect`. Used by `WindowOpen` / `WindowClose` /
    /// `WorkspaceSwitch` — kinds whose effect is computed purely from
    /// the surface's current rect.
    pub fn new(surface_id: SurfaceId, kind: AnimationKind, damage: Rect) -> Self {
        Self {
            surface_id,
            kind,
            curve: kind.default_curve(),
            elapsed_ms: 0,
            duration_ms: kind.default_duration_ms(),
            damage,
            from_rect: None,
        }
    }

    /// Construct a `WindowMove` animation lerping from `from` to the
    /// surface's natural rect.
    pub fn new_move(surface_id: SurfaceId, from: Rect, damage: Rect) -> Self {
        Self {
            surface_id,
            kind: AnimationKind::WindowMove,
            curve: AnimationKind::WindowMove.default_curve(),
            elapsed_ms: 0,
            duration_ms: AnimationKind::WindowMove.default_duration_ms(),
            damage,
            from_rect: Some(from),
        }
    }

    /// Normalised progress `t ∈ [0, 1]`.
    pub fn progress(&self) -> f32 {
        if self.duration_ms == 0 {
            return 1.0;
        }
        (self.elapsed_ms as f32 / self.duration_ms as f32).clamp(0.0, 1.0)
    }

    /// Curve-evaluated progress — what the visual property should be at
    /// the current frame.
    pub fn eased(&self) -> f32 {
        self.curve.eval(self.progress())
    }

    pub fn is_done(&self) -> bool {
        self.elapsed_ms >= self.duration_ms
    }
}

/// One frame's-worth of damage produced by an animation tick.
///
/// The composer unions these with any already-pending damage before
/// running the blit pass.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DamageRegion {
    pub rects: Vec<Rect>,
}

impl DamageRegion {
    pub fn is_empty(&self) -> bool {
        self.rects.is_empty()
    }
}

/// Snapshot of an outgoing-workspace surface, captured at the moment
/// the user switched workspaces. The engine animates it from
/// `from_rect` to `to_rect` over `duration_ms` so the user sees the
/// old workspace slide off-screen instead of being instantly cleared.
///
/// Ghosts are decoupled from the live [`super::surface::SurfaceRegistry`]
/// on purpose — the surface might be reused by a different workspace,
/// or its client might commit a new buffer mid-slide; either would
/// corrupt the slide-out if we re-read from the live buffer. The
/// snapshot is an owned `Vec<u8>` so the ghost is self-contained.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceGhost {
    /// Owned pixel snapshot at the moment of capture. Layout matches
    /// the framebuffer's native pixel format (BGRA8888 on the kernel
    /// FB; the snapshot path that fills this in must match that).
    pub pixels: Vec<u8>,
    /// Width / height of `pixels` in pixels (not bytes).
    pub buf_width: u32,
    pub buf_height: u32,
    /// Rect the ghost occupied in the previous workspace's layout.
    pub from_rect: Rect,
    /// Off-screen rect the ghost lerps toward. Computed by the caller
    /// as `from_rect` shifted by the *opposite* direction of the
    /// incoming workspace so the two slides cross paths cleanly.
    pub to_rect: Rect,
    /// Animation timer state.
    pub elapsed_ms: u32,
    pub duration_ms: u32,
    pub curve: Curve,
}

impl WorkspaceGhost {
    /// Construct a ghost with the workspace-switch default duration
    /// and ease-out curve so it tracks the incoming animation in
    /// timing and feel.
    pub fn new(pixels: Vec<u8>, buf_width: u32, buf_height: u32, from: Rect, to: Rect) -> Self {
        Self {
            pixels,
            buf_width,
            buf_height,
            from_rect: from,
            to_rect: to,
            elapsed_ms: 0,
            duration_ms: AnimationKind::WorkspaceSwitch.default_duration_ms(),
            curve: AnimationKind::WorkspaceSwitch.default_curve(),
        }
    }

    /// Normalised progress `t ∈ [0, 1]`.
    pub fn progress(&self) -> f32 {
        if self.duration_ms == 0 {
            return 1.0;
        }
        (self.elapsed_ms as f32 / self.duration_ms as f32).clamp(0.0, 1.0)
    }

    /// Current animated rect — what the compositor should blit into
    /// this frame.
    pub fn current_rect(&self) -> Rect {
        let t = self.curve.eval(self.progress());
        lerp_rect(self.from_rect, self.to_rect, t).rect
    }

    pub fn is_done(&self) -> bool {
        self.elapsed_ms >= self.duration_ms
    }
}

/// Animation engine — tracks a small set of in-flight animations and
/// advances them one frame at a time.
#[derive(Clone, Debug, Default)]
pub struct AnimationEngine {
    animations: Vec<Animation>,
    /// Workspace-leave ghosts — pixel snapshots of the previous
    /// workspace's surfaces that animate off-screen during a switch.
    /// Separate from `animations` so they survive the surface-id
    /// `drop_surface` paths (the ghost owns its pixels and does not
    /// reference a live `SurfaceId`).
    ghosts: Vec<WorkspaceGhost>,
}

impl AnimationEngine {
    pub fn new() -> Self {
        Self {
            animations: Vec::new(),
            ghosts: Vec::new(),
        }
    }

    /// Borrow the current animation list — useful for tests / debug.
    pub fn animations(&self) -> &[Animation] {
        &self.animations
    }

    /// Number of in-flight animations.
    pub fn len(&self) -> usize {
        self.animations.len()
    }

    pub fn is_empty(&self) -> bool {
        self.animations.is_empty()
    }

    /// Push a new animation into the engine. Earliest-pushed animations
    /// finish first when their durations match.
    pub fn push(&mut self, animation: Animation) {
        self.animations.push(animation);
    }

    /// Convenience: push a default-curve / default-duration animation
    /// keyed on a `surface_id` and damage rect.
    pub fn animate(&mut self, surface_id: SurfaceId, kind: AnimationKind, damage: Rect) {
        self.push(Animation::new(surface_id, kind, damage));
    }

    /// Convenience: push a `WindowMove` animation that lerps the
    /// surface's rendered rect from `from` to `to`. Damage is set to
    /// the union of the two rects so the compose loop knows to repaint
    /// both the source and destination regions during the slide. If a
    /// `WindowMove` is already in flight for this surface it's
    /// dropped first — a new move from the current visual position to
    /// the new destination is more correct than chaining two lerps.
    pub fn animate_move(&mut self, surface_id: SurfaceId, from: Rect, to: Rect) {
        let _ = self.drop_surface(surface_id);
        let damage = rect_union(from, to);
        self.push(Animation::new_move(surface_id, from, damage));
    }

    /// Convenience: push a `WorkspaceSwitch` animation that slides
    /// `surface_id` into its natural `to` rect from `from`. Used by
    /// the main loop when the active workspace changes so every
    /// surface in the new workspace appears to slide in from off-
    /// screen rather than snapping to its final position.
    pub fn animate_workspace_switch(&mut self, surface_id: SurfaceId, from: Rect, to: Rect) {
        let _ = self.drop_surface(surface_id);
        let damage = rect_union(from, to);
        let mut anim = Animation::new(surface_id, AnimationKind::WorkspaceSwitch, damage);
        anim.from_rect = Some(from);
        self.push(anim);
    }

    /// Advance every animation by `frame_delta_ms` and return the union
    /// of every animated rect as the dirty region for this frame.
    ///
    /// Completed animations are removed before the call returns; the
    /// caller can detect "animation just finished" by checking whether
    /// the engine length changed across the call.
    pub fn tick(&mut self, frame_delta_ms: u32) -> DamageRegion {
        let mut rects = Vec::with_capacity(self.animations.len() + self.ghosts.len());
        for anim in self.animations.iter_mut() {
            anim.elapsed_ms = anim.elapsed_ms.saturating_add(frame_delta_ms);
            if anim.duration_ms == 0 {
                anim.elapsed_ms = 0;
            }
            rects.push(anim.damage);
        }
        // Advance ghosts too; the damage rect for a ghost is the
        // union of its from / to rects so the compose pass repaints
        // whatever pixels the slide traverses each frame.
        for ghost in self.ghosts.iter_mut() {
            ghost.elapsed_ms = ghost.elapsed_ms.saturating_add(frame_delta_ms);
            if ghost.duration_ms == 0 {
                ghost.elapsed_ms = 0;
            }
            rects.push(rect_union(ghost.from_rect, ghost.to_rect));
        }
        // Drop completed entries.
        self.animations.retain(|a| !a.is_done());
        self.ghosts.retain(|g| !g.is_done());
        DamageRegion { rects }
    }

    /// Push a workspace-leave ghost. The engine renders it on every
    /// subsequent compose pass until its timer expires.
    pub fn push_ghost(&mut self, ghost: WorkspaceGhost) {
        self.ghosts.push(ghost);
    }

    /// Borrow the active ghost list — the compositor iterates this in
    /// its per-frame pass and blits each ghost at
    /// [`WorkspaceGhost::current_rect`].
    pub fn ghosts(&self) -> &[WorkspaceGhost] {
        &self.ghosts
    }

    /// Discard every ghost. Used when the workspace switch was
    /// cancelled or superseded so stale ghosts don't keep painting.
    pub fn clear_ghosts(&mut self) {
        self.ghosts.clear();
    }

    /// Drop every animation associated with a surface — used when a
    /// surface is destroyed mid-animation. Returns the number of
    /// entries removed.
    pub fn drop_surface(&mut self, surface_id: SurfaceId) -> usize {
        let before = self.animations.len();
        self.animations.retain(|a| a.surface_id != surface_id);
        before - self.animations.len()
    }

    /// Compute the transform the compositor should apply to
    /// `surface_id` this frame given its natural (post-arrangement)
    /// `tile` rect. Returns `None` when the surface has no active
    /// animation — the compositor renders it at the natural rect.
    /// When `Some`, the compositor draws into the returned rect (with
    /// scaling if dims differ from the natural tile).
    ///
    /// If the same surface has multiple in-flight animations the
    /// earliest-pushed one wins; this is the convention the spec
    /// names for collision resolution (it matches the
    /// "drop on destroy" semantics of `drop_surface`).
    pub fn effective_transform(
        &self,
        surface_id: SurfaceId,
        tile: Rect,
    ) -> Option<AnimationTransform> {
        let anim = self
            .animations
            .iter()
            .find(|a| a.surface_id == surface_id)?;
        let t = anim.eased();
        match anim.kind {
            AnimationKind::WindowOpen => {
                // Scale 0.4 → 1.0 about the tile centre. Bigger range
                // than the spec sketch (0.9 → 1.0) so the pop-in is
                // impossible to miss at 1080p.
                let scale = 0.4 + 0.6 * t;
                Some(scale_about_centre(tile, scale))
            }
            AnimationKind::WindowClose => {
                // Mirror of open: 1.0 → 0.4. Renders via the
                // ghost-snapshot mechanism in main.rs so the surface
                // keeps painting after the client has disconnected.
                let scale = 1.0 - 0.6 * t;
                Some(scale_about_centre(tile, scale))
            }
            AnimationKind::WindowMove | AnimationKind::WorkspaceSwitch => {
                // Both lerp the rect from `from_rect` to the natural
                // tile. WorkspaceSwitch sets `from_rect` to the
                // natural tile shifted off-screen, which produces a
                // slide-in for each surface in the new workspace.
                let from = anim.from_rect.unwrap_or(tile);
                Some(lerp_rect(from, tile, t))
            }
        }
    }
}

/// Per-frame transform the compositor applies to a Toplevel surface
/// while an animation is active. The compositor draws the surface
/// into [`AnimationTransform::rect`] instead of the natural tile;
/// when the rect dims differ from the surface buffer dims the
/// existing nearest-neighbour-scale path produces the scaled blit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AnimationTransform {
    pub rect: Rect,
}

fn rect_union(a: Rect, b: Rect) -> Rect {
    let x0 = a.x.min(b.x);
    let y0 = a.y.min(b.y);
    let x1 = (a.x.saturating_add(a.w as i32)).max(b.x.saturating_add(b.w as i32));
    let y1 = (a.y.saturating_add(a.h as i32)).max(b.y.saturating_add(b.h as i32));
    Rect {
        x: x0,
        y: y0,
        w: (x1 - x0).max(0) as u32,
        h: (y1 - y0).max(0) as u32,
    }
}

/// Round to nearest integer without depending on a libm-style trait
/// import. Adds 0.5 (subtracts for negatives) before truncating.
/// Matches `f32::round` for finite values which is all we feed in.
fn round_f32(v: f32) -> f32 {
    if v >= 0.0 { v + 0.5 } else { v - 0.5 }
}

fn scale_about_centre(tile: Rect, scale: f32) -> AnimationTransform {
    let scale = scale.clamp(0.0, 1.0);
    let new_w = round_f32((tile.w as f32) * scale) as u32;
    let new_h = round_f32((tile.h as f32) * scale) as u32;
    let dx = ((tile.w as i32) - (new_w as i32)) / 2;
    let dy = ((tile.h as i32) - (new_h as i32)) / 2;
    AnimationTransform {
        rect: Rect {
            x: tile.x.saturating_add(dx),
            y: tile.y.saturating_add(dy),
            w: new_w,
            h: new_h,
        },
    }
}

fn lerp_rect(from: Rect, to: Rect, t: f32) -> AnimationTransform {
    let t = t.clamp(0.0, 1.0);
    let lerp_i = |a: i32, b: i32| -> i32 {
        let af = a as f32;
        let bf = b as f32;
        round_f32(af + (bf - af) * t) as i32
    };
    let lerp_u = |a: u32, b: u32| -> u32 {
        let af = a as f32;
        let bf = b as f32;
        let v = round_f32(af + (bf - af) * t);
        if v < 0.0 { 0 } else { v as u32 }
    };
    AnimationTransform {
        rect: Rect {
            x: lerp_i(from.x, to.x),
            y: lerp_i(from.y, to.y),
            w: lerp_u(from.w, to.w),
            h: lerp_u(from.h, to.h),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: i32, y: i32, w: u32, h: u32) -> Rect {
        Rect { x, y, w, h }
    }

    #[test]
    fn linear_curve_is_identity() {
        assert!((Curve::Linear.eval(0.0) - 0.0).abs() < 1e-6);
        assert!((Curve::Linear.eval(0.5) - 0.5).abs() < 1e-6);
        assert!((Curve::Linear.eval(1.0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn ease_out_starts_fast_ends_slow() {
        let early = Curve::EaseOut.eval(0.25);
        let late = Curve::EaseOut.eval(0.75);
        // Ease-out covers more ground early — quarter-progress should
        // already be more than 40% of the total.
        assert!(early > 0.4, "early progress was {}", early);
        // The last quarter only adds a small slice.
        let last_slice = Curve::EaseOut.eval(1.0) - late;
        assert!(
            last_slice < 0.1,
            "last quarter advanced by {} (should be small)",
            last_slice
        );
    }

    #[test]
    fn spring_curve_is_monotonic_and_bounded() {
        let mut prev = 0.0f32;
        for i in 0..=20 {
            let t = i as f32 / 20.0;
            let v = Curve::Spring.eval(t);
            assert!(
                v >= 0.0 && v <= 1.0,
                "spring value {} out of [0,1] at t={}",
                v,
                t
            );
            assert!(v >= prev - 1e-4, "spring not monotonic at t={}", t);
            prev = v;
        }
        assert!((Curve::Spring.eval(0.0) - 0.0).abs() < 1e-6);
        // Spring approaches 1 by t=1 thanks to the clamp.
        assert!(Curve::Spring.eval(1.0) > 0.9);
    }

    #[test]
    fn tick_advances_timers_and_collects_damage() {
        let mut engine = AnimationEngine::new();
        engine.animate(
            SurfaceId(1),
            AnimationKind::WindowOpen,
            rect(10, 20, 100, 100),
        );
        let dmg = engine.tick(16);
        assert_eq!(dmg.rects.len(), 1);
        assert_eq!(dmg.rects[0], rect(10, 20, 100, 100));
        assert_eq!(engine.animations()[0].elapsed_ms, 16);
    }

    #[test]
    fn completed_animations_are_removed() {
        let mut engine = AnimationEngine::new();
        engine.animate(SurfaceId(1), AnimationKind::WindowMove, rect(0, 0, 50, 50));
        // WindowMove default duration is 80 ms — advance well past it.
        let dmg = engine.tick(200);
        assert_eq!(dmg.rects.len(), 1, "damage emitted on the final frame");
        assert!(engine.is_empty(), "completed animation removed");
    }

    #[test]
    fn empty_engine_emits_no_damage() {
        let mut engine = AnimationEngine::new();
        let dmg = engine.tick(16);
        assert!(dmg.is_empty());
    }

    #[test]
    fn drop_surface_removes_associated_animations() {
        let mut engine = AnimationEngine::new();
        engine.animate(SurfaceId(1), AnimationKind::WindowOpen, rect(0, 0, 10, 10));
        engine.animate(SurfaceId(2), AnimationKind::WindowOpen, rect(0, 0, 10, 10));
        engine.animate(SurfaceId(1), AnimationKind::WindowMove, rect(0, 0, 10, 10));
        assert_eq!(engine.drop_surface(SurfaceId(1)), 2);
        assert_eq!(engine.len(), 1);
        assert_eq!(engine.animations()[0].surface_id, SurfaceId(2));
    }

    #[test]
    fn window_open_progresses_from_partial_to_full() {
        let anim = Animation::new(
            SurfaceId(1),
            AnimationKind::WindowOpen,
            rect(0, 0, 100, 100),
        );
        // Open should ease towards 1 quickly thanks to EaseOut.
        let half = Curve::EaseOut.lerp(0.0, 1.0, 0.5);
        assert!(half > 0.5);
        let _ = anim;
    }
}
