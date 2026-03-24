# Phase 14 — Shell and Userspace Tools

**Branch:** `phase-14-shell-and-tools`
**Depends on:** Phase 12 (POSIX Compat) ✅, Phase 13 (Writable FS) ✅
**Status:** In Progress

## Track Status

| Track | Scope | Status |
|---|---|---|
| A | Per-process FD table | in progress |
| B | Pipe syscall + kernel pipe buffer | pending (blocked on A) |
| C | dup2 syscall | pending (blocked on A) |
| D | Argv/envp in execve | pending |
| E | Stdin integration (keyboard → FD 0) | pending (blocked on A, B) |
| F | Signal infrastructure | pending |
| G | Process groups + job control | pending (blocked on F) |
| H | Shell rewrite (fork+exec, pipes, redirection) | pending (blocked on A–G) |
| I | Core utilities (standalone ELF binaries) | pending (blocked on D) |
| J | Validation + documentation | pending (blocked on H, I) |

---

## Track A — Per-Process FD Table

| Task | Description | Status |
|---|---|---|
| P14-T001 | Add `fd_table: [Option<FdEntry>; MAX_FDS]` field to `Process` struct | |
| P14-T002 | Initialize FDs 0/1/2 (stdin/stdout/stderr) when creating a new process | |
| P14-T003 | Modify `sys_fork` to deep-clone parent's `fd_table` into child | |
| P14-T004 | Modify all FD syscalls to index into calling process's `fd_table` | |
| P14-T005 | Remove the global `FD_TABLE` static | |
| P14-T006 | Verify existing Phase 11–13 tests still pass after migration | |

## Track B — Pipe Syscall

| Task | Description | Status |
|---|---|---|
| P14-T007 | Define `Pipe` struct: ring buffer (4 KiB), read/write offsets, reader/writer-open flags | |
| P14-T008 | Add `FdBackend::PipeRead { pipe_id }` and `FdBackend::PipeWrite { pipe_id }` variants | |
| P14-T009 | Implement `sys_pipe(pipefd_ptr)`: allocate Pipe, allocate two FD slots | |
| P14-T010 | Implement pipe-aware `read()`: block if empty + writer open; EOF if writer closed | |
| P14-T011 | Implement pipe-aware `write()`: block if full + reader open; EPIPE if reader closed | |
| P14-T012 | Implement pipe-aware `close()`: mark reader/writer as closed; free when both closed | |
| P14-T013 | Add syscall 22 to dispatch table | |

## Track C — dup2 Syscall

| Task | Description | Status |
|---|---|---|
| P14-T014 | Implement `sys_dup2(oldfd, newfd)`: close newfd if open, copy FdEntry | |
| P14-T015 | Handle edge case: `dup2(fd, fd)` returns fd without closing | |
| P14-T016 | Add syscall 33 to dispatch table | |

## Track D — Argv/Envp in Execve

| Task | Description | Status |
|---|---|---|
| P14-T017 | Parse argv pointer array from user memory (null-terminated char* array) | |
| P14-T018 | Parse envp pointer array from user memory (same format) | |
| P14-T019 | Copy argv/envp strings into kernel buffers via `copy_from_user` | |
| P14-T020 | Pass argv/envp to `setup_abi_stack` instead of hardcoded `&[name]` / empty | |
| P14-T021 | Verify echo-args.elf receives correct arguments when launched with argv | |

## Track E — Stdin Integration

| Task | Description | Status |
|---|---|---|
| P14-T022 | Add a kernel-level stdin ring buffer: kbd_server writes chars into it | |
| P14-T023 | Wire FD 0 in new processes to the stdin buffer | |
| P14-T024 | Implement line-buffered mode: accumulate until Enter, then make available | |
| P14-T025 | Echo typed characters to stdout (console) as they arrive | |
| P14-T026 | Handle Backspace in the line buffer | |

## Track F — Signal Infrastructure

| Task | Description | Status |
|---|---|---|
| P14-T027 | Add `pending_signals: u64` bitfield to `Process` | |
| P14-T028 | Add `signal_action: [SignalAction; 32]` table to `Process` | |
| P14-T029 | Implement `sys_kill(pid, sig)` (syscall 62) | |
| P14-T030 | Implement `sys_rt_sigaction(sig, act, oldact)` (syscall 13) | |
| P14-T031 | Check pending signals on return to userspace; deliver default actions | |
| P14-T032 | Implement SIGCONT: resume a stopped process | |
| P14-T033 | Add syscalls 62, 13, 14 to dispatch table | |
| P14-T033a | Deliver SIGCHLD to parent when child exits or stops | |

