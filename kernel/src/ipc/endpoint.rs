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
use alloc::vec::Vec;
use spin::{Lazy, Mutex};

use super::{CapError, Capability, Message, NotifId};
use crate::task::{TaskId, scheduler};

pub use kernel_core::types::EndpointId;

// ---------------------------------------------------------------------------
// Global endpoint registry
// ---------------------------------------------------------------------------

/// Initial number of endpoint slots.
const INITIAL_ENDPOINTS: usize = 16;

/// Number of slots added each time the pool grows.
const ENDPOINT_GROW_INCREMENT: usize = 16;

/// Global registry of all kernel IPC endpoints.
///
/// Protected by a `Mutex` — IPC operations acquire this lock briefly to
/// inspect or mutate sender/receiver queues.
///
/// Uses `spin::Lazy` because `EndpointRegistry::new()` allocates a `Vec`.
pub static ENDPOINTS: Lazy<Mutex<EndpointRegistry>> =
    Lazy::new(|| Mutex::new(EndpointRegistry::new()));

/// Dynamically growable container for all [`Endpoint`] objects.
pub struct EndpointRegistry {
    slots: Vec<Option<Endpoint>>,
}

impl EndpointRegistry {
    fn new() -> Self {
        let mut slots = Vec::with_capacity(INITIAL_ENDPOINTS);
        for _ in 0..INITIAL_ENDPOINTS {
            slots.push(None);
        }
        EndpointRegistry { slots }
    }

    /// Allocate a new endpoint and return its [`EndpointId`].
    ///
    /// Scans for a free slot; if none found, grows the pool by
    /// [`ENDPOINT_GROW_INCREMENT`] and uses the first new slot.
    ///
    /// `EndpointId` wraps a `u8`, limiting the maximum to 256 endpoints.
    ///
    /// # Panics
    ///
    /// Panics if the pool would need to grow beyond 256 endpoints (the `u8`
    /// limit of `EndpointId`).
    pub fn create(&mut self) -> EndpointId {
        self.try_create().expect("endpoint registry full")
    }

    /// Fallible version of [`create`] — returns `None` when all slots are
    /// occupied instead of panicking.  Used by userspace-facing syscalls to
    /// avoid kernel DoS via endpoint exhaustion.
    pub fn try_create(&mut self) -> Option<EndpointId> {
        for (i, slot) in self.slots.iter_mut().enumerate() {
            if slot.is_none() {
                *slot = Some(Endpoint::new());
                return Some(EndpointId(i as u8));
            }
        }
        // No free slot — grow the pool.
        let old_len = self.slots.len();
        if old_len >= 256 {
            return None;
        }
        let grow_to = (old_len + ENDPOINT_GROW_INCREMENT).min(256);
        self.slots.resize_with(grow_to, || None);
        self.slots[old_len] = Some(Endpoint::new());
        Some(EndpointId(old_len as u8))
    }

    /// Like [`try_create`] but records the owning task so the endpoint can
    /// be reclaimed on task exit.  Used by the `create_endpoint` syscall.
    pub fn try_create_owned(&mut self, owner: TaskId) -> Option<EndpointId> {
        let id = self.try_create()?;
        if let Some(ep) = self.get_mut(id) {
            ep.owner = Some(owner);
        }
        Some(id)
    }

    /// Free an endpoint slot so it can be reused.
    ///
    /// Used to roll back a `try_create` when the subsequent capability insert
    /// fails, preventing permanent slot leaks from userspace syscalls.
    pub fn destroy(&mut self, id: EndpointId) {
        if let Some(slot) = self.slots.get_mut(id.0 as usize) {
            *slot = None;
        }
    }

    /// Borrow a mutable reference to an endpoint.
    pub fn get_mut(&mut self, id: EndpointId) -> Option<&mut Endpoint> {
        self.slots.get_mut(id.0 as usize)?.as_mut()
    }

    /// Return the IDs of every endpoint currently owned by `task_id`.
    ///
    /// Captured by `cleanup_task_ipc` *before* [`close_owned_by`] runs so the
    /// driver-facade death-detection hooks
    /// (`RemoteNic::on_endpoint_closed`, `RemoteBlockDevice::on_endpoint_closed`)
    /// can fire after `ENDPOINTS.lock()` is released. After `close_owned_by`
    /// the `owner` field has been cleared, so a post-close walk would miss
    /// the association.
    pub(super) fn endpoints_owned_by(&self, task_id: TaskId) -> Vec<EndpointId> {
        let mut out = Vec::new();
        for (i, slot) in self.slots.iter().enumerate() {
            if let Some(ep) = slot
                && ep.owner == Some(task_id)
            {
                out.push(EndpointId(i as u8));
            }
        }
        out
    }

