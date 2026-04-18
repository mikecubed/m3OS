# Phase 54a - Post-Serverization Kernel Hygiene

**Status:** Planned
**Source Ref:** phase-54a
**Depends on:** Phase 54 (Deep Serverization) ✅
**Builds on:** Closes the two cross-cutting kernel-hygiene items surfaced during Phase 54's closure review that were intentionally scoped out of PR #108 because they pre-date the serverization work and touch code beyond that PR's surface.
**Primary Components:** kernel/src/process, kernel/src/arch/x86_64/syscall, kernel/src/net, kernel/src/fs, kernel/src/epoll

## Milestone Goal

Close the `FdEntry` CLOEXEC / NONBLOCK plumbing gap and relocate the four `arch::x86_64::syscall::*_pub` wrappers into their owning subsystems so later phases do not inherit a silent security bug or a process-module arch dependency.

## Why This Phase Exists

Phase 54 extracted storage and UDP policy into supervised ring-3 services. Its closure review surfaced two items that were correctly deferred from the closure PR because they affect code the PR did not otherwise touch, but that cannot be left indefinitely:

1. **`FdEntry` CLOEXEC / NONBLOCK plumbing.** Every non-pipe, non-socket, non-epoll `FdEntry` construction site in `kernel/src/arch/x86_64/syscall/mod.rs` hardcodes `cloexec: false, nonblock: false`. `open(path, O_RDONLY | O_CLOEXEC)` therefore silently drops the CLOEXEC guarantee — the fd survives `execve` and leaks into the new program. Security-sensitive fd-creation paths (`pipe2`, `socket(SOCK_CLOEXEC)`, `epoll_create1`, `accept4`, `socketpair`, `fcntl F_SETFD`) already honor the flag, so the exposure is bounded, but `open` / `openat` / `openat2` and Phase 54's new `vfs_service_open` do not.
2. **`arch::x86_64::syscall::*_pub` wrappers in `kernel/src/process/mod.rs`.** `close_cloexec_fds` and `close_all_fds_for` currently call four layer-crossing wrappers (`release_socket_pub`, `epoll_free_pub`, `reap_unused_ext2_inode`, `vfs_service_close_pub`). The review thread flagged only one; a coherent fix relocates all four into their owning subsystems. Phase 54 itself added `vfs_service_close_pub`, so the pattern is actively growing.

Grouping these into a named aftermath phase follows the `52a / 52b / 52c / 52d / 53a` precedent and gives both items a tracking surface without re-opening the Phase 54 PR.

## Learning Goals

- Understand why `O_CLOEXEC` must be threaded through every fd-creating syscall path, not only the ones that expose a flags argument at the type level.
- See how ad-hoc `*_pub` wrappers accumulate when cross-layer calls lack a clear owner module, and how to relocate them into the owning subsystem without changing behavior.
- Recognize when a multi-item debug followup file should be promoted to a tracked phase rather than left as unplanned backlog.

## Feature Scope

### CLOEXEC / NONBLOCK plumbing through `FdEntry`

Introduce a `FdEntry::from_open_flags(backend, flags)` helper (or the simplest moral equivalent) and convert every hardcoded `cloexec: false, nonblock: false` construction site to consume the originating syscall's `flags` argument. Add a per-backend regression test that verifies `O_CLOEXEC` actually clears the fd across `execve`.

### Relocate `arch::x86_64::syscall::*_pub` wrappers

Move four layer-crossing wrappers out of `arch::x86_64::syscall` into their owning subsystems so `kernel/src/process/mod.rs` no longer has an arch-specific dependency for generic cleanup:

- `release_socket_pub` → `crate::net::release_socket`
- `epoll_free_pub` → `crate::epoll::free`
- `reap_unused_ext2_inode` → `crate::fs::ext2::reap_unused_inode`
- `vfs_service_close_pub` → `crate::fs::vfs::service_close`

The move preserves behavior — Phase 54a is not allowed to change process-cleanup semantics, including the VFS close refcounting fix that landed in PR #108.

## Important Components and How They Work

### `FdEntry::from_open_flags`

