# Phase 18: Directory VFS

**Aligned Roadmap Phase:** Phase 18
**Status:** Complete
**Source Ref:** phase-18

This document describes the directory and VFS features added in Phase 18:
per-process working directories, path resolution, the ramdisk directory
tree, directory file descriptors, `getdents64`, and `openat`.

## Per-Process Working Directory

Each process carries a `cwd: String` field in its `Process` struct
(`kernel/src/process/mod.rs`). It holds an absolute path and defaults
to `"/"`.

```rust
pub struct Process {
    // ...
    /// Current working directory (Phase 18). Defaults to "/".
    pub cwd: String,
}
```

**Initialization.** Every process spawned via `spawn_process()` or
`spawn_process_with_cr3()` starts with `cwd: String::from("/")`.

**Inheritance on fork.** The fork syscall clones the parent's `cwd`
into the child:

```rust
let parent_cwd = p.cwd.clone();    // read under PROCESS_TABLE lock
// ... after child is created:
child.cwd = parent_cwd;
```

**Retrieval.** The helper `current_cwd()` reads the running process's
`cwd` from the process table under the `PROCESS_TABLE` lock:

```rust
fn current_cwd() -> String {
    let pid = CURRENT_PID.load(Ordering::Relaxed);
    let table = PROCESS_TABLE.lock();
    match table.find(pid) {
        Some(p) => p.cwd.clone(),
        None => String::from("/"),
    }
}
```

## Path Resolution

All path-accepting syscalls (`open`, `openat`, `chdir`, `mkdir`,
`unlink`, `rename`, `stat`) call `resolve_path(cwd, path)` before
performing any filesystem operation.

```rust
fn resolve_path(cwd: &str, path: &str) -> String {
    let combined = if path.starts_with('/') {
        String::from(path)
    } else if path.is_empty() || path == "." {
        String::from(cwd)
    } else {
        format!("{}/{}", cwd.trim_end_matches('/'), path)
    };

    let mut parts: Vec<&str> = Vec::new();
    for component in combined.split('/') {
        match component {
            "" | "." => {}
            ".." => { parts.pop(); }
            other => parts.push(other),
        }
    }

    if parts.is_empty() {
        String::from("/")
    } else {
        let mut result = String::new();
        for part in &parts {
            result.push('/');
            result.push_str(part);
        }
        result
    }
}
```

The algorithm:

1. **Absolute vs. relative.** If `path` starts with `/`, use it
   directly. If it is empty or `"."`, use `cwd`. Otherwise prepend
   `cwd` with a `/` separator.
2. **Normalize.** Split on `/`. Skip empty segments and `"."`. On
   `".."`, pop the last component (at root, `..` is a no-op since
   `pop()` on an empty vec does nothing).
3. **Result.** Rejoin with `/` prefixes. An empty parts list yields
   `"/"`.

This produces a clean absolute path with no trailing slash (except for
root), no double slashes, and no `.`/`..` components.

## chdir and getcwd

### chdir (syscall 80)

`sys_linux_chdir(path_ptr)`:

1. Read the path string from userspace via `read_user_cstr`.
2. Call `resolve_path(current_cwd(), path)` to get an absolute path.
3. Call `is_directory(resolved)` to verify the target exists and is a
   directory. This helper checks `/` itself, then tmpfs via `stat`,
   then the ramdisk via `ramdisk_lookup`.
4. If the target is not a directory, return `ENOTDIR` if the path
   exists as a file, or `ENOENT` if it does not exist at all.
5. On success, write the resolved path into `process.cwd` under the
   `PROCESS_TABLE` lock.

### getcwd (syscall 79)

`sys_linux_getcwd(buf_ptr, size)`:

1. Read the current process's `cwd` via `current_cwd()`.
2. If the userspace buffer is too small for the path plus its null
   terminator, return `ERANGE`.
3. Copy the path bytes to userspace, then write a single `0x00` null
   terminator.
4. Return the total length (path + null), matching Linux semantics.

## Ramdisk Tree Structure

Before Phase 18 the ramdisk was a flat array of `(name, content)`
pairs. Phase 18 replaced it with a static directory tree defined in
`kernel/src/fs/ramdisk.rs`.

### Node type

```rust
pub enum RamdiskNode {
    File { content: &'static [u8] },
    Dir { children: &'static [(&'static str, RamdiskNode)] },
}
```

### Static tree layout

Rust const-eval cannot handle recursive types in a single `const`
expression, so the tree is built from separate `static` items:

```rust
static BIN_ENTRIES: &[(&str, RamdiskNode)] = &[
    ("cat.elf", RamdiskNode::File { content: CAT_ELF }),
    ("ls.elf",  RamdiskNode::File { content: LS_ELF }),
    // ...
];
static ETC_ENTRIES: &[(&str, RamdiskNode)] = &[
    ("hello.txt",  RamdiskNode::File { content: HELLO_TXT }),
    ("readme.txt", RamdiskNode::File { content: README_TXT }),
];
static SBIN_ENTRIES: &[(&str, RamdiskNode)] = &[
    ("init",     RamdiskNode::File { content: INIT_ELF }),
    ("init.elf", RamdiskNode::File { content: INIT_ELF }),
];
static ROOT_ENTRIES: &[(&str, RamdiskNode)] = &[
    ("bin",  RamdiskNode::Dir { children: BIN_ENTRIES }),
    ("sbin", RamdiskNode::Dir { children: SBIN_ENTRIES }),
    ("etc",  RamdiskNode::Dir { children: ETC_ENTRIES }),
];
static RAMDISK_ROOT: RamdiskNode = RamdiskNode::Dir { children: ROOT_ENTRIES };
```

