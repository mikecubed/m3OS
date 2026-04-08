# IPC Core

**Aligned Roadmap Phase:** Phase 6
**Status:** Complete
**Source Ref:** phase-06
**Supersedes Legacy Docs:** `docs/06-ipc.md` (design rationale), `docs/06-ipc-core.md` (implementation)

## Overview

Phase 6 turns m³OS into a real microkernel by wiring up the IPC subsystem
that all future userspace servers depend on.  By the end of this phase the
kernel:

- transfers messages between tasks through synchronous rendezvous endpoints,
- validates every capability handle on every syscall so no task can forge
  access to a kernel object it was never granted,
- delivers hardware IRQs to userspace drivers through asynchronous notification
  objects, and
- exposes the complete IPC surface via syscall numbers 1–5 and 7–8.

Everything from Phase 7 onward — the shell, console server, VFS — sits on top
of this foundation.  If the IPC contract is clean, those higher layers are
straightforward.  If it is muddled, nothing above it is trustworthy.

---

## Why Microkernels Use Explicit Message Passing

A monolithic kernel like Linux runs drivers, filesystems, and network stacks
inside ring 0.  A bug in any of them can corrupt kernel state, panic the
machine, or escalate privilege.

A microkernel moves those services into ring-3 processes.  The kernel becomes
a thin layer that provides only:

- memory management (frame allocator, page tables),
- scheduling (who runs, when),
- IPC (how processes talk to each other), and
- interrupt routing (delivering hardware events to the right process).

The price of this isolation is that a service can no longer call a driver
through a function pointer.  Instead it must ask the kernel to deliver a
message to the driver's endpoint.  The kernel validates the request, blocks the
caller, wakes the driver, copies the message, and unblocks the caller with the
reply.  This is more overhead than a direct call — but only slightly, because a
well-designed IPC path requires no allocation and fits in CPU registers.

The benefit is that a crashed driver does not corrupt the kernel.  The kernel
kills the task, cleans up its capabilities, and the supervisor can restart it.

---

## IPC Model: Synchronous Rendezvous + Async Notifications

m³OS follows the **seL4 model**:

### Synchronous Rendezvous

Both sender and receiver must be ready before any transfer happens.  The kernel
copies the message directly through registers — no intermediate buffer, no heap
allocation on the hot path.  Whichever party arrives first at the endpoint
blocks until its counterpart shows up.

This fits every server use case in m³OS perfectly.  Every interaction between
the shell and its servers (`console_server`, `vfs_server`) is request-response
by nature.  The shell blocks waiting for results; there is no benefit to
decoupling the two sides with a buffered channel.

### Async Notification Objects

One pattern is genuinely asynchronous: hardware interrupts.  A keypress fires
at an unpredictable time; the keyboard driver cannot afford to spin-poll in a
tight loop.

A `Notification` is a single machine-word bitfield.  Each bit is an independent
signal channel:

```text
Notification: [ bit63 | ... | bit2 | bit1 | bit0 ]
                                            ^
                                     IRQ1 (keyboard)
```

The kernel's interrupt handler sets a bit atomically (`fetch_or`) — no lock,
no allocation, safe to call from any interrupt handler.  A driver thread waits
on the notification object; when a bit is set the scheduler wakes it.

### Why Not Full Async Channels?

Async ring-buffer channels require kernel-managed buffers (heap allocation on
the hot IPC path), buffer-full/empty conditions, and a separate wakeup
mechanism anyway.  They add significant complexity for no benefit given the
synchronous request-response pattern all m³OS services use.

### Bulk Data Transfers

IPC carries **control messages only** — never pixel data or file block contents.
For large transfers the pattern is:

1. Transfer a physical page into the receiver's address space via a **page
   capability grant** — atomic, kernel-mediated, zero-copy.
2. Use sync IPC to signal "data is ready in the shared region."

Page capability grants are deferred to Phase 7+.  In Phase 6 all data fits in
the four-word inline payload.

---

## Message Format

```rust
pub struct Message {
    pub label: u64,       // operation identifier (method selector)
    pub data:  [u64; 4],  // up to 4 words of inline payload
}
```

A `Message` is 40 bytes — five 64-bit registers.  It transfers entirely through
CPU registers: no pointer, no allocation, no cache miss.

