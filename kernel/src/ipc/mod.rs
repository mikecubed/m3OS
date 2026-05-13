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
//! - Userspace IPC via the syscall gate (syscall numbers `0x1100`–`0x1113`;
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

/// Phase 57d follow-up — bounded budget for `log_send_with_bulk_anomaly`.
/// Caps log spam at 16 occurrences per boot so a recurrent bug stays
/// readable in the transcript without flooding when the violation fires
/// in a tight loop (term's renderer retries on submit failure).
static SEND_BULK_ANOMALY_BUDGET: core::sync::atomic::AtomicU32 =
    core::sync::atomic::AtomicU32::new(16);

/// Phase 57d follow-up — log when a sender allocates a 24-byte bulk
/// vec but writes a CommitSurface header (`04 00 15 00`) at offset 0.
/// This is the smoking-gun shape of the
/// `bulk_len=24 head=04001500010000000000000000000000` violation
/// observed at the receiver: term's `publish_frame` should send Damage
/// (24-byte) and Commit (8-byte) as separate calls, never a 24-byte
/// CommitSurface. The log captures `task_id`, `label`, and the first 12
/// bytes of the bulk so the next transcript pins down whether term is
/// passing the wrong `buf_len` or the kernel's per-core syscall args
/// got crossed across a context switch.
fn log_send_with_bulk_anomaly(task_id: crate::task::TaskId, label: u64, bulk: &[u8]) {
    if SEND_BULK_ANOMALY_BUDGET
        .fetch_update(
            core::sync::atomic::Ordering::Relaxed,
            core::sync::atomic::Ordering::Relaxed,
            |remaining| remaining.checked_sub(1),
        )
        .is_err()
    {
        return;
    }
    let n = bulk.len().min(12);
    let mut hex = [0u8; 24];
    let mut idx = 0;
    for &b in &bulk[..n] {
        hex[idx] = nibble_to_hex(b >> 4);
        hex[idx + 1] = nibble_to_hex(b & 0x0F);
        idx += 2;
    }
    log::warn!(
        "[ipc] send_with_bulk anomaly: task={} label={:#x} buf_len={} head={}",
        task_id.0,
        label,
        bulk.len(),
        core::str::from_utf8(&hex[..idx]).unwrap_or("??"),
    );
}

fn nibble_to_hex(v: u8) -> u8 {
    match v {
        0..=9 => b'0' + v,
        10..=15 => b'a' + (v - 10),
        _ => b'?',
    }
}

/// Phase 63 audio handoff follow-up — bounded budget for kernel-side
/// `u64::MAX` IPC return diagnostics. Caps spam at 32 occurrences per
/// boot (across all sites) so a recurring race stays readable in the
/// transcript without flooding when it fires inside a tight retry
/// loop (e.g. `audio-demo`'s 200×5 ms `Io(-32)` backoff). The
/// `site` discriminator pins which branch of `ipc_send_with_bulk` /
/// `endpoint::call_msg` / `endpoint::recv_msg_with_notif` produced
/// the sentinel — needed to distinguish:
///
/// - `send_with_bulk:bad_len` / `send_with_bulk:copy_failed` —
///   pre-flight failures (caller-side bug)
/// - `call_msg:endpoint_closed` — endpoint dropped between lookup
///   and lock acquisition (rare; persistent if true)
/// - `call_msg:cap_table_full` — server cap-table exhaustion (a
///   reply-cap leak signature)
/// - `call_msg:no_reply_message` — block primitive returned with
///   no `pending_msg` set (the "spurious wake" IPC logic bug)
/// - `recv_msg_with_notif:cap_full` /
///   `recv_msg_with_notif:transfer_cap_failed` — server-side
///   delivery failures that send a sentinel reply back to the caller
///
/// Pair this with the audio-demo retry budget to identify which
/// branch fires under the `Io(-32)` intermittency — see
/// `docs/handoffs/2026-05-11-phase-63-audio-irq-wake-race.md`
/// §"Known follow-ups".
static IPC_UMAX_DIAG_BUDGET: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(32);

