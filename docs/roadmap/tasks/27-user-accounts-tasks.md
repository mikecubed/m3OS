# Phase 27 — User Accounts and Login: Task List

**Status:** Complete
**Source Ref:** phase-27
**Depends on:** Phase 12 (POSIX Compat) ✅, Phase 24 (Persistent Storage) ✅
**Goal:** The OS supports multiple user accounts with login authentication, file
ownership, and permission enforcement. A `login` program prompts for username and
password before granting shell access.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Process UID/GID and identity syscalls | — | ✅ Done |
| B | VFS file metadata and permission model | A | ✅ Done |
| C | Permission enforcement in VFS operations | B | ✅ Done |
| D | syscall-lib and libc extensions | A | ✅ Done |
| E | Password hashing and /etc files | — | ✅ Done |
| F | Userspace programs (login, id, whoami) | C, D, E | ✅ Done |
| G | Userspace programs (su, passwd, adduser) | F | ✅ Done |
| H | Init integration and boot flow | F | ✅ Done |
| I | Build system and validation | G, H | ✅ Done |

### Implementation Notes

- **UID/GID model**: Classic Unix — real and effective IDs per process. UID 0
  is root and bypasses all permission checks (superuser).
- **Permission model**: Standard Unix rwxrwxrwx (9-bit mode) with user/group/other
  classes. No ACLs or capabilities in this phase.
- **Password storage**: `/etc/shadow` with SHA-256 hashed passwords. Never store
  plaintext. A minimal SHA-256 implementation (standalone C or Rust) avoids
  external dependencies.
- **VFS metadata trait (ext2-ready)**: The `FileMetadata` abstraction in Track B
  is a filesystem-agnostic trait so that Phase 28 (ext2) can return native
  inode metadata directly. The trait covers uid, gid, mode, size, and
  timestamps. Each filesystem backend implements this trait.
- **FAT32 permissions index file**: FAT32 has no native support for Unix
  permissions or ownership. A `.m3os_permissions` index file on the FAT32
  partition maps `path -> uid:gid:mode`. Files default to uid=0, gid=0,
  mode=0o644 (files) or 0o755 (dirs) if not in the index.
- **Login flow**: init spawns `/bin/login` instead of shell. `login` reads
  `/etc/passwd`, prompts for username/password, verifies against `/etc/shadow`,
  then calls `setuid`/`setgid` and `execve` to spawn the user's shell.

## Prerequisite Analysis

Current state (post-Phase 26):
- Process struct (`kernel/src/process/mod.rs`) has pid, ppid, state, pgid, cwd,
  fd_table, signal handling, memory management — but NO uid/gid fields
- `getuid` (102), `getgid` (104), `geteuid` (107), `getegid` (108) syscalls
  exist as stubs returning hardcoded 0 (root)
- `setuid` (105), `setgid` (106) syscalls are NOT implemented
- `chown` (92), `chmod` (15), `fchown` (94), `fchmod` (91) syscalls are NOT
  implemented
- VFS inodes have NO per-file ownership (uid/gid) or permission mode storage
- `sys_linux_fstat` returns hardcoded modes: dirs=0o755, files=0o644, chardevs=0o620
- `sys_linux_open` ignores the mode argument entirely
- No permission checking anywhere in VFS operations
- All processes run as root (uid=0) — single-user system
- Init spawns shell directly via fork+execve
- Persistent FAT32 filesystem on virtio-blk (Phase 24)

---

## Track A — Process UID/GID and Identity Syscalls

Add user/group identity to the process model and implement the POSIX identity
syscalls.

### A.1 — Add UID/GID fields to Process struct

**File:** `kernel/src/process/mod.rs`
**Symbol:** `uid`, `gid`, `euid`, `egid` fields on `Process`
**Why it matters:** Every permission check and identity syscall reads these fields from the current process.

**Acceptance:**
- [x] `uid: u32`, `gid: u32`, `euid: u32`, `egid: u32` fields added to `Process`
- [x] Initialized to 0 (root) in `Process::new()` and `Process::new_kernel()`
- [x] All four fields copied from parent to child in `sys_fork()`

### A.2 — Update identity syscalls to return real fields

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `sys_linux_getuid`, `sys_linux_getgid`, `sys_linux_geteuid`, `sys_linux_getegid`
**Why it matters:** Programs like `id` and `whoami` rely on these syscalls to report the current user.

**Acceptance:**
- [x] `sys_linux_getuid` (102) returns process `uid` field
- [x] `sys_linux_getgid` (104), `sys_linux_geteuid` (107), `sys_linux_getegid` (108) return real fields

