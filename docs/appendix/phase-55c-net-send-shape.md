# Phase 55c Track G — Net-Send Shape Decision (resend)

**Status:** Decided and implemented  
**Scope:** G.1 (design), G.2 (tests), G.3 (implementation)  
**Decided shape:** `sys_net_send` — new dedicated syscall, `kernel/src/syscall/net.rs`

---

## Chosen Shape: `sys_net_send` (new syscall, number `0x1013`)

### What it does

`sys_net_send(sock_fd: u64, buf_ptr: u64, len: u64) -> u64`

1. **Socket capability boundary** (arch dispatcher): resolves `sock_fd` against
   the calling process's fd table (`current_fd_entry`).  The call proceeds only
   when the fd entry is `FdBackend::Socket`; anything else returns `NEG_EBADF`.
   This preserves the existing socket-fd validation model used by `sys_sendto`.
2. Copies `len` bytes from userspace address `buf_ptr` into a kernel buffer
   (capped at `MAX_FRAME_BYTES` for safety).
3. If `RemoteNic::is_registered()`, calls `RemoteNic::send_frame(&kernel_buf)` and
   maps the result through `net_send_dispatch` in
   `kernel-core/src/driver_ipc/net.rs`.
4. Otherwise falls back to `virtio_net::send_frame(&kernel_buf)` — fire-and-forget,
   returns 0.

`DriverRestarting` (byte 4) → `NEG_EAGAIN` (-11).
`RingFull` (byte 2) → `NEG_EAGAIN` (-11).
Every other `NetDriverError` → `NEG_EIO` (-5).
No socket fd → `NEG_EBADF` (-9).

### Socket capability boundary (resend fix)

The original G.3 implementation accepted `(buf_ptr, len)` with no socket
ownership check, allowing any process to inject raw frames without owning a
socket.  The resend adds `sock_fd` as the first argument (`arg0`).

The arch-level dispatcher (`arch/x86_64/syscall/mod.rs`) validates `arg0` using
`current_fd_entry` (a private dispatcher function) before calling `sys_net_send`:

```
NET_SEND dispatch arm:
  has_socket = arg0 < MAX_FDS
               && current_fd_entry(arg0).backend == FdBackend::Socket
  → sys_net_send(has_socket, arg1, arg2)
```

`sys_net_send` gates on `has_socket` at the top of the function and returns
`NEG_EBADF` immediately if false.  Raw frame injection is therefore available
only to callers that have legitimately opened a socket, matching the ownership
proof that `sys_sendto` requires.

### `sendto()` and EAGAIN — Track G contract

`sys_sendto` (POSIX syscall 44) routes UDP sends through `net::udp::send` →
`net::send_frame`, which is fire-and-forget (returns `()`).  This path does
**not** surface `DriverRestarting` as `NEG_EAGAIN`.

**Track G delivers EAGAIN observability exclusively via `sys_net_send`.**
Callers that use the POSIX `sendto()` path do not see `EAGAIN` during a driver
restart window.  This is the documented Track G contract; it is not a deferred
item for Track H.

Extending `sendto()` to propagate `NetDriverError::DriverRestarting` would
require changing `kernel/src/net/mod.rs::send_frame` from `fn(_) -> ()` to
`fn(_) -> Result<(), NetDriverError>` and threading that change through
`net/arp.rs`, `net/ipv4.rs`, and `net/udp.rs` — none of which are in Track G
scope.  Track H may elect to implement this refactor if the userspace smoke
binary needs `sendto()` EAGAIN; until then, the smoke binary uses `sys_net_send`
directly (as Track H was always expected to do for the crash-smoke binary).

### `net_send_dispatch` — the dispatch-seam function (resend fix)

The resend introduces `kernel_core::driver_ipc::net::net_send_dispatch`:

```rust
pub const fn net_send_dispatch(has_socket: bool,
                                frame_result: Result<(), NetDriverError>) -> i64
```

This function is the single authoritative point for the combined socket-boundary
+ errno-mapping logic.  The kernel's `sys_net_send` calls it; the G.3 tests
in `kernel-core/tests/driver_restart.rs` test it directly.  Testing
`net_send_dispatch` covers the real ABI/dispatch seam, not just the underlying
`net_send_result_to_syscall_ret` pure helper.

### Precedent: `sys_block_{read,write}`

`sys_block_read` (0x1011) and `sys_block_write` (0x1012) are dedicated syscalls
that route raw I/O to the ring-3 block-driver facade, propagating
`BlockDriverError` through `block_error_to_neg_errno`.  `sys_net_send` follows
the same pattern at number 0x1013.

Both share the design invariant:

> **DRY errno translation.**  A single function in `kernel-core/src/driver_ipc/`
> owns the byte-to-errno mapping.  The syscall wrapper calls that function;
> nothing else does.

### Files changed under this shape

| File | Change |
|---|---|
| `docs/appendix/phase-55c-net-send-shape.md` | This memo (resend: socket boundary, sendto contract) |
| `kernel-core/src/driver_ipc/net.rs` | Add `net_send_dispatch(has_socket, frame_result) -> i64` |
| `kernel-core/tests/driver_restart.rs` | Add G.3 dispatch-seam tests; import `net_send_dispatch`; update stale e1000 QEMU stub |
| `kernel/src/syscall/net.rs` | Update: `has_socket: bool` param, use `net_send_dispatch`, add `NEG_EBADF` |
| `kernel/src/arch/x86_64/syscall/mod.rs` | Update dispatch arm: validate socket fd, pass `has_socket`; update `NET_SEND` doc |
| `kernel/src/net/remote.rs` | Dedup-warning guard (already landed in G.3 original) — unchanged |

`kernel/src/net/mod.rs` is **not** changed.  The existing `send_frame` (fire-and-forget
for the stack's internal ARP/IPv4 TX path) keeps its `fn(_) -> ()` surface.

---

## Rejected Alternative: Extend `sys_sendto`

### What this would require

Modifying `net::send_frame` (in `kernel/src/net/mod.rs`) to return
`Result<(), NetDriverError>` instead of `()`, then threading the error back up
through `net::udp::send` into `sys_sendto`'s UDP branch.

### Why rejected

1. **Scope violation.** `kernel/src/net/mod.rs` is not in the Track G file scope.
   Changing `send_frame`'s return type also requires touching `kernel/src/net/arp.rs`,
   `kernel/src/net/ipv4.rs`, and every other caller — none in scope.

2. **POSIX contract risk.** `sys_sendto` handles TCP, UDP, ICMP, and Unix domain
   sockets.  Threading a ring-3-driver error into it couples POSIX socket semantics to
   an implementation detail of the ring-3 NIC path.

3. **Inconsistent with the block precedent.** `sys_block_read` was added as a new
   syscall, not by extending `read()`.

4. **Larger blast radius.** ~6 additional files across `kernel/src/net/`.

### Track G contract is `sys_net_send` EAGAIN, not `sendto()` EAGAIN

The task contract for Track G is that callers can observe `NEG_EAGAIN` during a
ring-3 driver restart.  `sys_net_send` delivers this.  The `sendto()` path is
fire-and-forget and is out of scope.  This is the definitive Track G contract;
it supersedes any earlier language about "Track H wires the observation" — Track H
exercises the smoke binary against `sys_net_send`, not against `sendto()`.

