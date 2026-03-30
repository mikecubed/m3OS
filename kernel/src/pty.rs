//! PTY subsystem (Phase 29).
//!
//! Fixed pool of 16 PTY pairs. Each pair has bidirectional ring buffers,
//! per-PTY termios, edit buffer, and foreground process group tracking.

use kernel_core::pty::{MAX_PTYS, PtyPairState};
use spin::Mutex;

/// Global PTY pair table. Each slot is `None` (free) or `Some(PtyPairState)`.
pub static PTY_TABLE: Mutex<[Option<PtyPairState>; MAX_PTYS]> = {
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

/// Free a PTY pair slot. Only call when both master and slave refcounts are 0.
pub fn free_pty(id: u32) {
    let mut table = PTY_TABLE.lock();
    if (id as usize) < MAX_PTYS {
        table[id as usize] = None;
    }
}

/// Increment the master refcount for a PTY (called on fork/dup).
pub fn add_master_ref(id: u32) {
    let mut table = PTY_TABLE.lock();
    if let Some(Some(pair)) = table.get_mut(id as usize) {
        pair.master_refcount += 1;
    }
}

/// Increment the slave refcount for a PTY (called on fork/dup).
pub fn add_slave_ref(id: u32) {
    let mut table = PTY_TABLE.lock();
    if let Some(Some(pair)) = table.get_mut(id as usize) {
        pair.slave_refcount += 1;
    }
}

/// Close one master reference. Sends SIGHUP when last ref is closed.
pub fn close_master(id: u32) {
    let mut table = PTY_TABLE.lock();
    if let Some(Some(pair)) = table.get_mut(id as usize) {
        if pair.master_refcount > 0 {
            pair.master_refcount -= 1;
        }
        if pair.master_refcount == 0 {
            let fg = pair.slave_fg_pgid;
            let slave_gone = pair.slave_refcount == 0;
            drop(table);
            if fg != 0 {
                crate::process::send_signal_to_group(fg, crate::process::SIGHUP);
                crate::process::send_signal_to_group(fg, crate::process::SIGCONT);
            }
            if slave_gone {
                free_pty(id);
            }
        }
    }
}

/// Close one slave reference. Frees the PTY if both sides are done.
pub fn close_slave(id: u32) {
    let mut table = PTY_TABLE.lock();
    if let Some(Some(pair)) = table.get_mut(id as usize) {
        if pair.slave_refcount > 0 {
            pair.slave_refcount -= 1;
        }
        if pair.slave_refcount == 0 {
            let master_gone = pair.master_refcount == 0;
            drop(table);
            if master_gone {
                free_pty(id);
            }
        }
    }
}