### A.3 — Implement `sys_linux_setuid`

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `sys_linux_setuid`
**Why it matters:** The login program needs setuid to drop privileges from root to the authenticated user.

**Acceptance:**
- [x] Syscall 105: if euid==0, set both uid and euid; if non-root, only allow setting euid to real uid
- [x] Returns -EPERM on failure

### A.4 — Implement `sys_linux_setgid`

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `sys_linux_setgid`
**Why it matters:** Companion to setuid -- the login program sets group identity alongside user identity.

**Acceptance:**
- [x] Syscall 106: same logic as setuid but for gid/egid

### A.5 — Implement `sys_linux_setreuid` / `sys_linux_setregid`

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `sys_linux_setreuid`, `sys_linux_setregid`
**Why it matters:** Fine-grained control over real/effective IDs for programs like `su`.

**Acceptance:**
- [x] Syscall 113 (`setreuid`) and 114 (`setregid`) implemented with POSIX semantics

### A.6 — Verify UID/GID inheritance

**Component:** Process fork/exec lifecycle
**Why it matters:** Children must inherit the parent's identity or permission enforcement would be inconsistent.

**Acceptance:**
- [x] After fork, child has same uid/gid/euid/egid as parent
- [x] After execve, uid/gid preserved

---

## Track B — VFS File Metadata and Permission Model

Add ownership and permission metadata to VFS file entries across all filesystem
backends.

### B.1 — Define `FileMetadata` struct and `VfsMetadata` trait

**File:** `kernel/src/arch/x86_64/syscall.rs` (VFS layer)
**Why it matters:** This trait abstracts file metadata across all filesystem backends, enabling ext2 forward-compatibility.

**Acceptance:**
- [x] `FileMetadata` with `uid`, `gid`, `mode`, `size`, `mtime`
- [x] Each filesystem backend (ramdisk, tmpfs, FAT32) implements the trait

### B.2 — Extend ramdisk nodes with metadata

**File:** `kernel/src/fs/ramdisk.rs`
**Why it matters:** Ramdisk files (initrd binaries) need permission bits so exec permission checks work.

**Acceptance:**
- [x] `uid`, `gid`, `mode` fields on ramdisk File/Dir variants
- [x] Defaults: uid=0, gid=0, mode=0o644 (files) / 0o755 (dirs)

### B.3 — Extend tmpfs nodes with metadata

**File:** `kernel/src/fs/tmpfs.rs`
**Why it matters:** Tmpfs files created at runtime need per-file ownership for permission enforcement.

**Acceptance:**
- [x] `uid`, `gid`, `mode` fields on tmpfs file/directory entries

### B.4 — Handle FAT32 metadata via permissions index

**File:** `kernel/src/fs/fat32.rs`
**Why it matters:** FAT32 has no native Unix permission support; the overlay provides persistent permissions across reboots.

**Acceptance:**
- [x] `.m3os_permissions` index file read on mount into `BTreeMap<String, FileMetadata>`
- [x] Chown/chmod update the in-memory map and write back to disk

### B.5 — Update `sys_linux_fstat` with real metadata

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Why it matters:** Programs like `ls -l` and `stat` need correct uid/gid/mode from the filesystem.

**Acceptance:**
- [x] `st_uid`, `st_gid`, `st_mode` populated from file metadata instead of hardcoded values

### B.6 — Update `sys_linux_open` to respect mode argument

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Why it matters:** Files created with O_CREAT should have the requested permission mode, not a hardcoded default.

**Acceptance:**
- [x] Mode argument (arg2) applied to newly created files with `O_CREAT`

### B.7 — Implement `sys_linux_chmod` and `sys_linux_fchmod`

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Why it matters:** Users and administrators need to change file permissions.

**Acceptance:**
- [x] Syscall 90 (`chmod`): verify caller is owner or root, update mode
- [x] Syscall 91 (`fchmod`): same via file descriptor

### B.8 — Implement `sys_linux_chown` and `sys_linux_fchown`

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Why it matters:** Root needs to transfer file ownership when creating user home directories and account files.

**Acceptance:**
- [x] Syscall 92 (`chown`): root-only, update uid/gid
- [x] Syscall 93 (`fchown`): same via file descriptor

---

## Track C — Permission Enforcement in VFS Operations

Add permission checks to all VFS operations that access or modify files.

### C.1 — Implement `check_permission()` helper

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `check_permission`
**Why it matters:** This is the central permission enforcement function used by every file-access syscall.

