//! `term` binary entry point — Phase 57 Track G end-to-end wiring.
//!
//! `term` is the Phase 57 graphical terminal emulator. The binary
//! composes the lib pieces (`PtyHost`, `Screen`, `Renderer`,
//! `InputHandler`, `Bell`) into a single-threaded event loop:
//!
//! 1. Open an IPC endpoint and register `"term"` so `session_manager`
//!    can observe the boot step.
//! 2. Connect to `display_server` (Hello + `CreateSurface` +
//!    `SetSurfaceRole(Toplevel)`) via [`DisplayClient`].
//! 3. Open a PTY pair via the production [`SyscallPtyOps`], fork +
//!    `execve` `/bin/ion` (with `/bin/sh0` fallback) on the secondary
//!    side, set the primary nonblocking.
//! 4. Loop: drain PTY reads → ANSI parser → screen state → render
//!    commands; pull `KeyEvent`s from `display_server`'s C.5 outbound
//!    queue → input handler → PTY writes; ring the bell on `Bell`
//!    commands; compose dirty frames.
//! 5. Exit zero on shell exit so the supervisor restarts per
//!    `term.conf`.
//!
//! `cfg(not(test))` gates protect the OS-only entry point so
//! `cargo test -p term --target x86_64-unknown-linux-gnu --lib`
//! continues to compile on the host.

#![cfg_attr(not(test), no_std)]
#![cfg_attr(not(test), no_main)]
#![cfg_attr(not(test), feature(alloc_error_handler))]

extern crate alloc;
#[cfg(test)]
extern crate std;

#[cfg(not(test))]
use core::alloc::Layout;

#[cfg(not(test))]
use kernel_core::display::protocol::ServerMessage;
#[cfg(not(test))]
use syscall_lib::heap::BrkAllocator;
#[cfg(not(test))]
use syscall_lib::{CLOCK_MONOTONIC, STDOUT_FILENO};

#[cfg(not(test))]
use term::bell::{AudioClientBellSink, AudioUnavailableBellSink, Bell, BellError};
#[cfg(not(test))]
use term::display::DisplayClient;
#[cfg(not(test))]
use term::input::{InputHandler, PtyWriter};
#[cfg(not(test))]
use term::pty::PtyHost;
#[cfg(not(test))]
use term::render::Renderer;
#[cfg(not(test))]
use term::screen::{RenderCommand, Screen};
#[cfg(not(test))]
use term::syscall_pty::SyscallPtyOps;
#[cfg(not(test))]
use term::{BOOT_LOG_MARKER, READY_SENTINEL, SERVICE_NAME};

#[cfg(not(test))]
#[global_allocator]
static ALLOCATOR: BrkAllocator = BrkAllocator::new();

#[cfg(not(test))]
#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "term: alloc error\n");
    syscall_lib::exit(99)
}

#[cfg(not(test))]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "term: PANIC\n");
    syscall_lib::exit(101)
}

#[cfg(not(test))]
syscall_lib::entry_point!(program_main);

/// Phase 56 C.5 close-out — IPC label term sends to drain one
/// queued `ServerMessage` from `display_server`. Mirrors
/// `display_server::client::LABEL_CLIENT_EVENT_PULL`. The
/// complementary `LABEL_CLIENT_EVENT_NONE = 4` is the server's
/// reply when the queue is empty; term checks against equality with
/// `LABEL_CLIENT_EVENT_PULL` rather than naming `_NONE` separately.
#[cfg(not(test))]
const LABEL_CLIENT_EVENT_PULL: u64 = 3;

/// Per-iteration sleep when no work was found this tick. Mirrors the
/// `display_server` main-loop yield (1 ms → ~1000 polls/sec).
#[cfg(not(test))]
const IDLE_SLEEP_NS: u32 = 1_000_000;

/// Bytes-per-iteration drain cap on the PTY primary fd. Big enough to
/// cover a typical shell prompt + output line; small enough that one
/// noisy program cannot starve the input + render passes.
#[cfg(not(test))]
const PTY_READ_CHUNK: usize = 256;

