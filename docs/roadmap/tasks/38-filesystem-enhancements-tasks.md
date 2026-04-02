# Phase 38 — Filesystem Enhancements: Task List

**Status:** Planned
**Source Ref:** phase-38
**Depends on:** Phase 28 (ext2 Filesystem) ✅, Phase 27 (User Accounts) ✅, Phase 13 (Writable FS) ✅
**Goal:** Add symlinks, hard links, `umask`, the remaining Unix permission-model
work needed around directory reads and symlink-aware metadata, a minimal `/proc`
filesystem, device nodes (`/dev/null`, `/dev/zero`, `/dev/urandom`, `/dev/full`),
and `statfs`/`fstatfs` syscalls. These are the filesystem primitives that Unix
tools and build systems assume exist.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Symlink support (tmpfs + ext2 + path resolution) | — | Planned |
| B | Hard link support (ext2) | — | Planned |
| C | Permission-model completion and umask | A | Planned |
| D | `/proc` filesystem | A | Planned |
| E | Device nodes | — | Planned |
| F | `statfs` / `fstatfs` syscalls | — | Planned |
| G | Userspace utilities | A, B | Planned |
| H | Integration testing and documentation | A–G | Planned |

---

## Track A — Symlink Support

Implement symbolic links in tmpfs and ext2, including path resolution with
symlink following and loop detection.

### A.1 — Add `Symlink` variant to `TmpfsNode`

**File:** `kernel-core/src/fs/tmpfs.rs`
**Symbol:** `TmpfsNode`
**Why it matters:** tmpfs currently has `File` and `Dir` variants only. Symlinks
require a third variant that stores the target path string instead of file data.

**Acceptance:**
- [ ] `TmpfsNode::Symlink(SymlinkData)` variant added with `target: String` field
- [ ] `TmpfsStat` extended with `is_symlink: bool` field
- [ ] Existing `Tmpfs::stat()` returns `is_symlink = true` for symlink nodes
- [ ] `cargo test -p kernel-core` passes

### A.2 — Implement `Tmpfs::create_symlink()` and `Tmpfs::read_symlink()`

**File:** `kernel-core/src/fs/tmpfs.rs`
**Symbol:** `Tmpfs::create_symlink`, `Tmpfs::read_symlink`
**Why it matters:** These are the low-level operations for creating and reading
symlinks in tmpfs. The kernel syscall handlers delegate to these methods.

**Acceptance:**
- [ ] `create_symlink(path, target)` creates a `TmpfsNode::Symlink` at `path`
- [ ] `read_symlink(path)` returns the target string for symlink nodes
- [ ] `read_symlink()` on a non-symlink returns an error
- [ ] `unlink()` deletes symlink nodes (existing delete path works)
- [ ] Host tests validate round-trip create/read symlinks

### A.3 — Implement ext2 symlink creation and reading

**File:** `kernel/src/fs/ext2.rs`
**Symbol:** `Ext2Volume::create_symlink`, `Ext2Volume::read_symlink`
**Why it matters:** ext2 stores short symlink targets inline in the inode's block
pointers (up to 60 bytes) and longer targets in a data block. Both paths must work.

**Acceptance:**
- [ ] `create_symlink(parent_ino, name, target)` allocates an inode with `S_IFLNK` mode
- [ ] Short targets (<=60 bytes) stored inline in `Ext2Inode::block` array
- [ ] Long targets (>60 bytes) stored in an allocated data block
- [ ] Directory entry created with `EXT2_FT_SYMLINK` (7) file type
- [ ] `read_symlink(ino)` returns the target string from inline or block storage
- [ ] Inode `links_count` set to 1 on creation

### A.4 — Implement `sys_symlink()` and `sys_symlinkat()` syscalls

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `sys_symlink`, `sys_symlinkat`
**Why it matters:** These are the userspace entry points for symlink creation.
They must route through `resolve_fs_target()` to the correct filesystem backend.

**Acceptance:**
- [ ] Syscall 88 dispatches to `sys_symlink(target_ptr, linkpath_ptr)`
- [ ] Syscall 266 dispatches to `sys_symlinkat(target_ptr, dirfd, linkpath_ptr)`
- [ ] Routes to `Tmpfs::create_symlink()` for `/tmp/*` paths
- [ ] Routes to `Ext2Volume::create_symlink()` for ext2-backed paths at `/` (with `/data/*` as legacy fallback if present)
- [ ] Returns `NEG_EROFS` for ramdisk paths
- [ ] Returns `NEG_EEXIST` if linkpath already exists

