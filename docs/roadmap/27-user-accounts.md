# Phase 27 — User Accounts and Login

**Status:** Complete
**Source Ref:** phase-27
**Depends on:** Phase 12 (POSIX Compat) ✅, Phase 24 (Persistent Storage) ✅
**Builds on:** Adds UID/GID identity and permission enforcement to the process model from Phase 12; persists user accounts on the FAT32 storage from Phase 24
**Primary Components:** kernel process control block, kernel VFS permission checks, userspace/login/, userspace/su/, userspace/passwd/, userspace/adduser/

## Milestone Goal

The OS supports multiple user accounts with login authentication, file ownership, and
permission enforcement. A `login` program prompts for username and password before
granting shell access. This is the foundation for multi-user operation and secure remote
access.

## Why This Phase Exists

Without user accounts, every process runs with full system privileges and there is
no concept of file ownership or access control. This makes the OS unsuitable for
multi-user scenarios (including remote access via telnet) and prevents any
meaningful security boundary between processes. Adding UID/GID identity and
permission checks brings the OS in line with the fundamental Unix security model
and is a prerequisite for multi-user login, remote shells, and privilege
separation.

## Learning Goals

- Understand how Unix user/group identity works at the kernel level (UID/GID).
- Learn how file permissions are checked on every `open`/`exec`/`mkdir` call.
- See how `setuid`/`setgid` enable privilege transitions.
- Understand password hashing — why plaintext passwords are never stored.

## Feature Scope

### Kernel Changes

- Add `uid` and `gid` fields to the process control block.
- New syscalls: `getuid`, `getgid`, `geteuid`, `getegid`, `setuid`, `setgid`.
- Add owner UID, owner GID, and permission mode (`rwxrwxrwx`) to file inodes.
- Enforce permission checks in VFS `open`, `unlink`, `mkdir`, `rmdir`, `rename`, `exec`.
- UID 0 (root) bypasses permission checks.
- `chown` and `chmod` syscalls.

### Userspace Programs

- **`/bin/login`** — prompts for username and password, validates against `/etc/passwd`
  and `/etc/shadow`, then `exec`s the user's shell with the correct UID/GID.
- **`/bin/su`** — switch user: prompts for password, then spawns a shell as the target user.
- **`/bin/passwd`** — change password for current user (or any user if root).
- **`/bin/id`** — print current UID, GID, and username.
- **`/bin/whoami`** — print current username.
- **`/bin/adduser`** — create a new user account (root only).

### Configuration Files

- `/etc/passwd` — `username:x:uid:gid:gecos:home:shell` (standard Unix format)
- `/etc/shadow` — `username:hash:...` (hashed passwords, root-readable only)
- `/etc/group` — `groupname:x:gid:members`

### Default Accounts

- `root` (UID 0) — full system access, default password `root` (for development)
- `user` (UID 1000) — normal user account, home at `/home/user`

### Password Hashing

Use a simple but real hash: SHA-256 with salt, or bcrypt if a small C implementation
is available. For the initial phase, SHA-256 with a fixed salt prefix is sufficient.
The point is to never store plaintext passwords.

## Important Components and How They Work

### Kernel UID/GID Enforcement

The process control block gains `uid` and `gid` fields, defaulting to 0 (root)
for init. Every VFS operation (`open`, `unlink`, `mkdir`, `rmdir`, `rename`,
`exec`) checks the caller's UID/GID against the file's owner and permission
mode bits. UID 0 bypasses all permission checks.

### Login Flow

Init spawns `login` instead of directly spawning the shell. `login` reads
`/etc/passwd` to look up the user, verifies the password hash from `/etc/shadow`,
then calls `setuid`/`setgid` and `exec`s the user's configured shell.

### Permissions Index File

Because FAT32 has no native Unix permissions, Phase 27 uses a `.m3os_permissions`
index file to persist ownership and modes on the FAT32 partition. This workaround
is replaced by ext2 in Phase 28.

## How This Builds on Earlier Phases

- **Extends Phase 12 (POSIX Compat):** adds getuid/setuid family syscalls to the POSIX syscall ABI
- **Extends Phase 24 (Persistent Storage):** persists /etc/passwd, /etc/shadow, and /etc/group on the FAT32 partition
- **Extends Phase 13 (Writable FS):** requires writable /etc for password and account files
- **Extends Phase 14 (Shell and Tools):** login spawns the shell after successful authentication
- **Replaced by Phase 28:** the FAT32 permissions workaround is eliminated when ext2 provides native inode metadata

## Implementation Outline

1. Add UID/GID fields to the kernel process structure; default to 0 (root) for init.
2. Implement `getuid`, `setuid`, and related syscalls.
3. Add permission mode and owner fields to VFS inodes.
4. Implement permission checking in VFS operations.
5. Implement `chmod` and `chown` syscalls.
6. Write the `login` program: read `/etc/passwd`, verify password hash from `/etc/shadow`.
7. Modify init to spawn `login` instead of directly spawning the shell.
8. Write `su`, `passwd`, `id`, `whoami`, `adduser` utilities.
9. Create default `/etc/passwd`, `/etc/shadow`, `/etc/group` in the disk image.

## Acceptance Criteria

- Boot reaches a `login:` prompt instead of an immediate shell.
- Entering correct credentials for `root` drops into a root shell.
- Entering wrong credentials is rejected with an error message.
- `id` shows the correct UID/GID after login.
- Files created by `user` are owned by UID 1000.
- `user` cannot read `/etc/shadow` (permission denied).
- `user` cannot delete files owned by `root` in protected directories.
- `su root` from `user` prompts for root's password and grants a root shell.
- `passwd` changes the current user's password, and the new password works on next login.

## Companion Task List

- [Phase 27 Task List](./tasks/27-user-accounts-tasks.md)

## How Real OS Implementations Differ

- Real Unix systems use PAM (Pluggable Authentication Modules) for flexible auth backends.
- NSS (Name Service Switch) enables looking up users from LDAP, NIS, etc.
- Supplementary groups allow a user to belong to multiple groups.
- setuid/setgid binaries provide controlled privilege escalation.
- Fine-grained capabilities (Linux capabilities) replace the binary root vs. non-root model.
- SELinux/AppArmor provide mandatory access control beyond discretionary Unix permissions.
- Our implementation uses the classic Unix permission model (user/group/other with
  rwx bits), which is simple to understand and sufficient for learning.

## Deferred Until Later

- ext2 filesystem (Phase 28)
- PAM or pluggable authentication
- Supplementary groups
- ACLs (Access Control Lists)
- Mandatory access control (SELinux-style)
- Home directory creation on first login
- User quotas
- Account lockout after failed attempts
