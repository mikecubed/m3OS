//! Phase 57 Track F.5 — control-socket verb dispatcher.
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
//! ## Wire shape
//!
//! Request: a single tag byte (the [`ControlVerb`] discriminant)
//! delivered as the **bulk** of an `ipc_call`. The IPC message **label**
//! must be [`LABEL_CTL_CMD`] = 1; any other label is rejected as
//! malformed.
//!
//! Reply: the [`ControlReply`] encoded by
//! [`kernel_core::session_control::encode_reply`] delivered as the bulk
//! of an `ipc_reply`. The reply label is [`LABEL_CTL_REPLY`] = 2 on
//! success and [`u64::MAX`] on a transport-level failure.
//!
//! Mirrors the `display_server::control::dispatch_command` precedent
//! one-for-one: pure dispatcher consumes the codec from `kernel-core`,
//! stages the reply bulk, then `ipc_reply` transfers it to the caller.
//!
//! ## Capability gate
//!
//! The kernel's IPC service registry is process-scoped; any client
//! that can lookup `"session-control"` is on the same machine. F.5
//! gates verbs at the dispatcher level via a [`ControlSocketCap`] —
//! the cap is presented at every dispatch and the dispatcher refuses
//! the verb without it. In the local daemon the cap is implicitly
//! present (the dispatcher always passes `Some(&cap)`); the gate's
//! purpose is to surface the policy in the typed verb surface so a
//! future cap-transferring AF_UNIX migration plugs in without an API
//! change.
//!
//! Per the F.5 acceptance this matches the Phase 56 m3ctl precedent
//! and **introduces no UID-based access control**.

use kernel_core::session::SessionState;
use kernel_core::session_control::{
    ControlReply, ControlSocketCap, SessionControlBackend, SessionControlError,
    dispatch_authenticated, encode_reply,
};
use kernel_core::session_supervisor::SupervisorBackend;
use syscall_lib::{IpcMessage, STDOUT_FILENO};

use crate::recover;

/// Service-registry name of the control endpoint. Stable across F.2
/// (the prior stub) and F.5 (this dispatcher) so a future `m3ctl
/// session-state` can look up the same name.
pub const CONTROL_SERVICE_NAME: &str = "session-control";

/// IPC label `session_manager` accepts on the `"session-control"`
/// endpoint when the bulk carries an encoded [`ControlVerb`]. Mirrors
/// the Phase 56 `display_server` `LABEL_CTL_CMD = 1` constant.
pub const LABEL_CTL_CMD: u64 = 1;

/// IPC reply label `session_manager` returns when the dispatched verb
/// produced an encoded [`ControlReply`] in the reply bulk.
pub const LABEL_CTL_REPLY: u64 = 2;

/// Reply-cap handle the kernel writes when a recv produces a message.
const REPLY_CAP_HANDLE: u32 = 1;

/// Maximum bulk size accepted on the control endpoint. The verb is a
/// single byte; the buffer fits a 1-byte verb + the longest reply
/// (a `Recovering` state with a 32-byte step name + 4-byte retry count
/// + 3 bytes header = 40 bytes). 64 leaves headroom.
const MAX_CONTROL_BUF: usize = 64;

/// Holder for the control-socket endpoint's cap-handle. Constructed
/// once at startup; passed to [`poll_control_once`] each event-loop
/// iteration.
pub struct ControlSocket {
    /// Cap-handle of the registered endpoint. `None` if registration
    /// failed at startup — the daemon continues but the control socket
    /// is dormant.
    ep_handle: Option<u32>,
}

impl ControlSocket {
    /// A control socket whose endpoint registration has not yet been
    /// attempted. Use [`bind_control_socket`] for the production path.
    pub const fn dormant() -> Self {
        Self { ep_handle: None }
    }

    /// Whether the endpoint is bound and ready to receive.
    #[allow(dead_code)] // diagnostic accessor; F.5 dispatcher uses ep_handle directly.
    pub fn is_bound(&self) -> bool {
        self.ep_handle.is_some()
    }
}

