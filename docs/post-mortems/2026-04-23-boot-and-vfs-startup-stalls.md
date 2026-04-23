# Post-mortem: boot and VFS startup stalls after Phase 55c integration

**Incident:** normal `cargo xtask run` often stalled before a usable login, smoke boot was
intermittently wedged, and when login did appear it could fail with
`login: cannot read /etc/passwd`.
**Status:** Resolved 2026-04-23.
**Severity:** High — blocked normal boot, made smoke flaky, and left the system unable to
authenticate even when the kernel and userspace otherwise came up.
**Owners:** Kernel scheduler, init/service manager, kernel VFS fallback path, virtio-blk.
**Fix commit:** `c5ead6d` `fix: unblock boot and stabilize ring-3 VFS`.

## Summary

The failure was not one bug but a startup chain:

1. **Fresh fork-children were not getting prompt first dispatch under SMP.** PID 1, the
   smoke runner, and later child commands such as `tcc` could sit ready but undispatched
   long enough to look like a boot wedge.
2. **PID 1 was doing too much synchronous work in its hot startup path.** Per-service
   status-file churn and the normal-boot smoke-marker probe widened the stall window and
   made `cargo xtask run` fail differently from smoke mode.
3. **The ring-3 VFS service could die during boot and leave a short stale-registry
   window.** New opens routed to the dead service instead of falling back cleanly to the
   kernel ext2 path, which surfaced as `login: cannot read /etc/passwd`.
4. **The underlying VFS crash was a block-driver concurrency bug.** `kernel/src/blk/virtio_blk.rs`
   still used a single shared descriptor chain, scratch page, DMA buffer, and wake flag,
   but boot now issued concurrent reads from multiple tasks. Requests trampled each other,
   the completion status byte stayed at `0xFF`, and `vfs_server` failed early even on
   sector 0 reads.

The final fix set addressed all four layers so boot became deterministic again, smoke
returned to green, and `vfs_server` stayed alive through login.

## Impact

- `cargo xtask run` regularly stalled after SMP bring-up and service startup, before a
  usable login session.
- `cargo xtask smoke-test` was initially stuck before `SMOKE:BEGIN`, then later became
  flaky around `tcc` child startup.
- Normal boot could show `m3OS login:` but reject any login attempt with
  `login: cannot read /etc/passwd`.
- `vfs_server` sometimes died at startup with:

  ```text
  [ERROR] [virtio-blk] read_sectors: sector 0 failed with status 255
  vfs_server: failed to read MBR
  ```

- Heavy `syslogd` drain loops amplified scheduler unfairness during the boot burst and made
  the stalls easier to hit.

## Timeline (condensed)

- **2026-04-22 to early 2026-04-23.** Initial report started as a host-side TCC build
  failure on Omarchy Linux, but serial logs showed the real blocker was a boot freeze
  after `/sbin/init` registration.
- **Early investigation.** PID 1 and other fork-children were confirmed to be created but
  not first-dispatched promptly. A one-shot fork-child priority boost and least-loaded-core
  placement got smoke far enough to expose later failures.
- **Mid investigation.** Smoke mode progressed, but normal boot still wedged before login.
  The split between smoke and normal boot pointed to extra init-path work rather than a
  purely scheduler-only bug.
- **Later investigation.** Login failures were traced to `vfs_server` exiting during boot.
  The kernel fallback path helped only after IPC cleanup removed the dead service from the
  registry, leaving a race where `/etc/passwd` opens still targeted the dead VFS endpoint.
- **Final isolation.** `vfs_server` itself was not conceptually wrong; it was reading through
  a block path that still assumed one in-flight request globally. Concurrent readers during
  startup corrupted that shared request slot and returned status `255`.
- **2026-04-23 resolution.** Boot-path scheduling, init-path churn, VFS transport fallback,
  and virtio-blk request serialization were all fixed together. Normal boot reached
  `Password:` with `vfs_server` alive; smoke and repo validation both passed.

## Root cause

### 1. First-dispatch starvation of fork-children

`spawn_fork_task()` produced children with ordinary priority and local-core placement.
Under the boot service burst, fresh children such as PID 1, `smoke-runner`, and `tcc`
could sit ready for too long before their first dispatch.

### 2. Too much synchronous init-path churn

PID 1 was doing unnecessary synchronous work during the most latency-sensitive phase:

- per-service `/run/services.status` writes during the boot-service loop;
- a normal-boot-only negative lookup of `/etc/m3os-smoke-test-mode`;
- aggressive `syslogd` drain loops competing for CPU while the system was still trying to
  bring up core services.

