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

/// 2026-05-18 less-render flake probe — bracket each `fb.scroll(amount)`
/// call so a probe run can see exactly which Scroll instance touched
/// the buffer. Host tests do not exercise the `#[cfg(not(test))]` path.
#[cfg(not(test))]
fn scroll_trace_pre(amount: i16) {
    syscall_lib::write_str(syscall_lib::STDOUT_FILENO, "TC:scroll-pre amount=");
    write_signed_i16(amount);
    syscall_lib::write_str(syscall_lib::STDOUT_FILENO, "\n");
}

#[cfg(not(test))]
fn scroll_trace_post(amount: i16) {
    syscall_lib::write_str(syscall_lib::STDOUT_FILENO, "TC:scroll-post amount=");
    write_signed_i16(amount);
    syscall_lib::write_str(syscall_lib::STDOUT_FILENO, "\n");
}

#[cfg(test)]
fn scroll_trace_pre(_amount: i16) {}
#[cfg(test)]
fn scroll_trace_post(_amount: i16) {}

#[cfg(not(test))]
fn put_extent_trace(min_row: u16, max_row: u16, min_col: u16, max_col: u16, puts: u16) {
    if puts == 0 {
        return;
    }
    syscall_lib::write_str(syscall_lib::STDOUT_FILENO, "TC:put-extent rows=");
    write_decimal_u32(min_row as u32);
    let _ = syscall_lib::write(syscall_lib::STDOUT_FILENO, b"..");
    write_decimal_u32(max_row as u32);
    syscall_lib::write_str(syscall_lib::STDOUT_FILENO, " cols=");
    write_decimal_u32(min_col as u32);
    let _ = syscall_lib::write(syscall_lib::STDOUT_FILENO, b"..");
    write_decimal_u32(max_col as u32);
    syscall_lib::write_str(syscall_lib::STDOUT_FILENO, " puts=");
    write_decimal_u32(puts as u32);
    syscall_lib::write_str(syscall_lib::STDOUT_FILENO, "\n");
}

#[cfg(test)]
fn put_extent_trace(_min_row: u16, _max_row: u16, _min_col: u16, _max_col: u16, _puts: u16) {}

#[cfg(not(test))]
fn write_signed_i16(value: i16) {
    if value < 0 {
        let _ = syscall_lib::write(syscall_lib::STDOUT_FILENO, b"-");
        let mag = (-(value as i32)) as u32;
        write_decimal_u32(mag);
    } else {
        write_decimal_u32(value as u32);
    }
}

#[cfg(not(test))]
fn write_decimal_u32(value: u32) {
    let mut buf = [0u8; 10];
    let mut idx = buf.len();
    let mut n = value;
    loop {
        idx -= 1;
        buf[idx] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    let _ = syscall_lib::write(syscall_lib::STDOUT_FILENO, &buf[idx..]);
}

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

/// Maximum number of consecutive composes the render path will
/// defer a Clear-only queue waiting for follow-up Put ops. Each
/// compose tick is roughly 16 ms, so 8 frames ≈ 128 ms — long
/// enough to absorb a worst-case TCG-slow PTY chunk delay between
/// less's `\E[2J\E[H` header and its content, short enough that a
/// shell `clear` command (no content forthcoming) clears the screen
/// within a single user-perceivable frame.
const MAX_CLEAR_ONLY_DEFER: u8 = 8;

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
    /// 2026-05-17 less-render-disappearance fix — number of
    /// consecutive composes that have deferred a queue whose only
    /// pixel-changing op was a `Clear` (no `Put` followed it). Held
    /// across composes so the next call can drain both the deferred
    /// Clear and any newly-arrived Put ops together, avoiding a
    /// single-frame all-black publish that the double-buffer path
    /// otherwise propagates to both buffers via
    /// `refresh_back_from_front` and traps the screen in an all-
    /// zero state until the next paint cycle.
    ///
    /// Capped at [`MAX_CLEAR_ONLY_DEFER`] composes (~`MAX_*` * 16 ms
    /// ≈ wall-clock grace period) so a legit `clear` shell command
    /// — where the queue is genuinely Clear-only forever — eventually
    /// publishes the cleared screen rather than blocking indefinitely.
    /// Reset to 0 whenever a compose actually drains.
    clear_only_defers: u8,
    /// 2026-05-18 less-render flake — per-compose summary captured
    /// while [`Renderer::compose`] runs. Read via [`Renderer::last_outcome`]
    /// after every compose so the binary main loop can log structured
    /// trace lines without owning the renderer's internals. Reset at
    /// the top of every [`Renderer::compose`] call.
    last_outcome: ComposeOutcome,
}

