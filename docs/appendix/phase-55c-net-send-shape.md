# Phase 55c Track G — Net-Send Shape Decision (second resend)

**Status:** Decided and implemented  
**Scope:** G.1 (design), G.2 (tests), G.3 (implementation)  
**Decided shape:** `sys_sendto` extension (restart gate) + `sys_net_send` direct path

---

## Chosen Shape: `sys_sendto` restart gate + `sys_net_send` direct path

### R1 contract (accepted roadmap)

> **A userspace `sendto()` through the e1000 path observes `EAGAIN` while the
> ring-3 driver is mid-restart.**

Track G delivers this by adding a lightweight restart gate inside `sys_sendto`'s
UDP and ICMP branches.  No changes to `net/mod.rs`, `net/udp.rs`, or
`net/arp.rs` — the gate fires before the fire-and-forget send path, short-
circuiting with `NEG_EAGAIN` when `RemoteNic::check_restart_gate()` indicates the
ring-3 driver is mid-restart.

### sendto() EAGAIN path

The `sys_sendto` function in `kernel/src/arch/x86_64/syscall/mod.rs` already
validates the socket fd (`FdBackend::Socket` check) at the top.  After the
destination address is resolved and before the `udp::send` / `ipv4::send` call,
each branch now calls:

```rust
if let Some(err) = crate::net::remote::RemoteNic::check_restart_gate() {
    return kernel_core::driver_ipc::net::net_error_to_neg_errno(
        err.to_byte(),
    ) as u64;
}
```

`RemoteNic::check_restart_gate()` reads two `AtomicBool`s with `Acquire`
ordering — lock-free, safe on the syscall hot path:

```
REMOTE_NIC_REGISTERED && RESTART_SUSPECTED → Some(DriverRestarting) → NEG_EAGAIN
anything else                               → None → proceed normally
```

The socket capability boundary is already enforced by `sys_sendto`'s
`FdBackend::Socket` fd-table check; no additional gate is needed in
`check_restart_gate`.

### sys_net_send direct path (supplementary)

`sys_net_send` (syscall 0x1013) remains as a direct raw-frame send path:
`sys_net_send(sock_fd: u64, buf_ptr: u64, len: u64) -> u64`

1. The arch dispatcher validates `sock_fd` against `FdBackend::Socket`.
2. Copies `len` bytes from userspace into a kernel buffer.
3. If `RemoteNic::is_registered()`, calls `RemoteNic::send_frame(&kernel_buf)`
   and maps the result through `net_send_dispatch` (EAGAIN on `DriverRestarting`
   or `RingFull`, EIO on all other errors).
4. Otherwise falls back to `virtio_net::send_frame` — fire-and-forget, returns 0.

`sys_net_send` is useful for callers that construct raw Ethernet frames directly
(e.g., `e1000-crash-smoke`) but is not required for the R1 contract — `sendto()`
satisfies the contract independently.

### Errno table

| Condition | Errno |
|---|---|
| `sendto()` with ring-3 NIC restarting | `NEG_EAGAIN` (-11) via `check_restart_gate` |
| `sys_net_send`, no socket fd | `NEG_EBADF` (-9) |
| `sys_net_send` / `sendto()`, `DriverRestarting` | `NEG_EAGAIN` (-11) |
| `sys_net_send`, `RingFull` | `NEG_EAGAIN` (-11) |
| `sys_net_send`, hard error | `NEG_EIO` (-5) |
| `sendto()`, no ring-3 NIC registered | 0 (fire-and-forget via virtio) |

### Pure-logic seam functions in kernel-core

Two host-testable functions in `kernel-core/src/driver_ipc/net.rs` mirror the
dispatch logic for testing:

| Function | Covers |
|---|---|
| `sendto_restart_errno(is_registered, is_restarting)` | `sys_sendto` restart gate |
| `net_send_dispatch(has_socket, frame_result)` | `sys_net_send` socket boundary + errno map |

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
| `docs/appendix/phase-55c-net-send-shape.md` | This memo (second resend) |
| `kernel-core/src/driver_ipc/net.rs` | Add `sendto_restart_errno(is_registered, is_restarting) -> Option<i64>` |
| `kernel-core/tests/driver_restart.rs` | Add G.3 sendto-gate tests; update QEMU stub |
| `kernel/src/net/remote.rs` | Add `RemoteNic::check_restart_gate() -> Option<NetDriverError>` |
| `kernel/src/arch/x86_64/syscall/mod.rs` | Add restart gate in `sys_sendto` UDP + ICMP branches |
| `kernel/src/syscall/net.rs` | `sys_net_send` unchanged (supplementary direct path) |

`kernel/src/net/mod.rs` is **not** changed.  The existing `send_frame` keeps its
`fn(_) -> ()` surface.  `net/udp.rs`, `net/arp.rs`, and `net/ipv4.rs` are
unchanged.

---

## Rejected Alternative: Extend `net::send_frame` return type

### What this would require

Modifying `net::send_frame` (in `kernel/src/net/mod.rs`) to return
`Result<(), NetDriverError>` instead of `()`, then threading the error back up
through `net::udp::send` into `sys_sendto`'s UDP branch.

### Why rejected

1. **Scope violation.** `kernel/src/net/mod.rs` is not in the Track G file scope.
   Changing `send_frame`'s return type also requires touching `kernel/src/net/arp.rs`,
   `kernel/src/net/ipv4.rs`, and every other caller — none in scope.

2. **POSIX contract risk.** `sys_sendto` handles TCP, UDP, ICMP, and Unix domain
   sockets.  Threading a ring-3-driver error into the generic `send_frame` couples
   POSIX socket semantics to an implementation detail of the ring-3 NIC path.
   The restart gate in `sys_sendto` is narrower: it fires only when a ring-3 NIC is
   registered, leaving all other cases unaffected.

3. **Larger blast radius.** ~6 additional files across `kernel/src/net/`.

---

## Track G acceptance

- `sys_sendto()` UDP and ICMP callers observe `NEG_EAGAIN` (-11) during a ring-3
  driver restart window (`RESTART_SUSPECTED` set).
- Socket fd capability validation is intact (pre-existing `FdBackend::Socket` check
  in `sys_sendto`; `has_socket` gate in `sys_net_send`).
- Host-testable pure-logic tests cover both seams:
  - `sendto_restart_errno` (sendto path, 4 tests)
  - `net_send_dispatch` (sys_net_send path, 6 tests)