/// Bind the control endpoint and register it under
/// [`CONTROL_SERVICE_NAME`]. On failure, returns a dormant socket and
/// emits a structured `session.control` log line. The daemon continues
/// without the control surface — this matches the Phase 56 pattern
/// where `display_server` continues without input if the kbd/mouse
/// services are unavailable.
pub fn bind_control_socket() -> ControlSocket {
    let raw = syscall_lib::create_endpoint();
    if raw == u64::MAX {
        syscall_lib::write_str(
            STDOUT_FILENO,
            "session_manager: session.control: create_endpoint failed; control socket dormant\n",
        );
        return ControlSocket::dormant();
    }
    let ep = raw as u32;
    let reg = syscall_lib::ipc_register_service(ep, CONTROL_SERVICE_NAME);
    if reg == u64::MAX {
        syscall_lib::write_str(
            STDOUT_FILENO,
            "session_manager: session.control: register failed; control socket dormant\n",
        );
        return ControlSocket::dormant();
    }
    syscall_lib::write_str(
        STDOUT_FILENO,
        "session_manager: session.control: registered as 'session-control' (F.5 dispatcher)\n",
    );
    ControlSocket {
        ep_handle: Some(ep),
    }
}

/// Mutable daemon-wide state the F.5 dispatcher reads and updates.
///
/// `state` mirrors the daemon's last observed [`SessionState`] (the
/// boot sequence's final outcome, modulo subsequent stop/restart
/// motions). `restart_requested` flips `true` when a `session-restart`
/// verb arrives so the main event-loop can re-run the boot sequence
/// after the dispatcher returns the Ack.
///
/// Held by `main.rs` and threaded into [`poll_control_once`] each
/// event-loop iteration. Splitting the daemon's state from the
/// dispatcher keeps the dispatcher's signature explicit and the state
/// owner singular (SOLID SRP).
pub struct ControlContext {
    /// Last observed [`SessionState`] — read by `session-state`, written
    /// by `session-stop` (→ `TextFallback`) and `session-restart`.
    pub state: SessionState,
    /// Set to `true` when the dispatcher honored `session-restart`;
    /// the main event-loop reads this on the next iteration to re-run
    /// the boot sequence. Cleared by the loop after restart.
    pub restart_requested: bool,
}

impl ControlContext {
    /// Construct a fresh context whose state is `Booting`. The boot
    /// sequence updates the state after `seq.run` returns.
    pub const fn new() -> Self {
        Self {
            state: SessionState::Booting,
            restart_requested: false,
        }
    }
}

/// Backend adapter that bridges the F.5 codec
/// [`SessionControlBackend`] trait to the daemon's
/// [`ControlContext`] + [`SupervisorBackend`].
///
/// SOLID DI: the codec depends on the trait; this adapter owns the
/// borrow against the daemon's mutable state for the duration of one
/// dispatch.
struct DaemonBackend<'c, 'b, B: SupervisorBackend> {
    ctx: &'c mut ControlContext,
    supervisor: &'b mut B,
}

impl<'c, 'b, B: SupervisorBackend> SessionControlBackend for DaemonBackend<'c, 'b, B> {
    fn current_state(&mut self) -> SessionState {
        self.ctx.state
    }

    fn session_stop(&mut self) -> Result<(), SessionControlError> {
        // session-stop is the graceful-shutdown verb: run the F.4
        // text-fallback motion and transition the daemon to
        // `TextFallback`. The motion swallows individual stop errors
        // per the F.4 contract, so this verb cannot itself fail at the
        // protocol level.
        let _outcome = recover::run_text_fallback(self.supervisor);
        self.ctx.state = SessionState::TextFallback;
        Ok(())
    }

    fn session_restart(&mut self) -> Result<(), SessionControlError> {
        // session-restart is graceful stop + start. The graceful stop
        // is the F.4 motion; the start is signaled to `main.rs` via
        // `restart_requested = true`. The dispatcher cannot itself
        // re-drive the boot sequence because the boot sequence borrows
        // the supervisor mutably alongside the F.1 step adapters; the
        // event loop performs the restart after the dispatch returns.
        let _outcome = recover::run_text_fallback(self.supervisor);
        self.ctx.state = SessionState::TextFallback;
        self.ctx.restart_requested = true;
        Ok(())
    }
}

