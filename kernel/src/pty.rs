//! PTY subsystem (Phase 29).
//!
//! Fixed pool of 16 PTY pairs. Each pair has bidirectional ring buffers,
//! per-PTY termios, edit buffer, and foreground process group tracking.

use kernel_core::pty::{MAX_PTYS, PtyPairState};
use spin::Mutex;

/// Global PTY pair table. Each slot is `None` (free) or `Some(PtyPairState)`.
pub static PTY_TABLE: Mutex<[Option<PtyPairState>; MAX_PTYS]> = {
    // Initialize all 16 slots to None using a const array.
    const NONE: Option<PtyPairState> = None;
    Mutex::new([NONE; MAX_PTYS])
};

/// Allocate a new PTY pair. Returns the PTY ID (index) or `Err(())` if full.
pub fn alloc_pty() -> Result<u32, ()> {
    let mut table = PTY_TABLE.lock();
    for (i, slot) in table.iter_mut().enumerate() {
        if slot.is_none() {
            *slot = Some(PtyPairState::new(i as u32));
            return Ok(i as u32);
        }
    }
    Err(())
}

/// Free a PTY pair slot. Only call when both master and slave are closed.
pub fn free_pty(id: u32) {
    let mut table = PTY_TABLE.lock();
    if (id as usize) < MAX_PTYS {
        table[id as usize] = None;
    }
}

/// Close the master side of a PTY. Sends SIGHUP to slave foreground group.
/// Frees the PTY if the slave is also closed.
pub fn close_master(id: u32) {
    let mut table = PTY_TABLE.lock();
    if let Some(Some(pair)) = table.get_mut(id as usize) {
        pair.master_open = false;
        let fg = pair.slave_fg_pgid;
        let slave_closed = !pair.slave_open;
        drop(table);
        if fg != 0 {
            crate::process::send_signal_to_group(fg, crate::process::SIGHUP);
            crate::process::send_signal_to_group(fg, crate::process::SIGCONT);
        }
        if slave_closed {
            free_pty(id);
        }
    }
}

/// Close the slave side of a PTY. Frees the PTY if the master is also closed.
pub fn close_slave(id: u32) {
    let mut table = PTY_TABLE.lock();
    if let Some(Some(pair)) = table.get_mut(id as usize) {
        pair.slave_open = false;
        let master_closed = !pair.master_open;
        drop(table);
        if master_closed {
            free_pty(id);
        }
    }
}
