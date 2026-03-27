# Phase 19: Signal Handlers

This document describes the signal handler implementation in m3OS: how
userspace processes install handlers via `rt_sigaction`, how the kernel
delivers signals by building a sigframe on the user stack, and how
`sigreturn` restores the interrupted context.

## Signal Delivery Model

Each process carries three signal-related data structures:

| Field | Type | Purpose |
|-------|------|---------|
| `pending_signals` | `u64` bitfield | Bit N set = signal N is waiting for delivery |
| `blocked_signals` | `u64` bitfield | Bit N set = signal N is blocked from delivery |
| `signal_actions`  | `[SignalAction; 32]` | Per-signal disposition: `Default`, `Ignore`, or `Handler` |

A signal is **deliverable** when it is pending AND not blocked:

```rust
let deliverable = proc.pending_signals & !proc.blocked_signals;
```

`dequeue_signal()` finds the lowest-numbered deliverable signal via
`deliverable.trailing_zeros()`, clears its pending bit, and returns the
signal number with its resolved disposition.

The `SignalAction` enum determines how the kernel handles the signal:

```rust
pub enum SignalAction {
    Default,
    Ignore,
    Handler {
        entry: u64,      // userspace handler function address
        mask: u64,        // additional signals to block during handler
        flags: u64,       // SA_RESTORER, SA_ONSTACK, etc.
        restorer: u64,    // address of __restore_rt trampoline
    },
}
```

`SignalAction` is the per-signal stored configuration. The kernel resolves
it into a `SignalDisposition` at delivery time, which collapses `Default`
into one of `Terminate`, `Stop`, `Continue`, or `Ignore` based on the
signal number, and maps `Handler` to `UserHandler`.

## Sigframe Layout

When a signal has a user handler, the kernel pushes a **sigframe** onto
the user stack (or alternate signal stack). The layout matches the Linux
`rt_sigframe` structure that musl expects:

```
high addresses (original user RSP)
+-----------------------------------+
|       alignment padding           |
+-----------------------------------+ <-- frame_rsp (new RSP for handler)
| pretcode        [8 bytes]         |  restorer address (__restore_rt)
| uc_flags        [8 bytes]         |  0
| uc_link         [8 bytes]         |  0
| uc_stack        [24 bytes]        |  stack_t: ss_sp, ss_flags, ss_size
| uc_mcontext     [256 bytes]       |  sigcontext: saved GPRs (see below)
| uc_sigmask      [128 bytes]       |  saved blocked-signal mask
| siginfo_t       [128 bytes]       |  si_signo + zeroed fields
+-----------------------------------+
low addresses
              Total: 560 bytes
```

The `uc_mcontext` (sigcontext) contains the full interrupted register
state at these offsets from the start of mcontext:

| Offset | Register | Offset | Register |
|--------|----------|--------|----------|
| 0      | r8       | 64     | rdi      |
| 8      | r9       | 72     | rsi      |
| 16     | r10      | 80     | rbp      |
| 24     | r11      | 88     | rbx      |
| 32     | r12      | 96     | rdx      |
| 40     | r13      | 104    | rax      |
| 48     | r14      | 112    | rcx      |
| 56     | r15      | 120    | rsp      |
|        |          | 128    | rip      |
|        |          | 136    | rflags   |

The `pretcode` field at `[RSP+0]` acts as the handler's return address.
When the handler executes `ret`, the CPU pops `pretcode` into `RIP`,
jumping to the `__restore_rt` trampoline which invokes `sigreturn`.

### Stack Alignment

The frame RSP is computed as:

```rust
let frame_rsp = (base_rsp - SIGFRAME_SIZE as u64) & !15u64;
let frame_rsp = frame_rsp - 8;
```

The extra 8-byte subtraction satisfies the System V AMD64 ABI: at a
`CALL` instruction, `RSP % 16 == 8`. Since the handler is entered as if
called (with `pretcode` as the return address at `[RSP]`), `RSP` itself
must be 16-byte aligned minus 8. Misalignment causes SSE faults in musl
library code.

