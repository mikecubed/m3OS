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
//! - Userspace IPC via the syscall gate (syscall numbers `0x1100`–`0x110B`;
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

use crate::mm::user_mem::{UserSliceRo, UserSliceWo};

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
/// Userspace syscall numbers `0x1100`–`0x110F` are translated to internal
/// dispatch numbers 1–16 by the flat dispatch table in `arch/x86_64/syscall/mod.rs`.
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
/// | 11 | 0x110A | `create_irq_notification(irq)` | `arg0 = IRQ number` → new CapHandle |
/// | 12 | 0x110B | `create_endpoint()` | — → new CapHandle |
/// | 13 | 0x110C | `ipc_send_buf(ep_cap, label, data0, buf_ptr, buf_len)` | `arg0..4` |
/// | 14 | 0x110D | `ipc_call_buf(ep_cap, label, data0, buf_ptr, buf_len)` | `arg0..4` → label |
/// | 15 | 0x110E | `ipc_recv_msg(ep_cap, msg_ptr, buf_ptr, buf_len)` | `arg0..3` → label |
/// | 16 | 0x110F | `ipc_reply_recv_msg(reply_cap, label, ep_cap, msg_ptr, buf_ptr)` | `arg0..4` → label |
/// | 17 | 0x1110 | `ipc_store_reply_bulk(buf_ptr, buf_len)` | `arg0, arg1` → 0 or u64::MAX |
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
/// - `create_irq_notification` (11): returns the new `CapHandle` as `u64` on
///   success, or `u64::MAX` on error (invalid IRQ, cap table full, etc.).
pub fn dispatch(number: u64, arg0: u64, arg1: u64, arg2: u64, arg3: u64, arg4: u64) -> u64 {
    use crate::task::{TaskId, scheduler};

    // notify_wait (7) errors return 0; all other IPC errors return u64::MAX.
    let err_val = if number == 7 { 0 } else { u64::MAX };

    let task_id = match scheduler::current_task_id() {
        Some(id) => id,
        None => return err_val,
    };

    // Per-core syscall state (syscall_user_rsp, syscall_stack_top, FS.base)
    // is now saved/restored automatically by the scheduler via
    // UserReturnState, so blocking IPC paths no longer need manual
    // restore_caller_context calls.

    // Syscalls 10, 11, 12, and 17 do not use arg0 as a cap handle — handle them
    // before the cap-lookup preamble.
    if number == 10 {
        return ipc_lookup_service(task_id, arg0, arg1);
    }
    if number == 11 {
        return create_irq_notification(task_id, arg0);
    }
    if number == 12 {
        return ipc_create_endpoint(task_id);
    }
    if number == 17 {
        return ipc_store_reply_bulk(task_id, arg0, arg1);
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
            // Transfer it under the scheduler lock so endpoint cleanup cannot
            // observe a holderless gap and reclaim a tombstone too early.
            let target_id = TaskId(arg1);
            match scheduler::grant_task_cap(task_id, arg0 as CapHandle, target_id) {
                Ok(new_handle) => {
                    log::trace!(
                        "[ipc] sys_cap_grant: task {} -> task {} (new handle {})",
                        task_id.0,
                        target_id.0,
                        new_handle,
                    );
                    u64::from(new_handle)
                }
                Err(_) => u64::MAX,
            }
        }
        1 => {
            // ipc_recv(ep_cap_handle) — blocks until a sender arrives.
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
            // ipc_call(ep_cap_handle, label, data0) — blocks until reply.
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
                    endpoint::reply(task_id, caller_id, reply);
                    0
                }
                _ => u64::MAX,
            }
        }
        5 => {
            // ipc_reply_recv(reply_cap_handle, label, ep_cap_handle)
            // ep_cap is in arg2 (the third syscall argument), fitting the 3-arg
            // limit of the current syscall asm stub.
            // Blocks until a new message arrives on the endpoint.
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
            // notify_wait(notif_cap_handle) — blocks until bits are pending.
            // Errors return 0, not u64::MAX.
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
                Capability::Endpoint(ep_id) => ipc_register_service(task_id, ep_id, arg1, arg2),
                _ => u64::MAX,
            }
        }
        13 => {
            // ipc_send_buf(ep_cap, label, data0, buf_ptr, buf_len)
            match cap {
                Capability::Endpoint(ep_id) => {
                    let msg = message::Message::with2(arg1, arg2, 0);
                    ipc_send_with_bulk(task_id, ep_id, msg, arg3, arg4, false)
                }
                _ => u64::MAX,
            }
        }
        14 => {
            // ipc_call_buf(ep_cap, label, data0, buf_ptr, buf_len) — blocks until reply.
            match cap {
                Capability::Endpoint(ep_id) => {
                    let msg = message::Message::with2(arg1, arg2, 0);
                    ipc_send_with_bulk(task_id, ep_id, msg, arg3, arg4, true)
                }
                _ => u64::MAX,
            }
        }
        15 => {
            // ipc_recv_msg(ep_cap, msg_ptr, buf_ptr, buf_len) — blocks until a sender arrives.
            match cap {
                Capability::Endpoint(ep_id) => ipc_recv_msg(task_id, ep_id, arg1, arg2, arg3),
                _ => u64::MAX,
            }
        }
        16 => {
            // ipc_reply_recv_msg(reply_cap, reply_label, ep_cap, msg_ptr, buf_ptr, buf_len)
            // reply_cap = arg0 (already looked up as `cap`)
            // reply_label = arg1
            // ep_cap = arg2
            // msg_ptr = arg3
            // buf_ptr = arg4
            // buf_len = r9 (read from per-core saved registers)
            let caller_id = match cap {
                Capability::Reply(id) => id,
                _ => return u64::MAX,
            };
            if arg2 > u64::from(u32::MAX) {
                return u64::MAX;
            }
            let ep_id = match scheduler::task_cap(task_id, arg2 as CapHandle) {
                Ok(Capability::Endpoint(id)) => id,
                _ => return u64::MAX,
            };
            let _ = scheduler::remove_task_cap(task_id, arg0 as CapHandle);
            let reply = message::Message::new(arg1);
            endpoint::reply(task_id, caller_id, reply);
            // Read buf_len from the 6th syscall register (r9), capped at
            // MAX_BULK_LEN to match ipc_recv_msg's bounds.
            let buf_len = crate::smp::per_core().syscall_user_r9;
            ipc_recv_msg(task_id, ep_id, arg3, arg4, buf_len)
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
///
/// The calling task's ID is recorded as the owner, enabling owner-based
/// re-registration and cleanup on task exit.
fn ipc_register_service(
    task_id: crate::task::TaskId,
    ep_id: EndpointId,
    name_ptr: u64,
    name_len: u64,
) -> u64 {
    if name_ptr == 0 {
        return u64::MAX;
    }
    if name_len > 32 {
        return u64::MAX;
    }
    let name_len = name_len as usize;
    let mut name_buf = [0u8; 32];
    if UserSliceRo::new(name_ptr, name_len)
        .and_then(|s| s.copy_to_kernel(&mut name_buf[..name_len]))
        .is_err()
    {
        return u64::MAX;
    }
    let name = match core::str::from_utf8(&name_buf[..name_len]) {
        Ok(s) => s,
        Err(_) => return u64::MAX,
    };
    match registry::register_with_owner(name, ep_id, task_id.0) {
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
    if UserSliceRo::new(name_ptr, name_len)
        .and_then(|s| s.copy_to_kernel(&mut name_buf[..name_len]))
        .is_err()
    {
        return u64::MAX;
    }
    let name = match core::str::from_utf8(&name_buf[..name_len]) {
        Ok(s) => s,
        Err(_) => return u64::MAX,
    };
    match registry::with_lookup(name, |ep_id| {
        crate::task::scheduler::insert_cap(task_id, Capability::Endpoint(ep_id))
    }) {
        Some(Ok(handle)) => u64::from(handle),
        Some(Err(_)) | None => u64::MAX,
    }
}

/// Syscall 12 (0x110B): allocate a new IPC endpoint and insert an Endpoint
/// capability into the caller's capability table.
///
/// Returns the new capability handle on success, or `u64::MAX` on error.
fn ipc_create_endpoint(task_id: crate::task::TaskId) -> u64 {
    let ep_id = match endpoint::ENDPOINTS.lock().try_create_owned(task_id) {
        Some(id) => id,
        None => return u64::MAX,
    };
    match crate::task::scheduler::insert_cap(task_id, Capability::Endpoint(ep_id)) {
        Ok(handle) => u64::from(handle),
        Err(_) => {
            // Roll back: free the endpoint slot so it is not permanently leaked.
            endpoint::ENDPOINTS.lock().destroy(ep_id);
            u64::MAX
        }
    }
}

/// Syscall 11 (0x110A): create a notification registered for a hardware IRQ
/// and insert a Notification capability into the caller's capability table.
///
/// Only IRQ 1 (keyboard) is currently allowed for userspace services.
/// Returns the new capability handle on success, or `u64::MAX` on error.
fn create_irq_notification(task_id: crate::task::TaskId, irq: u64) -> u64 {
    // Only allow IRQ 1 (keyboard) for now.
    if irq != 1 {
        return u64::MAX;
    }
    // Exclusive registration: atomically claim this IRQ line using
    // compare_exchange so two concurrent callers on different cores cannot
    // both pass the check and overwrite each other.
    let notif_id = match x86_64::instructions::interrupts::without_interrupts(|| {
        notification::try_create().and_then(|id| {
            if notification::try_register_irq(irq as u8, id) {
                Some(id)
            } else {
                // IRQ line already taken — roll back the notification slot.
                notification::free(id);
                None
            }
        })
    }) {
        Some(id) => id,
        None => return u64::MAX,
    };
    match crate::task::scheduler::insert_cap(task_id, Capability::Notification(notif_id)) {
        Ok(handle) => u64::from(handle),
        Err(_) => {
            // Roll back: unregister the IRQ mapping and free the notification
            // slot so they are not permanently leaked/misrouted.
            x86_64::instructions::interrupts::without_interrupts(|| {
                notification::unregister_irq(irq as u8);
                notification::free(notif_id);
            });
            u64::MAX
        }
    }
}

// ---------------------------------------------------------------------------
// Bulk-data IPC helpers (Phase 52)
// ---------------------------------------------------------------------------

/// Maximum bulk-data payload accepted by `ipc_send_buf` / `ipc_call_buf`.
const MAX_BULK_LEN: usize = 4096;

/// Send (or call) with an attached bulk-data buffer.
///
/// Copies `buf_len` bytes from the sender's userspace address `buf_ptr` into
/// a kernel-owned `Vec<u8>`, then delivers the message + bulk data to the
/// receiver through the endpoint.  The `is_call` flag selects between
/// fire-and-forget send and RPC-style call.
///
/// Returns `0` on send success, the reply label on call success, or
/// `u64::MAX` on error.
fn ipc_send_with_bulk(
    task_id: crate::task::TaskId,
    ep_id: endpoint::EndpointId,
    mut msg: message::Message,
    buf_ptr: u64,
    buf_len: u64,
    is_call: bool,
) -> u64 {
    use crate::task::scheduler;

    let len = buf_len as usize;
    if len == 0 || len > MAX_BULK_LEN {
        return u64::MAX;
    }

    // Copy the sender's buffer into kernel memory while the sender's CR3
    // is still active.
    let mut bulk = alloc::vec![0u8; len];
    if UserSliceRo::new(buf_ptr, bulk.len())
        .and_then(|s| s.copy_to_kernel(&mut bulk))
        .is_err()
    {
        return u64::MAX;
    }

    // Encode the actual bulk data length in data[1] so the receiver knows
    // how many bytes to expect in its output buffer.
    msg.data[1] = len as u64;

    // Store bulk data in the sender's pending_bulk slot.  The endpoint
    // send/call code will transfer it to the receiver via
    // `deliver_message` + `deliver_bulk`.
    scheduler::deliver_bulk(task_id, bulk);

    if is_call {
        let reply = endpoint::call(task_id, ep_id, msg);
        if reply == u64::MAX {
            let _ = scheduler::take_bulk_data(task_id);
        }
        reply
    } else if endpoint::send(task_id, ep_id, msg) {
        0
    } else {
        // Send failed — clean up the bulk data.
        let _ = scheduler::take_bulk_data(task_id);
        u64::MAX
    }
}

/// Receive a message with full data words and optional bulk payload.
///
/// Calls `recv_msg` to get the full `Message`, then writes the header
/// (label + data[0..4]) to `msg_ptr` and any bulk data to `buf_ptr`
/// via `copy_to_user`.  `buf_len` caps the bulk copy.
///
/// Returns the message label on success, or `u64::MAX` on error.
fn ipc_recv_msg(
    task_id: crate::task::TaskId,
    ep_id: endpoint::EndpointId,
    msg_ptr: u64,
    buf_ptr: u64,
    buf_len: u64,
) -> u64 {
    use crate::task::scheduler;

    let msg = endpoint::recv_msg(task_id, ep_id);
    if msg.label == u64::MAX {
        return u64::MAX;
    }

    // Write the IpcMessage header (label + 4 data words = 40 bytes) to
    // userspace.  Layout must match syscall_lib::IpcMessage.
    if msg_ptr != 0 {
        let mut header = [0u8; 40];
        header[0..8].copy_from_slice(&msg.label.to_ne_bytes());
        for (i, &d) in msg.data.iter().enumerate() {
            let off = 8 + i * 8;
            header[off..off + 8].copy_from_slice(&d.to_ne_bytes());
        }
        if UserSliceWo::new(msg_ptr, header.len())
            .and_then(|s| s.copy_from_kernel(&header))
            .is_err()
        {
            return u64::MAX;
        }
    }

    // Copy bulk data to the receiver's buffer if present.
    if buf_ptr != 0
        && let Some(bulk) = scheduler::take_bulk_data(task_id)
    {
        let copy_len = bulk.len().min(buf_len as usize);
        if copy_len > 0
            && UserSliceWo::new(buf_ptr, copy_len)
                .and_then(|s| s.copy_from_kernel(&bulk[..copy_len]))
                .is_err()
        {
            return u64::MAX;
        }
    }

    msg.label
}

// ---------------------------------------------------------------------------
// Reply bulk data helper (Phase 54)
// ---------------------------------------------------------------------------

/// Syscall 17 (0x1110): store bulk data to be sent with the next IPC reply.
///
/// Copies `buf_len` bytes from the caller's userspace address `buf_ptr` into
/// the caller's `pending_bulk` slot.  The data is transferred to the reply
/// target when [`endpoint::reply`] is called (which now does `transfer_bulk`
/// from server → caller).
///
/// Returns `0` on success, or `u64::MAX` on error.
fn ipc_store_reply_bulk(task_id: crate::task::TaskId, buf_ptr: u64, buf_len: u64) -> u64 {
    use crate::task::scheduler;

    let len = buf_len as usize;
    if len == 0 || len > MAX_BULK_LEN {
        return u64::MAX;
    }

    let mut bulk = alloc::vec![0u8; len];
    if UserSliceRo::new(buf_ptr, bulk.len())
        .and_then(|s| s.copy_to_kernel(&mut bulk))
        .is_err()
    {
        return u64::MAX;
    }

    scheduler::deliver_bulk(task_id, bulk);
    0
}
