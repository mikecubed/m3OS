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

use alloc::string::{String, ToString};

use kernel_core::session::SessionState;
use kernel_core::session_control::{
    ControlReply, ControlSocketCap, SessionControlBackend, SessionControlError,
    TAG_VERB_SESSION_RESTART_SERVICE, dispatch_authenticated, encode_reply,
};
use kernel_core::session_supervisor::SupervisorBackend;
use session_manager::init_status::init_service_name;
use syscall_lib::{IpcMessage, STDOUT_FILENO};

use crate::init_proxy;
use crate::recover;

/// Phase 64b — maximum wall-clock time we will wait for an
/// async-restart to converge before reporting failure to the caller
/// via the deferred IPC reply. Bounded by init's worst-case restart
/// timing: 5 s SIGTERM grace + 1 s SIGKILL reap + restart_delay
/// (up to 60 s under heavy back-off, normally 1 s). 30 s covers the
/// common case with headroom; permanently-stalled cases escalate via
/// `m3ctl session-state --detailed`.
const ASYNC_RESTART_DEADLINE_MS: u64 = 30_000;

/// Service-registry name of the control endpoint. Stable across F.2
/// (the prior stub) and F.5 (this dispatcher) so a future `m3ctl
/// session-state` can look up the same name.
pub const CONTROL_SERVICE_NAME: &str = "session-control";

/// Maximum bytes we read from the `"session-events"` push endpoint on
/// each tick. Sized to fit one `ServiceExitEvent`'s wire layout with
/// headroom for future codec growth.
const MAX_EVENT_BUF: usize = 64;

/// IPC label `session_manager` accepts on the `"session-control"`
/// endpoint when the bulk carries an encoded [`ControlVerb`]. Mirrors
/// the Phase 56 `display_server` `LABEL_CTL_CMD = 1` constant.
pub const LABEL_CTL_CMD: u64 = 1;

/// IPC reply label `session_manager` returns when the dispatched verb
/// produced an encoded [`ControlReply`] in the reply bulk.
pub const LABEL_CTL_REPLY: u64 = 2;

/// Maximum bulk size accepted on the control endpoint. The verb is a
/// single byte; the buffer must fit the worst-case Phase 64
/// `ServiceStates` reply, which packs up to `MAX_SERVICE_STATE_ENTRIES`
/// (8) per-service quads — `(name, state, restart_count, step_failures)`.
/// On the wire each quad is 42 bytes: 1 byte name_len + ≤32 bytes name +
/// 1 byte state tag + 4 bytes restart_count + 4 bytes step_failures.
/// Worst case: 2-byte header + 8·42 = 338 bytes. We round to 384 so the
/// buffer stays one allocation page-fragment in size and tolerates
/// future codec additions without revisiting the constant. Must stay
/// in sync with `m3ctl`'s `SESSION_REPLY_MAX`.
const MAX_CONTROL_BUF: usize = 384;

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

/// Phase 64b — holder for the `"session-events"` push endpoint
/// `session_manager` exposes for init to deliver exit notifications.
/// Symmetrical with [`ControlSocket`]; both expose `ep_handle:
/// Option<u32>` so a bind failure at startup is non-fatal.
pub struct EventsSocket {
    ep_handle: Option<u32>,
}

impl EventsSocket {
    pub const fn dormant() -> Self {
        Self { ep_handle: None }
    }

    #[allow(dead_code)]
    pub fn is_bound(&self) -> bool {
        self.ep_handle.is_some()
    }
}