File payloads are `include_bytes!` statics referenced by both the tree
and the legacy flat file table (which is retained for IPC backward
compatibility). No file content is duplicated.

### Tree navigation

`ramdisk_lookup(path)` walks the tree by splitting the path into
components and matching each against the current directory's children.
Leading slashes are stripped; empty and `"."` components are skipped.

`ramdisk_list_dir(path)` looks up a directory node and returns its
children as `Vec<(String, bool)>` pairs (name, is_dir).

`get_file(name)` provides backward compatibility: it tries the path
directly, then falls back to searching `/bin/` and `/etc/` for bare
filenames without directory components.

## Directory File Descriptors

The `FdBackend` enum includes a `Dir` variant:

```rust
pub enum FdBackend {
    // ...
    Dir { path: String },
}
```

A directory fd stores the absolute path of the directory it represents.
`read()` on a directory fd returns `EISDIR`. `close()` requires no
special cleanup beyond removing the fd slot.

Opening a directory happens in two cases:

- `O_DIRECTORY` flag is set: the target must be a directory or the
  syscall returns `ENOTDIR`.
- Target path is a directory regardless of flags: an `FdBackend::Dir`
  fd is returned (instead of the pre-Phase-18 `EISDIR` error).

`O_DIRECTORY` is defined as `0o200000` (octal), matching the Linux
value.

## getdents64

Syscall 217 (`getdents64`) reads directory entries from a directory
fd into a userspace buffer.

### linux_dirent64 layout

Each entry is serialized as:

```
Offset  Size   Field
0       8      d_ino     (u64)  — synthesized inode number (entry index + 1)
8       8      d_off     (i64)  — offset to next entry (entry index + 1)
16      2      d_reclen  (u16)  — total size of this entry, 8-byte aligned
18      1      d_type    (u8)   — DT_DIR (4) or DT_REG (8)
19      var    d_name    (char[]) — null-terminated filename
```

`d_reclen` is computed as `(19 + name_len + 1 + 7) & !7` to ensure
8-byte alignment. The region between the null terminator and the next
record boundary is zero-padded.

### Serialization algorithm

1. Look up the fd; verify it is `FdBackend::Dir`. Extract `dir_path`
   and the current `offset` (entry index).
2. Build the full entry list: synthetic `"."` and `".."` at positions
   0 and 1, followed by the directory's actual children.
3. For the root directory (`/`), merge ramdisk top-level children
   (`bin`, `sbin`, `etc`) with a synthetic `tmp` entry for the tmpfs
   mount point. For `/tmp/*` paths, query tmpfs. For other paths,
   query the ramdisk.
4. Starting from `offset`, serialize entries into a kernel-side buffer
   until the buffer would exceed `count` bytes. If even one entry
   does not fit, return `EINVAL`.
5. Copy the buffer to userspace via `copy_to_user`.
6. Update the fd's `offset` to the index of the next unread entry so
   subsequent calls resume correctly.
7. Return the number of bytes written. Return 0 when all entries have
   been consumed (end of directory).

## openat and AT_FDCWD

Syscall 257 (`openat`) accepts a directory file descriptor as the base
for relative path resolution.

```rust
const AT_FDCWD: u64 = (-100_i64) as u64;
```

### Semantics

- **`dirfd == AT_FDCWD`**: resolve the path relative to the process's
  `cwd`, identical to plain `open()`.
- **`dirfd` is a valid directory fd**: read the directory's path from
  `FdBackend::Dir { path }` and call `resolve_path(&base_path, rel)`
  to produce an absolute path. Then proceed with the same open logic
  (flag decoding, `O_DIRECTORY` checks, tmpfs/ramdisk routing).
- **`dirfd` is invalid or not a directory fd**: return `EBADF` or
  `ENOTDIR` respectively.

`sys_linux_open` (syscall 2) delegates to the same code path as
`sys_linux_openat(AT_FDCWD, ...)`, so musl libc's `open()` (which
emits `openat` on modern Linux) works transparently.

## Filesystem Routing

Path-based syscalls route to one of two backends after resolution:

| Path prefix | Backend | Notes |
|-------------|---------|-------|
| `/tmp/...`  | tmpfs   | Mutable in-memory filesystem |
| everything else | ramdisk | Read-only static tree |

The root directory `/` is a special case: `getdents64` on `/` merges
children from both the ramdisk root (`bin`, `sbin`, `etc`) and a
synthetic `tmp` entry for the tmpfs mount point.

The `is_directory()` helper checks all backends in order: root `/`
itself, tmpfs via `stat`, then ramdisk via `ramdisk_lookup`. This
function is used by `chdir`, `open` with `O_DIRECTORY`, and `openat`.

## Limitations

These features are deferred to future phases:

- Full VFS abstraction with pluggable filesystem drivers
- Mount points and `mount`/`umount` syscalls
- Symbolic links and `readlink`
- Hard links and real inode numbers
- Permission bits and `chmod`/`chown`
- `..` across mount boundaries
- `fstat`/`fstatat` for directory fds