### A.5 — Implement `sys_readlink()` and `sys_readlinkat()` syscalls

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `sys_readlink`, `sys_readlinkat`
**Why it matters:** `readlink()` reads the target of a symlink without following
it. Many tools (ls -l, realpath, find) depend on this.

**Acceptance:**
- [ ] Syscall 89 dispatches to `sys_readlink(path_ptr, buf_ptr, bufsize)`
- [ ] Syscall 267 dispatches to `sys_readlinkat(dirfd, path_ptr, buf_ptr, bufsize)`
- [ ] Copies symlink target into userspace buffer, returns byte count
- [ ] Returns `NEG_EINVAL` if path is not a symlink
- [ ] Truncates target to `bufsize` without NUL terminator (POSIX behavior)

### A.6 — Add symlink following to path resolution

**Files:**
- `kernel/src/arch/x86_64/syscall.rs`
- `kernel/src/fs/ext2.rs`
**Symbol:** `resolve_fs_target`, `Ext2Volume::resolve_path`
**Why it matters:** Every path-based syscall (`open`, `stat`, `exec`, `chdir`, etc.)
must follow symlinks during path traversal. The lexical `resolve_path()` helper
can stay as a normalizer, but the FS-aware lookup path (`resolve_fs_target()`
and the filesystem-specific resolution underneath it) must detect symlinks and
resolve them.

**Acceptance:**
- [ ] Path resolution follows symlinks in each component (not just the final one)
- [ ] FS-aware lookup in `syscall.rs` detects tmpfs symlinks and substitutes targets
- [ ] `Ext2Volume::resolve_path()` detects `is_symlink()` inodes and follows them
- [ ] Absolute symlink targets restart resolution from root
- [ ] Relative symlink targets resolve relative to the symlink's directory
- [ ] `ELOOP` returned after 40 hops (symlink loop detection)
- [ ] `O_NOFOLLOW` flag in `open()` returns `NEG_ELOOP` if final component is a symlink

### A.7 — Implement `lstat()` / `AT_SYMLINK_NOFOLLOW` behavior

**Files:**
- `kernel/src/arch/x86_64/syscall.rs`
- `userspace/syscall-lib/src/lib.rs`
**Symbol:** `sys_linux_fstatat`, `newfstatat`
**Why it matters:** `lstat()` is the non-following metadata primitive that
userspace tools need to inspect symlinks themselves. Without explicit
`AT_SYMLINK_NOFOLLOW` handling, `ls -l` and `stat` cannot distinguish link
metadata from target metadata.

**Acceptance:**
- [ ] `sys_linux_fstatat()` honors `AT_SYMLINK_NOFOLLOW` for the final path component
- [ ] Syscall 6 (`lstat`) routes through the non-following path instead of behaving like `stat`
- [ ] `userspace/syscall-lib` exposes an `lstat`-style wrapper using the existing byte-slice API conventions
- [ ] `ls` / `stat` callers can request symlink metadata without following the target

### A.8 — Add error constants for symlink/link operations

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `NEG_ELOOP`, `NEG_EXDEV`
**Why it matters:** Symlink loops need `ELOOP` (errno 40) and cross-device links
need `EXDEV` (errno 18). These constants are not yet defined.

**Acceptance:**
- [ ] `NEG_ELOOP` = `(-40_i64) as u64` added to error constants block
- [ ] `NEG_EXDEV` = `(-18_i64) as u64` added to error constants block

### A.9 — Add symlink wrappers to syscall-lib

**File:** `userspace/syscall-lib/src/lib.rs`
**Symbol:** `symlink`, `readlink`, `symlinkat`, `readlinkat`
**Why it matters:** Userspace Rust binaries need safe wrappers for the new syscalls.

**Acceptance:**
- [ ] `pub fn symlink(target: &[u8], linkpath: &[u8]) -> isize` wrapper added
- [ ] `pub fn readlink(path: &[u8], buf: &mut [u8]) -> isize` wrapper added
- [ ] `pub fn symlinkat(target: &[u8], dirfd: i32, linkpath: &[u8]) -> isize` wrapper added
- [ ] `pub fn readlinkat(dirfd: i32, path: &[u8], buf: &mut [u8]) -> isize` wrapper added

---

## Track B — Hard Link Support

Implement hard links in ext2 with proper link count management.

### B.1 — Implement `Ext2Volume::create_hard_link()`

