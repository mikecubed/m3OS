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
//! Phase 74: capability grants via IPC and IPC timeouts are now delivered
//! (syscalls 0x1117–0x111A and the cap-bearing variants of `ipc_call` /
//! `ipc_recv_msg`). Page-capability bulk transfer lives in
//! [`page_grant`] (syscalls 0x1020 / 0x1021).
//! Many-to-one notification binding (`sys_notif_bind`, syscall 0x1111) is
//! also Phase 74 — it closes the Phase 55c deferred item alongside the
//! ones from Phase 6.

pub mod capability;
pub mod cleanup;
pub mod endpoint;
pub mod message;
pub mod notification;
pub mod page_grant;
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
/// | 24 | 0x1117 | `ipc_call_with_caps(ep_cap, msg_ptr, buf_ptr, buf_len)` | `arg0..3` → label, `u64::MAX` on error (Phase 74 Track A) |
/// | 25 | 0x1118 | `ipc_recv_with_caps(ep_cap, msg_ptr, buf_ptr, buf_len)` | `arg0..3` → label, `u64::MAX` on error (Phase 74 Track A) |
/// | 26 | 0x1119 | `ipc_call_timeout(ep_cap, label, data0, deadline_ns)` | `arg0..3` → label or `NEG_ETIMEDOUT` (Phase 74 Track C) |
/// | 27 | 0x111A | `ipc_recv_timeout(ep_cap, deadline_ns)` | `arg0..1` → label or `NEG_ETIMEDOUT` (Phase 74 Track C) |
/// | 28 | 0x111B | `sys_ipc_peer_is_driver(reply_cap)` | `arg0 = reply_cap_handle` → `1` if caller is an authorized driver process, else `0` (Phase 78c) |
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

    // Syscalls 10, 11, 12, 17, 18, 19, 21, 22, and 23 do not use arg0 as a
    // pre-looked-up cap handle — process them before the cap-lookup preamble.
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
        24 => {
            // Phase 74 Track A: ipc_call_with_caps(ep_cap, msg_ptr, buf_ptr, buf_len)
            //
            // Reads the full IpcMessage (including cap_slots / n_caps) from
            // userspace, validates and transfers each named capability to the
            // receiver, then performs an `ipc_call_buf`-shaped send-and-block.
            // On reply, the cap_slots field of the reply is updated with any
            // cap handles the server granted back.
            match cap {
                Capability::Endpoint(ep_id) => ipc_call_with_caps(task_id, ep_id, arg1, arg2, arg3),
                _ => u64::MAX,
            }
        }
        25 => {
            // Phase 74 Track A: ipc_recv_with_caps(ep_cap, msg_ptr, buf_ptr, buf_len)
            //
            // Receives a message and writes the full cap-bearing IpcMessage
            // (label + data + cap_slots + n_caps) to msg_ptr. Any caps the
            // sender transferred have already been inserted into the
            // receiver's capability table; their new handles appear in
            // `cap_slots[..n_caps]`.
            match cap {
                Capability::Endpoint(ep_id) => ipc_recv_with_caps(task_id, ep_id, arg1, arg2, arg3),
                _ => u64::MAX,
            }
        }
        26 => {
            // Phase 74 Track C: ipc_call_timeout(ep_cap, label, data0, deadline_ns)
            //
            // Like `ipc_call` but registers a deadline in the scheduler timer
            // wheel; if the call does not complete before `deadline_ns`
            // (absolute CLOCK_MONOTONIC nanoseconds), returns `NEG_ETIMEDOUT`.
            match cap {
                Capability::Endpoint(ep_id) => {
                    let msg = message::Message::with2(arg1, arg2, 0);
                    ipc_call_timeout(task_id, ep_id, msg, arg3)
                }
                _ => u64::MAX,
            }
        }
        27 => {
            // Phase 74 Track C: ipc_recv_timeout(ep_cap, deadline_ns)
            //
            // Like `ipc_recv` but registers a deadline in the scheduler timer
            // wheel; if no message arrives before `deadline_ns`, returns
            // `NEG_ETIMEDOUT`.
            match cap {
                Capability::Endpoint(ep_id) => ipc_recv_timeout(task_id, ep_id, arg1),
                _ => u64::MAX,
            }
        }
        28 => {
            // sys_ipc_peer_is_driver(reply_cap_handle) -> 1 if the task that
            // sent the message this reply cap answers is an authorized driver
            // process (exec_path under `/drivers/`), else 0.
            //
            // Phase 78c review follow-up. Input servers (`kbd_server` /
            // `mouse_server`) accept synthetic-input injection only from the
            // driver TCB; without this gate any ring-3 task that can look up
            // the public `kbd` / `mouse` service could forge keystrokes and
            // clicks. The kernel is the only party that can authenticate the
            // sender: it resolves the reply cap to the caller's `TaskId` and
            // checks the kernel-recorded (unforgeable) `exec_path`. Fails
            // closed — a non-Reply cap or an unknown task returns 0.
            match cap {
                Capability::Reply(sender) => match scheduler::pid_for_task_id(sender) {
                    Some(pid) if crate::syscall::device_host::is_authorized_driver_process(pid) => {
                        1
                    }
                    _ => 0,
                },
                _ => 0,
            }
        }
        29 => {
            // Phase 87: ipc_recv_msg_timeout(ep_cap, msg_ptr, buf_ptr, buf_len,
            // deadline_ns) — like ipc_recv_msg (opcode 15) but with an absolute
            // CLOCK_MONOTONIC-ns deadline. Returns the message label on a
            // message wake, or NEG_ETIMEDOUT if the deadline expires first. Lets
            // a request server wake periodically (when otherwise idle) to flush
            // deferred state instead of blocking indefinitely in recv.
            match cap {
                Capability::Endpoint(ep_id) => {
                    ipc_recv_msg_timeout(task_id, ep_id, arg1, arg2, arg3, arg4)
                }
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
    // Hide private kernel-facing services (vfs, net_udp): exposing their
    // owner PID would let unprivileged userspace target them with kill()
    // even though `ipc_lookup_service` intentionally refuses to hand out
    // their endpoint caps. Mirrors the gate in `ipc_lookup_service`.
    if is_private_service_name(name) {
        return u64::MAX;
    }
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
/// `kernel_core::display::protocol::MAX_FRAME_BODY_LEN`) — though only a
/// receiver that *expects* to read up to this ceiling needs to; each receiver
/// caps at its own buffer regardless. Per-call allocs are sized to the actual
/// payload, so this only changes the ceiling.
///
/// Phase 95c (Area A.2) — raised 80 KiB → 512 KiB so the 256 KiB
/// `VFS_MAX_PREAD`/`VFS_MAX_PWRITE` clusters (path + data) fit: ~4x fewer IPC
/// round-trips on the install + cold-load VFS paths. `vfs_server`'s `recv_buf`
/// (`MAX_BULK_BUF = VFS_MAX_PWRITE = 256 KiB`) tracks it; display/audio keep
/// their own smaller caps and are unaffected.
const MAX_BULK_LEN: usize = 512 * 1024;

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

    // Write the cap-bearing IpcMessage (label + 4 data words + cap_slots
    // + n_caps = 56 bytes) to userspace. Layout matches the Phase 74
    // syscall_lib::IpcMessage.
    //
    // Phase 74 Track F.1 — bumped from 40 bytes to 56 bytes so legacy
    // recv callers automatically see Phase 74 cap_slots when a sender
    // transfers caps through them. All in-tree userspace IpcMessage
    // values were bumped to 56 bytes in the Phase 74 syscall-lib edit;
    // pre-Phase-74 callers observe `n_caps = 0` (the default) and
    // identical legacy behaviour.
    //
    // `msg.data[1]` is preserved as the sender wrote it: for messages from
    // `ipc_send_with_bulk` it's the bulk length, but kernel-internal IPC
    // (e.g. `vfs_service_read` packs the file offset into `data[1]`) uses
    // the field for protocol-specific data and overriding it broke
    // `vfs_server`'s offset handling — every read came in as offset 0,
    // which made doom's WAD-directory load return the same first chunk
    // repeatedly and the lump-name lookup miss `PNAMES`.
    if msg_ptr != 0 {
        let header = build_cap_msg_wire(&msg);
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

/// Phase 87 — deadline variant of [`ipc_recv_msg`]. Blocks on `ep_id` until a
/// message arrives or the absolute `deadline_ns` (CLOCK_MONOTONIC) expires. On a
/// message wake, writes the header + bulk to userspace exactly like
/// `ipc_recv_msg` and returns the label; on deadline expiry, writes nothing and
/// returns `NEG_ETIMEDOUT` (-110). No notification fast-path — request servers
/// using this (vfs_server's periodic-flush loop) have no bound notification.
fn ipc_recv_msg_timeout(
    task_id: crate::task::TaskId,
    ep_id: endpoint::EndpointId,
    msg_ptr: u64,
    buf_ptr: u64,
    buf_len: u64,
    deadline_ns: u64,
) -> u64 {
    use crate::task::scheduler;
    const NEG_ETIMEDOUT: u64 = (-110_i64) as u64;

    let deadline_ticks = deadline_ns_to_ticks(deadline_ns);
    let msg = endpoint::recv_msg_with_deadline(task_id, ep_id, Some(deadline_ticks));

    if msg.label == NEG_ETIMEDOUT {
        return NEG_ETIMEDOUT;
    }
    if msg.label == u64::MAX {
        return u64::MAX;
    }

    // Header + bulk copy is identical to `ipc_recv_msg`'s message path.
    if msg_ptr != 0 {
        let header = build_cap_msg_wire(&msg);
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
    //
    // Phase 74 Track F.1 — header bumped from 40 to 56 bytes via
    // `build_cap_msg_wire` so non-blocking recv callers also observe
    // any Phase 74 cap_slots a sender transferred. See `ipc_recv_msg`
    // for the wire-format rationale.
    if msg_ptr != 0 {
        let header = build_cap_msg_wire(&msg);
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

// ---------------------------------------------------------------------------
// Phase 74 Track A — capability transfer via IPC (`sys_cap_grant` in-message)
// ---------------------------------------------------------------------------

/// Phase 74: wire-format size of the cap-bearing [`IpcMessageWire`] struct
/// shared with `userspace/syscall-lib/src/lib.rs::IpcMessage`. Composed of:
/// - `label`        :  8 bytes
/// - `data[0..4]`   : 32 bytes
/// - `cap_slots`    :  8 bytes (2 × u32)
/// - `n_caps`       :  1 byte
/// - padding        :  7 bytes
const CAP_MSG_WIRE_LEN: usize = 56;

// The wire layout above (CAP_MSG_WIRE_LEN, the `40 + i*4` cap-slot offsets
// in `build_cap_msg_wire` / `read_cap_msg_from_user`, and the `wire[48]`
// n_caps offset) is hand-coded for `CAP_SLOTS_PER_MSG == 2`. If the slot
// count ever changes, every offset above must be reviewed and the userspace
// wire mirror in `syscall-lib::IpcMessage` updated in lockstep. This
// const-assert turns silent ABI drift into a compile-time error.
const _: () = assert!(
    kernel_core::ipc::message::CAP_SLOTS_PER_MSG == 2,
    "CAP_MSG_WIRE_LEN and the cap-slot encode/decode offsets in build_cap_msg_wire \
     / read_cap_msg_from_user assume CAP_SLOTS_PER_MSG == 2; update both before \
     changing the slot count.",
);

/// Phase 74 Track A — atomically transfer the capability handles named in
/// `msg.cap_slots[..msg.n_caps]` from `sender` to `receiver`.
///
/// Returns the receiver-side handles (mutates `msg.cap_slots` in place to
/// hold them on success). Validates every source handle before mutating any
/// state; on receiver-side `TableFull` failure or invalid handle, rolls back
/// any caps already transferred and returns the inserting [`CapError`].
///
/// # Rollback atomicity
///
/// On rollback the function snapshots the original sender-side handle for
/// each slot (along with the underlying [`Capability`] value) before
/// `grant_task_cap` removes it from the sender. If a later slot fails, the
/// rollback removes the cap from the receiver and re-inserts it into the
/// sender at the *original* handle via [`scheduler::insert_cap_at`] — not
/// at whichever free slot `CapabilityTable::insert` happens to choose. From
/// the caller's perspective the sender-side handle space is untouched on a
/// failed transfer: the cap is back in exactly the slot it came from.
///
/// The transfer uses [`crate::task::scheduler::grant_task_cap`] under the
/// scheduler lock for each slot, matching the atomicity guarantee of the
/// existing single-cap `sys_cap_grant` path.
fn ipc_transfer_caps(
    sender: crate::task::TaskId,
    receiver: crate::task::TaskId,
    msg: &mut message::Message,
) -> Result<(), CapError> {
    use crate::task::scheduler;
    use kernel_core::ipc::message::CAP_SLOTS_PER_MSG;

    let n = msg.n_caps as usize;
    if n == 0 {
        return Ok(());
    }
    if n > CAP_SLOTS_PER_MSG {
        return Err(CapError::InvalidHandle);
    }
    if sender == receiver {
        // Cap transfer to self is a no-op — the handles already live in
        // the sender table. Preserve the source handles so the receiver
        // sees the same indices it sent.
        return Ok(());
    }

    // Phase A: snapshot every source handle and the underlying capability
    // *before* any mutation. Pre-validation tightens the common-case
    // failure mode (typo'd handle returns `InvalidHandle` before any
    // partial state) and the cap snapshot lets Phase C restore the
    // original sender-side slot on rollback.
    let mut orig_handles = [0u32; CAP_SLOTS_PER_MSG];
    let mut orig_caps: [Option<Capability>; CAP_SLOTS_PER_MSG] = [None, None];
    for i in 0..n {
        let cap = scheduler::task_cap(sender, msg.cap_slots[i])?;
        orig_handles[i] = msg.cap_slots[i];
        orig_caps[i] = Some(cap);
    }

    // Phase B: transfer each handle in turn. `grant_task_cap` performs
    // an atomic remove-from-source + insert-into-target under the
    // scheduler lock. If any transfer fails (validation lost the race
    // or the receiver table truly cannot grow), roll back the already-
    // transferred caps by restoring them to the sender's *original*
    // slot (preserving caller-side handle stability).
    let mut new_handles = [0u32; CAP_SLOTS_PER_MSG];
    let mut transferred = 0usize;
    for i in 0..n {
        match scheduler::grant_task_cap(sender, msg.cap_slots[i], receiver) {
            Ok(handle) => {
                new_handles[i] = handle;
                transferred += 1;
            }
            Err(err) => {
                // Phase C: roll back every cap already transferred to the
                // receiver back into the sender at the original slot. The
                // `remove_task_cap(receiver, h)` + `insert_cap_at(sender,
                // orig_handles[j], cap)` pair is not itself wrapped in a
                // single scheduler-lock acquisition, but each step IS
                // atomic — the worst-case observable state on a wedged
                // rollback (e.g. cap removed from receiver but
                // `insert_cap_at` returns `SlotOccupied` because another
                // thread filled the original slot) is a leaked cap, never
                // a duplicated one. That is the same failure model
                // `sys_cap_grant` carries today.
                for j in 0..transferred {
                    let h = new_handles[j];
                    let cap = match scheduler::remove_task_cap(receiver, h) {
                        Ok(c) => c,
                        Err(_) => continue,
                    };
                    let _ = scheduler::insert_cap_at(sender, orig_handles[j], cap);
                }
                return Err(err);
            }
        }
    }

    // Mutate the message in place so the receiver observes the new
    // receiver-side handles instead of the sender's now-stale indices.
    for (i, handle) in new_handles.iter().enumerate().take(n) {
        msg.cap_slots[i] = *handle;
    }
    Ok(())
}

#[allow(dead_code)]
const NEG_ETIMEDOUT: u64 = (-110_i64) as u64;
#[allow(dead_code)]
const NEG_EINVAL_IPC: u64 = (-22_i64) as u64;
#[allow(dead_code)]
const NEG_ENOSPC_IPC: u64 = (-28_i64) as u64;

/// Phase 74 Track A — read a cap-bearing IPC message wire-form from user
/// memory and produce the kernel-side [`message::Message`].
fn read_cap_msg_from_user(msg_ptr: u64) -> Option<message::Message> {
    if msg_ptr == 0 {
        return None;
    }
    let mut wire = [0u8; CAP_MSG_WIRE_LEN];
    UserSliceRo::new(msg_ptr, wire.len())
        .and_then(|s| s.copy_to_kernel(&mut wire))
        .ok()?;
    let label = u64::from_ne_bytes(wire[0..8].try_into().ok()?);
    let mut data = [0u64; 4];
    for (i, d) in data.iter_mut().enumerate() {
        let off = 8 + i * 8;
        *d = u64::from_ne_bytes(wire[off..off + 8].try_into().ok()?);
    }
    let mut cap_slots = [0u32; kernel_core::ipc::message::CAP_SLOTS_PER_MSG];
    for (i, slot) in cap_slots.iter_mut().enumerate() {
        let off = 40 + i * 4;
        *slot = u32::from_ne_bytes(wire[off..off + 4].try_into().ok()?);
    }
    Some(message::Message {
        label,
        data,
        cap: None,
        cap_slots,
        n_caps: wire[48],
    })
}

/// Phase 74 Track A — write a cap-bearing IPC message back to user memory.
fn write_cap_msg_to_user(msg_ptr: u64, msg: &message::Message) -> bool {
    if msg_ptr == 0 {
        return true;
    }
    let wire = build_cap_msg_wire(msg);
    UserSliceWo::new(msg_ptr, wire.len())
        .and_then(|s| s.copy_from_kernel(&wire))
        .is_ok()
}

/// Phase 74 Track F.1 — build the 56-byte cap-bearing wire form of `msg`.
/// Shared by [`ipc_recv_msg`] / [`ipc_try_recv_msg`] / [`write_cap_msg_to_user`]
/// so the cap_slot delivery format stays one source of truth.
fn build_cap_msg_wire(msg: &message::Message) -> [u8; CAP_MSG_WIRE_LEN] {
    let mut wire = [0u8; CAP_MSG_WIRE_LEN];
    wire[0..8].copy_from_slice(&msg.label.to_ne_bytes());
    for (i, &d) in msg.data.iter().enumerate() {
        let off = 8 + i * 8;
        wire[off..off + 8].copy_from_slice(&d.to_ne_bytes());
    }
    for (i, &h) in msg.cap_slots.iter().enumerate() {
        let off = 40 + i * 4;
        wire[off..off + 4].copy_from_slice(&h.to_ne_bytes());
    }
    wire[48] = msg.n_caps;
    wire
}

/// Phase 74 Track A.2 / A.3 — `sys_ipc_call_with_caps(ep, msg_ptr, buf_ptr, buf_len)`.
///
/// Reads the full cap-bearing message from user memory, transfers any
/// `cap_slots[..n_caps]` capabilities to the receiver atomically, performs
/// an `ipc_call_buf`-shaped send-and-block, and on reply writes the
/// receiver-side cap handles back into `msg_ptr`'s `cap_slots`.
fn ipc_call_with_caps(
    task_id: crate::task::TaskId,
    ep_id: endpoint::EndpointId,
    msg_ptr: u64,
    buf_ptr: u64,
    buf_len: u64,
) -> u64 {
    use crate::task::scheduler;

    let mut msg = match read_cap_msg_from_user(msg_ptr) {
        Some(m) => m,
        None => return u64::MAX,
    };

    // Validate the cap_slots upfront — `ipc_transfer_caps` runs at delivery
    // time, but failing early here saves the bulk allocation on a bad
    // handle. Note: this only catches obviously-invalid handles; the
    // authoritative validate-and-transfer happens at rendezvous (see
    // [`endpoint::call_msg_with_caps`]).
    if msg.n_caps as usize > kernel_core::ipc::message::CAP_SLOTS_PER_MSG {
        return u64::MAX;
    }

    // Optional bulk payload (Phase 52 buf-bearing path).
    if buf_len > 0 {
        let len = buf_len as usize;
        if len > MAX_BULK_LEN {
            return u64::MAX;
        }
        let mut bulk = alloc::vec![0u8; len];
        if UserSliceRo::new(buf_ptr, bulk.len())
            .and_then(|s| s.copy_to_kernel(&mut bulk))
            .is_err()
        {
            return u64::MAX;
        }
        msg.data[1] = len as u64;
        scheduler::deliver_bulk(task_id, bulk);
    }

    let reply = endpoint::call_msg_with_caps(task_id, ep_id, msg);
    if reply.label == u64::MAX {
        return u64::MAX;
    }
    if !write_cap_msg_to_user(msg_ptr, &reply) {
        return u64::MAX;
    }
    reply.label
}

/// Phase 74 Track A.2 / A.3 — `sys_ipc_recv_with_caps(ep, msg_ptr, buf_ptr, buf_len)`.
///
/// Receives a message and writes the full cap-bearing IpcMessage
/// (including cap_slots filled with the receiver-side handles created at
/// transfer time) to `msg_ptr`.
fn ipc_recv_with_caps(
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
    if !write_cap_msg_to_user(msg_ptr, &msg) {
        return u64::MAX;
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

// ---------------------------------------------------------------------------
// Phase 74 Track C — IPC timeouts
// ---------------------------------------------------------------------------

/// Convert a userspace absolute CLOCK_MONOTONIC deadline (nanoseconds) into
/// the scheduler's tick clock. The kernel BSP tick rate is 1 ms (see
/// `arch::x86_64::interrupts::tick_count`), so the conversion divides by
/// 1 000 000 ns/tick. A `deadline_ns == 0` value is treated as a literal
/// "expire immediately" deadline (matches Linux's `clock_nanosleep(0)`
/// behaviour).
fn deadline_ns_to_ticks(deadline_ns: u64) -> u64 {
    deadline_ns / 1_000_000
}

/// Phase 74 Track C.1 — `sys_ipc_call_timeout(ep_cap, label, data0, deadline_ns)`.
///
/// Blocks waiting for an `ipc_call`-shaped reply with a deadline. Returns
/// the reply label on success or `NEG_ETIMEDOUT` if the deadline elapses
/// without a reply.
fn ipc_call_timeout(
    task_id: crate::task::TaskId,
    ep_id: endpoint::EndpointId,
    msg: message::Message,
    deadline_ns: u64,
) -> u64 {
    let deadline_ticks = deadline_ns_to_ticks(deadline_ns);
    endpoint::call_msg_with_deadline(task_id, ep_id, msg, Some(deadline_ticks)).label
}

/// Phase 74 Track C.1 — `sys_ipc_recv_timeout(ep_cap, deadline_ns)`.
///
/// Blocks waiting for a message on `ep_id` with a deadline. Returns the
/// received message label on success or `NEG_ETIMEDOUT` if no message
/// arrives before the deadline.
fn ipc_recv_timeout(
    task_id: crate::task::TaskId,
    ep_id: endpoint::EndpointId,
    deadline_ns: u64,
) -> u64 {
    let deadline_ticks = deadline_ns_to_ticks(deadline_ns);
    endpoint::recv_msg_with_deadline(task_id, ep_id, Some(deadline_ticks)).label
}
