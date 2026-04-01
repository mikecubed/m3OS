# Phase 19 — Signal Handlers

**Status:** Complete
**Source Ref:** phase-19
**Depends on:** Phase 18 ✅
**Builds on:** Extends the process model and syscall gate from Phase 18; adds signal delivery infrastructure for userspace handler execution
**Primary Components:** kernel/src/signal.rs, kernel/src/arch/x86_64/, kernel/src/process/

## Milestone Goal

Enable userspace programs to install and execute signal handlers. Implement the
signal trampoline mechanism so ring-3 code can catch `SIGINT`, `SIGSEGV`, `SIGCHLD`,
and other signals, run a user-supplied handler, and return cleanly to the interrupted
execution without kernel assistance beyond the initial delivery and the `sigreturn`
syscall.

```mermaid
flowchart TD
    Event["hardware interrupt<br/>or kill() call"] --> Deliver["kernel: check pending<br/>signals on return to ring-3"]
    Deliver --> HasHandler{"registered<br/>handler?"}
    HasHandler -->|"SIG_DFL / SIG_IGN"| Default["default action<br/>(term / ignore / stop)"]
    HasHandler -->|"user handler"| Frame["push sigframe<br/>to user stack"]
    Frame --> Trampoline["set RIP = handler<br/>set RSP = sigframe"]
    Trampoline --> Ring3["ring-3: handler runs"]
    Ring3 -->|"sigreturn (syscall 15)"| Restore["kernel: restore ucontext<br/>from sigframe"]
    Restore --> Resume["resume interrupted<br/>instruction"]
```

## Why This Phase Exists

Without signal handlers, userspace programs have no way to respond to asynchronous
events like `Ctrl-C`, segmentation faults, or child process termination. The kernel
can only apply default actions (terminate or ignore). Real programs need to install
custom handlers for graceful shutdown, error recovery, and child process management.
The signal trampoline mechanism -- pushing a frame onto the user stack and returning
to a handler via `IRET` -- is one of the most intricate kernel-userspace interfaces
and a key piece of POSIX compatibility required by musl libc and the Ion shell.

## Learning Goals

- Understand why the kernel must save the full interrupted register state before
  branching to a signal handler and how it reconstructs it on return.