## Signal Delivery Decision Tree

Signal delivery is checked on **every return to ring 3** from a syscall.
The function `check_pending_signals()` runs in a loop, dequeuing one
signal at a time:

```
syscall_handler() returns
         |
         v
check_pending_signals()
         |
         v
dequeue_signal(pid)
    |              \
    | None          \ Some(signum, disposition)
    v                v
  return        +----------+----------+-----------+-------------+
    to          |          |          |           |             |
  user      Terminate   Stop     Continue/    UserHandler
                |          |      Ignore         |
                v          v        |            v
           sys_exit()   set state   |     deliver_user_signal()
                        Stopped,    |       1. read_saved_user_regs()
                        loop until  |       2. update blocked_signals
                        resumed     |       3. setup_signal_frame()
                                    |       4. enter_signal_handler()
                                    |              |
                                    v              v
                                  (done)     iretq to handler
                                             RIP=handler, RSP=frame,
                                             RDI=signum
```

For user handlers, `deliver_user_signal()` is a **divergent** function --
it never returns to the normal syscall return path. Instead it builds the
sigframe and enters ring 3 directly via `iretq`.

### Blocking Syscall Interruption

Blocking syscalls (`read`, `waitpid`, `nanosleep`, pipe I/O) periodically
call `has_pending_signal()` during their yield loops. This function checks
whether any pending, unblocked signal has a non-`Ignore` disposition:

```rust
fn has_pending_signal() -> bool {
    let deliverable = proc.pending_signals & !proc.blocked_signals;
    // ... check each deliverable signal's disposition is not Ignore
}
```

When it returns `true`, the blocking syscall returns `EINTR` (or its
negated form), and the subsequent `check_pending_signals()` call delivers
the signal.

## sigreturn Mechanism

`sigreturn` (syscall 15) is the **only** way to correctly restore the
interrupted context. A normal function return from the handler would
unwind the C call stack, losing the saved register state. `sigreturn`
reads the sigframe from the user stack and restores every register to
its pre-signal value.

The flow:

1. The handler finishes and executes `ret`.
2. `ret` pops `pretcode` (the restorer address) from the stack into `RIP`.
3. The restorer stub (`__restore_rt`) executes `mov $15, %rax; syscall`.
4. The kernel's `sys_sigreturn` reads `user_rsp`, computes the sigframe
   location (`user_rsp - 8`, since `ret` already popped pretcode), and
   calls `restore_sigframe()`.
5. `restore_sigframe()` reads all GPRs from `uc_mcontext` and the saved
   signal mask from `uc_sigmask`.
6. The kernel restores `blocked_signals` from the saved mask (clearing
   SIGKILL/SIGSTOP bits), clears `SS_ONSTACK` if applicable.
7. The kernel validates that the restored `RIP` and `RSP` are canonical
   userspace addresses (below `0x0000_8000_0000_0000`).
8. `restore_and_enter_userspace()` builds an `iretq` frame, loads all
   GPRs from the `SavedUserRegs` struct, and executes `iretq`.
9. The process resumes at the exact instruction that was interrupted,
   with all registers (including `RAX`) restored to their pre-signal
   values.

### RFLAGS Sanitization

Before `iretq`, the kernel sanitizes the restored RFLAGS to prevent
privilege escalation via a crafted sigframe:

```rust
const PRIV_MASK: u64 =
    (1 << 12) | (1 << 13) |  // IOPL
    (1 << 14) |              // NT
    (1 << 17) |              // VM
    (1 << 19) | (1 << 20) |  // VIF, VIP
    (1 << 21);               // ID
let rflags = (regs.rflags & !PRIV_MASK) | 0x202;  // force IF + reserved bit 1
```

`sigreturn` is divergent -- it does not return a value. The restored
`RAX` is whatever the interrupted code had in `RAX` before the signal,
not a syscall return value.

## SA_RESTORER Contract with musl

The kernel does not embed a signal trampoline in user memory. Instead,
it relies on the C library (musl) to provide one via the `SA_RESTORER`
mechanism:

