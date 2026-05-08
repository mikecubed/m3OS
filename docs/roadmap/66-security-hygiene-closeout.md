# Phase 66 - Security and Hygiene Closeout

**Status:** Planned
**Source Ref:** phase-66
**Depends on:** Phase 48 (Security Foundation) ✅, Phase 54a (Post-Serverization Kernel Hygiene) ✅, Phase 27 (User Accounts) ✅, Phase 28 (Extended Filesystem) ✅
**Builds on:** Closes the Phase 48 and Phase 54a trust-floor gaps that were identified but not implemented; closes supplemental-pass items F4 and F5 from the pre-1.0 blocker list; flips Phase 54a from Planned to Complete
**Primary Components:** kernel VFS unlink/rename, userspace/passwd, userspace/adduser, kernel syscall open/openat/openat2, kernel/src/process/mod.rs, kernel/src/arch/x86_64/syscall/

## Milestone Goal

The five concrete security and hygiene items deferred from Phase 48 and Phase 54a are implemented: `/tmp` enforces sticky-bit deletion semantics; `passwd` and `adduser` write shadow files atomically; `O_CLOEXEC` and `O_NONBLOCK` are honored at file-descriptor construction time; the four layer-crossing wrapper functions are relocated out of `kernel/src/process/mod.rs`; and the pre-seeded image password hash format is upgraded from the Phase 48 `$sha256i$10000$` scheme to a documented current format. Phase 54a's status is flipped from Planned to Complete.

## Why This Phase Exists

Phase 48 established the security foundation but explicitly deferred two trust-floor items (sticky-bit and atomic shadow writes) that were judged out of scope for that phase's primary goal. Phase 54a planned CLOEXEC plumbing and process-module hygiene but remained in Planned status. The supplemental audit pass surfaced these as 1.0 blockers that need an owner phase.

Individually each item is small; collectively they form a coherent "security trust-floor closeout" that belongs in a single phase to avoid scatter across unrelated phases.

## Learning Goals

- Understand how sticky-bit (S_ISVTX) protects shared temporary directories.
- Learn why atomic file writes (temp-file + rename) are the correct pattern for credential stores.
- See how file-descriptor flags set at `open()` time differ from flags set with `fcntl()`.
- Understand why layer-crossing function wrappers in `process/mod.rs` are an architectural smell.

## Feature Scope

### `/tmp` sticky-bit enforcement

The kernel VFS `unlink` and `rename` handlers check S_ISVTX on the parent directory. If the bit is set, the caller must be the file owner, the directory owner, or root. Failure returns `-EACCES`. The `/tmp` directory in the disk image has S_ISVTX set.

### Atomic shadow file writes

`passwd` and `adduser` write shadow-file changes to a temporary file (`/etc/shadow.new`), then call `rename("/etc/shadow.new", "/etc/shadow")`. A crash mid-write produces a `.new` file that can be inspected and removed; the original shadow file is untouched until the rename succeeds.

### CLOEXEC and NONBLOCK flag plumbing

`open`, `openat`, `openat2`, and `vfs_service_open` honor `O_CLOEXEC` and `O_NONBLOCK` at descriptor-creation time. FD flags are stored in the `FileDescription` struct and checked at `execve` (CLOEXEC) and at read/write (NONBLOCK). Before this phase the flags were accepted but silently discarded.

### Layer-crossing wrapper relocation

The four functions `release_socket_pub`, `epoll_free_pub`, `reap_unused_ext2_inode`, and `vfs_service_close_pub` are moved from `kernel/src/process/mod.rs` to their owning modules (`net`, `epoll`, `ext2`, `vfs`). The wrappers in `process/mod.rs` become `pub(crate)` re-exports until callers are updated.

### Pre-seeded image password hash format upgrade

The image bootstrap script and `passwd`/`adduser` are updated to use a documented current hash format. The Phase 48 `$sha256i$10000$` format is replaced; the new format is documented in a code comment naming the algorithm and iteration count. Existing pre-seeded hashes in the disk image are regenerated.

## Important Components and How They Work

### `kernel/src/fs/vfs.rs` — sticky-bit checks in `unlink` and `rename`

A new `check_sticky` helper reads the parent directory's mode, tests S_ISVTX, and compares file owner UID to the calling process UID. Called at the top of both `sys_unlink` and `sys_rename` before any inode mutation.

### `userspace/passwd/src/shadow.rs` and `userspace/adduser/src/shadow.rs`

Both binaries share a `shadow_write_atomic(path, content)` helper in a new `userspace/lib/shadow` crate. The helper: opens `{path}.new` with `O_WRONLY | O_CREAT | O_TRUNC | O_CLOEXEC`, writes all content, calls `fsync`, then calls `rename`. Failure at any step returns an error without touching the original file.

