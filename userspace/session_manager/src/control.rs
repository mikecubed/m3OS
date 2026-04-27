//! Phase 57 Track F.5 — control socket dispatcher (STUB for F.2).
//!
//! Per the F.5 acceptance:
//!
//! > Control socket lives on a separate AF_UNIX path consistent with
//! > the Phase 56 control-socket precedent. Verbs: `session-state`
//! > (returns the current `SessionState`), `session-stop` (graceful
//! > shutdown, falls through to `text-fallback`), `session-restart`
//! > (graceful stop + start). Access control follows the Phase 56
//! > m3ctl precedent: capability-based — the connecting peer must
//! > hold the `session_manager` control-socket cap.
//!
//! F.2 leaves this module as a STUB so the daemon's main event loop
//! can multiplex it alongside supervisor events without F.5's
//! semantics being committed. The real verb dispatcher lands in F.5.
//!
//! The stub:
//! - Creates an IPC endpoint via `syscall_lib::create_endpoint`.
//! - Registers it as `"session-control"` so a future `m3ctl` client
//!   can locate the same name F.5 will keep using.
//! - In [`poll_control_once`], non-blocking-recvs one message and
//!   replies with a zero-bulk Ack. No verb decoding (that's F.5).
//!
//! This is intentionally minimal: the F.2 acceptance only requires the
//! daemon to be a "Phase 56-style single-threaded event loop
//! multiplexing supervisor events and a control socket". The control
//! socket is wired but inert; F.5 fills the verb dispatcher in.

use syscall_lib::IpcMessage;

/// Service-registry name of the control endpoint. Stable across F.2
/// (this stub) and F.5 (real dispatcher) so a future `m3ctl
/// session-state` can be wired to the same name on both phases.
pub const CONTROL_SERVICE_NAME: &str = "session-control";

/// Reply-cap handle the kernel writes when a recv produces a message.
/// Mirrors the constant `kbd_server` and `display_server` use locally;
/// `syscall_lib` does not export it, so each daemon names it once.
const REPLY_CAP_HANDLE: u32 = 1;

/// Holder for the control-socket endpoint's cap-handle. Constructed
/// once at startup; passed to [`poll_control_once`] each event-loop
/// iteration.
pub struct ControlSocket {
    /// Cap-handle of the registered endpoint. `None` if registration
    /// failed at startup — the daemon continues but the control socket
    /// is dormant. Failures are observable via the startup log line
    /// emitted by [`bind_control_socket`].
    ep_handle: Option<u32>,
}

impl ControlSocket {
    /// A control socket whose endpoint registration has not yet been
    /// attempted. Use [`bind_control_socket`] for the production path.
    pub const fn dormant() -> Self {
        Self { ep_handle: None }
    }

    /// Whether the endpoint is bound and ready to receive. F.5 will
    /// consult this before honoring verbs that require a bound socket.
    #[allow(dead_code)] // F.5 consumer.
    pub fn is_bound(&self) -> bool {
        self.ep_handle.is_some()
    }

    /// The endpoint cap-handle, if bound. F.5 will consume this when
    /// the verb dispatcher needs to issue replies.
    #[allow(dead_code)] // F.5 consumer.
    pub fn ep_handle(&self) -> Option<u32> {
        self.ep_handle
    }
}

/// Bind the control endpoint and register it under
/// [`CONTROL_SERVICE_NAME`]. On failure, returns a dormant socket and
/// emits a structured `session.control` log line. The daemon
/// continues without the control surface — this matches the Phase 56
/// pattern where `display_server` continues without input if the
/// kbd/mouse services are unavailable.
pub fn bind_control_socket() -> ControlSocket {
    let raw = syscall_lib::create_endpoint();
    if raw == u64::MAX {
        syscall_lib::write_str(
            syscall_lib::STDOUT_FILENO,
            "session_manager: session.control: create_endpoint failed; control socket dormant\n",
        );
        return ControlSocket::dormant();
    }
    let ep = raw as u32;
    let reg = syscall_lib::ipc_register_service(ep, CONTROL_SERVICE_NAME);
    if reg == u64::MAX {
        syscall_lib::write_str(
            syscall_lib::STDOUT_FILENO,
            "session_manager: session.control: register failed; control socket dormant\n",
        );
        return ControlSocket::dormant();
    }
    syscall_lib::write_str(
        syscall_lib::STDOUT_FILENO,
        "session_manager: session.control: registered as 'session-control' (F.5 stub)\n",
    );
    ControlSocket {
        ep_handle: Some(ep),
    }
}

/// Non-blocking poll of the control socket. Returns `true` if a
/// request was handled this iteration, `false` if the queue was empty
/// (the normal idle path) or the socket is dormant.
///
/// F.2 STUB behavior: replies to every received label with a zero-bulk
/// reply (label = sentinel value `u64::MAX`) so the connecting client
/// observes a typed "verb not yet implemented" reply rather than
/// hanging indefinitely. F.5 replaces this with the real
/// `session-state` / `session-stop` / `session-restart` dispatcher.
pub fn poll_control_once(socket: &ControlSocket) -> bool {
    let Some(ep) = socket.ep_handle else {
        return false;
    };
    let mut msg = IpcMessage::new(0);
    let mut buf = [0u8; 64];
    let label = syscall_lib::ipc_try_recv_msg(ep, &mut msg, &mut buf);
    if label == u64::MAX {
        // Either an empty queue (the normal case) or a copy fault.
        // We cannot distinguish without an extra syscall; F.5's real
        // dispatcher will revisit observability here.
        return false;
    }
    // Reply with the F.5-not-yet-implemented sentinel. The reply-cap
    // slot is freed by the kernel after `ipc_reply`.
    syscall_lib::write_str(
        syscall_lib::STDOUT_FILENO,
        "session_manager: session.control: STUB received verb (F.5 will dispatch)\n",
    );
    let _ = syscall_lib::ipc_reply(REPLY_CAP_HANDLE, u64::MAX, 0);
    true
}
