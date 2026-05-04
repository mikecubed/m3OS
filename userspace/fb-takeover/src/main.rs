//! Phase 57d follow-up — Tier 1 fullscreen-takeover wrapper.
//!
//! Lets a legacy fullscreen program (e.g. doom) write directly to the
//! framebuffer while `display_server` is running, by orchestrating a
//! handshake on the `display-control` socket:
//!
//! 1. Resolve the `display-control` service handle (with backoff).
//! 2. Send `ControlCommand::YieldFb` — `display_server` calls
//!    `SYS_FB_YIELD` and pauses its compose loop until reclaim.
//! 3. `fork()` + `execve()` the requested program (argv passed
//!    through verbatim).
//! 4. `waitpid()` until the child terminates (any exit status).
//! 5. Send `ControlCommand::ReclaimFb` — `display_server` re-acquires
//!    via `SYS_FB_REACQUIRE` and marks every surface dirty so the
//!    next compose pass repaints the screen.
//!
//! The wrapper itself never touches the framebuffer — it only owns
//! the control-channel handshake. The takeover program is responsible
//! for calling `sys_framebuffer_acquire` on its own (today doom does
//! this in `DG_Init`).
//!
//! # Failure modes
//!
//! * Service lookup retry budget exhausted (`display_server` not up):
//!   exit 2, no syscalls issued.
//! * `YieldFb` returns `Error`: exit 3, child not spawned.
//! * `fork()` fails: best-effort `ReclaimFb` then exit 4.
//! * `execve()` fails inside the child: child exits 127; parent still
//!   issues `ReclaimFb` once `waitpid` returns.
//! * `ReclaimFb` returns `Error`: exit 5 — `display_server` is in a
//!   wedged state. Recoverable by sending `ReclaimFb` manually via
//!   `m3ctl` once the cause is resolved.
//!
//! # Composition with `m3ctl`
//!
//! `m3ctl` does not currently expose `yield-fb` / `reclaim-fb` verbs
//! because the verbs are most useful as an atomic
//! yield → run → reclaim sequence (a manual `m3ctl yield-fb` followed
//! by an arbitrary command leaves the screen wedged on crash). If a
//! future workflow wants the verbs separately, exposing them in
//! `m3ctl` is a one-line addition; the protocol opcodes already
//! encode/decode through `kernel-core::display::control`.
#![cfg_attr(feature = "os-binary", no_std)]
#![cfg_attr(feature = "os-binary", no_main)]
#![cfg_attr(feature = "os-binary", feature(alloc_error_handler))]

#[cfg(feature = "os-binary")]
extern crate alloc;

#[cfg(feature = "os-binary")]
mod os_binary {
    use alloc::vec;
    use alloc::vec::Vec;
    use core::alloc::Layout;

    use kernel_core::display::control::{
        ControlCommand, ControlEvent, decode_event, encode_command,
    };
    use m3ctl::{DISPLAY_CONTROL_SERVICE_NAME, LABEL_DISPLAY_CTL_CMD};
    use syscall_lib::heap::BrkAllocator;
    use syscall_lib::serial_print;

    #[global_allocator]
    static ALLOCATOR: BrkAllocator = BrkAllocator::new();

    #[alloc_error_handler]
    fn alloc_error(_layout: Layout) -> ! {
        serial_print("fb-takeover: alloc error\n");
        syscall_lib::exit(99)
    }

    /// Maximum bulk reply buffer — matches the kernel `MAX_BULK_LEN`.
    const MAX_BULK_BYTES: usize = 4096;

    /// Service-lookup retry attempts. Same shape as `m3ctl`.
    const SERVICE_LOOKUP_ATTEMPTS: u32 = 8;
    const SERVICE_LOOKUP_BACKOFF_NS: u32 = 5_000_000;

    /// Maximum argv length we forward to the child. The kernel's
    /// `execve` ABI imposes a smaller limit; this is just a sanity cap
    /// for the per-arg null-terminated copy buffer.
    const MAX_CHILD_ARGV: usize = 32;

    /// Per-arg byte budget. Must match what doom-style programs expect
    /// — long paths and -warp arguments still fit comfortably.
    const MAX_ARG_BYTES: usize = 256;

    syscall_lib::entry_point!(program_main);

