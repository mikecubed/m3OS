# Phase 52a - Kernel Reliability Fixes

**Status:** Planned
**Source Ref:** phase-52a
**Depends on:** Phase 52 (First Service Extractions) ✅
**Builds on:** Addresses four confirmed kernel bugs discovered during Phase 52 service extraction work — all are direct blockers for the copy_to_user reliability investigation and SSHD hang analysis
**Primary Components:** kernel/src/ipc/mod.rs, kernel/src/arch/x86_64/syscall/mod.rs, kernel/src/process/mod.rs, sunset-local/src/channel.rs, userspace/sshd/src/session.rs

## Milestone Goal

All four confirmed kernel bugs from the Phase 52 bug investigations are fixed, with targeted regression tests proving each fix.

## Why This Phase Exists

Phase 52's service extractions exposed two independent bug investigations:

1. **The `copy_to_user` intermittent reliability bug** (documented in `docs/appendix/copy-to-user-reliability-bug.md`) — kernel writes correct data but userspace reads stale/zeroed values. Investigation traced the root cause to an earlier address-space mapping divergence and confirmed a stale `syscall_user_rsp` bug on all IPC blocking paths.

2. **The SSHD post-authentication hang** (documented in `docs/appendix/sshd-hang-analysis.md`) — the SSH relay task stalls when the channel encounters write backpressure, due to a vendored library bug in sunset's `Channel::wake_write()`.

Both investigations also found secondary bugs: missing `clear_child_tid` on thread exit (breaks musl `pthread_join`) and missing signal action reset on exec.

These are not design improvements — they are confirmed bugs with known fixes. They must land before any structural hardening work.

## Learning Goals

- Understand why per-core mutable scratch for syscall return state is fragile
- Learn how blocking IPC paths interact with the scheduler's per-core state overwriting
- See how a single-line waker bug can stall an entire async I/O pipeline
- Understand the POSIX contracts for `pthread_join` (CLONE_CHILD_CLEARTID) and exec signal reset

## Feature Scope

### Fix stale `syscall_user_rsp` on IPC blocking paths

All six IPC-family blocking syscalls (recv, call, reply_recv, notify_wait, recv_msg, reply_recv_msg) do not call `restore_caller_context()` after waking from a block. After a context switch, the per-core `syscall_user_rsp` is overwritten by whichever task ran on this core. SYSRETQ then returns to userspace with a wrong user RSP.

**Fix:** Add `saved_user_rsp` capture at IPC dispatch entry and `restore_caller_context(pid, saved_user_rsp)` before returning from the dispatch function.

### Fix sunset `Channel::wake_write()` bug

`sunset-local/src/channel.rs:840-845` — `Channel::wake_write()` calls `self.read_waker.take()` instead of `self.write_waker.take()` for normal data. This means channel backpressure-cleared events wake the wrong task.

**Fix:** Change `self.read_waker.take()` to `self.write_waker.take()` in the `ChanData::Normal` arm of `Channel::wake_write()`.

### Fix `clear_child_tid` on thread exit

`Process.clear_child_tid` is populated by `CLONE_CHILD_CLEARTID` but the exit path does not write 0 to that address and wake the futex. musl's `pthread_join` relies on this to detect thread completion.

**Fix:** In the thread exit path, if `clear_child_tid != 0`: write `0u32` to that address via `copy_to_user`, then call `futex_wake(clear_child_tid, 1)`.

### Clear signal actions on exec

`sys_execve` does not reset `Handler` signal dispositions to `Default`. POSIX requires this — an exec'd program must not inherit custom signal handlers from its predecessor.

**Fix:** In `sys_execve`, after building the new page table, iterate `signal_actions` and reset any `Handler` disposition to `Default`.

## Important Components and How They Work

### IPC dispatch restore

The IPC dispatch function in `kernel/src/ipc/mod.rs` is the single entry point for all IPC syscalls. It calls `endpoint::recv()`, `endpoint::call()`, `notification::wait()`, etc., which can block via `block_current_on_*()`. The fix adds `restore_caller_context` around the entire dispatch, covering all six blocking paths with a single change.

### Sunset write waker

The `channel_relay_task` in `userspace/sshd/src/session.rs` registers `set_channel_write_waker()` when PTY output is pending but the channel is full. The sunset library's `Channel::wake_write()` should fire this waker when channel space becomes available (after `consume_output()` or `ChannelWindowAdjust`). The bug causes the read waker to fire instead, which doesn't retry the pending write.

### Thread exit cleanup

The `clear_child_tid` field stores a userspace address provided at `clone(CLONE_CHILD_CLEARTID)`. On thread exit, the kernel writes 0 to this address and wakes any futex waiters. musl's `pthread_join` blocks on a `futex_wait` at this address, checking for the 0 value.

## How This Builds on Earlier Phases

- Fixes bugs discovered during Phase 52 (First Service Extractions)
- The IPC stale-return-state bug was confirmed during the `copy_to_user` investigation documented in `docs/appendix/copy-to-user-reliability-bug.md`
- The SSHD hang was confirmed during the Phase 43 SSH server work and documented in `docs/appendix/sshd-hang-analysis.md`
- The `clear_child_tid` bug affects Phase 40 (Threading) functionality
- The exec signal reset affects Phase 19 (Signal Handlers) + Phase 11 (Process Model) interaction

## Implementation Outline

1. Add `restore_caller_context` to `kernel/src/ipc/mod.rs` dispatch function
2. Fix `Channel::wake_write()` in `sunset-local/src/channel.rs`
3. Add `clear_child_tid` cleanup to thread exit in `kernel/src/arch/x86_64/syscall/mod.rs`
4. Add signal action reset to `sys_execve` in `kernel/src/arch/x86_64/syscall/mod.rs`
5. Write targeted regression tests for each fix

## Acceptance Criteria

- IPC blocking syscalls (recv, call, reply_recv, notify_wait) return with correct user RSP on SMP4
- SSH session displays shell prompt without requiring a client keystroke after authentication
- musl `pthread_join` completes (does not hang) when a CLONE_CHILD_CLEARTID thread exits
- exec'd program does not inherit `Handler` signal dispositions from its predecessor
- `cargo xtask check` passes with all fixes
- Each fix has a corresponding entry in the `copy_to_user` or SSHD hang investigation doc showing it resolves the identified issue

## Companion Task List

- [Phase 52a Task List](./tasks/52a-kernel-reliability-fixes-tasks.md)

## How Real OS Implementations Differ

- Linux's syscall return path restores all user state from the `pt_regs` structure on the kernel stack — there is no per-core mutable scratch that needs manual restoration
- Linux's `exit_mm()` and `exit_signal()` properly handle `clear_child_tid` and signal reset as standard parts of the exit/exec code paths
- Production SSH libraries (OpenSSH, libssh) use multi-process or multi-threaded models rather than cooperative async, avoiding the single-threaded waker coordination problem entirely

## Deferred Until Later

- Moving `syscall_user_rsp` to task-owned state (Phase 52b structural hardening)
- Typed `UserBuffer` wrappers to make `copy_to_user` auditable (Phase 52b)
- AddressSpace object for the underlying mapping divergence (Phase 52b)
