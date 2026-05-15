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
//! - `MoveCursor` is bookkeeping; the cursor sprite is deferred.
//!   Phase 69 added DECSCUSR shape state on [`crate::screen::Screen`]
//!   and a 500 ms blink-tick that `mark_damaged`s the frame, but the
//!   actual cursor *pixels* (block / underline / bar fill) still need
//!   to be painted by the framebuffer owner. A follow-up phase will
//!   wire `Screen::cursor_shape()` + `cursor_visible` + the blink
//!   phase into a real cursor draw; until then, `cursor_shape` and
//!   blink are observable via host tests and the `tui-smoke cursor`
//!   subcommand asserts state changes — they are not yet user-visible
//!   on the framebuffer.

use crate::screen::RenderCommand;
use kernel_core::font::{Atlas, GlyphView};

/// Phase 69c Track E.2 — runtime glyph-resolution policy.
///
/// `Static` keeps Phase 69b's `kernel_core::session::resolve_glyph`
/// path; `Atlas` swaps in the TTF-backed atlas. `term` boots with
/// `Static` and upgrades to `Atlas` only when the font file at
/// `/usr/share/fonts/m3os/term.ttf` opens cleanly. Either way the
/// renderer's compose loop is identical — the `GlyphSource` is
/// internal state, never observable to callers.
pub enum GlyphSource {
    /// Phase 69b static-table path — ASCII / Latin-1 / box-drawing
    /// + centred-dot fallback.
    Static,
    /// Phase 69c TTF-atlas path. Owns the atlas so the renderer's
    /// `glyph_pixels` borrow is local.
    Atlas(Atlas),
}

/// Pluggable framebuffer-owner seam. Production wraps the Phase 56
/// `surface_buffer` crate; host tests record draw calls.
pub trait FramebufferOwner {
    /// Paint a glyph cell at `(row, col)` with the given colours.
    /// `glyph` is the pre-resolved 1-bit bitmap (`GlyphView`), so
    /// the framebuffer owner does not need to know whether the
    /// pixels came from the static tables or the TTF atlas —
    /// resolution happens in [`Renderer::compose`].
    fn put_glyph(
        &mut self,
        row: u16,
        col: u16,
        codepoint: u32,
        glyph: &GlyphView<'_>,
        fg: u32,
        bg: u32,
    );

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
    fn submit(&mut self) -> bool;
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
    pending_submit: bool,
    /// Phase 69c Track E.2 — the active glyph-resolution policy.
    /// Defaults to `Static` so callers that don't carry a font file
    /// (host tests, early-boot) get Phase 69b's behaviour.
    glyph_source: GlyphSource,
}

impl<F: FramebufferOwner> Renderer<F> {
    /// Wrap a framebuffer owner with a fresh renderer. Empty queue.
    /// Uses the static Phase 69b glyph tables.
    pub fn new(fb: F) -> Self {
        Self {
            fb,
            queue: alloc::vec::Vec::new(),
            pending_submit: false,
            glyph_source: GlyphSource::Static,
        }
    }

    /// Phase 69c Track E.1/E.2 — install a TTF atlas as the runtime
    /// glyph source. `term::main` calls this when the font opens
    /// cleanly; on failure the renderer keeps using `GlyphSource::Static`.
    pub fn set_atlas(&mut self, atlas: Atlas) {
        self.glyph_source = GlyphSource::Atlas(atlas);
    }

    /// True when this renderer is using the atlas path. Test helper.
    pub fn has_atlas(&self) -> bool {
        matches!(self.glyph_source, GlyphSource::Atlas(_))
    }