/// Bind the `"session-events"` push endpoint. On failure the daemon
/// continues with a dormant socket; init's lookups will simply find no
/// endpoint and skip notification, and the existing
/// `/run/services.status` polling path still observes exits (with
/// higher latency).
pub fn bind_events_socket() -> EventsSocket {
    use kernel_core::session_events::SESSION_EVENTS_SERVICE_NAME;
    let raw = syscall_lib::create_endpoint();
    if raw == u64::MAX {
        syscall_lib::write_str(
            STDOUT_FILENO,
            "session_manager: session.events: create_endpoint failed; events dormant\n",
        );
        return EventsSocket::dormant();
    }
    let ep = raw as u32;
    let reg = syscall_lib::ipc_register_service(ep, SESSION_EVENTS_SERVICE_NAME);
    if reg == u64::MAX {
        syscall_lib::write_str(
            STDOUT_FILENO,
            "session_manager: session.events: register failed; events dormant\n",
        );
        return EventsSocket::dormant();
    }
    syscall_lib::write_str(
        STDOUT_FILENO,
        "session_manager: session.events: registered as 'session-events' (Phase 64b)\n",
    );
    EventsSocket {
        ep_handle: Some(ep),
    }
}

/// Phase 64b — drain any pending exit-event push notifications from
/// init. Bounded loop (up to 8 events per tick) so a flood cannot
/// monopolize the event loop. Each event is logged at INFO level so
/// the boot transcript captures init reaps; future tracks may also
/// dispatch them to the supervisor's state machine.
pub fn drain_exit_events<B: SupervisorBackend>(events: &EventsSocket, _supervisor: &mut B) -> u32 {
    use kernel_core::session_events::{LABEL_SESSION_EVENT_EXIT, decode_exit_event};
    let Some(ep) = events.ep_handle else {
        return 0;
    };
    let mut handled: u32 = 0;
    // Bounded drain — 8 events per tick is plenty for the five session
    // services; preventing the loop from sticking on a noisy producer.
    for _ in 0..8 {
        let mut msg = IpcMessage::new(0);
        let mut buf = [0u8; MAX_EVENT_BUF];
        let label = syscall_lib::ipc_try_recv_msg(ep, &mut msg, &mut buf);
        if label == u64::MAX {
            break;
        }
        if label != LABEL_SESSION_EVENT_EXIT {
            syscall_lib::write_str(
                STDOUT_FILENO,
                "session_manager: session.events: unknown label; dropping\n",
            );
            continue;
        }
        let bulk_len = (msg.data[1] as usize).min(buf.len());
        match decode_exit_event(&buf[..bulk_len]) {
            Ok(event) => {
                syscall_lib::write_str(STDOUT_FILENO, "session_manager: session.events: ");
                if let Some(name) = event.name_as_str() {
                    syscall_lib::write_str(STDOUT_FILENO, name);
                } else {
                    syscall_lib::write_str(STDOUT_FILENO, "<?>");
                }
                syscall_lib::write_str(STDOUT_FILENO, " exited pid=");
                syscall_lib::write_u64(STDOUT_FILENO, event.pid as u64);
                if event.signaled {
                    syscall_lib::write_str(STDOUT_FILENO, " signal=");
                } else {
                    syscall_lib::write_str(STDOUT_FILENO, " code=");
                }
                // exit_code is i32; write as u64 with a leading '-' on negative.
                if event.exit_code < 0 {
                    syscall_lib::write_str(STDOUT_FILENO, "-");
                    syscall_lib::write_u64(STDOUT_FILENO, (-(event.exit_code as i64)) as u64);
                } else {
                    syscall_lib::write_u64(STDOUT_FILENO, event.exit_code as u64);
                }
                syscall_lib::write_str(STDOUT_FILENO, "\n");
                handled += 1;
            }
            Err(_) => {
                syscall_lib::write_str(
                    STDOUT_FILENO,
                    "session_manager: session.events: decode error; dropping\n",
                );
            }
        }
    }
    handled
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
    /// Phase 64b — one in-flight `SessionRestartService` operation.
    /// The dispatcher writes `restart <name>` to init.cmd, parks the
    /// caller's reply cap + the prior PID here, and returns without
    /// replying. The main loop calls [`tick_pending`] each iteration
    /// to observe convergence via `/run/services.status`; when the
    /// service is `Running` with a new PID, the reply cap is consumed
    /// and the caller sees `Ack`. Only one async restart in flight at
    /// a time — a second request reflects back `Internal` until the
    /// first completes.
    pub pending_restart: Option<PendingRestart>,
}

