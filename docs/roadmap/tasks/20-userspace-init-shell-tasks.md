# Phase 20 — Userspace Init and Shell: Task List

**Status:** Complete
**Source Ref:** phase-20
**Depends on:** Phase 19 ✅
**Goal:** Replace the kernel-resident `init_task` and `shell_task` ring-0 functions
with real ring-3 userspace processes. PID 1 becomes a `no_std` Rust binary loaded
from the ramdisk; the interactive shell is a userspace ELF that init spawns. The
kernel is no longer responsible for parsing commands or managing the interactive session.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Extend syscall-lib with high-level wrappers | — | ✅ Done |
| B | Userspace init binary | A | ✅ Done |
| C | Userspace shell binary | A | ✅ Done |
| D | Ramdisk and xtask integration | B, C | ✅ Done |
| E | Kernel cleanup — remove ring-0 shell/init | D | ✅ Done |
| F | Stdin bridge — keyboard to PID 1 fd | E | ✅ Done |
| G | Validation and documentation | F | ✅ Done |

---

## Track A — Extend syscall-lib

The existing `syscall-lib` only has `syscall0`–`syscall2` and a few constants.
Both init and the shell need higher-arity wrappers and safe Rust functions
for the full process lifecycle.

### A.1 — Add raw syscall wrappers (syscall3–syscall6)

**File:** `userspace/syscall-lib/src/lib.rs`
**Symbol:** `syscall3`, `syscall4`, `syscall5`, `syscall6`
**Why it matters:** All higher-arity syscalls (fork, exec, pipe, etc.) need more than two arguments passed through registers.

**Acceptance:**
- [x] `syscall3`, `syscall4`, `syscall5`, `syscall6` raw wrappers follow the existing `syscall0`–`syscall2` pattern
- [x] `r10` replaces `rcx` for arg 4 per Linux ABI

### A.2 — Add syscall number constants

**File:** `userspace/syscall-lib/src/lib.rs`
**Why it matters:** Named constants prevent magic-number bugs and keep the userspace ABI in sync with the kernel.

**Acceptance:**
- [x] Constants defined: `SYS_READ` (0), `SYS_WRITE` (1), `SYS_OPEN` (2), `SYS_CLOSE` (3), `SYS_FSTAT` (5), `SYS_LSEEK` (8), `SYS_MMAP` (9), `SYS_BRK` (12), `SYS_IOCTL` (16), `SYS_PIPE` (22), `SYS_DUP2` (33), `SYS_NANOSLEEP` (35), `SYS_KILL` (62), `SYS_CHDIR` (80), `SYS_MKDIR` (83), `SYS_GETCWD` (79), `SYS_SETPGID` (109), `SYS_GETPGID` (121)

### A.3 — Add file I/O wrappers

**File:** `userspace/syscall-lib/src/lib.rs`
**Symbol:** `read`, `write`, `open`, `close`
**Why it matters:** Provides safe Rust wrappers so userspace avoids raw unsafe syscall calls for basic file operations.

**Acceptance:**
- [x] `read(fd, buf) -> isize`, `write(fd, buf) -> isize`, `open(path, flags, mode) -> isize`, `close(fd) -> isize`

### A.4 — Add process lifecycle wrappers

**File:** `userspace/syscall-lib/src/lib.rs`
**Symbol:** `fork`, `execve`, `waitpid`, `getpid`, `getppid`
**Why it matters:** The init binary and shell both need process creation and reaping.

**Acceptance:**
- [x] `fork() -> isize`, `execve(path, argv, envp) -> isize`, `waitpid(pid, status, flags) -> isize`, `getpid() -> isize`, `getppid() -> isize`

### A.5 — Add pipe and directory wrappers

**File:** `userspace/syscall-lib/src/lib.rs`
**Symbol:** `pipe`, `dup2`, `chdir`, `getcwd`
**Why it matters:** The shell needs pipes for `cmd | cmd` and directory operations for `cd`.

**Acceptance:**
- [x] `pipe(fds: &mut [i32; 2]) -> isize`, `dup2(oldfd, newfd) -> isize`, `chdir(path) -> isize`, `getcwd(buf) -> isize`

### A.6 — Add signal and process group wrappers

