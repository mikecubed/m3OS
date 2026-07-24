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
