//! Phase 73 Track A — Animation engine.
//!
//! Tracks a small set of in-flight animations, advances them one
//! frame at a time, and reports the union of their damage rectangles
//! so the composer re-blits only the affected regions.
//!
//! Pure logic: no syscalls, no allocator hand-waving. The composer
//! calls [`AnimationEngine::tick`] from `compose_frame`; tests can
//! drive the same call directly without booting QEMU.
//!
//! Workspace-switch transitions are handled by [`WorkspaceSlide`]
//! rather than per-surface animations: a single slide value drives
//! the offset of every Toplevel in both the outgoing and incoming
//! workspace, with no per-surface allocation. This is the same
//! pattern Hyprland / KWin / Mutter use — the compositor never
//! snapshots pixels for a transition, it just renders the live
//! surfaces at a transformed position. Replaced the original
//! `WorkspaceGhost` design, which copied 8 MiB of pixels per surface
//! per switch and exhausted the kernel heap under rapid switching.

extern crate alloc;

use alloc::vec::Vec;

use kernel_core::display::protocol::{Rect, SurfaceId};

/// Animation easing / timing curve.
///
/// `Linear` is the identity curve: it completes the easing set and is what the
/// `curve_eval` unit tests below pin `eval` against, but no animation kind
/// currently selects it (`default_curve` picks `EaseOut`/`Spring`). Dropping it
/// would leave `eval` with no test-anchored reference point and force a config
/// or kind that wants a straight ramp to reintroduce it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Curve {
    #[allow(dead_code)]
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
    ///
    /// Scalar counterpart to the free `lerp_rect` the engine actually drives;
    /// no caller needs the scalar form today, but it is the piece any
    /// non-geometric animation (opacity, scale) would interpolate through, so
    /// it stays next to `eval` rather than being re-derived at the call site.
    #[allow(dead_code)]
    pub fn lerp(self, start: f32, end: f32, t: f32) -> f32 {
        start + (end - start) * self.eval(t)
    }
}

/// Animation kind tags. The engine treats every animation uniformly
/// (advance timer, sample curve, report damage); the kind is preserved
/// so the composer / observability layer can describe what is in
/// flight.
///
/// Workspace transitions are *not* per-surface animations — they live
/// in [`WorkspaceSlide`] and apply a uniform x-offset to every
/// Toplevel in the workspace. See the module docs.
// The shared `Window` prefix is load-bearing, not noise: this module also
// animates at workspace granularity (`WorkspaceSlide`, and the
// `AnimationKind::WorkspaceSwitch` variant that preceded it), and the prefix is
// what tells a reader which of the two scopes a kind acts on. Dropping it would
// leave `AnimationKind::Move` sitting beside `WorkspaceSlide` with nothing to
// say that one is per-surface and the other is per-workspace.
#[allow(clippy::enum_variant_names)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AnimationKind {
    /// Slide + fade from 90% scale / 20% opacity to 100% / 100%.
    WindowOpen,
    /// Fade from 100% opacity to 0%; the engine signals completion so
    /// the caller can drop the surface.
    ///
    /// Implemented end-to-end here (duration, curve, `transform_for`) but not
    /// yet started by anything: `main.rs` drops a Toplevel's state the moment
    /// the client disconnects, so there is no surviving pixel source to animate
    /// out. Wiring it needs the retained post-destroy snapshot `transform_for`
    /// refers to; the kind is kept so that work is a caller change only.
    #[allow(dead_code)]
    WindowClose,
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
            AnimationKind::WindowMove => 180,
        }
    }

    pub fn default_curve(self) -> Curve {
        match self {
            AnimationKind::WindowOpen => Curve::EaseOut,
            AnimationKind::WindowClose => Curve::EaseOut,
            AnimationKind::WindowMove => Curve::Spring,
        }
    }
}

