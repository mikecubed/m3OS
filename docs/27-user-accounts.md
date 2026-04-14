# Phase 27 — User Accounts and Login

**Aligned Roadmap Phase:** Phase 27
**Status:** Complete
**Source Ref:** phase-27

## Overview

Phase 27 adds multi-user support to m3OS: per-process UID/GID identity,
file ownership and Unix permission enforcement across all filesystem
backends, SHA-256 password hashing, and a login-based boot flow. Six new
userspace programs (`login`, `su`, `passwd`, `adduser`, `id`, `whoami`)
provide authentication and account management.

After Phase 27, the system boots to a `login:` prompt instead of
dropping directly into a shell. Two default accounts exist: `root`
(UID 0) and `user` (UID 1000).

## Kernel UID/GID Model

### Process Identity Fields

The `Process` struct (`kernel/src/process/mod.rs`) carries four
credential fields:

```rust
pub uid: u32,   // Real user ID
pub gid: u32,   // Real group ID
pub euid: u32,  // Effective user ID (used for permission checks)
pub egid: u32,  // Effective group ID
```

All process constructors initialize these to 0 (root). On `fork()`,
the kernel copies all four fields from parent to child after the new
process entry is created. On `execve()`, credentials are preserved for
normal binaries; `/bin/su` is the one temporary privileged helper that
execs with a root effective identity until generic setuid-on-exec exists.

### Identity Syscalls

| Syscall | Number | Behavior |
|---|---|---|
| `getuid` | 102 | Returns `process.uid` |
| `getgid` | 104 | Returns `process.gid` |
| `geteuid` | 107 | Returns `process.euid` |
| `getegid` | 108 | Returns `process.egid` |
| `setuid(uid)` | 105 | If `euid == 0`: sets both `uid` and `euid`. Otherwise: only allows setting `euid` back to the real `uid`. Returns `-EPERM` on violation. |
| `setgid(gid)` | 106 | Mirror of `setuid` for `gid`/`egid`. |
| `setreuid(ruid, euid)` | 113 | Fine-grained real/effective UID control. `-1` means no change. |
| `setregid(rgid, egid)` | 114 | Mirror of `setreuid` for GID. |

The helper `current_process_ids()` in `syscall.rs` returns
`(uid, gid, euid, egid)` and is used throughout the syscall layer.

## VFS Permission Enforcement

### Permission Check Function

All permission decisions go through a single function in
`kernel/src/arch/x86_64/syscall.rs`:

```rust
fn check_permission(file_uid: u32, file_gid: u32, file_mode: u16,
                    caller_uid: u32, caller_gid: u32, required: u8) -> bool
```

- `required` is a bitmask: `4` = read, `2` = write, `1` = execute/search
- UID 0 (root) always returns `true`
- Otherwise: matches caller UID against file owner, then GID against
  file group, then falls through to "other" bits

### Metadata Resolution

`path_metadata(abs_path)` dispatches to the correct filesystem backend:

| Path prefix | Backend | Metadata source |
|---|---|---|
| `/tmp/...` | tmpfs | `uid`, `gid`, `mode` fields on node structs |
| `/bin/`, `/sbin/` (initrd) | ramdisk | Hardcoded `(0, 0, 0o755)` |
| `/` (ext2 mount) | ext2 | Native inode `uid`, `gid`, `mode` |
| `/data/...` | ext2 or FAT32 | ext2 inode or FAT32 permissions overlay |

### Operations Protected

| Operation | Permission required |
|---|---|
| `open()` (read) | Read (4) on file |
| `open()` (write) | Write (2) on file |
| `open()` (O_CREAT, new file) | Write+execute (3) on parent directory |
| `execve()` | Execute (1) on file |
| `chdir()` | Execute/search (1) on directory |
| `mkdir()` | Write+execute (3) on parent directory |
| `rmdir()` | Write+execute (3) on parent directory |
| `unlink()` | Write+execute (3) on parent directory |
| `rename()` | Write+execute (3) on both source and dest parent dirs |

## File Metadata by Filesystem

### Tmpfs

`FileData` and `DirData` in `kernel-core/src/fs/tmpfs.rs` each hold
`uid: u32`, `gid: u32`, `mode: u16` directly. New files inherit the
creating process's UID/GID. `chmod()` and `chown()` methods update
these fields in place.

