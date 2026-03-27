//! TTY layer (Phase 22).
//!
//! Single console TTY (`TTY0`) holding the active termios configuration,
//! window size, foreground process group, and canonical-mode edit buffer.

use kernel_core::tty::{EditBuffer, Termios, Winsize};
use spin::Mutex;

/// Kernel-side TTY state for the single console.
pub struct TtyState {
    pub termios: Termios,
    pub winsize: Winsize,
    pub fg_pgid: u32,
    pub edit_buf: EditBuffer,
}

impl TtyState {
    const fn new() -> Self {
        TtyState {
            termios: Termios::default_cooked(),
            winsize: Winsize::default_console(),
            fg_pgid: 0,
            edit_buf: EditBuffer::new(),
        }
    }
}

/// The single console TTY instance.
pub static TTY0: Mutex<TtyState> = Mutex::new(TtyState::new());
