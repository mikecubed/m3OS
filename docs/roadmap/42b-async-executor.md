# Phase 42b — Async Executor

**Status:** Complete
**Source Ref:** phase-42b
**Depends on:** Phase 37 (I/O Multiplexing) ✅, Phase 43 (SSH Server) ✅
**Builds on:** Adds a userspace cooperative single-threaded async runtime on top of Phase 37's `poll(2)` infrastructure and the Phase 43 sshd. The runtime is what makes the Phase 43 sshd correct: before 42b, sshd's session loop blocked on synchronous reads and the upstream `sunset-local` SSH library had to be patched to avoid waiting on a separate task that the kernel never woke. After 42b, the session loop is `async` and futures yield control whenever I/O is not ready.
**Primary Components:** `userspace/async-rt/` (new crate: `executor.rs`, `reactor.rs`, `task.rs`, `io.rs`, `sync/`, `slab.rs`, `yield.rs`), `userspace/syscall-lib/src/lib.rs` (`poll`, `PollFd`, `fcntl`, `set_nonblocking`, `O_NONBLOCK`), `userspace/sshd/src/session.rs` (refactored to async), `sunset-local/` (forked SSH library — fork patches removed)

## Milestone Goal

Userspace gains a real `async`/`await` story so I/O-bound services can multiplex many file descriptors on a single thread without spawning per-FD tasks or blocking on synchronous syscalls. The first consumer is `sshd`, whose session loop becomes a single `async fn` that runs under `executor::block_on`. The runtime is small (one binary's worth of code, no third-party crates), pure in the sense that the `Waker`, `Task`, and `Reactor` types are host-testable, and discoverable in a single crate at `userspace/async-rt/`.

## Why This Phase Exists

Phase 43 shipped sshd by patching `sunset` (the upstream Rust SSH library) to remove its async assumptions. The patch worked, but it carried real cost:

1. **The fork was permanent.** Every upstream sunset improvement required re-applying our patches.
2. **Window size was wrong.** Sunset's default channel window (`DEFAULT_WINDOW = 32 KB`) was kept inside our fork even though the patch route had no concurrency, which led to "stuck on banner" bugs over the e1000 NIC under MTU pressure.
3. **The session loop was synchronous.** Every `read` or `write` blocked the whole connection. Two SSH sessions could not interleave even though the kernel had `poll(2)` shipping since Phase 37.

The 42b design fixes the root cause: build the async runtime sunset already expects, then refactor sshd to use it. Once sshd is async, the sunset fork patches can be reverted to upstream behaviour. `sunset-local/src/runner.rs` ends the phase with the upstream `BadUsage` path intact, and `docs/appendix/sunset-local-fork.md` records the disposition for future updates.

## Learning Goals

- Understand how Rust's `Future` trait, `Waker`, and `Context` map onto a single-threaded executor — no thread pool, no work-stealing.
- See how a `RawWakerVTable` lets a future re-arm itself by writing one byte into a wake-pipe FD that the executor's outer loop reads.
- Learn the readiness model: the reactor watches FDs with `poll(2)`; futures register interest, get back a `Waker`, and yield until the FD becomes ready.
- Understand why a tiny runtime is a better dependency for a teaching kernel than `tokio`: the whole pipeline (waker → task → reactor → executor → io) is ~1000 lines of `no_std`-compatible Rust with host-runnable unit tests.
- Learn how to refactor a synchronous I/O loop into `async` without redesigning the protocol layer — most of the sshd changes are at the I/O boundary, not in the SSH state machine itself.

## Feature Scope

### Syscall-lib wrappers (Track A)

Add the syscall surface the reactor needs: `poll`, `fcntl(F_GETFL/F_SETFL)`, `set_nonblocking`, and the `O_NONBLOCK` constant. These extend Phase 37's `poll(2)` to userspace as ergonomic Rust wrappers (`PollFd { fd, events, revents }`).

### `async-rt` crate skeleton (Track A)

A dedicated `userspace/async-rt/` crate, dual-target (kernel `no_std` for the executor itself, plus host `std` for tests via feature flags). Modules: `task.rs`, `reactor.rs`, `executor.rs`, `io.rs`, `sync/`, `slab.rs`, `yield.rs`.

### Waker and Task (Track B)

A custom `RawWakerVTable` whose `wake` impl writes one byte to the executor's wake-pipe FD (`WAKE_PIPE_FD`). Tasks are stored in a `slab::Slab` keyed by index; each `TaskHeader` carries a future, a state byte, and a back-reference to the executor for waker construction.

### Reactor (Track C)

`Reactor::poll_once` calls `poll(2)` with the registered `PollFd` set; for each ready FD it wakes the associated waker. `Reactor::register_read`/`register_write`/`deregister` mutate the FD set.

### Executor (Track D)

`executor::block_on(future)` is the entry point. It owns the wake-pipe pair, the reactor, and the task slab. The outer loop polls the root future, drains the wake-pipe, polls the reactor, and re-polls woken tasks. `executor::spawn(future)` adds a non-root task to the slab.

### `AsyncFd` and pollable I/O (Track E)

`AsyncFd::new(fd)` wraps a non-blocking FD. Its `readable()` / `writable()` futures register interest with the reactor and yield until the FD is ready. `ReadableFuture` / `WritableFuture` are the public types.

### sshd refactor (Track F)

`userspace/sshd/src/session.rs::run_session` becomes a thin shim that calls `executor::block_on(async_session(...))`. The session loop sets sockets and PTY masters non-blocking, wires `AsyncFd` against the FDs sunset cares about, and uses sunset's input/output/channel-read wakers (`set_input_waker`, `set_output_waker`, `set_channel_read_waker`) so sunset's protocol state machine can drive the loop's progress.

### Sunset fork elimination (Track G)

With the session loop async, the sunset fork patches can be reverted. `sunset-local/src/runner.rs` matches upstream's `BadUsage` form. `sunset-local/src/config.rs` keeps `DEFAULT_WINDOW = 32000` and `DEFAULT_MAX_PACKET = 32000` as a deliberate non-default override (32 KB matches our typical e1000 MTU x N path). The `xtask ssh-e1000-banner-check` subcommand and `ssh_overlap_steps` regression test cover the previous "stuck on banner" failure mode.

## Important Components and How They Work

### `executor::block_on` — the outer loop

`block_on` builds a wake-pipe (a pair of FDs created via `pipe(2)` and set non-blocking), constructs a `Reactor`, and inserts the root future as task index 0. It polls the root future once. If the future is `Pending`, `block_on` enters its loop:

1. `read(WAKE_PIPE_FD, scratch_buf)` — drains any wake bytes accumulated since the last poll.
2. `reactor.poll_once()` — calls `poll(2)` on the registered FD set; for each ready FD, calls the associated `Waker::wake_by_ref()` (which writes a byte back to the wake-pipe).
3. For each woken task index, `Pin<&mut F>::poll(&mut cx)` — re-polls that task. If the root task returns `Ready(value)`, `block_on` returns `value`. Spawned tasks that return `Ready` are removed from the slab.

### `task::TaskHeader` and `header_waker`

`TaskHeader` is a `#[repr(C)]` struct with `state: AtomicU8`, `executor: NonNull<Executor>`, `slab_index: u32`, and the boxed future. `header_waker` builds a `RawWaker` whose `wake` impl writes the slab index byte to the executor's wake-pipe. The `RawWakerVTable` is `static` in `task.rs` and host-tested in the same file's `#[cfg(test)]` module.

### `reactor::Reactor` — `poll(2)` adapter

`Reactor::register_read(fd, waker)` records `(fd, POLLIN, waker)`. `register_write` is symmetric for `POLLOUT`. `deregister` removes by FD. `poll_once` builds a transient `Vec<PollFd>` from the recorded entries, calls `syscall_lib::poll(&mut entries, /*timeout=*/0)`, and walks the result vector to wake the associated wakers.

### `io::AsyncFd` — readiness future

`AsyncFd` holds the FD. `AsyncFd::readable()` returns `ReadableFuture { fd, registered: false }`. `ReadableFuture::poll` first attempts a non-blocking `read` of zero bytes (or peek); if the FD is ready, returns `Ready(())`. Otherwise registers with the reactor (`reactor.register_read(fd, cx.waker().clone())`) and returns `Pending`. `WritableFuture` mirrors this for writes.

### sunset waker integration

Sunset's protocol state machine exposes `set_input_waker(Waker)`, `set_output_waker(Waker)`, and `set_channel_read_waker(Waker)`. The 42b `async_session` wires the executor's `cx.waker().clone()` into each of these so sunset can re-arm the session loop when its internal state advances. This is the contract the sunset fork patches removed in Phase 43; the 42b runtime restores it.

## How This Builds on Earlier Phases

- **Phase 37 (I/O Multiplexing).** Phase 37 shipped `sys_poll` with the `POLLIN`/`POLLOUT`/`POLLERR` event mask. 42b's reactor is a thin `poll(2)` adapter — it does not require new kernel work.
- **Phase 43 (SSH Server).** 42b refactors sshd's session loop to async. The protocol layer (sunset) is unchanged; only the I/O boundary moves. After 42b, the sunset fork patches are reverted to upstream behaviour.
- **Phase 12 (POSIX Compat).** `fcntl(F_GETFL/F_SETFL)` and `O_NONBLOCK` are POSIX surface; 42b adds the userspace wrappers in `syscall-lib`.

## Implementation Outline

1. Add `PollFd`, `poll`, `fcntl`, `set_nonblocking` to `userspace/syscall-lib/src/lib.rs`.
2. Create `userspace/async-rt/` crate with the module skeleton (`task.rs`, `reactor.rs`, `executor.rs`, `io.rs`, `sync/`, `slab.rs`, `yield.rs`).
3. Implement `task::TaskHeader` + `header_waker` + `WAKE_PIPE_FD` + `set_wake_pipe_fd`. Add host-runnable unit tests for the vtable.
4. Implement `reactor::Reactor` (register/deregister/poll_once).
5. Implement `executor::block_on` and `executor::spawn`. Add a `#[cfg(test)]` test that spawns two futures, has each yield twice, and confirms both complete.
6. Implement `io::AsyncFd::{readable, writable}` and the `ReadableFuture` / `WritableFuture` types.
7. Refactor `userspace/sshd/src/session.rs::run_session` to call `executor::block_on(async_session(...))`. Set sockets and PTY masters non-blocking. Wire `AsyncFd` against sunset's FDs and use `set_input_waker`/`set_output_waker`/`set_channel_read_waker` to drive the loop.
8. Revert the sunset fork patches (`sunset-local/src/runner.rs` back to upstream `BadUsage` form). Keep `DEFAULT_WINDOW = 32000` and `DEFAULT_MAX_PACKET = 32000` as a deliberate config override.
9. Add `xtask ssh-e1000-banner-check` and `ssh_overlap_steps` regression test to lock in the previous "stuck on banner" failure mode.
10. Update `docs/appendix/sunset-local-fork.md` to document the post-42b state of the fork.

## Acceptance Criteria

- `userspace/async-rt/` crate builds with `cargo xtask check` (clippy + fmt).
- Host-runnable tests pass with `cargo test -p async-rt --features std`.
- `sshd` boots, accepts at least two concurrent SSH sessions, and serves them through one async session loop per connection.
- The Phase 43 `sunset-local` fork has no patches applied (all reverted).
- The "stuck on banner" failure no longer reproduces under `cargo xtask ssh-e1000-banner-check`.
- The `ssh_overlap_steps` regression test passes (two interleaved SSH sessions reach a shell prompt).

## Companion Task List

- [Phase 42b Task List](./tasks/42b-async-executor-tasks.md)

## How Real OS Implementations Differ

Production async runtimes — `tokio`, `async-std`, Glommio, monoio — ship work-stealing thread pools, dedicated `epoll` / `kqueue` / `io_uring` integrations, timer wheels, channel implementations, and per-task heap-allocated states. They sit on top of fully featured I/O subsystems with edge-triggered readiness, vectored I/O, and zero-copy paths. m3OS's runtime is intentionally less ambitious: single-threaded, level-triggered through `poll(2)`, no timer support, no channels, no per-task allocator. The teaching value is that the moving parts of an async runtime are surprisingly simple once they are written from scratch — the complexity in production runtimes comes from performance and breadth, not from the core idea.

## Deferred Until Later

- Timers and `sleep` futures — sshd does not need them; if a later phase does, add a `Timer` type that registers an absolute deadline with the reactor and wakes when reached (would require a kernel `clock_gettime` call per `poll_once`, currently shipping).
- `epoll` / `io_uring` integration — currently the reactor uses `poll(2)`, which is O(N) in registered FDs. With ~10 FDs per sshd session this is invisible; if a later phase needs hundreds of concurrent FDs, a switch to edge-triggered `epoll` is the natural next step.
- Multi-threaded executor — single-threaded is sufficient for sshd; multi-threaded would require thread-safe wakers, work-stealing, and per-thread reactors.
- Channel and select! macro support — userspace IPC happens through file-descriptor-backed sockets; channels are not needed yet.
- `async fn` in trait implementations — sunset's existing trait surface is synchronous; 42b uses `async fn` only at the session-loop level.