### Ramdisk

The initrd ramdisk has no mutable metadata. All ramdisk files report
`uid=0, gid=0, mode=0o755` via hardcoded values in `path_metadata()`.
The ramdisk is effectively read-only and root-owned.

### FAT32 Permissions Overlay

FAT32 has no native Unix metadata. An in-memory overlay backed by a
`.m3os_permissions` index file on the FAT32 partition provides
persistence:

```rust
pub static FAT32_PERMISSIONS: Mutex<BTreeMap<String, Fat32FileMeta>>
```

- **Format**: one line per file, `path:uid:gid:mode_octal\n`
- **Load**: `load_permissions_index()` reads at mount time
- **Save**: `save_permissions_index()` writes back on chmod/chown
- **Default**: files not in the index report `(0, 0, 0o755)`

This overlay is a transitional solution — ext2 (Phase 28) stores
metadata natively in inodes and replaces FAT32 as the primary
persistent filesystem.

### ext2

ext2 inodes store `uid`, `gid`, and `mode` natively. The kernel's
ext2 driver (`kernel/src/fs/ext2.rs`) reads and writes these fields
directly via `metadata(path)` and `set_metadata(path, uid, gid, mode)`.
On file/directory creation, the caller's UID/GID are written into the
new inode.

## chmod/chown Syscalls

| Syscall | Number | Authorization |
|---|---|---|
| `chmod(path, mode)` | 90 | Owner or root |
| `fchmod(fd, mode)` | 91 | Owner or root |
| `chown(path, uid, gid)` | 92 | Root only |
| `fchown(fd, uid, gid)` | 93 | Root only |

These dispatch through `resolve_fs_target()` to determine the backing
filesystem (tmpfs, ext2, FAT32, or ramdisk). Ramdisk returns `EROFS`.
For ext2, metadata is written directly to the inode. For FAT32, the
in-memory overlay is updated and the index file is saved.

## Password Hashing

### SHA-256 Implementation

A pure no_std SHA-256 implementation lives in
`userspace/syscall-lib/src/sha256.rs` (FIPS 180-4, no external
dependencies). All Rust userspace programs link against it via
`syscall-lib`.

### Key Functions

- `sha256(data: &[u8]) -> [u8; 32]` — raw SHA-256 hash
- `hash_password(password: &[u8], salt: &[u8]) -> [u8; 32]` — computes
  `SHA-256(salt || password)`
- `verify_password(password: &[u8], shadow_entry: &[u8]) -> bool` —
  parses the shadow format, recomputes, and does constant-time comparison

### Shadow Format

```
$sha256$<hex_salt>$<hex_hash>
```

Salt is the username bytes hex-encoded (e.g., `root` becomes
`726f6f74`). No key stretching (PBKDF2/bcrypt) — this is a learning
OS, not a production system.

## Configuration Files

Three files on the ext2 partition at `/etc/` (ext2 is mounted at `/`):

| File | Mode | Contents |
|---|---|---|
| `/etc/passwd` | 0644 | `username:x:uid:gid:gecos:home:shell` |
| `/etc/shadow` | 0600 | `username:$sha256$salt$hash::::::` |
| `/etc/group` | 0644 | `groupname:x:gid:members` |

### Default Accounts

| Username | UID | GID | Password | Shell |
|---|---|---|---|---|
| `root` | 0 | 0 | `root` | `/bin/ion` |
| `user` | 1000 | 1000 | `user` | `/bin/ion` |

These are baked into the disk image at build time by
`populate_ext2_files()` in `xtask/src/main.rs`, which uses `debugfs`
to write the files and set inode uid/gid/mode.

## Login Flow

```
init (PID 1)
  └─ fork + execve("/bin/login")
       ├─ print "m3OS login: ", read username
       ├─ print "Password: ", disable echo, read password, restore echo
       ├─ parse /etc/passwd → (uid, gid, home, shell)
       ├─ read /etc/shadow → verify_password()
       ├─ on failure: print "Login incorrect", loop
       └─ on success:
            ├─ create home directory (while still root)
            ├─ setgid(gid), setuid(uid)
            ├─ set PATH, HOME, TERM, EDITOR, USER env vars
            └─ execve(shell)  [fallback: /bin/sh0]
```