/// Phase 64b — one in-flight async restart operation.
pub struct PendingRestart {
    /// Per-recv reply cap, captured from `msg.reply_cap_handle()` at
    /// dispatch time. Held until convergence or deadline, then
    /// consumed by exactly one `ipc_reply`.
    pub reply_cap_handle: u32,
    /// Step name (e.g. `"kbd_server"`) as it appears in the supervisor
    /// table; used to call `finish_async_restart` / `fail_async_restart`.
    pub step_name: String,
    /// Init manifest name (e.g. `"kbd"`) — what `/run/services.status`
    /// indexes by. Cached so each tick avoids re-running
    /// `init_service_name`.
    pub init_name: String,
    /// PID observed at request time. Convergence is "running with PID
    /// != prior_pid"; `0` means "no prior PID known", in which case
    /// any non-zero `Running` PID counts as convergence.
    pub prior_pid: i32,
    /// Monotonic deadline (ms) at which this request reports failure
    /// to the caller via the deferred reply.
    pub deadline_ms: u64,
}

impl ControlContext {
    /// Construct a fresh context whose state is `Booting`. The boot
    /// sequence updates the state after `seq.run` returns.
    pub const fn new() -> Self {
        Self {
            state: SessionState::Booting,
            restart_requested: false,
            pending_restart: None,
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

impl<'c, 'b, B: SupervisorBackend> DaemonBackend<'c, 'b, B> {
    /// Run the F.4 text-fallback motion and transition the daemon
    /// state. Shared by `session_stop` (terminal) and `session_restart`
    /// (followed by the event-loop's re-drive). The motion swallows
    /// individual stop errors per the F.4 contract, so this helper
    /// cannot itself fail at the protocol level.
    fn rollback_to_text_fallback(&mut self) {
        let _outcome = recover::run_text_fallback(self.supervisor);
        self.ctx.state = SessionState::TextFallback;
    }
}

impl<'c, 'b, B: SupervisorBackend> SessionControlBackend for DaemonBackend<'c, 'b, B> {
    fn current_state(&mut self) -> SessionState {
        self.ctx.state
    }

    fn session_stop(&mut self) -> Result<(), SessionControlError> {
        // session-stop is the graceful-shutdown verb. Stay in
        // TextFallback after the rollback; the operator can still
        // issue session-restart afterwards.
        self.rollback_to_text_fallback();
        Ok(())
    }

    fn session_restart(&mut self) -> Result<(), SessionControlError> {
        // session-restart is graceful stop + start. The dispatcher
        // cannot itself re-drive the boot sequence because the boot
        // sequence borrows the supervisor mutably alongside the F.1
        // step adapters; the event loop performs the restart after
        // dispatch returns.
        self.rollback_to_text_fallback();
        self.ctx.restart_requested = true;
        Ok(())
    }

    fn services_snapshot(
        &mut self,
    ) -> (
        u8,
        [kernel_core::session_control::ServiceStateEntry;
            kernel_core::session_control::MAX_SERVICE_STATE_ENTRIES],
    ) {
        // Phase 64 — forward to the supervisor's own snapshot. The
        // production `InitSupervisorBackend` reads its `ServiceTable`;
        // pre-Phase-64 backends use the default (empty) implementation.
        self.supervisor.services_snapshot()
    }

    fn session_restart_service(&mut self, service: &str) -> Result<(), SessionControlError> {
        // Phase 64a — delegate to the supervisor's per-service restart
        // motion. `InitSupervisorBackend::restart` writes
        // `restart <name>` to /run/init.cmd and polls
        // /run/services.status until the service comes back. Map the
        // supervisor's typed errors onto the codec's typed surface so
        // m3ctl operators see a stable wire result.
        use kernel_core::session_supervisor::SupervisorError;
        match self.supervisor.restart(service) {
            Ok(_) => Ok(()),
            Err(SupervisorError::UnknownService) => Err(SessionControlError::MalformedRequest),
            Err(_) => Err(SessionControlError::Internal),
        }
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
    // Phase 64b — use the per-recv reply cap the kernel staged into
    // `msg.data[3]`. A fire-and-forget sender carries no reply cap (the
    // kernel signals this with `msg.data[3] == 0`). Replying anyway on a
    // fallback handle is unsafe: the kernel's `ipc_reply` path removes the
    // cap at the given handle *before* type-checking it (`slot.take()` in
    // `CapabilityTable::remove`), so a bogus handle silently deletes an
    // unrelated cap from our own table — and the deferred path would defer
    // that corruption to a later `tick_pending`. Drop such messages instead;
    // every reply site below (immediate and deferred) replies exactly once
    // on this verified cap.
    //
    // Return `true`, not `false`: a message *was* dequeued and consumed this
    // iteration (we chose to drop it), so a drain-style caller should keep
    // polling rather than treat the socket as idle. This matches every other
    // post-dequeue path — the unknown-label branch below and all of
    // `handle_async_restart` likewise return `true` after consuming a
    // message. `false` stays reserved for the empty-queue and dormant-socket
    // cases named in the doc-comment above.
    let Some(reply_cap) = msg.reply_cap_handle() else {
        return true;
    };
    if label != LABEL_CTL_CMD {
        // Unknown label — F.2 stub used `u64::MAX` as the catch-all
        // sentinel; F.5 keeps that signal so the prior contract holds.
        reply_with_sentinel(reply_cap, "unknown label");
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

    // Phase 64b — peek at the verb byte. `SessionRestartService` is
    // long-running (1–30 s, dominated by init's stop-grace + restart
    // back-off) and would block other IPC if handled synchronously.
    // Route it to the async path; everything else still goes through
    // `dispatch_authenticated` synchronously.
    if request_bytes.first().copied() == Some(TAG_VERB_SESSION_RESTART_SERVICE) {
        return handle_async_restart(request_bytes, reply_cap, ctx, supervisor);
    }

    // Always-granted cap in the local daemon. Future cap-transferring
    // transport will replace this with a per-connection cap retrieved
    // from the IPC framing.
    let cap = ControlSocketCap::granted_for_m3ctl_only();
    let mut backend = DaemonBackend { ctx, supervisor };
    let reply = match dispatch_authenticated(request_bytes, Some(&cap), &mut backend) {
        Ok(reply) => reply,
        Err(err) => ControlReply::Error(err),
    };

    send_reply(reply_cap, &reply);
    true
}

/// Phase 64b — async dispatch for `SessionRestartService`. Writes
/// `restart <name>` to `/run/init.cmd`, parks the caller's reply cap +
/// the prior PID in `ctx.pending_restart`, and returns without
/// replying. [`tick_pending`] consumes the parked record on each
/// event-loop iteration.
fn handle_async_restart<B: SupervisorBackend>(
    request_bytes: &[u8],
    reply_cap: u32,
    ctx: &mut ControlContext,
    supervisor: &mut B,
) -> bool {
    use kernel_core::session_control::decode_verb;

    // One in-flight request at a time. A second request while one is
    // parked reflects back `Internal` so m3ctl operators see a typed
    // "busy" rather than a stale `Ack`.
    if ctx.pending_restart.is_some() {
        send_reply(
            reply_cap,
            &ControlReply::Error(SessionControlError::Internal),
        );
        syscall_lib::write_str(
            STDOUT_FILENO,
            "session_manager: session.control: async restart already in flight; replying Internal\n",
        );
        return true;
    }

    let verb = match decode_verb(request_bytes) {
        Ok(v) => v,
        Err(e) => {
            send_reply(reply_cap, &ControlReply::Error(e));
            return true;
        }
    };
    let service = match verb.restart_service_name() {
        Some(s) => s,
        None => {
            send_reply(
                reply_cap,
                &ControlReply::Error(SessionControlError::MalformedRequest),
            );
            return true;
        }
    };

    let init_name = init_service_name(service);
    if init_name.is_empty() {
        send_reply(
            reply_cap,
            &ControlReply::Error(SessionControlError::MalformedRequest),
        );
        return true;
    }

    match supervisor.begin_async_restart(service) {
        Ok(prior_pid) => {
            ctx.pending_restart = Some(PendingRestart {
                reply_cap_handle: reply_cap,
                step_name: service.to_string(),
                init_name: init_name.to_string(),
                prior_pid,
                deadline_ms: init_proxy::now_ms() + ASYNC_RESTART_DEADLINE_MS,
            });
            true
        }
        Err(_) => {
            send_reply(
                reply_cap,
                &ControlReply::Error(SessionControlError::Internal),
            );
            true
        }
    }
}

/// Phase 64b — drive any in-flight async restart toward convergence.
/// Called from the daemon's main event loop on each iteration; cheap
/// when nothing is parked (one `is_none()` check). When the parked
/// service is `Running` in `/run/services.status` with a PID != the
/// recorded prior PID, the deferred reply fires and the slot frees.
/// On deadline elapse the caller sees `Error(Internal)`.
pub fn tick_pending<B: SupervisorBackend>(ctx: &mut ControlContext, supervisor: &mut B) {
    let Some(pending) = ctx.pending_restart.as_ref() else {
        return;
    };
    let now = init_proxy::now_ms();
    // Observe convergence first — even past the deadline we prefer to
    // surface a successful restart if it landed at the same instant.
    if let Some(new_pid) =
        crate::init_backend::observe_async_restart(&pending.init_name, pending.prior_pid)
    {
        let step = pending.step_name.clone();
        let cap = pending.reply_cap_handle;
        ctx.pending_restart = None;
        let _ = supervisor.finish_async_restart(&step, new_pid);
        send_reply(cap, &ControlReply::Ack);
        return;
    }
    if now >= pending.deadline_ms {
        let step = pending.step_name.clone();
        let cap = pending.reply_cap_handle;
        ctx.pending_restart = None;
        let _ = supervisor.fail_async_restart(&step);
        syscall_lib::write_str(STDOUT_FILENO, "session_manager: lifecycle.restart: '");
        syscall_lib::write_str(STDOUT_FILENO, &step);
        syscall_lib::write_str(
            STDOUT_FILENO,
            "': deferred reply deadline elapsed; reporting Internal\n",
        );
        send_reply(cap, &ControlReply::Error(SessionControlError::Internal));
    }
}

/// Encode `reply` into the IPC reply bulk and call `ipc_reply` against
/// `reply_cap`. On encode or store-bulk failure, falls back to the
/// `u64::MAX` sentinel via [`reply_with_sentinel`].
fn send_reply(reply_cap: u32, reply: &ControlReply) {
    let mut out_buf = [0u8; MAX_CONTROL_BUF];
    let len = match encode_reply(reply, &mut out_buf) {
        Ok(n) => n,
        Err(_) => {
            reply_with_sentinel(reply_cap, "reply encode failed");
            return;
        }
    };
    if syscall_lib::ipc_store_reply_bulk(&out_buf[..len]) == u64::MAX {
        reply_with_sentinel(reply_cap, "store_reply_bulk failed");
        return;
    }
    let _ = syscall_lib::ipc_reply(reply_cap, LABEL_CTL_REPLY, len as u64);
}

/// Reply to the connected client with the `u64::MAX` sentinel label
/// (the prior F.2-stub contract for "verb not honored / transport
/// failure") and emit a structured `session.control` log line naming
/// the cause. Centralized so all four error branches (unknown label,
/// reply-encode failure, store-reply-bulk failure, and any future
/// branches) emit a consistent log shape and reply atomically.
fn reply_with_sentinel(reply_cap: u32, cause: &'static str) {
    syscall_lib::write_str(STDOUT_FILENO, "session_manager: session.control: ");
    syscall_lib::write_str(STDOUT_FILENO, cause);
    syscall_lib::write_str(STDOUT_FILENO, "; replying with sentinel\n");
    let _ = syscall_lib::ipc_reply(reply_cap, u64::MAX, 0);
}