`label` is a convention between sender and receiver: it identifies which
operation is being requested (analogous to a method ID in Mach or a message tag
in seL4).  `data` carries the arguments or the result.

The constructors match the number of data words used:

| Constructor | Data words set |
|---|---|
| `Message::new(label)` | none (all zero) |
| `Message::with1(label, d0)` | `data[0]` |
| `Message::with2(label, d0, d1)` | `data[0..1]` |

Capability grants in the message payload are deferred to Phase 7+.  For now, if
a server needs to share memory with a client it must use a pre-arranged shared
address.

---

## Capability Table

### What a Capability Is

A **capability** is an unforgeable token that grants the holder specific rights
to a kernel object.  In m³OS, capabilities are integer handles — indices into
a per-task `CapabilityTable`.  The kernel validates every handle on every IPC
syscall.  Passing an out-of-range or empty-slot index returns `u64::MAX`
immediately; no kernel state changes.

```mermaid
graph TD
    P1["Process A<br/>cap_table[3]"] -->|"Endpoint cap"| EP["Endpoint<br/>(console_server)"]
    P2["Process B<br/>cap_table[1]"] -->|"Endpoint cap"| EP
    P3["init<br/>cap_table[0]"] -->|"Endpoint cap"| EP
    EP -->|"owned by"| CS["console_server task"]

    style EP fill:#8e44ad,color:#fff
```

### Phase 6 Capability Variants

| Variant | What it grants |
|---|---|
| `Capability::Endpoint(EndpointId)` | Send or receive on a specific IPC endpoint |
| `Capability::Notification(NotifId)` | Signal or wait on a notification object |
| `Capability::Reply(TaskId)` | One-shot right to reply to a specific blocked caller |

`Reply` capabilities are ephemeral.  The kernel inserts one into the server's
table when it delivers a `call` message; `reply` or `reply_recv` consumes it.
Attempting a second reply returns `u64::MAX` because the slot is already empty.

### Table Layout

Each task holds a fixed 64-slot table (`CapabilityTable::SIZE = 64`) allocated
alongside the task structure.  `insert` scans for the first `None` slot;
`remove` clears the slot.  A `TableFull` error is returned if all 64 slots are
occupied — this should not occur in a teaching OS with a handful of services.

Capability delegation (`sys_cap_grant`, transferring a capability to another
task via IPC) is deferred to Phase 7+.

---

## Endpoint Operations

An `Endpoint` is a kernel object that holds two queues:

- **`senders`** — tasks blocked in `send` or `call`, each with a pending
  `Message` and a `wants_reply` flag.
- **`receivers`** — tasks blocked in `recv`, waiting for any sender.

Up to 16 endpoints can exist simultaneously (`MAX_ENDPOINTS = 16`).

### Operations Summary

| Operation | Caller | Effect |
|---|---|---|
| `recv(ep)` | Server | Block until a sender arrives; dequeue and return its message |
| `send(ep, msg)` | Client | Block until a receiver is ready; deliver message |
| `call(ep, msg)` | Client | `send` + block waiting for a `Reply` cap to be consumed |
| `reply(reply_cap, msg)` | Server | Wake the blocked caller and deliver reply message |
| `reply_recv(reply_cap, msg, ep)` | Server | `reply` + immediately `recv` next message |

### Send Path

```mermaid
sequenceDiagram
    participant Client
    participant Kernel
    participant Server

    Client->>Kernel: ipc_send(ep_cap, label, data)
    alt receiver already waiting
        Kernel->>Server: deliver_message(msg)<br/>wake_task(server)
    else no receiver yet
        Kernel->>Client: enqueue in senders<br/>block_current_on_send()
        Server->>Kernel: ipc_recv(ep_cap)
        Kernel->>Client: wake_task(client)
        Kernel->>Server: deliver_message(msg)
    end
```

### Call / Reply Path

`call` is the RPC pattern: send a message and wait for a reply.  The kernel
inserts a one-shot `Reply` capability into the server's table instead of
immediately waking the caller.

```mermaid
sequenceDiagram
    participant Client
    participant Kernel
    participant Server

    Client->>Kernel: ipc_call(ep_cap, label, data)
    Kernel->>Server: deliver_message(msg)
    Kernel->>Server: insert Reply(client) cap into server table
    Kernel->>Server: wake_task(server)
    Kernel->>Client: block_current_on_reply()

    Server->>Kernel: ipc_reply(reply_cap, label, data)
    Note over Kernel: reply cap consumed (slot cleared)
    Kernel->>Client: deliver_message(reply)<br/>wake_task(client)
    Client->>Client: take_message() → reply label
```

