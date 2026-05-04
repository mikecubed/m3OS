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

/// Service name under which `term` registers with the IPC service
/// registry so `session_manager` can probe its readiness.
pub const SERVICE_NAME: &str = "term";

/// Service marker published after the graphical shell has produced enough
/// PTY output to prove the prompt path is alive.
pub const PROMPT_READY_SERVICE: &str = "term.prompt-ready";

/// Minimum PTY bytes before publishing [`PROMPT_READY_SERVICE`].
pub const PROMPT_READY_MIN_BYTES: u64 = 32;

/// Service-manifest restart budget — pinned at 3 per the G.1
/// acceptance ("`restart=on-failure max_restart=3
/// depends=display,kbd,session_manager`"). The dep names match the
/// REGISTERED service names from each daemon's `.conf` (display_server.conf
/// `name=display`, kbd.conf `name=kbd`, session_manager.conf
/// `name=session_manager`), not the binary names.
pub const SERVICE_MAX_RESTART: u32 = 3;

/// Service-manifest restart policy literal.
pub const SERVICE_RESTART_POLICY: &str = "on-failure";

/// Service-manifest dependency list.  `term` requires display
/// (compositor — registered name from display_server.conf), kbd
/// (focus-aware key events — registered name from kbd.conf) and
/// session_manager (entry orchestration) to be running before it can
/// claim a surface.
pub const SERVICE_DEPENDS: &str = "display,kbd,session_manager";

/// Fixed scrollback cap in lines.  G.4 acceptance: "Scrollback fixed
/// at 1000 lines; exceeding the cap drops the oldest line".
pub const SCROLLBACK_LINES: usize = 1000;

/// Default cell grid columns.  Phase 57 ships a fixed geometry; resize
/// is an explicitly deferred follow-up.
pub const DEFAULT_COLS: u16 = 80;

/// Default cell grid rows.  Same fixed-geometry rationale as
/// [`DEFAULT_COLS`].
pub const DEFAULT_ROWS: u16 = 25;

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

    /// Phase 57 G.1 acceptance: the manifest constants record
    /// `restart=on-failure max_restart=3 depends=display,kbd,session_manager`.
    /// The dep names match the REGISTERED service names (from each
    /// daemon's `.conf` `name=` field), not the binary names.  This
    /// test pins the constants the `populate_ext2_files` helper consumes.
    #[test]
    fn service_manifest_constants_match_acceptance() {
        assert_eq!(SERVICE_RESTART_POLICY, "on-failure");
        assert_eq!(SERVICE_MAX_RESTART, 3);
        assert_eq!(SERVICE_DEPENDS, "display,kbd,session_manager");
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