**File:** `userspace/syscall-lib/src/lib.rs`
**Symbol:** `kill`, `setpgid`, `nanosleep`
**Why it matters:** Signal delivery (Ctrl-C) and process group management require these wrappers.

**Acceptance:**
- [x] `kill(pid, sig) -> isize`, `setpgid(pid, pgid) -> isize`, `nanosleep(seconds) -> isize`

### A.7 — Add flag and signal constants

**File:** `userspace/syscall-lib/src/lib.rs`
**Why it matters:** Named constants for open flags, wait options, and signals prevent ABI mismatches.

**Acceptance:**
- [x] Constants defined: `O_RDONLY`, `O_WRONLY`, `O_RDWR`, `O_CREAT`, `O_TRUNC`, `O_APPEND`, `WNOHANG`, `SIGINT`, `SIGCHLD`, `SIGTSTP`, `SIGCONT`, `STDIN_FILENO`, `STDOUT_FILENO`, `STDERR_FILENO`

### A.8 — Add write_str and write_u64 convenience helpers

**File:** `userspace/syscall-lib/src/lib.rs`
**Symbol:** `write_str`, `write_u64`
**Why it matters:** Error messages in `no_std` userspace need formatted output without `alloc`.

**Acceptance:**
- [x] `write_str(fd, s: &str) -> isize` convenience wrapper
- [x] `write_u64(fd, n: u64)` formats a number into a stack buffer and writes it

---

## Track B — Userspace Init Binary

Minimal `no_std` Rust binary that serves as PID 1: opens console fds,
spawns the shell, and reaps orphaned children.

### B.1 — Create init crate

**File:** `userspace/init/Cargo.toml`
**Why it matters:** Init is the first userspace process; without it there is no process tree.

**Acceptance:**
- [x] `no_std` crate, depends on `syscall-lib`, target `x86_64-unknown-none`

### B.2 — Create init entry point

**File:** `userspace/init/src/main.rs`
**Symbol:** `_start`
**Why it matters:** The kernel hands off control to this function; it must set up fds and never return.

**Acceptance:**
- [x] `#![no_std]`, `#![no_main]`, `#[panic_handler]` that calls `syscall_lib::exit(101)`
- [x] `#[no_mangle] pub extern "C" fn _start()` entry point

### B.3 — Open console file descriptors

**File:** `userspace/init/src/main.rs`
**Why it matters:** PID 1 needs stdin/stdout/stderr before it can spawn children that inherit them.

**Acceptance:**
- [x] Opens `/dev/console` three times as fds 0 (stdin), 1 (stdout), 2 (stderr)

### B.4 — Write boot banner

**File:** `userspace/init/src/main.rs`
**Why it matters:** Confirms userspace init has started successfully.

**Acceptance:**
- [x] Writes `"\nm3OS init (PID 1)\n"` to stdout

### B.5 — Fork and exec shell

**File:** `userspace/init/src/main.rs`
**Why it matters:** Init's primary job is launching the interactive shell.

**Acceptance:**
- [x] Calls `fork()`; child calls `execve("/bin/sh", argv, envp)` with `PATH=/bin:/sbin:/usr/bin`, `HOME=/`, `TERM=m3os`
- [x] Parent stores the shell PID

### B.6 — Reap loop

**File:** `userspace/init/src/main.rs`
**Why it matters:** PID 1 must reap orphans and respawn the shell to prevent zombies and keep the system usable.

**Acceptance:**
- [x] Infinite loop calling `waitpid(-1, &status, WNOHANG)`
- [x] If reaped PID is the shell, re-spawns it
- [x] On ECHILD, calls `nanosleep(1)` to avoid busy-spinning

### B.7 — Init never exits

**File:** `userspace/init/src/main.rs`
**Why it matters:** The kernel panics if PID 1 exits; defensive error handling prevents silent failures.

**Acceptance:**
- [x] If the reap loop somehow breaks, writes an error message and calls `exit(1)`

---

## Track C — Userspace Shell Binary

Interactive `no_std` Rust shell: reads lines, parses commands, fork-exec-wait,
pipes, redirection, builtins.

### C.1 — Create shell crate

**File:** `userspace/shell/Cargo.toml`
**Why it matters:** The shell provides the primary user interface to the OS.