### reply_recv Server Loop

A server that handles many clients back-to-back uses `reply_recv` to atomically
reply to the current caller and start waiting for the next one — all in a
single syscall:

```mermaid
sequenceDiagram
    participant Client
    participant Kernel
    participant Server

    Server->>Kernel: ipc_recv(ep_cap) — wait for first client
    Client->>Kernel: ipc_call(ep_cap, REQ, data)
    Kernel->>Server: deliver + Reply cap + wake
    Server->>Server: handle request
    Server->>Kernel: ipc_reply_recv(reply_cap, RESP, data, ep_cap)
    Note over Kernel: reply to client + recv next in one step
    Kernel->>Client: wake with reply
    Kernel->>Server: block_current_on_recv()
```

The equivalent server loop in pseudocode:

```text
server loop:
    label = ipc_recv(my_ep)            // wait for first client
    loop:
        response = handle(label)
        label = ipc_reply_recv(reply_cap, response, my_ep)  // reply + wait next
```

This is more efficient than separate `reply` + `recv` syscalls because the
server thread does not return to the scheduler between the two operations.

---

## Notification Objects

### Structure

```rust
pub struct Notification {
    pending: AtomicU64,        // bitfield of undelivered signals
    waiter:  Option<TaskId>,   // at most one blocked waiter
}
```

Up to 16 notification objects can exist simultaneously (`MAX_NOTIFS = 16`).
Each object maps to a `NotifId` that is stored in a `Capability::Notification`
slot.

### signal_irq (ISR-safe)

`signal_irq(irq)` looks up the registered `NotifId` for the hardware IRQ line
and performs an atomic `fetch_or` on the matching `PENDING` slot — no lock
is held, so it is safe to call directly from an interrupt handler.
It then calls `signal_reschedule()` so the blocked task runs on the next
scheduler tick.  It does **not** call `wake_task()`.

### signal (task-context only)

`signal(notif_id, bits)` performs the same lock-free `fetch_or` on `PENDING`,
then additionally acquires `WAITERS.lock()` to wake the blocked task.  Because
it takes a spin lock, it **must not** be called from an interrupt handler —
use `signal_irq` instead.

### wait (blocking)

`wait(task_id, notif_id)` atomically swaps the `pending` field to zero.  If the
result is non-zero it returns immediately.  Otherwise it registers the calling
task as the waiter and calls `block_current_on_notif()` (sets
`TaskState::BlockedOnNotif`).  On wake it loops back to drain the bits (to
handle a signal that arrived between the first swap and the block).

```mermaid
sequenceDiagram
    participant Driver
    participant Kernel

    Driver->>Kernel: notify_wait(notif_cap)
    alt bits already pending
        Kernel->>Driver: return bits immediately
    else no bits pending
        Kernel->>Driver: register waiter<br/>block_current_on_notif()
        Note over Kernel: time passes ...
        Kernel->>Kernel: signal_irq() sets bit in PENDING
        Kernel->>Kernel: signal_reschedule()
        Driver->>Kernel: loop: swap PENDING → return bits
    end
```

---

## IRQ Delivery

Hardware interrupts must reach userspace drivers without going through the
synchronous IPC path (which would require the kernel to block on a send, which
is never safe inside an interrupt handler).

The pattern:

1. At startup `kbd_server` allocates a `Notification` and calls `register_irq(1,
   notif_id)` to bind IRQ1 to it.
2. Every time IRQ1 fires, the kernel IDT handler calls `signal_irq(1)`.
3. `signal_irq` looks up the registered `NotifId` for IRQ1 in `IRQ_MAP`
   (a lock-free `AtomicU8` array) and atomically sets bit 1 in `PENDING[idx]`
   via `fetch_or`.  It then calls `signal_reschedule()` — **no lock is
   acquired, `wake_task()` is never called from the ISR**.
4. On its next scheduler dispatch, `kbd_server` returns from
   `block_current_on_notif()`, loops back in `wait()`, drains `PENDING`, and
   returns the accumulated bits.
