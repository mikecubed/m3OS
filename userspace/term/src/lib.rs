//! `term` — Phase 57 Track G graphical terminal emulator.
//!
//! `term` is the first non-demo graphical client of the Phase 56
//! display server. It hosts a PTY pair (Phase 29), spawns the in-tree
//! shell on the secondary side, parses the shell's ANSI output through
//! the Phase 22b parser (`kernel_core::fb::AnsiParser`), maintains a
//! fixed-size screen state machine + scrollback ring, and drives the
//! Phase 56 display protocol to put pixels on the surface. Keyboard
//! input arrives as typed `KeyEvent`s from `kbd_server` via
//! `display_server`'s focus dispatcher and is translated to PTY-side
//! byte sequences.
//!
//! # Module layout (Single Responsibility)
//!
//! | Module    | Concern                                                                                |
//! |-----------|----------------------------------------------------------------------------------------|
//! | [`bell`]  | BEL coalescing window + `BellSink` seam (audio_client integration is post-Track-E)     |
//! | [`input`] | `KeyEvent` → PTY byte translation                                                      |
//! | [`pty`]   | PTY pair open + shell spawn (`PtyHost`)                                                |
//! | [`render`]| `RenderCommand` → display-server surface buffer                                        |
//! | [`screen`]| ANSI parser consumer + cell buffer + scrollback ring (`Screen`, `RenderCommand`)       |
//!
//! # `#![no_std]` discipline
//!
//! Every module is `#![no_std]` + `alloc` (the binary supplies a
//! `BrkAllocator`). Host tests build under `cargo test -p term --target
//! x86_64-unknown-linux-gnu` because the lib target compiles without
//! the OS-only `entry_point!` macro (gated on the `os-binary` feature).
//!
//! # Resource bounds
//!
//! - Scrollback: fixed at [`SCROLLBACK_LINES`] = 1000 lines (per the
//!   Phase 57 task list G.4 acceptance "Scrollback fixed at 1000 lines;
//!   exceeding the cap drops the oldest line").
//! - Cell grid: 80 × 25 default; resize is deferred (G.5 ships fixed
//!   geometry, future track upgrades the surface protocol with
//!   resize). 80 × 25 × 16 bytes/cell ≈ 32 KiB heap.
//! - One PTY pair, one shell process; on shell exit `term` exits zero
//!   and the supervisor restarts per `term.conf` (`restart=on-failure
//!   max_restart=3`).

#![cfg_attr(not(test), no_std)]

extern crate alloc;
#[cfg(test)]
extern crate std;

use kernel_core::display::protocol::CLIPBOARD_MAX_BYTES;
use kernel_core::input::events::PointerEvent;

pub mod bell;
pub mod input;
pub mod mouse;
pub mod pty;
pub mod render;
pub mod screen;

#[cfg(all(not(test), feature = "os-binary"))]
pub mod display;
#[cfg(any(test, all(not(test), feature = "os-binary")))]
pub mod syscall_pty;

/// Boot-log marker written when the terminal starts.  Used by smoke
/// scripts to confirm the binary spawned.
pub const BOOT_LOG_MARKER: &str = "term: spawned\n";

/// Sentinel emitted immediately after the surface registration
/// completes and the input/PTY loop is about to start.  Smoke scripts
/// wait for this line to confirm `term` is live.
pub const READY_SENTINEL: &str = "TERM_SMOKE:ready\n";

/// Service marker published after the graphical shell has produced enough
/// PTY output to prove the prompt path is alive.
///
/// Phase 72b — `term` no longer registers a global `SERVICE_NAME`
/// because `term` is a user-facing app (potentially many concurrent
/// instances), not a singleton daemon. The first term to start still
/// best-effort registers [`PROMPT_READY_SERVICE`] as a smoke-test
/// readiness sentinel; subsequent term instances silently lose the
/// race (a no-op, not a fatal error) because only one such sentinel
/// is needed and any later term reaching the same point implies the
/// graphical-shell path is alive system-wide.
pub const PROMPT_READY_SERVICE: &str = "term.prompt-ready";

