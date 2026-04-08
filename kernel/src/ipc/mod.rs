//! # Ownership: Keep
//! IPC mechanism is a core kernel primitive — message passing, capabilities, and notifications must remain ring-0.
//!
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
//! - Userspace IPC via the syscall gate (syscall numbers `0x1100`–`0x1109`;
//!   earlier phases used low numbers 4 and 7, remapped in Phase 50).
//! - Capability validation per syscall.
//! - IRQ registration via notification capabilities.
//!
//! Deferred to Phase 7+: capability grants via IPC, page-capability bulk
//! transfers, IPC timeouts.

pub mod capability;
pub mod cleanup;
pub mod endpoint;
pub mod message;
pub mod notification;
pub mod registry;

pub use capability::{CapError, CapHandle, Capability, CapabilityTable};
pub use endpoint::EndpointId;
pub use message::Message;
#[allow(unused_imports)]
pub use notification::NotifId;
#[allow(unused_imports)]
pub use registry::RegistryError;

// ---------------------------------------------------------------------------
// Syscall dispatcher
// ---------------------------------------------------------------------------

/// IPC syscall dispatcher, called from `arch::x86_64::syscall::syscall_handler`.
///
/// Userspace syscall numbers `0x1100`–`0x1109` are translated to internal
/// dispatch numbers 1–10 by `handle_ipc_syscall` in `syscall/ipc.rs`.
///
/// | Internal | Userspace | Operation | Args (SysV: rdi=arg0, rsi=arg1, rdx=arg2) |
/// |---|---|---|---|
/// | 1 | 0x1100 | `ipc_recv(ep_cap)` | `arg0 = ep_cap_handle` |
/// | 2 | 0x1101 | `ipc_send(ep_cap, label, data0)` | `arg0..2` |
/// | 3 | 0x1102 | `ipc_call(ep_cap, label, data0)` | `arg0..2` |
/// | 4 | 0x1103 | `ipc_reply(reply_cap, label, data0)` | `arg0..2` |
/// | 5 | 0x1104 | `ipc_reply_recv(reply_cap, label, ep_cap)` | `arg0..2` — ep_cap in arg2 |
/// | 6 | 0x1105 | `sys_cap_grant(source_handle, target_task_id)` | `arg0, arg1` |
/// | 7 | 0x1106 | `notify_wait(notif_cap)` | `arg0 = notif_cap_handle` |
/// | 8 | 0x1107 | `notify_signal(notif_cap, bits)` | `arg0, arg1` |
/// | 9 | 0x1108 | `ipc_register_service(ep_cap, name_ptr, name_len)` | `arg0..2` |
/// | 10 | 0x1109 | `ipc_lookup_service(name_ptr, name_len)` | `arg0, arg1` → new CapHandle |
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
/// - `sys_cap_grant` (6): returns the new `CapHandle` as `u64` on success,
///   or `u64::MAX` on error (invalid handle, target not found, table full).
/// - `notify_signal` (8): returns `0` on success, `u64::MAX` on error.
/// - `ipc_register_service` (9): returns `0` on success, `u64::MAX` on error.
/// - `ipc_lookup_service` (10): returns the new `CapHandle` as `u64` on
///   success, or `u64::MAX` on error (not found, cap table full, etc.).
pub fn dispatch(number: u64, arg0: u64, arg1: u64, arg2: u64, _arg3: u64, _arg4: u64) -> u64 {
    use crate::task::{TaskId, scheduler};

    // notify_wait (7) errors return 0; all other IPC errors return u64::MAX.
    let err_val = if number == 7 { 0 } else { u64::MAX };

    let task_id = match scheduler::current_task_id() {
        Some(id) => id,
        None => return err_val,
    };

    // Syscall 10 (ipc_lookup_service): arg0 is a name pointer, not a cap
    // handle.  Handle it before the cap-lookup preamble that applies to all
    // other syscalls in this dispatcher.
    if number == 10 {
        return ipc_lookup_service(task_id, arg0, arg1);
    }

    // Range-check arg0 before casting to CapHandle (u32) to prevent
    // truncation wrap-around: a userspace caller passing arg0 = 0x1_0000_0000
    // would silently become handle 0, bypassing intended handle validation.
    if arg0 > u64::from(u32::MAX) {
        return err_val;
    }

    // Look up the capability for arg0 (the primary handle).
    let cap = match scheduler::task_cap(task_id, arg0 as CapHandle) {
        Ok(c) => c,
        Err(_) => return err_val,
    };

    match number {
        6 => {
            // sys_cap_grant(source_handle, target_task_id)
            // `cap` was already looked up from arg0 above — we know it's valid.
            // Now remove it from the caller and insert into the target.
            let target_id = TaskId(arg1);

            // Validate target task exists by trying to look up handle 0 in its
            // cap table.  If the task doesn't exist, task_cap returns
            // InvalidHandle — but that could also mean slot 0 is empty.
            // Instead, try a remove + insert sequence: remove from caller
            // first, attempt insert into target, and roll back on failure.
            let removed = match scheduler::remove_task_cap(task_id, arg0 as CapHandle) {
                Ok(c) => c,
                Err(_) => return u64::MAX,
            };

            match scheduler::insert_cap(target_id, removed) {
                Ok(new_handle) => {
                    log::trace!(
                        "[ipc] sys_cap_grant: task {} -> task {} (new handle {})",
                        task_id.0,
                        target_id.0,
                        new_handle,
                    );
                    u64::from(new_handle)
                }
                Err(_) => {
                    // Roll back: re-insert into the caller's table at the
                    // original slot so there are no side effects.
                    //
                    // NOTE: The remove+insert sequence is not atomic across
                    // capability tables — another core can briefly observe
                    // the capability absent from the source.  A future
                    // improvement could hold the scheduler lock across the
                    // entire grant operation.
                    if let Err(e) = scheduler::insert_cap_at(task_id, arg0 as CapHandle, removed) {
                        log::error!(
                            "[ipc] sys_cap_grant: CRITICAL rollback failed for task {} handle {} ({:?}) — capability lost",
                            task_id.0,
                            arg0,
                            e,
                        );
                    }
                    u64::MAX
                }
            }
        }
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
            // Range-check arg2 (ep_cap handle) before casting to CapHandle.
            if arg2 > u64::from(u32::MAX) {
                return u64::MAX;
            }
            // Validate the endpoint handle carried in arg2.
            let ep_id = match scheduler::task_cap(task_id, arg2 as CapHandle) {
                Ok(Capability::Endpoint(id)) => id,
                _ => return u64::MAX,
            };
            // Consume reply cap (arg0 already range-checked above).
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
        9 => {
            // ipc_register_service(ep_cap_handle, name_ptr, name_len)
            match cap {
                Capability::Endpoint(ep_id) => ipc_register_service(ep_id, arg1, arg2),
                _ => u64::MAX,
            }
        }
        _ => u64::MAX,
    }
}