#[cfg(not(test))]
fn program_main(_args: &[&str]) -> i32 {
    syscall_lib::write_str(STDOUT_FILENO, BOOT_LOG_MARKER);

    // 1. Open an IPC endpoint and register so `session_manager`
    //    observes this step. `term` does not yet accept inbound IPC
    //    traffic on this endpoint; it is a presence beacon.
    let ep = syscall_lib::create_endpoint();
    if ep == u64::MAX {
        syscall_lib::write_str(STDOUT_FILENO, "term: create_endpoint failed\n");
        return 2;
    }
    let ep_u32 = match u32::try_from(ep) {
        Ok(v) => v,
        Err(_) => {
            syscall_lib::write_str(STDOUT_FILENO, "term: endpoint id out of u32 range\n");
            return 3;
        }
    };
    let rc = syscall_lib::ipc_register_service(ep_u32, SERVICE_NAME);
    if rc == u64::MAX {
        syscall_lib::write_str(STDOUT_FILENO, "term: ipc_register_service failed\n");
        return 4;
    }

    // 2. Connect to display_server. Without it term has nothing to
    //    paint to; surface DisplayServerUnavailable cleanly.
    let display = match DisplayClient::connect() {
        Ok(d) => d,
        Err(_) => {
            syscall_lib::write_str(STDOUT_FILENO, "term: display_server unavailable\n");
            return 5;
        }
    };
    // Capture the display endpoint handle for the C.5 event-pull
    // path. `DisplayClient` did the lookup; we re-look-up here so
    // term can hold its own borrow without aliasing.
    let display_handle = match lookup_display_for_input() {
        Some(h) => h,
        None => {
            syscall_lib::write_str(STDOUT_FILENO, "term: display lookup for input failed\n");
            return 5;
        }
    };

    // 3. Open the PTY pair and spawn the production shell on the
    //    secondary side. `SyscallPtyOps::exec_shell` execve's
    //    `/bin/ion` first (matching `/etc/passwd`'s default and the
    //    path `login` exec's), falling back to `/bin/sh0` if ion
    //    is missing or broken.
    //    `SyscallPtyOps` is the production wiring of the
    //    `PtyOps` trait the lib already exercises against
    //    `MockPtyOps`.
    let mut pty = PtyHost::new(SyscallPtyOps::new());
    if let Err(_e) = pty.open_and_spawn() {
        syscall_lib::write_str(STDOUT_FILENO, "term: PTY open / shell spawn failed\n");
        return 6;
    }
    let primary_fd = match pty.primary_fd() {
        Some(fd) => fd,
        None => {
            syscall_lib::write_str(STDOUT_FILENO, "term: PtyHost has no primary fd\n");
            return 6;
        }
    };
    if syscall_lib::set_nonblocking(primary_fd) < 0 {
        syscall_lib::write_str(STDOUT_FILENO, "term: set_nonblocking failed\n");
        return 6;
    }

    // 4. Compose the screen state machine, the renderer, the input
    //    translator, and the bell. Bell starts on the production
    //    AudioClientBellSink; on first AudioUnavailable we swap
    //    permanently to the warn-once stub so noisy bell-loops do
    //    not retry the audio path forever.
    let mut screen = Screen::new();
    let mut renderer = Renderer::new(display);
    let mut input_handler = InputHandler::new();
    let mut bell_audio = Some(Bell::new(AudioClientBellSink::new()));
    let mut bell_unavail: Option<Bell<AudioUnavailableBellSink>> = None;
    let mut render_cmds: alloc::vec::Vec<RenderCommand> = alloc::vec::Vec::new();
    let mut event_buf = [0u8; 64];
    let mut pty_buf = [0u8; PTY_READ_CHUNK];
    let mut writer = PrimaryFdWriter {
        fd: primary_fd,
        warned: false,
    };
    let clock = MonotonicClock;

    syscall_lib::write_str(STDOUT_FILENO, READY_SENTINEL);

    // 5. Event loop. Single-threaded; multiplexes the PTY drain, the
    //    display_server outbound-event drain, the bell, the shell-exit
    //    poll, and the renderer's per-tick compose.
    loop {
        let mut did_work = false;

        // 5a. Drain the PTY primary fd. Nonblocking: -EAGAIN means
        //     no data this tick. 0 means the shell closed its end.
        let n = syscall_lib::read(primary_fd, &mut pty_buf);
        if n > 0 {
            did_work = true;
            for &byte in &pty_buf[..n as usize] {
                screen.feed(byte, &mut render_cmds);
            }
            for cmd in render_cmds.drain(..) {
                if matches!(cmd, RenderCommand::Bell) {
                    ring_bell(&mut bell_audio, &mut bell_unavail, clock.now_ms());
                } else {
                    renderer.apply(cmd);
                }
            }
        } else if n == 0 {
            // EOF on primary — the shell closed the slave; treat it
            // as shell exit and break.
            syscall_lib::write_str(STDOUT_FILENO, "term: PTY EOF; shell closed\n");
            break;
        }
        // n < 0 path: either -EAGAIN (no data) or a hard error. We
        // do not distinguish today; the next iteration retries.

        // 5b. Drain one queued ServerMessage from display_server.
        //     The C.5 outbound queue per-client cap is 128; a busy
        //     keyboard cannot starve the renderer because we drain
        //     at most one event per tick.
        match pull_one_event(display_handle, &mut event_buf) {
            PulledEvent::Key(ev) => {
                did_work = true;
                input_handler.translate(&ev, &mut writer);
            }
            PulledEvent::Disconnect => {
                syscall_lib::write_str(STDOUT_FILENO, "term: display_server disconnect\n");
                break;
            }
            PulledEvent::None => {}
        }

        // 5c. Poll shell exit. `Some(_)` ⇒ child exited (cleanly or
        //     not); break out of the loop.
        match pty.poll_shell_exit() {
            Ok(Some(_status)) => {
                syscall_lib::write_str(STDOUT_FILENO, "term: shell exited\n");
                break;
            }
            Ok(None) => {}
            Err(_) => {
                syscall_lib::write_str(STDOUT_FILENO, "term: poll_shell_exit error\n");
                break;
            }
        }

        // 5d. Compose dirty frame, if any.
        if renderer.damaged() {
            renderer.compose();
            did_work = true;
        }

        // 5e. Yield briefly when nothing happened so we don't burn CPU.
        if !did_work {
            let _ = syscall_lib::nanosleep_for(0, IDLE_SLEEP_NS);
        }
    }

    // Shell exited (or PTY EOF / unrecoverable error). Close the
    // primary fd cleanly so the kernel reclaims the slot, then exit
    // zero — the supervisor's `restart=on-failure` policy lets it
    // re-spawn term once the shell or display state recovers.
    pty.close_primary();
    0
}

