//! Pure-logic layout/constraint solver (Phase 105 Track A.2).
//!
//! The falsifiable core of the toolkit: given a container ([`LayoutSpec`]
//! — direction, bounds, padding, inter-item spacing) and a list of item
//! size specs ([`Item`] — a fixed or flex main-axis extent, and an
//! optional fixed cross extent), [`solve`] returns each item's placed
//! [`Rect`]. No framebuffer, no IPC — just arithmetic, so it is
//! host-tested exhaustively (`cargo test -p m3ui`).
//!
//! Flex distribution is deterministic and gap-free: leftover main-axis
//! space is split by weight, and the integer remainder is handed one
//! pixel at a time to the earliest flex items, so `sum(item extents) +
//! spacing == available` exactly (no off-by-rounding seams between
//! adjacent widgets).
//!
//! A [`ClipStack`] tracks the intersected drawing region for nested
//! containers; widgets consult `top()` to scissor their output.

use alloc::vec::Vec;

use crate::geom::Rect;

/// Layout axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    /// Items flow left→right; main axis is x.
    Row,
    /// Items flow top→bottom; main axis is y.
    Column,
}

/// An item's extent along the layout's main axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Size {
    /// An exact main-axis extent in pixels (clamped ≥ 0).
    Fixed(i32),
    /// A share of the leftover space after fixed items, by integer
    /// weight. Weight 0 behaves as `Fixed(0)`.
    Flex(u16),
}

/// Per-side padding inside a container.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Padding {
    pub left: i32,
    pub right: i32,
    pub top: i32,
    pub bottom: i32,
}

impl Padding {
    /// Uniform padding on all four sides.
    pub const fn all(p: i32) -> Padding {
        Padding {
            left: p,
            right: p,
            top: p,
            bottom: p,
        }
    }

    /// Separate horizontal/vertical padding.
    pub const fn symmetric(h: i32, v: i32) -> Padding {
        Padding {
            left: h,
            right: h,
            top: v,
            bottom: v,
        }
    }
}

/// One item to place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Item {
    /// Extent along the layout direction.
    pub main: Size,
    /// Fixed extent across the layout direction, or `None` to stretch to
    /// the container's cross extent.
    pub cross: Option<i32>,
}

impl Item {
    /// A fixed-main, cross-stretched item (the common widget case).
    pub fn fixed(main: i32) -> Item {
        Item {
            main: Size::Fixed(main),
            cross: None,
        }
    }

    /// A flex-main, cross-stretched item.
    pub fn flex(weight: u16) -> Item {
        Item {
            main: Size::Flex(weight),
            cross: None,
        }
    }

    /// Set a fixed cross extent.
    pub fn with_cross(mut self, cross: i32) -> Item {
        self.cross = Some(cross);
        self
    }
}

/// A container to lay out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayoutSpec {
    pub dir: Dir,
    pub bounds: Rect,
    pub padding: Padding,
    /// Gap inserted between consecutive items (not before the first or
    /// after the last).
    pub spacing: i32,
}

impl LayoutSpec {
    pub fn new(dir: Dir, bounds: Rect) -> LayoutSpec {
        LayoutSpec {
            dir,
            bounds,
            padding: Padding::default(),
            spacing: 0,
        }
    }

    pub fn padding(mut self, p: Padding) -> LayoutSpec {
        self.padding = p;
        self
    }

    pub fn spacing(mut self, s: i32) -> LayoutSpec {
        self.spacing = s.max(0);
        self
    }

    /// The content region after applying padding.
    pub fn content(&self) -> Rect {
        let b = self.bounds;
        let x = b.x + self.padding.left;
        let y = b.y + self.padding.top;
        let w = (b.w - self.padding.left - self.padding.right).max(0);
        let h = (b.h - self.padding.top - self.padding.bottom).max(0);
        Rect { x, y, w, h }
    }
}

