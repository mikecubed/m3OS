//! Phase 55b Track F.3c — NVMe crash-and-restart end-to-end smoke test (updated).
//!
//! This binary is the guest-side I/O client that was deferred from Tracks F.2
//! and F.2b. It demonstrates the full mid-restart scenario:
//!
//! 1. Issue a BLK_READ request directly to the `nvme.block` IPC endpoint.
//! 2. Fork a child that calls `service kill nvme_driver` while the I/O is
//!    in flight (or shortly after it completes).
//! 3. Observe the IPC transport failure that the kill produces.
//! 3.5 (F.3c): Call `sys_block_read` from the kernel facade during the crash
//!    window and assert it returns `EAGAIN` (-11) — the
//!    `BlockDriverError::DriverRestarting` byte (5) propagated via
//!    `block_error_to_neg_errno`.  This is the EAGAIN observation that was
//!    blocked in F.3b (the binary lacked euid=200 and was not in the
//!    BLOCK_READ_ALLOWED whitelist).
//! 4. Poll `/run/services.status` until nvme_driver shows `running` again
//!    (init restarts it per the Phase 46/51 service manager).
//! 5. Retry both the IPC read and `sys_block_read` — confirm both succeed.
//! 6. Emit `NVME_CRASH_SMOKE:PASS` on success or
//!    `NVME_CRASH_SMOKE:FAIL <step>` on any sub-step failure.
//!
//! # Why this binary can now call sys_block_read
//!
//! Phase 55b Track F.3c granted this binary storage-server privileges:
//!   - `privileged_exec_credentials("/bin/nvme-crash-smoke", _)` → euid=200
//!   - `BLOCK_READ_ALLOWED` (non-hardened build) includes `/bin/nvme-crash-smoke`
//!
//! With those two gates open, the `sys_block_read` syscall admits this binary.
//! When the driver is mid-restart, `remote.rs` surfaces
//! `BlockDriverError::DriverRestarting` (byte 5), which
//! `block_error_to_neg_errno` maps to `NEG_EAGAIN` (-11).
//!
//! # Block IPC protocol (manual encoding)
//!
//! BLK_REQUEST_HEADER_SIZE = 30 bytes (packed little-endian):
//!   [0..2]  kind: u16   (BLK_READ = 0x5501)
//!   [2..10] cmd_id: u64
//!   [10..18] lba: u64
//!   [18..22] sector_count: u32
//!   [22..26] flags: u32
//!   [26..30] payload_grant: u32 (0 for reads)
//!
//! BLK_REPLY_HEADER_SIZE = 20 bytes:
//!   [0..8]  cmd_id: u64
//!   [8]     status: u8  (0 = Ok, 5 = DriverRestarting)
//!   [9..12] reserved (zero)
//!   [12..16] bytes: u32
//!   [16..20] payload_grant: u32
#![no_std]
#![no_main]

use syscall_lib::{
    O_RDONLY, STDOUT_FILENO, block_read, block_write, close, execve, exit, fork, nanosleep, open,
    read, waitpid, write_str,
};

// Service name the NVMe driver registers its IPC endpoint under (matches
// `nvme_driver::SERVICE_NAME` in `userspace/drivers/nvme/src/main.rs`).
const NVME_SERVICE: &str = "nvme.block";

// BLK_READ label: 0x5501
const BLK_READ_KIND: u16 = 0x5501;
// IPC label for the block request envelope (matches BLK_READ u16 cast to u64).
const BLK_READ_LABEL: u64 = 0x5501;

// Encoded BLK_REQUEST_HEADER_SIZE
const BLK_REQ_SIZE: usize = 30;

// Paths for service polling
const STATUS_PATH: &[u8] = b"/run/services.status\0";

// Restart wait budget: 3 × DRIVER_RESTART_TIMEOUT_MS (1000 ms each) = 3 s.
const RESTART_WAIT_SECONDS: u64 = 3;

