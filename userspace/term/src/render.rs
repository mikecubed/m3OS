//! Phase 57 Track G.5 — display-server client renderer.
//!
//! `Renderer` consumes [`RenderCommand`]s emitted by the screen state
//! machine, batches damage per frame-tick, and drives the
//! [`FramebufferOwner`] only when damage exists. The renderer is a
//! thin policy layer: it does not own the surface buffer (that is
//! `FramebufferOwner`'s job), it only routes commands and tracks
//! buffered work.
//!
//! Damage is recorded as an ordered queue of framebuffer operations:
//! `PutGlyph`, `Clear`, and `Scroll`. The queue replays in the same
//! order on `compose` so a `Clear` followed by `PutGlyph` paints the
//! new cells on a cleared frame, and a `Scroll` shifts existing
//! pixels before any post-scroll `PutGlyph` lands. This keeps the
//! framebuffer in sync with the screen state machine after full-screen
//! operations.
//!
//! `Bell`, `SetColor`, and `MoveCursor` do not enqueue any operation:
//!
//! - `Bell` is an audio event handled by [`crate::bell::Bell`];
//! - `SetColor` is a state update that the next `PutGlyph` carries
//!   into the framebuffer;
//! - `MoveCursor` is bookkeeping; the cursor sprite is deferred
//!   (Phase 57 ships the screen, not a separate cursor layer).

use crate::screen::RenderCommand;

/// Pluggable framebuffer-owner seam. Production wraps the Phase 56
/// `surface_buffer` crate; host tests record draw calls.
pub trait FramebufferOwner {
    /// Paint a glyph cell at `(row, col)` with the given colours.
    fn put_glyph(&mut self, row: u16, col: u16, codepoint: u32, fg: u32, bg: u32);

    /// Clear the entire surface to the current background colour.
    /// Called when the screen state machine emits
    /// `RenderCommand::Clear` (e.g., from `ESC [ 2 J`). The
    /// framebuffer must drop any prior contents.
    fn clear(&mut self);

    /// Shift content vertically by `amount` rows. `amount > 0`
    /// scrolls UP (top rows lost, bottom rows blanked); `amount < 0`
    /// would scroll DOWN. Phase 57's screen state machine only emits
    /// `amount = 1` from `scroll_up`.
    fn scroll(&mut self, amount: i16);

    /// Submit the current frame to the display server.
    fn submit(&mut self);
}

/// One queued framebuffer op buffered between `apply` calls and
/// flushed in order on `compose`. Bounded by the screen's command
/// throughput per tick; never grows unbounded because each frame ends
/// with `compose` draining the queue.
#[derive(Clone, Copy, Debug)]
enum QueuedOp {
    Put {
        row: u16,
        col: u16,
        codepoint: u32,
        fg: u32,
        bg: u32,
    },
    Clear,
    Scroll {
        amount: i16,
    },
}

/// Renderer: batches framebuffer ops per frame, calls `submit` only
/// when damage exists. Composes against any [`FramebufferOwner`] so
/// host tests cover behaviour without a real surface.
///
/// `SetColor` updates the screen state machine's active colours; the
/// renderer does not need to track them separately because the screen
/// emits colours per `PutGlyph`. This decouples colour selection
/// from frame composition: the renderer is purely an ordered queue
/// flusher.
pub struct Renderer<F: FramebufferOwner> {
    fb: F,
    queue: alloc::vec::Vec<QueuedOp>,
}

impl<F: FramebufferOwner> Renderer<F> {
    /// Wrap a framebuffer owner with a fresh renderer. Empty queue.
    pub fn new(fb: F) -> Self {
        Self {
            fb,
            queue: alloc::vec::Vec::new(),
        }
    }

