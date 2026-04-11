# Kernel Reliability Fixes

**Aligned Roadmap Phase:** Phase 52a
**Status:** Complete
**Source Ref:** phase-52a
**Supersedes Legacy Doc:** (none -- new content)

## Overview

Phase 52a addresses four confirmed kernel bugs discovered during the Phase 52
service extraction work. These are not design improvements -- they are direct
correctness fixes for stale IPC return state, a vendored SSH library waker bug,
missing POSIX `clear_child_tid` on thread exit, and missing signal action reset
on exec. All four bugs blocked either the `copy_to_user` reliability
investigation or the SSHD post-authentication hang.

## What This Doc Covers

- Stale `syscall_user_rsp` on IPC blocking paths and why per-core mutable
  scratch is fragile
- The sunset `Channel::wake_write()` waker misroute and its effect on SSH
  session liveness
- POSIX `clear_child_tid` contract and musl `pthread_join` dependency
- Signal disposition reset on exec (SIG_DFL for caught signals)

## Core Implementation

### Stale IPC return state

All six IPC blocking syscalls (recv, call, reply_recv, notify_wait, recv_msg,
reply_recv_msg) enter the scheduler via `block_current_on_*()`. After the
context switch, whatever task ran on this core overwrites the per-core
`syscall_user_rsp`. The original IPC caller's user RSP is lost.

The fix adds `restore_caller_context(pid, saved_user_rsp)` at the IPC dispatch
boundary -- a single call site covering all six blocking paths. The saved user
RSP is captured at dispatch entry, before any blocking can occur.

### Sunset write waker bug

`sunset-local/src/channel.rs` line ~840 -- `Channel::wake_write()` calls
`self.read_waker.take()` instead of `self.write_waker.take()` for the
`ChanData::Normal` arm. When channel output space becomes available after
backpressure, the read waker fires instead of the write waker, so the pending
write is never retried. The SSH relay task stalls permanently.

The fix is a one-line change: `self.read_waker.take()` to
`self.write_waker.take()`.

### clear_child_tid on thread exit

`clone(CLONE_CHILD_CLEARTID)` stores a userspace address in
`Process.clear_child_tid`. POSIX requires the kernel to write `0u32` to this
address and wake the futex on thread exit. musl's `pthread_join` blocks on a
`futex_wait` at that address. Without the write + wake, `pthread_join` hangs.

The fix adds the write-zero-and-wake to the thread exit path in the syscall
handler.

### Signal action reset on exec

POSIX requires that all caught signal dispositions (`Handler`) are reset to
`Default` on exec. Without this, an exec'd program inherits the previous
program's custom signal handlers, which point to unmapped code addresses in the
new process image.

The fix iterates `signal_actions` in `sys_execve` after the new page table is
built and resets all `Handler` entries to `Default`.

## Key Files

| File | Purpose |
|---|---|
| `kernel/src/ipc/mod.rs` | IPC dispatch with `restore_caller_context` at the boundary |
| `kernel/src/arch/x86_64/syscall/mod.rs` | `clear_child_tid` cleanup and signal reset on exec |
| `sunset-local/src/channel.rs` | Write waker fix in vendored SSH library |
| `userspace/sshd/src/session.rs` | SSH relay task that depends on correct waker behavior |

## How This Phase Differs From Later Work

- This phase fixes specific bugs. Phase 52b eliminates the structural patterns
  that made those bugs possible (task-owned return state, typed UserBuffers).
- The `restore_caller_context` fix is a band-aid; Phase 52b removes the need
  for it entirely by moving return state into the Task struct.
- Thread exit cleanup here is minimal. A full POSIX-compliant exit sequence
  would also handle process groups, controlling terminals, and file lock release.

## Related Roadmap Docs

- [Phase 52a roadmap doc](./roadmap/52a-kernel-reliability-fixes.md)
- [Phase 52a task doc](./roadmap/tasks/52a-kernel-reliability-fixes-tasks.md)
- [Phase 52 -- First Service Extractions](./52-first-service-extractions.md)
- [copy_to_user reliability bug investigation](./appendix/copy-to-user-reliability-bug.md)

## Deferred or Later-Phase Topics

- Task-owned return state (Phase 52b -- eliminates the fragile per-core scratch)
- Typed UserBuffer wrappers (Phase 52b -- makes copy auditing structural)
- AddressSpace object for mapping tracking (Phase 52b)
