# SSHD Multi-Task Architecture — Debug Handoff Document

**Date**: 2026-04-04
**Branch**: `docs/phase-43-task-list`
**Status**: SSH sessions complete KEX, auth, PTY allocation, and shell spawn. Shell runs correctly (ion executes, PROMPT works). But shell output never reaches the SSH client — the session appears to hang after authentication.

## What Works

- Key exchange (curve25519-sha256, chacha20-poly1305) completes successfully
- Password and public key authentication work
- PTY pair is allocated (kernel confirms `[INFO] [pty] allocated PTY pair`)
- Shell child process forks and execves `/bin/ion` successfully
- Ion runs, executes PROMPT, writes to PTY slave
- The sunset BadUsage recovery patch has been removed — `progress()` returns the upstream `error::BadUsage.fail()` and no BadUsage errors occur (the multi-task Mutex pattern prevents them)
- 53 async-rt host tests pass
- `cargo xtask check` passes clean (clippy, formatting, kernel-core tests, all userspace builds)

## What Doesn't Work

Shell output (written to PTY slave by ion) never reaches the SSH client. The client sees the connection succeed and PTY allocated, then hangs waiting for data. Eventually the client times out and disconnects (TCP FIN). Pressing a key on the client occasionally "nudges" data through, suggesting a wakeup/scheduling issue rather than a fundamental data path error.

## Architecture

Three async tasks within a single-threaded cooperative executor (`async-rt::block_on`), sharing the sunset `Runner` via `Rc<async_rt::sync::Mutex<Runner>>`:

### Task 1: I/O Task (`io_task`)
- Flushes `runner.output_buf()` to the TCP socket
- Reads TCP socket data, feeds to `runner.input()`
- Registers sunset's `set_output_waker` so it wakes when output is available
- Registers socket fd with reactor for read readiness
- After wakeup: flushes output first, then checks socket readability with `poll(fd, 0)` before blocking read

### Task 2: Progress Task (`progress_task`)
- Loops calling `runner.progress()`, handling SSH events within the Mutex lock scope
- Event resume methods (allow/reject/succeed/fail) called before MutexGuard is dropped
- Returns `ProgressAction` enum to defer post-lock work (shell spawn, relay spawn)
- Yields via `yield_once()` on `Event::None`

### Task 3: Channel Relay Task (`channel_relay_task`)
- Spawned after shell is established
- Direction 1: `runner.read_channel()` → `write_all(pty_fd, data)` (client keystrokes → shell)
- Direction 2: `read(pty_fd)` → `runner.write_channel()` → `flush_output_locked()` (shell output → client)
- Registers sunset's `set_channel_read_waker` to wake on channel data
- Registers PTY fd with reactor for read readiness
- Uses `WaitReadable { fd: pty_fd }` to block between iterations

### Executor
- `block_on` main loop: poll spawned tasks, poll root, `reactor.poll_once(0)` (non-blocking I/O check every iteration), `reactor.poll_once(100)` (blocking if nothing runnable)
- `spawn()` returns `JoinHandle<T>`, tasks stored in a slab allocator

## The Data Path That Fails

```
ion shell → writes to PTY slave → PTY master becomes readable
    → relay task reads PTY master (read returns data)
    → relay task calls runner.write_channel(data)
        → sunset immediately generates SSH packet (send_packet)
        → sunset calls self.wake() → fires output_waker
    → relay task calls flush_output_locked()
        → locks runner, calls output_buf() → should have packet data
        → copies to temp buf, drops lock, writes to socket
    → SSH packet reaches client
```

## What Has Been Tried

### Fix 1: Remove progress() calls from I/O task
**Commit**: `689dd41`
**Problem**: The I/O task called `runner.progress()` when `input()` returned 0. This discarded events whose Drop handlers fired with default reject behavior, causing stale `resume_event`.
**Result**: Didn't fix the hang (but was a real bug — the I/O task should never call progress).

### Fix 2: Non-blocking reactor poll every executor iteration
**Commit**: `e378aa7`
**Problem**: `yield_once()` immediately re-wakes tasks, so there was always a runnable task. The executor's `reactor.poll_once()` was gated on `run_queue.is_empty() && !root_woken`, which was never true. The reactor was never polled, so I/O-waiting tasks (WaitReadable) were never woken.
**Result**: Fixed the KEX hang. SSH now completes handshake, auth, PTY, shell. But shell output still doesn't reach the client.

### Fix 3: Channel relay registers both PTY and channel wakers
**Commits**: `30c79d6`, `b206827`, `6a8bab0`
**Problem**: The relay task only waited on PTY readability, not channel data availability. Client keystrokes in the channel buffer were never read because the relay task slept on PTY-only.
**Iterations**:
1. `WaitPtyOrChannel` with `try_lock_mutex` — unreliable, skipped waker registration when mutex contended
2. Replaced with `get_current_waker()` + `runner.lock().await` + `set_channel_read_waker()` — guaranteed registration
**Result**: Shell child now reaches execve consistently. But output still doesn't flow to client.

### Fix 4: Close all inherited fds in shell child
**Commit**: `b206827`
**Problem**: Shell child inherited reactor self-pipe fds and other executor state.
**Result**: Child now execves reliably. Not the cause of the output hang.

### Fix 5: I/O task registers sunset's output_waker
**Commit**: `004046f`
**Problem**: The I/O task only woke on socket readability (incoming client data), not on runner output availability. After `write_channel` generates an SSH packet, nobody woke the I/O task to flush it.
**Changes**: I/O task calls `set_output_waker` before waiting. After wakeup, flushes output first, then checks socket with `poll(fd, 0)` before blocking read.
**Result**: Still hanging. This is the most recent fix and hasn't resolved the issue.