/// Solve a container: place `items` and return their rects, in order.
/// Always returns exactly `items.len()` rects.
pub fn solve(spec: &LayoutSpec, items: &[Item]) -> Vec<Rect> {
    let content = spec.content();
    let n = items.len();
    let mut out = Vec::with_capacity(n);
    if n == 0 {
        return out;
    }

    let (main_avail, cross_avail) = match spec.dir {
        Dir::Row => (content.w, content.h),
        Dir::Column => (content.h, content.w),
    };
    // Total inter-item spacing; the space left for item extents.
    let total_spacing = spec.spacing.saturating_mul((n as i32) - 1);
    let item_space = (main_avail - total_spacing).max(0);

    // Pass 1: sum fixed extents + flex weights.
    let mut fixed_sum: i32 = 0;
    let mut flex_total: u32 = 0;
    for it in items {
        match it.main {
            Size::Fixed(v) => fixed_sum += v.max(0),
            Size::Flex(w) => flex_total += w as u32,
        }
    }
    let leftover = (item_space - fixed_sum).max(0);

    // Pass 2: assign each flex item its weighted share, distributing the
    // integer remainder one pixel at a time to the earliest flex items so
    // the extents sum exactly to `leftover` (no rounding gap).
    let mut flex_extents: Vec<i32> = Vec::with_capacity(n);
    if flex_total > 0 {
        let mut assigned: i32 = 0;
        // First pass computes floor shares and tracks the running total.
        for it in items {
            match it.main {
                Size::Flex(w) => {
                    let share = ((leftover as i64) * (w as i64) / (flex_total as i64)) as i32;
                    flex_extents.push(share);
                    assigned += share;
                }
                _ => flex_extents.push(0),
            }
        }
        // Hand the remainder to the earliest flex items (weight > 0).
        let mut rem = leftover - assigned;
        if rem > 0 {
            for (i, it) in items.iter().enumerate() {
                if rem == 0 {
                    break;
                }
                if let Size::Flex(w) = it.main
                    && w > 0
                {
                    flex_extents[i] += 1;
                    rem -= 1;
                }
            }
        }
    } else {
        flex_extents.resize(n, 0);
    }

    // Place items along the main axis.
    let mut cursor = 0i32;
    for (i, it) in items.iter().enumerate() {
        let main_ext = match it.main {
            Size::Fixed(v) => v.max(0),
            Size::Flex(_) => flex_extents[i],
        };
        let cross_ext = it
            .cross
            .map(|c| c.max(0))
            .unwrap_or(cross_avail)
            .min(cross_avail);
        let rect = match spec.dir {
            Dir::Row => Rect {
                x: content.x + cursor,
                y: content.y,
                w: main_ext,
                h: cross_ext,
            },
            Dir::Column => Rect {
                x: content.x,
                y: content.y + cursor,
                w: cross_ext,
                h: main_ext,
            },
        };
        out.push(rect);
        cursor += main_ext + spec.spacing;
    }
    out
}

/// A stack of intersected clip rects for nested containers. `top()` is
/// the region drawing is currently scissored to; pushing intersects with
/// the current top so a child never draws outside its parent.
#[derive(Debug, Clone)]
pub struct ClipStack {
    stack: Vec<Rect>,
}

impl ClipStack {
    /// A new stack clipped to `root`.
    pub fn new(root: Rect) -> ClipStack {
        ClipStack {
            stack: alloc::vec![root],
        }
    }

    /// The current clip region.
    pub fn top(&self) -> Rect {
        *self.stack.last().expect("clip stack never empty")
    }

    /// Push `r` intersected with the current top.
    pub fn push(&mut self, r: Rect) {
        let clipped = self.top().intersect(&r);
        self.stack.push(clipped);
    }

    /// Pop the last pushed clip (never pops the root).
    pub fn pop(&mut self) {
        if self.stack.len() > 1 {
            self.stack.pop();
        }
    }

