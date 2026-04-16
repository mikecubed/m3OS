---
current-task: "Resolve PR #108 second-round review comments (9 new + 4 earlier-open)"
current-phase: "fix-batch-1-complete"
next-action: "push branch, reply + resolve threads, verify CI"
workspace: "PR #108 / feat/54-deep-serverization-closure"
last-updated: "2026-04-16T22:10:00Z"
---

## Decisions

All 13 open review threads triaged as VALID; all 13 fixed in a single batch.

Earlier-round:
- sys_access: dropped redundant `vfs_service_access_path` call after successful
  resolve; `vfs_service_access_path` deleted (unused).
- send_signal: IPC wait cancellation now gated on `signal_interrupts_ipc_wait`
  so non-fatal signals (e.g. SIGCHLD, SIGWINCH, blocked or Ignore-disposition
  signals) no longer inject `u64::MAX` into long-running IPC waits. Helper
  `interrupt_ipc_waits()` centralizes the cancel/deliver/wake trio.
- current_exec_path: added non-allocating `is_current_exec_path(expected)`;
  migrated all four hot-path callers (`vfs_service_can_handle_path`,
  `vfs_service_mount_action`, `vfs_service_umount_action`,
  `net_udp_service_available`); removed the now-unused allocating variant.
- Misplaced VfsPathStat doc: relocated `/// Returns true if …` onto
  `vfs_service_can_handle_path`.

Second-round:
- ipc_lookup_service: added `PRIVATE_SERVICE_NAMES` denylist (`vfs`, `net_udp`)
  so unprivileged userspace cannot obtain an endpoint capability to internal
  services and bypass kernel DAC.
- vfs_server handle_stat_path / handle_read / handle_list_dir: bulk-store
  failures now propagate as `NEG_EIO` instead of silently returning success
  without a bulk payload.
- vfs_protocol docs: reply table + VFS_OPEN + VFS_LIST_DIR now document the
  packed `handle | (file_size << 32)` and `bytes | (next_offset << 32)` layouts.
- smoke-runner verify_compiled_hello: error/unlink messages now say `/tmp/h`
  to match `HELLO_BIN_PATH`.
- service_status: parser now uses `split_whitespace()` so tabs and repeated
  spaces don't mis-parse the status field.
- thread-test: added `deadline_reached()` helper and guarded spinner and
  waitpid-timeout loops so failing `gettimeofday()` can't hang the test.

## Files Touched

- kernel/src/arch/x86_64/syscall/mod.rs
- kernel/src/process/mod.rs
- kernel/src/ipc/mod.rs
- kernel-core/src/fs/vfs_protocol.rs
- userspace/vfs_server/src/main.rs
- userspace/smoke-runner/src/main.rs
- userspace/coreutils-rs/src/service.rs
- userspace/thread-test/src/main.rs
- .agent/SESSION.md

Validation: `cargo xtask check` passes — clippy clean, rustfmt clean,
kernel-core host tests pass, passwd host tests pass.

## Open Questions

- None — all 13 items accepted + fixed, local quality gate green.

## Blockers

## Failed Hypotheses