Central helper that turns a syscall-level `flags` argument into the correct `FdEntry` fields. Every backend-specific open path (socket, epoll, pipe, open, openat, openat2, `vfs_service_open`) routes its flags through this helper rather than hardcoding `false, false`. New backends inherit the correct handling by default.

### Subsystem-owned cleanup entry points

Each subsystem that owns an fd backend exposes its own free / release function. `kernel/src/process/mod.rs::close_cloexec_fds` and `close_all_fds_for` call those functions directly, eliminating the last arch-specific import from process cleanup.

## How This Builds on Earlier Phases

- Closes open items carried over from Phase 54's closure review.
- Extends the fd-lifecycle model already used by `pipe2`, `socket(SOCK_CLOEXEC)`, `accept4`, `epoll_create1`, `socketpair`, and `fcntl F_SETFD` to every other fd-creating syscall.
- Preserves the Phase 54 direction by keeping the new `vfs_service_close` in the fs-layer rather than leaving it as arch-syscall glue.

## Implementation Outline

1. Introduce `FdEntry::from_open_flags(backend, flags)` in the module that owns `FdEntry`.
2. Convert every hardcoded `cloexec: false, nonblock: false` construction site in `kernel/src/arch/x86_64/syscall`, including `vfs_service_open`.
3. Add a per-backend userspace regression test that execs and verifies `O_CLOEXEC` cleared the fd.
4. Relocate each of the four `*_pub` wrappers into its owning subsystem; update `kernel/src/process/mod.rs` call sites.
5. Trim `docs/debug/54-followups.md` to the two long-term backlog items (MOUNT_OP_LOCK yielding, scheduler thresholds) that intentionally remain open.

## Acceptance Criteria

- `open(path, O_RDONLY | O_CLOEXEC)` results in an fd that does not survive `execve`, verified by a regression test, for every backend the kernel exposes.
- `grep -rn "cloexec: false, nonblock: false" kernel/src` returns only call sites that deliberately create a non-CLOEXEC fd, each carrying an inline comment explaining why.
- `kernel/src/process/mod.rs` no longer imports any symbol from `crate::arch::x86_64::syscall`.
- `docs/debug/54-followups.md` contains only the two long-term backlog items, each annotated with its owner.
- Kernel version is bumped to `v0.54.1` across `kernel/Cargo.toml`, `AGENTS.md`, `README.md`, and both roadmap READMEs.
- All existing QEMU tests pass unchanged.

## Companion Task List

- [Phase 54a Task List](./tasks/54a-post-serverization-kernel-hygiene-tasks.md)

## Related Documentation and Version Updates

- When the phase lands, bump `kernel/Cargo.toml` to `0.54.1` — a patch-level bump on top of the `v0.54.0` baseline that Phase 54 closed.
- Correct the stale `v0.51.0` mention in the `AGENTS.md` project-overview paragraph to `v0.54.1` in the same commit.
- Update `README.md` if it mentions a kernel version, and set the Phase 54a row status to `Complete` in both `docs/roadmap/README.md` and `docs/roadmap/tasks/README.md`.
- Phase 55 takes over from `v0.54.1` and bumps to `v0.55.0` at its own close; there is no intermediate re-bump in between.

## How Real OS Implementations Differ

- Linux threads `O_CLOEXEC` through `file_operations`-level open, so the pattern cannot skip a backend in the way `FdEntry` currently can.
- Mature kernels put per-subsystem fd cleanup behind VFS-style dispatch tables; the ad-hoc `*_pub` wrapper pattern is a transitional artifact of m3OS's current flat `FdEntry`.
- Phase 54a does not introduce a dispatch table — it only adds the consistent helper and moves the wrappers.

## Deferred Until Later

- MOUNT_OP_LOCK yielding primitive — tracked as long-term scheduling work in `docs/debug/54-followups.md`.
- Scheduler diagnostic threshold tuning — no action unless the current thresholds become noisy; tracked in `docs/debug/54-followups.md`.
- Full epoll extraction out of `kernel/src/arch/x86_64/syscall/mod.rs` — Phase 54a hoists only the cleanup helper; a full module split remains a later refactor if it becomes blocking.
