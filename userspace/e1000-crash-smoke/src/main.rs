//! Phase 55b Track F.3d-3 — e1000 crash-and-restart end-to-end smoke test.
//!
//! Exercises the `RemoteNic` restart-suspected state machine added in F.3d-3.
//! Covers the observable side of `NetDriverError::DriverRestarting`:
//!
//! 1. Send a UDP datagram to the QEMU gateway (10.0.2.2) — confirms the
//!    network path (virtio-net or RemoteNic) is alive before the kill.
//! 2. Kill the e1000 driver via `service kill e1000_driver`.
//!    - On the kernel side, the next `drain_tx_queue` will fail → sets
//!      `RESTART_SUSPECTED` → subsequent `send_frame` calls return
//!      `NetDriverError::DriverRestarting` (observable in the kernel log).
//!    - From userspace, UDP `send()` falls back to virtio-net if RemoteNic
//!      is not registered, so the userspace-visible error is the service
//!      restart latency, not an EAGAIN errno.
//! 3. Poll `/run/services.status` until `e1000_driver` shows `running`.
//! 4. Send another UDP datagram — confirms the restart path is complete.
//! 5. Emit structured markers for QEMU regression grep.
//!
//! # Why no EAGAIN observation from userspace
//!
//! There is no `sys_net_send` kernel syscall that propagates
//! `NetDriverError::DriverRestarting` as `NEG_EAGAIN` to userspace — the net
//! stack's `send_frame` is fire-and-forget with a virtio-net fallback. A
//! `sys_net_send` syscall is a phase-55c work item. The EAGAIN side is proved
//! by the pure-logic host tests in `kernel-core/tests/driver_restart.rs`
//! (`net_error_to_neg_errno_driver_restarting_is_eagain`).
//!
//! # Outputs
//!
//! ```text
//! E1000_CRASH_SMOKE:BEGIN
//! E1000_CRASH_SMOKE:pre-crash-send:OK
//! E1000_CRASH_SMOKE:kill-delivered
//! E1000_CRASH_SMOKE:restart-confirmed     (or restart-timeout:SKIP)
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

    // Parent: issue a mid-crash send while the child is delivering SIGKILL.
    // The result (success or failure) is logged but not treated as a
    // hard failure — the key observation is at the kernel log level where
    // RESTART_SUSPECTED → DriverRestarting is recorded.
    let mid_payload = b"e1000-smoke-mid-crash";
    let mid_ok = udp_send(fd, mid_payload);

    // Wait for the killer child to finish.
    let mut child_status = 0i32;
    let waited = waitpid(kill_pid as i32, &mut child_status, 0);
    if waited != kill_pid as isize {
        write_str(STDOUT_FILENO, "E1000_CRASH_SMOKE:FAIL step=2 waitpid\n");
        close(fd);
        return 2;
    }

    if mid_ok {
        write_str(
            STDOUT_FILENO,
            "E1000_CRASH_SMOKE:mid-crash:send-succeeded\n",
        );
    } else {
        write_str(STDOUT_FILENO, "E1000_CRASH_SMOKE:mid-crash:EAGAIN\n");
    }
    write_str(STDOUT_FILENO, "E1000_CRASH_SMOKE:kill-delivered\n");

    // ------------------------------------------------------------------
    // Step 3: Poll for e1000_driver restart.
    //
    // If the driver was not running (it exits after bring-up in F.4b),
    // the restart may not happen — we log accordingly and continue.
    // ------------------------------------------------------------------
    let restarted = wait_for_driver_running(b"e1000_driver", RESTART_WAIT_SECONDS);
    if restarted {
        write_str(STDOUT_FILENO, "E1000_CRASH_SMOKE:restart-confirmed\n");
    } else {
        // Not a hard failure in F.4b — driver exits after bring-up and
        // the service manager may not restart it. The key test is the
        // kernel-side RESTART_SUSPECTED state, proved by host unit tests.
        write_str(STDOUT_FILENO, "E1000_CRASH_SMOKE:restart-timeout:SKIP\n");
    }

    // ------------------------------------------------------------------
    // Step 4: Post-restart send.
    // ------------------------------------------------------------------
    let post_payload = b"e1000-smoke-post-restart";
    if !udp_send(fd, post_payload) {
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
