//! Per-process capability table.
//!
//! A **capability** is an unforgeable token that grants the holder specific
//! rights to a kernel object.  In this implementation capabilities are integer
//! handles — indices into a per-process [`CapabilityTable`].  The kernel
//! validates every handle on every IPC syscall; raw integer forgery returns
//! [`CapError::InvalidHandle`].
//!
//! # Capability types (Phase 6)
//!
//! | Variant | Grants |
//! |---|---|
//! | `Endpoint(EndpointId)` | Send/receive on a specific IPC endpoint |
//! | `Notification(NotifId)` | Signal or wait on a notification object |
//! | `Reply(TaskId)` | Reply to a specific blocked caller (one-shot) |
//!
//! # Table size
//!
//! Each process holds a fixed 64-slot table allocated alongside the task
//! structure.  64 entries is sufficient for a teaching OS with a handful of
//! system services.  Growable tables are deferred to Phase 7+.
//!
//! # Capability grants (deferred)
//!
//! `sys_cap_grant` (transfer of a capability to another process via IPC) is
//! deferred to Phase 7+.  Phase 6 focuses on the core IPC path.

// Capability table is integrated with Task and used by the IPC demo and
// syscall dispatcher; keep dead-code allowance for unused APIs.
#![allow(dead_code)]

use super::{EndpointId, NotifId};
use crate::task::TaskId;

// ---------------------------------------------------------------------------
// Handle type
// ---------------------------------------------------------------------------

/// An opaque integer index into a [`CapabilityTable`].
///
/// Userspace passes this value in a syscall register; the kernel validates it
/// before dereferencing the table.
pub type CapHandle = u32;

// ---------------------------------------------------------------------------
// Capability variants
// ---------------------------------------------------------------------------

/// A single capability slot — what a handle actually grants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capability {
    /// Right to send/receive on a kernel endpoint.
    Endpoint(EndpointId),
    /// Right to signal or wait on a notification object.
    Notification(NotifId),
    /// One-shot right to reply to a specific blocked caller.
    ///
    /// The kernel inserts this into the server's cap table when it delivers
    /// a `call` message.  Consuming it (via `reply` or `reply_recv`) removes
    /// the slot; a second reply attempt returns [`CapError::InvalidHandle`].
    Reply(TaskId),
}

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors returned by capability-table operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapError {
    /// The supplied handle is out-of-range or points to an empty slot.
    InvalidHandle,
    /// The slot holds a capability of a different type than requested.
    WrongType,
    /// All 64 slots are occupied; cannot insert a new capability.
    TableFull,
}

// ---------------------------------------------------------------------------
// Capability table
// ---------------------------------------------------------------------------

/// Fixed-size per-process capability table (64 slots).
///
/// `None` means the slot is empty.  Slots are filled on [`insert`][Self::insert]
/// and cleared on [`remove`][Self::remove].
pub struct CapabilityTable {
    slots: [Option<Capability>; Self::SIZE],
}

impl CapabilityTable {
    /// Maximum number of capabilities a single process may hold.
    pub const SIZE: usize = 64;

    /// Create an empty capability table.
    pub const fn new() -> Self {
        CapabilityTable {
            slots: [None; Self::SIZE],
        }
    }

    /// Insert a capability and return its handle.
    ///
    /// Scans for the first empty slot.  Returns [`CapError::TableFull`] if all
    /// slots are occupied.
    pub fn insert(&mut self, cap: Capability) -> Result<CapHandle, CapError> {
        for (i, slot) in self.slots.iter_mut().enumerate() {
            if slot.is_none() {
                *slot = Some(cap);
                return Ok(i as CapHandle);
            }
        }
        Err(CapError::TableFull)
    }

    /// Insert a capability at a specific slot (used during task initialisation).
    ///
    /// Overwrites any existing capability at that index.  Only intended for
    /// pre-boot wiring — use [`insert`][Self::insert] at runtime.
    pub fn insert_at(&mut self, handle: CapHandle, cap: Capability) -> Result<(), CapError> {
        let slot = self
            .slots
            .get_mut(handle as usize)
            .ok_or(CapError::InvalidHandle)?;
        *slot = Some(cap);
        Ok(())
    }

    /// Look up a capability by handle without consuming it.
    pub fn get(&self, handle: CapHandle) -> Result<Capability, CapError> {
        self.slots
            .get(handle as usize)
            .and_then(|s| *s)
            .ok_or(CapError::InvalidHandle)
    }

    /// Remove and return the capability at `handle`, clearing the slot.
    pub fn remove(&mut self, handle: CapHandle) -> Result<Capability, CapError> {
        let slot = self
            .slots
            .get_mut(handle as usize)
            .ok_or(CapError::InvalidHandle)?;
        slot.take().ok_or(CapError::InvalidHandle)
    }
}

impl Default for CapabilityTable {
    fn default() -> Self {
        Self::new()
    }
}
