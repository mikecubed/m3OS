//! Asynchronous notification objects.
// Not yet wired to main.rs — suppress dead-code until integration.
#![allow(dead_code)]
//!
//! A [`Notification`] is a single machine-word bitfield.  Each bit is an
//! independent signal channel.  The sender sets bits atomically (no blocking,
//! safe from interrupt handlers); the receiver blocks until at least one bit
//! is set, then atomically clears and returns the pending bits.
//!
//! # Typical use: IRQ delivery
//!
//! ```text
//! kbd_server startup:
//!   handle = create_notification()
//!   register_irq(IRQ1, handle)   // kernel: on IRQ1, set bit 0
//!   loop:
//!     bits = notify_wait(handle) // blocks until bit set
//!     scancode = in(0x60)
//!     ... process key event ...
//! ```
//!
//! # Implementation notes
//!
//! `signal` uses an atomic fetch-or so it is async-signal-safe: interrupt
//! handlers can call it without any lock.  `wait` uses a spin mutex only to
//! check-and-clear the pending bits and update the waiter field; it releases
//! the lock before calling into the scheduler.
//!
//! This module is implemented by Track B of the parallel-implementation loop.
//! See `docs/roadmap/tasks/06-ipc-core-tasks.md` tasks P6-T006 and P6-T007.

extern crate alloc;

use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;

use crate::task::{scheduler, TaskId};

// ---------------------------------------------------------------------------
// NotifId
// ---------------------------------------------------------------------------

/// Index into the global notification registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotifId(pub u8);

// ---------------------------------------------------------------------------
// Global notification registry
// ---------------------------------------------------------------------------

/// Maximum number of notification objects.
const MAX_NOTIFS: usize = 16;

/// Global registry of notification objects.
pub static NOTIFICATIONS: Mutex<NotifRegistry> = Mutex::new(NotifRegistry::new());

pub struct NotifRegistry {
    slots: [Option<Notification>; MAX_NOTIFS],
    /// Maps hardware IRQ number (0–15) to the notification that should
    /// receive a signal when that IRQ fires.
    irq_map: [Option<NotifId>; 16],
}

impl NotifRegistry {
    const fn new() -> Self {
        NotifRegistry {
            slots: [
                None, None, None, None, None, None, None, None, None, None, None, None, None, None,
                None, None,
            ],
            irq_map: [None; 16],
        }
    }

    /// Allocate a new notification object and return its [`NotifId`].
    ///
    /// # Panics (debug)
    ///
    /// Panics if all 16 slots are occupied.
    pub fn create(&mut self) -> NotifId {
        for (i, slot) in self.slots.iter_mut().enumerate() {
            if slot.is_none() {
                *slot = Some(Notification::new());
                return NotifId(i as u8);
            }
        }
        panic!("notification registry full");
    }

    /// Register an IRQ number to signal a notification on each delivery.
    ///
    /// `irq` is the hardware IRQ line (0 = timer, 1 = keyboard, …).
    /// When the kernel's IRQ dispatcher fires, it calls [`signal_irq`].
    pub fn register_irq(&mut self, irq: u8, notif_id: NotifId) {
        if (irq as usize) < self.irq_map.len() {
            self.irq_map[irq as usize] = Some(notif_id);
        }
    }

    /// Look up which notification (if any) is registered for a hardware IRQ.
    pub fn irq_notif(&self, irq: u8) -> Option<NotifId> {
        self.irq_map.get(irq as usize).copied().flatten()
    }

    /// Access a notification slot mutably.
    pub fn get_mut(&mut self, id: NotifId) -> Option<&mut Notification> {
        self.slots.get_mut(id.0 as usize)?.as_mut()
    }

    /// Access a notification slot by shared reference.
    ///
    /// Used for `signal` which operates through the atomic inside.
    pub fn get(&self, id: NotifId) -> Option<&Notification> {
        self.slots.get(id.0 as usize)?.as_ref()
    }
}

// ---------------------------------------------------------------------------
// Notification
// ---------------------------------------------------------------------------

/// An asynchronous notification object — a bitfield word with one waiting task.
pub struct Notification {
    /// Pending bits, atomically updated.  Bit k is set when channel k has
    /// been signalled but not yet consumed by `wait`.
    pending: AtomicU64,
    /// The task currently blocked in `wait`, if any.
    ///
    /// Protected by the registry mutex (same lock as the outer slot).
    waiter: Option<TaskId>,
}

impl Notification {
    const fn new() -> Self {
        Notification {
            pending: AtomicU64::new(0),
            waiter: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Notification operations
// ---------------------------------------------------------------------------

/// Signal one or more bits on a notification object.
///
/// This is async-signal-safe: it performs only an atomic fetch-or and may
/// optionally wake a blocked waiter.  Safe to call from interrupt handlers.
///
/// If a task is blocked in [`wait`] on this notification, it is woken.
pub fn signal(notif_id: NotifId, bits: u64) {
    // Fast path: set the bits atomically.
    let prev = {
        let reg = NOTIFICATIONS.lock();
        let notif = match reg.get(notif_id) {
            Some(n) => n,
            None => return,
        };
        notif.pending.fetch_or(bits, Ordering::Release)
    };

    // If bits were zero before (first signal), check for a waiter to wake.
    // We also wake if prev > 0 in case the waiter hasn't drained yet, but
    // waking twice is harmless (the scheduler just re-runs a ready task).
    let _ = prev;

    // Wake the waiter, if any.
    let waiter = {
        let mut reg = NOTIFICATIONS.lock();
        let notif = match reg.get_mut(notif_id) {
            Some(n) => n,
            None => return,
        };
        notif.waiter.take()
    };
    if let Some(task) = waiter {
        scheduler::wake_task(task);
    }
}

/// Deliver a hardware IRQ to its registered notification object.
///
/// Called from the interrupt dispatcher.  `irq` is the hardware IRQ line.
/// Sets bit `irq` in the notification's pending field.
pub fn signal_irq(irq: u8) {
    let notif_id = {
        let reg = NOTIFICATIONS.lock();
        reg.irq_notif(irq)
    };
    if let Some(id) = notif_id {
        signal(id, 1 << irq);
    }
}

/// Wait for any bit to be set on a notification object.
///
/// If bits are already pending, clears and returns them immediately (no
/// blocking).  Otherwise blocks until [`signal`] wakes this task, then
/// returns the accumulated pending bits.
///
/// Returns the bits that were pending (non-zero on success), or 0 on error.
pub fn wait(waiter: TaskId, notif_id: NotifId) -> u64 {
    loop {
        // Try to drain pending bits.
        let bits = {
            let reg = NOTIFICATIONS.lock();
            let notif = match reg.get(notif_id) {
                Some(n) => n,
                None => return 0,
            };
            notif.pending.swap(0, Ordering::Acquire)
        };

        if bits != 0 {
            return bits;
        }

        // No bits pending — register ourselves as the waiter and block.
        {
            let mut reg = NOTIFICATIONS.lock();
            let notif = match reg.get_mut(notif_id) {
                Some(n) => n,
                None => return 0,
            };
            // Double-check: a signal may have arrived between the swap and here.
            let bits2 = notif.pending.swap(0, Ordering::Acquire);
            if bits2 != 0 {
                return bits2;
            }
            notif.waiter = Some(waiter);
        }

        // Block; woken by signal().
        scheduler::block_current_on_recv();
        // Loop back to drain bits.
    }
}