/// Minimum PTY bytes before publishing [`PROMPT_READY_SERVICE`].
pub const PROMPT_READY_MIN_BYTES: u64 = 32;

/// Fixed scrollback cap in lines.  G.4 acceptance: "Scrollback fixed
/// at 1000 lines; exceeding the cap drops the oldest line".
pub const SCROLLBACK_LINES: usize = 1000;

/// Default cell grid columns.  Phase 57 ships a fixed geometry; resize
/// is an explicitly deferred follow-up.
pub const DEFAULT_COLS: u16 = 80;

/// Default cell grid rows.  Same fixed-geometry rationale as
/// [`DEFAULT_COLS`].
pub const DEFAULT_ROWS: u16 = 25;

/// Cell pixel width. Phase 73 bumped this from 16 to 24 (3× the static
/// 8×16 fallback width) so the terminal stays legible on a 1080p
/// framebuffer; the static IBM VGA bitmap still occupies a clean integer
/// sub-rect (top-left 8×16).
///
/// Phase 112 Track B.1 **moved this here from `display`**. `display` is
/// gated behind the `os-binary` feature, so `mouse` could not import it
/// and carried a private copy of the literals instead — a copy that was
/// never updated when Phase 73 changed them, leaving mouse reporting
/// projecting pixels onto a 16×32 grid that had not existed for
/// twenty-odd phases (a click was reported ~1.5 cells off). Selection
/// hit-testing has to agree with rendering exactly, so the constants now
/// live in the ungated crate root and every consumer reads the same two
/// values.
pub const CELL_WIDTH: u8 = 24;

/// Cell pixel height. Phase 73 bumped this from 32 to 48 so the cell
/// matches the wider [`CELL_WIDTH`] and stays a 3× multiple of the 8×16
/// static fallback. See [`CELL_WIDTH`] for why it lives here.
pub const CELL_HEIGHT: u8 = 48;

/// Project a pointer event's pixel position onto a 0-based `(row, col)`
/// display cell, clamped into a `cols` × `rows` grid.
///
/// The pixels are **surface-local**: `display_server` rebases
/// `PointerEvent::abs_position` onto the hit surface's geometry origin
/// before delivering the event, so `(0, 0)` is term's own top-left corner
/// and the plain division below needs no origin term. (Before that
/// rebasing landed, every term window's hit-test was off by its tile
/// origin — at minimum the bar's exclusive zone in `y`.)
///
/// An event with no absolute position projects to the origin cell rather
/// than being rejected: the only producer that omits it is the USB HID
/// Report-protocol decoder, whose wheel notches carry no coordinates and
/// for which a cell is still required to encode a report.
///
/// Mirrors `mouse::compute_cell_position`, but that one returns 1-based
/// coordinates for the VT wire protocol while selection indexes cells
/// from zero. [`CELL_WIDTH`] / [`CELL_HEIGHT`] are the same constants the
/// renderer lays glyphs out with, so the highlight lands under the
/// pointer.
pub fn pointer_cell(ev: &PointerEvent, cols: u16, rows: u16) -> (u16, u16) {
    let (px, py) = ev.abs_position.unwrap_or((0, 0));
    let col = (px.max(0) as u32 / CELL_WIDTH as u32) as u16;
    let row = (py.max(0) as u32 / CELL_HEIGHT as u32) as u16;
    (
        row.min(rows.saturating_sub(1)),
        col.min(cols.saturating_sub(1)),
    )
}

/// Whether `text` fits in a single compositor clipboard offer.
///
/// The compositor's `SetClipboard` verb carries a byte length, so the cap
/// is on **encoded bytes**, not characters — a selection of multi-byte
/// glyphs hits the limit at correspondingly fewer characters.
///
/// The check lives here rather than inline in the `display` module's
/// `DisplayClient::set_clipboard` because that module only compiles for
/// the OS target, which put the predicate out of reach of the host tests
/// that are supposed to pin it. `set_clipboard` calls this and *rejects*
/// an over-cap offer rather than truncating it: silently copying half a
/// selection is worse than copying nothing, because the user cannot see
/// the cut.
pub fn clipboard_payload_fits(text: &str) -> bool {
    text.len() <= CLIPBOARD_MAX_BYTES
}