1. When musl calls `rt_sigaction`, it sets the `SA_RESTORER` flag in
   `sa_flags` and writes the address of its `__restore_rt` stub into the
   `sa_restorer` field.
2. The kernel stores `restorer` in `SignalAction::Handler`.
3. At signal delivery, the kernel writes `restorer` into the sigframe's
   `pretcode` field (offset 0), which sits at `[RSP]` when the handler
   is entered.
4. When the handler returns via `ret`, execution jumps to `__restore_rt`.
5. musl's `__restore_rt` is:
   ```asm
   mov $15, %rax    ; SYS_sigreturn
   syscall
   ```

If `SA_RESTORER` is not set, the kernel stores `restorer = 0` and logs
a warning. The handler will fault on return (jumping to address 0),
making the bug immediately visible.

The `rt_sigaction` struct layout (Linux x86_64, 32 bytes):

| Offset | Field | Size |
|--------|-------|------|
| 0      | `sa_handler` | 8 bytes |
| 8      | `sa_flags` | 8 bytes |
| 16     | `sa_restorer` | 8 bytes |
| 24     | `sa_mask` | 8 bytes |

The kernel rejects handler addresses and restorer addresses that point
into kernel space (above `0x0000_8000_0000_0000`).

## Signal Masking Lifecycle

Signal masking prevents re-entrant delivery of the same signal during
handler execution. The lifecycle:

```
1. INSTALL HANDLER
   rt_sigaction(SIGUSR1, handler, sa_mask={SIGUSR2})
   → signal_actions[10] = Handler { mask=SIGUSR2, ... }

2. SIGNAL ARRIVES
   send_signal(pid, SIGUSR1)
   → pending_signals |= (1 << 10)

3. DELIVERY (in deliver_user_signal)
   a. Save old blocked_signals into sigframe's uc_sigmask
   b. blocked_signals |= sa_mask | (1 << signum)
      → blocks SIGUSR1 (auto) + SIGUSR2 (sa_mask)
   c. Clear SIGKILL/SIGSTOP from blocked_signals
   d. Push sigframe, enter handler

4. DURING HANDLER
   Any SIGUSR1 or SIGUSR2 sent to the process stays pending
   (blocked by the mask set in step 3b)

5. SIGRETURN
   a. Read uc_sigmask from sigframe
   b. Restore blocked_signals to the saved value
   c. Clear SIGKILL/SIGSTOP bits
   → SIGUSR1 and SIGUSR2 are unblocked again
   → Any pending signals are delivered on the next
     check_pending_signals() call
```

### rt_sigprocmask

Userspace can directly manipulate the blocked-signal mask:

```rust
match how {
    SIG_BLOCK   => proc.blocked_signals |= set,     // add signals
    SIG_UNBLOCK => proc.blocked_signals &= !set,    // remove signals
    SIG_SETMASK => proc.blocked_signals = set,       // replace entirely
}
proc.blocked_signals &= !UNBLOCKABLE_MASK;  // enforce SIGKILL/SIGSTOP
```

After `SIG_UNBLOCK` or `SIG_SETMASK`, the kernel calls
`check_pending_signals(0)` to immediately deliver any signals that
became unblocked.

### Bit Indexing Convention

The kernel uses signal-number-indexed bits: bit N represents signal N.
musl uses 0-indexed bits: bit N represents signal N+1. The kernel
converts between the two by shifting left/right by 1 at the
`rt_sigprocmask` and `rt_sigaction` boundaries.

## Unblockable Signals

SIGKILL (9) and SIGSTOP (19) cannot be blocked, caught, or ignored.
This is enforced at three points:

1. **`rt_sigaction`** returns `EINVAL` if `sig` is SIGKILL or SIGSTOP.
2. **`rt_sigprocmask`** always clears bits 9 and 19 after any mask
   modification:
   ```rust
   const UNBLOCKABLE_MASK: u64 =
       (1u64 << SIGKILL) | (1u64 << SIGSTOP);
   proc.blocked_signals &= !UNBLOCKABLE_MASK;
   ```
