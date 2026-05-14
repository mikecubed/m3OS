//! Phase 68 Track B — compositor damage tracking.
//!
//! `DamageTracker` collects per-frame dirty rectangles so the
//! compositor can blit a clipped subset of the screen instead of
//! repainting the entire framebuffer on every tick. Phase 56 shipped
//! the cursor-motion gate but the actual blit path repaints every
//! mapped surface on every cursor frame (`compose.rs:164-175` documents
//! this trade-off); Phase 68 closes the gap so cursor-only frames blit
//! only the area swept by the old + new cursor positions.
//!
//! Pure data structure: no I/O, no allocation past the bounded
//! `Vec<DamageRect>`. The tracker lives one level below `compose_frame`
//! — the compositor calls [`DamageTracker::mark_dirty`] for each
//! surface or cursor motion and asks [`DamageTracker::union_rect`] for
//! the clipping rectangle to feed into `write_pixels`.

extern crate alloc;

use alloc::vec::Vec;

use crate::display::protocol::Rect;

/// A single damaged rectangle in screen-space coordinates.
///
/// Aliased to [`Rect`] so the wider Phase 56 plumbing
/// (`compose_frame`, `FramebufferOwner::write_pixels`) continues to
/// take a single rectangle type. The alias documents intent at the
/// damage-tracking sites without proliferating types.
pub type DamageRect = Rect;

/// Maximum number of separate rectangles the tracker stores before
/// collapsing to a full-repaint signal.
///
/// 16 is plenty for the Phase 68 workload: each surface contributes at
/// most one rect per frame, plus the cursor's two-rect (old + new)
/// damage, plus a few cursor swap transitions. When the cap is
/// exceeded, [`DamageTracker::is_full_repaint_needed`] returns `true`
/// and the compositor falls back to repainting the entire output —
/// the bounded cost prevents adversarial damage streams from forcing
/// an unbounded `Vec`.
pub const MAX_DAMAGE_RECTS: usize = 16;

/// Per-frame damage accumulator.
///
/// The tracker is owned by the compositor (`compose.rs` keeps one in
/// the `ComposeContext`). The compositor calls
/// [`mark_dirty`](Self::mark_dirty) for each rectangle that needs
/// repainting and queries [`union_rect`](Self::union_rect) +
/// [`is_full_repaint_needed`](Self::is_full_repaint_needed) once per
/// frame to decide the clipping strategy.
#[derive(Debug, Clone)]
pub struct DamageTracker {
    rects: Vec<DamageRect>,
    /// Tracks "first-frame" + capacity-overflow + explicit
    /// invalidation. When `true`, the compositor must repaint
    /// everything regardless of `rects`.
    full_repaint: bool,
}

impl Default for DamageTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl DamageTracker {
    /// Construct a fresh tracker. The first frame always reports
    /// [`is_full_repaint_needed`](Self::is_full_repaint_needed) `true`
    /// — the compositor has no prior frame to delta against.
    pub fn new() -> Self {
        Self {
            rects: Vec::new(),
            full_repaint: true,
        }
    }

    /// Mark `rect` as dirty.
    ///
    /// If an existing entry already covers `rect`, this is a no-op. If
    /// `rect` overlaps an existing entry, the two are merged into a
    /// single bounding rect to keep the rect list compact. If the
    /// merge would push the tracker past [`MAX_DAMAGE_RECTS`], the
    /// tracker collapses to a full-repaint signal (see
    /// [`is_full_repaint_needed`](Self::is_full_repaint_needed)).
    ///
    /// Empty rects (`w == 0 || h == 0`) are ignored.
    pub fn mark_dirty(&mut self, rect: DamageRect) {
        if rect.w == 0 || rect.h == 0 {
            return;
        }
        if self.full_repaint {
            // Already full-repaint; no point tracking individual rects.
            return;
        }

        // Merge with any overlapping existing rect.
        let mut merged = rect;
        let mut i = 0;
        while i < self.rects.len() {
            if rects_overlap(merged, self.rects[i]) {
                merged = bounding_union(merged, self.rects[i]);
                // Remove the absorbed rect and rescan from index 0 so a
                // single newly-merged rect can absorb other formerly-
                // disjoint rects that now overlap.
                self.rects.swap_remove(i);
                i = 0;
            } else {
                i += 1;
            }
        }

        if self.rects.len() >= MAX_DAMAGE_RECTS {
            // Capacity overflow — drop to full repaint to keep the
            // tracker bounded.
            self.full_repaint = true;
            self.rects.clear();
            return;
        }

        self.rects.push(merged);
    }

