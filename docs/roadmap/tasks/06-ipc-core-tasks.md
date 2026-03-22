# Phase 6 Tasks - IPC Core

**Depends on:** Phase 5
**Branch:** `phase-6-ipc-core`
**PR:** https://github.com/mikecubed/ostest/pull/5

```mermaid
sequenceDiagram
    participant Design as IPC objects
    participant Caps as Capability table
    participant Call as call/reply path
    participant Notify as Notifications
    Design->>Caps: validate handles
    Caps->>Call: allow endpoint operations
    Call->>Notify: integrate async wakeups
```

## Implementation Tasks

- [x] P6-T001 Define the kernel IPC objects needed for endpoints and notifications.
  - `ipc/message.rs`: `Message { label, data: [u64; 4] }`
  - `ipc/endpoint.rs`: `Endpoint` with sender/receiver `VecDeque`, `EndpointRegistry` (16 slots)
  - `ipc/notification.rs`: `Notification` (AtomicU64 + waiter), `NotifRegistry` with IRQ map
- [x] P6-T002 Add a per-process capability table and explicit validation for every IPC syscall.
  - `ipc/capability.rs`: `CapabilityTable` (64 slots), `CapHandle = u32`, `Capability` enum
  - `task/mod.rs`: `Task` now holds `caps: CapabilityTable`
  - Scheduler helpers: `task_cap`, `remove_task_cap`, `insert_cap`
  - `ipc/mod.rs`: `dispatch()` validates cap handle on every IPC syscall
- [x] P6-T003 Implement blocking `recv` and `send` primitives.
  - `ipc/endpoint.rs`: `recv()` / `send()` with `block_current_on_{recv,send}` + `wake_task`
  - `task/mod.rs`: `TaskState` gains `BlockedOnRecv`, `BlockedOnSend`, `BlockedOnReply`, `BlockedOnNotif`
  - `task/scheduler.rs`: `block_current_on_{recv,send,reply}`, `wake_task`, `deliver_message`, `take_message`
- [x] P6-T004 Implement synchronous `call` and `reply` semantics.
  - `ipc/endpoint.rs`: `call()` inserts a one-shot `Capability::Reply`; `reply()` wakes caller
- [x] P6-T005 Add the `reply_recv` server pattern as the primary loop for services.
  - `ipc/endpoint.rs`: `reply_recv()` = `reply()` + `recv()` on the server endpoint
- [x] P6-T006 Implement notification objects for IRQ-style asynchronous events.
  - `ipc/notification.rs`: `signal()` (atomic, ISR-safe) + `wait()` (blocking)
- [x] P6-T007 Connect IRQ registration and delivery to the notification mechanism.
  - `arch/x86_64/interrupts.rs`: `keyboard_handler` calls `notification::signal_irq(1)`
  - `ipc/notification.rs`: `register_irq(irq, notif_id)` + `NotifRegistry.irq_map`

## Validation Tasks

- [x] P6-T008 Verify a client can send a request and receive a reply from a server.
  - `main.rs`: `client_task` calls `ipc-server` twice; QEMU output: `got first reply label=0xbeef`, `got second reply label=0xcafe`
- [x] P6-T009 Verify invalid or forged capability handles are rejected.
  - `ipc/mod.rs` `dispatch()`: `task_cap()` returns `u64::MAX` on invalid handle
- [x] P6-T010 Verify the server loop can block, reply, and receive the next message predictably.
  - `main.rs`: `server_task` uses `recv` + `reply_recv` for two sequential calls; verified in QEMU
- [x] P6-T011 Verify IRQ or signal-style notifications can wake a waiting userspace task.
  - `main.rs`: `kbd_notif_task` blocks on `notification::wait()`; woken by keyboard IRQ via `signal_irq(1)`

## Documentation Tasks

- [x] P6-T012 Document the rendezvous IPC model and why it was chosen for this project.
  - `docs/06-ipc-core.md`: sections "IPC Model" and "Why synchronous rendezvous?"
- [x] P6-T013 Document the capability table and the difference between endpoints and notifications.
  - `docs/06-ipc-core.md`: sections "Capability Table" and "Notification Objects"
- [x] P6-T014 Add a short note explaining how mature microkernels optimize IPC fast paths.
  - `docs/06-ipc-core.md`: section "How Real Microkernels Differ"
