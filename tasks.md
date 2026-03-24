# Phase 12 — POSIX Compatibility Layer

**Branch:** `phase-12-posix-compat`
**Depends on:** Phase 11 (ELF Loader and Process Model) — complete.

## Track Status

| Track | Scope | Status |
|---|---|---|
| A | Deferred Phase 11 fixes | 🔄 in progress |
| B | Safe user-memory access | 🔄 in progress |
| C | Build infrastructure | 🔄 in progress |
| D | Linux syscall ABI expansion | ⏳ pending |
| E | musl integration | ⏳ pending |
| F | Validation + Documentation | ⏳ pending |

---

## Track A — Deferred Phase 11 Fixes

| Task | Description | Status |
|---|---|---|
| P12-T001 | Two-phase kill path in exception handlers (KILL_PENDING + IRET trampoline) | ⏳ |
| P12-T002 | `free_process_page_table` + call in `sys_execve` after CR3 switch | ⏳ |
| P12-T003 | `TaskState::Dead` + reap dead tasks in scheduler | ⏳ |
| P12-T004 | Add `TRAP_FLAG` to `SFMASK` in syscall init | ⏳ |

## Track B — Safe User-Memory Access

| Task | Description | Status |
|---|---|---|
| P12-T005 | `copy_from_user` / `copy_to_user` in `mm/user_mem.rs` | ⏳ |
| P12-T006 | Replace `sys_debug_print` direct `from_raw_parts` with `copy_from_user` | ⏳ |
| P12-T007 | Replace `path_name_buf` direct `from_raw_parts` with `copy_from_user` | ⏳ |
| P12-T008 | Replace `sys_waitpid` status_ptr write with `copy_to_user` | ⏳ |
| P12-T009 | Replace `setup_abi_stack` virt_to_kptr pattern; return `Result<u64, ElfError>` | ⏳ |

## Track C — Build Infrastructure

| Task | Description | Status |
|---|---|---|
| P12-T010 | Move userspace ELF generation to `build.rs` / xtask; add `initrd/*.elf` to `.gitignore` | ⏳ |

## Track D — Syscall Gate Expansion

| Task | Description | Status |
|---|---|---|
| P12-T011 | Audit ~40 Linux syscall numbers musl requires; list implemented vs. missing | ⏳ |
| P12-T012 | Linux-ABI dispatch table mapping Linux numbers to existing implementations | ⏳ |
| P12-T013 | `read(fd, buf, count)` over VFS IPC path | ⏳ |
| P12-T014 | `write(fd, buf, count)` — stdout/stderr to console_server | ⏳ |
| P12-T015 | `open(path, flags)` / `openat` / `close` over vfs_server | ⏳ |
| P12-T016 | `fstat` / `fstatat` returning minimal `stat` structs | ⏳ |
| P12-T017 | `lseek` | ⏳ |
| P12-T018 | `mmap(NULL, len, PROT_READ\|PROT_WRITE, MAP_PRIVATE\|MAP_ANONYMOUS)` | ⏳ |
| P12-T019 | `munmap` (free frames, unmap pages) | ⏳ |
| P12-T020 | `brk` / `sbrk` backed by frame allocator | ⏳ |
| P12-T021 | `exit` / `exit_group` via Phase 11 path | ⏳ |
| P12-T022 | `getpid` via Phase 11 path | ⏳ |
| P12-T023 | `writev` / `readv` as loops over `write` / `read` | ⏳ |
| P12-T024 | `getcwd` / `chdir` (stub returning `/`) | ⏳ |
| P12-T025 | `ioctl` with TIOCGWINSZ stub | ⏳ |
| P12-T026 | `uname` returning fixed kernel identity | ⏳ |

## Track E — musl Integration

| Task | Description | Status |
|---|---|---|
| P12-T027 | Compile musl on host targeting `x86_64-unknown-none` with custom `__syscall` stubs | ⏳ |
| P12-T028 | Write `crt0.s`: read argc/argv/envp from stack, call `__libc_start_main`, fall through to `exit` | ⏳ |
| P12-T029 | Bundle musl headers and `libc.a` in disk image | ⏳ |

## Track F — Validation + Documentation

| Task | Description | Status |
|---|---|---|
| P12-T030 | Compile "hello world" C binary with musl, copy to image, run inside OS | ⏳ |
| P12-T031 | Verify `printf`, `malloc`, `fopen`, `exit` work in hello world binary | ⏳ |
| P12-T032 | Confirm Phase 11 Rust userspace binaries still work after Linux ABI dispatch | ⏳ |
| P12-T033 | Confirm trap-flag process doesn't generate spurious `#DB` (validates T004) | ⏳ |
| P12-T034 | Document Linux syscall number mapping table and dual-dispatch strategy | ⏳ |
| P12-T035 | Explain musl vs. glibc and why musl is the right first target | ⏳ |
| P12-T036 | Document C runtime entry sequence: `_start` → `__libc_start_main` → `main` → `exit` | ⏳ |
| P12-T037 | Document which syscalls are real vs. stubbed and what gaps mean | ⏳ |
| P12-T038 | Document `copy_from_user` / `copy_to_user` design and why direct casts are unsafe | ⏳ |