    fn program_main(args: &[&str]) -> i32 {
        if args.len() < 2 {
            print_usage();
            return 2;
        }

        // argv[0] is `fb-takeover`; argv[1..] is the child command.
        let child_argv_strs: &[&str] = &args[1..];
        if child_argv_strs.len() > MAX_CHILD_ARGV {
            print_str("fb-takeover: too many child arguments\n");
            return 2;
        }

        let handle = match lookup_with_backoff(DISPLAY_CONTROL_SERVICE_NAME) {
            Some(h) => h,
            None => {
                print_str("fb-takeover: display-control service not available\n");
                return 2;
            }
        };

        // Step 2 — yield the framebuffer.
        if !send_and_expect_ack(handle, &ControlCommand::YieldFb, "YieldFb") {
            return 3;
        }
        print_str("fb-takeover: yielded; spawning child\n");

        // Step 3 — fork + exec the child.
        let pid = syscall_lib::fork();
        if pid < 0 {
            print_str("fb-takeover: fork failed\n");
            // Best-effort reclaim so display_server isn't left wedged.
            let _ = send_and_expect_ack(handle, &ControlCommand::ReclaimFb, "ReclaimFb");
            return 4;
        }

        if pid == 0 {
            // Child — exec the requested program.
            child_exec(child_argv_strs);
            // child_exec never returns on success.
            syscall_lib::exit(127);
        }

        // Step 4 — parent waits for the child to terminate. We accept
        // any exit status; the takeover program may fail for its own
        // reasons (missing WAD, etc.) and we still need to reclaim.
        let mut status: i32 = 0;
        let waited = syscall_lib::waitpid(pid as i32, &mut status, 0);
        if waited < 0 {
            print_str("fb-takeover: waitpid failed (still attempting reclaim)\n");
        } else {
            print_str("fb-takeover: child exited; reclaiming\n");
        }

        // Step 5 — reclaim. Always run, even if waitpid failed, so the
        // server state matches reality.
        if !send_and_expect_ack(handle, &ControlCommand::ReclaimFb, "ReclaimFb") {
            return 5;
        }

        if waited < 0 { 1 } else { 0 }
    }

    /// Encode `cmd`, send it on the control endpoint, decode the reply
    /// and assert it is `Ack`. Returns `true` on success. On any
    /// transport, encode, or non-Ack reply, prints a diagnostic and
    /// returns `false`.
    fn send_and_expect_ack(handle: u32, cmd: &ControlCommand, label: &str) -> bool {
        let mut req_buf = [0u8; 16];
        let req_len = match encode_command(cmd, &mut req_buf) {
            Ok(n) => n,
            Err(_) => {
                print_str("fb-takeover: failed to encode ");
                print_str(label);
                print_str("\n");
                return false;
            }
        };
        let reply_label =
            syscall_lib::ipc_call_buf(handle, LABEL_DISPLAY_CTL_CMD, 0, &req_buf[..req_len]);
        if reply_label == u64::MAX {
            print_str("fb-takeover: ipc_call_buf failed for ");
            print_str(label);
            print_str("\n");
            return false;
        }
        let mut reply_buf = vec![0u8; MAX_BULK_BYTES];
        let n = syscall_lib::ipc_take_pending_bulk(&mut reply_buf);
        if n == u64::MAX {
            print_str("fb-takeover: ipc_take_pending_bulk failed\n");
            return false;
        }
        if n == 0 {
            // No bulk payload — display_server staged a label-only
            // reply. Treat as success: the control endpoint dispatched
            // the verb, and YieldFb / ReclaimFb side-effects already
            // ran on the server side before the reply was emitted.
            return true;
        }
        match decode_event(&reply_buf[..n as usize]) {
            Ok((ControlEvent::Ack, _)) => true,
            Ok((ControlEvent::Error { .. }, _)) => {
                print_str("fb-takeover: server returned Error for ");
                print_str(label);
                print_str("\n");
                false
            }
            Ok(_) => {
                print_str("fb-takeover: unexpected reply event for ");
                print_str(label);
                print_str("\n");
                false
            }
            Err(_) => {
                print_str("fb-takeover: failed to decode reply for ");
                print_str(label);
                print_str("\n");
                false
            }
        }
    }