**Acceptance:**
- [x] `no_std` crate, depends on `syscall-lib`, target `x86_64-unknown-none`

### C.2 — Create shell entry point

**File:** `userspace/shell/src/main.rs`
**Symbol:** `_start`, `main_loop`
**Why it matters:** Sets up the shell process and enters the read-eval-execute loop.

**Acceptance:**
- [x] `#![no_std]`, `#![no_main]`, `#[panic_handler]`, and `_start()` entry point

### C.3 — Prompt loop

**File:** `userspace/shell/src/main.rs`
**Why it matters:** The prompt loop is the core user interaction — read a line, execute, repeat.

**Acceptance:**
- [x] Writes `"$ "` to stdout, reads stdin one byte at a time into a 256-byte stack line buffer until `\n` or `\r`
- [x] Handles backspace (erase last byte, write `"\x08 \x08"` to erase on screen)

### C.4 — Character echo

**File:** `userspace/shell/src/main.rs`
**Why it matters:** Without TTY line discipline, the shell must echo typed characters itself.

**Acceptance:**
- [x] Writes each printable byte back to stdout as it is typed

### C.5 — Tokenizer

**File:** `userspace/shell/src/main.rs`
**Why it matters:** Command parsing must correctly split arguments, detect pipes, redirection, and background flags.

**Acceptance:**
- [x] Splits line on whitespace into argv array (max 32 tokens)
- [x] Respects single-quoted strings
- [x] Detects `|` as pipe separator, `>` / `>>` / `<` as redirection, `&` as background flag

### C.6 — cd builtin

**File:** `userspace/shell/src/main.rs`
**Why it matters:** Directory changes must happen in the shell process itself, not a child.

**Acceptance:**
- [x] Calls `chdir(path)`; if no argument, `chdir("/")`

### C.7 — exit builtin

**File:** `userspace/shell/src/main.rs`
**Why it matters:** Allows clean shell exit; init will re-spawn it.

**Acceptance:**
- [x] Calls `exit(0)`

### C.8 — Simple command execution

**File:** `userspace/shell/src/main.rs`
**Why it matters:** The fundamental fork-exec-wait pattern is the basis for running any external command.

**Acceptance:**
- [x] `fork()`, child calls `execve(cmd, argv, envp)`; parent calls `waitpid(child_pid, &status, 0)` for foreground
- [x] Prints `"command not found: <cmd>\n"` if execve returns

### C.9 — PATH resolution

**File:** `userspace/shell/src/main.rs`
**Why it matters:** Users expect to type `ls` instead of `/bin/ls`.

**Acceptance:**
- [x] Tries each directory in PATH (`/bin:/sbin:/usr/bin`) by concatenating `dir/cmd`
- [x] Also tries `dir/cmd.elf` for backward compatibility with ramdisk naming

### C.10 — Two-stage pipe

**File:** `userspace/shell/src/main.rs`
**Why it matters:** Pipes are the primary Unix composition mechanism.

**Acceptance:**
- [x] Detects `|` token; calls `pipe()` to get `[read_fd, write_fd]`
- [x] Forks left child with `dup2(write_fd, STDOUT_FILENO)`, right child with `dup2(read_fd, STDIN_FILENO)`
- [x] Closes both pipe fds in parent; `waitpid` both children

### C.11 — Output redirection (>)

**File:** `userspace/shell/src/main.rs`
**Why it matters:** Enables writing command output to files.

**Acceptance:**
- [x] Opens target file with `O_WRONLY | O_CREAT | O_TRUNC`; in child `dup2(file_fd, STDOUT_FILENO)`

### C.12 — Append redirection (>>)

**File:** `userspace/shell/src/main.rs`
**Why it matters:** Enables appending command output to files without overwriting.

**Acceptance:**
- [x] Same as `>` but open with `O_WRONLY | O_CREAT | O_APPEND`

### C.13 — Input redirection (<)

**File:** `userspace/shell/src/main.rs`
**Why it matters:** Enables feeding file contents as stdin to commands.

**Acceptance:**
- [x] Opens source file with `O_RDONLY`; in child `dup2(file_fd, STDIN_FILENO)`

### C.14 — Ctrl-C handling