/// Decide whether the event loop should publish the current renderer frame.
///
/// The normal throttle avoids excessive display IPC, but PTY output that has
/// already reached the bottom row must be published promptly. Otherwise the
/// current line can appear to contain only the first glyph until a later scroll
/// replays the queued glyph operations.
pub fn should_compose_frame(
    damaged: bool,
    pty_drained_this_tick: bool,
    cursor_row: u16,
    rows: u16,
    elapsed_ms: u64,
    interval_ms: u64,
) -> bool {
    if !damaged {
        return false;
    }
    if pty_drained_this_tick && rows > 0 && cursor_row >= rows.saturating_sub(1) {
        return true;
    }
    elapsed_ms >= interval_ms
}

/// Top-level error type for the terminal binary.  Every fallible
/// boundary inside `term` returns one of these; the binary's
/// `program_main` matches and writes a structured marker to stdout
/// before returning a non-zero exit so the supervisor can record the
/// failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum TermError {
    /// `display_server` was not registered after the bounded retry
    /// budget — the session is not yet ready or has crashed.
    DisplayServerUnavailable,
    /// PTY pair could not be allocated; root cause is the underlying
    /// errno from `openpty`.
    PtyOpen(i32),
    /// Shell process could not be spawned; root cause is the
    /// underlying errno from `execve`.
    ShellSpawn(i32),
    /// Encountered a malformed event from `kbd_server` that the input
    /// codec rejected.
    InputDecode,
    /// Failure rendering a glyph.
    Render(crate::screen::ScreenError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use kernel_core::input::events::{ModifierState, PointerButton};

    /// Motion event at `pos` with no buttons and no wheel — the shape the
    /// compositor delivers while a selection drag is in flight.
    fn motion_at(pos: Option<(i32, i32)>) -> PointerEvent {
        PointerEvent {
            timestamp_ms: 0,
            dx: 0,
            dy: 0,
            abs_position: pos,
            button: PointerButton::None,
            wheel_dx: 0,
            wheel_dy: 0,
            modifiers: ModifierState::empty(),
        }
    }

    #[test]
    fn pointer_cell_maps_the_origin_pixel_to_the_origin_cell() {
        assert_eq!(pointer_cell(&motion_at(Some((0, 0))), 80, 25), (0, 0));
        // Anywhere inside the first cell still resolves to it.
        let inside = Some((CELL_WIDTH as i32 - 1, CELL_HEIGHT as i32 - 1));
        assert_eq!(pointer_cell(&motion_at(inside), 80, 25), (0, 0));
    }

    #[test]
    fn pointer_cell_maps_the_first_pixel_of_each_cell_to_that_cell() {
        let pos = Some((CELL_WIDTH as i32, CELL_HEIGHT as i32));
        assert_eq!(pointer_cell(&motion_at(pos), 80, 25), (1, 1));
        let pos = Some((3 * CELL_WIDTH as i32 + 5, 7 * CELL_HEIGHT as i32 + 5));
        assert_eq!(pointer_cell(&motion_at(pos), 80, 25), (7, 3));
    }

    #[test]
    fn pointer_cell_resolves_the_last_cell_of_the_grid() {
        // Top-left pixel of the bottom-right cell of an 80×25 grid.
        let pos = Some((79 * CELL_WIDTH as i32, 24 * CELL_HEIGHT as i32));
        assert_eq!(pointer_cell(&motion_at(pos), 80, 25), (24, 79));
    }

    #[test]
    fn pointer_cell_clamps_past_the_right_and_bottom_edges() {
        // The surface is letterboxed inside its tile and the compositor
        // clips against the output, not against term's cell grid, so a
        // pointer can legitimately land past the last full cell.
        let pos = Some((10_000, 10_000));
        assert_eq!(pointer_cell(&motion_at(pos), 80, 25), (24, 79));
        // One pixel past the last cell of each axis is already outside.
        let pos = Some((80 * CELL_WIDTH as i32, 25 * CELL_HEIGHT as i32));
        assert_eq!(pointer_cell(&motion_at(pos), 80, 25), (24, 79));
    }

    #[test]
    fn pointer_cell_clamps_negative_coordinates_to_the_origin() {
        // Surface-local coordinates are produced by a saturating subtract
        // so they should never be negative, but a drag that leaves the
        // surface must clamp rather than wrap through the `as u32` cast.
        assert_eq!(pointer_cell(&motion_at(Some((-1, -1))), 80, 25), (0, 0));
        assert_eq!(
            pointer_cell(&motion_at(Some((i32::MIN, i32::MIN))), 80, 25),
            (0, 0)
        );
    }

    #[test]
    fn pointer_cell_treats_a_missing_position_as_the_origin_cell() {
        assert_eq!(pointer_cell(&motion_at(None), 80, 25), (0, 0));
    }

    #[test]
    fn pointer_cell_survives_a_degenerate_grid() {
        // `saturating_sub` keeps a zero-sized grid from underflowing to
        // 65535; a grid is never empty in practice, but a `SurfaceResized`
        // race could ask.
        assert_eq!(pointer_cell(&motion_at(Some((500, 500))), 0, 0), (0, 0));
        assert_eq!(pointer_cell(&motion_at(Some((500, 500))), 1, 1), (0, 0));
    }

    #[test]
    fn clipboard_cap_accepts_exactly_the_limit_and_rejects_one_more() {
        let at_cap = "a".repeat(CLIPBOARD_MAX_BYTES);
        assert!(clipboard_payload_fits(&at_cap));
        let over_cap = "a".repeat(CLIPBOARD_MAX_BYTES + 1);
        assert!(!clipboard_payload_fits(&over_cap));
    }

    #[test]
    fn clipboard_cap_accepts_an_empty_offer() {
        // An empty selection is a no-op at the call site, not a rejection.
        assert!(clipboard_payload_fits(""));
    }

    #[test]
    fn clipboard_cap_counts_bytes_not_characters() {
        // 'é' is two UTF-8 bytes, so this string has half as many chars as
        // bytes and straddles the cap: one char short fits, the char that
        // crosses it does not.
        let fits = "é".repeat(CLIPBOARD_MAX_BYTES / 2);
        assert_eq!(fits.len(), CLIPBOARD_MAX_BYTES);
        assert_eq!(fits.chars().count(), CLIPBOARD_MAX_BYTES / 2);
        assert!(clipboard_payload_fits(&fits));

        let over = "é".repeat(CLIPBOARD_MAX_BYTES / 2 + 1);
        assert_eq!(over.len(), CLIPBOARD_MAX_BYTES + 2);
        // Fewer characters than the cap, yet still rejected — the check is
        // on the wire length the `SetClipboard` verb carries.
        assert!(over.chars().count() < CLIPBOARD_MAX_BYTES);
        assert!(!clipboard_payload_fits(&over));
    }

    /// Default geometry must be the documented fixed grid.
    #[test]
    fn default_geometry_pinned() {
        assert_eq!(DEFAULT_COLS, 80);
        assert_eq!(DEFAULT_ROWS, 25);
    }

    /// Scrollback cap must remain 1000 — the value is referenced by
    /// `Screen` and tested for ring eviction in `screen` tests.
    #[test]
    fn scrollback_cap_pinned() {
        assert_eq!(SCROLLBACK_LINES, 1000);
    }

    #[test]
    fn compose_policy_flushes_damaged_last_row_without_waiting_for_throttle() {
        assert!(
            should_compose_frame(true, true, DEFAULT_ROWS - 1, DEFAULT_ROWS, 0, 16),
            "bottom-row PTY output must publish promptly instead of waiting for scroll"
        );
    }

    #[test]
    fn compose_policy_keeps_throttle_away_from_bottom_row() {
        assert!(
            !should_compose_frame(true, true, 0, DEFAULT_ROWS, 0, 16),
            "off-bottom PTY output should still respect the frame throttle"
        );
        assert!(
            should_compose_frame(true, true, 0, DEFAULT_ROWS, 16, 16),
            "elapsed throttle interval still permits compose"
        );
        assert!(
            !should_compose_frame(false, true, DEFAULT_ROWS - 1, DEFAULT_ROWS, 16, 16),
            "undamaged frames must not compose"
        );
    }
}
