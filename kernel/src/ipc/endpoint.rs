//! Synchronous rendezvous IPC endpoints.
// `send` is exercised via the syscall dispatcher; keep dead-code allowance for unused paths.
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
//! | [`recv`] | Server | Block until a sender arrives; return message label |
//! | [`recv_msg`] | Server | Block until a sender arrives; return full [`Message`] |
//! | [`send`] | Client | Block until a receiver is ready; deliver message |
//! | [`call`] | Client | `send` + block waiting for a reply; returns label only |
//! | [`call_msg`] | Client | `send` + block waiting for a reply; returns full [`Message`] |
//! | [`reply`] | Server | Deliver a reply to the blocked caller |
//! | [`reply_recv`] | Server | `reply` + immediately `recv` next message |
//! | [`reply_recv_msg`] | Server | `reply` + immediately `recv_msg` next message |
//!
//! # Phase 6 / Phase 7 implementation
//!
//! Phase 6: initial rendezvous IPC (P6-T003 through P6-T005).
//! Phase 7: adds [`recv_msg`] and [`reply_recv_msg`] so servers can access
//! the full message payload, not just the label.

extern crate alloc;

use alloc::collections::VecDeque;
use spin::Mutex;

use super::{CapError, Capability, Message};
use crate::task::{TaskId, scheduler};

pub use kernel_core::types::EndpointId;

// ---------------------------------------------------------------------------
// Global endpoint registry
// ---------------------------------------------------------------------------

/// Maximum number of concurrent IPC endpoints.
pub(super) const MAX_ENDPOINTS: usize = 16;

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
        // Manual `[None; N]` expansion: `[expr; N]` requires `Copy`, which
        // `Option<Endpoint>` does not implement (VecDeque is not Copy).
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
    pub(super) senders: VecDeque<PendingSend>,
    /// Tasks blocked waiting to *receive* a message.
    pub(super) receivers: VecDeque<TaskId>,
}