    /// Mark the entire framebuffer as dirty (explicit invalidation).
    /// Future [`mark_dirty`](Self::mark_dirty) calls are no-ops until
    /// [`reset`](Self::reset) is called.
    pub fn mark_full_repaint(&mut self) {
        self.full_repaint = true;
        self.rects.clear();
    }

    /// Compute the bounding union of every tracked rectangle.
    ///
    /// Returns `None` when there is no damage to repaint (and the
    /// `full_repaint` flag is clear — see
    /// [`is_full_repaint_needed`](Self::is_full_repaint_needed) for
    /// the full-repaint signal).
    ///
    /// `Some(rect)` is the smallest bounding rectangle that contains
    /// every tracked dirty rect — the compositor uses this as the
    /// `write_pixels` clip rectangle on partial-repaint frames.
    pub fn union_rect(&self) -> Option<DamageRect> {
        if self.rects.is_empty() {
            return None;
        }
        let mut acc = self.rects[0];
        for r in &self.rects[1..] {
            acc = bounding_union(acc, *r);
        }
        Some(acc)
    }

    /// `true` when the next compose pass must repaint the entire
    /// output: the very first frame, an explicit invalidation, or a
    /// capacity-overflow collapse.
    pub fn is_full_repaint_needed(&self) -> bool {
        self.full_repaint
    }

    /// Drop all tracked rectangles and clear the full-repaint flag.
    /// The compositor calls this at the end of a successful compose
    /// pass so the next frame starts with a clean slate.
    pub fn reset(&mut self) {
        self.rects.clear();
        self.full_repaint = false;
    }

    /// Number of distinct rectangles currently held. Used by tests
    /// and observability.
    pub fn len(&self) -> usize {
        self.rects.len()
    }

    /// `true` when no rects are tracked. Distinct from
    /// `is_full_repaint_needed()` — an empty tracker after `reset()`
    /// reports `len() == 0` and `is_full_repaint_needed() == false`.
    pub fn is_empty(&self) -> bool {
        self.rects.is_empty()
    }

    /// Snapshot of the currently tracked rects. Used by tests and
    /// debug dumps. Order is unspecified — the publish path merges
    /// overlapping rects and the merge order does not preserve
    /// insertion order.
    pub fn rects(&self) -> &[DamageRect] {
        &self.rects
    }
}

/// True iff `a` and `b` share any pixels (rectangles touching only at
/// the edge count as overlapping so the merge step is allowed to
/// coalesce abutting damage rects into a single bounding box).
fn rects_overlap(a: DamageRect, b: DamageRect) -> bool {
    if a.w == 0 || a.h == 0 || b.w == 0 || b.h == 0 {
        return false;
    }
    let a_x2 = (a.x as i64) + (a.w as i64);
    let a_y2 = (a.y as i64) + (a.h as i64);
    let b_x2 = (b.x as i64) + (b.w as i64);
    let b_y2 = (b.y as i64) + (b.h as i64);
    !((a_x2 < b.x as i64) || (b_x2 < a.x as i64) || (a_y2 < b.y as i64) || (b_y2 < a.y as i64))
}

