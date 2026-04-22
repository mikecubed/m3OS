# Phase 55c Track G — Net-Send Shape Decision

**Status:** Decided  
**Scope:** G.1 (design), G.3 (implementation)  
**Decided shape:** `sys_net_send` — new dedicated syscall, extracted-helper in `kernel/src/syscall/net.rs`

---

## Chosen Shape: `sys_net_send` (new syscall, number `0x1013`)

### What it does

`sys_net_send(buf_ptr: u64, len: u64) -> u64`

1. Copies `len` bytes from the userspace address `buf_ptr` into a kernel buffer  
   (capped at `MAX_FRAME_BYTES` for safety — identical bound to `RemoteNic::send_frame`).
2. If `RemoteNic::is_registered()`, calls `RemoteNic::send_frame(&kernel_buf)` and maps
   the `Result<(), NetDriverError>` through `net_error_to_neg_errno` via the new helper
   `net_send_result_to_syscall_ret` in `kernel-core/src/driver_ipc/net.rs`.
3. Otherwise falls back to `virtio_net::send_frame(&kernel_buf)` — the existing
   fire-and-forget path — and returns 0.

`DriverRestarting` (byte 4) → `NEG_EAGAIN` (-11) through the shared `net_error_to_neg_errno`
function.  `RingFull` (byte 2) → `NEG_EAGAIN` (-11) for the same reason.  Every other
`NetDriverError` → `NEG_EIO` (-5).

### Precedent: `sys_block_{read,write}`

`sys_block_read` (0x1011) and `sys_block_write` (0x1012) follow an identical pattern:
they are dedicated syscalls that bypass the POSIX file-descriptor abstraction and route
raw I/O directly to the ring-3 block-driver facade (`RemoteBlockDevice`), propagating
`BlockDriverError` bytes through `block_error_to_neg_errno`.  `sys_net_send` follows the
same pattern for the net path, allocated at the next consecutive number 0x1013.

Both `sys_block_read` and `sys_net_send` share the design invariant:

> **DRY errno translation.**  A single function in `kernel-core/src/driver_ipc/{block,net}.rs`
> owns the byte-to-errno mapping.  The syscall wrapper calls that function; nothing else does.

### Files changed under this shape

| File | Change |
|---|---|
| `docs/appendix/phase-55c-net-send-shape.md` | This memo (new) |
| `kernel-core/src/driver_ipc/net.rs` | Add `net_send_result_to_syscall_ret(Result<(), NetDriverError>) -> i64` |
| `kernel-core/tests/driver_restart.rs` | Add `sys_net_send_mid_restart_returns_eagain` (G.2 failing test, then G.3 green) |
| `kernel/src/syscall/mod.rs` | Add `pub mod net;` |
| `kernel/src/syscall/net.rs` | New: `pub fn sys_net_send(buf_ptr: u64, len: u64) -> u64` |
| `kernel/src/arch/x86_64/syscall/mod.rs` | Add `NET_SEND = 0x1013` constant + dispatch arm + `net_error_to_neg_errno` doc table |
| `kernel/src/net/remote.rs` | Add deduplicated-warning guard for `driver.absent` log on the send path |

`kernel/src/net/mod.rs` is **not** changed — the existing `send_frame` (fire-and-forget
for the stack's internal ARP/IPv4 TX path) keeps its `fn(_) -> ()` surface.  The
observable `DriverRestarting` → `EAGAIN` path is exposed exclusively through `sys_net_send`.

---

## Rejected Alternative: Extend `sys_sendto`

### What this would require

Modifying `net::send_frame` (in `kernel/src/net/mod.rs`) to return
`Result<(), NetDriverError>` instead of `()`, then threading the error back up through
`net::udp::send` into `sys_sendto`'s UDP branch.

### Why rejected

1. **Scope violation.** `kernel/src/net/mod.rs` is not in the Track G file scope.
   Changing `send_frame`'s return type also requires touching `kernel/src/net/arp.rs`,
   `kernel/src/net/ipv4.rs`, and every other caller — none of which are in scope.

2. **POSIX contract risk.** `sys_sendto` handles TCP, UDP, ICMP, and Unix domain
   sockets.  Threading a ring-3-driver error into it couples POSIX socket semantics to
   an implementation detail of the ring-3 NIC path.  `sys_net_send` isolates that
   coupling entirely: the POSIX path is unchanged.

3. **Inconsistent with the block precedent.** `sys_block_read` was added as a new
   syscall — not by extending `read()` — for exactly the same reasons: the VFS `read`
   path has different error semantics from the raw-driver path.

4. **Larger blast radius.** Changing `send_frame`'s signature would touch ~6 additional
   files across `kernel/src/net/` and require updating the ARP / IPv4 / UDP TX chains
   before any net-layer test could compile.  The new-syscall shape touches 2 kernel files
   and 1 kernel-core file.

### Tradeoff acknowledged

Under the new-syscall shape, userspace code that calls the standard `sendto()` C library
function still does not see EAGAIN during a ring-3 driver restart (the musl `sendto`
issues syscall 44 → `sys_sendto` → `net::send_frame` which is still fire-and-forget).
The EAGAIN observation is available only to callers that use `sys_net_send` directly
(e.g., `e1000-crash-smoke`).  Track H wires the observation into the smoke binary;
extending the POSIX `sendto()` path to surface EAGAIN is deferred to a later phase when
a net-server decoupling allows the socket layer to be refactored without touching
`net::send_frame`.
