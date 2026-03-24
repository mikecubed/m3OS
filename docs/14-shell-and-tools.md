# Phase 14 — Shell and Userspace Tools

Phase 14 delivers an interactive fork+exec shell with pipes, I/O redirection,
job control, environment variables, and 14 core utilities compiled as
standalone musl-linked static ELF binaries.

## Architecture

```
Keyboard IRQ1 → kbd_server → stdin_feeder → stdin buffer
                                                 ↓
                                            shell_task
                                           (reads stdin)
                                                 ↓
                                         parse command line
                                        /        |        \
                                   builtin   fork+exec   pipeline
                                  (cd,env)   (echo.elf)  (ls|grep)
                                                 ↓
                                          child process
                                         (userspace ELF)
```

## Per-Process File Descriptor Table

Each `Process` now owns its own `fd_table: [Option<FdEntry>; 32]`.

- **FD 0** — stdin (reads from kernel stdin buffer, blocking)
- **FD 1** — stdout (writes to serial/console)
- **FD 2** — stderr (writes to serial/console)
- **FD 3+** — ramdisk files, tmpfs files, or pipe ends

`fork()` deep-clones the parent's FD table into the child.

### FdBackend variants

| Variant | Description |
|---|---|
| `Stdin` | Reads from kernel stdin buffer |
| `Stdout` | Writes to serial output |
| `Ramdisk { addr, len }` | Read-only ramdisk file |
| `Tmpfs { path }` | Writable tmpfs file |
| `PipeRead { pipe_id }` | Read end of a kernel pipe |
| `PipeWrite { pipe_id }` | Write end of a kernel pipe |

## Pipes

`pipe(pipefd)` (syscall 22) allocates a 4 KiB kernel ring buffer and returns
two FDs: one for reading, one for writing.

- **Read**: blocks (yield-loop) when buffer is empty and writer is open;
  returns 0 (EOF) when writer closes.
- **Write**: blocks when buffer is full and reader is open; returns EPIPE
  when reader closes.
- **Close**: marks the corresponding end as closed; frees the pipe when
  both ends are closed.

## dup2

`dup2(oldfd, newfd)` (syscall 33) duplicates an FD entry. Closes `newfd`
if it was open (including pipe cleanup). `dup2(fd, fd)` is a no-op.

## Argv/Envp in execve

`execve(filename, argv, envp)` now follows the Linux ABI:
- `filename`: null-terminated C string path
- `argv`: null-terminated array of `char*` pointers
- `envp`: null-terminated array of `char*` pointers

Strings are copied into kernel buffers via `copy_from_user` and passed
to `setup_abi_stack_with_envp()` which builds the SysV AMD64 ABI initial
stack with both argv and envp arrays.

## Stdin Integration

The `stdin_feeder_task` reads scancodes from `kbd_server` via IPC, decodes
them to characters, and feeds them into a kernel stdin buffer (`stdin.rs`).

- **Line buffering**: characters accumulate until Enter, then the line
  (including `\n`) is flushed to the read buffer.
- **Echo**: typed characters are echoed to the console.
- **Backspace**: removes the last character from the line buffer and
  erases it from the console display.
- **Ctrl-C**: sends SIGINT to the foreground process group.
- **Ctrl-Z**: sends SIGTSTP to the foreground process group.

## Signal Infrastructure

Each process has:
- `pending_signals: u64` — bitfield of pending signals
- `signal_actions: [SignalAction; 32]` — Default or Ignore per signal

### Supported signals

| Signal | Default action |
|---|---|
| SIGINT (2) | Terminate |
| SIGKILL (9) | Terminate (cannot be caught) |
| SIGTERM (15) | Terminate |
| SIGCHLD (17) | Ignore |
| SIGCONT (18) | Continue (resume stopped process) |
| SIGSTOP (19) | Stop (cannot be caught) |
| SIGTSTP (20) | Stop |

### Syscalls

- `kill(pid, sig)` (62) — send signal to process or process group
- `rt_sigaction(sig, act, oldact)` (13) — install/query signal disposition
- `rt_sigprocmask(...)` (14) — stub (always succeeds)

Signal delivery happens after every non-divergent syscall via
`check_pending_signals()`.

## Process Groups and Job Control

Each process has a `pgid: Pid` field (defaults to own PID).

- `setpgid(pid, pgid)` (109) — set process group ID
- `getpgid(pid)` (121) — get process group ID
- `FG_PGID` atomic — tracks the foreground process group
- `kill(-pgid, sig)` — sends signal to all processes in a group
- `waitpid(-1, ...)` — wait for any child
- `WUNTRACED` flag — report stopped children

## Shell

The shell is a kernel task that reads from the stdin buffer and uses
fork+exec to launch external commands.

### Features

- **Simple commands**: `echo hello` → fork, execve echo.elf with argv
- **Pipelines**: `ls | grep txt` → two children connected by pipe+dup2
- **Background**: `sleep 10 &` → don't wait, track in job list
- **Environment**: `export FOO=bar`, `$FOO` expansion, `env`
- **Job control**: `fg` (bring to foreground + SIGCONT), `bg` (SIGCONT)
- **Builtins**: help, cd, exit, export, unset, env, fg, bg

### Command resolution

Commands are looked up as `{cmd}.elf` in the ramdisk.

## Core Utilities

14 standalone musl-linked static ELF binaries in `userspace/coreutils/`:

| Utility | Syscalls exercised |
|---|---|
| echo | write |
| true/false | exit |
| cat | open, read, write, close |
| ls | open, getdents64, write, close |
| pwd | getcwd, write |
| mkdir | mkdir |
| rmdir | rmdir |
| rm | unlink |
| cp | open, read, write, close |
| mv | rename |
| env | write (reads environ) |
| sleep | nanosleep |
| grep | open, read, write, close (string search) |

## New Syscalls Added

| Number | Name | Description |
|---|---|---|
| 13 | rt_sigaction | Install/query signal disposition |
| 14 | rt_sigprocmask | Stub (always succeeds) |
| 22 | pipe | Create pipe with read/write FDs |
| 33 | dup2 | Duplicate file descriptor |
| 35 | nanosleep | Sleep for specified time |
| 62 | kill | Send signal to process/group |
| 109 | setpgid | Set process group ID |
| 121 | getpgid | Get process group ID |

## Files Changed

### New files
- `kernel/src/pipe.rs` — kernel pipe implementation
- `kernel/src/stdin.rs` — kernel stdin buffer with line editing
- `userspace/coreutils/*.c` — 14 core utility C programs

### Modified files
- `kernel/src/process/mod.rs` — FdEntry/FdBackend types, fd_table, pgid,
  signals, process groups
- `kernel/src/arch/x86_64/syscall.rs` — per-process FD access, pipe/dup2,
  signals, nanosleep, process groups, argv/envp in execve
- `kernel/src/mm/elf.rs` — setup_abi_stack_with_envp (envp support)
- `kernel/src/main.rs` — shell rewrite, stdin_feeder_task
- `kernel/src/fs/ramdisk.rs` — 14 new ELF entries
- `xtask/src/main.rs` — 14 new musl build entries