5. The driver reads the scancode from I/O port `0x60`, translates it, and sends
   a key-event message to `console_server` via sync IPC.

```mermaid
sequenceDiagram
    participant HW as Hardware
    participant IDT as Kernel IDT handler
    participant Notif as Notification object
    participant KbdServer as kbd_server

    KbdServer->>Notif: notify_wait(notif_cap) — block_current_on_notif()
    HW->>IDT: IRQ1 fires
    IDT->>Notif: signal_irq(1): PENDING[idx].fetch_or(bit1)<br/>signal_reschedule()
    Note over Notif: lock-free — no wake_task in ISR
    Notif->>KbdServer: scheduler dispatches kbd_server
    KbdServer->>KbdServer: wait() loop: swap PENDING → bits
    KbdServer->>KbdServer: in(0x60) → scancode
    KbdServer->>KbdServer: translate → key event
    KbdServer->>KbdServer: ipc_call(console_ep, KEY_EVENT, scancode)
```

This is the only place in the kernel where an interrupt handler triggers a
scheduler operation.  Because `signal_irq` uses only lock-free atomics and
`signal_reschedule()` (which writes a single atomic flag), it is safe to call
from inside an ISR.

---

## Scheduler Integration

### Task States Added in Phase 6

Phase 4 introduced `Ready` and `Running`.  Phase 6 adds four blocked states:

```mermaid
stateDiagram-v2
    [*] --> Ready : spawn()
    Ready --> Running : scheduler dispatches
    Running --> Ready : yield_now()
    Running --> BlockedOnSend : send() — no receiver yet
    Running --> BlockedOnRecv : recv() — no sender yet
    Running --> BlockedOnReply : call() — waiting for reply
    Running --> BlockedOnNotif : notify_wait() — no bits set
    BlockedOnSend --> Ready : receiver picks up the message
    BlockedOnRecv --> Ready : sender delivers a message
    BlockedOnReply --> Ready : reply() wakes caller
    BlockedOnNotif --> Ready : signal() sets a bit
```

| State | Set by | Cleared by |
|---|---|---|
| `BlockedOnSend` | `block_current_on_send()` | `wake_task()` when a receiver picks up |
| `BlockedOnRecv` | `block_current_on_recv()` | `wake_task()` when a sender delivers |
| `BlockedOnReply` | `block_current_on_reply()` | `wake_task()` from `reply()` |
| `BlockedOnNotif` | `block_current_on_notif()` | `signal_reschedule()` (ISR) or `wake_task()` from `signal()` |

### How the "block and switch" primitives work

All four block primitives (`block_current_on_recv`, `block_current_on_send`,
`block_current_on_reply`, `block_current_on_notif`) follow the same pattern,
illustrated here for `block_current_on_notif`:

```rust
pub fn block_current_on_notif() {
    let task_rsp_ptr: *mut u64 = {
        let mut sched = SCHEDULER.lock();
        let idx = sched.current.unwrap();
        sched.tasks[idx].state = TaskState::BlockedOnNotif;
        sched.current = None;
        core::ptr::addr_of_mut!(sched.tasks[idx].saved_rsp)
        // lock released here
    };
    let sched_rsp = unsafe { SCHEDULER_RSP };
    unsafe { switch_context(task_rsp_ptr, sched_rsp) };
}
```

Key details:

1. **Lock released before `switch_context`** — the scheduler loop also acquires
   `SCHEDULER` when picking the next task.  If we held the lock across
   `switch_context`, the scheduler loop would deadlock.
2. **`addr_of_mut!` avoids a &mut reference** — creating `&mut Task` through a
   `Mutex` guard would violate aliasing rules if the guard is dropped while the
   reference is live.  The raw pointer is safe because the `Task` outlives the
   switch.
3. **State is set before the switch** — the scheduler loop reads `.state` to
   decide what is runnable.  Setting the blocked state before releasing the lock
   ensures the scheduler never sees this task as `Ready` while it is mid-block.

### Message Delivery

The IPC core cannot copy a message directly into a task's registers — the task
is blocked and its register state is saved on its kernel stack.  Instead the
scheduler provides a per-task `pending_msg: Option<Message>` slot:

1. `deliver_message(task_id, msg)` — stores the message in the slot.
2. `wake_task(task_id)` — transitions the task to `Ready`.
3. When the scheduler dispatches the task, it continues executing after the
   `switch_context` call inside the relevant block primitive.
