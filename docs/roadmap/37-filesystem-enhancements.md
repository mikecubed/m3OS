# Phase 37 - Filesystem Enhancements

## Milestone Goal

The filesystem supports symlinks, hard links, proper permission enforcement, a `/proc`
filesystem for process introspection, and basic device nodes (`/dev/null`, `/dev/zero`,
`/dev/urandom`). These are the filesystem primitives that Unix tools and build systems
assume exist.

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

Currently `chmod`/`chown` exist but permissions are not checked on file operations.
Add enforcement:

**On `open()`:**
- Check `r/w/x` bits against the process's effective UID/GID.
- Root (UID 0) bypasses all permission checks.
- Return `EACCES` if permission denied.

**On `exec()`:**
- Check execute (`x`) permission.
- Return `EACCES` if not executable.

**On directory operations (`readdir`, `mkdir`, `unlink`, `rename`):**
- Check appropriate directory permissions (write for create/delete, read for list, execute for traverse).

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
- Add a `DeviceNode` FD backend type with major/minor numbers.
- `mknod()` syscall (optional) or create at boot in a devtmpfs.

### `statfs` / `fstatfs`

Implement filesystem statistics syscalls:
- `statfs(path, buf)` (137) — filesystem type, block size, total/free blocks.
- `fstatfs(fd, buf)` (138) — same via fd.

## Prerequisites

| Phase | Why needed |
|---|---|
| Phase 28 (ext2) | ext2 supports inodes for symlinks and hard links |
| Phase 27 (User Accounts) | UID/GID for permission enforcement |
| Phase 13 (Writable FS) | tmpfs for symlink storage |

## Implementation Outline

1. Implement symlinks in tmpfs (create, readlink, path resolution with follow).
2. Implement symlinks in ext2.
3. Add symlink loop detection (max 40 hops, return `ELOOP`).
4. Implement hard links in ext2 (link count management).
5. Add permission checking to `open()`, `exec()`, directory operations.
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
- A non-root user cannot read a file with mode 0600 owned by another user.
- `umask(022)` causes `mkdir()` to create directories with mode 0755.
- `cat /proc/self/status` shows the process's own info.
- `cat /proc/meminfo` shows memory statistics.
- `cat /dev/urandom | hexdump | head` produces random output.
- `echo test > /dev/null` succeeds silently.
- `dd if=/dev/zero bs=4096 count=1` produces 4096 zero bytes.
- All existing tests pass without regression.

## Companion Task List

- Phase 37 Task List — *not yet created*

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