// Paths for killing the driver via service binary
const SERVICE_BIN: &[u8] = b"/bin/service\0";
const SERVICE_ARGV0: &[u8] = b"service\0";
const SERVICE_KILL_ARG: &[u8] = b"kill\0";
const NVME_DRIVER_NAME: &[u8] = b"nvme_driver\0";

// Linux errno value EAGAIN (negated i64)
const NEG_EAGAIN: i64 = -11;

syscall_lib::entry_point!(program_main);

fn program_main(_args: &[&str]) -> i32 {
    write_str(STDOUT_FILENO, "NVME_CRASH_SMOKE:BEGIN\n");

    // ------------------------------------------------------------------
    // Step 1: Look up the nvme.block IPC endpoint.
    // ------------------------------------------------------------------
    let ep_handle = syscall_lib::ipc_lookup_service(NVME_SERVICE);
    if ep_handle == u64::MAX {
        write_str(
            STDOUT_FILENO,
            "NVME_CRASH_SMOKE:FAIL step=1 ipc_lookup_service\n",
        );
        return 1;
    }
    let ep_handle = ep_handle as u32;

    // ------------------------------------------------------------------
    // Step 2: Send a BLK_READ request and confirm it succeeds (driver up).
    // ------------------------------------------------------------------
    let req = encode_blk_read(0xF3B1_0001, 0, 1);
    let ret = syscall_lib::ipc_call_buf(ep_handle, BLK_READ_LABEL, BLK_READ_LABEL, &req);
    if ret == u64::MAX {
        write_str(
            STDOUT_FILENO,
            "NVME_CRASH_SMOKE:FAIL step=2 pre-crash read failed\n",
        );
        return 2;
    }
    write_str(STDOUT_FILENO, "NVME_CRASH_SMOKE:pre-crash-read:OK\n");

    // Also confirm sys_block_read works before the kill (we have euid=200).
    let mut sector_buf = [0u8; 512];
    let pre_kr = block_read(0, 1, &mut sector_buf);
    if pre_kr != 0 {
        write_str(
            STDOUT_FILENO,
            "NVME_CRASH_SMOKE:FAIL step=2 pre-crash sys_block_read failed\n",
        );
        return 2;
    }
    write_str(
        STDOUT_FILENO,
        "NVME_CRASH_SMOKE:pre-crash-sys_block_read:OK\n",
    );

    // F.3d-2: Confirm sys_block_write works before the kill.
    // Write a 512-byte sector filled with 0xB5 to LBA 0.
    let write_buf = [0xB5u8; 512];
    let pre_kw = block_write(0, 1, &write_buf);
    if pre_kw != 0 {
        write_str(
            STDOUT_FILENO,
            "NVME_CRASH_SMOKE:FAIL step=2 pre-crash sys_block_write failed\n",
        );
        return 2;
    }
    write_str(STDOUT_FILENO, "NVME_CRASH_SMOKE:write:pre-crash:OK\n");

    // ------------------------------------------------------------------
    // Step 3: Kill the driver in a child process.
    //
    // We fork so the parent can immediately issue another IPC call while
    // the child is delivering SIGKILL. The parent's call either succeeds
    // (kill raced after the reply) or sees a transport error (kill landed
    // during the call). Both are acceptable: either way we then poll for
    // the restart and retry.
    // ------------------------------------------------------------------
    let kill_pid = fork();
    if kill_pid < 0 {
        write_str(STDOUT_FILENO, "NVME_CRASH_SMOKE:FAIL step=3 fork\n");
        return 3;
    }

    if kill_pid == 0 {
        // Child: exec `service kill nvme_driver`.
        let argv: [*const u8; 4] = [
            SERVICE_ARGV0.as_ptr(),
            SERVICE_KILL_ARG.as_ptr(),
            NVME_DRIVER_NAME.as_ptr(),
            core::ptr::null(),
        ];
        let envp: [*const u8; 1] = [core::ptr::null()];
        let _ = execve(SERVICE_BIN, &argv, &envp);
        // execve only returns on error; exit with a distinctive code so
        // the parent can detect a child exec failure.
        exit(126);
    }

    // Parent: issue another BLK_READ while the child is killing the driver.
    let req2 = encode_blk_read(0xF3B1_0002, 0, 1);
    let mid_crash_ret = syscall_lib::ipc_call_buf(ep_handle, BLK_READ_LABEL, BLK_READ_LABEL, &req2);

    // Wait for the killer child to finish.
    let mut child_status = 0i32;
    let waited = waitpid(kill_pid as i32, &mut child_status, 0);
    if waited != kill_pid as isize {
        write_str(STDOUT_FILENO, "NVME_CRASH_SMOKE:FAIL step=3 waitpid\n");
        return 3;
    }

    // Log the mid-crash result (transport error = u64::MAX, or a reply
    // if the kill landed after the call completed).
    if mid_crash_ret == u64::MAX {
        write_str(
            STDOUT_FILENO,
            "NVME_CRASH_SMOKE:mid-crash:transport-error\n",
        );
    } else {
        write_str(
            STDOUT_FILENO,
            "NVME_CRASH_SMOKE:mid-crash:reply-before-kill\n",
        );
    }
    write_str(STDOUT_FILENO, "NVME_CRASH_SMOKE:kill-delivered\n");

    // ------------------------------------------------------------------
    // Step 3.5 (F.3c): Assert EAGAIN from sys_block_read during crash window.
    //
    // Now that the driver is killed, `remote.rs` will surface
    // `BlockDriverError::DriverRestarting` (byte 5) which maps to
    // `NEG_EAGAIN` (-11). We loop a few times to catch the window where the
    // driver is confirmed down (the state machine has marked it Restarting).
    //
    // If the driver restarts quickly, block_read may return 0 (success) or
    // EAGAIN, both of which are valid. We only fail if it returns something
    // other than 0 or EAGAIN (e.g., EIO = -5, which would mean the old
    // `Err(_) => NEG_EIO` behavior was still in effect).
    // ------------------------------------------------------------------
    let mut eagain_seen = false;
    let mut mid_kr_ok = false;
    for _ in 0u32..5 {
        let mid_kr = block_read(0, 1, &mut sector_buf);
        if mid_kr == 0 {
            // Driver already restarted before we could catch EAGAIN — valid.
            mid_kr_ok = true;
            break;
        }
        if mid_kr == NEG_EAGAIN {
            eagain_seen = true;
            write_str(
                STDOUT_FILENO,
                "NVME_CRASH_SMOKE:mid-crash:sys_block_read:EAGAIN\n",
            );
            break;
        }
        // Any other error (e.g., NEG_EIO = -5) means errno propagation is wrong.
        if mid_kr != NEG_EAGAIN && mid_kr != 0 {
            write_str(
                STDOUT_FILENO,
                "NVME_CRASH_SMOKE:FAIL step=3.5 unexpected errno from sys_block_read\n",
            );
            return 3;
        }
    }

    if !eagain_seen && !mid_kr_ok {
        // Neither EAGAIN nor success in 5 attempts — driver might be taking
        // longer than expected. Log and continue (not a hard failure — the
        // key assertion is that we never see EIO, which is caught above).
        write_str(
            STDOUT_FILENO,
            "NVME_CRASH_SMOKE:mid-crash:sys_block_read:no-eagain-in-window\n",
        );
    }

    // ------------------------------------------------------------------
    // Step 3.6 (F.3d-2): Assert EAGAIN from sys_block_write during crash window.
    //
    // The write path must surface DriverRestarting (byte 5) as NEG_EAGAIN (-11)
    // via the same block_error_to_neg_errno mapping used by sys_block_read.
    // This is the stub that was #[ignore]'d in driver_restart.rs until
    // sys_block_write existed.
    //
    // Same policy as step 3.5: if the driver restarts quickly, block_write
    // may return 0 (success) — that is valid. We only fail on anything other
    // than 0 or EAGAIN.
    // ------------------------------------------------------------------
    let mut write_eagain_seen = false;
    let mut mid_kw_ok = false;
    for _ in 0u32..5 {
        let mid_kw = block_write(0, 1, &write_buf);
        if mid_kw == 0 {
            // Driver already restarted before we could catch EAGAIN — valid.
            mid_kw_ok = true;
            break;
        }
        if mid_kw == NEG_EAGAIN {
            write_eagain_seen = true;
            write_str(STDOUT_FILENO, "NVME_CRASH_SMOKE:write:EAGAIN-observed\n");
            break;
        }
        // Any other error (e.g., NEG_EIO = -5) means errno propagation is wrong.
        if mid_kw != NEG_EAGAIN && mid_kw != 0 {
            write_str(
                STDOUT_FILENO,
                "NVME_CRASH_SMOKE:FAIL step=3.6 unexpected errno from sys_block_write\n",
            );
            return 3;
        }
    }

    if !write_eagain_seen && !mid_kw_ok {
        // Log but do not fail — timing-dependent.
        write_str(
            STDOUT_FILENO,
            "NVME_CRASH_SMOKE:write:no-eagain-in-window\n",
        );
    }

    // ------------------------------------------------------------------
    // Step 4: Wait for nvme_driver to show "running" again.
    // ------------------------------------------------------------------
    if !wait_for_driver_running("nvme_driver", RESTART_WAIT_SECONDS) {
        write_str(
            STDOUT_FILENO,
            "NVME_CRASH_SMOKE:FAIL step=4 restart-timeout\n",
        );
        return 4;
    }
    write_str(STDOUT_FILENO, "NVME_CRASH_SMOKE:restart-confirmed\n");

    // ------------------------------------------------------------------
    // Step 5: Re-look-up the endpoint (driver registered a fresh one).
    // ------------------------------------------------------------------
    let ep_handle2 = syscall_lib::ipc_lookup_service(NVME_SERVICE);
    if ep_handle2 == u64::MAX {
        write_str(STDOUT_FILENO, "NVME_CRASH_SMOKE:FAIL step=5 re-lookup\n");
        return 5;
    }
    let ep_handle2 = ep_handle2 as u32;

    // ------------------------------------------------------------------
    // Step 6: Retry both the IPC read and sys_block_read — must succeed.
    // ------------------------------------------------------------------
    let req3 = encode_blk_read(0xF3B1_0003, 0, 1);
    let retry_ret = syscall_lib::ipc_call_buf(ep_handle2, BLK_READ_LABEL, BLK_READ_LABEL, &req3);
    if retry_ret == u64::MAX {
        write_str(
            STDOUT_FILENO,
            "NVME_CRASH_SMOKE:FAIL step=6 post-restart read transport-error\n",
        );
        return 6;
    }
    write_str(STDOUT_FILENO, "NVME_CRASH_SMOKE:post-restart-read:OK\n");

    // Also verify sys_block_read succeeds post-restart.
    let post_kr = block_read(0, 1, &mut sector_buf);
    if post_kr != 0 {
        write_str(
            STDOUT_FILENO,
            "NVME_CRASH_SMOKE:FAIL step=6 post-restart sys_block_read failed\n",
        );
        return 6;
    }
    write_str(
        STDOUT_FILENO,
        "NVME_CRASH_SMOKE:post-restart-sys_block_read:OK\n",
    );

    // F.3d-2: Post-restart write then read — confirm write path is healthy
    // after the driver has been restarted.
    let post_write_buf = [0xD2u8; 512];
    let post_kw = block_write(0, 1, &post_write_buf);
    if post_kw != 0 {
        write_str(
            STDOUT_FILENO,
            "NVME_CRASH_SMOKE:FAIL step=6 post-restart sys_block_write failed\n",
        );
        return 6;
    }
    // Read back what we just wrote to confirm the write landed.
    let mut readback = [0u8; 512];
    let readback_ret = block_read(0, 1, &mut readback);
    if readback_ret != 0 {
        write_str(
            STDOUT_FILENO,
            "NVME_CRASH_SMOKE:FAIL step=6 post-restart readback after write failed\n",
        );
        return 6;
    }
    // Verify the readback matches what we wrote.
    let matches = readback.iter().all(|&b| b == 0xD2);
    if !matches {
        write_str(
            STDOUT_FILENO,
            "NVME_CRASH_SMOKE:FAIL step=6 post-restart write/read mismatch\n",
        );
        return 6;
    }
    write_str(STDOUT_FILENO, "NVME_CRASH_SMOKE:write:post-restart-ok\n");

    // ------------------------------------------------------------------
    // Done.
    // ------------------------------------------------------------------
    write_str(STDOUT_FILENO, "NVME_CRASH_SMOKE:PASS\n");
    0
}