/// Production [`PtyWriter`] — wraps `syscall_lib::write` against the
/// PTY primary fd. The input handler has no recovery for a failing
/// write (the byte is already gone from the input queue), but the
/// failure is observable through the boot transcript so a developer
/// can correlate "shell looks deaf" with "term: PTY write error
/// errno=-X". The `warned` flag rate-limits the log line to once per
/// "stuck" episode — a chronic write failure (e.g. shell exited and
/// PTY EOF'd) would otherwise spam the serial console on every
/// keystroke.
#[cfg(not(test))]
struct PrimaryFdWriter {
    fd: i32,
    warned: bool,
}

#[cfg(not(test))]
impl PtyWriter for PrimaryFdWriter {
    fn write(&mut self, bytes: &[u8]) {
        let rc = syscall_lib::write(self.fd, bytes);
        if rc < 0 {
            if !self.warned {
                syscall_lib::write_str(STDOUT_FILENO, "term: PTY write error\n");
                self.warned = true;
            }
            return;
        }
        // Successful write resets the warned flag so a transient
        // failure followed by recovery still produces a fresh log
        // line if the failure recurs.
        self.warned = false;
    }
}

/// Monotonic clock for [`Bell::ring`]. Tiny wrapper around
/// `clock_gettime(CLOCK_MONOTONIC)` so the bell call site is
/// self-documenting without spending a trait abstraction on a
/// single-method type.
#[cfg(not(test))]
#[derive(Clone, Copy)]
struct MonotonicClock;

#[cfg(not(test))]
impl MonotonicClock {
    fn now_ms(self) -> u64 {
        let (sec, nsec) = syscall_lib::clock_gettime(CLOCK_MONOTONIC);
        let sec = sec.max(0) as u64;
        let nsec = nsec.max(0) as u64;
        sec.saturating_mul(1000).saturating_add(nsec / 1_000_000)
    }
}

