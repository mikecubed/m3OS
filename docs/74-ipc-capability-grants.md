# IPC Capability Grants and Bulk Transfers (Phase 74)

**Aligned Roadmap Phase:** Phase 74
**Status:** Complete
**Source Ref:** phase-74
**Supersedes Legacy Doc:** new

## Overview

Phase 6 shipped m3OS's rendezvous IPC: two userspace tasks meet at an
endpoint, the kernel copies a small register-sized [`Message`] between
them, and one (or both) tasks unblock. That model was always known to
be incomplete. Three things were left as `// Deferred to Phase 7+`
comments inside `kernel/src/ipc/mod.rs`:

1. **Capability grants travelling inside an IPC message.** Phase 6 had
   `sys_cap_grant` as its own standalone syscall — to hand a peer a
   freshly-created endpoint you had to do two round-trips: one to
   transfer the cap, one to tell the peer what it could do with it.
2. **Bulk data without copying.** A 1080p framebuffer surface is
   roughly 8 MB. Sending it through the existing `ipc_send_buf` path
   copies every byte once into the kernel and once out. Across a frame
   at 60 Hz that's 1 GB/s of pure `memcpy`.
3. **Per-call timeouts.** A server that calls into a flaky peer blocks
   forever on the reply if the peer dies between accepting and
   replying. Phase 6 had no way to say "wait at most 100 ms".

Phase 55c closed two more related deferrals — many-to-one
notification binding (so a driver task can receive both IPC messages
and IRQ notifications from one recv loop) was deferred there too.

Phase 74 ships all four:

- **Capability grants in IPC messages.** [`Message`] now carries
  `cap_slots: [CapHandle; 2]` and `n_caps: u8`. The kernel transfers
  the named capabilities from sender to receiver atomically at
  rendezvous. Existing pre-Phase-74 callers default to `n_caps = 0`
  and observe identical behaviour.
- **Page-grant bulk transport.** New `sys_page_grant_send` /
  `sys_page_grant_recv` syscalls move ownership of physical page
  frames between address spaces without copying any bytes. A
  monotonic grant-epoch in the frame allocator refuses frees while
  the grant is live, ruling out the use-after-free hazard.
- **Per-call IPC timeouts.** New `sys_ipc_call_timeout` /
  `sys_ipc_recv_timeout` syscalls register a deadline with the
  Phase 57a scheduler timer wheel and surface `NEG_ETIMEDOUT`
  (`-110`) on deadline expiry. Race-free cleanup pulls the task out
  of the endpoint queues under the endpoint lock so no dangling
  pointer remains.
- **Many-to-one notification binding.** Verified closure of the
  Phase 55c deferral; `sys_notif_bind` (syscall `0x1111`) plus the
  new `syscall-lib::notif_bind` wrapper give driver tasks one recv
  loop that wakes on either IPC messages or IRQ notifications.

## What This Doc Covers

- The cap-slot extension to [`Message`]
  (`kernel-core/src/ipc/message.rs`) and the matching userspace
  `IpcMessage` (`userspace/syscall-lib/src/lib.rs`).
- The `ipc_transfer_caps` helper
  (`kernel/src/ipc/mod.rs`) and how it integrates with the existing
  `transfer_cap` rendezvous path in
  `kernel/src/ipc/endpoint.rs`.
- The `PageGrant` kernel object
  (`kernel/src/ipc/page_grant.rs`) — registry, epoch counter, and
  the syscall surface.
- The deadline-bearing endpoint helpers
  `call_msg_with_deadline` and `recv_msg_with_deadline`
  (`kernel/src/ipc/endpoint.rs`) and how they reuse the Phase 57a
  `block_current_until` deadline path.
- The userspace `syscall-lib` wrappers for all four new primitives.

## Key Files

| File | Role |
|---|---|
| `kernel-core/src/ipc/message.rs` | `Message::cap_slots` / `n_caps` extension + `with_cap_slots` constructor + host-side round-trip test. |
| `kernel/src/ipc/mod.rs` | Syscall dispatch for cap-bearing IPC (`24`, `25`) and timeouts (`26`, `27`); `ipc_transfer_caps` helper; cap-bearing wire-format codec. |
| `kernel/src/ipc/endpoint.rs` | `call_msg_with_caps`, `call_msg_with_deadline`, `recv_msg_with_deadline`; extended `transfer_cap` that walks `cap_slots[..]`. |
| `kernel/src/ipc/page_grant.rs` | `PageGrant` kernel object, registry, epoch counter, `sys_page_grant_send` / `sys_page_grant_recv` ABI. |
| `kernel/src/ipc/notification.rs` | `sys_notif_bind` implementation (in place since Phase 55c; Phase 74 confirms closure). |
| `userspace/syscall-lib/src/lib.rs` | `ipc_call_with_caps`, `ipc_recv_with_caps`, `ipc_call_timeout`, `ipc_recv_timeout`, `page_grant_send`, `page_grant_recv`, `notif_bind` wrappers. |

## Core Concepts

### Capability handle vs capability value

