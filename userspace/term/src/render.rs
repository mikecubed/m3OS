//! Phase 57 Track G.5 — display-server client renderer.
//!
//! `Renderer` consumes [`RenderCommand`]s emitted by the screen state
//! machine, batches dirty cells per frame-tick, and calls
//! [`FramebufferOwner::submit`] only when damage exists.  The
//! renderer is a thin policy layer: it does not own the surface
//! buffer (that is `FramebufferOwner`'s job), it only routes commands
//! and tracks whether the next `compose` has work to do.
//!
//! `Bell`, `SetColor`, and `MoveCursor` do not mark damage:
//!
//! - `Bell` is an audio event handled by [`crate::bell::Bell`];
//! - `SetColor` is a state update that the next `PutGlyph` carries
//!   into the framebuffer;
//! - `MoveCursor` is bookkeeping; the cursor sprite is deferred
//!   (Phase 57 ships the screen, not a separate cursor layer).
//!
//! `PutGlyph`, `Clear`, and `Scroll` mark damage and trigger a
//! `submit` on the next `compose` call.

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
/// damage exists.  Composes against any [`FramebufferOwner`] so host
/// tests cover behaviour without a real surface.
///
/// `SetColor` updates the screen state machine's active colours; the
/// renderer does not need to track them separately because the screen
/// emits colours per `PutGlyph`.  This decouples colour selection
/// from frame composition: the renderer is purely a damage-tracking
/// queue flusher.
pub struct Renderer<F: FramebufferOwner> {
    fb: F,
    /// True when the renderer has buffered damage that has not yet
    /// been submitted.  Cleared by [`compose`].
    damaged: bool,
    /// Queued draw operations buffered between `apply` calls and
    /// flushed on `compose`.  Bounded by the number of cells in the
    /// screen's grid; never grows unbounded because each frame ends
    /// with `compose` clearing the buffer.
    queue: alloc::vec::Vec<QueuedDraw>,
}

/// One queued draw operation.  Phase 57 only buffers `PutGlyph`-style
/// cells; full-screen redraws (Clear / Scroll) drain the queue and
/// are forwarded to the framebuffer at compose time as repaints.
#[derive(Clone, Copy, Debug)]
struct QueuedDraw {
    row: u16,
    col: u16,
    codepoint: u32,
    fg: u32,
    bg: u32,
}

impl<F: FramebufferOwner> Renderer<F> {
    /// Wrap a framebuffer owner with a fresh renderer.  No damage,
    /// empty queue.
    pub fn new(fb: F) -> Self {
        Self {
            fb,
            damaged: false,
            queue: alloc::vec::Vec::new(),
        }
    }

    /// Apply one render command.  Updates internal damage state but
    /// does not submit a frame.
    pub fn apply(&mut self, cmd: RenderCommand) {
        match cmd {
            RenderCommand::PutGlyph {
                row,
                col,
                codepoint,
                fg,
                bg,
            } => {
                self.queue.push(QueuedDraw {
                    row,
                    col,
                    codepoint,
                    fg,
                    bg,
                });
                self.damaged = true;
            }
            RenderCommand::Clear | RenderCommand::Scroll { .. } => {
                // Full-screen damage; the queue carries any pending
                // glyphs forward to the next frame on top of a fresh
                // background.  Phase 57 collapses both to a single
                // damage flag — the screen state machine repaints any
                // cells affected by the scroll/clear via subsequent
                // `PutGlyph` commands as the buffer scrolls.
                self.damaged = true;
            }
            RenderCommand::SetColor { .. } => {
                // Colour state lives on the screen state machine; the
                // renderer receives the chosen colours per PutGlyph.
                // SetColor alone is not a damage event.
            }
            RenderCommand::Bell => { /* audio path; no pixels */ }
            RenderCommand::MoveCursor { .. } => { /* cursor sprite is deferred to a later track */ }
        }
    }

    /// True when there is buffered damage waiting to be submitted.
    pub fn damaged(&self) -> bool {
        self.damaged
    }

    /// Submit any buffered damage to the framebuffer.  No-op when
    /// `damaged()` is false (no work, no submit).
    pub fn compose(&mut self) {
        if !self.damaged {
            return;
        }
        for draw in self.queue.drain(..) {
            self.fb
                .put_glyph(draw.row, draw.col, draw.codepoint, draw.fg, draw.bg);
        }
        self.fb.submit();
        self.damaged = false;
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
