# Phase 66 - Security and Hygiene Closeout

**Status:** Planned
**Source Ref:** phase-66
**Depends on:** Phase 48 (Security Foundation) ✅, Phase 27 (User Accounts) ✅, Phase 28 (Extended Filesystem) ✅. Phase 54a (Post-Serverization Kernel Hygiene) is Planned and completed-by-this-phase (see Track F.2 in the task list).
**Builds on:** Closes the Phase 48 and Phase 54a trust-floor gaps that were identified but not implemented; closes supplemental-pass items F4 and F5 from the pre-1.0 blocker list; flips Phase 54a from Planned to Complete
**Primary Components:** kernel VFS unlink/rename, userspace/passwd, userspace/adduser, kernel open-family syscalls (`sys_linux_open`, `sys_linux_openat`), `kernel/src/process/mod.rs` (`FdEntry`), `kernel/src/arch/x86_64/syscall/mod.rs`

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

`sys_linux_open` and `sys_linux_openat` honor `O_CLOEXEC` and `O_NONBLOCK` at descriptor-creation time by writing them into the per-FD `FdEntry` struct. CLOEXEC was already enforced for `pipe2`-created and dup-created FDs and is already honored by `execve` via `close_cloexec_fds`; the gap closed here is the `open` path, which previously accepted the flag and discarded it. `O_NONBLOCK` storage is genuinely new: `FdEntry` gains a `nonblock: bool` field and the blocking-capable `read`/`write` paths return `-EAGAIN` instead of parking when it is set.

### Layer-crossing wrapper relocation

The four functions `release_socket_pub`, `epoll_free_pub`, `reap_unused_ext2_inode`, and `vfs_service_close_pub` currently live as wrapper bodies inside `kernel/src/arch/x86_64/syscall/mod.rs`, each reaching across into a subsystem that owns its real data (net socket table, epoll instance table, ext2 inode table, vfs-service handle table). Phase 66 moves each body into the module that owns its data: `kernel/src/net/mod.rs`, a new `kernel/src/epoll.rs`, `kernel/src/fs/ext2.rs`, and a new `kernel/src/fs/vfs_service.rs`. The two FD-teardown call sites inside `kernel/src/process/mod.rs` switch to the new paths in lock-step; no re-export shim is introduced.

### Pre-seeded image password hash format upgrade

The image bootstrap script and `passwd`/`adduser` are updated to use a documented current hash format. The Phase 48 `$sha256i$10000$` format is replaced; the new format is documented in a code comment naming the algorithm and iteration count. Existing pre-seeded hashes in the disk image are regenerated.

## Important Components and How They Work

### `kernel/src/fs/vfs.rs` and `kernel-core/src/fs/mode.rs` — sticky-bit checks in `unlink` and `rename`

A new `kernel-core::fs::mode` module defines `S_ISVTX = 0o1000` and a host-testable `check_sticky(parent_mode, file_uid, dir_uid, caller_uid, caller_is_root)` helper. The kernel calls `check_sticky` at the top of both `sys_linux_unlink` and `sys_linux_rename` (in `kernel/src/arch/x86_64/syscall/mod.rs`) before any inode mutation, across the tmpfs, ext2, and fat32-fallback backends. The `/tmp` directory is already created in tmpfs with mode `0o1777` by `populate_mountpoints` in `kernel/src/fs/tmpfs.rs`, so no disk-image change is required — only the VFS-side enforcement.

### `userspace/lib/shadow/src/lib.rs` — atomic write helper

A new `userspace/lib/shadow` crate exposes `shadow_write_atomic(path, content)`. The helper opens `{path}.new` with `O_WRONLY | O_CREAT | O_TRUNC | O_CLOEXEC`, writes all content, calls `fsync`, then `rename`s `{path}.new` over `{path}`. Failure at any step returns an error without touching the original file. Both `userspace/passwd` and `userspace/adduser` import the crate and remove their direct `/etc/shadow` write paths.

### `kernel/src/process/mod.rs` — `FdEntry` flags

`FdEntry` already carries `cloexec: bool`, and `execve` already closes CLOEXEC FDs via `close_cloexec_fds`. Phase 66 (a) makes `sys_linux_open` and `sys_linux_openat` actually propagate `O_CLOEXEC` into `FdEntry.cloexec` (today the bit is silently discarded by the open path), and (b) adds a parallel `nonblock: bool` field so `O_NONBLOCK` can finally take effect. The blocking-capable `read`/`write` paths consult `FdEntry.nonblock` and return `-EAGAIN` instead of parking when set.

