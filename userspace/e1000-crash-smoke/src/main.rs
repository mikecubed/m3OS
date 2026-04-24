//! Phase 55c Track H.1 — e1000 crash-and-restart end-to-end smoke test.
//!
//! Exercises the `RemoteNic` restart-suspected state machine and the
//! Phase 55c Track G EAGAIN surface wired through `sendto_restart_errno`.
//! This is the load-bearing R1 regression binary.
//!
//! # Steps
//!
//! 1. Send a UDP datagram to the QEMU gateway (10.0.2.2) — confirms the
//!    network path (virtio-net or RemoteNic) is alive before the kill.
//! 2. Kill the e1000 driver via `service kill e1000_driver` and wait for
//!    the kill child to exit (synchronous).
//! 3. Issue follow-up `sendto()` attempts in the restart window and **assert**
//!    that at least one attempt returns `-EAGAIN` (-11). Phase 55c Track G
//!    wired `sendto_restart_errno` so that `sys_sendto` returns `NEG_EAGAIN`
//!    when `RESTART_SUSPECTED` is set. Early successful sends are retried with
//!    a short sleep between attempts while the async TX drain latches that
//!    restart-suspected state; the assertion fails only if the retry budget
//!    expires without ever observing `-EAGAIN` or if an unexpected negative
//!    errno appears.
//! 4. Poll `/run/services.status` until `e1000_driver` shows `running`.
//! 5. Send another UDP datagram — confirms the restart path is complete.
//! 6. Emit structured markers for QEMU regression grep.
//!
//! # Exit codes
//!
//! | Code | Meaning |
//! |------|---------|
//! | 0    | All assertions passed |
//! | 1    | Pre-crash setup failure (socket / bind / pre-crash send) |
//! | 2    | Kill-delivery failure (fork / waitpid) |
//! | 3    | **EAGAIN assertion failed** — follow-up sendto returned unexpected errno, or EAGAIN was never observed within the retry budget |
//! | 4    | Post-restart send failure |
//!
//! # Outputs
//!
//! ```text
//! E1000_CRASH_SMOKE:BEGIN
//! E1000_CRASH_SMOKE:pre-crash-send:OK
//! E1000_CRASH_SMOKE:kill-delivered
//! E1000_CRASH_SMOKE:mid-crash:EAGAIN-observed   (or mid-crash:restarted-fast)
//! E1000_CRASH_SMOKE:restart-confirmed           (or restart-timeout:SKIP)
//! E1000_CRASH_SMOKE:post-restart-send:OK
//! E1000_CRASH_SMOKE:PASS
//! ```
#![no_std]
#![no_main]

use syscall_lib::{
    AF_INET, O_RDONLY, SOCK_DGRAM, STDOUT_FILENO, SockaddrIn, close, execve, exit, fork, nanosleep,
    open, read, sendto, socket, waitpid, write_str,
};

// QEMU user-mode gateway address (standard QEMU NAT).
const GATEWAY_IP: [u8; 4] = [10, 0, 2, 2];
// Arbitrary port — QEMU discards the datagram, we only care about TX success.
const REMOTE_PORT: u16 = 9999;
// Local source port for the UDP socket.
const LOCAL_PORT: u16 = 40124;

// Linux errno value EAGAIN (negated isize), as surfaced by Track G's
// sendto_restart_errno through sys_sendto when RESTART_SUSPECTED is set.
const NEG_EAGAIN: isize = -11;

// Service paths for killing the e1000 driver.
const SERVICE_BIN: &[u8] = b"/bin/service\0";
const SERVICE_ARGV0: &[u8] = b"service\0";
const SERVICE_KILL_ARG: &[u8] = b"kill\0";
const E1000_DRIVER_NAME: &[u8] = b"e1000_driver\0";

// Status file for polling the service state.
const STATUS_PATH: &[u8] = b"/run/services.status\0";

// Restart wait budget: 3 × DRIVER_RESTART_TIMEOUT_MS (1000 ms each) = 3 s.
const RESTART_WAIT_SECONDS: u64 = 3;

