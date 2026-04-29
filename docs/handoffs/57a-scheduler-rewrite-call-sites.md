---
Status: Complete
Source Ref: phase-57a / docs/roadmap/tasks/57a-scheduler-rewrite-tasks.md
Date: 2026-04-29
---

# Phase 57a — Track A.1: Block/Wake Call-Site Catalogue

## Why this exists

Track A.1 requires a complete inventory of every call site that invokes
`block_current_unless_woken_inner`, `block_current_unless_woken_until`,
`block_current_unless_woken`, any `block_current_unless_woken_with_*`
variants, and the broader `block_current_on_*` family that all bottom out
in the same v1 protocol machinery (`block_current` /
`block_current_unless_message`), as well as every caller of `wake_task`
and `scan_expired_wake_deadlines`. Without a complete inventory, a partial
migration leaves a mix of v1 and v2 protocol that produces the same race
class documented in
`docs/handoffs/2026-04-25-scheduler-design-comparison.md` and
`docs/handoff/2026-04-28-graphical-stack-startup.md`. This catalogue is
the source of truth for Track F's migration progress: every row below maps
to a Track F sub-task, and F.7 (delete v1 functions) cannot land until
every row is migrated.

---

## Proposed new buckets

No new F.x buckets are required. Every caller fits one of F.1–F.6. See the
notes in the table for callers that touch the boundary between buckets.

**Important note on `sys_nanosleep` (F.5 migration target):**
`sys_nanosleep` (`kernel/src/arch/x86_64/syscall/mod.rs:3162`) currently
does **not** call any `block_current_unless_woken*` primitive. For sleeps
≥ 5 ms it uses a TSC busy-spin with repeated `yield_now()` calls; for
sleeps < 5 ms it uses a pure TSC busy-spin. F.5 will migrate the ≥ 1 ms
branch to `block_current_until`. Until F.5 lands, `sys_nanosleep` has no
row in the block table (no existing call site to migrate), but it is
included in the wake table implicitly via the scheduler's
`scan_expired_wake_deadlines` path once F.5 adds a deadline.

**Important note on `block_current_unless_woken_with_*` variants:**
`git grep -n 'block_current_unless_woken_with' kernel/` returns zero
results. No `_with_*` variants exist in the current codebase.

---

## The v1 block primitive family

All blocking in the kernel currently routes through one of two internal
primitives in `kernel/src/task/scheduler.rs`:

- `block_current(state: TaskState)` — unconditional block (lines 1095–1136)
- `block_current_unless_message(state: TaskState)` — block unless a
  pending message is already queued (lines 1138–1166)

Public entry points are thin wrappers:

| Public symbol | Internal call | Introduced state |
|---|---|---|
| `block_current_on_recv()` | `block_current(BlockedOnRecv)` | BlockedOnRecv |
| `block_current_on_recv_unless_message()` | `block_current_unless_message(BlockedOnRecv)` | BlockedOnRecv |
| `block_current_on_send()` | `block_current(BlockedOnSend)` | BlockedOnSend |
| `block_current_on_send_unless_completed()` | custom (lines 1180–1213) | BlockedOnSend |
| `block_current_on_notif()` | `block_current(BlockedOnNotif)` | BlockedOnNotif |
| `block_current_on_notif_unless_message()` | `block_current_unless_message(BlockedOnNotif)` | BlockedOnNotif |
| `block_current_on_reply()` | `block_current(BlockedOnReply)` | BlockedOnReply |
| `block_current_on_reply_unless_message()` | `block_current_unless_message(BlockedOnReply)` | BlockedOnReply |
| `block_current_on_futex()` | `block_current(BlockedOnFutex)` | BlockedOnFutex |
| `block_current_on_futex_unless_woken(woken)` | custom (lines 1240–1268) | BlockedOnFutex |
| `block_current_unless_woken(woken)` | `block_current_unless_woken_inner(woken, None)` | BlockedOnRecv |
| `block_current_unless_woken_until(woken, deadline)` | `block_current_unless_woken_inner(woken, Some(deadline))` | BlockedOnRecv |

The v2 rewrite targets the single new primitive `block_current_until`
(Track C) to replace all entries in this table.

---

## Block-side call-site table

Columns: **Callee** = the v1 primitive invoked; **Caller (file:line)** =
exact location found by `git grep -n` from the worktree root;
**Block kind** = the taxonomy from the task list;
**Wake side** = what entity sets the task Ready;
**F.x bucket** = the Track F migration sub-task.