    /// Phase 69c Track E.2 — resolve `codepoint` through the active
    /// `GlyphSource`. Returns a borrowed view backed by either the
    /// static-table data (Phase 69b) or the atlas's owned bitmap.
    /// Never allocates per call: the atlas hits its cache for the
    /// hot path and the static tables are `'static`.
    pub fn glyph_pixels(&mut self, codepoint: u32) -> GlyphView<'_> {
        match &mut self.glyph_source {
            GlyphSource::Atlas(atlas) => atlas.resolve(codepoint).as_view(),
            GlyphSource::Static => kernel_core::session::resolve_glyph(codepoint).as_view(),
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
            RenderCommand::MoveCursor { .. } => {
                // Cursor pixel rendering is deferred — Phase 69 ships
                // DECSCUSR shape state + a blink-tick that drives
                // `mark_damaged()`, but the actual block / underline /
                // bar pixels need a future framebuffer-owner update.
                // See the module-level doc comment for the full
                // deferral notes; `tui-smoke cursor` covers the state
                // transitions today.
            }
            // Phase 69 Track E — mouse-mode changes are routed to
            // `MouseReporter` by the main loop; the renderer is a
            // pure pixel sink and never sees this command.
            RenderCommand::SetMouseMode { .. } => {}
        }
    }

    /// Phase 69 Track F.2 — force the next [`Renderer::compose`] call
    /// to push a frame even if no `PutGlyph` / `Clear` / `Scroll`
    /// arrived this tick. Used by the blink-tick path in
    /// `main.rs` so a blinking cursor still drives display updates
    /// while the PTY is idle.
    pub fn mark_damaged(&mut self) {
        self.pending_submit = true;
    }

    /// True when there is buffered damage waiting to be submitted.
    pub fn damaged(&self) -> bool {
        !self.queue.is_empty() || self.pending_submit
    }

    /// Submit any buffered damage to the framebuffer. No-op when
    /// `damaged()` is false (no work, no submit).
    pub fn compose(&mut self) {
        if self.queue.is_empty() && !self.pending_submit {
            return;
        }
        if !self.queue.is_empty() {
            // Move the queue out so the field-level borrows below
            // do not conflict with iteration. `mem::take` leaves
            // `self.queue` as a freshly-empty `Vec` (no allocation);
            // after iterating we `clear()` the local and write it
            // back so the capacity survives — the hot render path
            // must not re-allocate every frame.
            let mut queue = core::mem::take(&mut self.queue);
            // Split-borrow the fields so `glyph_source` and `fb`
            // can be borrowed concurrently inside the loop.
            let Self {
                fb, glyph_source, ..
            } = self;
            for op in queue.drain(..) {
                match op {
                    QueuedOp::Put {
                        row,
                        col,
                        codepoint,
                        fg,
                        bg,
                    } => {
                        let view = match glyph_source {
                            GlyphSource::Atlas(atlas) => atlas.resolve(codepoint).as_view(),
                            GlyphSource::Static => {
                                kernel_core::session::resolve_glyph(codepoint).as_view()
                            }
                        };
                        fb.put_glyph(row, col, codepoint, &view, fg, bg);
                    }
                    QueuedOp::Clear => fb.clear(),
                    QueuedOp::Scroll { amount } => fb.scroll(amount),
                }
            }
            // `drain(..)` left `queue` empty but with its allocation
            // intact. Restore it so the next frame's `submit_op`
            // pushes into the same buffer instead of re-allocating.
            self.queue = queue;
            self.pending_submit = true;
        }
        if self.pending_submit && self.fb.submit() {
            self.pending_submit = false;
        }
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
        submit_results: Vec<bool>,
    }

    impl FakeFb {
        fn new() -> Self {
            Self {
                ops: Vec::new(),
                submit_results: Vec::new(),
            }
        }

        fn with_submit_results(results: Vec<bool>) -> Self {
            Self {
                ops: Vec::new(),
                submit_results: results,
            }
        }
    }

    impl FramebufferOwner for FakeFb {
        fn put_glyph(
            &mut self,
            row: u16,
            col: u16,
            codepoint: u32,
            _glyph: &GlyphView<'_>,
            _fg: u32,
            _bg: u32,
        ) {
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

        fn submit(&mut self) -> bool {
            self.ops.push(FakeOp::Submit);
            if self.submit_results.is_empty() {
                return true;
            }
            self.submit_results.remove(0)
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
    fn failed_submit_keeps_frame_pending_without_replaying_ops() {
        let mut r = Renderer::new(FakeFb::with_submit_results(Vec::from([false, true])));
        r.apply(RenderCommand::PutGlyph {
            row: 1,
            col: 2,
            codepoint: b'X' as u32,
            fg: 0xFFFF_FFFF,
            bg: 0,
        });

        r.compose();
        assert!(r.damaged(), "failed submit must leave a retry pending");

        r.compose();
        assert!(!r.damaged(), "successful retry clears pending submit");
        assert_eq!(
            r.fb.ops,
            Vec::from([
                FakeOp::Put {
                    row: 1,
                    col: 2,
                    codepoint: b'X' as u32
                },
                FakeOp::Submit,
                FakeOp::Submit,
            ]),
            "retry should resubmit the already-rendered frame, not replay drawing ops"
        );
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

    /// `compose()` must not throw away the queue's allocation each
    /// frame — the hot render path runs once per tick, and a Vec
    /// that grows and re-allocates on every frame would dominate
    /// the renderer's allocator traffic.
    #[test]
    fn compose_preserves_queue_capacity_across_frames() {
        let mut r = Renderer::new(FakeFb::new());
        for col in 0..32u16 {
            r.apply(RenderCommand::PutGlyph {
                row: 0,
                col,
                codepoint: b'A' as u32 + col as u32,
                fg: 0,
                bg: 0,
            });
        }
        let cap_after_grow = r.queue.capacity();
        assert!(cap_after_grow >= 32);
        r.compose();
        assert_eq!(r.queue.len(), 0, "compose must drain the queue");
        assert_eq!(
            r.queue.capacity(),
            cap_after_grow,
            "compose must retain the queue's capacity for reuse",
        );
    }
}
