//! IPC resource cleanup on task exit.

use crate::task::TaskId;
use crate::task::scheduler;

use super::Message;
use super::endpoint::{ENDPOINTS, EndpointId};
use super::notification;

/// Clean up all IPC state for a dying task.
///
/// - Removes the task from all endpoint sender and receiver queues
/// - Wakes any peers still blocked on endpoints **owned** by this task
/// - Releases notification capabilities and waiter slots held by the dying task
///
/// When the dying task is the current thread, call this **before** closing FDs
/// in `do_full_process_exit` so IPC peers see the error promptly. Deferred
/// dead-task sweeps also reuse this once a remotely-killed thread is quiesced.
pub fn cleanup_task_ipc(task_id: TaskId) {
    // 1. Remove any service registry entries owned by the dying task before
    //    closing endpoints so new lookups stop before the endpoint turns into
    //    a tombstone.
    super::registry::remove_by_owner(task_id.0);

    let reply_waiters = scheduler::reply_waiters(task_id);
    let notif_ids = scheduler::task_notification_caps(task_id);

    // 2. Clean up endpoint queues and close any user-created endpoints owned
    //    by the dying task.
    //
    // Walk every endpoint and remove the dying task from both sender and
    // receiver queues.  For senders with `wants_reply == true` (i.e. the
    // dying task was in a `call`), the pending send is simply dropped —
    // the task will never consume a reply anyway.
    // After draining queues, close endpoints owned by this task so stale caps
    // fail cleanly. Any now-unreferenced tombstones are reclaimed after we
    // drop the ENDPOINTS lock.
    let (stranded_senders, stranded_receivers, reclaim_candidates) = {
        let mut reg = ENDPOINTS.lock();
        let slot_count = reg.slot_count();
        for i in 0..slot_count {
            let ep_id = EndpointId(i as u8);
            if let Some(ep) = reg.get_mut(ep_id) {
                // Remove dying task from receivers queue.
                ep.receivers.retain(|&r| r != task_id);

                // Remove dying task's pending sends.
                ep.senders.retain(|ps| ps.task != task_id);
            }
        }
        let (stranded_senders, stranded_receivers) = reg.close_owned_by(task_id);
        let reclaim_candidates = reg.closed_ids();
        (stranded_senders, stranded_receivers, reclaim_candidates)
    };

    // Wake stranded peers outside the ENDPOINTS lock. Everyone blocked on a
    // closed endpoint gets an error sentinel so their syscall returns an
    // explicit failure instead of looking like success.
    let error_msg = Message::new(u64::MAX);
    for stranded in stranded_senders {
        if let Some(cap) = stranded.cap
            && let Err(err) = scheduler::insert_cap(stranded.task, cap)
        {
            log::error!(
                "[ipc] cleanup_task_ipc: failed to restore stranded cap to task {}: {:?}",
                stranded.task.0,
                err,
            );
        }
        let _ = scheduler::take_bulk_data(stranded.task);
        scheduler::deliver_message(stranded.task, error_msg);
        let _ = scheduler::wake_task(stranded.task);
    }
    for task in stranded_receivers {
        scheduler::deliver_message(task, error_msg);
        let _ = scheduler::wake_task(task);
    }

    for caller in reply_waiters {
        scheduler::deliver_message(caller, error_msg);
        let _ = scheduler::wake_task(caller);
    }

    let reclaimable: alloc::vec::Vec<_> = reclaim_candidates
        .into_iter()
        .filter(|&ep_id| !scheduler::other_task_holds_endpoint_cap(task_id, ep_id))
        .collect();
    if !reclaimable.is_empty() {
        let mut reg = ENDPOINTS.lock();
        for ep_id in reclaimable {
            if reg.queued_message_holds_endpoint(ep_id) {
                continue;
            }
            let should_destroy = matches!(
                reg.get_mut(ep_id),
                Some(ep) if ep.closed && ep.owner.is_none()
            );
            if should_destroy {
                reg.destroy(ep_id);
            }
        }
    }

    // 3. Release notifications owned by the dying task and clear any waiter
    //    slots that still mention it.
    for notif_id in notif_ids {
        notification::release(notif_id);
    }
    notification::clear_waiter(task_id);
    // Clear any persistent bound-notification entry for the dying task after
    // waiter teardown so signal() cannot observe a stale waiter for an already
    // unbound task.
    cleanup_task_bound_notif(task_id);
    scheduler::mark_ipc_cleaned(task_id);

    log::trace!(
        "[ipc] cleanup_task_ipc: cleaned up IPC state for task {}",
        task_id.0
    );
}

pub(crate) fn cleanup_task_bound_notif(task_id: TaskId) {
    if let Some(task_sched_idx) = scheduler::task_idx_for_task_id(task_id) {
        notification::clear_bound_task(task_sched_idx);
    }
}

#[cfg(test)]
mod tests {
    use super::cleanup_task_bound_notif;
    use crate::ipc::notification::{NotifId, TCB_BOUND_NOTIF, bind_task, clear_bound_task};
    use crate::task::{TaskId, scheduler};
    use core::sync::atomic::Ordering;

    #[test_case]
    fn cleanup_task_bound_notif_uses_scheduler_lookup_and_clears_binding() {
        const TASK_IDX: usize = 53;
        let task_id = TaskId(0xBEEF);
        let notif_id = NotifId(53);

        scheduler::install_test_task_idx(task_id, TASK_IDX);
        clear_bound_task(TASK_IDX);

        bind_task(notif_id, TASK_IDX).expect("bind_task must succeed on a free slot");
        assert_eq!(
            TCB_BOUND_NOTIF[TASK_IDX].load(Ordering::Acquire),
            notif_id.0,
            "bind_task must publish the task-side binding before cleanup",
        );

        cleanup_task_bound_notif(task_id);

        assert_eq!(
            TCB_BOUND_NOTIF[TASK_IDX].load(Ordering::Acquire),
            0xff,
            "cleanup seam must clear the task-side binding through the shared path",
        );
        bind_task(notif_id, TASK_IDX)
            .expect("cleanup seam must free the notif-side binding for a later rebind");

        // leave the global slot clean for subsequent tests
        clear_bound_task(TASK_IDX);
    }
}