| Callee | Caller (file:line) | Block kind | Wake side | F.x bucket |
|---|---|---|---|---|
| `block_current_on_recv_unless_message` | `kernel/src/ipc/endpoint.rs:389` (`recv_msg`) | `recv` | Sender calls `endpoint::reply` or `endpoint::send` → `wake_task` in `endpoint.rs` | F.1 |
| `block_current_on_notif_unless_message` | `kernel/src/ipc/endpoint.rs:612` (`recv_msg_with_notif`) | `recv` / `notif` | Notification signal via `notification::signal` / `signal_irq` → `wake_task` in `notification.rs:538,742`; or sender → `wake_task` in `endpoint.rs` | F.1 |
| `block_current_on_send_unless_completed` | `kernel/src/ipc/endpoint.rs:716` (`send`) | `send` | Receiver calls `recv_msg` which calls `scheduler::deliver_message` + `wake_task` in `endpoint.rs:380,464,483,551,564,580,784` | F.1 |
| `block_current_on_reply_unless_message` | `kernel/src/ipc/endpoint.rs:796` (`call_msg`) | `reply` | Server calls `endpoint::reply` → `wake_task` in `endpoint.rs:853` | F.1 |
| `block_current_on_send_unless_completed` | `kernel/src/ipc/endpoint.rs:993` (`send_with_cap`) | `send` | Receiver calls `recv_msg` which calls `scheduler::deliver_message` + `wake_task` in `endpoint.rs:976,986` | F.1 |
| `block_current_on_notif` | `kernel/src/ipc/notification.rs:804` (`notification::wait`) | `notif` | `notification::signal` or `notification::signal_irq` → `wake_task` in `notification.rs:538,742`; or `drain_pending_waiters` at `notification.rs:742` | F.2 |
| `block_current_on_futex_unless_woken` | `kernel/src/arch/x86_64/syscall/mod.rs:12449` (`sys_futex`, `FUTEX_WAIT`/`FUTEX_WAIT_BITSET` branch) | `futex` | `sys_futex` `FUTEX_WAKE`/`FUTEX_WAKE_BITSET` branch: sets `woken_flag`, calls `wake_task` at `syscall/mod.rs:12499` | F.3 |
| `block_current_unless_woken` | `kernel/src/arch/x86_64/syscall/mod.rs:14763` (`sys_poll`, indefinite-timeout branch) | `poll` | FD wait-queue `WaitQueue::wake_one` / `wake_all` → `wake_task` at `wait_queue.rs:81,93`; or `fd_deregister_waiter` wake path | F.4 |
| `block_current_unless_woken` | `kernel/src/arch/x86_64/syscall/mod.rs:15019` (`select_inner`, indefinite-timeout branch) | `select` | FD wait-queue `WaitQueue::wake_one` / `wake_all` → `wake_task` at `wait_queue.rs:81,93` | F.4 |
| `block_current_unless_woken` | `kernel/src/arch/x86_64/syscall/mod.rs:15432` (`sys_epoll_wait`, indefinite-timeout branch) | `epoll` | FD wait-queue `WaitQueue::wake_one` / `wake_all` → `wake_task` at `wait_queue.rs:81,93` | F.4 |
| `block_current_unless_woken` | `kernel/src/main.rs:648` (`net_task`) | `driver-irq` | virtio-net ISR → `virtio_net::wake_net_task` → `wake_task` at `net/virtio_net.rs:490`; or ring-3 e1000 `RemoteNic::inject_rx_frame` → same path | F.6 |
| `block_current_unless_woken` | `kernel/src/task/wait_queue.rs:56` (`WaitQueue::sleep`) | `wait_queue` | `WaitQueue::wake_one` → `wake_task` at `wait_queue.rs:81`; `WaitQueue::wake_all` → `wake_task` at `wait_queue.rs:93` | F.6 |
| `serial_stdin_feeder_task` (indirect) | `kernel/src/main.rs:515` (`serial_stdin_feeder_task`, `enable_and_hlt` in inner loop) | `driver-irq` | COM1 RX IRQ fires, `enable_and_hlt` returns | H.1 migrates to notification-based wait, then bottoms out in F.6 |

### Notes on the indefinite vs. timeout branches

`sys_poll`, `select_inner`, and `sys_epoll_wait` each have two blocking paths:
- **Positive-timeout branch** (e.g. `sys_poll` when `timeout_i > 0`): calls
  `yield_now()` in a loop and checks the tick-count deadline at the top of
  each iteration. This path does NOT call `block_current_unless_woken` and
  is instead migrated as part of F.4 + G.3 together (the v2 primitive
  replaces `yield_now` and the `÷ 10` deadline arithmetic is fixed at the
  same time).
- **Indefinite-timeout branch** (`timeout_i < 0` / `NULL`): calls
  `block_current_unless_woken`. This is the row catalogued in the table
  above.

Both branches must be migrated in F.4 (they cannot be split between tasks
without a window of mixed-protocol state).

---

## `sys_nanosleep` — F.5 pre-migration note

