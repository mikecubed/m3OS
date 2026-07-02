//! Geometry primitives shared by the layout solver, input router, and
//! widgets. Integer pixel coordinates (framebuffer space); no floats.

/// A point in surface pixel space.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

impl Point {
    pub const fn new(x: i32, y: i32) -> Point {
        Point { x, y }
    }
}

/// An axis-aligned rectangle: origin (`x`,`y`) + size (`w`,`h`). Widths
/// and heights are non-negative by construction (the layout solver
/// clamps); an empty rect has `w == 0` or `h == 0`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Rect {
        Rect { x, y, w, h }
    }

    /// The empty rect at the origin.
    pub const ZERO: Rect = Rect {
        x: 0,
        y: 0,
        w: 0,
        h: 0,
    };

    pub fn right(&self) -> i32 {
        self.x + self.w
    }

    pub fn bottom(&self) -> i32 {
        self.y + self.h
    }

    pub fn is_empty(&self) -> bool {
        self.w <= 0 || self.h <= 0
    }

    /// Does this rect contain `p`? Right/bottom edges are exclusive so
    /// adjacent rects never both claim a pixel (clean hit-testing).
    pub fn contains(&self, p: Point) -> bool {
        p.x >= self.x && p.x < self.right() && p.y >= self.y && p.y < self.bottom()
    }

    /// Shrink on all four sides by `pad` (clamped at zero size).
    pub fn inset(&self, pad: i32) -> Rect {
        self.inset_xy(pad, pad)
    }

    /// Shrink by `px` horizontally and `py` vertically on each side.
    pub fn inset_xy(&self, px: i32, py: i32) -> Rect {
        let w = (self.w - 2 * px).max(0);
        let h = (self.h - 2 * py).max(0);
        Rect {
            x: self.x + px,
            y: self.y + py,
            w,
            h,
        }
    }

    /// The intersection with `other`, or an empty rect at the overlap
    /// origin if they do not overlap (used for the clip stack).
    pub fn intersect(&self, other: &Rect) -> Rect {
        let x0 = self.x.max(other.x);
        let y0 = self.y.max(other.y);
        let x1 = self.right().min(other.right());
        let y1 = self.bottom().min(other.bottom());
        Rect {
            x: x0,
            y: y0,
            w: (x1 - x0).max(0),
            h: (y1 - y0).max(0),
        }
    }
}

/// 0xAARRGGBB packed color (the desktop_client BGRA8888 convention is
/// applied at blit time; the toolkit works in ARGB for readability).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Color(pub u32);

impl Color {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Color {
        Color(0xFF00_0000 | ((r as u32) << 16) | ((g as u32) << 8) | b as u32)
    }

    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Color {
        Color(((a as u32) << 24) | ((r as u32) << 16) | ((g as u32) << 8) | b as u32)
    }

    pub fn r(&self) -> u8 {
        (self.0 >> 16) as u8
    }
    pub fn g(&self) -> u8 {
        (self.0 >> 8) as u8
    }
    pub fn b(&self) -> u8 {
        self.0 as u8
    }
    pub fn a(&self) -> u8 {
        (self.0 >> 24) as u8
    }

    /// Linear blend toward `other` by `t` in [0,255] (0 = self).
    pub fn lerp(&self, other: Color, t: u8) -> Color {
        let mix = |a: u8, b: u8| -> u8 {
            let a = a as u32;
            let b = b as u32;
            ((a * (255 - t as u32) + b * (t as u32)) / 255) as u8
        };
        Color::rgba(
            mix(self.r(), other.r()),
            mix(self.g(), other.g()),
            mix(self.b(), other.b()),
            mix(self.a(), other.a()),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contains_is_half_open() {
        let r = Rect::new(10, 10, 20, 20);
        assert!(r.contains(Point::new(10, 10)));
        assert!(r.contains(Point::new(29, 29)));
        assert!(!r.contains(Point::new(30, 10)), "right edge exclusive");
        assert!(!r.contains(Point::new(10, 30)), "bottom edge exclusive");
        assert!(!r.contains(Point::new(9, 10)));
    }

    #[test]
    fn inset_clamps_at_zero() {
        let r = Rect::new(0, 0, 10, 10);
        assert_eq!(r.inset(3), Rect::new(3, 3, 4, 4));
        assert_eq!(r.inset(100), Rect::new(100, 100, 0, 0));
    }

    #[test]
    fn intersect_non_overlapping_is_empty() {
        let a = Rect::new(0, 0, 10, 10);
        let b = Rect::new(20, 20, 10, 10);
        assert!(a.intersect(&b).is_empty());
        let c = Rect::new(5, 5, 10, 10);
        assert_eq!(a.intersect(&c), Rect::new(5, 5, 5, 5));
    }

    #[test]
    fn color_channels_round_trip() {
        let c = Color::rgba(0x12, 0x34, 0x56, 0x78);
        assert_eq!((c.a(), c.r(), c.g(), c.b()), (0x78, 0x12, 0x34, 0x56));
        assert_eq!(Color::rgb(1, 2, 3).a(), 0xFF);
    }

    #[test]
    fn color_lerp_endpoints() {
        let a = Color::rgb(0, 0, 0);
        let b = Color::rgb(255, 255, 255);
        assert_eq!(a.lerp(b, 0), a);
        assert_eq!(a.lerp(b, 255), b);
        assert_eq!(a.lerp(b, 128).r(), 128);
    }
}
