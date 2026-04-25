# Ring-3 Driver Correctness Closure

**Aligned Roadmap Phase:** Phase 55c
**Status:** Complete
**Source Ref:** phase-55c
**Supersedes Legacy Doc:** N/A

## Overview

Phase 55c closes three ring-3-driver correctness gaps that Phase 55b left behind. The
gaps share surfaces — the kernel IPC recv path, the IOMMU domain setup, and the kernel's
`RemoteNic` facade — so they were grouped into one pre-1.0 closure pass. The phase
delivers: a bound-notification primitive that lets a ring-3 driver multiplex IRQ wakes
and IPC messages on a single `recv()` call (R3); identity-mapped MMIO coverage for each
claimed device's BAR regions inside its IOMMU domain (R2); and typed `EAGAIN` visibility
for a userspace `sendto()` call during a ring-3 driver restart window (R1). Kernel
version bumps to `v0.55.3`.

## What This Doc Covers

1. **Bound notifications and the seL4 wake-model composition pattern (R3).** How a
   `Notification` object is bound to a TCB so that `ipc_recv_msg(ep_cap)` wakes for
   either an IRQ signal or a peer IPC send, with the wake source identified by a typed
   `WakeKind` return channel. Why this is the canonical seL4 answer to the
   "TCB-waits-on-two-sources" problem, and how it differs from Linux `epoll` and Fuchsia
   ports.

2. **IOMMU domain MMIO identity mapping for claimed devices (R2).** Why a device's IOMMU
   translation domain must include identity-mapped entries for each BAR's `(base, length)`
   pair so that the driver's `CTRL.RST` writes (NVMe `CC.EN`, e1000 `CTRL.RST`) reach
   the hardware instead of being silently dropped by VT-d remapping. How the
   `BarCoverage` invariant is enforced at `sys_device_claim` time and verified by a
   host-testable pure-logic helper.

3. **Driver-restart error propagation through the kernel's `RemoteNic` facade to
   userspace `EAGAIN` (R1).** How `RemoteNic::send_frame` translates
   `NetDriverError::DriverRestarting` to `-EAGAIN` via `net_error_to_neg_errno`, why the
   existing `sys_sendto` path did not route through `RemoteNic` before Phase 55c, and
   why surfacing `EAGAIN` preserves the caller's retry semantic without sprinkling
   driver-specific logic into the socket layer.

## Core Implementation

### R3 — Bound notifications

The kernel maintains a `BOUND_TCB: [AtomicI32; MAX_NOTIFS]` array in
`kernel/src/ipc/notification.rs`, one slot per notification, parallel to the existing
`ISR_WAITERS`. A value of `-1` means unbound; otherwise the slot holds the bound TCB's
scheduler index. The array is lock-free so `signal_irq` reads it without acquiring any
lock, honoring the ISR-safety contract from Phase 52c.

`sys_notif_bind(notif_cap, ep_cap)` writes both directions atomically (guarded by the
`WAITERS` mutex wrapped in `without_interrupts`). Idempotent on re-bind with the same
pair; returns `-EBUSY` on a conflicting bind; returns `-EBADF` on invalid capabilities.
Bindings are cleared on TCB death and on `sys_notif_release`.

`ipc_recv_msg(ep_cap)` is extended with a bound-notification fast path:

1. Take endpoint queue lock. If a sender is queued, rendezvous → `WakeKind::Message`.
2. Look up the caller's bound notification via `TCB_BOUND_NOTIF[task_idx]`.
3. If pending bits exist, swap atomically → `WakeKind::Notification(bits)`. No parking.
4. Otherwise, register as both the endpoint's receiver and the notification's waiter, then
   block. The first wake — from `endpoint::send` or `notification::signal*` — sets
   `last_wake_kind` on the TCB; `recv_msg` inspects it and returns accordingly.

The driver-facing surface is `driver_runtime::RecvResult`:

```rust
pub enum RecvResult {
    Message(RecvFrame),
    Notification { bits: u64 },
}
```

`IrqNotification::bind_to_endpoint` issues `sys_notif_bind` during driver init.
`run_io_loop` collapses to one `endpoint.recv()` per iteration.

### R2 — IOMMU BAR identity coverage

`kernel-core/src/iommu/bar_coverage.rs` adds a `BarCoverage` type that records each
BAR's `(base, length)` pair and an `assert_bar_identity_mapped(domain, coverage)` helper.
The kernel's per-device domain setup in `kernel/src/iommu/intel.rs` and
`kernel/src/iommu/amd.rs` calls this helper at `sys_device_claim` time, inserting
identity-mapped 4 KiB pages for each BAR before the driver process starts. A failure
emits `iommu.missing_bar_coverage` at warn level and returns
`DeviceHostError::Internal` — the driver cannot start with an incomplete domain.

The Phase 55a IOMMU substrate handles the address-space translation machinery unchanged;
Phase 55c only extends the set of covered ranges from DMA to MMIO.