// ---------------------------------------------------------------------------
// Service registry syscall helpers
// ---------------------------------------------------------------------------

/// Syscall 9: register a named endpoint in the service registry.
///
/// `name_ptr` is a userspace virtual address pointing to `name_len` bytes of
/// UTF-8. The name is safely copied from the caller's address space via
/// `copy_from_user`. Invalid or unmapped pointers return an error.
fn ipc_register_service(ep_id: EndpointId, name_ptr: u64, name_len: u64) -> u64 {
    if name_ptr == 0 {
        return u64::MAX;
    }
    if name_len > 32 {
        return u64::MAX;
    }
    let name_len = name_len as usize;
    let mut name_buf = [0u8; 32];
    if crate::mm::user_mem::copy_from_user(&mut name_buf[..name_len], name_ptr).is_err() {
        return u64::MAX;
    }
    let name = match core::str::from_utf8(&name_buf[..name_len]) {
        Ok(s) => s,
        Err(_) => return u64::MAX,
    };
    match registry::register(name, ep_id) {
        Ok(()) => 0,
        Err(_) => u64::MAX,
    }
}

/// Syscall 10: look up a named endpoint and insert it into the caller's cap table.
///
/// `name_ptr` is a userspace virtual address pointing to `name_len` bytes of
/// UTF-8. The name is safely copied from the caller's address space via
/// `copy_from_user`. Invalid or unmapped pointers return an error.
///
/// Returns the new [`CapHandle`] cast to `u64`, or `u64::MAX` on any error.
fn ipc_lookup_service(task_id: crate::task::TaskId, name_ptr: u64, name_len: u64) -> u64 {
    if name_ptr == 0 {
        return u64::MAX;
    }
    if name_len > 32 {
        return u64::MAX;
    }
    let name_len = name_len as usize;
    let mut name_buf = [0u8; 32];
    if crate::mm::user_mem::copy_from_user(&mut name_buf[..name_len], name_ptr).is_err() {
        return u64::MAX;
    }
    let name = match core::str::from_utf8(&name_buf[..name_len]) {
        Ok(s) => s,
        Err(_) => return u64::MAX,
    };
    let ep_id = match registry::lookup(name) {
        Some(id) => id,
        None => return u64::MAX,
    };
    match crate::task::scheduler::insert_cap(task_id, Capability::Endpoint(ep_id)) {
        Ok(handle) => u64::from(handle),
        Err(_) => u64::MAX,
    }
}
