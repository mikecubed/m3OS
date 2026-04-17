---
current-task: "Resolve PR #108 fifth-round ad-hoc review (3 verified items)"
current-phase: "fix-batch-5-complete"
next-action: "push, reply summary to developer"
workspace: "PR #108 / feat/54-deep-serverization-closure"
last-updated: "2026-04-17T00:05:00Z"
---

## Decisions

Three ad-hoc review items verified against current code. All VALID.

- P1 / `kernel/src/process/mod.rs:287-290` — VALID. `close_cloexec_fds` (and
  also `close_all_fds_for`) lacked a `FdBackend::VfsService` arm; execve
  with CLOEXEC and process exit silently dropped the service_handle without
  sending `VFS_CLOSE`, leaking the server-side handle and eventually
  exhausting the vfs_server's 32-slot table. Fix: added
  `process::vfs_handle_open_count` and `syscall::cleanup_vfs_handle_if_unused`
  helpers; wired them into `sys_linux_close`, `close_cloexec_fds`, and
  `close_all_fds_for`. `VFS_CLOSE` now fires only when the last alias is
  removed.
- P1 / `kernel/src/process/mod.rs:205-211` — VALID. `add_fd_refs` (fork /
  dup2 cloning path) did not refcount `FdBackend::VfsService`, so each
  `sys_close` on a duplicate fired `VFS_CLOSE` and invalidated the
  server-side handle even though sibling fds still referenced it. Fixed by
  the same refcount-via-table-scan approach: close sites now count
  remaining aliases and defer `VFS_CLOSE` to the last one.
- P2 / `userspace/net_server/src/main.rs:54-55` — VALID. `MAX_BINDINGS = 16`
  was a hard system-wide cap on simultaneously bound UDP ports, half the
  kernel's historical `net::MAX_SOCKETS = 32`. Fix: bumped `MAX_BINDINGS`
  to 32 and documented the pairing with kernel's MAX_SOCKETS.

## Files Touched

- kernel/src/process/mod.rs
- kernel/src/arch/x86_64/syscall/mod.rs
- userspace/net_server/src/main.rs
- .agent/SESSION.md

Validation: `cargo xtask check` passes.

## Open Questions

## Blockers

## Failed Hypotheses