    /// Apply one render command. Updates the queued op stream but
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
                self.queue.push(QueuedOp::Put {
                    row,
                    col,
                    codepoint,
                    fg,
                    bg,
                });
            }
            RenderCommand::Clear => {
                self.queue.push(QueuedOp::Clear);
            }
            RenderCommand::Scroll { amount } => {
                self.queue.push(QueuedOp::Scroll { amount });
            }
            RenderCommand::SetColor { .. } => {
                // Colour state lives on the screen state machine; the
                // renderer receives the chosen colours per PutGlyph.
            }
            RenderCommand::Bell => { /* audio path; no pixels */ }
            RenderCommand::MoveCursor { .. } => { /* cursor sprite is deferred to a later track */ }
        }
    }

    /// True when there is buffered damage waiting to be submitted.
    pub fn damaged(&self) -> bool {
        !self.queue.is_empty()
    }

    /// Submit any buffered damage to the framebuffer. No-op when
    /// `damaged()` is false (no work, no submit).
    pub fn compose(&mut self) {
        if self.queue.is_empty() {
            return;
        }
        for op in self.queue.drain(..) {
            match op {
                QueuedOp::Put {
                    row,
                    col,
                    codepoint,
                    fg,
                    bg,
                } => {
                    self.fb.put_glyph(row, col, codepoint, fg, bg);
                }
                QueuedOp::Clear => self.fb.clear(),
                QueuedOp::Scroll { amount } => self.fb.scroll(amount),
            }
        }
        self.fb.submit();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    #[derive(Clone, Debug, PartialEq, Eq)]
    enum FakeOp {
        Put { row: u16, col: u16, codepoint: u32 },
        Clear,
        Scroll { amount: i16 },
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

        fn clear(&mut self) {
            self.ops.push(FakeOp::Clear);
        }

        fn scroll(&mut self, amount: i16) {
            self.ops.push(FakeOp::Scroll { amount });
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
        assert!(r.fb.ops.is_empty());
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

    /// Compose drives a real `clear` op on the framebuffer for a
    /// `RenderCommand::Clear`, followed by submit. Without this the
    /// framebuffer would silently keep stale pixels.
    #[test]
    fn compose_emits_fb_clear_for_render_clear() {
        let mut r = Renderer::new(FakeFb::new());
        r.apply(RenderCommand::Clear);
        r.compose();
        assert_eq!(r.fb.ops, alloc::vec![FakeOp::Clear, FakeOp::Submit]);
    }

    #[test]
    fn scroll_marks_damage() {
        let mut r = Renderer::new(FakeFb::new());
        r.apply(RenderCommand::Scroll { amount: 1 });
        assert!(r.damaged());
    }

    /// Compose drives a real `scroll` op on the framebuffer for a
    /// `RenderCommand::Scroll`, followed by submit. Without this the
    /// framebuffer would silently keep the unscrolled content.
    #[test]
    fn compose_emits_fb_scroll_for_render_scroll() {
        let mut r = Renderer::new(FakeFb::new());
        r.apply(RenderCommand::Scroll { amount: 1 });
        r.compose();
        assert_eq!(
            r.fb.ops,
            alloc::vec![FakeOp::Scroll { amount: 1 }, FakeOp::Submit]
        );
    }

    /// Order is preserved across mixed commands: PutGlyph before Clear
    /// is overwritten by Clear; PutGlyph after Clear paints the
    /// already-cleared frame.
    #[test]
    fn compose_preserves_order_around_clear_and_scroll() {
        let mut r = Renderer::new(FakeFb::new());
        r.apply(RenderCommand::PutGlyph {
            row: 0,
            col: 0,
            codepoint: b'A' as u32,
            fg: 0,
            bg: 0,
        });
        r.apply(RenderCommand::Clear);
        r.apply(RenderCommand::PutGlyph {
            row: 0,
            col: 0,
            codepoint: b'B' as u32,
            fg: 0,
            bg: 0,
        });
        r.apply(RenderCommand::Scroll { amount: 1 });
        r.apply(RenderCommand::PutGlyph {
            row: 24,
            col: 0,
            codepoint: b'C' as u32,
            fg: 0,
            bg: 0,
        });
        r.compose();
        assert_eq!(
            r.fb.ops,
            alloc::vec![
                FakeOp::Put {
                    row: 0,
                    col: 0,
                    codepoint: b'A' as u32
                },
                FakeOp::Clear,
                FakeOp::Put {
                    row: 0,
                    col: 0,
                    codepoint: b'B' as u32
                },
                FakeOp::Scroll { amount: 1 },
                FakeOp::Put {
                    row: 24,
                    col: 0,
                    codepoint: b'C' as u32
                },
                FakeOp::Submit,
            ]
        );
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