/// Default duration of a workspace-slide transition in milliseconds.
/// Matches the old `AnimationKind::WorkspaceSwitch` default so the
/// perceived feel of the switch is unchanged.
pub const WORKSPACE_SLIDE_DURATION_MS: u32 = 260;

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
    /// `from_rect`. Used by `WindowOpen` / `WindowClose` — kinds whose
    /// effect is computed purely from the surface's current rect.
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

/// Single workspace-slide state. Drives the x-offset applied to every
/// Toplevel in the outgoing (`from_ws`) and incoming (`to_ws`)
/// workspaces during a transition. The compositor reads the offsets
/// from [`WorkspaceSlide::from_offset_x`] / [`to_offset_x`] each frame
/// and applies them to the workspace's natural tile arrangement.
///
/// Constant-size, no per-surface allocation. Replaces the original
/// `WorkspaceGhost` design which copied the entire pixel buffer of
/// every outgoing surface (8 MiB+ per surface at 1920×1080).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct WorkspaceSlide {
    /// Index of the workspace sliding *out*.
    pub from_ws: usize,
    /// Index of the workspace sliding *in*.
    pub to_ws: usize,
    /// `+1` when the new workspace enters from the right (forward
    /// step), `-1` when it enters from the left (backward step). Drives
    /// the sign of the offset.
    pub direction: i32,
    /// Output width in pixels — the magnitude of a full-screen slide.
    pub output_width: i32,
    /// Elapsed milliseconds since the slide started.
    pub elapsed_ms: u32,
    /// Total slide duration in milliseconds.
    pub duration_ms: u32,
    /// Easing curve.
    pub curve: Curve,
}

impl WorkspaceSlide {
    /// Normalised progress `t ∈ [0, 1]`.
    pub fn progress(&self) -> f32 {
        if self.duration_ms == 0 {
            return 1.0;
        }
        (self.elapsed_ms as f32 / self.duration_ms as f32).clamp(0.0, 1.0)
    }

    /// Curve-evaluated progress.
    pub fn eased(&self) -> f32 {
        self.curve.eval(self.progress())
    }

    /// X-offset to apply to every tile in the outgoing (`from_ws`)
    /// workspace this frame. Starts at `0` and lerps to `-output_width
    /// * direction` as the slide completes — i.e. the outgoing
    /// workspace slides off in the opposite direction of `direction`.
    ///
    /// The `from_` / `to_` prefixes here name this slide's `from_ws` / `to_ws`
    /// endpoints — they are not the conversion conventions
    /// `wrong_self_convention` is looking for, and renaming either one would
    /// break the pairing with the fields they read.
    #[allow(clippy::wrong_self_convention)]
    pub fn from_offset_x(&self) -> i32 {
        let t = self.eased();
        let mag = (t * self.output_width as f32) as i32;
        -mag * self.direction
    }

    /// X-offset to apply to every tile in the incoming (`to_ws`)
    /// workspace this frame. Starts at `output_width * direction` (off
    /// screen on the entry side) and lerps to `0` (in place).
    ///
    /// `to_` names the slide's `to_ws` endpoint — see `from_offset_x`. Taking
    /// `&self` keeps the pair symmetric.
    #[allow(clippy::wrong_self_convention)]
    pub fn to_offset_x(&self) -> i32 {
        let t = self.eased();
        let mag = ((1.0 - t) * self.output_width as f32) as i32;
        mag * self.direction
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
    /// Active workspace transition, if any. `None` between switches.
    /// At most one slide is in flight at a time — a fresh switch
    /// replaces the in-flight one.
    slide: Option<WorkspaceSlide>,
}

impl AnimationEngine {
    pub fn new() -> Self {
        Self {
            animations: Vec::new(),
            slide: None,
        }
    }