**Acceptance:**
- [x] `check_permission(metadata, caller_uid, caller_gid, required) -> bool`
- [x] Root (uid==0) always returns true
- [x] Checks user/group/other permission bits against caller identity

### C.2 — Read permission check on open

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Why it matters:** Prevents unauthorized users from reading sensitive files like `/etc/shadow`.

**Acceptance:**
- [x] `O_RDONLY`/`O_RDWR` checked with required=4; returns -EACCES if denied

### C.3 — Write permission check on open

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Why it matters:** Prevents unauthorized modification of system files and other users' data.

**Acceptance:**
- [x] `O_WRONLY`/`O_RDWR`/`O_APPEND` checked with required=2; returns -EACCES if denied

### C.4 — Execute permission check on execve

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Why it matters:** Prevents execution of files not marked as executable.

**Acceptance:**
- [x] ELF binary checked for execute permission (required=1) before loading

### C.5 — Write permission check on directory-modifying operations

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Why it matters:** Prevents unauthorized creation/deletion of files in protected directories like `/bin`.

**Acceptance:**
- [x] `unlink`, `mkdir`, `rmdir`, `rename` verify write permission on parent directory

### C.6 — Search permission check on chdir

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Why it matters:** Directory traversal requires execute (search) permission per POSIX.

**Acceptance:**
- [x] `chdir` verifies execute permission (required=1) on target directory

### C.7 — Root bypass verification

**Component:** All permission checks
**Why it matters:** Root must be able to administer all files regardless of permission bits.

**Acceptance:**
- [x] Root (euid==0) bypasses ALL permission checks

---

## Track D — syscall-lib and libc Extensions

Add userspace wrappers for the new syscalls so C and Rust programs can use them.

### D.1 — Add syscall number constants

**File:** `userspace/syscall-lib/src/lib.rs`
**Why it matters:** Userspace programs need defined constants to invoke the new syscalls.

**Acceptance:**
- [x] `SYS_CHMOD` (90), `SYS_FCHMOD` (91), `SYS_CHOWN` (92), `SYS_FCHOWN` (93), `SYS_SETUID` (105), `SYS_SETGID` (106), `SYS_SETREUID` (113), `SYS_SETREGID` (114)

### D.2 — Add Rust wrapper functions

**File:** `userspace/syscall-lib/src/lib.rs`
**Why it matters:** Safe wrappers prevent ABI misuse and make the syscalls ergonomic for Rust programs.

**Acceptance:**
- [x] `chmod`, `chown`, `setuid`, `setgid`, `getuid`, `getgid`, `geteuid`, `getegid` wrappers

### D.3 — Add C libc stubs

**File:** `userspace/coreutils/libc/` (C libc)
**Why it matters:** C programs (coreutils) need the same syscall access as Rust programs.

**Acceptance:**
- [x] `getuid()`, `getgid()`, `geteuid()`, `getegid()`, `setuid()`, `setgid()`, `chmod()`, `chown()` stubs

### D.4 — Update C stat struct

**File:** `userspace/coreutils/libc/` (C libc)
**Why it matters:** The stat struct must match the kernel's layout for uid/gid fields to be correctly interpreted.

**Acceptance:**
- [x] `struct stat` includes `st_uid` and `st_gid` matching kernel layout

---

## Track E — Password Hashing and Configuration Files

Implement SHA-256 hashing and create the initial user account configuration.

### E.1 — Implement SHA-256

**Component:** Userspace crypto implementation
**Why it matters:** Password verification requires a cryptographic hash function with no external dependencies.

**Acceptance:**
- [x] Minimal SHA-256 function takes byte slice, returns 32-byte hash
- [x] Usable from both C and Rust userspace

### E.2 — Implement password hashing

**Component:** Userspace crypto implementation
**Symbol:** `hash_password`, `verify_password`
**Why it matters:** Passwords must be stored as salted hashes, never plaintext.

**Acceptance:**
- [x] `hash_password(password, salt) -> [u8; 32]` with salt+password concatenation
- [x] Output format: `$sha256$<hex_salt>$<hex_hash>`
- [x] `verify_password` with constant-time comparison

### E.3 — Create initial `/etc/passwd`

**Component:** Disk image build
**Why it matters:** This is the user database that login, id, and whoami read.

**Acceptance:**
- [x] `root:x:0:0:root:/root:/bin/ion` and `user:x:1000:1000:user:/home/user:/bin/ion`

### E.4 — Create initial `/etc/shadow`

**Component:** Disk image build
**Why it matters:** Stores password hashes separately with restricted permissions (mode 0o600).

**Acceptance:**
- [x] Hashed passwords for root and user accounts
- [x] File readable only by root

