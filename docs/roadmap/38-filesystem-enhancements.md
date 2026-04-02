# Phase 38 - Filesystem Enhancements

**Status:** Complete
**Source Ref:** phase-38
**Depends on:** Phase 28 (ext2 Filesystem) ✅, Phase 27 (User Accounts) ✅, Phase 13 (Writable FS) ✅
**Builds on:** Extends the ext2 filesystem (Phase 28) with symlinks and hard links, activates the permission mode bits stored since Phase 27, and adds new filesystem types (procfs, device nodes) to the VFS routing layer introduced in Phase 13.
**Primary Components:** kernel/src/fs/ext2.rs, kernel/src/fs/procfs.rs, kernel-core/src/fs/tmpfs.rs, kernel/src/arch/x86_64/syscall.rs, kernel/src/process/mod.rs

## Milestone Goal

The filesystem supports symlinks, hard links, `umask`-backed creation defaults,
the remaining Unix permission semantics needed for symlink-aware metadata and
directory listing, a `/proc` filesystem for process introspection, and basic
device nodes (`/dev/null`, `/dev/zero`, `/dev/urandom`, `/dev/full`). These are the
filesystem primitives that Unix tools and build systems assume exist.

## Why This Phase Exists

Earlier phases gave the filesystem basic read/write capability (Phase 13),
directory operations (Phase 18), ext2 on-disk format (Phase 28), and user/group
identity plus core DAC checks (Phase 27). But there is still no way to create
symbolic or hard links, file creation does not honor a per-process `umask`, and
the filesystem layer has no `/proc` introspection surface or standard Unix
device nodes beyond `/dev/null`. Once symlinks enter the picture, metadata and
directory-reading paths also need to stay aligned with Unix permission rules.
Without these primitives, Unix tools (`make`, `ln`, `ls -l`, `find`), build
systems, and package managers cannot work correctly. This phase fills those
gaps.

## Learning Goals

- Understand the difference between hard links and symbolic links at the inode level.
- Learn how `/proc` provides kernel introspection without dedicated syscalls.
- See how Unix permission enforcement (DAC) works: owner/group/other bits checked on
  every file operation.
- Understand device nodes and the major/minor number scheme.

## Feature Scope

### Symbolic Links

Implement symlinks in tmpfs and ext2:

**Kernel changes:**
- `symlink(target, linkpath)` syscall (88) — create a symlink.
- `symlinkat(target, dirfd, linkpath)` syscall (266).
- `readlink(path, buf, bufsize)` syscall (89) — read symlink target.
- `readlinkat(dirfd, path, buf, bufsize)` syscall (267).
- `lstat()` — stat without following symlinks (distinct from `stat()`).
- Path resolution: follow symlinks during path traversal (with loop detection, max 40 hops).
- `O_NOFOLLOW` flag for `open()` — fail if path is a symlink.

**Storage:**
- tmpfs: symlink target stored as a string in the inode.
- ext2: symlink stored inline (if ≤60 bytes) or in a data block.

### Hard Links

Implement hard links in ext2 (tmpfs hard links are optional):

- `link(oldpath, newpath)` syscall (86) — create a hard link.
- `linkat(olddirfd, oldpath, newdirfd, newpath, flags)` syscall (265).
- Increment inode link count on `link()`, decrement on `unlink()`.
- File is only freed when link count reaches 0 and no open fds remain.
- Cannot hard-link directories (EPERM).
- Cannot hard-link across filesystems (EXDEV).

### Permission Enforcement

Phase 27 introduced the core permission checks for `open()`, `exec()`, and the
main create/delete/rename paths. Phase 38 completes the Unix-facing behavior:

**On symlink-aware metadata and path traversal:**
- `stat()` follows symlinks while `lstat()` and `readlink()` inspect the link itself.
- Symlink traversal still respects directory search permission on every path component.

**On directory operations (`readdir`, `/proc` listing):**
- Check appropriate directory permissions (read for list, execute for traverse).
- Keep synthetic `/proc` and `/dev` entries root-owned with stable mode bits.

**umask:**
- `umask(mask)` syscall (95) — set default permission mask for new files.
- Store per-process `umask` field (default 022).
- Apply on `open(O_CREAT)`, `mkdir()`.

### `/proc` Filesystem

A minimal procfs providing essential kernel introspection:

| Path | Content |
|---|---|
| `/proc/self/` | Symlink to `/proc/<pid>/` |
| `/proc/<pid>/status` | Process name, state, PID, PPID, UID, GID |
| `/proc/<pid>/cmdline` | Command line (NUL-separated) |
| `/proc/<pid>/maps` | Memory mappings (address, perms, path) |
| `/proc/<pid>/exe` | Symlink to the process executable path |
| `/proc/<pid>/fd/` | Directory of open file descriptors (symlinks) |
| `/proc/meminfo` | Total, free, available memory |
| `/proc/uptime` | Seconds since boot, idle time |
| `/proc/version` | Kernel version string |
| `/proc/stat` | Per-CPU time accounting |
| `/proc/mounts` | Mounted filesystems |

**Implementation:**
- Register procfs as a VFS filesystem at `/proc`.
- Generate file contents on read (no storage — synthesized from kernel state).
- Support `opendir`/`readdir` for `/proc/` to list PIDs.

### Device Nodes

Add basic character devices:

| Device | Path | Behavior |
|---|---|---|
| null | `/dev/null` | Read returns EOF; write discards |
| zero | `/dev/zero` | Read returns zero bytes; write discards |
| urandom | `/dev/urandom` | Read returns random bytes (from `getrandom`) |
| full | `/dev/full` | Read returns zero bytes; write returns `ENOSPC` |

