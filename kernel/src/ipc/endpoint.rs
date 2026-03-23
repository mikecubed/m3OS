//! Synchronous rendezvous IPC endpoints.
// Not yet wired to main.rs — suppress dead-code until integration.
#![allow(dead_code)]
//!
//! An [`Endpoint`] is a kernel object through which two tasks exchange a
//! [`Message`] synchronously.  The model is pure rendezvous: sender and
//! receiver must both be ready before the transfer completes; whichever
//! arrives first blocks until its counterpart shows up.
//!
//! # Operations
//!
//! | Function | Who calls it | Effect |
//! |---|---|---|
//! | [`recv`] | Server | Block until a sender arrives; dequeue its message |
//! | [`send`] | Client | Block until a receiver is ready; deliver message |
//! | [`call`] | Client | `send` + block waiting for a reply |
//! | [`reply`] | Server | Deliver a reply to the blocked caller |
//! | [`reply_recv`] | Server | `reply` + immediately `recv` next message |
//!
//! # Phase 6 implementation
//!
//! This module is implemented by Track A of the parallel-implementation loop.
//! See `docs/roadmap/tasks/06-ipc-core-tasks.md` tasks P6-T003 through P6-T005.

extern crate alloc;

use alloc::collections::VecDeque;
use spin::Mutex;

use super::{Capability, Message};
use crate::task::{scheduler, TaskId};

// ---------------------------------------------------------------------------
// EndpointId
// ---------------------------------------------------------------------------

/// Index into the global endpoint registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EndpointId(pub u8);

// ---------------------------------------------------------------------------
// Global endpoint registry
// ---------------------------------------------------------------------------

/// Maximum number of concurrent IPC endpoints.
const MAX_ENDPOINTS: usize = 16;

/// Global registry of all kernel IPC endpoints.
///
/// Protected by a `Mutex` — IPC operations acquire this lock briefly to
/// inspect or mutate sender/receiver queues.
pub static ENDPOINTS: Mutex<EndpointRegistry> = Mutex::new(EndpointRegistry::new());

/// Container for all [`Endpoint`] objects.
pub struct EndpointRegistry {
    slots: [Option<Endpoint>; MAX_ENDPOINTS],
}

impl EndpointRegistry {
    const fn new() -> Self {
        // SAFETY: `Option<Endpoint>` is valid at all-zero bytes since `Endpoint`
        // uses `VecDeque` which is heap-allocated and `None` encodes as zeros.
        // We use a manual const initializer — `[None; N]` requires Copy.
        EndpointRegistry {
            slots: [
                None, None, None, None, None, None, None, None, None, None, None, None, None, None,
                None, None,
            ],
        }
    }

    /// Allocate a new endpoint and return its [`EndpointId`].
    ///
    /// # Panics
    ///
    /// Panics if all 16 slots are occupied (in both debug and release builds).
    pub fn create(&mut self) -> EndpointId {
        for (i, slot) in self.slots.iter_mut().enumerate() {
            if slot.is_none() {
                *slot = Some(Endpoint::new());
                return EndpointId(i as u8);
            }
        }
        panic!("endpoint registry full");
    }

    /// Borrow a mutable reference to an endpoint.
    pub fn get_mut(&mut self, id: EndpointId) -> Option<&mut Endpoint> {
        self.slots.get_mut(id.0 as usize)?.as_mut()
    }
}

// ---------------------------------------------------------------------------
// Endpoint
// ---------------------------------------------------------------------------

/// A single IPC rendezvous point.
///
/// Tracks two queues:
/// - `senders` — tasks blocked in [`send`] or [`call`], each carrying a
///   pending [`Message`] and a flag indicating whether they expect a reply.
/// - `receivers` — tasks blocked in [`recv`], each waiting for any sender.
pub struct Endpoint {
    /// Tasks blocked waiting to *send* a message (or in `call`, also waiting
    /// for a reply afterwards).
    senders: VecDeque<PendingSend>,
    /// Tasks blocked waiting to *receive* a message.
    receivers: VecDeque<TaskId>,
}

/// A task that is blocked trying to send (or `call`) on an endpoint.
struct PendingSend {
    task: TaskId,
    msg: Message,
    /// `true` if this is a `call` — the sender expects a reply cap to be
    /// inserted into the server's capability table.
    wants_reply: bool,
}