### E.5 — Create initial `/etc/group`

**Component:** Disk image build
**Why it matters:** Maps group IDs to group names for programs like `id`.

**Acceptance:**
- [x] `root:x:0:root` and `user:x:1000:user`

### E.6 — Add config files to disk image

**File:** `xtask/src/main.rs`
**Why it matters:** The config files must be present on the filesystem at boot time.

**Acceptance:**
- [x] `/etc/` directory and files written to FAT32 image during `cargo xtask image`

---

## Track F — Core Userspace Programs (login, id, whoami)

Build the essential user-facing programs for authentication and identity.

### F.1 — Create `login` program

**File:** `userspace/login/src/main.rs`
**Why it matters:** This is the authentication gateway -- without it, there is no access control on the system.

**Acceptance:**
- [x] Prompts for username/password, verifies against `/etc/shadow`
- [x] On success: `setgid`, `setuid`, set `HOME`/`USER` env vars, `execve(shell)`
- [x] On failure: prints "Login incorrect", loops back

### F.2 — Implement `/etc/passwd` parsing

**File:** `userspace/login/src/main.rs`
**Why it matters:** Login must extract uid, gid, home directory, and shell from the user database.

**Acceptance:**
- [x] Reads line by line, splits on `:`, matches username, extracts fields
- [x] Handles malformed lines gracefully

### F.3 — Implement password verification in login

**File:** `userspace/login/src/main.rs`
**Why it matters:** The authentication check is the security boundary of the login system.

**Acceptance:**
- [x] Opens `/etc/shadow`, finds matching entry, calls `verify_password()`
- [x] Login runs as root (uid 0) to read shadow file

### F.4 — Create `id` utility

**File:** `userspace/coreutils/` (C program)
**Why it matters:** Standard Unix utility for displaying current user and group identity.

**Acceptance:**
- [x] Prints `uid=<uid>(<username>) gid=<gid>(<groupname>)` from getuid/getgid + /etc/passwd lookup

### F.5 — Create `whoami` utility

**File:** `userspace/coreutils/` (C program)
**Why it matters:** Quick way to check the current effective user identity.

**Acceptance:**
- [x] Prints username of current effective user via geteuid + /etc/passwd lookup

---

## Track G — Additional Userspace Programs (su, passwd, adduser)

Build programs for switching users and managing accounts.

### G.1 — Create `su` program

**File:** `userspace/su/src/main.rs`
**Why it matters:** Allows switching between user accounts without logging out and back in.

**Acceptance:**
- [x] `su <username>` prompts for target user's password, verifies, calls setgid/setuid/execve
- [x] On failure: prints "su: Authentication failure"

### G.2 — Create `passwd` program

**File:** `userspace/passwd/src/main.rs`
**Why it matters:** Users need to be able to change their passwords for security.

**Acceptance:**
- [x] Non-root: prompts for current password, then new password twice
- [x] Root: can change any user's password via `passwd <username>`
- [x] Updates `/etc/shadow` with fresh salt and hash

### G.3 — Implement `/etc/shadow` update

**File:** `userspace/passwd/src/main.rs`
**Why it matters:** Password changes must persist to the shadow file on disk.

**Acceptance:**
- [x] Reads file, finds user line, replaces hash field, writes back

### G.4 — Create `adduser` program

**File:** `userspace/adduser/src/main.rs`
**Why it matters:** The system administrator needs to create new user accounts.

**Acceptance:**
- [x] Root-only: assigns next available UID, creates entries in passwd/shadow/group
- [x] Creates home directory at `/home/<username>` owned by new user

### G.5 — Random salt generation

**Component:** passwd/adduser salt generation
**Why it matters:** Unique salts prevent rainbow table attacks against the password database.

**Acceptance:**
- [x] PRNG seeded from TSC (`rdtsc`) or equivalent (proper entropy deferred)

---

## Track H — Init Integration and Boot Flow

Modify the boot sequence so users must log in before accessing the shell.

### H.1 — Init spawns login instead of shell

**File:** `userspace/init/src/main.rs`
**Why it matters:** This is the change that enforces authentication -- the shell is no longer directly accessible.

**Acceptance:**
- [x] Init forks and execs `/bin/login` instead of shell directly

### H.2 — Init respawns login on exit

**File:** `userspace/init/src/main.rs`
**Why it matters:** When a user logs out, the system must present a new login prompt.

**Acceptance:**
- [x] When login/shell child terminates, init forks and execs `/bin/login` again

### H.3 — Set initial file permissions

**Component:** Init boot sequence / disk image
**Why it matters:** `/etc/shadow` must be root-only readable from the start for security.

