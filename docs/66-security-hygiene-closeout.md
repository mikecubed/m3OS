# Security and Hygiene Closeout (Phase 66)

**Aligned Roadmap Phase:** Phase 66
**Status:** Complete
**Source Ref:** phase-66
**Supersedes Legacy Doc:** new

## Overview

Phase 66 closes the five trust-floor items that Phase 48 (Security Foundation) and Phase 54a (Post-Serverization Kernel Hygiene) identified but did not implement. None of the five items is large individually; collectively they form the "security trust-floor" m3OS is built on, and leaving any one of them open kept the kernel a step away from the safety-floor a learner-friendly OS should already enforce. The closeout: `/tmp` now enforces sticky-bit (`S_ISVTX`) deletion semantics on `unlink` and `rename` before any inode mutation, so a non-owner non-root caller can no longer remove another user's `/tmp` file; `passwd` and `adduser` write `/etc/shadow` atomically through a shared `userspace/lib/shadow` crate (temp-file + `fsync` + `rename`), so a crash mid-write cannot leave the credential store torn; the open-family syscalls honor `O_CLOEXEC` and `O_NONBLOCK` at FD construction (the bits were silently discarded before), so `O_CLOEXEC` is finally race-free across `execve` and `O_NONBLOCK` reads/writes actually return `-EAGAIN` instead of parking; the four `arch::x86_64::syscall::*_pub` FD-teardown wrappers are relocated into the modules that own the underlying tables (`kernel/src/net`, new `kernel/src/epoll`, `kernel/src/fs/ext2`, new `kernel/src/fs/vfs_service`), so `kernel/src/process` no longer reaches across the arch boundary; and the pre-seeded image `/etc/shadow` hashes are regenerated through the same canonical `$sha256i$10000$` helper the in-guest `passwd` binary uses, so the bootstrap format cannot drift from runtime updates.

## What This Doc Covers

- The new `kernel_core::fs::mode` module and `check_sticky` truth-table helper called from `sys_linux_unlink` / `sys_linux_rename`.
- The `userspace/lib/shadow` crate's `shadow_write_atomic` helper and `ShadowFs` trait, and how `passwd` / `adduser` consume it.
- The `O_CLOEXEC` / `O_NONBLOCK` plumbing through `open_resolved_path` and `open_ext2_file`, and why `dup` / `dup2` preservation was already correct.
- The four wrapper relocations (`release_socket_pub`, `epoll_free_pub`, `reap_unused_ext2_inode`, `vfs_service_close_pub`) and the new paths `kernel/src/process/mod.rs` calls them through.
- The canonical `HASH_FORMAT_PREFIX` + `HASH_ROUNDS` constants in `passwd_lib` and the xtask `generate_seeded_shadow_line` helper that mirrors `syscall_lib::sha256::hash_password_iterated` byte-for-byte.

## Key Files

| File | Role |
|---|---|
| `kernel-core/src/fs/mode.rs` | `S_ISVTX` constant and the host-testable `check_sticky` helper. Six `#[cfg(test)]` cases cover the full truth table. |
| `kernel/src/fs/vfs.rs` | Kernel VFS layer (re-export of tmpfs primitives, mount-point bootstrap). |
| `kernel/src/arch/x86_64/syscall/mod.rs` | `sys_linux_unlink`, `sys_linux_rename`, `sys_linux_open`, `sys_linux_openat`, `open_resolved_path`, `open_ext2_file`, `vfs_service_open`. The sticky-bit check fires in unlink/rename ahead of any inode mutation; `O_CLOEXEC` / `O_NONBLOCK` are extracted at the top of `open_resolved_path` and propagated to every `FdEntry` construction. |
| `kernel/src/process/mod.rs` | `FdEntry` (carries both `cloexec` and `nonblock`); `close_cloexec_fds` / `close_all_fds_for` now call the four moved wrappers through `crate::net::*`, `crate::epoll::*`, `crate::fs::ext2::*`, and `crate::fs::vfs_service::*`. |
| `kernel/src/net/mod.rs` | New home for `release_socket_pub` — inlines the UDP-service handshake + `free_socket_with_result` + `finalize_socket_close` orchestration. |
| `kernel/src/epoll.rs` | New module hosting `epoll_free_pub`; forwards to `pub(crate) epoll_free_internal` in the syscall layer until a full epoll extraction lands. |
| `kernel/src/fs/ext2.rs` | New home for `reap_unused_ext2_inode`, next to the `EXT2_VOLUME` table it mutates. |
| `kernel/src/fs/vfs_service.rs` | New module hosting `vfs_service_close_pub`; forwards to `pub(crate) vfs_service_close_internal`. |
| `userspace/lib/shadow/src/lib.rs` | New crate. `shadow_write_atomic`, `ShadowFs` trait, six inline tests + two integration tests covering happy path, partial-write, fsync-fail, rename-fail, open-fail, path-too-long. |
| `userspace/passwd/src/main.rs` | Calls `shadow_write_atomic` instead of the previous direct `open(SHADOW_PATH, O_WRONLY \| O_TRUNC)` + write + fsync. |
| `userspace/passwd/src/lib.rs` | Named constants `HASH_FORMAT_PREFIX = "$sha256i$10000$"` and `HASH_ROUNDS = 10000`, plus a module-doc comment naming the algorithm. |
| `userspace/adduser/src/main.rs` | Reads existing `/etc/shadow`, builds the new content in memory (existing + new line), and commits via `shadow_write_atomic`. Uses `passwd::HASH_FORMAT_PREFIX` instead of the inline literal. |
| `xtask/src/main.rs` | `generate_seeded_shadow_line` mirrors `syscall_lib::sha256::hash_password_iterated` byte-for-byte using the `sha2` workspace crate, pinned to `passwd::HASH_ROUNDS`. The seeded `/etc/shadow` content is generated through it rather than pasted. |

## Closure of Related Phases

- [Phase 48 — Security Foundation](./roadmap/48-security-foundation.md) explicitly deferred the sticky-bit and atomic-shadow-write items. Both are closed here.
- [Phase 54a — Post-Serverization Kernel Hygiene](./roadmap/54a-post-serverization-kernel-hygiene.md) had been in Planned status since it was written; its entire scope (CLOEXEC / NONBLOCK plumbing + the four wrapper relocations) is delivered here, and its Status field has been flipped to Complete.

## Related Roadmap Docs

- [Phase 66 design doc](./roadmap/66-security-hygiene-closeout.md)
- [Phase 66 task list](./roadmap/tasks/66-security-hygiene-closeout-tasks.md)
