# Sunset SSH Library — Local Fork Documentation

**Location:** `sunset-local/` (forked from `sunset` v0.4.0 on crates.io)
**Workspace reference:** `Cargo.toml` → `sunset = { path = "sunset-local", default-features = false }`
**Related phase:** Phase 43 (SSH Server)

## Why a Local Fork Exists

The [sunset](https://crates.io/crates/sunset) SSH library (v0.4.0) is an IO-less,
`no_std`-compatible SSH-2 protocol engine designed for embedded systems. It provides
key exchange, encryption, authentication callbacks, and channel multiplexing without
performing any I/O itself — the application feeds bytes in and reads bytes out.

m3OS uses sunset as the protocol engine for `sshd` (Phase 43). However, two issues
in the upstream library prevent correct operation in our synchronous, single-threaded
event loop. Both are configuration/behavioral issues that cannot be resolved through
the public API, requiring source-level patches.

## Patch 1: BadUsage Recovery in `runner.rs`

### The Problem

Sunset's `Runner::progress()` method maintains internal state (`resume_event`) that
tracks which SSH protocol event was last returned to the application. Each event that
"needs resume" (authentication, channel open, PTY request, shell request, etc.) must
have its resume handler called (e.g., `pw_auth.allow()`, `pty_req.succeed()`) before
the next `progress()` call. If `progress()` finds a stale `resume_event`, it returns
`Error::BadUsage` and aborts the session.

In sunset's intended async usage (via `sunset-async`), I/O and progress run as
separate concurrent tasks with mutex-protected access. The async executor naturally
yields between event handling and the next `progress()` call, ensuring the resume
handler completes and the output is flushed before `progress()` runs again.

In m3OS's synchronous event loop, the sshd session handler calls `progress()` in a
loop after feeding socket data. Despite correctly calling resume handlers (allow,
reject, accept, succeed, fail) and flushing output between calls, `BadUsage` fires
persistently after password authentication succeeds. Extensive diagnostic logging
confirmed:

1. The resume handler (`pw_auth.allow()`) IS called and succeeds.
2. The `resume_event` IS cleared by `resume_servauth()` via `take()`.
3. The next `progress()` call nevertheless finds `resume_event` set to a `ServEvent`
   that `needs_resume()` — specifically `Environment` (the SSH env channel request).

The root cause appears to be a timing interaction between sunset's internal payload
state machine and the synchronous call pattern. When the SSH client sends multiple
channel requests in a single TCP segment (env, pty-req, shell), sunset's `traf_in`
buffers them. After processing one event and calling its resume handler (which calls
`done_payload()`), the next `progress()` call processes the subsequent buffered
packet. However, the internal `resume_event` field ends up set from a previous event
that was handled by its `Drop` impl rather than by our explicit code path. The
`Drop` impl calls the default resume handler (e.g., `resume_chanreq(false)` for
rejection), but does not clear `resume_event` through the same code path as explicit
handler calls.

### The Fix

```rust
// sunset-local/src/runner.rs, line 293-300
// BEFORE (upstream):
let prev = self.resume_event.take();
if prev.needs_resume() {
    debug!("No response provided to {:?} event", prev);
    return error::BadUsage.fail();
}

// AFTER (m3OS patch):
let mut prev = self.resume_event.take();
if prev.needs_resume() {
    debug!("Recovering from unhandled {:?} event", prev);
    prev = DispatchEvent::None;
}
```

Instead of aborting the session, the patched code clears the stale `resume_event`
and continues. Setting `prev = DispatchEvent::None` is critical — it prevents the
subsequent `if prev.is_event() { self.traf_in.done_payload(); }` check (line 308)
from consuming the next pending packet's payload.

### Impact

Without this patch, every SSH session aborts with `BadUsage` after authentication,
before the channel/PTY/shell phase can begin. With the patch, the session continues
through channel open, environment requests, PTY allocation, and shell spawning.

The `SessionPty` event is still lost due to a related internal state issue (the
pty-req packet's `Drop` impl fires before our handler receives it). This is worked
around in `session.rs` by allocating the PTY lazily at `SessionShell` time if it
wasn't already allocated during `SessionPty` handling.

## Patch 2: SSH Window and Packet Size in `config.rs`

### The Problem

Sunset's default configuration uses very small SSH channel windows and maximum
packet sizes:

```rust
// upstream config.rs
pub const DEFAULT_WINDOW: usize = 1000;
pub const DEFAULT_MAX_PACKET: usize = 1000;
```

These values are negotiated during SSH channel open. The OpenSSH client sends
`pty-req`, `env`, and `shell` channel requests immediately after channel confirmation.
With a 1000-byte window, the channel cannot accommodate the request/response flow
for realistic SSH sessions — OpenSSH's terminal modes blob alone can be several
hundred bytes.

The small window also limits the data relay throughput: only 1000 bytes of shell
output can be in-flight before the client must acknowledge receipt.

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

## Affected sshd Workarounds

The following workarounds in `userspace/sshd/src/session.rs` exist because of
sunset's behavior, independent of the patches:

### Lazy PTY Allocation at Shell Request

```rust
// session.rs — SessionShell handler
if pty_master.is_none() {
    if let Ok((m, s)) = syscall_lib::openpty() {
        pty_master = Some(m);
        pty_slave = Some(s);
    }
}
```

The `SessionPty` event is frequently not delivered to our handler because sunset's
internal `Drop`-based resume handling consumes it before we can match it. The PTY is
allocated at `SessionShell` time as a fallback.

### Inner Loop with Break-After-Resume

```rust
loop {
    flush_output(&mut runner, sock_fd);
    match runner.progress() {
        Ok(Event::Serv(ServEvent::PasswordAuth(pw_auth))) => {
            // ... handle auth ...
            let _ = pw_auth.allow();
            break;  // MUST break after any resume call
        }
        Ok(Event::Serv(ServEvent::SessionEnv(env_req))) => {
            let _ = env_req.fail();
            continue;  // Env uses continue (no break needed)
        }
        // ... other events with break ...
        Ok(Event::Serv(ServEvent::PollAgain) | Event::Progressed) => continue,
        Ok(Event::None) => break,
        Ok(_) => break,
        Err(_) => break,  // Recoverable — retry after I/O
    }
}
```

- **Break after resume calls**: After calling `allow()`, `reject()`, `accept()`,
  `succeed()`, or `fail()`, we break to the outer loop to flush output and feed
  new socket data before the next `progress()` call.
- **Continue for env requests**: The `SessionEnv` handler uses `continue` because
  env requests arrive between the channel open and PTY/shell requests.
- **Err as recoverable**: `progress()` errors (including residual `BadUsage` from
  the patched recovery) break to the outer loop for retry rather than aborting.

### Flush Before Every Progress Call

```rust
flush_output(&mut runner, sock_fd);
match runner.progress() { ... }
```

The `flush_output()` before each `progress()` ensures sunset's output buffer is
drained to the TCP socket before processing new packets. Sunset needs output buffer
space to queue protocol responses (auth success, channel confirmations, etc.), and
the remote peer may be waiting for these responses before sending subsequent packets.

## What Would Need to Change to Remove the Fork

To return to upstream `sunset = "0.4"` from crates.io without the local fork, both
the sshd session handler and possibly the sunset library itself need changes.

### Eliminating Patch 1 (BadUsage Recovery)

The BadUsage error fires because `resume_event` is set from a previous `progress()`
call and not cleared before the next call. Our resume handlers (allow/reject/etc.)
DO call `resume_event.take()` via their internal `resume_*` functions, yet the flag
persists. The investigation narrowed the stuck event to `ServEventId::Environment`
(the LANG env channel request).

To fix this without patching sunset, we would need to determine **why** `resume_event`
remains set after our `env_req.fail()` call. The most promising lines of investigation:

1. **Verify `resume_chanreq` completes successfully.** Our `env_req.fail()` calls
   `runner.resume_chanreq(false)` which does `self.resume_event.take()` at line 780.
   If `resume_chanreq` returns `Err` (e.g., `traf_in.payload()` returns `None`),
   `resume_event` was already cleared but `done_payload()` is NOT called, leaving the
   payload pending for re-processing by the next `progress()`. Fixing this requires
   understanding why `payload()` would return `None` when sunset just returned the
   event from that payload.

2. **Audit the `Drop` impl interaction.** The `ServEnvironmentRequest::Drop` impl
   calls `resume_chanreq(false)` if `done` is false. If `fail()` is called (setting
   `done = true`) but then the event is re-created by a subsequent `progress()` call
   processing the same payload, the `Drop` of the new event would call
   `resume_chanreq` again — but `resume_event` is already `None`, causing confusion.

3. **Implement a proper async executor.** Sunset's API was designed for concurrent
   I/O + progress tasks. The `sunset-async` crate runs `progress()` in one task and
   I/O in another, with mutex-protected access to the `Runner`. An async executor
   (even a simple cooperative one using `core::task::Waker`) would match sunset's
   expected usage pattern and likely eliminate the BadUsage entirely. This is the
   cleanest long-term solution.

4. **Use `sunset-embassy` or `sunset-async` directly.** If m3OS adds an async runtime
   (e.g., a simple poll-based executor), the `sunset-async` wrapper handles all the
   I/O/progress coordination and event lifecycle automatically.

### Eliminating Patch 2 (Window/Packet Size)

The `DEFAULT_WINDOW` and `DEFAULT_MAX_PACKET` constants are not configurable through
sunset's public API — they are compile-time constants in `config.rs`. To remove this
patch:

1. **Request upstream configurability.** The sunset author could expose these as
   `Runner::new_server_with_config(...)` parameters or feature-gated constants.

2. **Use the `larger` feature flag.** Sunset has a `larger` feature that increases
   some limits (e.g., `MAX_USERNAME` from 31 to 256). A similar feature could gate
   larger window/packet defaults. This does not currently exist for window size.

3. **Accept the 1000-byte limit.** The session works with 1000-byte windows but
   interactive throughput is very low. Commands with large output (like `ls -la` in
   a large directory) would be noticeably slow due to frequent flow-control pauses.

### Eliminating the Lazy PTY Workaround

The lazy PTY allocation at `SessionShell` time exists because `SessionPty` is not
delivered to our handler. To fix this properly:

1. **Solve the BadUsage root cause.** If the `resume_event` stickiness is fixed,
   all events (including `SessionPty`) should be delivered in order: `OpenSession` →
   `SessionPty` → `SessionEnv` → `SessionShell`.

2. **Use `ssh -o RequestTTY=no` for testing.** This tells the client not to request
   a PTY, which avoids the `SessionPty` event entirely. Useful for non-interactive
   commands (`ssh host command`) but not for interactive shells.

3. **Pre-allocate the PTY at channel open time.** Instead of waiting for `SessionPty`,
   allocate the PTY when `OpenSession` is accepted. This sidesteps the event ordering
   issue entirely but means the PTY is always allocated even for non-PTY sessions.

## Summary of Dependencies

| Change | Type | Required for | Removable by |
|---|---|---|---|
| BadUsage recovery | sunset patch | Session survives past auth | Fixing root cause or async executor |
| Window size 32000 | sunset config | Reasonable SSH throughput | Upstream configurability |
| Lazy PTY alloc | sshd workaround | PTY works when event is missed | Fixing BadUsage root cause |
| Break-after-resume | sshd pattern | Correct event lifecycle | Async executor |
| Flush before progress | sshd pattern | Output reaches client | Would remain even with async |
| Err-as-recoverable | sshd pattern | Session survives transient errors | Fixing BadUsage root cause |

## Future Considerations

- **Upstream contribution**: The BadUsage recovery and larger default window could
  be proposed as upstream changes to `sunset`. The recovery is safe (the event's
  `Drop` impl already called the default resume handler) and the window size is more
  practical for real SSH clients.
- **Async integration**: If m3OS gains an async executor in the future, the
  synchronous workarounds could be replaced with `sunset-async`, which handles the
  I/O/progress coordination naturally through concurrent tasks.
- **SessionPty event delivery**: The root cause of why `SessionPty` is consumed by
  its `Drop` impl before reaching our handler remains unresolved. The lazy PTY
  allocation workaround is functional but means the client sees "PTY allocation
  request failed" even though the session works correctly.