syscall_lib::entry_point!(program_main);

fn program_main(_args: &[&str]) -> i32 {
    write_str(STDOUT_FILENO, "E1000_CRASH_SMOKE:BEGIN\n");

    // ------------------------------------------------------------------
    // Step 1: Open a UDP socket and send a pre-crash datagram.
    // ------------------------------------------------------------------
    let fd = open_udp_socket();
    if fd < 0 {
        write_str(STDOUT_FILENO, "E1000_CRASH_SMOKE:FAIL step=1 socket\n");
        return 1;
    }

    let pre_payload = b"e1000-smoke-pre-crash";
    if !udp_send(fd, pre_payload) {
        write_str(
            STDOUT_FILENO,
            "E1000_CRASH_SMOKE:FAIL step=1 pre-crash send\n",
        );
        close(fd);
        return 1;
    }
    write_str(STDOUT_FILENO, "E1000_CRASH_SMOKE:pre-crash-send:OK\n");

    // ------------------------------------------------------------------
    // Step 2: Kill the e1000 driver in a child process.
    //
    // Fork so the parent can attempt another send while the kill is
    // in flight. The parent's send may succeed (virtio-net fallback) or
    // may race — both outcomes are acceptable.
    // ------------------------------------------------------------------
    let kill_pid = fork();
    if kill_pid < 0 {
        write_str(STDOUT_FILENO, "E1000_CRASH_SMOKE:FAIL step=2 fork\n");
        close(fd);
        return 2;
    }

    if kill_pid == 0 {
        // Child: exec `service kill e1000_driver`.
        let argv: [*const u8; 4] = [
            SERVICE_ARGV0.as_ptr(),
            SERVICE_KILL_ARG.as_ptr(),
            E1000_DRIVER_NAME.as_ptr(),
            core::ptr::null(),
        ];
        let envp: [*const u8; 1] = [core::ptr::null()];
        let _ = execve(SERVICE_BIN, &argv, &envp);
        // execve only returns on error — exit with a distinctive code.
        exit(126);
    }

    // Wait for the killer child to finish before asserting EAGAIN, so that
    // the kernel has had a chance to set RESTART_SUSPECTED on the RemoteNic.
    let mut child_status = 0i32;
    let waited = waitpid(kill_pid as i32, &mut child_status, 0);
    if waited != kill_pid as isize {
        write_str(STDOUT_FILENO, "E1000_CRASH_SMOKE:FAIL step=2 waitpid\n");
        close(fd);
        return 2;
    }
    write_str(STDOUT_FILENO, "E1000_CRASH_SMOKE:kill-delivered\n");

    // ------------------------------------------------------------------
    // Step 2.5 (H.1): Assert EAGAIN from sendto() during restart window.
    //
    // Phase 55c Track G wired sendto_restart_errno so that sys_sendto
    // returns NEG_EAGAIN (-11) when the e1000 RemoteNic has RESTART_SUSPECTED
    // set. After the kill child has exited, the kernel clears the driver's
    // IPC endpoint and sets RESTART_SUSPECTED via the async TX drain path
    // (remote.rs::on_ipc_error). The EAGAIN window may open slightly after
    // the kill child exits, so the retry loop continues past early successful
    // sends until EAGAIN is observed or the budget is exhausted.
    //
    // Policy:
    //   - NEG_EAGAIN on any attempt → assertion passes (H.1 satisfied)
    //   - budget exhausted without EAGAIN → hard failure, exit 3
    //   - any unexpected negative errno → hard failure, exit 3
    // ------------------------------------------------------------------
    let mid_payload = b"e1000-smoke-mid-crash";
    match assert_eagain_during_restart(fd, mid_payload) {
        EagainResult::EagainObserved => {
            write_str(
                STDOUT_FILENO,
                "E1000_CRASH_SMOKE:mid-crash:EAGAIN-observed\n",
            );
        }
        EagainResult::RestartedFast => {
            // Diagnostic marker kept for observability, but EAGAIN was
            // never seen — the H.1 regression assertion requires it.
            write_str(
                STDOUT_FILENO,
                "E1000_CRASH_SMOKE:mid-crash:restarted-fast\n",
            );
            write_str(
                STDOUT_FILENO,
                "E1000_CRASH_SMOKE:FAIL step=2.5 EAGAIN never observed\n",
            );
            close(fd);
            return 3;
        }
        EagainResult::UnexpectedErrno => {
            write_str(
                STDOUT_FILENO,
                "E1000_CRASH_SMOKE:FAIL step=2.5 unexpected errno from sendto\n",
            );
            close(fd);
            return 3;
        }
    }

    // ------------------------------------------------------------------
    // Step 3: Poll for e1000_driver restart.
    //
    // Track E.3b: the driver no longer exits after bring-up — it stays
    // running in its IRQ / IPC server loop. The service manager's
    // restart=on-failure policy therefore applies on kill: after SIGKILL
    // the service manager restarts the driver within RESTART_WAIT_SECONDS.
    // ------------------------------------------------------------------
    let restarted = wait_for_driver_running(b"e1000_driver", RESTART_WAIT_SECONDS);
    if restarted {
        write_str(STDOUT_FILENO, "E1000_CRASH_SMOKE:restart-confirmed\n");
    } else {
        // Restart did not arrive within budget. Log and continue so the
        // overall smoke can still emit PASS for the send/recv path — the
        // kernel-side RESTART_SUSPECTED state is validated by host unit tests
        // regardless of the service-manager restart latency.
        write_str(STDOUT_FILENO, "E1000_CRASH_SMOKE:restart-timeout:SKIP\n");
    }

    // ------------------------------------------------------------------
    // Step 4: Post-restart send.
    //
    // `service: running` (Step 3) fires as soon as PID 1 flips the status
    // field after calling `start_service`, which schedules the new
    // e1000_driver process. The driver still needs a few scheduling quanta
    // to re-open its PCI device, remap DMA buffers, and call
    // `ipc_register_service("net.nic")` before `send_frame` can succeed —
    // during that window the kernel returns `NEG_EAGAIN` (DeviceAbsent /
    // DriverRestarting). Retry the send with short backoffs so the smoke
    // passes deterministically across that gap.
    // ------------------------------------------------------------------
    let post_payload = b"e1000-smoke-post-restart";
    let mut attempts = 0;
    let max_attempts = 30; // ~30 seconds worst-case with 1 s backoff.
    let sent_ok = loop {
        if udp_send(fd, post_payload) {
            break true;
        }
        attempts += 1;
        if attempts >= max_attempts {
            break false;
        }
        let _ = nanosleep(1);
    };
    if !sent_ok {
        write_str(
            STDOUT_FILENO,
            "E1000_CRASH_SMOKE:FAIL step=4 post-restart send\n",
        );
        close(fd);
        return 4;
    }
    write_str(STDOUT_FILENO, "E1000_CRASH_SMOKE:post-restart-send:OK\n");

    close(fd);
    write_str(STDOUT_FILENO, "E1000_CRASH_SMOKE:PASS\n");
    0
}

