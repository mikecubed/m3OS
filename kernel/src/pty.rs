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

/// Free a PTY pair slot if both refcounts are 0. Must be called with table locked.
fn try_free(table: &mut [Option<PtyPairState>; MAX_PTYS], id: u32) {
    let idx = id as usize;
    if idx >= MAX_PTYS {
        return;
    }
    if let Some(pair) = &table[idx]
        && pair.master_refcount == 0
        && pair.slave_refcount == 0
    {
        table[idx] = None;
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
    let fg;
    {
        let mut table = PTY_TABLE.lock();
        if let Some(Some(pair)) = table.get_mut(id as usize) {
            let old = pair.master_refcount;
            if pair.master_refcount > 0 {
                pair.master_refcount -= 1;
            }
            let pid = crate::process::CURRENT_PID.load(core::sync::atomic::Ordering::Relaxed);
            log::info!(
                "[pty] close_master({}): pid={} refcount {} → {}, slave_refcount={}",
                id,
                pid,
                old,
                pair.master_refcount,
                pair.slave_refcount
            );
            fg = if pair.master_refcount == 0 {
                let pgid = pair.slave_fg_pgid;
                try_free(&mut table, id);
                pgid
            } else {
                0
            };
        } else {
            let pid = crate::process::CURRENT_PID.load(core::sync::atomic::Ordering::Relaxed);
            log::warn!("[pty] close_master({}): pid={} PTY not found!", id, pid);
            return;
        }
    }
    // Send signals outside the lock to avoid deadlock with process table.
    if fg != 0 {
        crate::process::send_signal_to_group(fg, crate::process::SIGHUP);
        crate::process::send_signal_to_group(fg, crate::process::SIGCONT);
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
            try_free(&mut table, id);
        }
    }
}