/// Non-blocking poll of the control socket. Returns `true` if a
/// request was handled this iteration, `false` if the queue was empty
/// (the normal idle path) or the socket is dormant.
///
/// The F.5 dispatcher decodes the verb from the request bulk, calls
/// [`dispatch_authenticated`] with the implicit cap (always granted in
/// the local daemon — see module docs for the rationale), encodes the
/// reply, stages it as the IPC reply bulk, and `ipc_reply`s.
///
/// The reply label is [`LABEL_CTL_REPLY`] on success and `u64::MAX`
/// on a transport-level failure (encode error, bulk-stage failure,
/// recv buffer too small for the encoded reply).
pub fn poll_control_once<B: SupervisorBackend>(
    socket: &ControlSocket,
    ctx: &mut ControlContext,
    supervisor: &mut B,
) -> bool {
    let Some(ep) = socket.ep_handle else {
        return false;
    };
    let mut msg = IpcMessage::new(0);
    let mut buf = [0u8; MAX_CONTROL_BUF];
    let label = syscall_lib::ipc_try_recv_msg(ep, &mut msg, &mut buf);
    if label == u64::MAX {
        // Empty queue (the normal case) or copy fault — see
        // `ipc_try_recv_msg` doc-comment for the ambiguity. We cannot
        // distinguish without an extra syscall; idle is the default.
        return false;
    }
    if label != LABEL_CTL_CMD {
        // Unknown label — F.2 stub used `u64::MAX` as the catch-all
        // sentinel; F.5 keeps that signal so the prior contract holds.
        syscall_lib::write_str(
            STDOUT_FILENO,
            "session_manager: session.control: unknown label; replying with sentinel\n",
        );
        let _ = syscall_lib::ipc_reply(REPLY_CAP_HANDLE, u64::MAX, 0);
        return true;
    }

    // Determine bulk length. The kernel writes the staged bulk size
    // into `msg.data[1]` when the sender called `ipc_call_buf` /
    // `ipc_send_buf`; this matches the Phase 56
    // `display_server::main.rs::header.data[1]` convention.
    let bulk_len = msg.data[1] as usize;
    let bulk_len = if bulk_len > buf.len() {
        // Defensive: truncate to the buffer's capacity. The dispatcher
        // surfaces this as `MalformedRequest` via the codec.
        buf.len()
    } else {
        bulk_len
    };
    let request_bytes = &buf[..bulk_len];

    // Always-granted cap in the local daemon. Future cap-transferring
    // transport will replace this with a per-connection cap retrieved
    // from the IPC framing.
    let cap = ControlSocketCap::granted_for_m3ctl_only();
    let mut backend = DaemonBackend { ctx, supervisor };
    let reply = match dispatch_authenticated(request_bytes, Some(&cap), &mut backend) {
        Ok(reply) => reply,
        Err(err) => ControlReply::Error(err),
    };

    // Encode the reply into a fresh buffer so we don't aliasing the
    // request buffer (which the kernel may still hold a reference to
    // through the recv path).
    let mut out_buf = [0u8; MAX_CONTROL_BUF];
    let len = match encode_reply(&reply, &mut out_buf) {
        Ok(n) => n,
        Err(_) => {
            syscall_lib::write_str(
                STDOUT_FILENO,
                "session_manager: session.control: reply encode failed; replying with sentinel\n",
            );
            let _ = syscall_lib::ipc_reply(REPLY_CAP_HANDLE, u64::MAX, 0);
            return true;
        }
    };
    if syscall_lib::ipc_store_reply_bulk(&out_buf[..len]) == u64::MAX {
        syscall_lib::write_str(
            STDOUT_FILENO,
            "session_manager: session.control: store_reply_bulk failed; replying with sentinel\n",
        );
        let _ = syscall_lib::ipc_reply(REPLY_CAP_HANDLE, u64::MAX, 0);
        return true;
    }
    let _ = syscall_lib::ipc_reply(REPLY_CAP_HANDLE, LABEL_CTL_REPLY, len as u64);
    true
}