3. **`dequeue_signal`** forces default disposition for SIGKILL and
   SIGSTOP even if the action table says `Ignore` or `Handler`.

## SIGCHLD Delivery on Child Exit

When a child process exits or is killed by a signal, the kernel sends
SIGCHLD to its parent:

```rust
pub fn send_sigchld_to_parent(child_pid: Pid) {
    let ppid = {
        let table = PROCESS_TABLE.lock();
        table.find(child_pid).map(|p| p.ppid).unwrap_or(0)
    };
    if ppid != 0 {
        send_signal(ppid, SIGCHLD);
    }
}
```

This is called from two locations:

- **`sys_exit()`** -- when a process terminates normally or via
  `exit_group`.
- **`check_pending_signals()`** -- when a process is killed by a
  signal's default Terminate action, and when a process is stopped by
  SIGSTOP/SIGTSTP (so the parent's `waitpid` with `WUNTRACED` wakes).

SIGCHLD's default disposition is `Ignore`, so it does not terminate the
parent. If the parent has installed a SIGCHLD handler (e.g., to call
`waitpid` and reap children), the handler is delivered via the normal
sigframe path. If the parent is blocked in `waitpid`, the pending
SIGCHLD causes `has_pending_signal()` to return `false` (since Ignore
disposition is filtered out), so `waitpid` is not spuriously interrupted
-- it wakes naturally when it finds a zombie child.

## Alternate Signal Stack (sigaltstack)

A process can register an alternate stack for signal handlers via
`sigaltstack` (syscall 131). This is essential for handling SIGSEGV
caused by stack overflow -- the default stack is already exhausted, so
the sigframe cannot be pushed there.

Process fields:

| Field | Default | Purpose |
|-------|---------|---------|
| `alt_stack_base` | 0 | Base address of the alt stack |
| `alt_stack_size` | 0 | Size in bytes (must be >= `MINSIGSTKSZ` = 2048) |
| `alt_stack_flags` | 0 | `SS_DISABLE` (2) or `SS_ONSTACK` (1) |

During signal delivery, the kernel checks whether to use the alt stack:

```rust
if sa_flags & SA_ONSTACK != 0
    && proc.alt_stack_base != 0
    && proc.alt_stack_flags & SS_DISABLE == 0
    && proc.alt_stack_flags & SS_ONSTACK == 0
{
    proc.alt_stack_flags |= SS_ONSTACK;
    alt_rsp = proc.alt_stack_base + proc.alt_stack_size;
}
```

The `SS_ONSTACK` flag prevents recursive use of the alt stack. It is
cleared by `sigreturn`. While `SS_ONSTACK` is set, `sigaltstack()`
returns `EPERM` if the caller tries to change the alt stack.

## Test Coverage

The `userspace/signal-test/signal-test.c` binary validates four aspects:

1. **Basic handler**: Install a SIGINT handler, `raise(SIGINT)`, verify
   the handler flag is set and execution continues after `raise`.
2. **Signal masking**: Block SIGUSR1, send it to self, verify it is NOT
   delivered, unblock, verify it IS delivered immediately.
3. **Uncatchable signals**: `sigaction(SIGKILL)` and `sigaction(SIGSTOP)`
   both return error.
4. **Auto-masking**: A handler that `raise()`s the same signal during
   execution does not re-enter; the second delivery is deferred until
   after `sigreturn` restores the mask.

## Limitations

The following are not implemented in this phase:

- FPU/SSE/AVX state is not saved in the sigframe (no `XSAVE`)
- Real-time signals (`SIGRTMIN`--`SIGRTMAX`) and `sigqueue`
- `SA_NODEFER` (parsed but not honored -- signal is always auto-masked)
- `SA_RESETHAND` (parsed but not honored -- handler is not reset to default)
- `SA_RESTART` (blocked syscalls return `EINTR`, not auto-restarted)
- Per-thread signal masks (requires `clone`)
- `signalfd`
