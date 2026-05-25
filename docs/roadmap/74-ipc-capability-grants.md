# Phase 74 - IPC Capability Grants and Bulk Transfers

**Status:** Planned
**Source Ref:** phase-74
**Depends on:** Phase 6 (IPC Core) ✅, Phase 55a (IOMMU Substrate) ✅, Phase 55b (Ring-3 Driver Host) ✅, Phase 55c (Ring-3 Driver Correctness Closure) ✅, Phase 57a (Scheduler Rewrite) ✅
**Builds on:** Closes the "deferred to Phase 7+" IPC capability-grant and bulk-transfer items opened in Phase 6, using the IOMMU substrate from Phase 55a as the zero-copy transport where present
**Primary Components:** `kernel/src/ipc/mod.rs`, `kernel/src/mm/`, `kernel/src/syscall/`, `userspace/syscall-lib`

## Milestone Goal

The IPC subsystem gains three long-deferred primitives: capability handles transmitted inside IPC messages (`sys_cap_grant`), page-grant bulk-data transport that moves ownership of pages without copying, and per-call timeouts for both the send and receive sides. Together these close the IPC deferrals from Phase 6 and Phase 55c and unlock true zero-copy display-surface sharing for the Phase 72 compositor.

## Why This Phase Exists

Phase 6 shipped synchronous rendezvous IPC and noted two explicit deferrals at `kernel/src/ipc/mod.rs:34-35`: capability grants via IPC messages, and bulk data transfer via page grants rather than inline copy. Phase 55c's deferred list added `ipc_recv_timeout` and many-to-one notification binding. All four items have been accumulating technical debt since Phase 6.

The Phase 72 multi-app compositor is the forcing function: large surface buffers (a 1920×1080 RGBA buffer is ~8 MB) traveling via inline copy on every frame budget will dominate system time. Page-grant transport eliminates the copy. Capability grants let a client prove framebuffer ownership to `display_server` without a separate syscall round-trip. IPC timeouts let servers handle slow clients without blocking the entire compositor compose loop.

SOLID/Liskov: any IPC consumer — `fat_server`, `audio_server`, `display_server` — can substitute page-grant bulk transport for the existing inline-copy path because both satisfy the same bulk-transfer interface abstracted in Phase 55c; no caller needs to know which transport is in use. DRY: today those three servers each carry a hand-rolled inline-copy path; the page-grant primitive introduced here is the single implementation they all adopt, eliminating duplicated buffer-management logic. TDD: the capability-table copy arithmetic in `ipc_transfer_caps` is pure logic and host-testable in `kernel-core`; page-grant page-table mutations and the timeout race are exercised via QEMU smoke tests using the Phase 57a timer wheel.

## Learning Goals

- Understand how capability table entries are transferred between processes through IPC messages
- Learn how page-grant transport transfers physical page ownership without copying bytes
- See how the IOMMU substrate (Phase 55a) accelerates capability grants for device-backed buffers
- Understand how per-call timeouts interact with the scheduler's wait queue
- Learn the design trade-offs between inline copy IPC and page-grant IPC for different message sizes

## Feature Scope

### `sys_cap_grant` via IPC messages

IPC messages grow a capability-handle slot: up to `CAP_SLOTS_PER_MSG` (initially 2) capability indices can be included in a single IPC call. The kernel copies the capability from the sender's capability table into the receiver's capability table at receive time. The receiver gets the capability index in a well-known out-parameter register. Sender revocation follows the same rules as `sys_cap_grant` (existing syscall); the IPC path is a syntactic convenience that saves a separate `sys_cap_grant` round-trip.

### Page-grant bulk transfer

A new syscall family: `sys_page_grant_send(endpoint, cap, pages, n_pages)` and `sys_page_grant_recv(endpoint) -> (cap, pages, n_pages)`. The kernel removes the specified physical pages from the sender's address space (unmaps and invalidates TLB), records them in an in-flight grant object, and transfers ownership to the receiver's address space on `recv`. Where the IOMMU substrate (Phase 55a) is active, the IOMMU translation domain is updated atomically; on non-IOMMU platforms, identity mapping fallback is used. The receiving process can then `mmap` the transferred pages. Page ownership is tracked in the kernel's frame allocator to prevent double-transfer.

### IPC timeouts

`sys_ipc_call_timeout(endpoint, msg, deadline_ns)` and `sys_ipc_recv_timeout(endpoint, deadline_ns)` accept an absolute deadline in nanoseconds (using `CLOCK_MONOTONIC`). If the operation does not complete before the deadline, the kernel returns `ETIMEDOUT`. The implementation adds the calling thread to both the endpoint's wait queue and the scheduler's timeout wheel. When the timeout fires, the thread is removed from the endpoint's wait queue and woken with the timeout error code.

### Many-to-one notification binding

Multiple notification objects can be bound to a single endpoint's receive set. A server calling `ipc_recv` blocks until any of its bound notification objects fires or a direct message arrives. This closes the Phase 55c deferral item. The bind is expressed via `sys_notif_bind(endpoint, notif_cap)`.

### Documentation updates

