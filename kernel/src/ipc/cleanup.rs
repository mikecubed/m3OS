//! IPC resource cleanup on task exit.

use crate::task::TaskId;

use super::endpoint::{ENDPOINTS, EndpointId};
use super::notification;

/// Clean up all IPC state for a dying task.
///
/// - Removes the task from all endpoint sender and receiver queues
/// - Clears notification waiter slots for the dying task
///
/// Must be called **before** closing FDs in `do_full_process_exit` so that
/// IPC peers see the error promptly.
pub fn cleanup_task_ipc(task_id: TaskId) {
    // 1. Clean up endpoint queues.
    //
    // Walk every endpoint and remove the dying task from both sender and
    // receiver queues.  For senders with `wants_reply == true` (i.e. the
    // dying task was in a `call`), the pending send is simply dropped —
    // the task will never consume a reply anyway.
    {
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
    }

    // 2. Clean up notification waiters.
    notification::clear_waiter(task_id);

    // 3. Remove any service registry entries owned by the dying task so
    //    that a restarted service can re-register the same name.
    super::registry::remove_by_owner(task_id.0);

    log::trace!(
        "[ipc] cleanup_task_ipc: cleaned up IPC state for task {}",
        task_id.0
    );
}
