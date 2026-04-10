//! TTY layer (Phase 22, updated Phase 52c).
//!
//! Single console TTY (`TTY0`) holding the active termios configuration,
//! window size, foreground process group, and unified line discipline.

use kernel_core::tty::{LineDiscipline, Winsize};
use spin::Mutex;

/// Kernel-side TTY state for the single console.
///
/// The `ldisc` field owns both the termios settings and the canonical-mode
/// edit buffer. Access termios via `tty.ldisc.termios`.
pub struct TtyState {
    pub ldisc: LineDiscipline,
    pub winsize: Winsize,
    pub fg_pgid: u32,
}

impl TtyState {
    const fn new() -> Self {
        TtyState {
            ldisc: LineDiscipline::new(),
            winsize: Winsize::default_console(),
            fg_pgid: 0,
        }
    }
}

/// The single console TTY instance.
pub static TTY0: Mutex<TtyState> = Mutex::new(TtyState::new());
