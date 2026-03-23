//! IPC Core — Phase 6.
//!
//! Provides the building blocks for microkernel inter-process communication:
//!
//! - [`message`] — the [`Message`] type carried through a rendezvous
//! - [`capability`] — per-process capability tables and handle validation
//! - [`endpoint`] — synchronous rendezvous endpoints (`send`, `recv`, `call`,
//!   `reply`, `reply_recv`)
//! - [`notification`] — asynchronous notification objects for IRQ delivery
//!
//! # IPC model
//!
//! Synchronous rendezvous: sender and receiver must both be ready.  The kernel
//! copies the message directly through registers — no buffering, no heap
//! allocation on the hot path.  When only one party is ready, the other blocks
//! and the scheduler picks the next ready task.
//!
//! Notification objects handle the one genuinely asynchronous pattern: IRQ
//! delivery.  An interrupt handler calls [`notification::signal_irq`], which
//! atomically sets a bit in the lock-free `PENDING` array and signals a
//! reschedule — no spinlock is acquired in the ISR path.
//!
//! # Phase 6 scope
//!
//! - Kernel-thread IPC (kernel tasks call into the IPC subsystem directly).
//! - Userspace IPC via the syscall gate (syscall numbers 1–5, 7–8).
//! - Capability validation per syscall.
//! - IRQ registration via notification capabilities.
//!
//! Deferred to Phase 7+: capability grants via IPC, page-capability bulk
//! transfers, IPC timeouts.

pub mod capability;
pub mod endpoint;
pub mod message;
pub mod notification;

pub use capability::{CapError, CapHandle, Capability, CapabilityTable};
pub use endpoint::EndpointId;
pub use message::Message;
pub use notification::NotifId;

// ---------------------------------------------------------------------------
// Syscall dispatcher
// ---------------------------------------------------------------------------

/// IPC syscall dispatcher, called from `arch::x86_64::syscall::syscall_handler`.
///
/// | Number | Operation | Args (SysV: rdi=arg0, rsi=arg1, rdx=arg2) |
/// |---|---|---|
/// | 1 | `ipc_recv(ep_cap)` | `arg0 = ep_cap_handle` |
/// | 2 | `ipc_send(ep_cap, label, data0)` | `arg0..2` |
/// | 3 | `ipc_call(ep_cap, label, data0)` | `arg0..2` |
/// | 4 | `ipc_reply(reply_cap, label, data0)` | `arg0..2` |
/// | 5 | `ipc_reply_recv(reply_cap, label, ep_cap)` | `arg0..2` — ep_cap in arg2 |
/// | 7 | `notify_wait(notif_cap)` | `arg0 = notif_cap_handle` |
/// | 8 | `notify_signal(notif_cap, bits)` | `arg0, arg1` |
///
/// Syscall 5 (`ipc_reply_recv`) uses only 3 args (reply_cap, label, ep_cap)
/// because the syscall ABI currently forwards only 3 arguments through the
/// assembly stub.  The ep_cap is packed into arg2; the reply's data payload
/// is not included in the syscall form (kernel threads use the Rust API directly).
///
/// Error convention (per-syscall):
/// - `ipc_recv` (1), `ipc_call` (3), `ipc_reply_recv` (5): return the message
///   label on success, or `u64::MAX` on error.
/// - `ipc_send` (2), `ipc_reply` (4): return `0` on success, or `u64::MAX`
///   on error (invalid handle, wrong capability type).
/// - `notify_wait` (7): returns the pending-bit word on success, or `0` on
///   error (invalid handle or wrong type).  Note: `0` cannot be a valid
///   notification word since `wait` only returns when at least one bit is set.
/// - `notify_signal` (8): returns `0` on success, `u64::MAX` on error.
pub fn dispatch(number: u64, arg0: u64, arg1: u64, arg2: u64, _arg3: u64, _arg4: u64) -> u64 {
    use crate::task::scheduler;

    // notify_wait (7) errors return 0; all other IPC errors return u64::MAX.
    let err_val = if number == 7 { 0 } else { u64::MAX };

    let task_id = match scheduler::current_task_id() {
        Some(id) => id,
        None => return err_val,
    };

    // Look up the capability for arg0 (the primary handle).
    let cap = match scheduler::task_cap(task_id, arg0 as CapHandle) {
        Ok(c) => c,
        Err(_) => return err_val,
    };

    match number {
        1 => {
            // ipc_recv(ep_cap_handle)
            match cap {
                Capability::Endpoint(ep_id) => endpoint::recv(task_id, ep_id),
                _ => u64::MAX,
            }
        }
        2 => {
            // ipc_send(ep_cap_handle, label, data0)
            match cap {
                Capability::Endpoint(ep_id) => {
                    let msg = message::Message::with2(arg1, arg2, 0);
                    if endpoint::send(task_id, ep_id, msg) {
                        0
                    } else {
                        u64::MAX
                    }
                }
                _ => u64::MAX,
            }
        }
        3 => {
            // ipc_call(ep_cap_handle, label, data0)
            match cap {
                Capability::Endpoint(ep_id) => {
                    let msg = message::Message::with2(arg1, arg2, 0);
                    endpoint::call(task_id, ep_id, msg)
                }
                _ => u64::MAX,
            }
        }
        4 => {
            // ipc_reply(reply_cap_handle, label, data0)
            match cap {
                Capability::Reply(caller_id) => {
                    // Consume the one-shot reply cap before replying.
                    let _ = scheduler::remove_task_cap(task_id, arg0 as CapHandle);
                    let reply = message::Message::with2(arg1, arg2, 0);
                    endpoint::reply(caller_id, reply);
                    0
                }
                _ => u64::MAX,
            }
        }
        5 => {
            // ipc_reply_recv(reply_cap_handle, label, ep_cap_handle)
            // ep_cap is in arg2 (the third syscall argument), fitting the 3-arg
            // limit of the current syscall asm stub.
            let caller_id = match cap {
                Capability::Reply(id) => id,
                _ => return u64::MAX,
            };
            // Validate the endpoint handle carried in arg2.
            let ep_id = match scheduler::task_cap(task_id, arg2 as CapHandle) {
                Ok(Capability::Endpoint(id)) => id,
                _ => return u64::MAX,
            };
            // Consume reply cap.
            let _ = scheduler::remove_task_cap(task_id, arg0 as CapHandle);
            let reply = message::Message::new(arg1);
            endpoint::reply_recv(task_id, caller_id, ep_id, reply)
        }
        7 => {
            // notify_wait(notif_cap_handle) — errors return 0, not u64::MAX
            match cap {
                Capability::Notification(notif_id) => notification::wait(task_id, notif_id),
                _ => 0,
            }
        }
        8 => {
            // notify_signal(notif_cap_handle, bits)
            match cap {
                Capability::Notification(notif_id) => {
                    notification::signal(notif_id, arg1);
                    0
                }
                _ => u64::MAX,
            }
        }
        _ => u64::MAX,
    }
}