    /// Close all endpoints owned by `task_id`.
    ///
    /// Closed endpoints stay tombstoned until no live task still holds a cap
    /// to their [`EndpointId`]. That prevents stale caps from aliasing a later
    /// endpoint that reuses the same slot.
    ///
    /// Returns the blocked peers stranded on those endpoints so the caller can
    /// wake them **after** releasing the registry lock.
    pub(super) fn close_owned_by(&mut self, task_id: TaskId) -> (Vec<StrandedSender>, Vec<TaskId>) {
        let mut stranded_senders = Vec::new();
        let mut stranded_receivers = Vec::new();
        for slot in self.slots.iter_mut() {
            if let Some(ep) = slot
                && ep.owner == Some(task_id)
            {
                ep.owner = None;
                ep.closed = true;
                stranded_receivers.extend(ep.receivers.drain(..));
                for mut ps in ep.senders.drain(..) {
                    stranded_senders.push(StrandedSender {
                        task: ps.task,
                        cap: ps.msg.cap.take(),
                    });
                }
            }
        }
        (stranded_senders, stranded_receivers)
    }

    /// Return the IDs of endpoints currently tombstoned by owner exit.
    pub(super) fn closed_ids(&self) -> Vec<EndpointId> {
        self.slots
            .iter()
            .enumerate()
            .filter_map(|(i, slot)| {
                slot.as_ref()
                    .filter(|ep| ep.closed)
                    .map(|_| EndpointId(i as u8))
            })
            .collect()
    }

    /// Return whether any queued send currently carries `ep_id` as an attached
    /// capability inside its message.
    pub(super) fn queued_message_holds_endpoint(&self, ep_id: EndpointId) -> bool {
        self.slots.iter().flatten().any(|ep| {
            ep.senders.iter().any(
                |pending| matches!(pending.msg.cap, Some(Capability::Endpoint(id)) if id == ep_id),
            )
        })
    }

