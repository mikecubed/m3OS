//! Phase 55c Track G.3 — `sys_net_send`: raw frame transmit with
//! `NetDriverError` propagation.
//!
//! Bridges the arch-level syscall dispatcher (`arch::x86_64::syscall::mod.rs`)
//! to `RemoteNic::send_frame`, surfacing `NetDriverError::DriverRestarting`
//! as `NEG_EAGAIN` (-11) to userspace for the first time.
//!
//! ## Design shape chosen (G.1)
//!
//! New dedicated syscall `sys_net_send` at number `0x1013`, following the
//! `sys_block_read` (0x1011) / `sys_block_write` (0x1012) precedent exactly.
//! See `docs/appendix/phase-55c-net-send-shape.md` for full rationale and
//! the rejected alternative.
//!
//! ## Dispatch priority (socket-layer preference)
//!
//! 1. `RemoteNic` — ring-3 e1000 driver via IPC, when registered.
//!    `RemoteNic::send_frame` may return `Err(NetDriverError::DriverRestarting)`
//!    or `Err(NetDriverError::RingFull)` during a restart window; both map to
//!    `NEG_EAGAIN` through `net_send_result_to_syscall_ret`.
//! 2. VirtIO-net — fallback when no ring-3 NIC driver is registered.
//!    `virtio_net::send_frame` is fire-and-forget; this path always returns 0.
//!
//! ## Errno table
//!
//! | `NetDriverError` | Errno |
//! |---|---|
//! | `Ok` | 0 (success) |
//! | `DriverRestarting` (4) | `NEG_EAGAIN` (-11) |
//! | `RingFull` (2) | `NEG_EAGAIN` (-11) |
//! | everything else | `NEG_EIO` (-5) |
//!
//! Single source of truth:
//! `kernel_core::driver_ipc::net::net_send_result_to_syscall_ret`
//! (tested in `kernel-core/tests/driver_restart.rs`, G.2).

use kernel_core::driver_ipc::net::net_send_result_to_syscall_ret;

use crate::mm::user_mem::UserSliceRo;
use crate::net::{remote::RemoteNic, virtio_net};

const NEG_EINVAL: u64 = (-22_i64) as u64;
const NEG_EFAULT: u64 = (-14_i64) as u64;

/// Maximum raw Ethernet frame size accepted by `sys_net_send`.
///
/// Matches `kernel_core::driver_ipc::net::MAX_FRAME_BYTES` (1518); capped
/// here as a defense-in-depth check before the kernel_buf allocation.
const MAX_FRAME_LEN: usize = kernel_core::driver_ipc::net::MAX_FRAME_BYTES as usize;

/// `sys_net_send(buf_ptr, len) → errno`
///
/// Transmit a raw Ethernet frame from userspace through whichever NIC
/// driver is currently registered.
///
/// # Arguments
///
/// - `buf_ptr`: userspace address of the frame bytes.
/// - `len`: frame length in bytes.  Must be in `1..=MAX_FRAME_LEN`.
///
/// # Returns
///
/// - `0` — frame was queued successfully (or delivered to virtio-net).
/// - `NEG_EAGAIN` (-11) — `RemoteNic::send_frame` returned
///   `DriverRestarting` or `RingFull`; caller should retry.
/// - `NEG_EIO` (-5) — hard send error (`LinkDown`, `DeviceAbsent`,
///   `InvalidFrame`).
/// - `NEG_EINVAL` (-22) — `len` is 0 or exceeds `MAX_FRAME_LEN`.
/// - `NEG_EFAULT` (-14) — `buf_ptr` is not a valid userspace pointer.
pub fn sys_net_send(buf_ptr: u64, len: u64) -> u64 {
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
        // Propagate NetDriverError through net_send_result_to_syscall_ret so
        // DriverRestarting and RingFull surface as NEG_EAGAIN to the caller.
        let result = RemoteNic::send_frame(&kernel_buf);
        return net_send_result_to_syscall_ret(result) as u64;
    }

    // Fallback: virtio-net is fire-and-forget; no error to propagate.
    virtio_net::send_frame(&kernel_buf);
    0
}