**File:** `userspace/shell/src/main.rs`
**Why it matters:** Users expect Ctrl-C to kill the foreground child without killing the shell.

**Acceptance:**
- [x] If the shell has a foreground child, calls `kill(child_pid, SIGINT)`
- [x] If no child is running, prints a new prompt

### C.15 — Print exit status

**File:** `userspace/shell/src/main.rs`
**Why it matters:** Helps users debug failing commands.

**Acceptance:**
- [x] After `waitpid`, if child exited with non-zero status, prints `"exit <code>\n"`

---

## Track D — Ramdisk and Xtask Integration

Wire init and shell binaries into the build pipeline and ramdisk so they
appear at `/sbin/init` and `/bin/sh`.

### D.1 — Uncomment workspace members

**File:** `Cargo.toml`
**Why it matters:** The workspace must include the new crates for them to be built.

**Acceptance:**
- [x] `"userspace/init"` and `"userspace/shell"` uncommented in workspace `Cargo.toml` members

### D.2 — Update xtask build step

**File:** `xtask/src/main.rs`
**Why it matters:** The build system must compile the new binaries alongside existing ones.

**Acceptance:**
- [x] Compiles `userspace/init` and `userspace/shell` with same target/flags as existing test binaries

### D.3 — Copy binaries to initrd

**Files:** `kernel/initrd/`
**Why it matters:** Binaries must be embedded in the ramdisk for the kernel to load them.

**Acceptance:**
- [x] Compiled binaries copied to `kernel/initrd/init.elf` and `kernel/initrd/sh.elf`

### D.4 — Update ramdisk file table

**File:** `kernel/src/fs/ramdisk.rs`
**Why it matters:** The ramdisk must register the binaries at their expected paths for exec to find them.

**Acceptance:**
- [x] Init registered at `/sbin/init` and sh at `/bin/sh` in the ramdisk file table

### D.5 — Verify build

**Why it matters:** Ensures the full build pipeline works end-to-end with the new binaries.

**Acceptance:**
- [x] `cargo xtask image` builds successfully with the new binaries included

---

## Track E — Kernel Cleanup

Remove the ring-0 init_task and shell_task from the kernel. Replace with
loading `/sbin/init` as PID 1 via the ELF loader.

### E.1 — Load /sbin/init as PID 1

**File:** `kernel/src/main.rs`
**Why it matters:** Transitions from kernel-resident init to userspace init — the core goal of this phase.

**Acceptance:**
- [x] After kernel-task servers are spawned, loads `/sbin/init` from ramdisk via ELF loader and transfers to ring-3 as PID 1

### E.2 — Pass initial environment

**File:** `kernel/src/main.rs`
**Symbol:** `setup_abi_stack_with_envp`
**Why it matters:** PID 1 needs PATH and other environment variables from the very start.

**Acceptance:**
- [x] Initial environment passed via `setup_abi_stack_with_envp()`: `PATH=/bin:/sbin:/usr/bin`, `HOME=/`, `TERM=m3os`

### E.3 — Remove shell_task

**File:** `kernel/src/main.rs`
**Why it matters:** Eliminates ~870 lines of ring-0 shell code that is now handled in userspace.

**Acceptance:**
- [x] `shell_task()`, `shell_execute()`, `shell_fork_exec()`, `shell_pipeline()`, `resolve_command()`, background job management, and environment variable storage removed

### E.4 — Remove init_task

**File:** `kernel/src/main.rs`
**Why it matters:** Kernel-task server spawning remains in kernel_main directly; the init function wrapper is no longer needed.

**Acceptance:**
- [x] `init_task()` function removed; server spawning code remains in `kernel_main`

### E.5 — Remove p11_launcher_task

**File:** `kernel/src/main.rs`
**Why it matters:** This Phase 11 test launcher is no longer needed with userspace init in place.

**Acceptance:**
- [x] `p11_launcher_task()` removed or reduced

### E.6 — Verify kernel-task servers

**Why it matters:** Server tasks must still start and be reachable via IPC after the restructuring.

**Acceptance:**
- [x] `console_server`, `kbd_server`, `vfs_server`, `fat_server` still start correctly and are reachable from userspace via IPC

---

## Track F — Stdin Bridge

