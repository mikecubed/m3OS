# Phase 52a — Kernel Reliability Fixes: Task List

**Status:** Complete
**Source Ref:** phase-52a
**Depends on:** Phase 52 (First Service Extractions) ✅
**Goal:** Fix four confirmed kernel bugs discovered during Phase 52 service extraction and bug investigations.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | IPC blocking return state fix | None | Complete |
| B | Sunset write waker fix | None | Already Fixed (pre-52a) |
| C | Thread exit clear_child_tid | None | Already Fixed (pre-52a) |
| D | Exec signal action reset | None | Complete |

---

## Track A — Fix Stale `syscall_user_rsp` on IPC Blocking Paths

### A.1 — Add `restore_caller_context` to IPC dispatch

**File:** `kernel/src/ipc/mod.rs`
**Symbol:** `dispatch`
**Why it matters:** All seven IPC blocking syscalls (recv, call, reply_recv, notify_wait, call_buf, recv_msg, reply_recv_msg) return with a stale per-core `syscall_user_rsp` after a context switch. This is the confirmed root cause of wrong user RSP on return from blocking IPC.

**Acceptance:**
- [x] `dispatch()` captures `saved_user_rsp = per_core_syscall_user_rsp()` and `pid = current_pid()` at entry
- [x] `dispatch()` calls `restore_caller_context(pid, saved_user_rsp)` before every blocking return path (syscalls 1, 3, 5, 7, 14, 15, 16)
- [x] On SMP4, `cargo xtask regression --test ipc-wake` passes (unix-socket-test exercises overlapping IPC send/recv/call/reply cycles)
- [x] The `copy_to_user` reliability bug investigation doc is updated with the fix status

### A.2 — Add futex WAIT restore (same pattern)

**File:** `kernel/src/arch/x86_64/syscall/mod.rs`
**Symbol:** `sys_futex` (FUTEX_WAIT path)
**Why it matters:** The futex WAIT path has the same structural hole — it blocks via `block_current_on_futex_unless_woken()` and returns without an adjacent `restore_caller_context()`. This was identified in the `copy_to_user` investigation as a parallel defect.

**Acceptance:**
- [x] FUTEX_WAIT path captures `saved_user_rsp` before blocking and calls `restore_caller_context` after wakeup
- [x] All three regression tests pass on SMP4 (`cargo xtask regression`: fork-overlap, ipc-wake, pty-overlap)

---

## Track B — Fix Sunset `Channel::wake_write()` Bug

### B.1 — Correct `wake_write()` waker field

**File:** `sunset-local/src/channel.rs`
**Symbol:** `Channel::wake_write`
**Why it matters:** `wake_write()` calls `self.read_waker.take()` instead of `self.write_waker.take()` for `ChanData::Normal`. This causes PTY output to stall under channel backpressure — the SSH session appears hung until a client keystroke "nudges" output through.

**Acceptance:**
- [x] `Channel::wake_write()` uses `self.write_waker.take()` for the `ChanData::Normal` arm (verified: already correct at line 842)
- [x] SSH session displays shell prompt after authentication without any client keystroke (verified via expect + QEMU SMP4)
- [x] The SSHD hang analysis doc (`docs/appendix/sshd-hang-analysis.md`) already contains historical note marking pre-fix status

> **Note:** B.1 was already correct in the checked-in code. The bug described in the analysis docs was fixed before Phase 52a.

### B.2 — Verify relay task write waker registration

**File:** `userspace/sshd/src/session.rs`
**Symbol:** `channel_relay_task`
**Why it matters:** The relay task must register `set_channel_write_waker()` when `pty_pending_len > 0`. This was previously missing but may have been partially fixed. Verify the registration is correct and complete.

**Acceptance:**
- [x] When `write_channel` returns `Ok(0)` and `pty_pending_len > 0`, the relay task calls `set_channel_write_waker` before sleeping (verified: registered at session.rs:877 in the should_wait block when pty_pending_len > 0)
- [ ] Large shell output (e.g., `cat` a 100-line file) over SSH completes without stalling (expect pattern matching issues with SSH escape sequences prevented automated verification)

> **Note:** B.2 was already implemented. The should_wait block at line 874-880 checks `pty_pending_len > 0` and registers `set_channel_write_waker` before sleeping.

---

## Track C — Fix `clear_child_tid` on Thread Exit

### C.1 — Implement clear_child_tid cleanup in exit path

**Files:**
- `kernel/src/arch/x86_64/syscall/mod.rs`
- `kernel/src/process/mod.rs`

**Symbol:** `sys_exit` or process exit path
**Why it matters:** `Process.clear_child_tid` is populated by `CLONE_CHILD_CLEARTID` but the exit path does not write 0 to that address and wake the futex. musl's `pthread_join` hangs indefinitely.

**Acceptance:**
- [x] When a thread with `clear_child_tid != 0` exits, the kernel writes `0u32` to that address via `copy_to_user` (verified: `do_clear_child_tid` at syscall/mod.rs:1643-1683)
- [x] The kernel calls `futex_wake(clear_child_tid, 1)` after the write (verified: wakes one futex waiter with FUTEX_BITSET_MATCH_ANY)
- [x] `thread-test` binary uses `CLONE_CHILD_CLEARTID` + `futex_wait` for join (line 149, 194-201) — exercises this exact path
- [x] `thread-test` passes on SMP4: ALL TESTS PASSED (3/3: basic create/join, futex mutex stress, thread exit)

> **Note:** C.1 was already fully implemented via `do_clear_child_tid()` called from `sys_exit` at line 1743. No code changes needed. The `thread-test` binary is a direct regression test for this functionality.

---

## Track D — Clear Signal Actions on Exec

### D.1 — Reset `Handler` dispositions to `Default` in `sys_execve`

**File:** `kernel/src/arch/x86_64/syscall/mod.rs`
**Symbol:** `sys_execve`
**Why it matters:** POSIX requires that caught signals (those with `Handler` disposition) are reset to `Default` on exec. m3OS does not do this, so an exec'd program can inherit unexpected signal handlers.

**Acceptance:**
- [x] After ELF loading and before `enter_userspace`, `sys_execve` iterates `signal_actions` and resets any `Handler` disposition to `Default`
- [x] `Ignore` and `Default` dispositions are preserved (not reset)
- [x] `signal-test` passes on SMP4: 4 passed, 0 failed (sigint_handler, signal_masking, uncatchable, auto_masking)

---

## Documentation Notes

- All four fixes originate from the Phase 52 bug investigations documented in:
  - `docs/appendix/copy-to-user-reliability-bug.md`
  - `docs/appendix/sshd-hang-analysis.md`
  - `docs/appendix/redox-copy-to-user-comparison.md`
- The architectural analysis in `docs/appendix/architecture/current/` documents the exact code paths and data structures involved
- The proposed long-term solutions are in `docs/appendix/architecture/next/`
- Track A is a stop-gap; Phase 52b replaces the manual `restore_caller_context` pattern with task-owned return state
