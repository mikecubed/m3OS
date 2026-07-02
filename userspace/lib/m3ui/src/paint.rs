//! The drawing seam (Phase 105 Track A.5).
//!
//! Widgets draw through the [`Painter`] trait, never a concrete surface.
//! The `render` feature supplies a `SurfacePainter` over `desktop_client`'s
//! `SharedSurface` + the `kernel_core::font` atlas; tests use
//! [`RecordingPainter`], which logs every draw call and reports a
//! predictable text width. This is what makes the *widget* layer — not
//! just the layout solver — host-testable: `Ui` + every widget is
//! generic over `Painter`, so their interaction + drawing behavior is
//! asserted against a mock with no framebuffer.

use alloc::string::String;
use alloc::vec::Vec;

use crate::geom::{Color, Rect};

/// The minimal 2D drawing surface the toolkit needs. Coordinates are
/// surface pixels; the concrete implementation applies clipping and the
/// BGRA byte order at blit time.
pub trait Painter {
    /// Fill `rect` with a solid color.
    fn fill_rect(&mut self, rect: Rect, color: Color);

    /// Stroke `rect`'s border `thickness` px thick (inset).
    fn stroke_rect(&mut self, rect: Rect, color: Color, thickness: i32);

    /// Draw `text` with its top-left at (`x`,`y`) in `color`. Text is
    /// clipped to the current clip region.
    fn text(&mut self, x: i32, y: i32, text: &str, color: Color);

    /// Advance width of `text` in pixels (for layout + cursor placement).
    fn text_width(&self, text: &str) -> i32;

    /// Line height of the text metrics (baseline-to-baseline).
    fn text_height(&self) -> i32;

    /// Restrict subsequent drawing to `rect` (intersected with any
    /// existing clip). Balanced by [`Painter::clip_pop`].
    fn clip_push(&mut self, rect: Rect);

    /// Undo the last [`Painter::clip_push`].
    fn clip_pop(&mut self);
}

/// A recorded draw operation, for host-test assertions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DrawOp {
    FillRect {
        rect: Rect,
        color: Color,
    },
    StrokeRect {
        rect: Rect,
        color: Color,
        thickness: i32,
    },
    Text {
        x: i32,
        y: i32,
        text: String,
        color: Color,
    },
    ClipPush(Rect),
    ClipPop,
}

/// A `Painter` that records operations instead of drawing, and reports a
/// fixed-width font (`char_w` px/char, `line_h` px tall). Used by the
/// widget host tests.
pub struct RecordingPainter {
    pub ops: Vec<DrawOp>,
    char_w: i32,
    line_h: i32,
}

impl RecordingPainter {
    /// A recorder with an 8×16 fixed-cell font (matching the fallback
    /// `draw_text` metrics), so text-width math in tests is exact.
    pub fn new() -> RecordingPainter {
        RecordingPainter {
            ops: Vec::new(),
            char_w: 8,
            line_h: 16,
        }
    }

    /// A recorder with custom fixed metrics.
    pub fn with_metrics(char_w: i32, line_h: i32) -> RecordingPainter {
        RecordingPainter {
            ops: Vec::new(),
            char_w,
            line_h,
        }
    }

    /// Count of `FillRect` ops whose color equals `color`.
    pub fn fills_with_color(&self, color: Color) -> usize {
        self.ops
            .iter()
            .filter(|op| matches!(op, DrawOp::FillRect { color: c, .. } if *c == color))
            .count()
    }

    /// The text strings drawn, in order.
    pub fn texts(&self) -> Vec<&str> {
        self.ops
            .iter()
            .filter_map(|op| match op {
                DrawOp::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    /// Whether any drawn text equals `s`.
    pub fn drew_text(&self, s: &str) -> bool {
        self.texts().iter().any(|t| *t == s)
    }
}

impl Default for RecordingPainter {
    fn default() -> RecordingPainter {
        RecordingPainter::new()
    }
}

impl Painter for RecordingPainter {
    fn fill_rect(&mut self, rect: Rect, color: Color) {
        self.ops.push(DrawOp::FillRect { rect, color });
    }
    fn stroke_rect(&mut self, rect: Rect, color: Color, thickness: i32) {
        self.ops.push(DrawOp::StrokeRect {
            rect,
            color,
            thickness,
        });
    }
    fn text(&mut self, x: i32, y: i32, text: &str, color: Color) {
        self.ops.push(DrawOp::Text {
            x,
            y,
            text: String::from(text),
            color,
        });
    }
    fn text_width(&self, text: &str) -> i32 {
        text.chars().count() as i32 * self.char_w
    }
    fn text_height(&self) -> i32 {
        self.line_h
    }
    fn clip_push(&mut self, rect: Rect) {
        self.ops.push(DrawOp::ClipPush(rect));
    }
    fn clip_pop(&mut self) {
        self.ops.push(DrawOp::ClipPop);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_ops_and_queries() {
        let mut p = RecordingPainter::new();
        p.fill_rect(Rect::new(0, 0, 10, 10), Color::rgb(1, 2, 3));
        p.text(2, 2, "hi", Color::rgb(255, 255, 255));
        assert_eq!(p.ops.len(), 2);
        assert_eq!(p.fills_with_color(Color::rgb(1, 2, 3)), 1);
        assert!(p.drew_text("hi"));
        assert_eq!(p.text_width("hi"), 16);
        assert_eq!(p.text_height(), 16);
    }
}
