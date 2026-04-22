//! Phase 55c Track G.3 — `sys_net_send`: direct raw-frame transmit path with
//! socket-capability gate and `NetDriverError` propagation.
//!
//! This is a **supplementary** send path for callers that construct raw Ethernet
//! frames directly (e.g., `e1000-crash-smoke`).  The R1 contract — that
//! userspace `sendto()` observes `-EAGAIN` during a ring-3 driver restart — is
//! satisfied by the `sys_sendto` restart gate in
//! `kernel/src/arch/x86_64/syscall/mod.rs` (see `RemoteNic::check_restart_gate`).
//! `sys_net_send` is not required for the contract to hold.
//!
//! ## Design shape chosen (G.1)
//!
//! New dedicated syscall `sys_net_send` at number `0x1013`, following the
//! `sys_block_read` (0x1011) / `sys_block_write` (0x1012) precedent.
//! See `docs/appendix/phase-55c-net-send-shape.md` for full rationale.
//!
//! ## Socket capability boundary
//!
//! `sys_net_send` requires the caller to pass a valid open socket fd as `arg0`.
//! The arch dispatcher resolves `arg0` against the calling process's fd table
//! (`current_fd_entry`) and passes `has_socket = true` only when the entry's
//! backend is `FdBackend::Socket`.  Callers without an open socket receive
//! `NEG_EBADF` (-9) without any driver interaction.
//!
//! ## Dispatch priority
//!
//! 1. `RemoteNic` — ring-3 e1000 driver via IPC, when registered.
//!    `RemoteNic::send_frame` may return `Err(NetDriverError::DriverRestarting)`
//!    or `Err(NetDriverError::RingFull)` during a restart window; both map to
//!    `NEG_EAGAIN` through `net_send_dispatch`.
//! 2. VirtIO-net — fallback when no ring-3 NIC driver is registered.
//!    `virtio_net::send_frame` is fire-and-forget; this path always returns 0.
//!
//! ## Errno table
//!
//! | `NetDriverError` | Errno |
//! |---|---|
//! | (no socket fd)  | `NEG_EBADF` (-9) |
//! | `Ok` | 0 (success) |
//! | `DriverRestarting` (4) | `NEG_EAGAIN` (-11) |
//! | `RingFull` (2) | `NEG_EAGAIN` (-11) |
//! | everything else | `NEG_EIO` (-5) |
//!
//! Single source of truth:
//! `kernel_core::driver_ipc::net::net_send_dispatch`
//! (tested in `kernel-core/tests/driver_restart.rs`, G.2/G.3).

use kernel_core::driver_ipc::net::net_send_dispatch;

use crate::mm::user_mem::UserSliceRo;
use crate::net::{remote::RemoteNic, virtio_net};

const NEG_EBADF: u64 = (-9_i64) as u64;
const NEG_EINVAL: u64 = (-22_i64) as u64;
const NEG_EFAULT: u64 = (-14_i64) as u64;

/// Maximum raw Ethernet frame size accepted by `sys_net_send`.
///
/// Matches `kernel_core::driver_ipc::net::MAX_FRAME_BYTES`; capped here as
/// a defense-in-depth check before the `kernel_buf` allocation.
const MAX_FRAME_LEN: usize = kernel_core::driver_ipc::net::MAX_FRAME_BYTES as usize;

/// `sys_net_send(sock_fd, buf_ptr, len) → errno`
///
/// Transmit a raw Ethernet frame from userspace through whichever NIC driver
/// is currently registered.  The caller must own an open socket fd (`sock_fd`)
/// — the arch dispatcher validates this before calling here.
///
/// # Arguments
///
/// - `has_socket`: `true` when the arch dispatcher confirmed `sock_fd` resolves
///   to a `FdBackend::Socket` entry in the calling process's fd table.
/// - `buf_ptr`: userspace address of the frame bytes.
/// - `len`: frame length in bytes.  Must be in `1..=MAX_FRAME_LEN`.
///
/// # Returns
///
/// - `0` — frame queued (RemoteNic) or delivered (virtio-net).
/// - `NEG_EBADF` (-9) — caller has no open socket fd.
/// - `NEG_EAGAIN` (-11) — `DriverRestarting` or `RingFull`; caller retries.
/// - `NEG_EIO` (-5) — hard send error.
/// - `NEG_EINVAL` (-22) — `len` is 0 or exceeds `MAX_FRAME_LEN`.
/// - `NEG_EFAULT` (-14) — `buf_ptr` is not a valid userspace address.
pub fn sys_net_send(has_socket: bool, buf_ptr: u64, len: u64) -> u64 {
    // Socket capability boundary: reject callers without an open socket fd.
    if !has_socket {
        return NEG_EBADF;
    }

    let len = len as usize;
    if len == 0 || len > MAX_FRAME_LEN {
        return NEG_EINVAL;
    }

    let mut kernel_buf = alloc::vec![0u8; len];
    if UserSliceRo::new(buf_ptr, len)
        .and_then(|s| s.copy_to_kernel(&mut kernel_buf))
        .is_err()
    {
        return NEG_EFAULT;
    }

    if RemoteNic::is_registered() {
        let result = RemoteNic::send_frame(&kernel_buf);
        // net_send_dispatch enforces both the socket boundary and the errno
        // mapping; has_socket is already validated above so this always
        // routes to net_send_result_to_syscall_ret internally.
        return net_send_dispatch(true, result) as u64;
    }

    // Fallback: virtio-net is fire-and-forget.
    virtio_net::send_frame(&kernel_buf);
    0
}