4. The task then calls `take_message(task_id)` to drain the slot and return
   the label to the caller.

---

## Syscall ABI

IPC syscalls follow the register convention established in Phase 5:

| Register | Role |
|---|---|
| `rax` | Syscall number (in) / return value (out) |
| `rdi` | Argument 0 (primary capability handle) |
| `rsi` | Argument 1 |
| `rdx` | Argument 2 |
| `r10` | Argument 3 |
| `r8` | Argument 4 |

`rcx` and `r11` are clobbered by `syscall`/`sysret` — never use them for
arguments.

### IPC Syscall Table (Phase 6)

| Number | Name | Arguments | Return |
|---|---|---|---|
| 1 | `ipc_recv` | `rdi` = ep_cap_handle | message label, or `u64::MAX` on error |
| 2 | `ipc_send` | `rdi` = ep_cap, `rsi` = label, `rdx` = data[0] | `0` on success, `u64::MAX` on error |
| 3 | `ipc_call` | `rdi` = ep_cap, `rsi` = label, `rdx` = data[0] | reply label, or `u64::MAX` on error |
| 4 | `ipc_reply` | `rdi` = reply_cap, `rsi` = label, `rdx` = data[0] | `0` on success, `u64::MAX` on error |
| 5 | `ipc_reply_recv` | `rdi` = reply_cap, `rsi` = label, `rdx` = ep_cap¹ | next message label, or `u64::MAX` on error |
| 7 | `notify_wait` | `rdi` = notif_cap_handle | pending bits (non-zero on success), or `0` on error |
| 8 | `notify_signal` | `rdi` = notif_cap_handle, `rsi` = bits | `0` on success, `u64::MAX` on error |

¹ The Phase 6 asm stub forwards only 3 arguments (rdi/rsi/rdx).  `ipc_reply_recv`
therefore packs the endpoint cap handle into `rdx` (arg2) rather than the
full SysV `r8` (arg4) position.  The reply payload (`data[0]`) is not
forwarded in the syscall form; kernel threads use the Rust API directly.

Note: in the original Phase 6 design, syscall number 6 was `sys_exit` (Phase 5)
and syscall number 12 was `sys_debug_print` (Phase 5), with a contiguous range
of syscall numbers reserved for IPC.  In the current implementation, the syscall
table follows a Linux-like layout (e.g., `1 = write`, `2 = open`, etc.); the
authoritative mapping lives in `kernel/src/arch/x86_64/syscall.rs`, and
userspace uses it via `userspace/syscall-lib`.  Treat the table above as
describing the logical IPC interface; consult the code for the actual syscall
numbers or call paths.

### Error Convention

Error returns are per-syscall:

- **Rendezvous IPC calls** (e.g., `ipc_call`, `ipc_reply`, `ipc_reply_recv`):
  return `u64::MAX` on any error (invalid handle, wrong capability type,
  capability table full).
- **`notify_wait`:** returns `0` on error (invalid handle or wrong type).
  A return of `0` cannot be a valid success value because `wait()` only returns
  when at least one pending bit is set.
- **`notify_signal`:** returns `u64::MAX` on error, `0` on success.

`u64::MAX` is chosen for rendezvous errors because it cannot be a valid message
label, clearly distinguishing success from failure without a separate register.

---

## Bulk-Data Transport

IPC messages carry control data only — up to 4 machine words (32 bytes) of
inline payload.  Bulk data such as file contents, framebuffer spans, and
network packets uses a separate mechanism that bypasses the register-based
message path entirely.

### Hybrid Model: copy_from_user + Page Grants

m3OS uses a two-tier bulk-data strategy selected by payload size:

| Tier | Payload size | Mechanism | Latency |
|---|---|---|---|
| **Small copy** | 0 – 64 KiB | `copy_from_user` / `copy_to_user` | Low (memcpy through validated page tables) |
| **Page grant** | > 64 KiB | `Capability::Grant` page transfer | Near-zero (remap, no copy) |

**Small-copy path** — The kernel validates the user-space buffer address
(must be above `0x1000`, below `0x0000_8000_0000_0000`, no wraparound, length
<= 64 KiB) and then uses `copy_from_user` / `copy_to_user` (implemented in
`kernel/src/mm/user_mem.rs`) to transfer bytes through the caller's page
tables.  This is the common path for the vast majority of IPC payloads.

