# Phase 50 — IPC Completion

**Aligned Roadmap Phase:** Phase 50
**Status:** Complete
**Source Ref:** phase-50

## Overview

Phase 50 finishes the IPC transport model that Phase 6 introduced as a
control-path-only design. The kernel now supports safe capability transfer
between process capability tables, a two-tier bulk-data mechanism (validated
copy for small payloads, page grants for large transfers), ring-3-safe service
registration with owner tracking and re-registration, and documented
server-loop failure semantics. These additions close the gap between the
existing message-passing infrastructure and the requirements of real ring-3
services.

## What This Doc Covers

- Capability grants between capability tables (`sys_cap_grant`, `CapabilityTable::grant`)
- The `Grant` capability variant for page transfers
- Message cap field for in-band capability delivery
- Buffer validation via `validate_user_buffer` and `copy_from_user` in IPC paths
- Owner-tracked service registry with re-registration support and increased capacity (16 entries)
- IPC syscall dispatch module wired into the main syscall handler
- Server-loop failure semantics (client death, server death, service restart)
- IPC cleanup on task exit (`cleanup_task_ipc`)
- Console server validated data path (proof-of-concept port)

## Core Implementation

### Capability Grants

The capability system now supports transferring capabilities between tasks.
`CapabilityTable::grant(source_handle, dest_table)` atomically removes a
capability from the source table and inserts it into the destination table.
The operation is all-or-nothing: if the destination table is full, the source
retains the capability and a `TableFull` error is returned.

A new `sys_cap_grant` syscall (IPC dispatch number 6) exposes this to
userspace. It accepts `(source_handle, target_task_id)` and validates
that the caller owns the source handle and that the target task exists.

### Grant Capability Variant

A new `Capability::Grant { frame, page_count, writable }` variant represents
ownership of physical page frames that can be mapped into a receiver's address
space. This is the zero-copy path for transfers larger than 64 KiB, primarily
framebuffer spans. Ownership transfers atomically: the sender loses access
when the grant succeeds.

### Message Cap Field

`Message` gains an optional capability field (`cap: Option<Capability>`) so
that capability transfers can be bundled with IPC messages. Existing
constructors (`new`, `with1`, `with2`) continue to work unchanged with no
capability attached. When a message with an attached capability is delivered
via `send` or `call`, the kernel invokes the grant logic to transfer the
capability from sender to receiver.

### Buffer Validation

Before any `copy_from_user` or `copy_to_user`, the kernel calls
`validate_user_buffer(addr, len)` (defined in `kernel-core/src/ipc/buffer.rs`)
to perform pure-logic address checks:

- Address must be above `0x1000` and below `0x0000_8000_0000_0000`
- Length must not exceed 64 KiB
- `addr + len` must not wrap around
- Zero-length buffers are accepted (no-op)
- Null pointers are rejected

These checks run before page-table validation, catching obviously invalid
addresses without touching the page tables.

### Bulk-Data Transport: Two-Tier Model

| Tier | Payload size | Mechanism | Latency |
|---|---|---|---|
| Small copy | 0 -- 64 KiB | `copy_from_user` / `copy_to_user` | Low (memcpy through validated page tables) |
| Page grant | > 64 KiB | `Capability::Grant` page transfer | Near-zero (remap, no copy) |

The small-copy path covers service-name strings, VFS paths, console write
buffers, network packets, and FAT32 disk blocks. The page-grant path covers
framebuffer spans.

### Owner-Tracked Registry

The service registry now tracks which task owns each service entry via a
`TaskId` field. `replace_service()` atomically replaces a dead service's
endpoint mapping when a restarted instance re-registers. The capacity was
increased from 8 to 16 entries to accommodate the service inventory from
the Phase 49 ownership matrix.

`ipc_register_service` and `ipc_lookup_service` now use `copy_from_user`
to read service name strings from the caller's address space. The old
`// Safety: Phase 7 only -- all callers are kernel tasks` comments and their
raw kernel-pointer dereferences have been removed.

### IPC Syscall Dispatch

A new `kernel/src/arch/x86_64/syscall/ipc.rs` module follows the Phase 49
per-subsystem pattern. IPC syscall numbers are defined as named constants
and routed to `kernel::ipc::dispatch()`. The module is wired into the main
syscall dispatch chain.

### Server-Loop Failure Semantics

The IPC doc now describes three failure scenarios:

1. **Client dies before server replies** -- The server's `Reply(caller_id)`
   capability is consumed normally. The reply is delivered to the dead task's
   message slot (harmless no-op). The server loop continues.

2. **Server dies while client is blocked in `call`** -- `cleanup_task_ipc()`
   removes the server from all endpoint queues and clears notification waiters.
   Blocked callers remain in `BlockedOnReply` until the service manager
   restarts the server.

3. **Service restarts and re-registers** -- `replace_service()` atomically
   replaces the old endpoint mapping. New clients get the new endpoint via
   `ipc_lookup_service`. Existing clients with cached endpoints must re-lookup
   after receiving an error.

### IPC Cleanup on Task Exit

`cleanup_task_ipc(task_id)` is called during `do_full_process_exit`. It:

1. Removes the task from all endpoint receiver queues
2. Removes pending sends from all endpoint sender queues
3. Clears notification waiter slots held by the task

## Key Files

| File | Purpose |
|---|---|
| `kernel-core/src/ipc/capability.rs` | `CapabilityTable::grant`, `Capability::Grant` variant |
| `kernel-core/src/ipc/message.rs` | `Message.cap` field, updated constructors |
| `kernel-core/src/ipc/buffer.rs` | `validate_user_buffer` pure-logic address checks |
| `kernel-core/src/ipc/registry.rs` | Owner-tracked registry, `replace_service`, capacity 16 |
| `kernel/src/ipc/mod.rs` | `copy_from_user` in register/lookup, `sys_cap_grant` |
| `kernel/src/ipc/endpoint.rs` | Capability transfer during message delivery |
| `kernel/src/arch/x86_64/syscall/ipc.rs` | IPC syscall dispatch module |
| `kernel/src/mm/user_mem.rs` | `copy_from_user` / `copy_to_user` implementations |

## How This Phase Differs From Later IPC Work

- This phase completes the transport model so ring-3 services can be extracted.
- Later Phase 51 (Service Model Maturity) builds lifecycle management on top.
- Later Phase 52 (First Service Extractions) actually moves services to ring 3.
- This phase does not implement typed IDLs, code-generated message bindings,
  or advanced delegation patterns.
- Performance tuning of zero-copy paths is deferred to later phases.

## Related Roadmap Docs

- [Phase 50 roadmap doc](./roadmap/50-ipc-completion.md)
- [Phase 50 task doc](./roadmap/tasks/50-ipc-completion-tasks.md)

## Deferred or Later-Phase Topics

- Typed service IDLs and code-generated message bindings
- Advanced capability delegation patterns beyond basic grant
- Performance tuning of the page-grant zero-copy path
- Full extraction of kernel-resident services to ring-3 processes (Phase 52+)
- IPC timeouts and cancellation
- Priority inheritance for IPC
- Growable capability tables