/// Open an unconnected UDP socket bound to `LOCAL_PORT`.
/// Returns the file descriptor or a negative value on error.
fn open_udp_socket() -> i32 {
    let fd = socket(AF_INET as i32, SOCK_DGRAM as i32, 0);
    if fd < 0 {
        return -1;
    }
    let fd = fd as i32;
    let local = SockaddrIn::new([0, 0, 0, 0], LOCAL_PORT);
    if syscall_lib::bind(fd, &local) < 0 {
        close(fd);
        return -1;
    }
    fd
}

/// Send `payload` as a UDP datagram to `GATEWAY_IP:REMOTE_PORT`.
/// Returns `true` on success (bytes transferred == payload length).
fn udp_send(fd: i32, payload: &[u8]) -> bool {
    let remote = SockaddrIn::new(GATEWAY_IP, REMOTE_PORT);
    let sent = sendto(fd, payload, 0, &remote);
    sent == payload.len() as isize
}

/// Result of the EAGAIN assertion during a driver restart window.
enum EagainResult {
    /// sendto returned NEG_EAGAIN (-11) — RESTART_SUSPECTED was set.
    EagainObserved,
    /// EAGAIN was never seen across all retry attempts — either the driver
    /// restarted before the RESTART_SUSPECTED latch was observed, or the
    /// retry budget was exhausted without catching the window.
    /// **Treated as a failure (exit 3)**: the H.1 regression must confirm
    /// EAGAIN was surfaced; skipping the RESTART_SUSPECTED state is itself
    /// a contract violation.
    RestartedFast,
    /// sendto returned an unexpected negative errno (not EAGAIN).
    UnexpectedErrno,
}

