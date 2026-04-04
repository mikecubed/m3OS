# Sunset SSH Library — Local Fork Documentation

**Location:** `sunset-local/` (forked from `sunset` v0.4.0 on crates.io)
**Workspace reference:** `Cargo.toml` → `sunset = { path = "sunset-local", default-features = false }`
**Related phase:** Phase 43 (SSH Server), Phase 42b (Async Executor)

## Why a Local Fork Exists

The [sunset](https://crates.io/crates/sunset) SSH library (v0.4.0) is an IO-less,
`no_std`-compatible SSH-2 protocol engine designed for embedded systems. It provides
key exchange, encryption, authentication callbacks, and channel multiplexing without
performing any I/O itself — the application feeds bytes in and reads bytes out.

m3OS uses sunset as the protocol engine for `sshd` (Phase 43). As of Phase 42b, the
sshd session handler uses a cooperative async executor (`async-rt`) to drive I/O
readiness and sunset event processing. This eliminated the need for the BadUsage
recovery patch (Patch 1), which was required by the previous synchronous event loop.

**Only one patch remains:** the SSH window/packet size configuration (Patch 2).

## Patch 1: BadUsage Recovery — ELIMINATED

**Status:** Removed in Phase 42b (async executor refactor).

The BadUsage error occurred because the synchronous event loop violated sunset's
expected async sequencing — `resume_event` remained set between `progress()` calls
due to timing interactions with sunset's internal payload state machine. The async
executor drives `progress()` and I/O as cooperating tasks with proper yield points,
matching sunset's intended usage pattern. The `resume_event` stickiness no longer
occurs because:

1. The session yields to the executor between I/O and event processing.
2. FDs are set to non-blocking, preventing the event loop from blocking mid-sequence.
3. The reactor-driven poll replaces the manual 200ms poll timeout.

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

### Decision (Phase 42b)

**Keep the 32KB window size.** This is a throughput/correctness setting independent
of async vs sync. The fork remains solely for this configuration change. Options
considered:

- **Keep 32KB (chosen):** Fork stays for this single config change.
- **Upstream via Config API:** Would require sunset to expose window size as a
  configurable parameter. No such API exists in v0.4.0.
- **Accept 1KB:** Not viable — sessions stall or fail with realistic SSH clients.

### Impact

Without this change, the SSH client reports `rwindow 1000 rmax 1000` during channel
open, and the session may stall or drop packets under normal interactive use.

## sshd Architecture (Phase 42b)

The sshd session handler uses the `async-rt` executor:

```rust
pub fn run_session(sock_fd: i32, host_key: &HostKey) -> i32 {
    let mut reactor = Reactor::new();
    executor::block_on(&mut reactor, async_session(sock_fd, host_key))
}
```

The async session:
- Sets socket and PTY FDs to non-blocking via `set_nonblocking()`.
- Registers FDs with the reactor for read readiness.
- Yields to the executor between I/O and event processing.
- Eliminates the manual poll() call, pending data buffers, and 200ms timeout.

### Remaining Workarounds

#### Lazy PTY Allocation at Shell Request

The `SessionPty` event may still be consumed by sunset's internal `Drop`-based
resume handling before reaching our handler. The PTY is allocated at `SessionShell`
time as a fallback. This workaround is independent of async/sync.

#### Flush Before Every Progress Call

```rust
flush_output(&mut runner, sock_fd);
match runner.progress() { ... }
```

Sunset needs output buffer space for protocol responses. This pattern remains even
with the async executor.

## What Would Need to Change to Remove the Fork

### Eliminating Patch 2 (Window/Packet Size)

The `DEFAULT_WINDOW` and `DEFAULT_MAX_PACKET` constants are not configurable through
sunset's public API. To remove this patch:

1. **Request upstream configurability.** Expose these as `Runner::new_server_with_config(...)`
   parameters or feature-gated constants.
2. **Use a `larger` feature flag.** Sunset has a `larger` feature that increases some
   limits. A similar feature could gate larger window/packet defaults.
3. **Accept the 1000-byte limit.** Not viable for real SSH sessions.

### Eliminating the Lazy PTY Workaround

The lazy PTY allocation exists because `SessionPty` may not be delivered. This could
be fixed by pre-allocating the PTY at channel open time, but that means the PTY is
always allocated even for non-PTY sessions.

## Summary of Dependencies

| Change | Type | Status | Required for |
|---|---|---|---|
| BadUsage recovery | sunset patch | **Eliminated** (Phase 42b) | Was needed for sync event loop |
| Window size 32000 | sunset config | **Retained** | Reasonable SSH throughput |
| Lazy PTY alloc | sshd workaround | Retained | PTY works when event is missed |
| Break-after-resume | sshd pattern | Retained (simplified) | Correct event lifecycle |
| Flush before progress | sshd pattern | Retained | Output reaches client |
| Pending data buffers | sshd pattern | **Eliminated** (Phase 42b) | Was needed for sync backpressure |
| 200ms poll timeout | sshd pattern | **Eliminated** (Phase 42b) | Was needed for manual poll loop |