/// A task that is blocked trying to send (or `call`) on an endpoint.
pub(super) struct PendingSend {
    pub(super) task: TaskId,
    pub(super) msg: Message,
    /// `true` if this is a `call` — the sender expects a reply cap to be
    /// inserted into the server's capability table.
    pub(super) wants_reply: bool,
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

/// Receive a full message (label + data) from an endpoint.
///
/// If a sender is already waiting, dequeue it, wake it (if it used `send`
/// rather than `call`), copy its message, and return the complete [`Message`].
/// If the endpoint is for a `call`, insert a reply capability into the
/// server's table instead of waking the sender immediately.
///
/// If no sender is waiting, the calling task blocks until one arrives.
///
/// Returns the full [`Message`] on success, or a sentinel message with
/// `label = u64::MAX` on error.  Use this when the server needs the data
/// payload; use [`recv`] when only the label is needed.
pub fn recv_msg(receiver: TaskId, ep_id: EndpointId) -> Message {
    debug_assert!(
        (ep_id.0 as usize) < MAX_ENDPOINTS,
        "recv_msg: ep_id {} out of range",
        ep_id.0
    );
    let action = {
        let mut reg = ENDPOINTS.lock();
        let ep = match reg.get_mut(ep_id) {
            Some(e) => e,
            None => return Message::new(u64::MAX),
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
        Some(mut pending) => {
            // Transfer any attached capability from the sender's message.
            if transfer_cap(pending.task, receiver, &mut pending.msg).is_err() {
                log::warn!(
                    "[ipc] recv_msg: capability transfer failed, dropping message from task {}",
                    pending.task.0,
                );
                // Wake the sender with an error so it doesn't block forever.
                scheduler::deliver_message(pending.task, Message::new(u64::MAX));
                let _ = scheduler::wake_task(pending.task);
                return Message::new(u64::MAX);
            }
            if pending.wants_reply {
                // Insert reply cap BEFORE delivering the message, so the receiver
                // never sees a request it cannot reply to.
                if scheduler::insert_cap(receiver, Capability::Reply(pending.task)).is_err() {
                    log::warn!(
                        "[ipc] recv_msg: capability table full, unblocking sender without reply"
                    );
                    scheduler::deliver_message(pending.task, Message::new(u64::MAX));
                    let _ = scheduler::wake_task(pending.task);
                    return Message::new(u64::MAX);
                }
            }
            // Deliver the message to the receiver now that all caps are in place.
            scheduler::deliver_message(receiver, pending.msg);
            crate::trace::trace_event(kernel_core::trace_ring::TraceEvent::MessageDelivered {
                task_idx: receiver.0 as u32,
                ep: ep_id.0 as u32,
            });
            if !pending.wants_reply {
                let _ = scheduler::wake_task(pending.task);
            }
        }
        None => {
            // Block; sender will call deliver_message + wake_task on us.
            crate::trace::trace_event(kernel_core::trace_ring::TraceEvent::RecvBlock {
                task_idx: receiver.0 as u32,
                ep: ep_id.0 as u32,
            });
            scheduler::block_current_on_recv_unless_message();
        }
    }
    // After waking (or immediate delivery), consume the pending message.
    // None here is always an IPC/scheduler bug: the sender must call
    // deliver_message before calling wake_task.
    match scheduler::take_message(receiver) {
        Some(msg) => {
            crate::trace::trace_event(kernel_core::trace_ring::TraceEvent::RecvWake {
                task_idx: receiver.0 as u32,
                ep: ep_id.0 as u32,
            });
            msg
        }
        None => {
            debug_assert!(
                false,
                "[ipc] recv_msg: woke with no pending message — IPC logic bug"
            );
            Message::new(u64::MAX)
        }
    }
}

/// Receive a message from an endpoint.
///
/// Identical to [`recv_msg`] but returns only the message label.
/// Use [`recv_msg`] when the server needs the full data payload.
///
/// Returns the message label on success, or `u64::MAX` on error.
pub fn recv(receiver: TaskId, ep_id: EndpointId) -> u64 {
    recv_msg(receiver, ep_id).label
}

/// Send a message to an endpoint.
///
/// If a receiver is already waiting, deliver directly and wake it.
/// Otherwise, block until a receiver is ready.
///
/// Returns `true` on success (message delivered or enqueued), `false` if the
/// endpoint ID is invalid.  The syscall dispatcher propagates `false` as
/// `u64::MAX` to the caller.
pub fn send(sender: TaskId, ep_id: EndpointId, msg: Message) -> bool {
    let matched_receiver = {
        let mut reg = ENDPOINTS.lock();
        let ep = match reg.get_mut(ep_id) {
            Some(e) => e,
            None => return false,
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
            crate::trace::trace_event(kernel_core::trace_ring::TraceEvent::SendWake {
                task_idx: receiver.0 as u32,
                ep: ep_id.0 as u32,
            });
            // Best-effort wake: the receiver may still be Running if send()
            // races between the receiver enqueueing itself and actually
            // blocking.  In that case wake_task() correctly returns false
            // and the receiver will observe the delivered message and skip
            // blocking.
            let _ = scheduler::wake_task(receiver);
        }
        None => {
            // No receiver yet — we're enqueued; block until picked up.
            crate::trace::trace_event(kernel_core::trace_ring::TraceEvent::SendBlock {
                task_idx: sender.0 as u32,
                ep: ep_id.0 as u32,
            });
            scheduler::block_current_on_send();
        }
    }
    true
}

/// Call an endpoint: send a message and block waiting for a reply.
///
/// Returns the full reply [`Message`], or a sentinel message with
/// `label = u64::MAX` on error.  Use this when the caller needs the reply
/// data payload (e.g. a VFS server forwarding a fat_server reply to a client).
pub fn call_msg(caller: TaskId, ep_id: EndpointId, msg: Message) -> Message {
    let matched_receiver = {
        let mut reg = ENDPOINTS.lock();
        let ep = match reg.get_mut(ep_id) {
            Some(e) => e,
            None => return Message::new(u64::MAX),
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
            // Insert the reply cap BEFORE delivering the message so the server
            // always has a reply cap when it sees the request, or never gets
            // the request at all (consistent failure for the caller).
            if scheduler::insert_cap(receiver, Capability::Reply(caller)).is_err() {
                log::warn!("[ipc] call_msg: server capability table full, reply cap not inserted");
                // Put the receiver back at the front of the queue so it
                // remains blocked on the endpoint as if this call never arrived.
                let mut reg = ENDPOINTS.lock();
                if let Some(ep) = reg.get_mut(ep_id) {
                    ep.receivers.push_front(receiver);
                } else {
                    // Endpoint was destroyed; wake receiver to avoid leaving it blocked.
                    drop(reg);
                    let _ = scheduler::wake_task(receiver);
                }
                return Message::new(u64::MAX);
            }
            scheduler::deliver_message(receiver, msg);
            let _ = scheduler::wake_task(receiver);
        }
        None => {
            // Server not yet waiting — we're already enqueued above with
            // wants_reply=true.  Block until the server picks us up.
        }
    }
    // Block waiting for reply regardless of whether server was already waiting.
    crate::trace::trace_event(kernel_core::trace_ring::TraceEvent::CallBlock {
        task_idx: caller.0 as u32,
        ep: ep_id.0 as u32,
    });
    scheduler::block_current_on_reply_unless_message();
    // Woken by reply() — reply message was delivered into our slot.
    match scheduler::take_message(caller) {
        Some(msg) => msg,
        None => {
            debug_assert!(
                false,
                "[ipc] call_msg: woke with no reply message — IPC logic bug"
            );
            Message::new(u64::MAX)
        }
    }
}

/// Call an endpoint: send a message and block waiting for a reply.
///
/// Returns the reply message label, or `u64::MAX` on error.
/// Use [`call_msg`] when the full reply payload is needed.
pub fn call(caller: TaskId, ep_id: EndpointId, msg: Message) -> u64 {
    call_msg(caller, ep_id, msg).label
}

/// Reply to a blocked caller.
///
/// Wakes the caller task and delivers `reply_msg` to it.
/// The reply capability must have been removed by the caller before invoking.
pub fn reply(caller: TaskId, reply_msg: Message) {
    scheduler::deliver_message(caller, reply_msg);
    // ep is u32::MAX because reply() operates on a caller TaskId, not an
    // endpoint — the reply capability was already consumed by the caller.
    crate::trace::trace_event(kernel_core::trace_ring::TraceEvent::ReplyDeliver {
        caller_idx: caller.0 as u32,
        ep: u32::MAX,
    });
    // Can legitimately race with the caller still transitioning into its
    // reply-blocked state.  If that happens, the reply is already pending
    // and the caller will observe it and skip blocking.
    let _ = scheduler::wake_task(caller);
}

/// Reply to the current caller and immediately receive the next message.
///
/// Atomically: deliver reply to `caller`, then block on `ep_id` for the
/// next incoming message.  Returns the next message label.
pub fn reply_recv(server: TaskId, caller: TaskId, ep_id: EndpointId, reply_msg: Message) -> u64 {
    reply(caller, reply_msg);
    recv(server, ep_id)
}

/// Reply to the current caller and immediately receive the next full message.
///
/// Equivalent to [`reply_recv`] but returns the complete [`Message`] instead
/// of only the label.  Use this in server loops that need access to the data
/// payload of the next request.
pub fn reply_recv_msg(
    server: TaskId,
    caller: TaskId,
    ep_id: EndpointId,
    reply_msg: Message,
) -> Message {
    reply(caller, reply_msg);
    recv_msg(server, ep_id)
}

// ---------------------------------------------------------------------------
// Capability transfer helper
// ---------------------------------------------------------------------------

/// Transfer an attached capability from sender to receiver.
///
/// If `msg` carries a capability (`msg.cap` is `Some`), insert it into the
/// receiver's capability table.  On success, clear the cap from `msg` and
/// store the assigned handle index in `msg.data[3]` so the receiver can
/// discover the new capability.  On failure (receiver table full), return an
/// error; the caller should abort the send.
///
/// If `msg` has no attached cap, this is a no-op returning `Ok(())`.
fn transfer_cap(_sender: TaskId, receiver: TaskId, msg: &mut Message) -> Result<(), CapError> {
    if let Some(cap) = msg.cap.take() {
        match scheduler::insert_cap(receiver, cap) {
            Ok(handle) => {
                // Communicate the assigned handle to the receiver via data[3].
                msg.data[3] = handle as u64;
                log::trace!(
                    "[ipc] capability transferred: {:?} -> task {} (handle {})",
                    cap,
                    receiver.0,
                    handle,
                );
                crate::trace::trace_event(kernel_core::trace_ring::TraceEvent::MessageDelivered {
                    task_idx: receiver.0 as u32,
                    ep: u32::MAX, // sentinel: capability transfer, not endpoint delivery
                });
                Ok(())
            }
            Err(e) => {
                log::warn!(
                    "[ipc] capability transfer failed: receiver {} table full",
                    receiver.0,
                );
                // Put the cap back so the sender doesn't lose it.
                msg.cap = Some(cap);
                Err(e)
            }
        }
    } else {
        Ok(())
    }
}

/// Variant of `send` that also transfers an attached capability.
///
/// If the message carries a capability and the receiver's table is full,
/// the send fails and returns `false`.  The sender retains the capability.
pub fn send_with_cap(sender: TaskId, ep_id: EndpointId, mut msg: Message) -> bool {
    let matched_receiver = {
        let mut reg = ENDPOINTS.lock();
        let ep = match reg.get_mut(ep_id) {
            Some(e) => e,
            None => return false,
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
            // Transfer capability before delivering the message.
            if transfer_cap(sender, receiver, &mut msg).is_err() {
                // Put receiver back — act as if the send never happened.
                let mut reg = ENDPOINTS.lock();
                if let Some(ep) = reg.get_mut(ep_id) {
                    ep.receivers.push_front(receiver);
                }
                return false;
            }
            scheduler::deliver_message(receiver, msg);
            crate::trace::trace_event(kernel_core::trace_ring::TraceEvent::SendWake {
                task_idx: receiver.0 as u32,
                ep: ep_id.0 as u32,
            });
            let _ = scheduler::wake_task(receiver);
        }
        None => {
            crate::trace::trace_event(kernel_core::trace_ring::TraceEvent::SendBlock {
                task_idx: sender.0 as u32,
                ep: ep_id.0 as u32,
            });
            scheduler::block_current_on_send();
        }
    }
    true
}
