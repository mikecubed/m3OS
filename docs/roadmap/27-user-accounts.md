# Phase 27 - User Accounts and Login

## Milestone Goal

The OS supports multiple user accounts with login authentication, file ownership, and
permission enforcement. A `login` program prompts for username and password before
granting shell access. This is the foundation for multi-user operation and secure remote
access.

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

## Prerequisites

| Phase | Why needed |
|---|---|
| Phase 12 (POSIX Compat) | Syscall ABI for getuid/setuid family |
| Phase 13 (Writable FS) | Writable /etc for passwd/shadow files |
| Phase 14 (Shell and Tools) | Shell to log into |
| Phase 24 (Persistent Storage) | User accounts persist across reboots |

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

Real Unix systems have:
- PAM (Pluggable Authentication Modules) for flexible auth backends
- NSS (Name Service Switch) for looking up users from LDAP, NIS, etc.
- Supplementary groups (a user can belong to multiple groups)
- setuid/setgid binaries for controlled privilege escalation
- Fine-grained capabilities (Linux capabilities, not just root vs. non-root)
- SELinux/AppArmor for mandatory access control

Our implementation uses the classic Unix permission model (user/group/other with
rwx bits), which is simple to understand and sufficient for learning.

## Deferred Until Later

- PAM or pluggable authentication
- Supplementary groups
- ACLs (Access Control Lists)
- Mandatory access control (SELinux-style)
- Home directory creation on first login
- User quotas
- Account lockout after failed attempts
