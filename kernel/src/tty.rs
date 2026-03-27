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

// ---------------------------------------------------------------------------
// PTY skeleton (Phase 22 — data path deferred to Phase 23+)
// ---------------------------------------------------------------------------

use core::sync::atomic::{AtomicU32, Ordering};

/// Next PTY pair ID (monotonically increasing).
static NEXT_PTY_ID: AtomicU32 = AtomicU32::new(0);

/// Allocate a new PTY pair, returning the pty_id.
pub fn alloc_pty() -> u32 {
    NEXT_PTY_ID.fetch_add(1, Ordering::Relaxed)
}