/// 2026-05-18 less-render flake — structured summary of a single
/// [`Renderer::compose`] call. The main loop reads this after every
/// compose to emit a serial trace line; reading the same fields back
/// in a host test verifies the renderer follows the expected flow
/// across defer / drain / submit transitions.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ComposeOutcome {
    /// Count of `Clear` ops drained this compose. `0` for an
    /// incremental-only compose; `1+` indicates the framebuffer was
    /// reset before the puts.
    pub clears: u16,
    /// Count of `Put` ops drained this compose.
    pub puts: u16,
    /// Count of `Scroll` ops drained this compose.
    pub scrolls: u16,
    /// True if this compose deferred a Clear-only queue waiting for
    /// follow-up Put ops to arrive. When set, `clears`/`puts`/`scrolls`
    /// are zero — the queue was not drained.
    pub deferred: bool,
    /// True if this compose force-drained a deferred Clear-only queue
    /// after [`MAX_CLEAR_ONLY_DEFER`] cycles. When set, `clears >= 1`
    /// and `puts == 0` — the buffer was cleared without follow-up
    /// content. This is the documented loophole where a legitimate
    /// shell `clear` command produces a single-frame zero publish.
    pub force_drained: bool,
    /// True if the compose actually called `fb.submit()` (regardless
    /// of whether submit returned true or false). False indicates a
    /// no-op compose (empty queue, no `pending_submit`, no submit).
    pub submitted: bool,
    /// Return value of `fb.submit()`; meaningful only when
    /// `submitted` is true.
    pub submit_ok: bool,
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
            clear_only_defers: 0,
            last_outcome: ComposeOutcome::default(),
        }
    }

    /// 2026-05-18 less-render flake — return the outcome of the most
    /// recent [`Renderer::compose`] call. The renderer resets
    /// `last_outcome` at the top of every `compose`, so reading this
    /// immediately after `compose` returns yields fresh data; reading
    /// it before any `compose` runs returns `ComposeOutcome::default()`.
    pub fn last_outcome(&self) -> ComposeOutcome {
        self.last_outcome
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
    ///
    /// Allocation: the `Static` path is allocation-free (the tables
    /// are `'static`). The `Atlas` path is allocation-free on a
    /// cache hit; on a miss it rasterizes the glyph (allocates a
    /// `Vec<OutlineSegment>` plus a `RasterBitmap`) and inserts the
    /// new slot into the cache. The hot path is hit-dominated once
    /// the warm-up range has been pre-resolved, but a stream of
    /// novel codepoints will allocate per miss until the cache fills
    /// its capacity.
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
            // 2026-05-18 less-render follow-up — host-bound reply
            // bytes (DA / DSR) are intercepted by `main.rs` and
            // written to the PTY primary. The renderer is a pure
            // pixel sink and never sees this command in practice;
            // a no-op match arm keeps the type system honest in
            // case a future callsite hands the renderer the full
            // command stream unfiltered.
            RenderCommand::RespondToHost { .. } => {}
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
        self.last_outcome = ComposeOutcome::default();
        if self.queue.is_empty() && !self.pending_submit {
            return;
        }
        if !self.queue.is_empty() {
            // 2026-05-17 less-render-disappearance fix: defer the
            // drain by one compose if the queue has a `Clear` but
            // no `Put` after it. Without this, an arbitrary PTY
            // chunk boundary right after a `\E[2J\E[H` sequence
            // (and before the `<content>` portion) would produce
            // a back buffer of all-zeros, which the double-buffered
            // `refresh_back_from_front` then propagates to *both*
            // SHM buffers — trapping the screen in an all-black
            // state until the next paint cycle. The single-compose
            // defer gives the next PTY chunk a 16 ms window to
            // arrive; if a legitimate `clear` shell command never
            // produces content, the second compose drains anyway
            // so the user still sees a cleared screen.
            let last_clear = self
                .queue
                .iter()
                .rposition(|op| matches!(op, QueuedOp::Clear));
            let has_put_after_clear = match last_clear {
                Some(idx) => self.queue[idx + 1..]
                    .iter()
                    .any(|op| matches!(op, QueuedOp::Put { .. })),
                None => true,
            };
            if !has_put_after_clear && self.clear_only_defers < MAX_CLEAR_ONLY_DEFER {
                self.clear_only_defers += 1;
                self.last_outcome.deferred = true;
                return;
            }
            // If a Clear was queued without any follow-up Put across
            // `MAX_CLEAR_ONLY_DEFER` cycles, this drain is a
            // force-drain: the back buffer gets cleared and published
            // as a single-frame zero publish. Track it so the trace
            // can distinguish "legitimate `clear` command" from "PTY
            // chunk boundary stranded a less repaint header".
            if !has_put_after_clear {
                self.last_outcome.force_drained = true;
            }
            self.clear_only_defers = 0;
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
            // 2026-05-18 less-render flake probe — accumulate per-op
            // row/col extents to detect out-of-grid puts that the
            // FramebufferOwner would drop silently. Logged once at
            // end of drain.
            let mut min_row = u16::MAX;
            let mut max_row = 0u16;
            let mut min_col = u16::MAX;
            let mut max_col = 0u16;
            for op in queue.drain(..) {
                match op {
                    QueuedOp::Put {
                        row,
                        col,
                        codepoint,
                        fg,
                        bg,
                    } => {
                        if row < min_row {
                            min_row = row;
                        }
                        if row > max_row {
                            max_row = row;
                        }
                        if col < min_col {
                            min_col = col;
                        }
                        if col > max_col {
                            max_col = col;
                        }
                        let view = match glyph_source {
                            GlyphSource::Atlas(atlas) => atlas.resolve(codepoint).as_view(),
                            GlyphSource::Static => {
                                kernel_core::session::resolve_glyph(codepoint).as_view()
                            }
                        };
                        fb.put_glyph(row, col, codepoint, &view, fg, bg);
                        self.last_outcome.puts = self.last_outcome.puts.saturating_add(1);
                    }
                    QueuedOp::Clear => {
                        fb.clear();
                        self.last_outcome.clears = self.last_outcome.clears.saturating_add(1);
                    }
                    QueuedOp::Scroll { amount } => {
                        // 2026-05-18 less-render flake probe — bracket the
                        // scroll with serial markers so a probe run can
                        // see exactly which Scroll(amount) call zeroed
                        // the back buffer. The amount value matters: if
                        // it ever lands at -32768 (or wraps to a value
                        // whose row_bytes product >= buf_len), the
                        // display.rs scroll path falls through to
                        // `pixels.fill(0)` and the bug shape matches
                        // exactly what the trace shows.
                        scroll_trace_pre(amount);
                        fb.scroll(amount);
                        scroll_trace_post(amount);
                        self.last_outcome.scrolls = self.last_outcome.scrolls.saturating_add(1);
                    }
                }
            }
            put_extent_trace(min_row, max_row, min_col, max_col, self.last_outcome.puts);
            // `drain(..)` left `queue` empty but with its allocation
            // intact. Restore it so the next frame's `submit_op`
            // pushes into the same buffer instead of re-allocating.
            self.queue = queue;
            self.pending_submit = true;
        }
        if self.pending_submit {
            self.last_outcome.submitted = true;
            let ok = self.fb.submit();
            self.last_outcome.submit_ok = ok;
            if ok {
                self.pending_submit = false;
            }
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
        // 2026-05-17 less-render-disappearance fix: a Clear-only
        // queue (no Put after the Clear) is deferred for up to
        // [`MAX_CLEAR_ONLY_DEFER`] consecutive composes, on the
        // chance that the matching `<content>` portion of a
        // `\E[2J\E[H<content>` repaint is still in transit through
        // PTY chunking. After the cap the renderer drains anyway so
        // a legit shell `clear` command (no content forthcoming)
        // isn't permanently deferred.
        let mut r = Renderer::new(FakeFb::new());
        r.apply(RenderCommand::Clear);
        for i in 0..MAX_CLEAR_ONLY_DEFER {
            r.compose();
            assert!(
                r.fb.ops.is_empty(),
                "compose #{i} with Clear-only queue should defer, got: {:?}",
                r.fb.ops
            );
        }
        r.compose();
        assert_eq!(r.fb.ops, alloc::vec![FakeOp::Clear, FakeOp::Submit]);
    }

    /// Clear followed by Put drains on the *first* compose — the
    /// defer-once gate looks at the queue tail and lets a fully-
    /// queued repaint (Clear + at least one Put) through without
    /// delay.
    #[test]
    fn compose_drains_clear_followed_by_put_in_one_pass() {
        let mut r = Renderer::new(FakeFb::new());
        r.apply(RenderCommand::Clear);
        r.apply(RenderCommand::PutGlyph {
            row: 0,
            col: 0,
            codepoint: b'X' as u32,
            fg: 0,
            bg: 0,
        });
        r.compose();
        assert_eq!(
            r.fb.ops,
            alloc::vec![
                FakeOp::Clear,
                FakeOp::Put {
                    row: 0,
                    col: 0,
                    codepoint: b'X' as u32
                },
                FakeOp::Submit,
            ]
        );
    }

    /// Clear-only first compose defers, then a Put queued before the
    /// second compose draws normally — the deferred Clear is replayed
    /// alongside the Put.
    #[test]
    fn defer_then_put_drains_clear_and_put_together() {
        let mut r = Renderer::new(FakeFb::new());
        r.apply(RenderCommand::Clear);
        r.compose();
        assert!(r.fb.ops.is_empty(), "first compose deferred");
        r.apply(RenderCommand::PutGlyph {
            row: 1,
            col: 2,
            codepoint: b'A' as u32,
            fg: 0,
            bg: 0,
        });
        r.compose();
        assert_eq!(
            r.fb.ops,
            alloc::vec![
                FakeOp::Clear,
                FakeOp::Put {
                    row: 1,
                    col: 2,
                    codepoint: b'A' as u32
                },
                FakeOp::Submit,
            ]
        );
    }

    /// Once the deferred Clear has drained (with or without a Put),
    /// the defer counter resets so the next standalone Clear gets
    /// the full grace period again. Without the reset, an
    /// interactive session that does many `clear` commands would
    /// see the second `clear` skip its grace period and publish
    /// an all-zero frame immediately after the first.
    #[test]
    fn defer_counter_resets_after_drain() {
        let mut r = Renderer::new(FakeFb::new());
        // First Clear-only cycle — exhaust the defer budget and drain.
        r.apply(RenderCommand::Clear);
        for _ in 0..MAX_CLEAR_ONLY_DEFER {
            r.compose();
        }
        r.compose();
        assert_eq!(r.fb.ops, alloc::vec![FakeOp::Clear, FakeOp::Submit]);
        // Second Clear-only cycle — defer counter must have reset.
        let baseline_len = r.fb.ops.len();
        r.apply(RenderCommand::Clear);
        r.compose();
        assert_eq!(
            r.fb.ops.len(),
            baseline_len,
            "second Clear-only queue should defer, not drain immediately"
        );
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

    // -----------------------------------------------------------
    // FramebufferOwner contract validation
    // -----------------------------------------------------------
    //
    // The 2026-05-17 less-render-disappearance fix landed in two
    // commits: 8497e0c (double-buffered SHM + Clear-only defer) and
    // d899f73 (always refresh back-from-front, no first-publish
    // skip). The double-buffer logic lives in
    // `userspace/term/src/display.rs::DisplayClient` and is not
    // host-testable (it uses real `syscall_lib::shm_*` and
    // `ipc_call_buf`). The contract DisplayClient must uphold *is*
    // testable here: between submits, the buffer the next
    // `put_glyph` / `scroll` writes into must contain the
    // previously-published frame's pixels — not be zeroed out.
    //
    // [`StatefulFakeFb`] is a single-buffer mock of that contract.
    // It tracks the cell grid the renderer paints into and records
    // a snapshot on every submit. The tests below drive the
    // renderer through real less-style compose cycles and assert
    // the published snapshots match the expected cumulative state.
    //
    // If a future regression to DisplayClient breaks the back-
    // from-front refresh, these tests still pass (the mock is not
    // the production code), but the assertions document the
    // expected behaviour and serve as a reference for any future
    // FramebufferOwner implementation.
    struct StatefulFakeFb {
        /// Toy 8 × 4 cell grid — wide enough to exercise scrolling
        /// without paying the full 80 × 25 cost. Cells store the
        /// codepoint last `put_glyph` wrote there, or 0 if blank.
        cells: [[u32; 8]; 4],
        /// Snapshot of `cells` recorded by every `submit` call.
        /// Tests inspect this to verify the cumulative grid state.
        snapshots: Vec<[[u32; 8]; 4]>,
    }

    impl StatefulFakeFb {
        fn new() -> Self {
            Self {
                cells: [[0u32; 8]; 4],
                snapshots: Vec::new(),
            }
        }
    }

    impl FramebufferOwner for StatefulFakeFb {
        fn put_glyph(
            &mut self,
            row: u16,
            col: u16,
            codepoint: u32,
            _glyph: &GlyphView<'_>,
            _fg: u32,
            _bg: u32,
        ) {
            let (r, c) = (row as usize, col as usize);
            if r < self.cells.len() && c < self.cells[0].len() {
                self.cells[r][c] = codepoint;
            }
        }

        fn clear(&mut self) {
            self.cells = [[0u32; 8]; 4];
        }

        fn scroll(&mut self, amount: i16) {
            if amount > 0 {
                // Scroll up: shift rows up, blank the bottom rows.
                let shift = (amount as usize).min(self.cells.len());
                for r in 0..self.cells.len() - shift {
                    self.cells[r] = self.cells[r + shift];
                }
                for r in self.cells.len() - shift..self.cells.len() {
                    self.cells[r] = [0u32; 8];
                }
            } else if amount < 0 {
                let shift = ((-(amount as i32)) as usize).min(self.cells.len());
                for r in (shift..self.cells.len()).rev() {
                    self.cells[r] = self.cells[r - shift];
                }
                for r in 0..shift {
                    self.cells[r] = [0u32; 8];
                }
            }
        }

        fn submit(&mut self) -> bool {
            self.snapshots.push(self.cells);
            true
        }
    }

    /// Real less-like paint sequence: first compose drains
    /// `[Clear, Put×N]` (the initial repaint after the keystroke);
    /// subsequent composes drain `[Put×M]` only (incremental updates
    /// that overwrite specific cells). The cumulative published
    /// snapshot must contain *every* cell that was ever painted,
    /// not just the cells touched in the most recent compose.
    ///
    /// The pre-fix DisplayClient bug at 8497e0c silently violated
    /// this contract: the back buffer was zeroed across the first
    /// submit's flip, so the second compose's `[Put×M]` queue
    /// painted M cells onto a zero buffer instead of onto the
    /// previously-published frame. The fix at d899f73 restores
    /// the contract by always refreshing back-from-front on
    /// submit. This test would catch any future regression of
    /// that contract in a host-testable FramebufferOwner impl.
    #[test]
    fn published_frames_accumulate_state_across_incremental_composes() {
        let mut r = Renderer::new(StatefulFakeFb::new());

        // Frame 1: full repaint — Clear + paint row 0 cells 0..4.
        r.apply(RenderCommand::Clear);
        for col in 0..4u16 {
            r.apply(RenderCommand::PutGlyph {
                row: 0,
                col,
                codepoint: b'A' as u32 + col as u32,
                fg: 0,
                bg: 0,
            });
        }
        r.compose();
        assert_eq!(r.fb.snapshots.len(), 1, "first submit");
        assert_eq!(
            r.fb.snapshots[0][0][..4],
            [b'A' as u32, b'B' as u32, b'C' as u32, b'D' as u32]
        );

        // Frame 2: incremental put — paint one cell on row 1.
        // The published frame must still contain row 0's content.
        r.apply(RenderCommand::PutGlyph {
            row: 1,
            col: 2,
            codepoint: b'X' as u32,
            fg: 0,
            bg: 0,
        });
        r.compose();
        assert_eq!(r.fb.snapshots.len(), 2, "second submit");
        let snap = &r.fb.snapshots[1];
        assert_eq!(
            snap[0][..4],
            [b'A' as u32, b'B' as u32, b'C' as u32, b'D' as u32]
        );
        assert_eq!(snap[1][2], b'X' as u32);

        // Frame 3: another incremental put. All previous cells survive.
        r.apply(RenderCommand::PutGlyph {
            row: 2,
            col: 7,
            codepoint: b'Z' as u32,
            fg: 0,
            bg: 0,
        });
        r.compose();
        assert_eq!(r.fb.snapshots.len(), 3, "third submit");
        let snap = &r.fb.snapshots[2];
        assert_eq!(
            snap[0][..4],
            [b'A' as u32, b'B' as u32, b'C' as u32, b'D' as u32]
        );
        assert_eq!(snap[1][2], b'X' as u32);
        assert_eq!(snap[2][7], b'Z' as u32);
    }

    /// `[Scroll, Put]` queue between Clear-painted frames: the
    /// scroll must operate on the previously-published content
    /// (so existing rows shift up) and the post-scroll Put paints
    /// the new bottom row. This is the shell-newline pattern.
    ///
    /// A regression that zeroes the buffer on submit would scroll
    /// blank rows instead of real content and the final snapshot
    /// would only show the Put-painted cell.
    #[test]
    fn scroll_operates_on_previously_published_content() {
        let mut r = Renderer::new(StatefulFakeFb::new());

        // Frame 1: paint 4 rows of distinct cells.
        r.apply(RenderCommand::Clear);
        for row in 0..4u16 {
            for col in 0..2u16 {
                r.apply(RenderCommand::PutGlyph {
                    row,
                    col,
                    codepoint: (b'1' as u32) + (row as u32),
                    fg: 0,
                    bg: 0,
                });
            }
        }
        r.compose();

        // Frame 2: scroll up by 1, paint the new bottom row.
        r.apply(RenderCommand::Scroll { amount: 1 });
        r.apply(RenderCommand::PutGlyph {
            row: 3,
            col: 0,
            codepoint: b'B' as u32,
            fg: 0,
            bg: 0,
        });
        r.compose();
        let snap = &r.fb.snapshots[1];
        // After scroll, row 0 is the *old* row 1, row 1 is old row 2, etc.
        assert_eq!(snap[0][0], b'2' as u32);
        assert_eq!(snap[1][0], b'3' as u32);
        assert_eq!(snap[2][0], b'4' as u32);
        assert_eq!(snap[3][0], b'B' as u32);
    }

    /// Blink-tick triggered re-submit on an empty queue must
    /// re-publish the *current* buffer state unchanged. A
    /// regression that zeros the buffer on submit would publish
    /// an all-blank frame, exactly the bug the user reported
    /// before the d899f73 fix landed.
    #[test]
    fn blink_only_submit_republishes_existing_buffer_unchanged() {
        let mut r = Renderer::new(StatefulFakeFb::new());

        // Frame 1: paint a cell.
        r.apply(RenderCommand::Clear);
        r.apply(RenderCommand::PutGlyph {
            row: 0,
            col: 0,
            codepoint: b'Q' as u32,
            fg: 0,
            bg: 0,
        });
        r.compose();
        assert_eq!(r.fb.snapshots[0][0][0], b'Q' as u32);

        // Frame 2: simulate the blink-tick path — no new queue ops,
        // just `mark_damaged()` then compose. Submit must fire on
        // the unchanged buffer and the snapshot must still hold the
        // Q cell.
        r.mark_damaged();
        r.compose();
        assert_eq!(r.fb.snapshots.len(), 2, "blink-tick submit");
        assert_eq!(
            r.fb.snapshots[1][0][0], b'Q' as u32,
            "blink-only submit must preserve existing buffer state"
        );
    }
}