`sys_nanosleep` at `kernel/src/arch/x86_64/syscall/mod.rs:3162` uses
`yield_now()` and TSC busy-spin. **No existing `block_current_unless_woken*`
call site exists today** — F.5 creates the call site. After F.5 lands,
this table gains a new row:

| `block_current_until` (new) | `sys_nanosleep` (≥ 1 ms branch) | `nanosleep` | `scan_expired_wake_deadlines` (timer path) | F.5 |

---

## Wake-side call-site table

These are all `wake_task` / `scan_expired_wake_deadlines` callers. They are
the wake counterpart to every block above. Tracks D and F.1–F.6 must
migrate the wake side to the CAS-style `wake_task` before F.7 can delete
v1.

| Callee | Caller (file:line) | Context | Wakes block kind | F.x bucket |
|---|---|---|---|---|
| `wake_task` | `kernel/src/ipc/endpoint.rs:335` (`recv_msg`, pending-sender fast-path) | sender already queued, receiver just arrived | `send` | F.1 |
| `wake_task` | `kernel/src/ipc/endpoint.rs:354` (`recv_msg`, pending-sender fast-path) | sender already queued, receiver just arrived | `send` | F.1 |
| `wake_task` | `kernel/src/ipc/endpoint.rs:380` (`recv_msg`, no-reply send complete) | non-reply send completes | `send` | F.1 |
| `wake_task` | `kernel/src/ipc/endpoint.rs:447` (`recv_msg_nowait`) | drain pending-sender queue | `send` | F.1 |
| `wake_task` | `kernel/src/ipc/endpoint.rs:464` (`recv_msg_nowait`) | drain pending-sender queue | `send` | F.1 |
| `wake_task` | `kernel/src/ipc/endpoint.rs:483` (`recv_msg_nowait`) | drain pending-sender queue | `send` | F.1 |
| `wake_task` | `kernel/src/ipc/endpoint.rs:551` (`recv_msg_with_notif`, pending-sender fast-path) | sender already queued, notif-receiver just arrived | `send` | F.1 |
| `wake_task` | `kernel/src/ipc/endpoint.rs:564` (`recv_msg_with_notif`, pending-sender fast-path) | sender already queued, notif-receiver just arrived | `send` | F.1 |
| `wake_task` | `kernel/src/ipc/endpoint.rs:580` (`recv_msg_with_notif`, no-reply send complete) | non-reply send completes | `send` | F.1 |
| `wake_task` | `kernel/src/ipc/endpoint.rs:702` (`send`, receiver-already-waiting fast-path) | receiver was waiting, sender delivers immediately | `recv` | F.1 |
| `wake_task` | `kernel/src/ipc/endpoint.rs:778` (`call_msg`, receiver-already-waiting fast-path) | receiver was waiting, call-sender delivers immediately | `recv` | F.1 |
| `wake_task` | `kernel/src/ipc/endpoint.rs:784` (`call_msg`, receiver-already-waiting fast-path) | receiver was waiting, call-sender delivers immediately | `recv` | F.1 |
| `wake_task` | `kernel/src/ipc/endpoint.rs:853` (`reply`) | server replies to blocked caller | `reply` | F.1 |
| `wake_task` | `kernel/src/ipc/endpoint.rs:976` (`send_with_cap`, receiver-already-waiting fast-path) | capped send, receiver immediately woken | `recv` | F.1 |
| `wake_task` | `kernel/src/ipc/endpoint.rs:986` (`send_with_cap`, receiver-already-waiting fast-path) | capped send, receiver immediately woken | `recv` | F.1 |
| `wake_task` | `kernel/src/ipc/cleanup.rs:95` (`cleanup_task_ipc`, stranded-sender loop) | endpoint closed while sender was blocked | `send` | F.1 |
| `wake_task` | `kernel/src/ipc/cleanup.rs:99` (`cleanup_task_ipc`, stranded-receiver loop) | endpoint closed while receiver was blocked | `recv` | F.1 |
| `wake_task` | `kernel/src/ipc/cleanup.rs:104` (`cleanup_task_ipc`, reply-waiter loop) | endpoint closed while caller was blocked on reply | `reply` | F.1 |
| `wake_task` | `kernel/src/ipc/notification.rs:538` (`notification::signal`) | notification bits signalled from task context | `notif` | F.2 |
| `wake_task` | `kernel/src/ipc/notification.rs:742` (`notification::drain_pending_waiters`) | periodic waiter drain from kernel context | `notif` | F.2 |
| `wake_task` | `kernel/src/arch/x86_64/syscall/mod.rs:2045` (`do_clear_child_tid`) | `pthread_join` futex wake on thread exit | `futex` | F.3 |
| `wake_task` | `kernel/src/arch/x86_64/syscall/mod.rs:12499` (`sys_futex`, `FUTEX_WAKE`/`FUTEX_WAKE_BITSET` branch) | userspace futex wake syscall | `futex` | F.3 |
| `wake_task` | `kernel/src/task/wait_queue.rs:81` (`WaitQueue::wake_one`) | single-waiter FD/pipe event | `wait_queue` / `poll` / `select` / `epoll` | F.4 / F.6 |
| `wake_task` | `kernel/src/task/wait_queue.rs:93` (`WaitQueue::wake_all`) | broadcast FD/pipe event | `wait_queue` / `poll` / `select` / `epoll` | F.4 / F.6 |
| `wake_task` | `kernel/src/blk/virtio_blk.rs:362` (`drain_used_from_irq`) | virtio-blk completion IRQ; wakes the userspace task that submitted the I/O | `driver-irq` | F.6 |
| `wake_task` | `kernel/src/net/virtio_net.rs:490` (`wake_net_task`) | virtio-net / ring-3 e1000 RX IRQ | `driver-irq` | F.6 |
| `wake_task` | `kernel/src/process/mod.rs:1382` (`interrupt_ipc_waits`) | signal delivery interrupts a blocked IPC syscall (EINTR path) | `recv` / `send` / `reply` | F.1 |
| `scan_expired_wake_deadlines` | `kernel/src/task/scheduler.rs:1843` (`scheduler::run` main dispatch loop) | timer-driven deadline expiry; wakes tasks with elapsed `wake_deadline` | `nanosleep` / `poll` / `select` / `epoll` (any timed block) | D.4 / F.4 / F.5 |

