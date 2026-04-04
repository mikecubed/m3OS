# Sunset SSH Library — Local Fork Documentation

**Location:** `sunset-local/` (forked from `sunset` v0.4.0 on crates.io)
**Workspace reference:** `Cargo.toml` → `sunset = { path = "sunset-local", default-features = false }`
**Related phase:** Phase 43 (SSH Server), Phase 42b+ (Async Executor)

## Why a Local Fork Exists

The [sunset](https://crates.io/crates/sunset) SSH library (v0.4.0) is an IO-less,
`no_std`-compatible SSH-2 protocol engine designed for embedded systems. It provides
key exchange, encryption, authentication callbacks, and channel multiplexing without
performing any I/O itself — the application feeds bytes in and reads bytes out.

m3OS uses sunset as the protocol engine for `sshd` (Phase 43). As of Phase 42b+, the
sshd session handler uses a multi-task cooperative async executor (`async-rt`) that
matches sunset's intended `sunset-async` architecture: separate I/O, progress, and
channel relay tasks sharing the Runner behind an async Mutex.

**Only one patch remains:** the SSH window/packet size configuration (Patch 2).

## Patch 1: BadUsage Recovery — ELIMINATED

**Status:** Removed in Phase 42b+ (multi-task async executor).

The BadUsage error occurred because the single-task event loop could not guarantee
that event resume handlers were called before the next `progress()` call. Sunset's
internal `Drop`-based resume handling would fire for events like `SessionPty` before
our handler could process them, leaving `resume_event` in a stale state.

The multi-task executor eliminates this by matching `sunset-async`'s architecture:
- The progress task acquires the async Mutex, calls `progress()`, handles the event,
  and calls the resume method — all within a single lock scope.
- The Mutex is released only after the event is fully handled.
- When `progress()` is called again (after re-acquiring the Mutex), `resume_event`
  is always clean.

The original upstream code (`return error::BadUsage.fail()`) has been restored.

## Patch 2: SSH Window and Packet Size in `config.rs`

**Status:** Retained — sole remaining reason for the local fork.

### The Problem

Sunset's default configuration uses very small SSH channel windows and maximum
packet sizes:

```rust
// upstream config.rs
pub const DEFAULT_WINDOW: usize = 1000;
pub const DEFAULT_MAX_PACKET: usize = 1000;
```

These values are negotiated during SSH channel open. With a 1000-byte window, the
channel cannot accommodate the request/response flow for realistic SSH sessions —
OpenSSH's terminal modes blob alone can be several hundred bytes. The small window
also limits data relay throughput to 1000 bytes in-flight.

### The Fix

```rust
// sunset-local/src/config.rs
pub const DEFAULT_WINDOW: usize = 32000;
pub const DEFAULT_MAX_PACKET: usize = 32000;
```

32 KB is sufficient for interactive SSH sessions and provides reasonable throughput
for the data relay between the PTY and the encrypted channel.

### Impact

Without this change, the SSH client reports `rwindow 1000 rmax 1000` during channel
open, and the session may stall or drop packets under normal interactive use.

## sshd Architecture (Phase 42b+)

The sshd session handler uses a three-task async architecture:

```rust
pub fn run_session(sock_fd: i32, host_key: &HostKey) -> i32 {
    let mut reactor = Reactor::new();
    async_rt::block_on(&mut reactor, async_session(sock_fd, host_key))
}
```

Three spawned tasks share the Runner via `Rc<async_rt::sync::Mutex<Runner>>`:

1. **I/O task** — reads socket data, feeds to `runner.input()`, flushes
   `runner.output_buf()` to socket. Uses reactor for socket readiness.
2. **Progress task** — loops calling `runner.progress()`, handles all SSH events
   (auth, channel open, PTY, shell) within the Mutex lock scope.
3. **Channel relay task** — relays data between PTY and `runner.read_channel()`/
   `runner.write_channel()`. Spawned when shell is established.

### Why Three Tasks Eliminates BadUsage

The key is that events from `progress()` borrow from the Runner, which is behind
the Mutex. The progress task:

```rust
let mut guard = mutex.lock().await;
match guard.progress() {
    Ok(Event::Serv(ServEvent::SessionPty(pty_req))) => {
        pty_req.succeed();  // resume called WITHIN the lock scope
    }
    // ...
}
// guard dropped here — lock released with clean resume_event
```

The event's resume method is called before the MutexGuard is dropped. When the
progress task re-acquires the Mutex and calls `progress()` again, `resume_event`
is always clean — no BadUsage.

## What Would Need to Change to Remove the Fork

### Eliminating Patch 2 (Window/Packet Size)

The `DEFAULT_WINDOW` and `DEFAULT_MAX_PACKET` constants are not configurable through
sunset's public API. To remove this patch:

1. **Request upstream configurability.** Expose these as `Runner::new_server_with_config(...)`
   parameters or feature-gated constants.
2. **Use a `larger` feature flag.** Sunset has a `larger` feature that increases some
   limits. A similar feature could gate larger window/packet defaults.
3. **Accept the 1000-byte limit.** Not viable for real SSH sessions.

## Summary of Dependencies

| Change | Type | Status | Required for |
|---|---|---|---|
| BadUsage recovery | sunset patch | **Eliminated** (Phase 42b+) | Was needed before multi-task executor |
| Window size 32000 | sunset config | Retained | Reasonable SSH throughput |
| Lazy PTY alloc | sshd workaround | Retained | PTY works when SessionPty event is missed |
| Pending data buffers | sshd I/O task | Retained | Runner input buffer may not accept full read |
| 200ms poll timeout | sshd pattern | **Eliminated** (Phase 42b) | Was needed for manual poll loop |
| Break-after-resume | sshd pattern | **Eliminated** (Phase 42b+) | Was needed in single-task loop |
| Flush before progress | sshd pattern | **Eliminated** (Phase 42b+) | I/O task handles flushing independently |