## Track G — Process Groups and Job Control

| Task | Description | Status |
|---|---|---|
| P14-T034 | Add `pgid: Pid` field to `Process`; default to own PID | |
| P14-T035 | Implement `sys_setpgid` (109) and `sys_getpgid` (121) | |
| P14-T036 | Extend `sys_kill` for negative PID (kill process group) | |
| P14-T037 | Track foreground process group (`FG_PGID`) | |
| P14-T038 | Wire Ctrl-C → SIGINT to `FG_PGID` | |
| P14-T039 | Wire Ctrl-Z → SIGTSTP to `FG_PGID` | |
| P14-T040 | Implement `waitpid(-1, ...)` to wait for any child | |
| P14-T041 | Implement `WUNTRACED` flag in waitpid | |
| P14-T041a | Encode waitpid status: WIFEXITED, WIFSTOPPED, WIFSIGNALED | |

## Track H — Shell Rewrite

| Task | Description | Status |
|---|---|---|
| P14-T042 | Shell main loop: read line from stdin, parse, execute, loop | |
| P14-T043 | Command parser: split on `\|`, handle `>`, `<`, `>>`, `&` | |
| P14-T044 | Simple command execution: fork → child execve → parent waitpid | |
| P14-T045 | Pipeline execution: fork two children, connect with pipe + dup2 | |
| P14-T046 | Output redirection: `cmd > file` | |
| P14-T047 | Input redirection: `cmd < file` | |
| P14-T048 | Append redirection: `cmd >> file` | |
| P14-T049 | Background execution: `cmd &` | |
| P14-T050 | Environment variables: `export KEY=val`, `$KEY` expansion | |
| P14-T051 | Built-in `cd`: chdir syscall | |
| P14-T052 | Built-in `exit` | |
| P14-T053 | Built-in `export` / `unset` / `env` | |
| P14-T054 | Built-in `fg` / `bg` | |
| P14-T055 | Built-in `help` | |
| P14-T056 | PATH search for commands | |

## Track I — Core Utilities

| Task | Description | Status |
|---|---|---|
| P14-T057 | `echo` — print arguments to stdout | |
| P14-T058 | `true` / `false` — exit 0 / exit 1 | |
| P14-T059 | `cat` — read file(s) and write to stdout | |
| P14-T060 | `ls` — list directory entries via getdents64 | |
| P14-T061 | `pwd` — print working directory via getcwd | |
| P14-T062 | `mkdir` / `rmdir` — create/remove directories | |
| P14-T063 | `rm` — remove files via unlink | |
| P14-T064 | `cp` — copy file: open+read source, open+write dest | |
| P14-T065 | `mv` — rename file via rename, fallback to cp+rm | |
| P14-T066 | `env` — print all environment variables | |
| P14-T067 | `sleep` — sleep for N seconds | |
| P14-T067a | `grep` — search stdin or files for a fixed string | |
| P14-T068 | Implement `sys_nanosleep` (syscall 35) | |
| P14-T069 | Implement `getdents64` (syscall 217) for real | |
| P14-T070 | Add all utility binaries to musl build + ramdisk | |

## Track J — Validation and Documentation

| Task | Description | Status |
|---|---|---|
| P14-T071 | Acceptance: `echo hello` prints "hello" | |
| P14-T072 | Acceptance: `cat /tmp/test.txt` prints file contents | |
| P14-T073 | Acceptance: `cat file.txt > /tmp/copy.txt` creates copy via redirection | |
| P14-T074 | Acceptance: `ls \| grep txt` produces filtered listing | |
| P14-T075 | Acceptance: Ctrl-C kills foreground command, shell survives | |
| P14-T076 | Acceptance: `sleep 10 &` runs in background, shell stays responsive | |
| P14-T076a | Acceptance: `fg` brings background job to foreground | |
| P14-T077 | Acceptance: `export FOO=bar && env` shows FOO=bar | |
| P14-T077a | Acceptance: `export PATH=/bin && ls` — PATH-based lookup works | |
| P14-T078 | Acceptance: all utility binaries run standalone | |
| P14-T079 | `cargo xtask check` passes (clippy + fmt) | |
| P14-T080 | QEMU boot validation — no panics, no regressions | |
| P14-T081 | Write `docs/14-shell-and-tools.md` | |