/// Ring the bell using whichever sink is currently active. On the
/// first `AudioUnavailable` from `AudioClientBellSink`, swap the
/// `Bell` permanently to the warn-once stub so a tight bell loop
/// does not re-attempt the audio path on every ring.
#[cfg(not(test))]
fn ring_bell(
    audio: &mut Option<Bell<AudioClientBellSink>>,
    unavail: &mut Option<Bell<AudioUnavailableBellSink>>,
    now_ms: u64,
) {
    if let Some(b) = audio.as_mut() {
        match b.ring(now_ms) {
            Ok(_) => return,
            Err(BellError::AudioUnavailable) => {
                // Permanently downgrade.
                *audio = None;
                *unavail = Some(Bell::new(AudioUnavailableBellSink::new()));
            }
            Err(_) => return,
        }
    }
    if let Some(b) = unavail.as_mut() {
        let _ = b.ring(now_ms);
    }
}

/// Outcome of one [`pull_one_event`] call. Pure data so the main
/// loop's match remains exhaustive and a future variant addition
/// fails to compile rather than silently dropping events.
#[cfg(not(test))]
enum PulledEvent {
    /// A `KeyEvent` for the input handler to translate.
    Key(kernel_core::input::events::KeyEvent),
    /// `display_server` told us the connection is closing — exit
    /// cleanly so the supervisor can restart per `term.conf`.
    Disconnect,
    /// No event this tick (`LABEL_CLIENT_EVENT_NONE`, transport
    /// error, decode failure, or a `ServerMessage` term doesn't
    /// consume — `Pointer`, `Welcome`, `SurfaceConfigured`,
    /// `FocusIn` / `FocusOut`, `BufferReleased`, `SurfaceDestroyed`).
    /// All of these are non-fatal and the next iteration retries.
    None,
}

/// Pull one queued `ServerMessage` from `display_server`'s C.5
/// outbound queue and classify it for the main loop.
///
/// Disconnect is the only non-Key variant that changes behaviour:
/// it asks term to exit. Every other variant is dropped with no
/// state change because term's contract today is "Toplevel surface
/// + keyboard-focused PTY" — pointer events, focus changes, and
/// buffer-released are not load-bearing for that contract. A
/// future track that adds e.g. mouse-aware shell selection would
/// thread `Pointer` into the input handler here.
#[cfg(not(test))]
fn pull_one_event(display_handle: u32, buf: &mut [u8]) -> PulledEvent {
    let label = syscall_lib::ipc_call(display_handle, LABEL_CLIENT_EVENT_PULL, 0);
    if label != LABEL_CLIENT_EVENT_PULL {
        // LABEL_CLIENT_EVENT_NONE (= 4) or transport error — no
        // event. Even on the NONE path the kernel may have staged
        // an empty bulk; drain to keep the slot clean for the next
        // call.
        let _ = syscall_lib::ipc_take_pending_bulk(buf);
        return PulledEvent::None;
    }
    let n = syscall_lib::ipc_take_pending_bulk(buf);
    if n == 0 || n == u64::MAX {
        return PulledEvent::None;
    }
    let len = n as usize;
    if len > buf.len() {
        return PulledEvent::None;
    }
    match ServerMessage::decode(&buf[..len]) {
        Ok((ServerMessage::Key(ev), _)) => PulledEvent::Key(ev),
        Ok((ServerMessage::Disconnect { .. }, _)) => PulledEvent::Disconnect,
        // Pointer / Welcome / FocusIn / FocusOut / SurfaceConfigured /
        // SurfaceDestroyed / BufferReleased: not load-bearing for
        // term's contract — drop silently.
        Ok(_) => PulledEvent::None,
        Err(_) => PulledEvent::None,
    }
}

/// Mirror of `DisplayClient`'s lookup-with-backoff so the input loop
/// can hold its own handle on the `"display"` service. `connect`
/// already paid the boot-time backoff cost; this call is expected to
/// resolve on the first attempt.
#[cfg(not(test))]
fn lookup_display_for_input() -> Option<u32> {
    let raw = syscall_lib::ipc_lookup_service("display");
    if raw == u64::MAX {
        return None;
    }
    Some(raw as u32)
}