    pub fn depth(&self) -> usize {
        self.stack.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn col(bounds: Rect) -> LayoutSpec {
        LayoutSpec::new(Dir::Column, bounds)
    }

    #[test]
    fn empty_items_yields_empty() {
        assert!(solve(&col(Rect::new(0, 0, 100, 100)), &[]).is_empty());
    }

    #[test]
    fn fixed_column_stacks_with_spacing() {
        let spec = col(Rect::new(0, 0, 100, 100)).spacing(10);
        let items = [Item::fixed(20), Item::fixed(30)];
        let r = solve(&spec, &items);
        assert_eq!(r[0], Rect::new(0, 0, 100, 20));
        // Second item starts after 20 + 10 spacing.
        assert_eq!(r[1], Rect::new(0, 30, 100, 30));
    }

    #[test]
    fn padding_insets_content() {
        let spec = col(Rect::new(0, 0, 100, 100)).padding(Padding::all(8));
        let items = [Item::fixed(20)];
        let r = solve(&spec, &items);
        // Content is (8,8, 84,84); cross stretches to 84.
        assert_eq!(r[0], Rect::new(8, 8, 84, 20));
    }

    #[test]
    fn flex_fills_leftover_and_is_gap_free() {
        // 100 tall, one fixed 40, two flex 1:1 → leftover 60 → 30 each.
        let spec = col(Rect::new(0, 0, 50, 100));
        let items = [Item::fixed(40), Item::flex(1), Item::flex(1)];
        let r = solve(&spec, &items);
        assert_eq!(r[0].h, 40);
        assert_eq!(r[1].h, 30);
        assert_eq!(r[2].h, 30);
        // Exactly fills the container: last item's bottom == 100.
        assert_eq!(r[2].bottom(), 100);
    }

    #[test]
    fn flex_remainder_goes_to_earliest_no_gap() {
        // leftover 100 across weights 1:1:1 → 33,33,33 + remainder 1 → the
        // first flex item gets 34; total is exactly 100.
        let spec = col(Rect::new(0, 0, 10, 100));
        let items = [Item::flex(1), Item::flex(1), Item::flex(1)];
        let r = solve(&spec, &items);
        assert_eq!((r[0].h, r[1].h, r[2].h), (34, 33, 33));
        assert_eq!(r[2].bottom(), 100);
    }

    #[test]
    fn weighted_flex_split() {
        // leftover 90, weights 2:1 → 60, 30.
        let spec = col(Rect::new(0, 0, 10, 90));
        let items = [Item::flex(2), Item::flex(1)];
        let r = solve(&spec, &items);
        assert_eq!((r[0].h, r[1].h), (60, 30));
    }

    #[test]
    fn row_lays_out_horizontally() {
        let spec = LayoutSpec::new(Dir::Row, Rect::new(0, 0, 100, 40)).spacing(5);
        let items = [Item::fixed(30), Item::flex(1)];
        let r = solve(&spec, &items);
        assert_eq!(r[0], Rect::new(0, 0, 30, 40));
        // flex fills 100 - 30 - 5 = 65, starting at x = 35.
        assert_eq!(r[1], Rect::new(35, 0, 65, 40));
    }

    #[test]
    fn fixed_cross_is_honored_and_clamped() {
        let spec = col(Rect::new(0, 0, 100, 100));
        let items = [
            Item::fixed(20).with_cross(40),
            Item::fixed(20).with_cross(500),
        ];
        let r = solve(&spec, &items);
        assert_eq!(r[0].w, 40);
        assert_eq!(r[1].w, 100, "cross clamps to content width");
    }

    #[test]
    fn overflow_does_not_produce_negative_extents() {
        // Fixed items exceed the container: no flex space, extents stay
        // as requested but never negative.
        let spec = col(Rect::new(0, 0, 10, 30)).spacing(20);
        let items = [Item::fixed(40), Item::flex(1)];
        let r = solve(&spec, &items);
        assert_eq!(r[0].h, 40);
        assert_eq!(r[1].h, 0, "no leftover for flex");
    }

    #[test]
    fn clip_stack_intersects_and_pops() {
        let mut cs = ClipStack::new(Rect::new(0, 0, 100, 100));
        cs.push(Rect::new(50, 50, 100, 100));
        assert_eq!(cs.top(), Rect::new(50, 50, 50, 50));
        cs.push(Rect::new(0, 0, 60, 60));
        assert_eq!(cs.top(), Rect::new(50, 50, 10, 10));
        cs.pop();
        assert_eq!(cs.top(), Rect::new(50, 50, 50, 50));
        cs.pop();
        cs.pop(); // never pops the root
        assert_eq!(cs.top(), Rect::new(0, 0, 100, 100));
        assert_eq!(cs.depth(), 1);
    }
}