**Acceptance:**
- [x] `/etc/shadow` is mode 0o600; `/etc/*` is 0o644; `/bin/*` is 0o755

### H.4 — Create home directories

**Component:** Disk image build / init
**Why it matters:** Users need home directories for their working files.

**Acceptance:**
- [x] `/root` exists (root:root); `/home/user` exists (1000:1000)

---

## Track I — Build System, Validation, and Documentation

### I.1 — Add login to xtask build

**File:** `xtask/src/main.rs`
**Why it matters:** The login binary must be compiled and included in the initrd.

**Acceptance:**
- [x] `login` compiled and included in initrd as `/bin/login`

### I.2 — Add su, passwd, adduser to xtask build

**File:** `xtask/src/main.rs`
**Why it matters:** All account management tools must be available in the OS image.

**Acceptance:**
- [x] `su`, `passwd`, `adduser` compiled and included; `id`, `whoami` in coreutils

### I.3 — Add SHA-256 to build

**Component:** Build system
**Why it matters:** The crypto implementation must be linked into programs that verify passwords.

**Acceptance:**
- [x] SHA-256 available to both C and Rust userspace programs

### I.4 — Login prompt acceptance

**Acceptance:**
- [x] Boot reaches `login:` prompt instead of immediate shell

### I.5 — Root login acceptance

**Acceptance:**
- [x] `root`/`root` credentials drop into root shell; `id` shows `uid=0(root) gid=0(root)`

### I.6 — Wrong credentials acceptance

**Acceptance:**
- [x] Wrong credentials print "Login incorrect" and re-prompt

### I.7 — User login acceptance

**Acceptance:**
- [x] `user`/`user` credentials drop into user shell; `id` shows `uid=1000(user) gid=1000(user)`

### I.8 — File ownership acceptance

**Acceptance:**
- [x] Files created by `user` are owned by UID 1000

### I.9 — Shadow file protection acceptance

**Acceptance:**
- [x] `user` cannot read `/etc/shadow` ("Permission denied")

### I.10 — Binary protection acceptance

**Acceptance:**
- [x] `user` cannot delete files owned by root in `/bin/`

### I.11 — su acceptance

**Acceptance:**
- [x] `su root` from user account prompts for root's password and grants root shell

### I.12 — passwd acceptance

**Acceptance:**
- [x] `passwd` changes password; new password works on next login

### I.13 — whoami acceptance

**Acceptance:**
- [x] `whoami` prints correct username

### I.14 — adduser acceptance

**Acceptance:**
- [x] `adduser testuser` (as root) creates account; login as `testuser` works

### I.15 — Lint and format

**Acceptance:**
- [x] `cargo xtask check` passes (clippy + fmt)

### I.16 — QEMU boot validation

**Acceptance:**
- [x] Full login cycle works without panics in both serial and GUI modes

### I.17 — Documentation

**File:** `docs/27-user-accounts.md`
**Why it matters:** Documents the security model and permission enforcement architecture.

**Acceptance:**
- [x] Covers kernel UID/GID model, VFS permission enforcement, password hashing, login flow, FAT32 metadata overlay

---

## Deferred Until Later

These items are explicitly out of scope for Phase 27:

- **ext2 filesystem** — deferred to Phase 28
- PAM (Pluggable Authentication Modules)
- NSS (Name Service Switch) for external user databases
- Supplementary groups (user in multiple groups)
- Setuid/setgid binaries (the setuid bit on executables)
- Linux capabilities (fine-grained privilege splitting)
- SELinux / AppArmor / mandatory access control
- Home directory auto-creation on first login (adduser creates it explicitly)
- User quotas (disk space limits per user)
- Account lockout after failed login attempts
- `/dev/urandom` or proper entropy source (use TSC-based PRNG for salt)
- ACLs (Access Control Lists) beyond rwxrwxrwx
- Password aging and expiration fields in /etc/shadow
- `umask` syscall and per-process file creation mask
- `sudo` (deferred -- su is sufficient for Phase 27)
- `/etc/securetty` or login restrictions
- utmp/wtmp login accounting

---

## Documentation Notes

- Phase 27 transitions the OS from single-user (everything runs as root) to
  multi-user with authentication and permission enforcement.
- The `VfsMetadata` trait introduced here is specifically designed for Phase 28
  (ext2) forward-compatibility -- ext2 can implement it natively without touching
  permission enforcement code.
- The FAT32 `.m3os_permissions` overlay is a transitional mechanism replaced by
  ext2's native inode metadata in Phase 28.