impl Endpoint {
    const fn new() -> Self {
        Endpoint {
            senders: VecDeque::new(),
            receivers: VecDeque::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// IPC operations
// ---------------------------------------------------------------------------

/// Receive a message from an endpoint.
///
/// If a sender is already waiting, dequeue it, wake it (if it used `send`
/// rather than `call`), copy its message, and return the message label.
/// If the endpoint is for a `call`, insert a reply capability into the
/// server's table instead of waking the sender immediately.
///
/// If no sender is waiting, the calling task blocks until one arrives.
///
/// Returns the message label on success, or `u64::MAX` on error.
pub fn recv(receiver: TaskId, ep_id: EndpointId) -> u64 {
    let action = {
        let mut reg = ENDPOINTS.lock();
        let ep = match reg.get_mut(ep_id) {
            Some(e) => e,
            None => return u64::MAX,
        };
        if let Some(pending) = ep.senders.pop_front() {
            Some(pending)
        } else {
            // No sender yet — enqueue self and block.
            ep.receivers.push_back(receiver);
            None
        }
    };
    // ENDPOINTS lock is released before any scheduler calls.

    match action {
        Some(pending) => {
            // A sender was waiting — deliver message to ourselves.
            scheduler::deliver_message(receiver, pending.msg);
            if pending.wants_reply {
                // Insert a one-shot reply cap; sender stays blocked awaiting reply().
                // If the table is full, deliver an explicit error reply so the
                // sender's take_message() returns Some(u64::MAX) rather than None
                // (which would fire a misleading debug_assert in call()).
                if scheduler::insert_cap(receiver, Capability::Reply(pending.task)).is_err() {
                    log::warn!(
                        "[ipc] recv: capability table full, unblocking sender without reply"
                    );
                    scheduler::deliver_message(pending.task, Message::new(u64::MAX));
                    scheduler::wake_task(pending.task);
                }
            } else {
                scheduler::wake_task(pending.task);
            }
        }
        None => {
            // Block; sender will call deliver_message + wake_task on us.
            scheduler::block_current_on_recv();
        }
    }
    // After waking (or immediate delivery), consume the pending message.
    // None here is always an IPC/scheduler bug: the sender must call
    // deliver_message before calling wake_task.
    match scheduler::take_message(receiver) {
        Some(msg) => msg.label,
        None => {
            debug_assert!(
                false,
                "[ipc] recv: woke with no pending message — IPC logic bug"
            );
            u64::MAX
        }
    }
}

/// Send a message to an endpoint.
///
/// If a receiver is already waiting, deliver directly and wake it.
/// Otherwise, block until a receiver is ready.
pub fn send(sender: TaskId, ep_id: EndpointId, msg: Message) {
    let matched_receiver = {
        let mut reg = ENDPOINTS.lock();
        let ep = match reg.get_mut(ep_id) {
            Some(e) => e,
            None => return,
        };
        if let Some(receiver) = ep.receivers.pop_front() {
            Some(receiver)
        } else {
            ep.senders.push_back(PendingSend {
                task: sender,
                msg,
                wants_reply: false,
            });
            None
        }
    };

    match matched_receiver {
        Some(receiver) => {
            scheduler::deliver_message(receiver, msg);
            scheduler::wake_task(receiver);
        }
        None => {
            // No receiver yet — we're enqueued; block until picked up.
            scheduler::block_current_on_send();
        }
    }
}

/// Call an endpoint: send a message and block waiting for a reply.
///
/// Returns the reply message label, or `u64::MAX` on error.
pub fn call(caller: TaskId, ep_id: EndpointId, msg: Message) -> u64 {
    let matched_receiver = {
        let mut reg = ENDPOINTS.lock();
        let ep = match reg.get_mut(ep_id) {
            Some(e) => e,
            None => return u64::MAX,
        };

        if let Some(receiver) = ep.receivers.pop_front() {
            Some(receiver)
        } else {
            ep.senders.push_back(PendingSend {
                task: caller,
                msg,
                wants_reply: true,
            });
            None
        }
    };

    match matched_receiver {
        Some(receiver) => {
            scheduler::deliver_message(receiver, msg);
            // Insert reply cap into the server's table.  If that fails the
            // server would get the message but have no way to reply, stranding
            // the caller forever on BlockedOnReply.  Instead return an
            // immediate error to the caller without blocking.
            if scheduler::insert_cap(receiver, Capability::Reply(caller)).is_err() {
                log::warn!("[ipc] call: server capability table full, reply cap not inserted");
                scheduler::wake_task(receiver);
                return u64::MAX;
            }
            scheduler::wake_task(receiver);
        }
        None => {
            // Server not yet waiting — we're already enqueued above with
            // wants_reply=true.  Block until the server picks us up.
        }
    }
    // Block waiting for reply regardless of whether server was already waiting.
    scheduler::block_current_on_reply();
    // Woken by reply() — reply message was delivered into our slot.
    match scheduler::take_message(caller) {
        Some(msg) => msg.label,
        None => {
            debug_assert!(
                false,
                "[ipc] call: woke with no reply message — IPC logic bug"
            );
            u64::MAX
        }
    }
}

/// Reply to a blocked caller.
///
/// Wakes the caller task and delivers `reply_msg` to it.
/// The reply capability must have been removed by the caller before invoking.
pub fn reply(caller: TaskId, reply_msg: Message) {
    scheduler::deliver_message(caller, reply_msg);
    scheduler::wake_task(caller);
}

/// Reply to the current caller and immediately receive the next message.
///
/// Atomically: deliver reply to `caller`, then block on `ep_id` for the
/// next incoming message.  Returns the next message label.
pub fn reply_recv(server: TaskId, caller: TaskId, ep_id: EndpointId, reply_msg: Message) -> u64 {
    reply(caller, reply_msg);
    recv(server, ep_id)
}
