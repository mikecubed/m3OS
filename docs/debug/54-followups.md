---
title: Phase 54 follow-up work
status: open
---

# Phase 54: follow-up work

**Status:** Phase 54 deep serverization closed on PR #108. The items below
were surfaced during the closure debugging and review cycle, triaged, and
intentionally deferred. None of them blocks merge; each has a concrete
scope and reason it was not done in the closure PR.

This file replaces two earlier debugging handoffs whose recommendations
were all landed (`docs/debug/54-remaining-smp-race.md` and
`docs/debug/54-review-findings.md`).

## 1. `FdEntry` CLOEXEC / NONBLOCK plumbing (pre-existing, cross-cutting)

**Site:** `kernel/src/arch/x86_64/syscall/mod.rs` — every non-pipe /
socket / epoll `FdEntry` construction hardcodes `cloexec: false,
nonblock: false`. Phase 54's `vfs_service_open` (≈ line 5720) copies the
pattern.

**Impact:** `open(path, O_RDONLY | O_CLOEXEC)` routed through any of
these paths silently loses the CLOEXEC guarantee — the fd survives
`execve` and leaks into the new program. Only `fcntl F_SETFD`, `pipe2`,
`socket(SOCK_CLOEXEC)`, `epoll_create1`, `accept4`, and `socketpair`
honor CLOEXEC at open time today.

**Why deferred:** Not a Phase 54 regression — the defect exists at ~10
call sites and predates this phase. A proper fix threads `flags`
through every `FdEntry::new`-like construction, ideally by introducing
a `FdEntry::from_open_flags(backend, flags)` helper. That's a
cross-cutting cleanup that belongs in its own PR.

**Recommended scope:** one PR that (a) adds the helper, (b) converts
every hardcoded `cloexec: false, nonblock: false` site to consume
`flags`, (c) adds a regression test that verifies `O_CLOEXEC` actually
clears the fd across `execve` for each backend type.

## 2. Relocate `arch::x86_64::syscall::*_pub` wrappers out of `process`

**Sites:** `kernel/src/process/mod.rs:334-348` — `close_cloexec_fds`
and `close_all_fds_for` call four layer-crossing wrappers:

```text
crate::arch::x86_64::syscall::release_socket_pub
crate::arch::x86_64::syscall::epoll_free_pub
crate::arch::x86_64::syscall::reap_unused_ext2_inode
crate::arch::x86_64::syscall::vfs_service_close_pub
```

**Impact:** The generic process-cleanup path has an arch-specific
dependency. Fine today, but it would block any future attempt to host
the `process` module on a second architecture or to extract cleanup
logic for unit testing.

**Why deferred:** The review thread flagged only `release_socket_pub`;
fixing that in isolation would leave the other three on the same
pattern, which is inconsistent. A coherent fix moves all four into
their owning subsystems:

- `release_socket_pub` → `crate::net::release_socket`.
- `epoll_free_pub` → a new `crate::epoll::free` (would need extracting
  epoll out of `syscall/mod.rs` first, or at least hoisting the
  cleanup helper).
- `reap_unused_ext2_inode` → `crate::fs::ext2::reap_unused_inode`.
- `vfs_service_close_pub` → `crate::fs::vfs::service_close` (module
  does not exist yet; would land alongside).

**Recommended scope:** architecture-hygiene PR. No behavior change.

## 3. Optional: `/var/run → /run` compatibility symlink

**Why deferred:** Our own userspace (init, service, crontab) uses
`/run` directly. `/var/run` is a Linux backwards-compatibility shim for
software that predates `/run`. We have no ported software that
hardcodes `/var/run` today.

**When to revisit:** the first time a port refuses to locate its PID
file. At that point, add a symlink in the ext2 disk builder (or a
runtime bootstrap) from `/var/run` to `/run`. Verify the kernel's
symlink resolution in `path_node_nofollow` crosses the ext2 → tmpfs
boundary cleanly (no obvious reason it wouldn't).

## 4. Long-term: replace `MOUNT_OP_LOCK` with a yielding primitive

**Site:** `kernel/src/arch/x86_64/syscall/mod.rs:94` —
`static MOUNT_OP_LOCK: spin::Mutex<()>`.

**Current state after PR #108:** The lock is only held around the
mount / umount mutation itself — path resolution runs outside it — so
"sleep while holding spinlock" is no longer reachable. The remaining
concern is that two cores that do race on the lock still busy-spin in
ring 0 until the holder releases.

**Why deferred:** not hot in practice. Mount / umount is rare.

**Long-term options:**

- Replace with a yielding mutex that calls `task::yield_now()` while
  waiting. Works cleanly for kernel-task callers; needs care for
  callers that already hold the scheduler lock.
- Replace with an `RwLock<()>` — readers (path resolution) take
  shared, writers (mount/umount) take exclusive. Lets parallel path
  resolution proceed while mount/umount is quiescent.

Either fits into a general "cooperative kernel synchronization" pass.

## 5. Long-term: interrupt-driven virtio_blk completion

**Site:** `kernel/src/blk/virtio_blk.rs:413` — `read_sectors`
spin-polls the used ring while holding `DRIVER`. Write path at line
~482 has the same shape.

**Impact:** Under TCG or a busy host the spin-poll can stretch to
tens of ms. Other cores that want `DRIVER` (concurrent file I/O) spin
with it. Not on any known hang path, but adds tail latency and
serializes all block I/O across cores.

**Recommended shape:** register an interrupt handler for the virtio
queue, have `read_sectors` / `write_sectors` submit the descriptor
chain and then `block_current_unless_woken` on a per-request
`AtomicBool` that the interrupt handler sets via `wake_task`. The
handler walks the used ring and wakes one task per completion.

**Why deferred:** not blocking correctness. Belongs in a dedicated
"virtio IRQ completion" task that can also update virtio-net's
receive path in the same style.

## 6. Scheduler diagnostic thresholds — tune with baseline data

**Sites:** `kernel/src/task/scheduler.rs`:

- `[sched] stale-ready` — fires when a Ready task waits ≥ 50 ticks
  (≈ 500 ms) before dispatch.
- `[sched] cpu-hog` — fires when a task held a core ≥ 20 ticks
  (≈ 200 ms) before yielding.

**Why open:** The 200 ms cpu-hog threshold is aggressive — it
surfaces legitimate one-time work during init's service startup and
login. That's fine for now (rare, one-shot, identifiable). If it
becomes noise over day-to-day use, raise to 50 ticks (≈ 500 ms) so
only genuine hangs fire.

**Recommended change:** no code change unless the noise becomes a
problem. Then a one-line threshold bump.

## 7. Parent doc cleanup (this file's own lifecycle)

This file replaces:

- `docs/debug/54-remaining-smp-race.md` — superseded; all six
  recommendations landed on PR #108.
- `docs/debug/54-review-findings.md` — superseded; findings 2–5
  landed, finding 1 is captured as item 1 above.

Delete both in the same commit that publishes this file.
