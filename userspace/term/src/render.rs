//! Phase 57 Track G.5 — display-server client renderer.
//!
//! Red commit: `Renderer` declares the public seam — `apply` (one
//! command), `compose` (frame-tick), and `damaged` (true when
//! `compose` has work to do) — but the bodies are `unimplemented!()`
//! so the host tests panic.  The green commit lands the real
//! batching logic.

use crate::screen::RenderCommand;

/// Pluggable framebuffer-owner seam.  Production wraps the Phase 56
/// `surface_buffer` crate; host tests record draw calls.
pub trait FramebufferOwner {
    /// Paint a glyph cell at `(row, col)` with the given colours.
    fn put_glyph(&mut self, row: u16, col: u16, codepoint: u32, fg: u32, bg: u32);

    /// Submit the current frame to the display server.
    fn submit(&mut self);
}

/// Renderer: batches dirty cells per frame, calls `submit` only when
/// damage exists.  Composes against any `FramebufferOwner` so host
/// tests cover behaviour without a real surface.
pub struct Renderer<F: FramebufferOwner> {
    #[allow(dead_code)]
    fb: F,
}

impl<F: FramebufferOwner> Renderer<F> {
    pub fn new(_fb: F) -> Self {
        unimplemented!("G.5 green commit lands this")
    }

    /// Apply one render command.  Updates internal damage state but
    /// does not submit a frame.
    pub fn apply(&mut self, _cmd: RenderCommand) {
        unimplemented!("G.5 green commit lands this")
    }

    /// True when there is buffered damage waiting to be submitted.
    pub fn damaged(&self) -> bool {
        unimplemented!("G.5 green commit lands this")
    }

    /// Submit any buffered damage to the framebuffer.  Idempotent
    /// when `damaged()` is false (no work, no submit).
    pub fn compose(&mut self) {
        unimplemented!("G.5 green commit lands this")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    #[derive(Clone, Debug, PartialEq, Eq)]
    enum FakeOp {
        Put { row: u16, col: u16, codepoint: u32 },
        Submit,
    }

    struct FakeFb {
        ops: Vec<FakeOp>,
    }

    impl FakeFb {
        fn new() -> Self {
            Self { ops: Vec::new() }
        }
    }

    impl FramebufferOwner for FakeFb {
        fn put_glyph(&mut self, row: u16, col: u16, codepoint: u32, _fg: u32, _bg: u32) {
            self.ops.push(FakeOp::Put {
                row,
                col,
                codepoint,
            });
        }

        fn submit(&mut self) {
            self.ops.push(FakeOp::Submit);
        }
    }

    #[test]
    fn put_glyph_marks_damage() {
        let mut r = Renderer::new(FakeFb::new());
        assert!(!r.damaged(), "fresh renderer has no damage");
        r.apply(RenderCommand::PutGlyph {
            row: 0,
            col: 0,
            codepoint: b'A' as u32,
            fg: 0,
            bg: 0,
        });
        assert!(r.damaged(), "PutGlyph must mark damage");
    }

    #[test]
    fn compose_submits_only_when_damaged() {
        let mut r = Renderer::new(FakeFb::new());
        // No damage → no submit.
        r.compose();
        // After damage, exactly one submit.
        r.apply(RenderCommand::PutGlyph {
            row: 0,
            col: 0,
            codepoint: b'A' as u32,
            fg: 0,
            bg: 0,
        });
        r.compose();
        assert!(!r.damaged(), "compose must clear damage");
    }

    #[test]
    fn compose_calls_submit_with_glyph() {
        let mut r = Renderer::new(FakeFb::new());
        r.apply(RenderCommand::PutGlyph {
            row: 1,
            col: 2,
            codepoint: b'X' as u32,
            fg: 0xFFFF_FFFF,
            bg: 0,
        });
        r.compose();
        // Exactly one Put + one Submit; no extras.
        assert_eq!(r.fb.ops.len(), 2);
        assert!(
            matches!(r.fb.ops[0], FakeOp::Put { row: 1, col: 2, codepoint } if codepoint == b'X' as u32)
        );
        assert_eq!(r.fb.ops[1], FakeOp::Submit);
    }

    #[test]
    fn bell_does_not_mark_damage() {
        let mut r = Renderer::new(FakeFb::new());
        r.apply(RenderCommand::Bell);
        assert!(!r.damaged(), "Bell is audio, not pixels");
    }

    #[test]
    fn clear_marks_damage() {
        let mut r = Renderer::new(FakeFb::new());
        r.apply(RenderCommand::Clear);
        assert!(r.damaged());
    }

    #[test]
    fn scroll_marks_damage() {
        let mut r = Renderer::new(FakeFb::new());
        r.apply(RenderCommand::Scroll { amount: 1 });
        assert!(r.damaged());
    }

    #[test]
    fn set_color_does_not_mark_damage() {
        // SetColor is a state update; the next PutGlyph carries the
        // colour into the framebuffer, so SetColor alone does not
        // need a compose.
        let mut r = Renderer::new(FakeFb::new());
        r.apply(RenderCommand::SetColor {
            fg: 0,
            bg: 0xFFFF_FFFF,
        });
        assert!(!r.damaged());
    }

    #[test]
    fn move_cursor_does_not_mark_damage() {
        // MoveCursor is internal state; the renderer doesn't paint a
        // cursor sprite in Phase 57 (deferred to a later track).
        let mut r = Renderer::new(FakeFb::new());
        r.apply(RenderCommand::MoveCursor { row: 1, col: 2 });
        assert!(!r.damaged());
    }
}