**Implementation:**
- Add concrete `FdBackend` variants for `/dev/zero`, `/dev/urandom`, and `/dev/full`
  alongside the existing `DevNull` backend.
- `mknod()` syscall (optional) or create at boot in a devtmpfs.

### `statfs` / `fstatfs`

Implement filesystem statistics syscalls:
- `statfs(path, buf)` (137) — filesystem type, block size, total/free blocks.
- `fstatfs(fd, buf)` (138) — same via fd.

## Important Components and How They Work

### Symlink Path Resolution

The lexical `resolve_path()` helper in `syscall.rs` can remain responsible for
joining `cwd` with a relative path and normalizing `.` / `..`. Actual symlink
following belongs in the FS-aware lookup path (`resolve_fs_target()` and the
filesystem-specific traversal underneath it, including `Ext2Volume::resolve_path()`).
A hop counter (max 40) prevents infinite loops. `O_NOFOLLOW` and `lstat()`
bypass this resolution for the final path component.

### Permission Checker

A `check_permission()` helper in `syscall.rs` implements the standard Unix DAC model:
compare the process's effective UID/GID against the file's owner/group/other mode
bits. Root (UID 0) bypasses all checks. This helper is called from `open()`,
`execve()`, `mkdir()`, `unlink()`, `rename()`, and other path-based syscalls.

### Procfs

A new `kernel/src/fs/procfs.rs` module synthesizes file content on read from kernel
data structures (`PROCESS_TABLE`, frame allocator stats, tick counter). There is no
persistent storage. `resolve_fs_target()` gains a `FsTarget::Proc` variant to route
`/proc/*` paths.

### Device Nodes

Three new `FdBackend` variants (`DevZero`, `DevUrandom`, `DevFull`) join the existing
`DevNull`. They are recognized by `sys_linux_open()` when the path matches `/dev/*`
and dispatch to trivial read/write implementations.

## How This Builds on Earlier Phases

- Extends Phase 28 (ext2) by adding symlink and hard link inode operations to `Ext2Volume`.
- Extends Phase 13 (tmpfs) by adding a `Symlink` variant to `TmpfsNode`.
- Extends Phase 27's DAC model with `umask`, symlink-aware metadata handling, and
  directory-read permission checks.
- Reuses Phase 12's `FdBackend::DevNull` pattern for the new device node backends.
- Reuses Phase 34's timekeeping for `/proc/uptime`.
- Reuses Phase 21's `sys_getrandom()` PRNG for `/dev/urandom` reads.

## Implementation Outline

1. Implement symlinks in tmpfs (create, readlink, path resolution with follow).
2. Implement symlinks in ext2.
3. Add symlink loop detection (max 40 hops, return `ELOOP`).
4. Implement hard links in ext2 (link count management).
5. Complete the permission model for directory listing and symlink-aware metadata lookups.
6. Implement `umask()` syscall with per-process mask.
7. Implement `/proc` filesystem (start with `/proc/<pid>/status` and `/proc/meminfo`).
8. Add `/dev/null`, `/dev/zero`, `/dev/urandom`, `/dev/full` device nodes.
9. Implement `statfs`/`fstatfs`.
10. Write `ln` utility for creating links.
11. Test: `ln -s /bin/sh0 /tmp/mysh && /tmp/mysh` works via symlink.
12. Test: permission enforcement blocks unauthorized access.

## Acceptance Criteria

- `symlink("/bin/sh0", "/tmp/mysh")` creates a working symlink.
- `readlink("/tmp/mysh")` returns `"/bin/sh0"`.
- Symlink chain resolution works (A→B→C) with loop detection.
- Hard links share an inode: modify via one path, see changes via the other.
- `unlink()` of a hard-linked file only removes it when link count hits 0.
- `getdents64()` on a directory without read permission returns `EACCES`.
- `umask(022)` causes `mkdir()` to create directories with mode 0755.
- `cat /proc/self/status` shows the process's own info.
- `readlink /proc/self/exe` returns the current process executable path.
- `cat /proc/meminfo` shows memory statistics.
- `cat /dev/urandom | hexdump | head` produces random output.
- `echo test > /dev/null` succeeds silently.
- `dd if=/dev/zero bs=4096 count=1` produces 4096 zero bytes.
- All existing tests pass without regression.

## Companion Task List

- [Phase 38 Task List](./tasks/38-filesystem-enhancements-tasks.md)

## How Real OS Implementations Differ

Linux's VFS is vastly more complex:
- **Dentry cache** for fast path lookup with negative caching.
- **Inode cache** for recently accessed inodes.
- **Page cache** for file data (unified with VM).
- **Overlay filesystems** (overlayfs, unionfs).
- **Extended attributes** (xattr) for SELinux labels, ACLs, capabilities.
- **Full procfs** with hundreds of entries per process.
- **sysfs** for device/driver introspection.
- **devtmpfs** with udev rules for dynamic device creation.
- **inotify/fanotify** for filesystem event monitoring.

Our implementation provides the essential POSIX filesystem semantics without
caching layers or advanced features.

## Deferred Until Later

- Extended attributes (xattr)
- Access Control Lists (ACLs)
- inotify / fanotify (filesystem events)
- sysfs
- Full /proc (cgroups, net, sys subdirectories)
- File locking (flock, lockf)
- Overlay filesystems
- Page cache for file data
- mknod syscall for arbitrary device creation