**Page-grant path** — For transfers larger than 64 KiB (primarily framebuffer
spans), the sender grants one or more physical page frames to the receiver via
a `Capability::Grant { frame, page_count, writable }` capability.  The kernel
remaps the pages into the receiver's address space; no byte-copying occurs.
Ownership transfers atomically: the sender loses access when the grant
succeeds.

### Payload Coverage

The hybrid model covers every bulk-data type in the system:

| Payload type | Typical size | Tier |
|---|---|---|
| Service-name strings (registry lookup) | 1 – 32 B | Small copy |
| VFS paths | up to 4 KiB | Small copy |
| Console write buffers | up to 4 KiB | Small copy |
| Network packets (Ethernet MTU) | up to 1500 B | Small copy |
| FAT32 disk blocks | 512 B – 64 KiB | Small copy |
| Framebuffer spans | 4 KiB – 8 MiB | Page grant |

### Ownership Rules

1. **Allocator** — The sender allocates the buffer (either a user-space heap
   buffer for small copies, or physical frames for page grants).
2. **Lifetime** — For small copies, the kernel copies synchronously during the
   syscall; the sender may reuse or free its buffer immediately after the
   syscall returns.  For page grants, ownership transfers to the receiver on
   successful grant; the sender must not access the pages afterward.
3. **Service crash** — If a service holding granted pages crashes, the kernel
   reclaims the pages during task cleanup (the same path that reclaims all
   task-owned physical memory).  Small-copy buffers are ordinary user-space
   allocations and are freed with the process address space.
4. **Grant-of-grant** — A receiver that holds a `Grant` capability may
   further grant it to another task.  Ownership chains are implicit; the
   kernel tracks only the current holder.

### Buffer Validation

Before any `copy_from_user` / `copy_to_user`, the kernel calls
`validate_user_buffer(addr, len)` (defined in `kernel-core/src/ipc/buffer.rs`)
to perform pure-logic address checks:

- Address must be in the valid user-space range (> `0x1000`, < `0x0000_8000_0000_0000`)
- Length must not exceed 64 KiB
- `addr + len` must not wrap around
- Zero-length buffers are accepted (no-op)
- Null pointers (`0x0`) are rejected

These checks run before page-table validation, catching obviously invalid
addresses without touching the page tables.

---

## Server-Loop Failure Semantics

IPC endpoints and notification objects are kernel resources that outlive
individual syscalls.  When a task dies, the kernel must clean up its IPC
state to prevent resource leaks and unblock peers that are waiting for
the dying task.

### Client dies before server replies

The server holds a `Reply(caller_id)` capability.  When the server calls
`reply()`, the reply message is delivered to the dead task's message slot
(a harmless no-op since the task is dead and will never consume it).
The server loop continues normally.  The dangling reply cap is consumed
by `reply` and cleared from the server's capability table — no leak.
In a `reply_recv` loop, the server atomically replies and waits for the
next message, so the dead-client reply is a fire-and-forget operation.

### Server dies while client is blocked in `call`

The client is blocked in `BlockedOnReply` state, waiting for the server
to call `reply()`.  During the server's exit, `cleanup_task_ipc(server_task_id)`
is called (from `do_full_process_exit`), which:

1. Removes the server from all endpoint receiver queues.
2. Removes the server's pending sends from all endpoint sender queues.
3. Clears any notification waiter slots held by the server.

Callers that are blocked in `call` waiting for a reply from the server
remain in `BlockedOnReply` state until the service manager restarts the
server.  In a future enhancement, the kernel could scan for Reply caps
pointing at the dying task and wake the corresponding callers with an
error message.

### Service restarts and re-registers

The service registry (Phase 50, Track D) supports re-registration via
`replace_service()`.  After the service manager restarts a crashed
service, the new instance calls `ipc_register_service` with the same
name, which atomically replaces the old endpoint mapping.  New clients
that call `ipc_lookup_service` receive the new endpoint.

Existing clients that cached the old endpoint cap must re-lookup the
service after receiving an error from `call`.  The recommended pattern
for resilient clients:

```text
loop:
    result = ipc_call(server_ep, REQ, data)
    if result == u64::MAX:
        server_ep = ipc_lookup_service("my_service")
        continue
    handle(result)
```