These were not the deepest root cause, but they materially widened the timing windows and
made the scheduler problem easier to trigger.

### 3. VFS stale-registry routing window

When `vfs_server` died, IPC cleanup removed its service registration only on the deferred
dead-task cleanup sweep. In that window, `registry::lookup_endpoint_id("vfs")` still
returned an endpoint, and `vfs_service_open()` could fail with transport-level
`u64::MAX`. The kernel open path did not treat that as a recoverable "service unavailable"
condition, so login surfaced the failure directly instead of falling back to kernel ext2.

### 4. Single-slot virtio-blk request path under concurrent readers

The decisive bug was in `kernel/src/blk/virtio_blk.rs`:

- descriptors are hard-coded to `0`, `1`, `2`;
- one scratch page stores both the request header and status byte;
- one DMA page is reused for data;
- one global `REQ_WOKEN` flag is reused for completion.

That design is valid only if **all requesters are serialized**. Earlier boot paths
effectively behaved that way, but by Phase 55c both the kernel and multiple userspace
services could hit block I/O during startup. Concurrent requests clobbered the shared
descriptor / scratch / wake state, leaving the completion byte at `0xFF` and making the
VFS mount fail spuriously.

## Resolution

The fix landed as one coherent startup-stability set:

1. **Scheduler first-dispatch hardening** (`kernel/src/task/scheduler.rs`)
   - fresh fork-children get a one-shot priority boost;
   - PID 1 stays local, later fork-children go to the least-loaded core;
   - priority is restored after the child's first trampoline dispatch.

2. **Init hot-path reduction** (`userspace/init/src/main.rs`, `xtask/src/main.rs`)
   - status-file writes are deferred out of the boot-service loop;
   - the smoke-mode marker is now always present and stores `0` or `1`, avoiding the
     normal-boot missing-file probe;
   - smoke mode keeps the fast path that avoids unnecessary early churn.

3. **Boot-time log fairness** (`userspace/syslogd/src/main.rs`)
   - bounded per-loop socket and kmsg draining;
   - cooperative yield after each bounded batch.

4. **Dead-VFS fallback hardening** (`kernel/src/arch/x86_64/syscall/mod.rs`)
   - transport-level `u64::MAX` from `vfs_service_open()` is now treated like a temporary
     VFS-unavailable result and falls back to the kernel ext2 open path.

5. **Block I/O serialization for the legacy single-request virtio-blk path**
   (`kernel/src/blk/virtio_blk.rs`)
   - new `REQUEST_LOCK` serializes task-context block I/O through the existing
     single in-flight request slot;
   - `REQ_WOKEN` is now correctly scoped by that serialization guarantee.

## Why the symptom changed over time

The investigation looked nonlinear because each partial fix exposed the next failure:

- once PID 1 got first dispatch, smoke progressed far enough to reveal child-start latency;
- once child-start latency improved, normal boot reached login and exposed the VFS death;
- once login routed around stale VFS transport failures, the deeper virtio-blk concurrency
  bug became obvious because `vfs_server` still died at startup;
- once virtio-blk requesters were serialized, `vfs_server` stayed up and the login path
  behaved normally.

Each later symptom depended on the earlier one already being improved enough to become
visible.

## Validation

- `cargo xtask run --fresh` reaches `m3OS login:` and advances to `Password:` after typing
  `root`.
- `userspace/vfs_server` logs:

  ```text
  vfs_server: ext2 mounted
  vfs_server: registered, entering server loop
  ```

  and no longer exits during boot.
- `cargo xtask smoke-test --timeout 90` passes.
- `cargo xtask check` passes.

## Follow-ups

- The current virtio-blk fix intentionally **serializes** requesters instead of teaching the
  legacy driver true multi-request bookkeeping. That is correct for the current code, but it
  is a throughput ceiling, not a scalable queue model.
- If the ring-3 block / VFS stack grows more concurrent at boot, a future cleanup should
  replace the single shared request slot with per-request descriptors, per-request status
  bytes, and per-request completion state.

## Related docs

- `docs/post-mortems/2026-04-21-scheduler-lock-isr-deadlock.md`
- `docs/post-mortems/2026-04-22-e1000-bound-notif.md`
- `docs/roadmap/55c-ring-3-driver-correctness-closure.md`
- `docs/roadmap/tasks/55c-ring-3-driver-correctness-closure-tasks.md`
