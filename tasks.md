# Phase 13 — Writable Filesystem

**Branch:** `phase-13-writable-fs`
**Depends on:** Phase 12 (POSIX Compatibility Layer) — complete.
**Status:** 🚧 In Progress

## Track Status

| Track | Scope | Status |
|---|---|---|
| A | tmpfs core data structure | 🚧 in progress |
| B | FD table + write syscalls | ⏳ blocked on A |
| C | VFS mount table + protocol | ⏳ blocked on A |
| D | Validation (userspace test + QEMU) | ⏳ blocked on B, C |
| E | Documentation + tasks.md | ⏳ blocked on D |

---

## Track A — tmpfs Core Data Structure

| Task | Description | Status |
|---|---|---|
| P13-T001 | `kernel/src/fs/tmpfs.rs` — TmpfsNode enum (File/Dir), tree structure | ⬜ |
| P13-T002 | create_file / write_file / read_file operations | ⬜ |
| P13-T003 | mkdir / rmdir / list_dir operations | ⬜ |
| P13-T004 | stat / rename / truncate operations | ⬜ |
| P13-T005 | Global TMPFS instance with spin::Mutex | ⬜ |

## Track B — FD Table + Write Syscalls

| Task | Description | Status |
|---|---|---|
| P13-T006 | Refactor FdEntry to support ramdisk (read-only) and tmpfs (path-based r/w) | ⬜ |
| P13-T007 | sys_linux_write to file FDs (route through tmpfs for /tmp paths) | ⬜ |
| P13-T008 | sys_linux_open with O_CREAT + O_WRONLY/O_RDWR support for tmpfs | ⬜ |
| P13-T009 | sys_linux_read for tmpfs-backed FDs | ⬜ |
| P13-T010 | sys_linux_mkdir (syscall 83) | ⬜ |
| P13-T011 | sys_linux_unlink (syscall 87) | ⬜ |
| P13-T012 | sys_linux_rmdir (syscall 84) | ⬜ |
| P13-T013 | sys_linux_rename (syscall 82) | ⬜ |
| P13-T014 | sys_linux_truncate (syscall 76) / ftruncate (syscall 77) | ⬜ |
| P13-T015 | sys_linux_fsync (syscall 74) — no-op for tmpfs | ⬜ |
| P13-T016 | Update Linux syscall dispatch table with new entries | ⬜ |

## Track C — VFS Mount Table + Protocol

| Task | Description | Status |
|---|---|---|
| P13-T017 | Mount table in vfs.rs: path prefix → backend routing | ⬜ |
| P13-T018 | New IPC labels: FILE_WRITE, FILE_CREATE, FILE_MKDIR, FILE_UNLINK, FILE_RMDIR | ⬜ |
| P13-T019 | tmpfs IPC server task (tmpfs_server_task) registered as "tmpfs" | ⬜ |
| P13-T020 | vfs_server_task dispatches /tmp to tmpfs, / to ramdisk | ⬜ |

## Track D — Validation

| Task | Description | Status |
|---|---|---|
| P13-T021 | Userspace C test: create + write + close + reopen + read in /tmp | ⬜ |
| P13-T022 | Userspace C test: mkdir + rmdir + unlink in /tmp | ⬜ |
| P13-T023 | QEMU boot validation — all tests pass, no panics | ⬜ |
| P13-T024 | cargo xtask check passes (clippy + fmt) | ⬜ |

## Track E — Documentation

| Task | Description | Status |
|---|---|---|
| P13-T025 | docs/13-writable-filesystem.md — tmpfs design, VFS routing, syscall additions | ⬜ |
| P13-T026 | docs/roadmap/tasks/13-writable-fs-tasks.md | ⬜ |