- Learn what a signal trampoline is and why userspace (musl's `__restore_rt`) calls
  `sigreturn` rather than returning normally from the handler function.
- See why `sigreturn` is the only safe way to restore privileged state: a user
  function returning normally would unwind the C call stack, losing the interrupted
  registers.
- See how signal masking during handler execution prevents re-entrant delivery of
  the same signal.
- Understand the role of `sigaltstack` in handling `SIGSEGV` when the process stack
  has already overflowed.
- Learn how `sa_restorer` in `rt_sigaction` links the kernel's frame layout to the
  libc-provided trampoline stub.

## Feature Scope

- **`sigreturn` (syscall 15)**: restore the `ucontext` / `sigframe` previously pushed
  to the user stack and resume the interrupted thread
- **Signal trampoline**: when delivering a signal to a process with a registered
  handler, the kernel pushes a `sigframe` (saved registers + `ucontext`) onto the
  user stack, sets `RIP` to the handler address, and sets `RSP` to the adjusted frame
  pointer; the frame includes a return address pointing at the `__restore_rt` stub
- **`rt_sigprocmask` (syscall 14)**: implement the blocked-signal bitfield per process;
  `SIG_BLOCK`, `SIG_UNBLOCK`, `SIG_SETMASK` operations
- **`sa_mask` honour**: during handler execution, add `sa_mask | signal_being_delivered`
  to the process's blocked set; restore the original mask on `sigreturn`
- **`sigaltstack` (syscall 131)**: register and activate an alternate signal stack;
  used for `SIGSEGV` handlers when the main stack has overflowed
- **musl compatibility**: validate that `rt_sigaction` with `SA_RESTORER` stores the
  restorer pointer and that the kernel uses it as the return address in the frame

## Important Components and How They Work

### Sigframe Layout

The `sigframe` struct matches the Linux `ucontext_t` + `siginfo_t` layout that musl
expects: general-purpose registers, `rflags`, `rip`, `rsp`, the old signal mask, and
the restorer address. The frame must be 16-byte aligned per the System V AMD64 ABI.

### Signal Delivery Path

`check_pending_signals()` runs on every return to ring-3 (including from blocking
syscalls). It selects a pending, unblocked signal and either applies the default action
or calls `setup_signal_frame()` if a user handler is registered.

### `setup_signal_frame()`

Reads the current user `RSP` (or alt-stack base if `SS_ONSTACK` and `SA_ONSTACK` is
set), subtracts the frame size, writes the `sigframe` struct to user memory, then
mutates the saved trap frame so that `IRET`/`SYSRET` returns to the handler address
with `RSP` pointing at the sigframe.

### `sys_sigreturn`

Reads the `sigframe` pointer from the user stack, validates it is in user-space, copies
saved registers back into the trap frame, and restores the old signal mask. This is the
only safe way to restore the interrupted execution context.

### Signal Masking

`sys_rt_sigprocmask` updates `task.blocked_signals` per the `how` argument. `SIGKILL`
and `SIGSTOP` can never be blocked. During handler execution, `sa_mask | delivered_signal`
is automatically added to the blocked set and restored by `sigreturn`.

## How This Builds on Earlier Phases

- **Extends Phase 11**: builds on the process model and trap frame infrastructure from
  the ELF loader and process lifecycle phase
- **Extends Phase 14**: uses the `fork`/`exec`/`waitpid` syscall infrastructure to
  deliver signals to child processes
- **Extends Phase 18**: signal checks integrate with the syscall return path established
  in the directory/VFS phase
- **Reuses Phase 17**: CoW page handling must interact correctly with signal frame
  writes to user stack pages

## Implementation Outline

1. Define the `sigframe` layout in the kernel (matches the Linux `ucontext_t` +
   `siginfo_t` layout that musl expects): general-purpose registers, `rflags`,
   `rip`, `rsp`, the old signal mask, and the restorer address.
2. In `check_pending_signals()`, after selecting a signal to deliver, check the
   process's `SignalDisposition` table for a user handler. If present, call
   `setup_signal_frame()` instead of the default-action path.
3. Implement `setup_signal_frame()`: read the current user `RSP` (or alt-stack base
   if `SS_ONSTACK` and the signal has `SA_ONSTACK` set), subtract the frame size,
   write the `sigframe` struct to user memory, then mutate the saved trap frame so
   that `IRET` / `SYSRET` returns to the handler.
4. Implement `sys_sigreturn`: read the `sigframe` pointer from the user stack (`RSP`
   at the time of the syscall), validate it is in user-space, copy saved registers
   back into the trap frame, and restore the old signal mask.
5. Implement `sys_rt_sigprocmask`: update `task.blocked_signals` according to the
   `how` argument (`SIG_BLOCK`, `SIG_UNBLOCK`, `SIG_SETMASK`). Never allow blocking
   `SIGKILL` or `SIGSTOP`.
6. Extend `rt_sigaction` to record `sa_restorer` (the `SA_RESTORER` flag) and store
   it in the `SignalAction` entry; use this address as the return address written into
   the `sigframe`.
7. Implement `sys_sigaltstack` (syscall 131): read / write the `stack_t` struct in
   the process's task block; set the `SS_ONSTACK` flag in the saved `stack_t` while
   a handler using the alt stack is executing.
8. Write a test program that installs a `SIGINT` handler via `rt_sigaction`, raises
   `SIGINT` via `kill(getpid(), SIGINT)`, executes the handler, and returns; assert
   that execution continues after the `raise` call.
9. Write a `SIGSEGV` handler test: map no guard page, overflow the stack, recover
   via `sigaltstack`-backed handler.
10. Audit `check_pending_signals()` call sites: it must be invoked on every return to
    ring-3, including from `sys_read`, `sys_write`, `sys_waitpid`, and any future
    blocking syscalls that can be interrupted by a signal (`EINTR` handling).
11. Add a kernel-side assertion that `sigframe` written to user memory is 16-byte
    aligned (System V AMD64 ABI requires 16-byte stack alignment at `CALL`); misaligned
    frames cause SSE faults in musl startup code.

## Acceptance Criteria

- A statically linked musl binary that installs a `SIGINT` handler, calls `raise(SIGINT)`,
  prints inside the handler, and continues execution after `raise` returns runs correctly.
- `SIGSEGV` delivered to a process with a `sigaltstack`-backed handler executes the
  handler and does not triple-fault.
- A handler does not re-enter itself when the same signal fires during handler execution
  (blocked by automatic masking).
- `rt_sigprocmask(SIG_BLOCK, ...)` prevents delivery of the blocked signal until
  `SIG_UNBLOCK` is called; the blocked signal is held as pending and delivered
  immediately on unblock.
- After `sigreturn`, the process resumes at the exact instruction that was interrupted,
  with all registers restored to their pre-signal values.
- `SIGKILL` and `SIGSTOP` cannot be blocked or caught; `rt_sigaction` returns `EINVAL`
  for both.
- Nested signals with distinct numbers are handled correctly: signal A fires during
  signal B's handler (B is not in `sa_mask` of A), both handlers run, both frames are
  restored in reverse order.
- The kernel shell (Phase 9 / ring-0) continues to handle `SIGINT` via its existing
  default-action path; no regression.
- `sigaltstack` stack is marked `SS_ONSTACK` while the handler runs and cleared on
  `sigreturn`; a second call to `sigaltstack` while `SS_ONSTACK` returns `EPERM`.

## Companion Task List

- [Phase 19 Task List](./tasks/19-signal-handlers-tasks.md)

## How Real OS Implementations Differ

- Linux maintains a full `sigcontext` embedded inside `ucontext_t` for every supported
  architecture, including FPU / SSE / AVX state (managed via `XSAVE`).
- The in-kernel `copy_siginfo_to_user` and related helpers handle dozens of signal
  sources with different `siginfo` payloads.
- Linux also supports real-time signals (`SIGRTMIN` through `SIGRTMAX`) with a queued
  delivery model and `sigqueue` for passing an integer or pointer payload alongside
  the signal.
- This phase implements only the standard-signal subset with no FPU state save and no
  real-time signal queue.

## Deferred Until Later

- Real-time signals (`SIGRTMIN` through `SIGRTMAX`) and the `sigqueue` API
- `signalfd` -- receiving signals as readable file descriptors
- FPU / SSE / AVX state save and restore in the `sigframe`
- Per-thread signal masks and thread-directed signal delivery (requires `clone`)
- `SA_NODEFER`, `SA_RESETHAND` flag semantics
- `SIGALRM` and `timer_create` timer signals
- `ptrace`-stop signals