### R1 — Userspace EAGAIN visibility

`kernel/src/net/remote.rs::RemoteNic::send_frame` already returned
`NetDriverError::DriverRestarting` on IPC transport failure. Phase 55c adds the missing
glue: the userspace send path (via the shape chosen in Track G — either a new
`sys_net_send` syscall or an extension of `sys_sendto`) routes through `RemoteNic` when a
ring-3 net driver is registered, and maps `DriverRestarting` to `-EAGAIN` through the
existing `net_error_to_neg_errno`. The virtio-net path remains the fallback when no
ring-3 net driver is bound. `userspace/e1000-crash-smoke` asserts the EAGAIN is
observable during the mid-restart window.

## Key Files

| File | Purpose |
|---|---|
| `kernel-core/src/ipc/bound_notif.rs` | Pure-logic state model for the bound-notification table: bind/unbind/signal/recv invariants; property-tested for arbitrary interleavings |
| `kernel/src/ipc/notification.rs` | ISR-reachable `BOUND_TCB` and `TCB_BOUND_NOTIF` arrays; `signal_irq` reads `BOUND_TCB` lock-free |
| `kernel/src/ipc/endpoint.rs` | `recv_msg` bound-notification fast path; `WakeKind` return channel; lock-ordering documentation |
| `kernel-core/src/iommu/bar_coverage.rs` | `BarCoverage` invariant and `assert_bar_identity_mapped` helper; host-testable; shared by VT-d and AMD-Vi backends |
| `kernel/src/net/remote.rs` | `RemoteNic::send_frame` → `NetDriverError::DriverRestarting` → `-EAGAIN` translation; `net_error_to_neg_errno` routing |
| `userspace/lib/driver_runtime/src/ipc/mod.rs` | `RecvResult` enum; `IrqNotification::bind_to_endpoint` helper; mock and syscall backends |
| `userspace/drivers/e1000/src/io.rs` | First bound-notification consumer: `subscribe_and_bind` + collapsed `run_io_loop` |

## How This Phase Differs From Later Work

- Later IRQ-backed userspace drivers (e.g., a future vsync or HID interrupt driver
  introduced in Phase 56 or beyond) may consume `RecvResult` and
  `IrqNotification::bind_to_endpoint` by following the same pattern the e1000 driver
  established in Phase 55c. The primitive is designed to be reused by any driver that
  genuinely mixes async hardware events with sync IPC requests.
- **The Phase 56 compositor core does not depend on Phase 55c.** The Phase 56 display
  and input architecture is socket-centric. `display_server`, `kbd_server`, and initial
  input services use AF_UNIX sockets and existing IPC endpoints. They do not require
  `sys_notif_bind`, `RecvResult`, or `IrqNotification::bind_to_endpoint`. Phase 55c is
  available as an optional template, not a hard prerequisite for Phase 56.
- Many-to-one binding (multiple notifications per TCB) is deferred. The e1000 driver and
  every Phase 56 driver use at most one IRQ notification. A future phase can lift this
  constraint if a driver genuinely needs two independent async sources.
- `ipc_recv_timeout` is deferred. The e1000 deadlock is resolved without it. Any timeout
  support is a separate design decision for a later phase.
- NVMe migration to the bound-notification model is deferred. NVMe is request-response;
  polling for completion inside the handler is correct for its workload shape.

## Related Roadmap Docs

- [Phase 55c design doc](./roadmap/55c-ring-3-driver-correctness-closure.md)
- [Phase 55c task list](./roadmap/tasks/55c-ring-3-driver-correctness-closure-tasks.md)
- [Phase 55b design doc](./roadmap/55b-ring-3-driver-host.md)
- [Phase 55b residuals](./appendix/phase-55b-residuals.md) — R1 and R2 source records (closed)
- [Post-mortem: e1000 bound-notification deadlock](./post-mortems/2026-04-22-e1000-bound-notif.md)
- [Phase 56 design doc](./roadmap/56-display-and-input-architecture.md) — next phase; not a consumer of Phase 55c primitives

## Deferred or Later-Phase Topics

- Many-to-one notification binding (multiple notifications per TCB) — no current driver
  needs it; deferred until a concrete need arises
- `ipc_recv_timeout` — not needed to resolve the e1000 deadlock; separate decision
- NVMe migration to bound-notification model — NVMe polling is correct for its shape; no
  urgency
- IOMMU coverage for MSI-X table regions — MSI-X tables live inside a BAR and are
  covered transitively by the BAR identity map; a future hardening phase can audit this
- Generalized EAGAIN over block I/O — R1 covers net send only; the NVMe side's
  `DriverRestarting` path through `sys_block_{read,write}` is a separate later task
- Driver-side seccomp sandbox, hot-plug/surprise-removal, VirtIO-blk/VirtIO-net
  extraction, live-update/zero-downtime restart, multi-queue NVMe — all remain deferred
  from the Phase 55b list