---

## Validation cross-check

The following commands, run from the worktree root, were used to produce
this table and confirm completeness:

```
# All block_current_unless_woken* callers outside scheduler.rs:
git grep -n 'block_current_unless_woken' kernel/ | grep -v 'task/scheduler.rs:'

# Any block_current_unless_woken_with_* variants (expect no results):
git grep -n 'block_current_unless_woken_with' kernel/

# All wake_task and scan_expired_wake_deadlines callers:
git grep -nE 'wake_task\(|scan_expired_wake_deadlines' kernel/

# All block_current_on_* callers outside scheduler.rs:
git grep -rn 'block_current_on_' kernel/src/ | grep -v 'task/scheduler.rs'
```

Every result from these commands appears in the tables above.

---

## Migration readiness summary

| F.x bucket | Call sites to migrate | Current primitive(s) | Notes |
|---|---|---|---|
| F.1 (IPC) | `endpoint.rs:389,612,716,796,993` (5 block sites); `endpoint.rs:335,354,380,447,464,483,551,564,580,702,778,784,853,976,986` + `cleanup.rs:95,99,104` + `process/mod.rs:1382` (18 wake sites) | `block_current_on_{recv,send,reply,notif}_unless_{message,completed}` | Also covers `ipc_recv_msg`, `ipc_send_with_bulk`, `ipc_call_buf` (via `endpoint::recv_msg` and `endpoint::call`) |
| F.2 (notification) | `notification.rs:804` (1 block site); `notification.rs:538,742` (2 wake sites) | `block_current_on_notif` | IPC dispatch number 7 (`notify_wait`) routes to `notification::wait` |
| F.3 (futex) | `syscall/mod.rs:12449` (1 block site); `syscall/mod.rs:2045,12499` (2 wake sites) | `block_current_on_futex_unless_woken` | |
| F.4 (I/O mux) | `syscall/mod.rs:14763,15019,15432` (3 block sites, indefinite-timeout branches); positive-timeout `yield_now` loops also in `sys_poll`, `select_inner`, `sys_epoll_wait` | `block_current_unless_woken` (indefinite); `yield_now` loop (positive timeout) | Must land together with G.3 (÷10 multiplier fix) |
| F.5 (nanosleep) | no existing block call site — F.5 creates it | `yield_now` / TSC busy-spin | `sys_nanosleep` at `syscall/mod.rs:3162` |
| F.6 (kernel-internal) | `main.rs:648` (`net_task`), `wait_queue.rs:56` (`WaitQueue::sleep`) (2 block sites); `virtio_blk.rs:362`, `virtio_net.rs:490` (2 IRQ wake sites) | `block_current_unless_woken` | `serial_stdin_feeder_task` is H.1 (not F.6) |
| H.1 (serial feeder) | `main.rs:515` (`enable_and_hlt` loop) | `enable_and_hlt` (not a `block_current*` call) | Migrates to notification wait → eventually bottoms out in F.6 `block_current_until` |

**Total block call sites: 12** (excluding the H.1 indirect and the
F.5 site that does not yet exist).
**Total wake call sites: 25** (`wake_task` callers) + 1 (`scan_expired_wake_deadlines`).