### `kernel/src/net/mod.rs`, `kernel/src/epoll.rs` (new), `kernel/src/fs/ext2.rs`, `kernel/src/fs/vfs_service.rs` (new)

Receive the four relocated wrapper bodies that previously lived in `kernel/src/arch/x86_64/syscall/mod.rs`. The two FD-teardown call sites inside `kernel/src/process/mod.rs` switch to the new paths in lock-step; no re-export shim is introduced.

## How This Builds on Earlier Phases

- Extends Phase 48's security foundation by closing the two trust-floor items that phase explicitly deferred.
- Completes Phase 54a's planned work (CLOEXEC plumbing, process-module hygiene); flips its status to Complete.
- Uses Phase 28's VFS hook points to add the sticky-bit check without restructuring the filesystem layer.
- Uses Phase 27's UID tracking to determine file ownership for the sticky-bit comparison.

## Implementation Outline

Each item in this phase is deliberately narrow in scope (SRP): `check_sticky` touches only the VFS unlink/rename path; `shadow_write_atomic` touches only credential write paths; the `FdEntry.cloexec`/`FdEntry.nonblock` plumbing touches only descriptor construction and the blocking-capable read/write paths; the four wrapper relocations touch only module boundaries. Keep each change isolated — resist combining them into omnibus commits, as mixed diffs obscure both review and bisection.

The `shadow_write_atomic` temp-file-rename pattern is the canonical DRY target for this phase: both `passwd` and `adduser` previously duplicated the direct write path independently. The shared `userspace/lib/shadow` crate eliminates that duplication once and ensures any future credential-writing binary inherits the atomic behavior automatically.

Follow TDD for the sticky-bit check: write the four unit tests (`check_sticky` with bit clear, bit set + owner match, bit set + dir owner match, bit set + neither) in the new `kernel-core::fs::mode` module before wiring the kernel VFS hook. The QEMU integration test is the top of the pyramid, not a substitute for the host-side cases.

1. Write host-side `check_sticky` unit tests in a new `kernel-core/src/fs/mode.rs`; then implement `check_sticky` and call it from `sys_linux_unlink` and `sys_linux_rename` in `kernel/src/arch/x86_64/syscall/mod.rs` for all three filesystem backends (tmpfs, ext2, fat32 fallback). The `/tmp` directory already has `0o1777` via tmpfs's `populate_mountpoints`, so no disk-image change is required.
2. Create `userspace/lib/shadow` crate with `shadow_write_atomic`; update `passwd` and `adduser` to import from the shared crate.
3. Add a `nonblock: bool` field to `FdEntry` in `kernel/src/process/mod.rs`; teach `sys_linux_open` / `sys_linux_openat` (via `open_user_path`) to propagate `O_CLOEXEC` and `O_NONBLOCK` into the new FD; teach the blocking-capable `read`/`write` paths to return `-EAGAIN` when `nonblock` is set.
4. Relocate the four wrapper bodies out of `kernel/src/arch/x86_64/syscall/mod.rs` into `kernel/src/net/mod.rs`, a new `kernel/src/epoll.rs`, `kernel/src/fs/ext2.rs`, and a new `kernel/src/fs/vfs_service.rs`; update the two FD-teardown call sites in `kernel/src/process/mod.rs`.
5. Upgrade the pre-seeded image password hash format and consolidate the hashing helper.
6. Update Phase 48 and Phase 54a docs; flip Phase 54a status to Complete.

## Acceptance Criteria

- `cargo xtask test --test sticky_bit` passes: a non-owner non-root process cannot unlink another user's file in a sticky-bit directory; the owner can.
- `cargo xtask test --test shadow_atomic` passes: a crash injected mid-write (via `kill -9` to `passwd`) leaves `/etc/shadow` untouched and `/etc/shadow.new` containing partial content.
- `cargo xtask test --test cloexec_fd` passes: an FD opened with `O_CLOEXEC` is not present in the child after `execve`.
- `grep -n 'fn release_socket_pub\|fn epoll_free_pub\|fn reap_unused_ext2_inode\|fn vfs_service_close_pub' kernel/src/arch/x86_64/syscall/mod.rs` returns zero lines (the bodies have moved to their owning modules); `kernel/src/process/mod.rs` calls them through the new paths (`crate::net::*`, `crate::epoll::*`, `crate::fs::ext2::*`, `crate::fs::vfs_service::*`).
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