### `kernel/src/fs/file.rs` — `FileDescription` flags

`O_CLOEXEC` is stored as `FdFlags::CLOEXEC` on the `FileDescription`. `execve` iterates all FDs and closes those with `CLOEXEC`. `O_NONBLOCK` is stored as `FileStatusFlags::NONBLOCK` and checked in `read` / `write` before blocking.

### `kernel/src/net/mod.rs`, `kernel/src/epoll.rs`, `kernel/src/fs/ext2/mod.rs`, `kernel/src/vfs_service.rs`

Receive the four relocated functions. `process/mod.rs` retains `pub(crate)` re-exports for one release cycle to avoid breaking all call sites in a single commit.

## How This Builds on Earlier Phases

- Extends Phase 48's security foundation by closing the two trust-floor items that phase explicitly deferred.
- Completes Phase 54a's planned work (CLOEXEC plumbing, process-module hygiene); flips its status to Complete.
- Uses Phase 28's VFS hook points to add the sticky-bit check without restructuring the filesystem layer.
- Uses Phase 27's UID tracking to determine file ownership for the sticky-bit comparison.

## Implementation Outline

Each item in this phase is deliberately narrow in scope (SRP): `check_sticky` touches only the VFS unlink/rename path; `shadow_write_atomic` touches only credential write paths; `FdFlags` touches only descriptor construction; the four wrapper relocations touch only module boundaries. Keep each change isolated — resist combining them into omnibus commits, as mixed diffs obscure both review and bisection.

The `shadow_write_atomic` temp-file-rename pattern is the canonical DRY target for this phase: both `passwd` and `adduser` previously duplicated the direct write path independently. The shared `userspace/lib/shadow` crate eliminates that duplication once and ensures any future credential-writing binary inherits the atomic behavior automatically.

Follow TDD for the sticky-bit check: write the four unit tests (`check_sticky` with bit clear, bit set + owner match, bit set + dir owner match, bit set + neither) in `kernel-core::fs::mode` before implementing the kernel VFS hook. The QEMU integration test is the top of the pyramid, not a substitute for the host-side cases.

1. Write host-side `check_sticky` unit tests in `kernel-core::fs::mode`; then add S_ISVTX constant and implement `check_sticky` in `kernel/src/fs/vfs.rs`.
2. Set S_ISVTX on `/tmp` in the disk image builder.
3. Create `userspace/lib/shadow` crate with `shadow_write_atomic`; update `passwd` and `adduser` to import from the shared crate.
4. Add `FdFlags::CLOEXEC` and `FileStatusFlags::NONBLOCK` to `FileDescription`; wire into `open`, `openat`, `openat2`, `vfs_service_open`, `execve`, `read`, `write`.
5. Move the four layer-crossing wrappers to their owning modules.
6. Upgrade the pre-seeded image password hash format.
7. Update Phase 48 and Phase 54a docs; flip Phase 54a status to Complete.

## Acceptance Criteria

- `cargo xtask test --test sticky_bit` passes: a non-owner non-root process cannot unlink another user's file in a sticky-bit directory; the owner can.
- `cargo xtask test --test shadow_atomic` passes: a crash injected mid-write (via `kill -9` to `passwd`) leaves `/etc/shadow` untouched and `/etc/shadow.new` containing partial content.
- `cargo xtask test --test cloexec_fd` passes: an FD opened with `O_CLOEXEC` is not present in the child after `execve`.
- `grep -rn 'release_socket_pub\|epoll_free_pub\|reap_unused_ext2_inode\|vfs_service_close_pub' kernel/src/process/mod.rs` returns only `pub(crate) use` lines (no function bodies).
- Phase 54a status field reads "Complete" in `docs/roadmap/54a-post-serverization-kernel-hygiene.md`.

## Companion Task List

- [Phase 66 Task List](./tasks/66-security-hygiene-closeout-tasks.md)

## How Real OS Implementations Differ

- Linux implements sticky-bit checks in `may_delete()` in `fs/namei.c`, invoked from both `unlink` and `rename`.
- POSIX `O_CLOEXEC` was standardized in POSIX.1-2008; older code used `fcntl(FD_CLOEXEC)` after `open`.
- Modern systems use `bcrypt` or `argon2id` for password hashing; m3OS uses a SHA-256 iterative scheme for simplicity.

## Deferred Until Later

- bcrypt or argon2id password hashing
- Mandatory access control (SELinux / AppArmor style)
- `setuid` / `setgid` bit enforcement
- Capability bounding sets
- Full POSIX ACL support beyond owner/group/other mode bits