pub(super) fn log_ipc_umax(
    task_id: crate::task::TaskId,
    site: &'static str,
    label: u64,
    detail: u64,
) {
    if IPC_UMAX_DIAG_BUDGET
        .fetch_update(
            core::sync::atomic::Ordering::Relaxed,
            core::sync::atomic::Ordering::Relaxed,
            |remaining| remaining.checked_sub(1),
        )
        .is_err()
    {
        return;
    }
    log::warn!(
        "[ipc] u64::MAX diag: task={} site={} label={:#x} detail={:#x}",
        task_id.0,
        site,
        label,
        detail,
    );
}

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
/// Userspace syscall numbers `0x1100`–`0x1113` are translated to internal
/// dispatch numbers 1–20 by the flat dispatch table in `arch/x86_64/syscall/mod.rs`.
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
/// | 15 | 0x110E | `ipc_recv_msg(ep_cap, msg_ptr, buf_ptr, buf_len)` | `arg0..3` → label or `1` on notification wake |
/// | 16 | 0x110F | `ipc_reply_recv_msg(reply_cap, label, ep_cap, msg_ptr, buf_ptr)` | `arg0..4` → label |
/// | 17 | 0x1110 | `ipc_store_reply_bulk(buf_ptr, buf_len)` | `arg0, arg1` → 0 or u64::MAX |
/// | 18 | 0x1111 | `sys_notif_bind(notif_cap, ep_cap)` | `arg0 = notif_cap, arg1 = ep_cap` → 0 or NEG_EBUSY/NEG_EBADF/u64::MAX |
/// | 19 | 0x1112 | `ipc_take_pending_bulk(buf_ptr, buf_len)` | `arg0, arg1` → bytes_copied or u64::MAX |
/// | 20 | 0x1113 | `ipc_try_recv_msg(ep_cap, msg_ptr, buf_ptr, buf_len)` | `arg0..3` → label, or u64::MAX if no pending message |
/// | 21 | 0x1114 | `ipc_service_exists(name_ptr, name_len)` | `arg0, arg1` → 1 if registered, 0 otherwise |
/// | 22 | 0x1115 | `ipc_wait_service(name_ptr, name_len, timeout_ms)` | `arg0..2` → 1 ready, 0 timeout |
/// | 23 | 0x1116 | `ipc_lookup_service_owner_pid(name_ptr, name_len)` | `arg0, arg1` → owner PID or `u64::MAX` |
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

    // Syscalls 10, 11, 12, 17, 18, 19, 21, and 22 do not use arg0 as a pre-looked-up cap
    // handle — process them before the cap-lookup preamble.
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
    if number == 18 {
        return sys_notif_bind(task_id, arg0, arg1);
    }
    if number == 19 {
        return ipc_take_pending_bulk(task_id, arg0, arg1);
    }
    if number == 21 {
        return ipc_service_exists(arg0, arg1);
    }
    if number == 22 {
        return ipc_wait_service(task_id, arg0, arg1, arg2);
    }
    // Phase 64 Track A.2: query the owner PID of a registered service.
    // `session_manager` consumes this after `await_ready` succeeds to
    // populate `ServiceTable::update_pid`, which is the foundation for
    // the new SIGTERM/SIGKILL lifecycle methods. The kernel already
    // tracks owners via `registry::lookup_endpoint_with_owner`; this
    // syscall is a thin wrapper that maps the owner `TaskId` to a
    // userspace-visible `pid`.
    if number == 23 {
        return ipc_lookup_service_owner_pid(arg0, arg1);
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
            //
            // Atomically check-and-remove the reply cap before replying.
            // The earlier `task_cap` peek is racy against
            // `revoke_reply_caps_for`: between the peek and the remove, a
            // signal delivery could wake the caller with the EINTR sentinel
            // and drop the cap. Running `endpoint::reply` after that would
            // clobber the caller's state.
            match scheduler::remove_task_cap(task_id, arg0 as CapHandle) {
                Ok(Capability::Reply(caller_id)) => {
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
            //
            // Validate the ep handle first so a bad ep_cap does not strand the
            // reply cap. Range-check arg2 before casting to CapHandle.
            if arg2 > u64::from(u32::MAX) {
                return u64::MAX;
            }
            let ep_id = match scheduler::task_cap(task_id, arg2 as CapHandle) {
                Ok(Capability::Endpoint(id)) => id,
                _ => return u64::MAX,
            };
            // Atomically check-and-remove the reply cap. If revocation raced
            // between the earlier peek and here, the caller was already woken
            // with the EINTR sentinel — do not deliver a stale reply.
            let caller_id = match scheduler::remove_task_cap(task_id, arg0 as CapHandle) {
                Ok(Capability::Reply(id)) => id,
                _ => return u64::MAX,
            };
            let reply = message::Message::new(arg1);
            endpoint::reply_recv(task_id, caller_id, ep_id, reply)
        }
        7 => {
            // notify_wait(notif_cap_handle) — blocks until bits are pending.
            // DeviceIrq caps alias their underlying notification so ring-3
            // drivers can wait on the handle returned by
            // sys_device_irq_subscribe directly. Errors return 0, not u64::MAX.
            match cap.ipc_notification_id() {
                Some(notif_id) => notification::wait(task_id, notif_id),
                None => 0,
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
        20 => {
            // ipc_try_recv_msg(ep_cap, msg_ptr, buf_ptr, buf_len) — non-blocking
            // variant of ipc_recv_msg. Returns the message label on success,
            // or u64::MAX if no sender was pending. Used by display_server's
            // main loop to multiplex frame-tick driving + control-endpoint
            // serving without blocking.
            match cap {
                Capability::Endpoint(ep_id) => ipc_try_recv_msg(task_id, ep_id, arg1, arg2, arg3),
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
            //
            // Validate the ep handle first so a bad ep_cap does not strand
            // the reply cap.
            if arg2 > u64::from(u32::MAX) {
                return u64::MAX;
            }
            let ep_id = match scheduler::task_cap(task_id, arg2 as CapHandle) {
                Ok(Capability::Endpoint(id)) => id,
                _ => return u64::MAX,
            };
            // Atomically check-and-remove the reply cap. If revocation raced
            // between the earlier peek and here, the caller was already woken
            // with the EINTR sentinel — do not deliver a stale reply.
            let caller_id = match scheduler::remove_task_cap(task_id, arg0 as CapHandle) {
                Ok(Capability::Reply(id)) => id,
                _ => return u64::MAX,
            };
            let reply = message::Message::new(arg1);
            endpoint::reply(task_id, caller_id, reply);
            // Read buf_len from the 6th syscall register (r9), capped at
            // MAX_BULK_LEN to match ipc_recv_msg's bounds.
            // Phase 57e Bug #3 fix — per-task snapshot, see crate::task.
            let buf_len = crate::task::current_task_syscall_snapshot().user_r9;
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

/// Internal-only services that userspace is not allowed to look up by name.
///
/// These services act as a private kernel-side facade (e.g., `vfs_server` is
/// only ever meant to be called via kernel syscall routing that already
/// enforces DAC). Handing out endpoint capabilities to arbitrary userspace
/// tasks would let unprivileged code bypass the kernel's access checks and
/// drive the service directly.
const PRIVATE_SERVICE_NAMES: &[&str] = &["vfs", "net_udp"];

fn is_private_service_name(name: &str) -> bool {
    PRIVATE_SERVICE_NAMES.contains(&name)
}

/// Syscall 10: look up a named endpoint and insert it into the caller's cap table.
///
/// `name_ptr` is a userspace virtual address pointing to `name_len` bytes of
/// UTF-8. The name is safely copied from the caller's address space via
/// `copy_from_user`. Invalid or unmapped pointers return an error.
///
/// Private services (see `PRIVATE_SERVICE_NAMES`) are never exposed to
/// userspace — lookups for those names fail as if the service were not
/// registered.
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
    if is_private_service_name(name) {
        return u64::MAX;
    }
    match registry::with_lookup(name, |ep_id| {
        crate::task::scheduler::insert_cap(task_id, Capability::Endpoint(ep_id))
    }) {
        Some(Ok(handle)) => u64::from(handle),
        Some(Err(_)) | None => u64::MAX,
    }
}

/// Syscall 21 (0x1114): query whether a named service is currently registered
/// without inserting a capability into the caller's cap table.
///
/// Private services are visible through this presence-only probe so dependent
/// userspace can wait for readiness without receiving a callable endpoint.
fn ipc_service_exists(name_ptr: u64, name_len: u64) -> u64 {
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
    u64::from(registry::is_registered(name))
}

/// Syscall 22 (0x1115): block until a named service is registered.
///
/// `timeout_ms == 0` waits indefinitely. A positive timeout uses the scheduler
/// deadline scanner (1 tick = 1 ms). Returns `1` when ready, `0` on timeout,
/// and `u64::MAX` for invalid input.
fn ipc_wait_service(
    task_id: crate::task::TaskId,
    name_ptr: u64,
    name_len: u64,
    timeout_ms: u64,
) -> u64 {
    if name_ptr == 0 {
        return u64::MAX;
    }
    if name_len == 0 || name_len > 32 {
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
    let deadline_ticks = if timeout_ms == 0 {
        None
    } else {
        Some(crate::arch::x86_64::interrupts::tick_count().saturating_add(timeout_ms))
    };
    u64::from(registry::wait_until_registered(
        name,
        task_id,
        deadline_ticks,
    ))
}

/// Phase 64 Track A.2 — Syscall 23 (0x1116): return the userspace PID of the
/// process owning the registered service `name`.
///
/// Used by `session_manager` after `await_ready` succeeds: the
/// `ServiceTable` records this PID so the new `stop_service` /
/// `restart_service` lifecycle methods can target the child by PID
/// rather than guessing.
///
/// Returns the PID as `u64` on success, or `u64::MAX` on any error
/// (name too long, copy fault, service not registered, kernel-owned
/// service with no PID). Kernel-registered services (e.g. blk facades)
/// report owner-task-id `0`; this syscall surfaces that as `u64::MAX`
/// because they cannot be targeted with `kill`.
fn ipc_lookup_service_owner_pid(name_ptr: u64, name_len: u64) -> u64 {
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
    let (_, owner_task_id) = match registry::lookup_endpoint_with_owner(name) {
        Some(pair) => pair,
        None => return u64::MAX,
    };
    if owner_task_id == 0 {
        // Kernel-owned service — no userspace PID to target.
        return u64::MAX;
    }
    let task_id = crate::task::TaskId(owner_task_id);
    match crate::task::scheduler::pid_for_task_id(task_id) {
        Some(pid) => u64::from(pid),
        None => u64::MAX,
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
///
/// Sized at 20 4 KiB pages so high-bandwidth pixel-upload paths
/// (`term` → `display_server` chunked surface buffers) hit the kernel
/// IPC primitive in ~16 roundtrips per 1 MiB frame instead of ~252,
/// and the audio path can ship a full `MAX_SUBMIT_BYTES` (64 KiB)
/// PCM payload alongside its 16 B request frame in one `ipc_call_buf`.
/// The kernel allocates `len` bytes on demand per `ipc_send_with_bulk`,
/// not `MAX_BULK_LEN`, so small protocol verbs still cost ~tens of bytes
/// each — this bump only changes the ceiling, not the per-call alloc.
/// Raising it further is safe in principle but consumers'
/// `bulk_buf: Vec<u8>` reservations need to track in lockstep
/// (`display_server::client::MAX_BULK_BYTES`,
/// `kernel_core::display::protocol::MAX_FRAME_BODY_LEN`). 81920 leaves
/// 16 KiB headroom over the 64 KiB PCM submit payload so future
/// driver protocols can grow without re-bumping this constant.
const MAX_BULK_LEN: usize = 81920;

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
        log_ipc_umax(task_id, "send_with_bulk:bad_len", msg.label, buf_len);
        return u64::MAX;
    }

    // Copy the sender's buffer into kernel memory while the sender's CR3
    // is still active.
    let mut bulk = alloc::vec![0u8; len];
    if UserSliceRo::new(buf_ptr, bulk.len())
        .and_then(|s| s.copy_to_kernel(&mut bulk))
        .is_err()
    {
        log_ipc_umax(task_id, "send_with_bulk:copy_failed", msg.label, buf_ptr);
        return u64::MAX;
    }

    // Phase 57d follow-up — diagnostic for the
    // `bulk_len=24 head=04001500010000000000000000000000` violation
    // pattern. Log every send whose first four bytes look like a
    // CommitSurface header (`04 00 15 00`) but whose `buf_len` is 24
    // (DamageSurface size). That is the smoking-gun shape of the
    // mismatch: a commit-shaped frame body delivered with a damage-
    // shaped bulk size. Rate-limited so a real attack cannot flood
    // the log; budget bumps every boot.
    if bulk.len() == 24
        && bulk.len() >= 4
        && bulk[0] == 0x04
        && bulk[1] == 0x00
        && bulk[2] == 0x15
        && bulk[3] == 0x00
    {
        log_send_with_bulk_anomaly(task_id, msg.label, &bulk);
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
            // Diagnostic is emitted by `endpoint::call_msg` at the
            // specific u64::MAX-producing branch (endpoint_closed /
            // cap_table_full / no_reply_message). Re-logging here
            // would lose the per-branch discriminator that motivates
            // the helper, so just drain the staged bulk and propagate.
            let _ = scheduler::take_bulk_data(task_id);
        }
        reply
    } else if endpoint::send(task_id, ep_id, msg) {
        0
    } else {
        // Send failed — clean up the bulk data.
        log_ipc_umax(task_id, "send_with_bulk:send_failed", msg.label, buf_len);
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
/// Returns the message label on message wake, `1` on notification wake, or
/// `u64::MAX` on error.
fn ipc_recv_msg(
    task_id: crate::task::TaskId,
    ep_id: endpoint::EndpointId,
    msg_ptr: u64,
    buf_ptr: u64,
    buf_len: u64,
) -> u64 {
    use crate::task::scheduler;
    use kernel_core::ipc::wake_kind::{RECV_KIND_MESSAGE, RECV_KIND_NOTIFICATION};

    let (kind, msg) = if let Some(task_sched_idx) = scheduler::get_current_task_idx() {
        if let Some(notif_id) = notification::lookup_bound_notif(task_sched_idx) {
            endpoint::recv_msg_with_notif(task_id, ep_id, notif_id)
        } else {
            (RECV_KIND_MESSAGE, endpoint::recv_msg(task_id, ep_id))
        }
    } else {
        (RECV_KIND_MESSAGE, endpoint::recv_msg(task_id, ep_id))
    };

    if msg.label == u64::MAX && kind == RECV_KIND_MESSAGE {
        return u64::MAX;
    }

    // Write the IpcMessage header (label + 4 data words = 40 bytes) to
    // userspace.  Layout must match syscall_lib::IpcMessage.
    //
    // `msg.data[1]` is preserved as the sender wrote it: for messages from
    // `ipc_send_with_bulk` it's the bulk length, but kernel-internal IPC
    // (e.g. `vfs_service_read` packs the file offset into `data[1]`) uses
    // the field for protocol-specific data and overriding it broke
    // `vfs_server`'s offset handling — every read came in as offset 0,
    // which made doom's WAD-directory load return the same first chunk
    // repeatedly and the lump-name lookup miss `PNAMES`.
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
    if kind == RECV_KIND_MESSAGE
        && buf_ptr != 0
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

    if kind == RECV_KIND_NOTIFICATION {
        u64::from(RECV_KIND_NOTIFICATION)
    } else {
        msg.label
    }
}

/// Syscall 20 (0x1113): non-blocking variant of [`ipc_recv_msg`].
///
/// Phase 56 close-out — closes the second half of the runtime byte-flow gap
/// (companion to [`ipc_take_pending_bulk`]). The display_server main loop
/// drives the frame-tick + compose path via polling, so it can't block on
/// `ipc_recv` for the control endpoint. This syscall returns immediately
/// with `u64::MAX` if no sender is queued, letting the loop multiplex.
///
/// Same arguments and same `msg_ptr` / `buf_ptr` semantics as
/// [`ipc_recv_msg`] except no notification fast-path: this is purely a
/// non-blocking peek-and-take of the endpoint queue.
///
/// Returns:
/// - the message label on success (header + bulk copied to user buffers)
/// - `u64::MAX` if no sender is pending OR on any copy failure
fn ipc_try_recv_msg(
    task_id: crate::task::TaskId,
    ep_id: endpoint::EndpointId,
    msg_ptr: u64,
    buf_ptr: u64,
    buf_len: u64,
) -> u64 {
    use crate::task::scheduler;

    let msg = match endpoint::recv_msg_nowait(task_id, ep_id) {
        Some(m) => m,
        None => return u64::MAX,
    };

    if msg.label == u64::MAX {
        return u64::MAX;
    }

    // `msg.data[1]` is preserved as the sender wrote it. The receive-side
    // override (= `take_bulk_data().len()`) was removed because the field
    // is protocol-specific for kernel-internal IPC: `vfs_service_read`
    // packs the read offset into `data[1]`, and forcing it to the bulk
    // length pinned every read at offset 0, which made doom's WAD load
    // miss `PNAMES`. The legitimate bulk-mismatch the override was
    // chasing (display_server seeing `bulk_len=24` with stale `bulk_buf`
    // bytes) now needs a fix that doesn't conflate the size carried in
    // `data[1]` with whatever value the userspace protocol carries there.
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

/// Syscall 18 (0x1111): bind a notification object to the calling task's TCB.
///
/// Binding allows `ipc_recv_msg` to consult the notification's pending bits
/// before parking on the endpoint, delivering a notification wake when an IRQ
/// or task-context signal fires.
///
/// # Return value
///
/// - `0` on success (including idempotent re-bind of the same pair).
/// - `NEG_EBADF` (-9) on invalid or missing capability (bad handle, wrong
///   capability type, or handle out of range). No side effects on error.
/// - `NEG_EBUSY` (-16) if the notification is already bound to a different task.
/// - `u64::MAX` on internal error: `get_current_task_idx()` returned `None`,
///   meaning the calling task has no active scheduler slot. This should not
///   occur in normal operation but is returned rather than panicking.
fn sys_notif_bind(task_id: crate::task::TaskId, notif_cap_handle: u64, ep_cap_handle: u64) -> u64 {
    use crate::task::scheduler;

    const NEG_EBADF: u64 = (-9_i64) as u64;
    const NEG_EBUSY: u64 = (-16_i64) as u64;

    if notif_cap_handle > u64::from(u32::MAX) {
        return NEG_EBADF;
    }
    let notif_id = match scheduler::task_cap(task_id, notif_cap_handle as CapHandle) {
        Ok(cap) => match cap.ipc_notification_id() {
            Some(id) => id,
            None => return NEG_EBADF,
        },
        _ => return NEG_EBADF,
    };

    if ep_cap_handle > u64::from(u32::MAX) {
        return NEG_EBADF;
    }
    match scheduler::task_cap(task_id, ep_cap_handle as CapHandle) {
        Ok(Capability::Endpoint(_)) => {}
        _ => return NEG_EBADF,
    }

    let task_sched_idx = match scheduler::get_current_task_idx() {
        Some(idx) => idx,
        None => return u64::MAX,
    };

    match notification::bind_task(notif_id, task_sched_idx) {
        Ok(()) => {
            log::trace!(
                "[ipc] sys_notif_bind: task {} (sched_idx={}) bound to notif {:?}",
                task_id.0,
                task_sched_idx,
                notif_id,
            );
            0
        }
        Err(()) => NEG_EBUSY,
    }
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

/// Syscall 19 (0x1112): drain the calling task's `pending_bulk` slot.
///
/// Phase 56 close-out — closes the bulk-reply visibility gap that gated
/// D.3's input-event delivery, E.4's `m3ctl` reply decoding, and the
/// G.1 / G.2 / G.4 deferred-runtime regression stubs.
///
/// After a successful `ipc_call_buf`, the kernel's `transfer_bulk(server →
/// caller)` path moves any bulk the server staged via `ipc_store_reply_bulk`
/// into the caller's `pending_bulk` slot. This syscall copies that slot's
/// contents into a user-supplied buffer and clears the slot.
///
/// # Return value
///
/// - `0..=MAX_BULK_LEN` on success: the number of bytes copied. `0` means
///   either no bulk was pending or a zero-length bulk was staged (the slot
///   is cleared either way).
/// - `u64::MAX` on error: zero-length user buffer, oversized buffer
///   (`buf_len > MAX_BULK_LEN`), or user-memory copy failure. The slot is
///   left untouched on parameter-validation errors so the caller can retry
///   with a correctly-sized buffer; on copy-out failure the bulk is already
///   consumed (reflecting the userspace memory state being broken).
///
/// # Truncation behavior
///
/// If the pending bulk is larger than `buf_len`, the call **truncates** to
/// `buf_len` and returns `buf_len`. The remainder is dropped — callers that
/// need full payloads must size the buffer to the largest expected wire
/// frame (e.g. `KEY_EVENT_WIRE_SIZE = 19`, `POINTER_EVENT_WIRE_SIZE = 37`,
/// `MAX_BULK_LEN = 4096`).
///
/// # Caller ordering constraint (single-slot `pending_bulk`)
///
/// `Task::pending_bulk` is a single `Option<Vec<u8>>` slot. The same slot
/// is read by [`ipc_recv_msg`] / [`ipc_try_recv_msg`] (via
/// `take_bulk_data`) to deliver inbound bulk on a recv, and is written
/// by `transfer_bulk` whenever bulk crosses task boundaries. Callers
/// **must** drain the slot via this syscall **immediately after**
/// `ipc_call_buf` returns and **before any other IPC operation** that
/// touches the same slot. Interleaving an `ipc_recv_msg` between the
/// call and the drain will either:
///
/// - lose the staged reply bulk (overwritten by the next `transfer_bulk`
///   from a later sender), or
/// - misdeliver it as the inbound bulk of an unrelated recv (the recv
///   path cannot distinguish staged-reply bytes from sender-attached
///   bytes — they are the same `Vec<u8>` in the same slot).
///
/// The kernel cannot enforce this — both reads of the slot are valid
/// operations on their own. Production callers (`m3ctl`,
/// `display-multi-client-smoke`, `grab-hook-smoke`,
/// `display-server-crash-smoke`) all observe the constraint; the
/// userspace-side docstring on `syscall_lib::ipc_take_pending_bulk`
/// states the supported usage pattern.
fn ipc_take_pending_bulk(task_id: crate::task::TaskId, buf_ptr: u64, buf_len: u64) -> u64 {
    use crate::task::scheduler;

    let max_len = buf_len as usize;
    if max_len == 0 || max_len > MAX_BULK_LEN {
        return u64::MAX;
    }

    let bulk = match scheduler::take_bulk_data(task_id) {
        Some(b) => b,
        // No bulk pending — caller's most recent reply (if any) carried no
        // bulk data. Return 0 as a non-error sentinel so callers can use a
        // single check (`n == u64::MAX`) for true errors.
        None => return 0,
    };

    let copy_len = bulk.len().min(max_len);
    if copy_len == 0 {
        return 0;
    }

    if UserSliceWo::new(buf_ptr, copy_len)
        .and_then(|s| s.copy_from_kernel(&bulk[..copy_len]))
        .is_err()
    {
        // Bulk was already taken from the slot; return error so the caller
        // observes the broken-userspace-mapping condition.
        return u64::MAX;
    }

    copy_len as u64
}