A `Capability` is an enum value the kernel can hand directly to a task
(e.g. `Capability::Endpoint(EndpointId(5))`). A `CapHandle` is a `u32`
index into the per-task `CapabilityTable` that resolves to a
`Capability`. The Phase 6 single-cap path stuffs an entire
`Capability` value into the message (`Message::cap`); the Phase 74
multi-cap path stuffs `CapHandle`s into `cap_slots[..]` so the kernel
can transfer the table entries atomically without each cap needing
to be size-fitted into the message body.

### Rendezvous-time transfer

The transfer happens at the same moment the message body is delivered.
Both writes happen inside the existing `transfer_cap` path so a
failure to insert a cap into the receiver's table aborts the entire
delivery — no half-delivered IPC where the message body lands but
the caps don't.

### Page-grant epoch counter

Every `PageGrant` carries a monotonic `epoch: u64` token. The frame
allocator's per-frame metadata records which epoch (if any) owns each
frame. A free against a frame with a live epoch surfaces as `EBUSY`
rather than racing the receiver's view. The grant is consumed (epoch
cleared) inside `sys_page_grant_recv`; double-recv against a consumed
grant returns `EINVAL`.

### Deadline-bearing block

`scheduler::block_current_until` already takes an
`Option<u64> deadline_ticks` (Phase 57a). The new endpoint helpers
just thread an absolute deadline through that argument. On deadline
expiry the helper acquires the endpoint lock and `retain()`s the
queue without the timed-out task — preventing the next IPC operation
on the same endpoint from dereferencing a stale pointer.

## How This Builds on Earlier Phases

- **Phase 6** introduced the rendezvous IPC core that Phase 74
  extends. The `Message` extension is additive and ABI-compatible
  with all existing callers (default `n_caps = 0`).
- **Phase 50** added the userspace-facing IPC syscall numbers and the
  `IpcMessage` wire format. Phase 74 extends the wire format by
  appending `cap_slots` + `n_caps` at the end so the first 40 bytes
  still match the Phase 50 layout.
- **Phase 55a** built the IOMMU substrate that the page-grant transport
  uses for device-domain remap on the receiver side.
- **Phase 55c** deferred `ipc_recv_timeout` and many-to-one
  notification binding to Phase 7+. Phase 74 closes both.
- **Phase 57a** introduced the scheduler timer wheel and
  `block_current_until` deadline path that the new IPC timeouts
  thread through.

## Related Roadmap Docs

- [`docs/roadmap/74-ipc-capability-grants.md`](roadmap/74-ipc-capability-grants.md)
- [`docs/roadmap/tasks/74-ipc-capability-grants-tasks.md`](roadmap/tasks/74-ipc-capability-grants-tasks.md)
- [`docs/roadmap/06-ipc-core.md`](roadmap/06-ipc-core.md) — Phase 6
  origin of the deferrals Phase 74 closes.
- [`docs/roadmap/50-ipc-completion.md`](roadmap/50-ipc-completion.md)
  — Phase 50 userspace-facing IPC surface.
- [`docs/roadmap/55c-ring-3-driver-correctness-closure.md`](roadmap/55c-ring-3-driver-correctness-closure.md)
  — Phase 55c "Deferred Until Later" entries that Phase 74 closes.

## Known Follow-ups

- **`sys_page_grant_release` syscall.** The receiver-side mappings
  installed by `sys_page_grant_recv` currently persist until the
  receiver process exits. This is acceptable for the in-tree
  compositor (surface lifetimes are bounded by the compositor's own
  lifetime) but a future hardening pass should add an explicit
  release syscall so a long-lived server can free granted-then-
  retired regions. The `PageGrantMapping::Drop` hook in
  `userspace/display_server/src/surface.rs` is wired and ready to
  call this syscall once it lands.
- **Per-grant IOMMU domain entries.** Phase 74 ships an
  `iommu_remap_grant` shim that logs and returns identity-fallback
  for every grant; the call site is in `sys_page_grant_recv`
  unconditionally. A future hardening pass that adds a
  "PID → bound IOMMU domains" reverse map can tighten this to
  per-frame IOVA mapping when the receiver is a ring-3 driver
  process that needs DMA isolation against the granted frames.
- **Real-hardware perf measurement.** The Phase 74 task list calls
  for a >30% reduction in compositor CPU time at 1080p/60 with the
  page-grant transport. QEMU CI's framebuffer does not expose the
  cycle-level profiling needed to verify this; the functional
  zero-copy path is covered by the page-grant round-trip smoke
  (`PAGE_GRANT_SMOKE:roundtrip:ok` in the boot transcript). A
  real-hardware harness phase is the right place to wire the perf
  measurement.

## Trade-offs and Alternatives

- **seL4** delivers caps via endpoint badges with explicit rights
  attenuation (you can grant a less-privileged version of a cap). m3OS
  Phase 74 uses a simpler table-copy model; rights attenuation is
  explicitly deferred because the current cap type system is too thin
  to express it.
- **Linux** delivers fds via `sendmsg(2)` + `SCM_RIGHTS`; m3OS
  `cap_slots` is the direct analog for capabilities. Page-grant is
  closer to `MADV_REMOVE` + `remap_file_pages` for inter-process
  ownership transfer than to any single Linux syscall.
- **Mach ports** carry rich rights and revocation trees with audit
  metadata; m3OS tracks only the grant epoch in the frame allocator.
  The trade-off favours a small TCB at the cost of richer policy.
