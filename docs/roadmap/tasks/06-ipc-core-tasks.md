# Phase 06 — IPC Core: Task List

**Status:** Complete
**Source Ref:** phase-06
**Depends on:** Phase 5 ✅
**Goal:** Implement the synchronous rendezvous IPC model with endpoints, a per-process capability table, blocking send/recv primitives, call/reply semantics, the reply_recv server loop, and notification objects for asynchronous IRQ-style events.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | IPC objects + capability table | Phase 5 | ✅ Done |
| B | Blocking send/recv + call/reply | A | ✅ Done |
| C | Notifications + IRQ delivery | B | ✅ Done |
| D | Validation + docs | C | ✅ Done |

---

## Track A — IPC Objects + Capability Table

### A.1 — Define kernel IPC objects for endpoints and notifications

**Files:**
- `kernel/src/ipc/message.rs`
- `kernel/src/ipc/endpoint.rs`
- `kernel/src/ipc/notification.rs`

**Symbols:** `Message`, `Endpoint`, `EndpointRegistry`, `Notification`
**Why it matters:** These are the kernel objects through which all inter-process communication flows — endpoints for synchronous rendezvous, notifications for async signals.

**Acceptance:**
- [x] `Message` holds a label and `data: [u64; 4]`
- [x] `Endpoint` has sender/receiver queues and an `EndpointRegistry` for allocation
- [x] `Notification` uses `AtomicU64` for lock-free signaling with a waiter field

### A.2 — Add a per-process capability table with validation

**Files:**
- `kernel/src/ipc/capability.rs`
- `kernel-core/src/ipc/capability.rs`
- `kernel/src/task/mod.rs`

**Symbols:** `CapabilityTable`, `CapHandle`, `Capability`, `CapError`
**Why it matters:** Every IPC operation must go through capability validation — a process can only use handles it legitimately holds, preventing capability forgery.

**Acceptance:**
- [x] `CapabilityTable` holds 64 slots of `Capability` enum values
- [x] `Task` struct holds a `caps: CapabilityTable`
- [x] IPC dispatcher in `kernel/src/ipc/mod.rs` validates the cap handle on every syscall

---

## Track B — Blocking Send/Recv + Call/Reply

### B.1 — Implement blocking recv and send primitives

**Files:**
- `kernel/src/ipc/endpoint.rs`
- `kernel/src/task/scheduler.rs`

**Symbols:** `recv_msg`, `send_msg`, `block_current_on_recv`, `block_current_on_send`, `wake_task`
**Why it matters:** Synchronous IPC requires the ability to block the calling task until a partner arrives on the endpoint, then transfer the message and wake both sides.

**Acceptance:**
- [x] `recv()` / `send()` block the caller with `BlockedOnRecv` / `BlockedOnSend` task states
- [x] `wake_task` transitions a blocked task back to `Ready`
- [x] `TaskState` gains `BlockedOnRecv`, `BlockedOnSend`, `BlockedOnReply`, `BlockedOnNotif` variants

### B.2 — Implement synchronous call and reply semantics

**File:** `kernel/src/ipc/endpoint.rs`
**Symbols:** `call_msg`, `reply`
**Why it matters:** The call/reply pattern is the standard client-server interaction — the client blocks until the server processes its request and sends a reply.

**Acceptance:**
- [x] `call()` inserts a one-shot `Capability::Reply` and blocks the caller
- [x] `reply()` delivers the response and wakes the caller

### B.3 — Add the reply_recv server loop pattern

**File:** `kernel/src/ipc/endpoint.rs`
**Symbol:** `reply_recv`
**Why it matters:** `reply_recv` is the primary server loop — it replies to the previous client and blocks waiting for the next request in a single operation.

**Acceptance:**
- [x] `reply_recv()` = `reply()` + `recv()` on the server endpoint

---

## Track C — Notifications + IRQ Delivery

### C.1 — Implement notification objects for async events

**File:** `kernel/src/ipc/notification.rs`
**Symbols:** `create`, `signal`, `signal_irq`, `wait`
**Why it matters:** Notifications provide a lock-free, ISR-safe signaling mechanism for hardware interrupts and other asynchronous events that cannot use synchronous IPC.

**Acceptance:**
- [x] `signal_irq()` is lock-free and safe to call from interrupt handlers
- [x] `signal()` wakes a blocked task from task context
- [x] `wait()` blocks the caller until bits are set
- [x] `drain_pending_waiters()` processes deferred wakeups

### C.2 — Connect IRQ registration and delivery to notifications

**Files:**
- `kernel/src/ipc/notification.rs`
- `kernel/src/arch/x86_64/interrupts.rs`

**Symbols:** `register_irq`, `signal_irq`, `keyboard_handler`
**Why it matters:** Bridging hardware IRQs to notification objects lets userspace tasks wait for hardware events without polling or running in ring 0.

**Acceptance:**
- [x] `register_irq(irq, notif_id)` maps an IRQ line to a notification
- [x] Keyboard handler calls `signal_irq(1)` to wake the registered notification
- [x] IRQ map stored in `NotifRegistry`

---

## Track D — Validation + Docs

### D.1 — Verify client-server IPC round trip

**File:** `kernel/src/main.rs`
**Why it matters:** Confirms that the full send/call/reply/recv path works end-to-end between two tasks.

**Acceptance:**
- [x] Client task calls server twice and receives correct reply labels (`0xbeef`, `0xcafe`)
- [x] Invalid or forged capability handles are rejected (returns `u64::MAX`)

### D.2 — Verify server loop and notification wakeup

**File:** `kernel/src/main.rs`
**Why it matters:** Confirms that the server loop can block, reply, and receive predictably, and that IRQ notifications wake waiting tasks.

**Acceptance:**
- [x] Server task uses `recv` + `reply_recv` for sequential calls; verified in QEMU
- [x] `kbd_notif_task` blocks on `notification::wait()` and is woken by keyboard IRQ

### D.3 — Document the IPC model

**File:** `docs/06-ipc.md`
**Why it matters:** The IPC model is central to the microkernel architecture and must be understood by anyone adding new servers or syscalls.

**Acceptance:**
- [x] Rendezvous IPC model and rationale documented ("IPC Model", "Why synchronous rendezvous?")
- [x] Capability table and endpoint vs. notification distinction documented
- [x] A note explains how mature microkernels optimize IPC fast paths ("How Real Microkernels Differ")

---

## Documentation Notes

- Adds the `kernel/src/ipc/` module tree (`mod.rs`, `endpoint.rs`, `capability.rs`, `message.rs`, `notification.rs`).
- Core types (`CapabilityTable`, `Capability`, `CapHandle`, `Message`) are defined in `kernel-core/src/ipc/` for host testability.
- Extends `TaskState` with four new blocked variants for IPC synchronization.
