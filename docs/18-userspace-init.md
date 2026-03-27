# Phase 20: Userspace Init and Shell

This document describes the transition from kernel-resident init/shell
functions to real ring-3 userspace processes: PID 1 (`/sbin/init`) and
an interactive shell (`/bin/sh`).

## Why PID 1 Must Never Exit

In Unix systems, PID 1 is the first userspace process and serves as the
ancestor of all other processes. The kernel relies on PID 1 to:

1. **Reap orphaned children.** When a parent exits before its child, the
   child is re-parented to PID 1. If PID 1 doesn't call `waitpid`, these
   orphans become zombies that leak process table entries.
2. **Restart essential services.** If the shell crashes or exits, init
   respawns it so the system remains interactive.
3. **Signal the end of the system.** If PID 1 exits, the kernel has no
   process to schedule and typically panics.

Our init implements the minimal PID 1 contract: fork+exec the shell,
enter a reap loop calling `waitpid(-1, &status, WNOHANG)`, and respawn
the shell if it exits.

## Entry Sequence for `no_std` Rust Userspace

Userspace binaries are `no_std` Rust crates compiled for
`x86_64-unknown-none`. The entry point is:

```rust
#[no_mangle]
pub extern "C" fn _start() -> ! {
    // program logic
    exit(0)
}
```

The kernel's ELF loader sets up the System V AMD64 ABI initial stack:

```
high addresses
  +-----------------+
  |    envp[n]=NULL  |
  |    envp[1]       |
  |    envp[0]       |
  |    argv[argc]=NULL|
  |    argv[0]       |
  |    argc          |  <-- RSP at entry
  +-----------------+
low addresses
```

For `no_std` binaries, `_start` receives `argc` in `[rsp]` and `argv`
in `[rsp+8..]`, but our init and shell don't parse command-line args.

## Syscall Wrapper Pattern

The `syscall-lib` crate provides raw wrappers (`syscall0`..`syscall6`)
and safe high-level functions. The syscall ABI:

| Register | Role |
|----------|------|
| `rax`    | Syscall number (in) / return value (out) |
| `rdi`    | Argument 1 |
| `rsi`    | Argument 2 |
| `rdx`    | Argument 3 |
| `r10`    | Argument 4 (NOT `rcx` -- clobbered by `syscall`) |
| `r8`     | Argument 5 |
| `r9`     | Argument 6 |

Example wrapper:

```rust
pub fn fork() -> isize {
    unsafe { syscall0(SYS_FORK) as isize }
}
```

## Shell Fork-Exec-Wait Loop

The shell's main loop:

1. Write `"$ "` to stdout
2. Read one byte at a time from stdin, echoing each character
3. On newline: tokenize the line, check builtins (`cd`, `exit`)
4. For external commands: `fork()`, child calls `execve(cmd, argv, envp)`
5. Parent calls `waitpid(child_pid, &status, 0)` to wait
6. Print non-zero exit codes

PATH resolution tries each directory in `/bin:/sbin:/usr/bin`, both with
and without `.elf` suffix (for backward compatibility with ramdisk naming).

## Pipe FD Plumbing

For `cmd1 | cmd2`:

```
                 pipe
Parent:    [read_fd, write_fd]  -- creates pipe
                |         |
  fork left     |         |
  child:   close(read_fd) |
           dup2(write_fd, STDOUT)
           close(write_fd)
           exec(cmd1)
                          |
  fork right              |
  child:        close(write_fd)
                dup2(read_fd, STDIN)
                close(read_fd)
                exec(cmd2)

Parent:    close(read_fd)
           close(write_fd)
           waitpid(left)
           waitpid(right)
```

Both children MUST close the pipe ends they don't use, otherwise:
- If the left child doesn't close `read_fd`, the right child's `read()`
  never gets EOF (the pipe appears to still have a reader).
- If the right child doesn't close `write_fd`, the left child's `write()`
  never gets EPIPE.

The parent also closes both ends so it doesn't hold open references.

## Kernel-Task Servers Remaining in Ring 0

These IPC-based services still run as kernel tasks (not userspace):

| Server | Why still in ring 0 |
|--------|-------------------|
| `console_server` | Needs direct serial port I/O and framebuffer access |
| `kbd_server` | Reads hardware scan codes from IRQ handler's ring buffer |
| `fat_server` | Serves ramdisk file content embedded in kernel binary |
| `vfs_server` | Routes file ops to fat/tmpfs backends in kernel memory |
| `stdin_feeder` | Bridges keyboard IRQ to the global stdin buffer |

Moving these to ring 3 requires capability-grant IPC for hardware access,
which is planned for a future phase.

## Bugs Fixed During Phase 20

### CoW Fork Parent Table Flags

The `cow_clone_user_pages` function used `map_to()` which derives
intermediate page table flags from the leaf PTE flags. Since child PTEs
have `WRITABLE` cleared for CoW, the intermediate entries (PD, PDPT,
PML4) were also created without `WRITABLE`. After CoW resolution set the
leaf PTE writable, writes still failed because x86_64 checks `WRITABLE`
at ALL page table levels.

Fix: Use `map_to_with_table_flags()` with `PRESENT | WRITABLE |
USER_ACCESSIBLE` for intermediate entries.

### ELF PIE Relocation

PIE binaries compiled with `compiler_builtins` have `R_X86_64_RELATIVE`
relocations in the `.rela.dyn` section (GOT entries for `memset`/`memcpy`).
The ELF loader didn't process these, leaving GOT entries un-relocated.
Indirect calls through the GOT jumped to address 0.

Fix: Parse the `PT_DYNAMIC` segment to find `DT_RELA`/`DT_RELASZ`,
then apply `R_X86_64_RELATIVE` relocations: write `load_bias + addend`
at each target address.

### Syscall Yield Concurrency

Blocking syscalls (`read`, `waitpid`, `nanosleep`, pipe I/O) call
`yield_now()` inside the syscall handler. The global `SYSCALL_USER_RSP`
and `SYSCALL_STACK_TOP` variables were overwritten by other tasks entering
the syscall path during the yield. On resume, `sysretq` returned to the
wrong user stack.

Fix: Save `SYSCALL_USER_RSP` before yielding and restore it via
`restore_caller_context()` which also restores CR3 and kernel stack.