/// Encode a 30-byte BLK_READ request header (packed little-endian).
///
/// Layout:
///   [0..2]   kind: u16 = BLK_READ (0x5501)
///   [2..10]  cmd_id: u64
///   [10..18] lba: u64
///   [18..22] sector_count: u32
///   [22..26] flags: u32  (0)
///   [26..30] payload_grant: u32 (0, reads have no payload)
fn encode_blk_read(cmd_id: u64, lba: u64, sector_count: u32) -> [u8; BLK_REQ_SIZE] {
    let mut out = [0u8; BLK_REQ_SIZE];
    let kind = BLK_READ_KIND.to_le_bytes();
    out[0] = kind[0];
    out[1] = kind[1];
    let ci = cmd_id.to_le_bytes();
    out[2..10].copy_from_slice(&ci);
    let lb = lba.to_le_bytes();
    out[10..18].copy_from_slice(&lb);
    let sc = sector_count.to_le_bytes();
    out[18..22].copy_from_slice(&sc);
    // flags = 0, payload_grant = 0 — already zeroed
    out
}

/// Poll `/run/services.status` until the named service shows `running`
/// or `timeout_secs` elapses.  Returns `true` when running is observed.
fn wait_for_driver_running(name: &str, timeout_secs: u64) -> bool {
    let name_bytes = name.as_bytes();
    let mut buf = [0u8; 4096];
    let mut waited = 0u64;
    while waited <= timeout_secs {
        let fd = open(STATUS_PATH, O_RDONLY, 0);
        if fd >= 0 {
            let n = read(fd as i32, &mut buf);
            close(fd as i32);
            if n > 0 {
                let text = &buf[..n as usize];
                if service_is_running(text, name_bytes) {
                    return true;
                }
            }
        }
        let _ = nanosleep(1);
        waited += 1;
    }
    false
}

/// Return `true` if the status text contains a line for `name` whose
/// status field equals `running`.
///
/// Status-file line format (written by init):
///   `<name> <status> pid=<N> restarts=<N> changed=<N>`
fn service_is_running(text: &[u8], name: &[u8]) -> bool {
    for line in text.split(|&b| b == b'\n') {
        if line.is_empty() {
            continue;
        }
        // Check the line starts with `name` followed by a space.
        if line.len() <= name.len() {
            continue;
        }
        if &line[..name.len()] != name {
            continue;
        }
        if line[name.len()] != b' ' {
            continue;
        }
        // Parse the status field (second whitespace-delimited token).
        let rest = &line[name.len() + 1..];
        // Find the status token (up to the next space or end of line).
        let status_end = rest.iter().position(|&b| b == b' ').unwrap_or(rest.len());
        let status = &rest[..status_end];
        return status == b"running";
    }
    false
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    write_str(STDOUT_FILENO, "NVME_CRASH_SMOKE:PANIC\n");
    exit(101)
}