    /// Return the current number of slots (for iteration / diagnostics).
    pub fn slot_count(&self) -> usize {
        self.slots.len()
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
    /// Owner task for user-created endpoints (`Some`), or `None` for
    /// kernel-created static endpoints.  Used for reclamation on task exit.
    pub(super) owner: Option<TaskId>,
    /// Tombstone bit set when the owner exits while foreign caps may still
    /// reference this [`EndpointId`]. Closed endpoints reject new IPC until
    /// the slot can be safely reclaimed.
    pub(super) closed: bool,
    /// Optional hook fired *after* a sender enqueues itself with no
    /// receiver waiting. Used by `net_task` to learn that the
    /// `net.nic.ingress` endpoint has work without a dedicated kernel
    /// receiver task on the run queue. Invoked outside `ENDPOINTS.lock()`
    /// so the hook may freely acquire other kernel locks (e.g.
    /// `SCHEDULER.lock()` via `wake_task`).
    pub(super) on_pending_send: Option<fn()>,
}

/// A task that is blocked trying to send (or `call`) on an endpoint.
pub(super) struct PendingSend {
    pub(super) task: TaskId,
    pub(super) msg: Message,
    /// `true` if this is a `call` — the sender expects a reply cap to be
    /// inserted into the server's capability table.
    pub(super) wants_reply: bool,
}

/// Sender stranded on an endpoint that closed while it was blocked.
pub(super) struct StrandedSender {
    pub(super) task: TaskId,
    pub(super) cap: Option<Capability>,
}

impl Endpoint {
    const fn new() -> Self {
        Endpoint {
            senders: VecDeque::new(),
            receivers: VecDeque::new(),
            owner: None,
            closed: false,
            on_pending_send: None,
        }
    }
}

/// Install a hook to invoke whenever a sender enqueues onto `ep_id` with no
/// receiver waiting. Used by `net_task` to drain the `net.nic.ingress`
/// endpoint without a dedicated kernel receiver task — see
/// `docs/post-mortems/2026-04-24-ingress-task-starvation.md` "what the real
/// fix would look like" item 3.
///
/// The hook runs outside `ENDPOINTS.lock()` so it may safely call into the
/// scheduler (e.g. `wake_task`).
pub fn set_endpoint_pending_send_hook(ep_id: EndpointId, hook: fn()) {
    let mut reg = ENDPOINTS.lock();
    if let Some(ep) = reg.get_mut(ep_id) {
        ep.on_pending_send = Some(hook);
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
    // Bounds check is done by get_mut() below — it returns None for
    // out-of-range IDs, which is handled by the match.
    let action = {
        let mut reg = ENDPOINTS.lock();
        let ep = match reg.get_mut(ep_id) {
            Some(e) if !e.closed => e,
            _ => return Message::new(u64::MAX),
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
            // Insert reply cap FIRST so it reserves a slot before transfer_cap
            // can consume the last free entry.  This prevents a scenario where
            // transfer_cap succeeds but the reply-cap insertion fails, leaving
            // an unreachable capability in the receiver's table.
            let reply_cap_handle = if pending.wants_reply {
                match scheduler::insert_cap(receiver, Capability::Reply(pending.task)) {
                    Ok(handle) => Some(handle),
                    Err(_) => {
                        log::warn!(
                            "[ipc] recv_msg: capability table full, unblocking sender without reply"
                        );
                        scheduler::deliver_message(pending.task, Message::new(u64::MAX));
                        let _ = scheduler::wake_task(pending.task);
                        return Message::new(u64::MAX);
                    }
                }
            } else {
                None
            };
            // Transfer any attached capability from the sender's message.
            if transfer_cap(pending.task, receiver, &mut pending.msg).is_err() {
                log::warn!(
                    "[ipc] recv_msg: capability transfer failed, dropping message from task {}",
                    pending.task.0,
                );
                // Remove the reply cap we just inserted to avoid a dangling cap.
                if let Some(handle) = reply_cap_handle {
                    let _ = scheduler::remove_task_cap(receiver, handle);
                }
                // Wake the sender with an error so it doesn't block forever.
                scheduler::deliver_message(pending.task, Message::new(u64::MAX));
                let _ = scheduler::wake_task(pending.task);
                return Message::new(u64::MAX);
            }
            // Phase 56 close-out — communicate the assigned reply-cap handle
            // to the receiver via `data[2]`. Userspace can use this directly
            // in `ipc_reply` instead of guessing the slot via a hardcoded
            // convention. `0` (no reply cap) signals fire-and-forget messages.
            if let Some(handle) = reply_cap_handle {
                pending.msg.data[2] = handle as u64;
            }
            // Deliver the message to the receiver now that all caps are in place.
            scheduler::deliver_message(receiver, pending.msg);
            transfer_bulk(pending.task, receiver);
            crate::trace::trace_event(kernel_core::trace_ring::TraceEvent::MessageDelivered {
                task_idx: receiver.0 as u32,
                ep: ep_id.0 as u32,
            });
            if !pending.wants_reply {
                scheduler::complete_send(pending.task);
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

/// Non-blocking receive: pop one queued sender from `ep_id`, if any, and
/// deliver its message to `receiver`.
///
/// Returns:
/// - `Some(msg)` when a sender was queued; the sender is woken (or its reply
///   cap is inserted into the receiver's table for `call`-shaped sends),
///   bulk data has been transferred, and `msg` is the full [`Message`].
/// - `None` when the senders queue is empty or the endpoint is closed/invalid.
///   The receiver is **not** enqueued and does **not** block.
///
/// Used by `net_task` to drain the `net.nic.ingress` endpoint without a
/// dedicated kernel receiver task. Replaces the Phase 55c Track E
/// `remote_nic_ingress_task` that starved PID 1's reap loop just by sitting
/// blocked on `recv_msg`.
///
/// Mirrors `recv_msg`'s sender-found path exactly. The deliver/take dance
/// is preserved so the receiver's `pending_msg` slot stays the single
/// authoritative source of an IPC message — a future blocking caller would
/// observe a consistent state if a wake races with a delivery.
pub fn recv_msg_nowait(receiver: TaskId, ep_id: EndpointId) -> Option<Message> {
    let mut pending = {
        let mut reg = ENDPOINTS.lock();
        let ep = reg.get_mut(ep_id).filter(|e| !e.closed)?;
        ep.senders.pop_front()?
    };

    let reply_cap_handle = if pending.wants_reply {
        match scheduler::insert_cap(receiver, Capability::Reply(pending.task)) {
            Ok(handle) => Some(handle),
            Err(_) => {
                log::warn!(
                    "[ipc] recv_msg_nowait: capability table full, unblocking sender without reply"
                );
                scheduler::deliver_message(pending.task, Message::new(u64::MAX));
                let _ = scheduler::wake_task(pending.task);
                return Some(Message::new(u64::MAX));
            }
        }
    } else {
        None
    };

    if transfer_cap(pending.task, receiver, &mut pending.msg).is_err() {
        log::warn!(
            "[ipc] recv_msg_nowait: capability transfer failed, dropping message from task {}",
            pending.task.0,
        );
        if let Some(handle) = reply_cap_handle {
            let _ = scheduler::remove_task_cap(receiver, handle);
        }
        scheduler::deliver_message(pending.task, Message::new(u64::MAX));
        let _ = scheduler::wake_task(pending.task);
        return Some(Message::new(u64::MAX));
    }

    // Phase 56 close-out — communicate the assigned reply-cap handle to the
    // receiver via `data[2]` (mirrors the same convention in `recv_msg`).
    if let Some(handle) = reply_cap_handle {
        pending.msg.data[2] = handle as u64;
    }

    scheduler::deliver_message(receiver, pending.msg);
    transfer_bulk(pending.task, receiver);
    crate::trace::trace_event(kernel_core::trace_ring::TraceEvent::MessageDelivered {
        task_idx: receiver.0 as u32,
        ep: ep_id.0 as u32,
    });
    if !pending.wants_reply {
        scheduler::complete_send(pending.task);
        let _ = scheduler::wake_task(pending.task);
    }

    scheduler::take_message(receiver)
}

/// Receive a message or a notification on an endpoint.
///
/// Extends [`recv_msg`] with a bound-notification fast path: if the calling
/// task has a notification bound and its `PENDING` bits are non-zero, drains
/// them atomically and returns `(1, notification_bits)` without touching the
/// endpoint queue.
///
/// # Return value
///
/// Returns a tuple `(kind: u8, msg: Message)`:
/// - `(0, msg)` — a peer delivered a message; `msg` is the full [`Message`].
/// - `(1, bits_msg)` — a bound notification fired; `bits_msg.data[0]` carries
///   the drained bit mask and `bits_msg.label = 0`.
///
/// # Lock order
///
/// When both locks must be held simultaneously, the canonical order is
/// `ENDPOINTS` first, then notification `WAITERS`. This order must never be
/// reversed.
///
/// In the registration-window cleanup path (`register_recv_waiter` returns
/// `Some`), `WAITERS` is acquired and released entirely inside
/// `register_recv_waiter` before this function re-acquires `ENDPOINTS` to
/// remove the receiver from the queue. The two locks are therefore **not** held
/// simultaneously in that path, and no lock-order inversion occurs.
pub fn recv_msg_with_notif(
    receiver: TaskId,
    ep_id: EndpointId,
    notif_id: NotifId,
) -> (u8, Message) {
    use super::notification;
    use kernel_core::ipc::wake_kind::{RECV_KIND_MESSAGE, RECV_KIND_NOTIFICATION, classify_recv};

    let bits = notification::drain_bits(notif_id);
    if classify_recv(bits) == RECV_KIND_NOTIFICATION {
        let mut msg = Message::new(0);
        msg.data[0] = bits;
        return (RECV_KIND_NOTIFICATION, msg);
    }

    let action = {
        let mut reg = ENDPOINTS.lock();
        let ep = match reg.get_mut(ep_id) {
            Some(e) if !e.closed => e,
            _ => return (RECV_KIND_MESSAGE, Message::new(u64::MAX)),
        };
        if let Some(pending) = ep.senders.pop_front() {
            Some(pending)
        } else {
            ep.receivers.push_back(receiver);
            None
        }
    };

    match action {
        Some(mut pending) => {
            let reply_cap_handle = if pending.wants_reply {
                match scheduler::insert_cap(receiver, Capability::Reply(pending.task)) {
                    Ok(handle) => Some(handle),
                    Err(_) => {
                        log::warn!("[ipc] recv_msg_with_notif: cap table full");
                        scheduler::deliver_message(pending.task, Message::new(u64::MAX));
                        let _ = scheduler::wake_task(pending.task);
                        return (RECV_KIND_MESSAGE, Message::new(u64::MAX));
                    }
                }
            } else {
                None
            };

            if transfer_cap(pending.task, receiver, &mut pending.msg).is_err() {
                if let Some(handle) = reply_cap_handle {
                    let _ = scheduler::remove_task_cap(receiver, handle);
                }
                scheduler::deliver_message(pending.task, Message::new(u64::MAX));
                let _ = scheduler::wake_task(pending.task);
                return (RECV_KIND_MESSAGE, Message::new(u64::MAX));
            }

            // Phase 56 close-out — communicate the assigned reply-cap handle
            // to the receiver via `data[2]` (mirrors the same convention in
            // `recv_msg` and `recv_msg_nowait`).
            if let Some(handle) = reply_cap_handle {
                pending.msg.data[2] = handle as u64;
            }

            scheduler::deliver_message(receiver, pending.msg);
            transfer_bulk(pending.task, receiver);
            if !pending.wants_reply {
                scheduler::complete_send(pending.task);
                let _ = scheduler::wake_task(pending.task);
            }

            match scheduler::take_message(receiver) {
                Some(msg) => (RECV_KIND_MESSAGE, msg),
                None => (RECV_KIND_MESSAGE, Message::new(u64::MAX)),
            }
        }
        None => {
            let task_sched_idx = match scheduler::get_current_task_idx() {
                Some(idx) => idx,
                None => {
                    let mut reg = ENDPOINTS.lock();
                    if let Some(ep) = reg.get_mut(ep_id) {
                        ep.receivers.retain(|&r| r != receiver);
                    }
                    return (RECV_KIND_MESSAGE, Message::new(u64::MAX));
                }
            };

            if let Some(bits2) =
                notification::register_recv_waiter(notif_id, receiver, task_sched_idx)
            {
                let mut reg = ENDPOINTS.lock();
                if let Some(ep) = reg.get_mut(ep_id) {
                    ep.receivers.retain(|&r| r != receiver);
                }
                let mut msg = Message::new(0);
                msg.data[0] = bits2;
                return (RECV_KIND_NOTIFICATION, msg);
            }

            scheduler::block_current_on_notif_unless_message();
            notification::unregister_recv_waiter(notif_id, receiver);

            if let Some(msg) = scheduler::take_message(receiver) {
                return (RECV_KIND_MESSAGE, msg);
            }

            let bits = notification::drain_bits(notif_id);
            if bits != 0 {
                {
                    let mut reg = ENDPOINTS.lock();
                    if let Some(ep) = reg.get_mut(ep_id) {
                        ep.receivers.retain(|&r| r != receiver);
                    }
                }
                if let Some(msg) = scheduler::take_message(receiver) {
                    notification::signal(notif_id, bits);
                    return (RECV_KIND_MESSAGE, msg);
                }
                let mut msg = Message::new(0);
                msg.data[0] = bits;
                (RECV_KIND_NOTIFICATION, msg)
            } else {
                debug_assert!(false, "[ipc] recv_msg_with_notif: spurious wake");
                (RECV_KIND_MESSAGE, Message::new(u64::MAX))
            }
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

/// Transfer any pending bulk data from `src` to `dst` task.
///
/// Called alongside `deliver_message` to move the sender's bulk payload to
/// the receiver's slot.  No-op if no bulk data is pending.
fn transfer_bulk(src: TaskId, dst: TaskId) {
    if let Some(bulk) = scheduler::take_bulk_data(src) {
        scheduler::deliver_bulk(dst, bulk);
    }
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
    let (matched_receiver, pending_hook) = {
        let mut reg = ENDPOINTS.lock();
        let ep = match reg.get_mut(ep_id) {
            Some(e) if !e.closed => e,
            _ => return false,
        };
        if let Some(receiver) = ep.receivers.pop_front() {
            (Some(receiver), None)
        } else {
            ep.senders.push_back(PendingSend {
                task: sender,
                msg,
                wants_reply: false,
            });
            (None, ep.on_pending_send)
        }
    };

    match matched_receiver {
        Some(receiver) => {
            scheduler::deliver_message(receiver, msg);
            transfer_bulk(sender, receiver);
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
            // If a pending-send hook is installed (e.g. `net_task` watches
            // `net.nic.ingress`), invoke it before blocking so the hook
            // owner is woken to drain the queue.
            if let Some(hook) = pending_hook {
                hook();
            }
            crate::trace::trace_event(kernel_core::trace_ring::TraceEvent::SendBlock {
                task_idx: sender.0 as u32,
                ep: ep_id.0 as u32,
            });
            scheduler::block_current_on_send_unless_completed();
            if let Some(msg) = scheduler::take_message(sender) {
                debug_assert!(
                    msg.label == u64::MAX,
                    "[ipc] send: woke with unexpected pending message"
                );
                return false;
            }
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
    let (matched_receiver, pending_hook) = {
        let mut reg = ENDPOINTS.lock();
        let ep = match reg.get_mut(ep_id) {
            Some(e) if !e.closed => e,
            _ => return Message::new(u64::MAX),
        };

        if let Some(receiver) = ep.receivers.pop_front() {
            (Some(receiver), None)
        } else {
            ep.senders.push_back(PendingSend {
                task: caller,
                msg,
                wants_reply: true,
            });
            (None, ep.on_pending_send)
        }
    };
    if matched_receiver.is_none()
        && let Some(hook) = pending_hook
    {
        hook();
    }

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
                if let Some(ep) = reg.get_mut(ep_id)
                    && !ep.closed
                {
                    ep.receivers.push_front(receiver);
                } else {
                    // Endpoint was closed or destroyed; wake receiver with an
                    // explicit IPC error so it does not remain stranded.
                    drop(reg);
                    scheduler::deliver_message(receiver, Message::new(u64::MAX));
                    let _ = scheduler::wake_task(receiver);
                }
                return Message::new(u64::MAX);
            }
            scheduler::deliver_message(receiver, msg);
            transfer_bulk(caller, receiver);
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

/// Remove a task from any endpoint sender/receiver wait queues.
///
/// Used when signal delivery needs to interrupt a task blocked inside IPC so it
/// can return to userspace and observe the pending signal.
pub fn cancel_task_wait(task_id: TaskId) {
    let mut reg = ENDPOINTS.lock();
    let slot_count = reg.slot_count();
    for i in 0..slot_count {
        if let Some(ep) = reg.get_mut(EndpointId(i as u8)) {
            ep.receivers.retain(|&receiver| receiver != task_id);
            ep.senders.retain(|pending| pending.task != task_id);
        }
    }
}

/// Reply to a blocked caller.
///
/// Wakes the caller task and delivers `reply_msg` to it.  If the `server`
/// task has pending bulk data (set via `ipc_store_reply_bulk`), it is
/// transferred to the caller alongside the message (Phase 54).
///
/// The reply capability must have been removed by the caller before invoking.
pub fn reply(server: TaskId, caller: TaskId, reply_msg: Message) {
    // Phase 54: transfer any reply bulk data from server → caller.
    transfer_bulk(server, caller);
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
    reply(server, caller, reply_msg);
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
    reply(server, caller, reply_msg);
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
    let (matched_receiver, pending_hook) = {
        let mut reg = ENDPOINTS.lock();
        let ep = match reg.get_mut(ep_id) {
            Some(e) if !e.closed => e,
            _ => return false,
        };
        if let Some(receiver) = ep.receivers.pop_front() {
            (Some(receiver), None)
        } else {
            ep.senders.push_back(PendingSend {
                task: sender,
                msg,
                wants_reply: false,
            });
            (None, ep.on_pending_send)
        }
    };
    if matched_receiver.is_none()
        && let Some(hook) = pending_hook
    {
        hook();
    }

    match matched_receiver {
        Some(receiver) => {
            // Transfer capability before delivering the message.
            if transfer_cap(sender, receiver, &mut msg).is_err() {
                // Put receiver back — act as if the send never happened.
                let mut reg = ENDPOINTS.lock();
                if let Some(ep) = reg.get_mut(ep_id)
                    && !ep.closed
                {
                    ep.receivers.push_front(receiver);
                } else {
                    drop(reg);
                    scheduler::deliver_message(receiver, Message::new(u64::MAX));
                    let _ = scheduler::wake_task(receiver);
                }
                return false;
            }
            scheduler::deliver_message(receiver, msg);
            transfer_bulk(sender, receiver);
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
            scheduler::block_current_on_send_unless_completed();
            if let Some(msg) = scheduler::take_message(sender) {
                debug_assert!(
                    msg.label == u64::MAX,
                    "[ipc] send_with_cap: woke with unexpected pending message"
                );
                return false;
            }
        }
    }
    true
}