Ensure keyboard input reaches the userspace shell's stdin fd. The current
`stdin_feeder_task` feeds a kernel buffer; it needs to feed PID 1's fd 0 instead.

### F.1 — Evaluate keyboard-to-userspace path

**File:** `kernel/src/stdin.rs`
**Why it matters:** The existing stdin path may or may not work for userspace processes.

**Acceptance:**
- [x] Keyboard bytes correctly reach userspace `read(0, ...)` calls

### F.2 — Implement stdin bridge if needed

**File:** `kernel/src/stdin.rs`, `kernel/src/main.rs`
**Symbol:** `stdin_feeder_task`
**Why it matters:** Without the bridge, the shell cannot read keyboard input.

**Acceptance:**
- [x] Decoded keyboard bytes reach the shell's fd 0 via `read(0, buf, 1)` blocking

### F.3 — Verify fd inheritance

**Why it matters:** The shell inherits fds from init via fork+exec; broken inheritance breaks input.

**Acceptance:**
- [x] Init's fd 0 is inherited by the shell after `fork` + `execve`

### F.4 — Verify echo end-to-end

**Why it matters:** Characters must be visible on screen as the user types them.

**Acceptance:**
- [x] Shell writes typed characters to stdout (fd 1), which routes to the console/framebuffer

---

## Track G — Validation and Documentation

### G.1 — Acceptance tests

**Why it matters:** Validates the complete userspace init and shell implementation.

**Acceptance:**
- [x] OS boots and presents `$ ` prompt from the ring-3 shell without any ring-0 command parsing
- [x] `echo hello world` prints correctly
- [x] `ls | cat` produces directory listing via a two-stage pipe
- [x] `cat /sbin/init > /dev/null` exercises I/O redirection
- [x] `cd /bin && pwd` prints `/bin`
- [x] Ctrl-C during a long-running child kills the child; shell returns to prompt
- [x] Running a nonexistent command prints an error and returns to prompt
- [x] `true` exits 0 silently; `false` exits 1 with status notice
- [x] Orphaned children are reaped by PID 1 (no zombie accumulation)
- [x] `exit` in the shell causes init to re-spawn it; a new `$ ` prompt appears
- [x] `kernel/src/main.rs` no longer contains `shell_task` or `init_task` functions
- [x] `cargo xtask check` passes (clippy + fmt)
- [x] QEMU boot validation — no panics, no regressions

### G.2 — Documentation

**File:** `docs/18-userspace-init.md`
**Why it matters:** Documents the PID 1 contract and the new userspace architecture.

**Acceptance:**
- [x] Written: PID 1 contract (why init must never exit, orphan reaping), `_start` → `main` entry sequence, syscall wrapper pattern, shell fork-exec-wait loop, pipe fd plumbing diagram, which kernel-task servers remain in ring-0 and why

---

## Deferred Until Later

These items are explicitly out of scope for Phase 20:

- Moving kernel-task servers (console, kbd, vfs, fat) to ring-3 (requires capability-grant IPC)
- PTY / TTY line discipline (`/dev/pts`, `termios`, raw mode, kernel-side echo)
- Job control: `SIGTSTP`, `SIGCONT`, `fg`, `bg`, process groups, sessions
- Multi-user login and `/etc/passwd`
- Shell scripting: loops, conditionals, functions, variable assignment
- Environment variable `$VAR` expansion (can be added incrementally later)
- Tab completion and readline-style line editing
- `exec` builtin, `source` / `.`
- Pipelines longer than two stages
- Subshell expansion `$(...)` and backtick substitution
- Here-documents and here-strings
- Stderr redirection (`2>`, `2>&1`) and fd duplication beyond 0/1/2
- `alloc`-based dynamic data structures in userspace (use fixed-size stack buffers)

---

## Documentation Notes

- Phase 20 is the transition from ring-0 to ring-3 for init and shell
- ~870 lines of kernel-mode shell code removed from `kernel/src/main.rs`
- `userspace/init/` and `userspace/shell/` are new crate directories
- `syscall-lib` expanded from `syscall0`–`syscall2` to `syscall0`–`syscall6` with full process lifecycle wrappers
- `docs/18-userspace-init.md` written as the phase documentation
