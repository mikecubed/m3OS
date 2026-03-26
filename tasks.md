# Phase 18 — Directory and VFS: Implementation Progress

**Branch:** `phase-18-directory-vfs`
**Status:** In Progress

## Track Layout

| Track | Scope | Dependencies | Status |
|-------|-------|-------------|--------|
| A | Per-process cwd + path resolution | — | Done |
| B | Directory fds + getdents64 (tmpfs) | — | Done |
| C | Ramdisk directory tree | — | Done |
| D | openat + root listing + ramdisk getdents64 | A, B, C | Done |
| E | Shell integration + validation | A, B, C, D | In Progress |

## Track A — Per-Process Working Directory and Path Resolution

- [x] P18-T001: Add `cwd: String` field to `Process` struct; initialize to `"/"` in spawn functions
- [x] P18-T002: Copy `cwd` from parent to child in fork path
- [x] P18-T003: Implement `resolve_path(cwd, path) -> String` with `.`/`..` normalization
- [x] P18-T004: Implement `sys_chdir(path_ptr)` with directory validation
- [x] P18-T005: Implement `sys_getcwd(buf_ptr, size)` returning process cwd
- [x] P18-T006: Update `sys_open` to use `resolve_path` before routing
- [x] P18-T007: Update `sys_mkdir`, `sys_rmdir`, `sys_unlink`, `sys_rename`, `sys_stat` to use `resolve_path`

## Track B — Directory File Descriptors and getdents64 (tmpfs)

- [x] P18-T008: Add `FdBackend::Dir { path: String }` variant
- [x] P18-T009: Define `O_DIRECTORY = 0o200000` constant
- [x] P18-T010: Implement `is_directory(resolved_path)` helper
- [x] P18-T011: Update `sys_open` for O_DIRECTORY and directory opens
- [x] P18-T012: Define `linux_dirent64` layout and DT_REG/DT_DIR constants
- [x] P18-T013: Implement `sys_getdents64` for tmpfs directories
- [x] P18-T014: Handle `sys_read` on directory fd (return EISDIR)
- [x] P18-T015: Handle `sys_close` on directory fd

## Track C — Ramdisk Directory Tree

- [x] P18-T016: Define `RamdiskNode` enum with File and Dir variants
- [x] P18-T017: Restructure FILES into tree with `/bin` and `/etc` directories
- [x] P18-T018: Implement `ramdisk_lookup(path) -> Option<&RamdiskNode>`
- [x] P18-T019: Implement `ramdisk_list_dir(path) -> Option<Vec<(String, bool)>>`
- [x] P18-T020: Update ramdisk `handle_open` to use `ramdisk_lookup`
- [x] P18-T021: Update ELF loader paths for `/bin/` prefix
- [x] P18-T022: Update or remove ramdisk FILE_LIST/name_list() endpoint

## Track D — openat, Root Listing, and Ramdisk getdents64

- [x] P18-T023: Implement `sys_getdents64` for ramdisk directories
- [x] P18-T024: Implement unified root directory listing (ramdisk + tmpfs)
- [x] P18-T025: Handle `sys_open("/")` as directory open
- [x] P18-T026: Update `sys_open` ramdisk routing with `ramdisk_lookup`
- [x] P18-T027: Define `AT_FDCWD` constant
- [x] P18-T028: Implement `sys_openat(dirfd, path_ptr, flags, mode)`
- [x] P18-T029: Ensure backward compatibility (`sys_open` delegates to `sys_openat(AT_FDCWD, ...)`)

## Track E — Shell Integration and Validation

- [ ] P18-T030: Update kernel shell `cd` builtin to validate directory
- [ ] P18-T031: Update `resolve_command` for `/bin/` paths
- [ ] P18-T032: Verify musl `ls.elf` works with getdents64
- [ ] P18-T033: Acceptance: `ls /bin` lists ELF binaries
- [ ] P18-T034: Acceptance: `ls /tmp` lists runtime files
- [ ] P18-T035: Acceptance: `ls /` shows bin, tmp, etc
- [ ] P18-T036: Acceptance: `cd /bin && pwd` prints `/bin`
- [ ] P18-T037: Acceptance: `cd nonexistent` returns error
- [ ] P18-T038: Acceptance: `getcwd()` correct after chdir
- [ ] P18-T039: Acceptance: directory open without O_DIRECTORY works
- [ ] P18-T040: Acceptance: file open with O_DIRECTORY returns ENOTDIR
- [ ] P18-T041: Acceptance: getdents64 resumes across calls
- [ ] P18-T042: Acceptance: relative paths resolve correctly
- [ ] P18-T043: Acceptance: openat resolves relative to dirfd
- [ ] P18-T044: Acceptance: all existing tests pass
- [ ] P18-T045: `cargo xtask check` passes
- [ ] P18-T046: QEMU boot validation — no panics
- [ ] P18-T047: Write documentation