/// Smallest bounding rectangle containing both `a` and `b`.
fn bounding_union(a: DamageRect, b: DamageRect) -> DamageRect {
    let x1 = a.x.min(b.x);
    let y1 = a.y.min(b.y);
    let a_x2 = (a.x as i64) + (a.w as i64);
    let a_y2 = (a.y as i64) + (a.h as i64);
    let b_x2 = (b.x as i64) + (b.w as i64);
    let b_y2 = (b.y as i64) + (b.h as i64);
    let x2 = a_x2.max(b_x2);
    let y2 = a_y2.max(b_y2);
    let w = (x2 - x1 as i64).max(0) as u64;
    let h = (y2 - y1 as i64).max(0) as u64;
    DamageRect {
        x: x1,
        y: y1,
        // Saturating cast — the bounding box can in principle exceed
        // `u32::MAX` on degenerate inputs (i32::MIN..i32::MAX). The
        // compositor's `write_pixels` will clip against the output
        // rect, so a saturated bound is safe.
        w: if w > u32::MAX as u64 {
            u32::MAX
        } else {
            w as u32
        },
        h: if h > u32::MAX as u64 {
            u32::MAX
        } else {
            h as u32
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(x: i32, y: i32, w: u32, h: u32) -> DamageRect {
        DamageRect { x, y, w, h }
    }

    #[test]
    fn empty_tracker_reports_full_repaint_until_reset() {
        let mut t = DamageTracker::new();
        assert!(t.is_full_repaint_needed());
        assert_eq!(t.union_rect(), None);
        assert!(t.is_empty());
        t.reset();
        assert!(!t.is_full_repaint_needed());
        assert!(t.is_empty());
    }

    #[test]
    fn single_rect_union_returns_that_rect() {
        let mut t = DamageTracker::new();
        t.reset();
        t.mark_dirty(r(10, 20, 30, 40));
        assert_eq!(t.union_rect(), Some(r(10, 20, 30, 40)));
        assert!(!t.is_full_repaint_needed());
        assert_eq!(t.len(), 1);
    }

    #[test]
    fn non_overlapping_rects_kept_separate_but_union_bounds_both() {
        let mut t = DamageTracker::new();
        t.reset();
        t.mark_dirty(r(0, 0, 10, 10));
        t.mark_dirty(r(100, 100, 10, 10));
        assert_eq!(t.len(), 2);
        let u = t.union_rect().expect("union");
        assert_eq!(u.x, 0);
        assert_eq!(u.y, 0);
        assert_eq!(u.w, 110);
        assert_eq!(u.h, 110);
    }

    #[test]
    fn overlapping_rects_merge_into_one_entry() {
        let mut t = DamageTracker::new();
        t.reset();
        t.mark_dirty(r(0, 0, 50, 50));
        t.mark_dirty(r(25, 25, 50, 50));
        assert_eq!(t.len(), 1);
        let merged = t.rects()[0];
        assert_eq!(merged, r(0, 0, 75, 75));
    }

    #[test]
    fn touching_edge_counts_as_overlap_so_abutting_rects_merge() {
        let mut t = DamageTracker::new();
        t.reset();
        t.mark_dirty(r(0, 0, 10, 10));
        t.mark_dirty(r(10, 0, 10, 10));
        assert_eq!(t.len(), 1);
        assert_eq!(t.rects()[0], r(0, 0, 20, 10));
    }

    #[test]
    fn empty_rect_is_ignored() {
        let mut t = DamageTracker::new();
        t.reset();
        t.mark_dirty(r(10, 10, 0, 5));
        t.mark_dirty(r(10, 10, 5, 0));
        assert_eq!(t.len(), 0);
        assert_eq!(t.union_rect(), None);
    }

    #[test]
    fn capacity_overflow_collapses_to_full_repaint() {
        let mut t = DamageTracker::new();
        t.reset();
        for i in 0..(MAX_DAMAGE_RECTS as i32) {
            // Each rect non-overlapping with all others so the merge
            // path doesn't coalesce them.
            t.mark_dirty(r(i * 100, i * 100, 10, 10));
        }
        assert!(!t.is_full_repaint_needed());
        // One more pushes the tracker into full-repaint mode.
        t.mark_dirty(r(9999, 9999, 10, 10));
        assert!(t.is_full_repaint_needed());
        assert_eq!(t.len(), 0);
        // Subsequent marks are no-ops while full-repaint is set.
        t.mark_dirty(r(5, 5, 5, 5));
        assert_eq!(t.len(), 0);
    }

    #[test]
    fn mark_full_repaint_invalidates_existing_rects() {
        let mut t = DamageTracker::new();
        t.reset();
        t.mark_dirty(r(0, 0, 5, 5));
        t.mark_full_repaint();
        assert!(t.is_full_repaint_needed());
        assert_eq!(t.len(), 0);
        assert_eq!(t.union_rect(), None);
    }

    #[test]
    fn reset_clears_full_repaint_flag() {
        let mut t = DamageTracker::new();
        t.reset();
        t.mark_full_repaint();
        assert!(t.is_full_repaint_needed());
        t.reset();
        assert!(!t.is_full_repaint_needed());
    }

    #[test]
    fn merging_cascade_absorbs_disjoint_rects() {
        // Two rects that are disjoint until a third bridges them; the
        // tracker should detect that and collapse all three.
        let mut t = DamageTracker::new();
        t.reset();
        t.mark_dirty(r(0, 0, 10, 10));
        t.mark_dirty(r(100, 0, 10, 10));
        assert_eq!(t.len(), 2);
        // Bridge rect overlaps both existing entries.
        t.mark_dirty(r(0, 0, 110, 10));
        assert_eq!(t.len(), 1);
        assert_eq!(t.rects()[0], r(0, 0, 110, 10));
    }
}