Phase 6, Phase 50, and Phase 55c design docs are updated to mark the deferred items as closed in Phase 74. The `kernel/src/ipc/mod.rs` deferral comments at lines 34-35 are removed and replaced with a reference to Phase 74.

## Important Components and How They Work

### Capability slot in IPC message

The `IpcMessage` struct gains a `cap_slots: [CapHandle; CAP_SLOTS_PER_MSG]` field and a `n_caps: u8` count. On `sys_ipc_call`, the kernel iterates `n_caps` slots, validates each capability handle against the sender's table, and marks them as "in-flight." On the corresponding `ipc_recv_reply`, the kernel inserts them into the receiver's table and writes the new indices into the receiver's out-parameter registers. A failed capability copy (invalid handle, receiver table full) causes the IPC call to fail atomically before any message delivery occurs.

### Page-grant object

A `PageGrant` kernel object represents a set of physical frames removed from one address space and pending delivery to another. It is referenced by a capability handle (grantable like any capability). The frame allocator records the grant epoch so that freed pages with a pending grant are detected and the grant is invalidated. The IOMMU domain update (Phase 55a) is performed inside the kernel's grant transfer path, holding the IOMMU domain lock for the duration of the remap.

### Timeout wheel integration

The Phase 57a scheduler already maintains a timer wheel for `sys_nanosleep` and `sys_poll` timeouts. `ipc_call_timeout` and `ipc_recv_timeout` register a timeout entry in the same wheel. On timeout expiry, the wheel fires a wake event that sets `thread.ipc_result = ETIMEDOUT` and removes the thread from the endpoint's blocked queue. The normal IPC completion path checks for a racing timeout and handles the case where both the message arrival and the timeout fire within the same scheduler tick.

## How This Builds on Earlier Phases

- Extends Phase 6's synchronous rendezvous IPC with the capability-slot and page-grant primitives that were explicitly deferred in `ipc/mod.rs:34-35`
- Reuses Phase 55a's IOMMU domain remap path for the DMA-side of page-grant transfers
- Extends Phase 57a's timeout wheel (used for `sys_nanosleep` and `sys_poll`) to cover IPC blocking operations
- Closes the Phase 55c explicit deferral of `ipc_recv_timeout` and many-to-one notification binding

## Implementation Outline

1. Extend `IpcMessage` struct with `cap_slots` array; update serialization and the syscall ABI
2. Implement capability copy in the `sys_ipc_call`/`ipc_recv_reply` kernel path
3. Implement `PageGrant` kernel object, frame-allocator epoch tracking, and `sys_page_grant_send`/`recv` syscalls
4. Wire IOMMU domain remap into the page-grant transfer path (Phase 55a integration point)
5. Add `ETIMEDOUT`-returning timeout path to `sys_ipc_call` and `sys_ipc_recv` using Phase 57a timer wheel
6. Implement `sys_notif_bind` for many-to-one notification binding
7. Add `syscall-lib` bindings for all new syscalls
8. Update `fat_server`, `audio_server`, and `display_server` to adopt page-grant for their bulk paths (optional migration in this phase)
9. Update Phase 6, Phase 50, and Phase 55c design docs; remove deferral comments from `ipc/mod.rs`

## Acceptance Criteria

- `sys_cap_grant` via an IPC message transfers a capability from sender to receiver; the receiver can use the granted capability in a subsequent syscall
- A page-grant transfer of 4 MB (1024 × 4 KB pages) completes without any byte-by-byte copy; the sender's mapping is unmapped before the receiver's mapping is established
- An `sys_ipc_recv_timeout(endpoint, deadline_ns)` call fires `ETIMEDOUT` correctly when no message arrives within the deadline
- Many-to-one notification binding: a server bound to two notification objects wakes on either firing without requiring two separate `ipc_recv` calls
- All existing IPC-dependent binaries (`init`, `display_server`, `audio_server`, `fat_server`) continue to function correctly

## Companion Task List

- [Phase 74 Task List](./tasks/74-ipc-capability-grants-tasks.md)

## How Real OS Implementations Differ

- seL4 and Fiasco.OC implement capability grants via explicit endpoint rights attached to the IPC message badge; m3OS uses a simpler table-copy model without rights attenuation in this phase
- Linux's `sendmsg`/`SCM_RIGHTS` is the closest analog for FD passing; page-grant is analogous to `MADV_REMOVE` + `remap_file_pages` but for inter-process ownership transfer
- Production IPC systems (Mach ports, L4 IPC) support capability delegation with revocation trees and audit trails; m3OS tracks only the transfer epoch in the frame allocator
- Real timeout implementations use hardware watchdog timers or high-resolution HPET for sub-millisecond precision; m3OS uses the existing timer wheel whose resolution is one scheduler tick

## Deferred Until Later

- Rights attenuation (grant a capability with fewer rights than the sender holds) — requires a richer capability type system
- Asynchronous IPC (fire-and-forget without blocking the sender) — the Phase 6 synchronous model is retained
- Cross-machine capability delegation — out of scope for a single-node microkernel
- `wl_shm`-compatible cross-process `MAP_SHARED` semantics (a follow-on to page-grant, needed for a future Wayland compatibility shim)