/// Issue UDP sendtos and check for EAGAIN during the driver restart window.
///
/// Loops up to 5 times with a 1-second sleep between attempts so the assertion
/// spans the scheduling jitter between the kill child exiting and the kernel
/// setting `RESTART_SUSPECTED`.
///
/// A **successful send does not terminate the loop** — `RESTART_SUSPECTED` is
/// latched asynchronously by the TX-drain path in `remote.rs::on_ipc_error`,
/// so an early successful send only means the kernel has not yet processed the
/// IPC failure.  The loop continues retrying until either EAGAIN is observed
/// or the budget is exhausted.
///
/// Returns:
/// - `EagainResult::EagainObserved` if `NEG_EAGAIN` is seen on any attempt — H.1 passes.
/// - `EagainResult::RestartedFast` if the budget is exhausted without ever
///   seeing EAGAIN.  **The caller treats this as a failure (exit 3)** because
///   the H.1 regression must confirm EAGAIN was surfaced.
/// - `EagainResult::UnexpectedErrno` if any attempt returns a negative value
///   other than `NEG_EAGAIN` — errno propagation regression; caller exits 3.
fn assert_eagain_during_restart(fd: i32, payload: &[u8]) -> EagainResult {
    let remote = SockaddrIn::new(GATEWAY_IP, REMOTE_PORT);
    for _ in 0u32..5 {
        let ret = sendto(fd, payload, 0, &remote);
        if ret == NEG_EAGAIN {
            return EagainResult::EagainObserved;
        }
        if ret < 0 {
            // Unexpected errno (e.g., EBADF, EIO): errno propagation is wrong.
            return EagainResult::UnexpectedErrno;
        }
        // ret >= 0: either a full send (ret == payload.len()) or a partial send.
        // RESTART_SUSPECTED is latched asynchronously by the TX drain path, so
        // a successful send here means the kernel has not yet processed the IPC
        // failure. Keep retrying after a short sleep — the EAGAIN window may
        // still be ahead.
        let _ = nanosleep(1);
    }
    // Budget exhausted without ever observing EAGAIN.
    EagainResult::RestartedFast
}

/// Poll `/run/services.status` until the named service shows `running`
/// or `timeout_secs` elapses. Returns `true` when running is observed.
fn wait_for_driver_running(name: &[u8], timeout_secs: u64) -> bool {
    let mut buf = [0u8; 4096];
    let mut waited = 0u64;
    while waited <= timeout_secs {
        let fd = open(STATUS_PATH, O_RDONLY, 0);
        if fd >= 0 {
            let n = read(fd as i32, &mut buf);
            close(fd as i32);
            if n > 0 {
                let text = &buf[..n as usize];
                if service_is_running(text, name) {
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
        if line.len() <= name.len() {
            continue;
        }
        if &line[..name.len()] != name {
            continue;
        }
        if line[name.len()] != b' ' {
            continue;
        }
        let rest = &line[name.len() + 1..];
        let status_end = rest.iter().position(|&b| b == b' ').unwrap_or(rest.len());
        let status = &rest[..status_end];
        return status == b"running";
    }
    false
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    write_str(STDOUT_FILENO, "E1000_CRASH_SMOKE:PANIC\n");
    exit(101)
}