---

## Phase 6 Simplifications vs. Real Microkernels

Phase 6 deliberately keeps the IPC contract small.  Here is what a production
microkernel does that m³OS does not (yet):

### seL4

seL4 has a formally verified microkernel with a complete formal model of
capability semantics.  Its IPC uses "message registers" (a convention that the
compiler can map to physical registers on the fast path) and "IPC buffer" pages
for larger payloads.  It supports fine-grained capability rights (read-only vs.
read-write endpoint access), capability revocation trees, and priority-aware
IPC scheduling where a high-priority client can temporarily boost the
server's priority ("priority inheritance for IPC").

m³OS Phase 6 has none of these.  Capability rights are all-or-nothing,
revocation is not implemented, and the round-robin scheduler has no priority
concept.

### Mach

Mach uses **ports** (similar to endpoints) and **port rights** (similar to
capability handles).  Messages can carry out-of-line data (copy-on-write pages)
and port rights in the same message.  The Mach IPC path is notoriously complex;
its performance was a major critique that drove the L4 lineage.

m³OS Phase 6 carries only inline data (no out-of-line pages) and defers
capability grants in messages to Phase 7+.

### L4 / Fiasco.OC / Genode

L4-family kernels use typed message words and "message items" for capability
transfer.  Fiasco.OC has kernel-object reference counting.  Genode adds a
capability-based component framework on top.

m³OS does not have reference-counted kernel objects or typed message words.
The 64-slot fixed capability table is sufficient for a teaching OS; a real
system would need growable per-process tables or a tree structure.

### What all of them share with m³OS

All production microkernels use:

- rendezvous or near-rendezvous semantics (no unbounded kernel-side buffering),
- small inline message payloads plus separate page-grant paths for bulk data,
- capability-based access control validated in the kernel, not in userspace, and
- scheduler integration so blocked senders/receivers do not burn CPU time.

These are the ideas Phase 6 implements.  The differences above are engineering
refinements — important for production, deferred here for clarity.

---

## Server Registration Pattern

Servers register themselves with `init` at startup, which acts as a nameserver:

```mermaid
sequenceDiagram
    participant init
    participant ConsoleServer
    participant Shell

    ConsoleServer->>init: register("console", my_endpoint_cap)
    Shell->>init: lookup("console") → console_ep_cap
    Shell->>ConsoleServer: call(console_ep, Write("hello\n"))
    ConsoleServer-->>Shell: reply(OK)
```

This pattern allows any task to discover any server by name without
hard-coding endpoint IDs.  Phase 7 implements this as a static
name-to-endpoint table inside `init_task`.

---

## What Is Deferred to Phase 7+

| Feature | Why Deferred |
|---|---|
| **Capability grants via IPC** (`sys_cap_grant`) | Requires `ipc_call` to carry capability slots in the message, copy-on-revocation semantics, and a parent-child capability tree |
| **Page-capability bulk transfers** | Requires per-process page tables (CR3 switching) and page-mapping syscalls |
| **IPC timeouts / cancellation** | Requires a kernel timer list and a way to unblock a sender whose receiver never shows up |
| **Priority inheritance for IPC** | Requires a priority scheduler (deferred until after Phase 6) |
| **Multi-process userspace IPC** | Phase 6 exercises IPC with kernel tasks; full ring-3 multi-process IPC is Phase 7 |
| **Growable capability tables** | 64 slots is sufficient; growable tables need heap reallocation and handle remapping |

---

## See Also

- `docs/05-userspace-entry.md` — ring-3 execution model (Phase 5)
- `docs/07-core-servers.md` — server infrastructure built on this IPC (Phase 7)
- `docs/appendix/testing.md` — how to test IPC paths in QEMU
- `docs/roadmap/06-ipc-core.md` — roadmap phase doc
- `docs/roadmap/tasks/06-ipc-core-tasks.md` — task list
- `kernel/src/ipc/mod.rs` — module overview and syscall dispatcher
- `kernel/src/ipc/endpoint.rs` — rendezvous endpoint implementation
- `kernel/src/ipc/notification.rs` — async notification objects
- `kernel/src/ipc/capability.rs` — per-task capability table
- `kernel/src/task/scheduler.rs` — IPC scheduler primitives