When the shell exits, `waitpid()` in init detects the child
termination and respawns `/bin/login` for a new session.

### Init Integration

Init (`userspace/init/src/main.rs`) was modified to:

1. Mount ext2 at `/` via `mount("/dev/blk0", "/", "ext2")`
2. Call `chmod("/tmp", 0o1777)` for world-writable tmp
3. Fork and exec `/bin/login` instead of spawning a shell directly
4. Reap children in a loop; respawn login when the session ends

## Userspace Programs

### login (`userspace/login/`)

Rust no_std binary. Implements the full authentication cycle: prompt,
password input with echo disabled (via termios), passwd/shadow parsing,
credential verification, privilege drop via `setgid`/`setuid`, and
shell exec.

### su (`userspace/su/`)

Rust no_std binary. `su <username>` (defaults to root). Any user can
run `su`; non-root callers are prompted for the target user's password.
Root skips the password prompt. Verifies against shadow, then switches
to the target identity and execs their shell. The kernel currently
launches `/bin/su` as a temporary privileged helper so it can read
`/etc/shadow` and perform the authenticated transition before generic
setuid-on-exec support exists.

### passwd (`userspace/passwd/`)

Rust no_std binary. Root-only. Defaults to the current username via
`/etc/passwd` UID lookup, but root may also run `passwd <username>` to
update another account. It prompts for a new password twice, hashes
with SHA-256, and rewrites `/etc/shadow` with the updated entry.

### adduser (`userspace/adduser/`)

Rust no_std binary. Root-only. Scans `/etc/passwd` for the maximum
UID, assigns `new_uid = max_uid + 1`. Validates the username (max 32
chars, no `:`, `\n`, or `\0`). Appends entries to passwd, shadow, and
group files. Creates the home directory and sets ownership.

### id (`userspace/id/`)

Rust no_std binary. Calls `getuid()`/`getgid()`, looks up names from
`/etc/passwd` and `/etc/group`, prints `uid=N(name) gid=N(name)`.

### whoami (`userspace/whoami/`)

Rust no_std binary. Calls `geteuid()`, looks up the username in
`/etc/passwd`, prints it.

## syscall-lib Extensions

Phase 27 added to `userspace/syscall-lib/`:

- `sha256` module — SHA-256, password hashing, password verification
- Syscall wrappers: `chmod`, `chown`, `setuid`, `setgid`, `getuid`,
  `getgid`, `geteuid`, `getegid`
- Syscall constants: `SYS_CHMOD` (90), `SYS_FCHMOD` (91),
  `SYS_CHOWN` (92), `SYS_FCHOWN` (93), `SYS_SETUID` (105),
  `SYS_SETGID` (106), `SYS_SETREUID` (113), `SYS_SETREGID` (114)

C libc stubs were added to the coreutils libc for `getuid`, `getgid`,
`geteuid`, `getegid`, `setuid`, `setgid`, `chmod`, and `chown`.

## Known Limitations

- **No generic setuid-bit on executables** — setuid-on-exec is still
  deferred. `/bin/su` is the current narrow exception so password-based
  user switching can authenticate against `/etc/shadow`. Phase 48 added
  kernel enforcement for `setuid`/`setgid` syscalls (root-only
  escalation), replacing the previous unconditional behavior.
- **No sticky bit enforcement** — `/tmp` is mode `0o1777` but
  `check_permission` does not prevent users from deleting each other's
  files in sticky directories.
- **No supplementary groups** — only a single GID per process.
- **No umask** — file creation mode is not masked.
- **Iterated SHA-256 password hashing** — Phase 48 replaced
  single-iteration SHA-256 with 10,000-round iterated hashing and
  cryptographically random salts. The legacy `$sha256$` format is still
  verified for migration. A proper KDF (Argon2id) is deferred.

## Foundation for Future Phases

Phase 27's permission model enables:

- **Phase 28 (ext2)** — the `VfsMetadata` trait ensures ext2 can
  implement metadata natively without changing permission enforcement
- **Phase 30 (Telnet)** — remote login uses the same login/passwd
  infrastructure
- **Phase 35 (SSH)** — secure remote access with per-user authentication