    /// Borrow the current animation list — useful for tests / debug.
    ///
    /// The compose loop only ever asks `is_empty()` / `transform_for()`, so
    /// these two read-only accessors exist for the unit tests below and for
    /// debug dumps. Keeping them costs nothing and avoids tests reaching into
    /// the private `animations` vec.
    #[allow(dead_code)]
    pub fn animations(&self) -> &[Animation] {
        &self.animations
    }

    /// Number of in-flight animations.
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.animations.len()
    }

    pub fn is_empty(&self) -> bool {
        self.animations.is_empty() && self.slide.is_none()
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

    /// Borrow the active workspace slide, if any.
    pub fn workspace_slide(&self) -> Option<&WorkspaceSlide> {
        self.slide.as_ref()
    }

    /// Start (or replace) a workspace slide. `direction` is `+1` when
    /// the new workspace enters from the right, `-1` from the left.
    /// `output_width` is the framebuffer width in pixels.
    ///
    /// A `from_ws == to_ws` request clears any in-flight slide. Any
    /// existing slide is replaced wholesale — there is no progress
    /// preservation across retargets. Mainstream compositors accept
    /// this minor visual snap to keep the state bounded; we do the
    /// same.
    pub fn request_workspace_slide(
        &mut self,
        from_ws: usize,
        to_ws: usize,
        direction: i32,
        output_width: i32,
    ) {
        if from_ws == to_ws || output_width <= 0 {
            self.slide = None;
            return;
        }
        let dir = if direction >= 0 { 1 } else { -1 };
        self.slide = Some(WorkspaceSlide {
            from_ws,
            to_ws,
            direction: dir,
            output_width,
            elapsed_ms: 0,
            duration_ms: WORKSPACE_SLIDE_DURATION_MS,
            curve: Curve::EaseOut,
        });
    }

    /// Cancel any in-flight workspace slide without producing damage.
    /// Intended for the case where the compose loop detects state that
    /// invalidates the slide entirely (e.g. the framebuffer was reclaimed from
    /// a fullscreen takeover and the previous slide context is stale).
    ///
    /// No caller yet: the Tier 1 reclaim handler in `main.rs` marks every
    /// surface dirty and requests a post-reclaim background fill, but leaves a
    /// slide that was in flight across the takeover running. Kept because that
    /// is the operation the reclaim path needs if the stale-slide case is ever
    /// observed — see the report accompanying this lint pass.
    #[allow(dead_code)]
    pub fn clear_workspace_slide(&mut self) {
        self.slide = None;
    }

    /// Advance every animation by `frame_delta_ms` and return the union
    /// of every animated rect as the dirty region for this frame.
    ///
    /// Completed animations are removed before the call returns; the
    /// caller can detect "animation just finished" by checking whether
    /// the engine length changed across the call.
    pub fn tick(&mut self, frame_delta_ms: u32) -> DamageRegion {
        let mut rects = Vec::with_capacity(self.animations.len() + 1);
        for anim in self.animations.iter_mut() {
            anim.elapsed_ms = anim.elapsed_ms.saturating_add(frame_delta_ms);
            if anim.duration_ms == 0 {
                anim.elapsed_ms = 0;
            }
            rects.push(anim.damage);
        }
        // Advance the workspace slide. While in flight the slide
        // damages the full output rect — a workspace transition is the
        // closest thing to a true full-screen repaint we have, and
        // emitting a single screen-spanning rect is cheaper than
        // computing the per-surface dirty list at sub-pixel sliding
        // offsets.
        if let Some(slide) = self.slide.as_mut() {
            slide.elapsed_ms = slide.elapsed_ms.saturating_add(frame_delta_ms);
            if slide.duration_ms == 0 {
                slide.elapsed_ms = 0;
            }
            rects.push(Rect {
                x: 0,
                y: 0,
                w: slide.output_width.max(0) as u32,
                h: u32::MAX,
            });
        }
        self.animations.retain(|a| !a.is_done());
        if let Some(slide) = self.slide.as_ref()
            && slide.is_done()
        {
            self.slide = None;
        }
        DamageRegion { rects }
    }

    /// Drop every animation associated with a surface — used when a
    /// surface is destroyed mid-animation. Returns the number of
    /// entries removed. Does not touch the workspace slide; a surface
    /// destroyed mid-slide simply stops appearing in the registry's
    /// compose iterator on the next frame.
    pub fn drop_surface(&mut self, surface_id: SurfaceId) -> usize {
        let before = self.animations.len();
        self.animations.retain(|a| a.surface_id != surface_id);
        before - self.animations.len()
    }

    /// Compute the transform the compositor should apply to
    /// `surface_id` this frame given its natural (post-arrangement)
    /// `tile` rect. Returns `None` when the surface has no active
    /// per-surface animation — the compositor renders it at the
    /// natural rect. When `Some`, the compositor draws into the
    /// returned rect (with scaling if dims differ from the natural
    /// tile).
    ///
    /// The workspace slide is *not* applied here; it is folded into
    /// the tile arrangement by the layout adapter before the compose
    /// pass reaches this method. That keeps per-surface animations
    /// (WindowOpen / WindowMove) and workspace transitions
    /// composable: an open animation on a slide-in surface scales the
    /// already-offset rect.
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
                // Mirror of open: 1.0 → 0.4. Unreachable today — see the
                // note on `AnimationKind::WindowClose`; nothing retains a
                // pixel source past client disconnect to feed this.
                let scale = 1.0 - 0.6 * t;
                Some(scale_about_centre(tile, scale))
            }
            AnimationKind::WindowMove => {
                // Lerp the rect from `from_rect` to the natural tile.
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
        assert!(early > 0.4, "early progress was {}", early);
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
        let dmg = engine.tick(500);
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
    fn workspace_slide_offsets_start_off_screen_and_end_at_zero() {
        let mut engine = AnimationEngine::new();
        engine.request_workspace_slide(0, 1, 1, 1920);
        let slide = engine.workspace_slide().expect("slide installed");
        // At progress=0 the incoming workspace is off-screen-right.
        assert_eq!(slide.to_offset_x(), 1920);
        // The outgoing workspace is in place.
        assert_eq!(slide.from_offset_x(), 0);
    }

    #[test]
    fn workspace_slide_completes_after_duration() {
        let mut engine = AnimationEngine::new();
        engine.request_workspace_slide(0, 1, 1, 1920);
        engine.tick(WORKSPACE_SLIDE_DURATION_MS + 50);
        assert!(engine.workspace_slide().is_none());
    }

    #[test]
    fn workspace_slide_direction_negative_enters_from_left() {
        let mut engine = AnimationEngine::new();
        engine.request_workspace_slide(2, 0, -1, 1920);
        let slide = engine.workspace_slide().expect("slide installed");
        // Backward step: new workspace enters from the left → negative
        // to_offset at t=0.
        assert_eq!(slide.to_offset_x(), -1920);
    }

    #[test]
    fn workspace_slide_request_with_same_ws_is_a_noop() {
        let mut engine = AnimationEngine::new();
        engine.request_workspace_slide(3, 3, 1, 1920);
        assert!(engine.workspace_slide().is_none());
    }

    #[test]
    fn workspace_slide_retarget_replaces_in_flight() {
        let mut engine = AnimationEngine::new();
        engine.request_workspace_slide(0, 1, 1, 1920);
        engine.tick(100);
        engine.request_workspace_slide(1, 2, 1, 1920);
        let slide = engine.workspace_slide().expect("retarget installed");
        assert_eq!(slide.from_ws, 1);
        assert_eq!(slide.to_ws, 2);
        assert_eq!(slide.elapsed_ms, 0);
    }

    #[test]
    fn engine_is_not_empty_while_slide_in_flight() {
        let mut engine = AnimationEngine::new();
        engine.request_workspace_slide(0, 1, 1, 1920);
        assert!(!engine.is_empty());
    }
}
