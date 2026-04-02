//! PTY subsystem (Phase 29).
//!
//! Fixed pool of 16 PTY pairs. Each pair has bidirectional ring buffers,
//! per-PTY termios, edit buffer, and foreground process group tracking.

use kernel_core::pty::{MAX_PTYS, PtyPairState};
use spin::Mutex;

use crate::task::wait_queue::WaitQueue;

/// Global PTY pair table. Each slot is `None` (free) or `Some(PtyPairState)`.
pub static PTY_TABLE: Mutex<[Option<PtyPairState>; MAX_PTYS]> = {
    const NONE: Option<PtyPairState> = None;
    Mutex::new([NONE; MAX_PTYS])
};

/// Per-PTY wait queues for master side (woken when slave writes data to s2m).
#[allow(clippy::declare_interior_mutable_const)]
static PTY_MASTER_WQ: [WaitQueue; MAX_PTYS] = {
    const WQ: WaitQueue = WaitQueue::new();
    [WQ; MAX_PTYS]
};

/// Per-PTY wait queues for slave side (woken when master writes data to m2s).
#[allow(clippy::declare_interior_mutable_const)]
static PTY_SLAVE_WQ: [WaitQueue; MAX_PTYS] = {
    const WQ: WaitQueue = WaitQueue::new();
    [WQ; MAX_PTYS]
};

/// Wake tasks waiting on the master side of a PTY.
pub fn wake_master(id: u32) {
    if (id as usize) < MAX_PTYS {
        PTY_MASTER_WQ[id as usize].wake_all();
    }
}

/// Wake tasks waiting on the slave side of a PTY.
pub fn wake_slave(id: u32) {
    if (id as usize) < MAX_PTYS {
        PTY_SLAVE_WQ[id as usize].wake_all();
    }
}

/// Register the current task on a PTY master's wait queue.
#[allow(dead_code)]
pub fn register_master_waiter(id: u32) {
    if (id as usize) < MAX_PTYS {
        PTY_MASTER_WQ[id as usize].sleep();
    }
}

/// Register the current task on a PTY slave's wait queue.
#[allow(dead_code)]
pub fn register_slave_waiter(id: u32) {
    if (id as usize) < MAX_PTYS {
        PTY_SLAVE_WQ[id as usize].sleep();
    }
}

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
        pair.slave_opened = true;
    }
}

/// Close one master reference. Sends SIGHUP when last ref is closed.
/// Also frees the PTY pair if the slave side has already fully closed.
pub fn close_master(id: u32) {
    let fg;
    {
        let mut table = PTY_TABLE.lock();
        if let Some(Some(pair)) = table.get_mut(id as usize) {
            if pair.master_refcount > 0 {
                pair.master_refcount -= 1;
            }
            fg = if pair.master_refcount == 0 {
                pair.slave_fg_pgid
            } else {
                0
            };
            // Free if both sides are done and the slave was opened at
            // least once (prevents a race where master is closed by a
            // forked child before the slave has been opened).
            if pair.master_refcount == 0 && pair.slave_refcount == 0 && pair.slave_opened {
                try_free(&mut table, id);
            }
        } else {
            return;
        }
    }
    // Send signals outside the lock to avoid deadlock with process table.
    if fg != 0 {
        crate::process::send_signal_to_group(fg, crate::process::SIGHUP);
        crate::process::send_signal_to_group(fg, crate::process::SIGCONT);
    }
    // Wake both sides — slave readers see EOF, master pollers see HUP.
    wake_master(id);
    wake_slave(id);
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
    drop(table);
    // Wake master side — master pollers see HUP when slave closes.
    wake_master(id);
    wake_slave(id);
}