**File:** `kernel/src/fs/ext2.rs`
**Symbol:** `Ext2Volume::create_hard_link`
**Why it matters:** Hard links create a new directory entry pointing to an existing
inode without allocating a new inode. The inode's `links_count` must be incremented.

**Acceptance:**
- [ ] `create_hard_link(parent_ino, name, target_ino)` creates a dir entry for existing inode
- [ ] Inode `links_count` incremented by 1 and written back to disk
- [ ] Returns error if target inode is a directory (hard-linking directories is forbidden)
- [ ] Returns error if target inode is on a different filesystem

### B.2 — Preserve unlinked ext2 files until the last open FD closes

**Files:**
- `kernel/src/fs/ext2.rs`
- `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `Ext2Volume::delete_file`, `sys_linux_close`, `FdBackend::Ext2Disk`
**Why it matters:** ext2 already tracks `links_count`, but Unix unlink semantics
also require an unlinked file to remain accessible through existing file
descriptors until the final close. That lifetime is not currently tracked for
ext2-backed FDs.

**Acceptance:**
- [ ] `unlink()` removes the directory entry immediately but open ext2 FDs keep working
- [ ] Final block/inode reclamation happens only after `links_count == 0` and the last ext2 FD closes
- [ ] `sys_linux_close()` participates in the final cleanup path for unlinked ext2 files
- [ ] Hard-linked files with remaining names are unaffected by the extra FD-lifetime tracking

### B.3 — Implement `sys_link()` and `sys_linkat()` syscalls

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `sys_link`, `sys_linkat`
**Why it matters:** These are the userspace entry points for hard link creation.

**Acceptance:**
- [ ] Syscall 86 dispatches to `sys_link(oldpath_ptr, newpath_ptr)`
- [ ] Syscall 265 dispatches to `sys_linkat(olddirfd, oldpath_ptr, newdirfd, newpath_ptr, flags)`
- [ ] Routes to `Ext2Volume::create_hard_link()` for ext2 paths
- [ ] Returns `NEG_EPERM` when attempting to hard-link a directory
- [ ] Returns `NEG_EXDEV` when oldpath and newpath are on different filesystems
- [ ] Returns `NEG_EROFS` for ramdisk or tmpfs targets (no tmpfs hard links)

### B.4 — Add link wrappers to syscall-lib

**File:** `userspace/syscall-lib/src/lib.rs`
**Symbol:** `link`, `linkat`
**Why it matters:** Userspace Rust binaries need safe wrappers for the link syscalls.

**Acceptance:**
- [ ] `pub fn link(oldpath: &[u8], newpath: &[u8]) -> isize` wrapper added
- [ ] `pub fn linkat(olddirfd: i32, oldpath: &[u8], newdirfd: i32, newpath: &[u8], flags: i32) -> isize` wrapper added

---

## Track C — Permission-Model Completion and `umask`

Phase 27 already added the core DAC checks for `open()`, `execve()`, and the
main create/delete/rename paths. Phase 38 finishes the filesystem-facing pieces
that become important once symlinks and procfs exist, and adds `umask()`.

### C.1 — Enforce directory listing permission in `getdents64()`

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `sys_linux_getdents64`
**Why it matters:** Directory mutation checks already exist, but listing a
directory should also require the correct DAC bits. Without this, unreadable
directories can still be enumerated.

**Acceptance:**
- [ ] `getdents64()` checks directory read permission before returning entries
- [ ] Traversal still requires execute/search permission on the directory path
- [ ] Returns `NEG_EACCES` for callers lacking the required bits
- [ ] Root bypass remains unchanged

### C.2 — Keep metadata and symlink syscalls aligned with DAC rules

**Files:**
- `kernel/src/arch/x86_64/syscall.rs`
- `kernel/src/fs/ext2.rs`
**Symbol:** `path_metadata`, `parent_dir_metadata`, `sys_linux_fstatat`, `sys_readlink`, `resolve_path`
**Why it matters:** Once the kernel follows symlinks, metadata lookups must keep
permission checks attached to the correct object. `stat()` should follow the
target, while `lstat()` and `readlink()` must inspect the link itself without
skipping directory search permissions.

**Acceptance:**
- [ ] `stat()`/`open()` permission checks apply to the resolved target after symlink traversal
- [ ] `lstat()` and `readlink()` use the symlink's own metadata for the final component
- [ ] Parent-directory search permission is still required even when the final component is a symlink
- [ ] Synthetic `/proc` and `/dev` entries continue to expose stable root-owned metadata

### C.3 — Add `umask` field to `Process`

**File:** `kernel/src/process/mod.rs`
**Symbol:** `Process`
**Why it matters:** The per-process umask determines the default permissions for
newly created files and directories. It must be stored in the process and
inherited across `fork()`.

**Acceptance:**
- [ ] `Process` struct has `umask: u16` field, default 0o022
- [ ] `fork()` copies parent's `umask` to child
- [ ] `exec()` preserves the umask

### C.4 — Implement `sys_umask()` and apply it to create operations

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `sys_umask`
**Why it matters:** The `umask()` syscall sets the process's file creation mask
and returns the old value.

**Acceptance:**
- [ ] Syscall 95 dispatches to `sys_umask(mask)`
- [ ] Returns the previous umask value
- [ ] Sets new umask to `mask & 0o777`
- [ ] `open(O_CREAT)` applies umask: `effective_mode = mode & ~umask`
- [ ] `mkdir()` applies umask: `effective_mode = mode & ~umask`

### C.5 — Add `umask` wrapper to syscall-lib

**File:** `userspace/syscall-lib/src/lib.rs`
**Symbol:** `umask`
**Why it matters:** Userspace Rust binaries need a wrapper for the umask syscall.

**Acceptance:**
- [ ] `pub fn umask(mask: u32) -> isize` wrapper added

---

## Track D — `/proc` Filesystem

Implement a minimal procfs providing essential kernel introspection.

### D.1 — Add `ProcFs` struct and VFS registration

**Files:**
- `kernel/src/fs/mod.rs`
- `kernel/src/fs/procfs.rs` (new file)
**Symbol:** `ProcFs`
**Why it matters:** `/proc` must be recognized by `resolve_fs_target()` as a
distinct filesystem target. A new `FsTarget::Proc` variant routes `/proc/*`
paths to the procfs handler.

**Acceptance:**
- [ ] `kernel/src/fs/procfs.rs` module created and registered in `fs/mod.rs`
- [ ] `FsTarget::Proc(String)` variant added to `FsTarget` enum
- [ ] `resolve_fs_target()` routes `/proc/*` paths to `FsTarget::Proc`
- [ ] `FdBackend::Proc` variant added for open procfs file descriptors

### D.2 — Implement `/proc/<pid>/status`

**File:** `kernel/src/fs/procfs.rs`
**Symbol:** `proc_pid_status`
**Why it matters:** This is the most commonly read per-process file, showing
name, state, PID, PPID, UID, and GID. `ps` and other tools rely on it.

**Acceptance:**
- [ ] Reading `/proc/<pid>/status` returns formatted process metadata
- [ ] Output includes: Name, State, Pid, PPid, Uid, Gid fields
- [ ] Returns `NEG_ENOENT` for non-existent PIDs
- [ ] Content generated on-read from `PROCESS_TABLE`

### D.3 — Implement `/proc/<pid>/cmdline`

**File:** `kernel/src/fs/procfs.rs`
**Symbol:** `proc_pid_cmdline`
**Why it matters:** `cmdline` provides the command-line arguments of a process,
NUL-separated. Used by `ps`, `top`, and process monitors.

**Acceptance:**
- [ ] Reading `/proc/<pid>/cmdline` returns NUL-separated argument strings
- [ ] Returns empty for processes with no stored arguments
- [ ] Process struct stores `cmdline: String` (set during `execve`)

### D.4 — Implement `/proc/<pid>/maps`

**File:** `kernel/src/fs/procfs.rs`
**Symbol:** `proc_pid_maps`
**Why it matters:** `/proc/<pid>/maps` is the standard process-memory
introspection file. Debuggers, shells, and future tooling rely on it to inspect
mapped regions and permissions.

**Acceptance:**
- [ ] Reading `/proc/<pid>/maps` emits one line per mapped region
- [ ] Each line includes start/end addresses, rwx-style permissions, and a path or region label
- [ ] Anonymous mappings, ELF segments, stack, and heap regions are represented consistently
- [ ] Output is generated from the process VMA list at read time

### D.5 — Implement `/proc/<pid>/fd/` directory and symlink entries

**File:** `kernel/src/fs/procfs.rs`
**Symbol:** `proc_pid_fd_dir`, `proc_pid_fd_link`
**Why it matters:** `/proc/<pid>/fd/` is how Unix tools inspect a process's open
descriptors. The entries should behave like symlinks back to the underlying
paths or synthetic device names.

**Acceptance:**
- [ ] `/proc/<pid>/fd/` lists one entry per open file descriptor
- [ ] `readlink("/proc/<pid>/fd/<n>")` returns a stable target string for that FD
- [ ] Closed FD slots are omitted from the directory listing
- [ ] Pipe, socket, PTY, and device FDs use readable synthetic targets when no real path exists

### D.6 — Implement `/proc/<pid>/exe` and `/proc/self` symlinks

**Files:**
- `kernel/src/fs/procfs.rs`
- `kernel/src/process/mod.rs`
- `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `proc_pid_exe`, `proc_self`, `Process`, `sys_execve`
**Why it matters:** `/proc/<pid>/exe` is the standard symlink to a process's
current executable, and `/proc/self` resolves the calling process to its PID
directory. Tools use `/proc/self/exe` to find their own binary path without
argv-dependent heuristics.

**Acceptance:**
- [ ] `Process` stores the executable path separately from `cmdline`
- [ ] `sys_execve` records the resolved executable path when loading a new image
- [ ] `readlink("/proc/<pid>/exe")` returns the process executable path when known
- [ ] `readlink("/proc/self/exe")` resolves through `/proc/self` to the current executable path
- [ ] `/proc/self` resolves to `/proc/<pid>/` using `current_pid()`
- [ ] Works with all per-PID procfs files

### D.7 — Implement `/proc/meminfo` and `/proc/stat`

**File:** `kernel/src/fs/procfs.rs`
**Symbol:** `proc_meminfo`, `proc_stat`
**Why it matters:** `/proc/meminfo` exposes total, free, and available memory.
`/proc/stat` exposes aggregate CPU accounting. Together they provide the global
kernel-introspection files most userspace tools expect.

**Acceptance:**
- [ ] Reading `/proc/meminfo` returns MemTotal, MemFree, MemAvailable lines
- [ ] Values sourced from the frame allocator statistics
- [ ] Output in kB units matching Linux format
- [ ] Reading `/proc/stat` returns at least `cpu` aggregate counters and boot count lines
- [ ] CPU counters are derived from the scheduler/tick accounting available in the kernel

### D.8 — Implement `/proc/uptime`, `/proc/version`, and `/proc/mounts`

**File:** `kernel/src/fs/procfs.rs`
**Symbol:** `proc_uptime`, `proc_version`, `proc_mounts`
**Why it matters:** `/proc/uptime` provides seconds since boot (reusing Phase 34
timekeeping), `/proc/version` identifies the kernel, and `/proc/mounts` reports
the mounted filesystems.

**Acceptance:**
- [ ] `/proc/uptime` returns `<seconds_since_boot> 0.00` (idle time stubbed)
- [ ] `/proc/version` returns `m3os version <version>` string
- [ ] Uptime sourced from the tick counter or RTC
- [ ] `/proc/mounts` lists each mounted filesystem with device, mount point, and type
- [ ] Includes ramdisk, tmpfs, ext2 mounted at `/`, and procfs entries
- [ ] Format: `<device> <mountpoint> <type> <options> 0 0`

### D.9 — Implement procfs directory listing

**File:** `kernel/src/fs/procfs.rs`
**Symbol:** `proc_readdir`
**Why it matters:** `ls /proc/` must list PID directories and global files.
`ls /proc/<pid>/` must list the per-process files.

**Acceptance:**
- [ ] `readdir("/proc/")` lists PID directories + `self`, `meminfo`, `uptime`, `version`, `mounts`
- [ ] `readdir("/proc/<pid>/")` lists `status`, `cmdline`, `maps`, `exe`, `fd`
- [ ] `readdir("/proc/<pid>/fd/")` lists one entry per open file descriptor
- [ ] PIDs listed only for processes that exist in `PROCESS_TABLE`
- [ ] Integrates with existing `sys_linux_getdents64()` dispatch

---

## Track E — Device Nodes

Add `/dev/zero`, `/dev/urandom`, and `/dev/full` device nodes as new FD backend
types. (`/dev/null` already exists as `FdBackend::DevNull`.)

### E.1 — Add `FdBackend::DevZero` variant

**File:** `kernel/src/process/mod.rs`
**Symbol:** `FdBackend::DevZero`
**Why it matters:** `/dev/zero` returns zero bytes on read and discards writes.
It is used by `dd`, `mmap` MAP_ANONYMOUS alternatives, and test tools.

**Acceptance:**
- [ ] `FdBackend::DevZero` variant added to `FdBackend` enum
- [ ] `open("/dev/zero")` creates an FD with `DevZero` backend
- [ ] Read fills buffer with zero bytes, returns requested count
- [ ] Write discards data, returns requested count
- [ ] `fstat` returns character device mode (`0x2000 | 0o666`)

### E.2 — Add `FdBackend::DevUrandom` variant

**File:** `kernel/src/process/mod.rs`
**Symbol:** `FdBackend::DevUrandom`
**Why it matters:** `/dev/urandom` returns random bytes on read, reusing the
existing `sys_getrandom()` PRNG. Programs that read `/dev/urandom` directly
(rather than calling `getrandom()`) need this.

**Acceptance:**
- [ ] `FdBackend::DevUrandom` variant added to `FdBackend` enum
- [ ] `open("/dev/urandom")` creates an FD with `DevUrandom` backend
- [ ] Read fills buffer with PRNG bytes (reuse `sys_getrandom` logic)
- [ ] Write discards data (accepted but ignored, like Linux)
- [ ] `fstat` returns character device mode (`0x2000 | 0o666`)

### E.3 — Add `FdBackend::DevFull` variant

**File:** `kernel/src/process/mod.rs`
**Symbol:** `FdBackend::DevFull`
**Why it matters:** `/dev/full` returns zero bytes on read but returns `ENOSPC`
on write. It is used for testing error handling in write paths.

**Acceptance:**
- [ ] `FdBackend::DevFull` variant added to `FdBackend` enum
- [ ] `open("/dev/full")` creates an FD with `DevFull` backend
- [ ] Read fills buffer with zero bytes (same as `/dev/zero`)
- [ ] Write returns `NEG_ENOSPC`
- [ ] `fstat` returns character device mode (`0x2000 | 0o666`)

### E.4 — Wire device nodes into `sys_linux_open()` dispatch

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `sys_linux_open`
**Why it matters:** The open path must recognize `/dev/zero`, `/dev/urandom`,
and `/dev/full` alongside the existing `/dev/null` and `/dev/ttyN` handling.

**Acceptance:**
- [ ] `open("/dev/zero")` returns an FD with `DevZero` backend
- [ ] `open("/dev/urandom")` returns an FD with `DevUrandom` backend
- [ ] `open("/dev/full")` returns an FD with `DevFull` backend
- [ ] Existing `/dev/null` and `/dev/ttyN` behavior unchanged
- [ ] Device nodes appear in `sys_linux_fstatat()` for `/dev/*` paths

### E.5 — Wire device read/write into `sys_linux_read()` and `sys_linux_write()`

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `sys_linux_read`, `sys_linux_write`
**Why it matters:** The read and write syscall dispatchers must handle the new
FdBackend variants.

**Acceptance:**
- [ ] `sys_linux_read` match arm for `DevZero` fills buffer with 0x00
- [ ] `sys_linux_read` match arm for `DevUrandom` fills buffer with PRNG bytes
- [ ] `sys_linux_read` match arm for `DevFull` fills buffer with 0x00
- [ ] `sys_linux_write` match arm for `DevZero` returns count (discard)
- [ ] `sys_linux_write` match arm for `DevUrandom` returns count (discard)
- [ ] `sys_linux_write` match arm for `DevFull` returns `NEG_ENOSPC`

---

## Track F — `statfs` / `fstatfs` Syscalls

### F.1 — Define `Statfs` struct layout

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `Statfs`
**Why it matters:** The `statfs` struct has a specific layout that userspace
expects. It includes filesystem type, block size, total/free/available blocks,
total/free inodes, and filesystem ID.

**Acceptance:**
- [ ] `Statfs` struct defined matching Linux x86_64 layout (120 bytes)
- [ ] Fields: f_type, f_bsize, f_blocks, f_bfree, f_bavail, f_files, f_ffree, f_fsid, f_namelen, f_frsize

### F.2 — Implement `sys_statfs()` and `sys_fstatfs()`

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `sys_statfs`, `sys_fstatfs`
**Why it matters:** Build tools and package managers call `statfs()` to check
available disk space before writing.

**Acceptance:**
- [ ] Syscall 137 dispatches to `sys_statfs(path_ptr, buf_ptr)`
- [ ] Syscall 138 dispatches to `sys_fstatfs(fd, buf_ptr)`
- [ ] Returns correct `f_type` for each filesystem (tmpfs, ext2, procfs, ramdisk)
- [ ] `f_bsize` and `f_frsize` set to 4096 (block size)
- [ ] `f_blocks`/`f_bfree`/`f_bavail` reflect actual ext2 usage when available
- [ ] tmpfs and procfs return reasonable defaults (e.g., large f_bavail)
- [ ] Writes the `Statfs` struct to userspace buffer

---

## Track G — Userspace Utilities

### G.1 — Implement `ln` command in coreutils-rs

**Files:**
- `userspace/coreutils-rs/src/ln.rs` (new file)
- `userspace/coreutils-rs/Cargo.toml`
**Symbol:** `ln`
**Why it matters:** `ln` is the standard utility for creating hard and symbolic
links. It exercises the new `link()` and `symlink()` syscalls.

**Acceptance:**
- [ ] `ln <target> <link>` creates a hard link
- [ ] `ln -s <target> <link>` creates a symbolic link
- [ ] Prints error messages on failure (EEXIST, EXDEV, EPERM, etc.)
- [ ] Binary added to initrd via xtask build

### G.2 — Implement `readlink` command in coreutils-rs

**Files:**
- `userspace/coreutils-rs/src/readlink.rs` (new file)
- `userspace/coreutils-rs/Cargo.toml`
**Symbol:** `readlink`
**Why it matters:** `readlink` prints the target of a symbolic link. It is needed
for scripts and debugging symlink chains.

**Acceptance:**
- [ ] `readlink <path>` prints symlink target to stdout
- [ ] Returns exit code 1 if path is not a symlink
- [ ] Binary added to initrd via xtask build

### G.3 — Update `ls` to display symlink targets

**File:** `userspace/coreutils-rs/src/ls.rs`
**Symbol:** `ls`
**Why it matters:** `ls -l` must show `link -> target` for symbolic links and
display the `l` file type character. Without this, symlinks are indistinguishable
from regular files in directory listings.

**Acceptance:**
- [ ] `ls -l` shows `l` type prefix for symlinks
- [ ] `ls -l` shows ` -> <target>` suffix for symlinks
- [ ] Hard link count displayed for all files (from stat `st_nlink`)

### G.4 — Update `stat` to display link information

**File:** `userspace/coreutils-rs/src/stat_cmd.rs`
**Symbol:** `stat_cmd`
**Why it matters:** The `stat` utility must display file type (symlink, regular,
directory), link count, and device numbers for device nodes.

**Acceptance:**
- [ ] `stat` shows "symbolic link" type and target for symlinks
- [ ] `stat` shows link count (nlinks) for all files
- [ ] `stat` shows "character device" type for `/dev/*` nodes

### G.5 — Add initrd entries for new binaries

**File:** `xtask/src/main.rs`
**Symbol:** `build_userspace_bins`
**Why it matters:** New coreutils-rs binaries must be compiled and embedded in the
initrd so they are available at boot.

**Acceptance:**
- [ ] `ln` and `readlink` binaries compiled and placed in `kernel/initrd/`
- [ ] Init process or shell can execute `/bin/ln` and `/bin/readlink`

---

## Track H — Integration Testing and Documentation

### H.1 — Symlink round-trip test

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `sys_symlink`, `sys_readlink`
**Why it matters:** Validates end-to-end symlink creation and resolution.

**Acceptance:**
- [ ] `symlink("/bin/sh0", "/tmp/mysh")` creates a working symlink
- [ ] `readlink("/tmp/mysh")` returns `"/bin/sh0"`
- [ ] Executing `/tmp/mysh` launches `sh0` via symlink resolution
- [ ] `lstat("/tmp/mysh")` returns symlink metadata (not target metadata)
- [ ] `stat("/tmp/mysh")` follows the symlink and returns target metadata

### H.2 — Symlink chain and loop detection test

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `resolve_fs_target`, `Ext2Volume::resolve_path`
**Why it matters:** Multi-hop symlinks and circular symlinks must be handled
correctly to avoid infinite loops in the kernel.

**Acceptance:**
- [ ] Chain A -> B -> C resolves correctly to C's target
- [ ] Circular symlink (A -> B -> A) returns `NEG_ELOOP`
- [ ] Self-referencing symlink returns `NEG_ELOOP`
- [ ] Resolution stops after 40 hops

### H.3 — Hard link test

**File:** `kernel/src/fs/ext2.rs`
**Symbol:** `Ext2Volume::create_hard_link`
**Why it matters:** Validates that hard links share an inode and that unlink
decrements link count correctly.

**Acceptance:**
- [ ] Creating a hard link increases the inode's `links_count` to 2
- [ ] Both paths return identical `stat` results (same inode number)
- [ ] Unlinking one path leaves the file accessible via the other
- [ ] Unlinking the last path frees the inode and data blocks
- [ ] `link()` on a directory returns `NEG_EPERM`

### H.4 — Permission enforcement test

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `check_permission`
**Why it matters:** Validates that non-root users cannot access files they do
not have permission for, and that root bypasses all checks.

**Acceptance:**
- [ ] Non-root user cannot open a 0600-mode file owned by another user
- [ ] Non-root user cannot execute a file without the execute bit
- [ ] Root user can open any file regardless of permissions
- [ ] Non-root user cannot create files in a directory without write permission
- [ ] `umask(077)` causes `mkdir()` to create directories with mode 0700

### H.5 — Procfs test

**File:** `kernel/src/fs/procfs.rs`
**Symbol:** (various)
**Why it matters:** Validates that `/proc` files are readable and return correct
content.

**Acceptance:**
- [ ] `cat /proc/self/status` shows the cat process's own PID and name
- [ ] `readlink /proc/self/exe` returns the running binary path
- [ ] `cat /proc/meminfo` shows MemTotal, MemFree lines with non-zero values
- [ ] `cat /proc/self/maps` shows mapped regions with permission flags
- [ ] `ls /proc/self/fd` lists open descriptors and `readlink /proc/self/fd/1` resolves
- [ ] `cat /proc/stat` shows aggregate CPU counters
- [ ] `cat /proc/uptime` returns a reasonable uptime value
- [ ] `cat /proc/version` returns the kernel version string
- [ ] `ls /proc/` lists PID directories for running processes

### H.6 — Device node test

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `sys_linux_read`, `sys_linux_write`
**Why it matters:** Validates that device nodes behave correctly.

**Acceptance:**
- [ ] `echo test > /dev/null` succeeds silently
- [ ] Reading 4096 bytes from `/dev/zero` returns all zero bytes
- [ ] Reading from `/dev/urandom` returns non-zero data
- [ ] Writing to `/dev/full` returns `ENOSPC`

### H.7 — Regression test suite

**File:** `xtask/src/main.rs`
**Symbol:** `cmd_test`
**Why it matters:** Filesystem and syscall changes are high-risk. Every existing
test must continue to pass.

**Acceptance:**
- [ ] All QEMU integration tests pass
- [ ] All kernel-core host tests pass
- [ ] `cargo xtask check` clean (no warnings)
- [ ] Existing shell, pipe, socket, and filesystem workloads function correctly

### H.8 — Update documentation

**Files:**
- `docs/roadmap/38-filesystem-enhancements.md`
- `docs/roadmap/tasks/38-filesystem-enhancements-tasks.md`
- `docs/roadmap/README.md`
- `docs/roadmap/tasks/README.md`
**Symbol:** n/a
**Why it matters:** Roadmap docs must reflect completion status and any scope
changes discovered during implementation.

**Acceptance:**
- [ ] Phase 38 design doc status updated to "Complete"
- [ ] Task list status updated to "Complete"
- [ ] Roadmap README row updated from "Planned" to "Complete"
- [ ] Task index README row updated from "Planned" to "Complete"
- [ ] Companion Task List link in design doc points to the task file

---

## Documentation Notes

- Phase 38 adds symlink and hard link primitives that were stubbed or missing
  in the ext2 (Phase 28) and tmpfs (Phase 13) implementations.
- The existing `Ext2Inode::is_symlink()` method and `S_IFLNK` / `EXT2_FT_SYMLINK`
  constants are already defined in `kernel-core/src/fs/ext2.rs` but unused — this
  phase activates them.
- `FdBackend::DevNull` already exists from Phase 12. Phase 38 adds `DevZero`,
  `DevUrandom`, and `DevFull` as parallel variants following the same pattern.
- `resolve_path()` in `syscall.rs` remains a lexical normalizer. Symlink
  following belongs in the FS-aware lookup path (`resolve_fs_target()` and the
  filesystem-specific resolution such as `Ext2Volume::resolve_path()`).
- Phase 27 already shipped the core DAC checks for `open()`, `exec()`, `mkdir()`,
  `unlink()`, and `rename()`. Phase 38 completes the remaining pieces that depend
  on new symlink and procfs behavior, and adds `umask`.
- The `Process` struct gains a `umask: u16` field. The `FdBackend` enum gains three
  new variants. Both are in `kernel/src/process/mod.rs`.
- Procfs is a new filesystem backend (`kernel/src/fs/procfs.rs`). It synthesizes
  content on read from `PROCESS_TABLE` and frame allocator state — no persistent
  storage.