## Hypotheses Not Yet Tested

### H1: write_channel output not visible in output_buf without intervening progress()
Sunset's `write_channel` calls `traf_out.send_packet()` directly — verified by reading the source (runner.rs:503). The packet should be in `output_buf()` immediately. But there might be an internal state issue where `output_buf()` doesn't return the new packet until something else happens (e.g., a `progress()` call advances internal state).

**How to test**: Add a debug print in the relay task after `write_channel`: lock runner, call `output_buf()`, print its length. If it's 0, the packet isn't being generated as expected.

### H2: flush_output_locked is re-acquiring the mutex and finding output_buf empty
Between `write_channel` (which generates the packet) and `flush_output_locked` (which reads it), the relay task drops the guard and re-acquires. Another task (progress) could acquire the mutex in between and call `progress()`, which might consume or move the output data internally.

**How to test**: Combine write_channel and output_buf reads in a single lock acquisition. Don't drop the guard between write_channel and output_buf.

### H3: The output_waker fires but the I/O task's WaitReadable doesn't resolve
The output_waker calls `waker.wake()` which sets the task's `woken` flag and writes to the self-pipe. But `WaitReadable { fd: sock_fd }` registered the socket fd with the reactor. When the self-pipe is written, the reactor's `poll()` returns, but the socket fd might not have `revents` set. The `WaitReadable` future checks `self.registered` (a boolean) — if `registered` is true, it returns `Ready(())`. Since the task was woken (by output_waker), it IS re-polled, and `registered` is true from the first poll, so it returns Ready. This should work.

**However**: the executor's `poll_once(0)` non-blocking check might drain the self-pipe before the I/O task is polled. If the self-pipe byte is consumed by drain_wake_pipe() but the I/O task hasn't been re-queued yet... Let me check: `requeue_woken()` scans all tasks' `woken` flags. The output_waker set the I/O task's woken flag. So requeue_woken should find it. This should work.

### H4: The async Mutex has a fairness issue
If the progress task and relay task keep acquiring the mutex in a pattern that starves the I/O task's flush_output_locked, the output never gets written. The async Mutex uses FIFO waker ordering, so this shouldn't happen. But if the I/O task's waker registration on the output_waker is one-shot (sunset takes the waker via `.take()`) and the I/O task doesn't re-register after flushing...

**Likely issue**: sunset's `set_output_waker` stores the waker, and `wake()` calls `self.output_waker.take()` — it's consumed after one fire. The I/O task registers the waker once per loop iteration (before WaitReadable). But after the waker fires and the I/O task runs and flushes, it goes back to the top of the loop, re-registers, and waits again. This should be fine IF the I/O task actually reaches the re-registration point.

### H5: The relay task's flush_output_locked works but the data never reaches the kernel socket send buffer
The `write_all_count` function calls `syscall_lib::write(sock_fd, data)`. If the kernel's TCP stack has an issue (e.g., Nagle's algorithm with small packets, or a send buffer that doesn't flush), the data might be buffered in the kernel.

**How to test**: Check if the kernel's TCP implementation flushes small writes. The old single-task sshd worked, so this is unlikely unless the write pattern changed (smaller, more frequent writes due to the multi-task architecture).

### H6: Fundamental architecture mismatch — sunset needs a different task split
The sunset-async crate uses a slightly different architecture than what we implemented. In sunset-async, the I/O and progress are more tightly coupled — there's a `progress_notify` signal that coordinates them. Our three-task split might be too loosely coupled.

**How to test**: Look at sunset-async's source on GitHub (`https://github.com/mkj/sunset`, the `async/` directory) and compare the task coordination pattern.

### H7: The simplest possible test — go back to single-task
Revert to the working single-task session (pre-multi-task, with BadUsage patch) and verify it still works. If it does, the issue is definitively in the multi-task architecture, not in a kernel regression.

## Recommended Next Steps

1. **H7 first**: Verify the old single-task code still works on the current kernel build. This rules out kernel regressions.

2. **H2**: Combine write_channel and flush in a single lock scope in the relay task. Don't drop the guard between them. This tests whether the output disappears between lock acquisitions.

3. **Add debug logging**: In the relay task, after write_channel succeeds, immediately check output_buf().len() (while still holding the guard) and print it. In flush_output_locked, print how many bytes were found and written.

4. **H6**: Read sunset-async's actual source and compare the task interaction pattern. We might be missing a coordination mechanism.

## Key Files

| File | Purpose |
|---|---|
| `userspace/sshd/src/session.rs` | The multi-task session handler |
| `userspace/async-rt/src/executor.rs` | Multi-task executor with spawn/block_on |
| `userspace/async-rt/src/sync/mutex.rs` | Async Mutex with FIFO waiters |
| `userspace/async-rt/src/reactor.rs` | Poll-based reactor with self-pipe |
| `sunset-local/src/runner.rs` | Sunset Runner — progress(), write_channel, output_buf |
| `sunset-local/src/channel.rs` | Channel waker fields and wake methods |
| `.sdd/async-executor-production-r7k3m2p1/` | Full spec, plan, and task list |

## Environment

- Branch: `docs/phase-43-task-list`
- Latest commit: `004046f`
- Test: `ssh -v root@10.0.2.15` from host (QEMU with port forwarding)
- 53 async-rt host tests passing
- `cargo xtask check` clean