    /// Build the null-terminated argv arrays expected by `execve` and
    /// hand off control. Never returns on success; on failure (alloc
    /// fail, path too long, execve errno) prints a diagnostic and
    /// falls back to the caller, which exits 127.
    ///
    /// If the program name has no `/` in it, we prefix `/bin/` — the
    /// kernel `execve` resolves names relative to cwd (no PATH lookup
    /// in-kernel), so a bare `doom` would otherwise be looked up at
    /// `/doom` and ENOENT. This mirrors the convention every m3OS
    /// daemon launcher uses (`/bin/<name>`).
    fn child_exec(child_argv_strs: &[&str]) {
        let raw_path = child_argv_strs[0];
        // Enforce per-arg length so the per-string null-terminated
        // copy stays bounded.
        for arg in child_argv_strs {
            if arg.len() >= MAX_ARG_BYTES {
                print_str("fb-takeover: child arg exceeds length budget\n");
                return;
            }
        }

        // Path must be null-terminated. Resolve relative names by
        // prefixing `/bin/`.
        let needs_bin_prefix = !raw_path.starts_with('/') && !raw_path.contains('/');
        let path_buf: Vec<u8> = if needs_bin_prefix {
            let mut buf = Vec::with_capacity(5 + raw_path.len() + 1);
            buf.extend_from_slice(b"/bin/");
            buf.extend_from_slice(raw_path.as_bytes());
            buf.push(0);
            buf
        } else {
            let mut buf = Vec::with_capacity(raw_path.len() + 1);
            buf.extend_from_slice(raw_path.as_bytes());
            buf.push(0);
            buf
        };
        print_str("fb-takeover: execve ");
        if let Ok(s) = core::str::from_utf8(&path_buf[..path_buf.len() - 1]) {
            print_str(s);
        }
        print_str("\n");

        // Per-arg storage: vec of vec<u8>, each null-terminated.
        let mut arg_storage: Vec<Vec<u8>> = Vec::with_capacity(child_argv_strs.len());
        for arg in child_argv_strs {
            let mut buf: Vec<u8> = Vec::with_capacity(arg.len() + 1);
            buf.extend_from_slice(arg.as_bytes());
            buf.push(0);
            arg_storage.push(buf);
        }

        // argv array: pointer per arg, null sentinel at end.
        let mut argv: Vec<*const u8> = Vec::with_capacity(child_argv_strs.len() + 1);
        for s in &arg_storage {
            argv.push(s.as_ptr());
        }
        argv.push(core::ptr::null());

        // Empty environment (envp = [null]). The takeover program
        // inherits no env vars from the wrapper today; if a future
        // program needs the parent env, threading it through the
        // entry-point-with-env macro is straightforward.
        let envp: [*const u8; 1] = [core::ptr::null()];

        let _rc = syscall_lib::execve(&path_buf, &argv, &envp);
        // execve only returns on failure.
        print_str("fb-takeover: execve failed\n");
    }

    fn lookup_with_backoff(name: &str) -> Option<u32> {
        for attempt in 0..SERVICE_LOOKUP_ATTEMPTS {
            let raw = syscall_lib::ipc_lookup_service(name);
            if raw != u64::MAX {
                return Some(raw as u32);
            }
            if attempt + 1 == SERVICE_LOOKUP_ATTEMPTS {
                return None;
            }
            let _ = syscall_lib::nanosleep_for(0, SERVICE_LOOKUP_BACKOFF_NS);
        }
        None
    }

    /// Diagnostic output — routed to the kernel serial log via
    /// `SYS_DEBUG_PRINT` rather than stdout, because between
    /// `YieldFb` and `ReclaimFb` the framebuffer is owned by the
    /// takeover program and the wrapper's PTY-backed stdout is not
    /// visible. Using serial means every fb-takeover diagnostic shows
    /// up in `m3os.log` regardless of FB ownership state.
    fn print_str(s: &str) {
        serial_print(s);
    }

    fn print_usage() {
        print_str(
            "Usage: fb-takeover <program> [args...]\n\
             \n\
             Yields the display_server framebuffer, runs <program> with the given\n\
             arguments, then reclaims the framebuffer once the program exits.\n\
             \n\
             Example: fb-takeover /bin/doom\n",
        );
    }

    #[panic_handler]
    fn panic(_: &core::panic::PanicInfo) -> ! {
        serial_print("fb-takeover: PANIC\n");
        syscall_lib::exit(101)
    }
}

#[cfg(not(feature = "os-binary"))]
fn main() {}
