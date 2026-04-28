//! Phase 57 Track F.2 — `session_manager` daemon.
//!
//! `session_manager` is the supervised userspace daemon that owns the
//! Phase 57 graphical-session entry contract (A.4 memo). It runs once
//! at boot, drives each declared service through the
//! [`kernel_core::session::StartupSequence`] (F.1), and on success
//! transitions to a single-threaded event loop that multiplexes
//! supervisor events and a control socket.
//!
//! ## Boot ordering
//!
//! Per the A.4 memo, the declared step order is:
//!
//! `display_server → kbd_server → mouse_server → audio_server → term`
//!
//! [`crate::boot::build_session_steps`] constructs one
//! [`crate::boot::ServiceStep`] per declared name from
//! [`kernel_core::session_supervisor::declared_session_step_names`].
//! No big match; each step is a small struct holding the name + a
//! borrow of the shared backend (SOLID: SRP).
//!
//! ## Phase 57 transitional behaviour
//!
//! Tracks D (`audio_server`) and G (`term`) land later than F.2.
//! Until they ship, this daemon's adapter reports their `start()` as
//! a step failure, which the F.1 sequencer counts against the per-step
//! retry budget; after 3 attempts the sequence escalates to
//! `SessionState::TextFallback` with a clean rollback. Once D and G
//! land, the same boot path reaches `SessionState::Running` without
//! any change to this binary.
//!
//! ## Concurrency
//!
//! Single-threaded. After boot the daemon idles in a Phase 56-style
//! event loop that:
//! 1. Polls the control socket non-blocking (F.5 stub for now).
//! 2. Sleeps briefly so PID 1 doesn't burn CPU.
//!
//! No worker threads, no `recv_multi` (Phase 56 precedent).
//!
//! ## Error discipline
//!
//! No `unwrap` / `expect` / `panic!` outside test code. Every
//! supervisor verb call is checked and surfaced via a structured log
//! line.

#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

mod boot;
mod control;
mod recover;

use core::alloc::Layout;

use kernel_core::session::{MAX_RETRIES_PER_STEP, SessionState, SessionStep, StartupSequence};
use syscall_lib::STDOUT_FILENO;
use syscall_lib::heap::BrkAllocator;

#[global_allocator]
static ALLOCATOR: BrkAllocator = BrkAllocator::new();

#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "session_manager: alloc error\n");
    syscall_lib::exit(99)
}

syscall_lib::entry_point!(program_main);

/// Service-registry name under which `session_manager` exposes its own
/// IPC endpoint. Distinct from `control::CONTROL_SERVICE_NAME` — that
/// one is the F.5 control surface; this one is the supervisor-events
/// channel that future tracks may extend.
const SESSION_MANAGER_SERVICE: &str = "session-manager";

/// Idle sleep between control-socket polls in the steady-state event
/// loop. 5 ms matches the Phase 56 daemon idle cadence and keeps the
/// daemon responsive to operator commands without burning CPU.
const IDLE_SLEEP_NS: u32 = 5_000_000;

fn program_main(_args: &[&str]) -> i32 {
    syscall_lib::write_str(
        STDOUT_FILENO,
        "session_manager: starting (Phase 57 F.2 — boot ordering + control-socket stub)\n",
    );

    // Register a service endpoint so init's supervisor and future
    // tracks can locate the daemon. Failure here is fatal — the daemon
    // has no purpose if it cannot be reached.
    let ep_handle = syscall_lib::create_endpoint();
    if ep_handle == u64::MAX {
        syscall_lib::write_str(
            STDOUT_FILENO,
            "session_manager: create_endpoint failed; exiting\n",
        );
        return 1;
    }
    let ep_handle = ep_handle as u32;
    let reg = syscall_lib::ipc_register_service(ep_handle, SESSION_MANAGER_SERVICE);
    if reg == u64::MAX {
        syscall_lib::write_str(
            STDOUT_FILENO,
            "session_manager: ipc_register_service('session-manager') failed; exiting\n",
        );
        return 1;
    }
    syscall_lib::write_str(
        STDOUT_FILENO,
        "session_manager: registered as 'session-manager'\n",
    );

    // F.5: bind the control socket and construct the dispatcher
    // context. A bind failure is non-fatal — the boot sequence still
    // runs and the daemon idles afterwards; the dispatcher's `Some(ep)`
    // guard short-circuits.
    let control_socket = control::bind_control_socket();
    let mut control_ctx = control::ControlContext::new();

    // Drive the declared boot sequence.
    let mut backend = init_backend::InitSupervisorBackend::new();
    let final_state = run_boot_sequence(&mut backend);
    log_final_state(final_state);
    control_ctx.state = final_state;

    // F.4: on text-fallback, run the rollback executor and stay alive
    // so the serial admin shell remains reachable. Per A.4: the
    // operator does not lose the daemon entirely on a graphical-session
    // failure; the daemon falls back to "graphical-session offline" but
    // continues servicing the control socket so an operator can issue
    // `session-restart` (F.5) once the underlying issue is fixed.
    if matches!(final_state, SessionState::TextFallback) {
        let _outcome = recover::run_text_fallback(&mut backend);
    }

    syscall_lib::write_str(
        STDOUT_FILENO,
        "session_manager: entering steady-state loop\n",
    );
    loop {
        // Poll the control socket non-blocking. F.5 dispatches the
        // session-state / session-stop / session-restart verbs.
        let _serviced = control::poll_control_once(&control_socket, &mut control_ctx, &mut backend);

        // F.5 honored a `session-restart`: re-drive the F.1 boot
        // sequence. The text-fallback motion that the dispatcher ran
        // already stopped every declared service in reverse order, so
        // the next `seq.run` starts from a clean slate.
        if control_ctx.restart_requested {
            control_ctx.restart_requested = false;
            syscall_lib::write_str(
                STDOUT_FILENO,
                "session_manager: session.control: session-restart re-driving boot sequence\n",
            );
            let new_state = run_boot_sequence(&mut backend);
            log_final_state(new_state);
            control_ctx.state = new_state;
            if matches!(new_state, SessionState::TextFallback) {
                let _outcome = recover::run_text_fallback(&mut backend);
            }
        }

        // Idle sleep so PID 1 stays responsive.
        let _ = syscall_lib::nanosleep_for(0, IDLE_SLEEP_NS);
    }
}

/// Run the F.1 sequencer over the declared session steps, using the
/// init-backed supervisor adapter. Returns the final
/// [`SessionState`].
///
/// `backend` is owned by the caller so the F.4 text-fallback rollback
/// can reuse the same instance after the boot sequence completes (the
/// rollback issues stops via the same supervisor surface as the boot
/// path).
fn run_boot_sequence(backend: &mut init_backend::InitSupervisorBackend) -> SessionState {
    let backend_cell = core::cell::RefCell::new(backend);
    let mut steps = boot::build_session_steps(&backend_cell);

    let (s0, rest) = steps.split_at_mut(1);
    let (s1, rest) = rest.split_at_mut(1);
    let (s2, rest) = rest.split_at_mut(1);
    let (s3, s4) = rest.split_at_mut(1);
    let mut step_refs: [&mut dyn SessionStep; 5] =
        [&mut s0[0], &mut s1[0], &mut s2[0], &mut s3[0], &mut s4[0]];
    let mut seq = StartupSequence::new(&mut step_refs);
    match seq.run(MAX_RETRIES_PER_STEP) {
        Ok(state) => state,
        Err(_e) => {
            // The F.1 sequencer's `run` only returns Err in
            // out-of-order paths; treat any err as an escalation so
            // F.4's rollback runs.
            SessionState::TextFallback
        }
    }
}

/// Emit a structured log line for the final session state.
fn log_final_state(state: SessionState) {
    match state {
        SessionState::Running => {
            syscall_lib::write_str(
                STDOUT_FILENO,
                "session_manager: session.boot: state=running\n",
            );
        }
        SessionState::TextFallback => {
            syscall_lib::write_str(
                STDOUT_FILENO,
                "session_manager: session.boot: state=text-fallback (boot retry budget exhausted)\n",
            );
        }
        SessionState::Booting => {
            syscall_lib::write_str(
                STDOUT_FILENO,
                "session_manager: session.boot: state=booting (unexpected; sequencer did not advance)\n",
            );
        }
        SessionState::Recovering { .. } => {
            syscall_lib::write_str(
                STDOUT_FILENO,
                "session_manager: session.boot: state=recovering (unexpected at run() exit)\n",
            );
        }
    }
}

mod init_backend {
    //! Phase 57 F.2 — production adapter that satisfies the F.3
    //! [`kernel_core::session_supervisor::SupervisorBackend`] trait by
    //! talking to init through its existing root-only control surface.
    //!
    //! The adapter is intentionally minimal in F.2:
    //!
    //! - `start(name)` performs an `ipc_lookup_service(name)` round
    //!   trip. If the service is registered, we treat it as already
    //!   started by init's existing manifest-driven boot. If it is
    //!   not registered, we surface `SupervisorError::UnknownService`
    //!   so the F.1 sequencer counts it against the retry budget.
    //!   This shape matches the F.2 acceptance: services that have not
    //!   landed yet (audio, term in Phase 57 transitional) cleanly
    //!   escalate to text-fallback.
    //! - `await_ready` re-checks the lookup; F.4 will replace this
    //!   with a `/run/services.status` poll.
    //!
    //! The full file-based supervisor protocol (`/run/init.cmd`
    //! writes for stop/restart) lands in F.4 alongside the recovery
    //! state machine. F.2 only needs the `start` path to reach
    //! `Running` when every service is up, and to escalate when one
    //! is missing.

    use kernel_core::session_supervisor::{SupervisorBackend, SupervisorError, SupervisorReply};

    /// Names that `init`'s service manifest registers under different
    /// IPC service names than the F.1 step name. The kbd_server, for
    /// instance, registers as `"kbd"`. The values here MUST match the
    /// `SERVICE_NAME` constant in each daemon's `lib.rs` (or the
    /// equivalent `ipc_register_service` call) — the binary names on
    /// the left come from
    /// [`kernel_core::session_supervisor::DECLARED_SESSION_STEP_NAMES`].
    ///
    /// Keep this list in sync with:
    /// - `display_server::SERVICE_NAME` = `"display"`
    /// - `kbd_server::SERVICE_NAME`     = `"kbd"`
    /// - `mouse_server::SERVICE_NAME`   = `"mouse"`
    /// - `audio_server::SERVICE_NAME`   = `"audio.cmd"`
    /// - `term::SERVICE_NAME`           = `"term"`
    fn ipc_service_name(step_name: &str) -> &'static str {
        match step_name {
            "display_server" => "display",
            "kbd_server" => "kbd",
            "mouse_server" => "mouse",
            "audio_server" => "audio.cmd",
            "term" => "term",
            _ => "",
        }
    }

    /// Probe whether the named service is registered with the kernel
    /// IPC registry.
    fn is_service_registered(step_name: &str) -> bool {
        let svc = ipc_service_name(step_name);
        if svc.is_empty() {
            return false;
        }
        let handle = syscall_lib::ipc_lookup_service(svc);
        handle != u64::MAX
    }

    pub struct InitSupervisorBackend {
        // No mutable state in F.2; F.4 will hold the
        // `/run/init.cmd` fd and a small write buffer.
    }

    impl InitSupervisorBackend {
        pub const fn new() -> Self {
            Self {}
        }
    }

    /// Polling interval for [`InitSupervisorBackend::await_ready`].
    /// Mirrors `kernel_core::session::RETRY_BACKOFF_MS` (200 ms) but
    /// quoted directly here so this module is the single sleep site.
    const AWAIT_POLL_MS: u32 = 200;

    impl SupervisorBackend for InitSupervisorBackend {
        fn start(&mut self, _service: &str) -> Result<SupervisorReply, SupervisorError> {
            // Phase 57 F.2 transitional: init drives the actual
            // service spawn through its `KNOWN_CONFIGS` manifest
            // walker; `session_manager` is a passive observer that
            // reports the step as Ack here and waits for the service
            // to register in [`Self::await_ready`]. Returning Ack
            // unconditionally means the F.1 sequencer's per-step
            // attempt becomes "wait for ready" rather than "wait for
            // already-registered" — which is the correct semantic
            // for an observer that doesn't itself spawn the
            // processes.
            //
            // F.4 replaces this with a `/run/init.cmd` start verb so
            // session_manager can drive the lifecycle directly.
            Ok(SupervisorReply::Ack)
        }

        fn stop(&mut self, service: &str) -> Result<SupervisorReply, SupervisorError> {
            // F.2: the rollback-on-text-fallback path stops services in
            // reverse order via init's existing supervisor. F.2's
            // adapter logs the intent so the boot transcript names
            // the rollback; F.4 replaces this with a `/run/init.cmd`
            // write.
            syscall_lib::write_str(
                syscall_lib::STDOUT_FILENO,
                "session_manager: session.recover: stop(",
            );
            syscall_lib::write_str(syscall_lib::STDOUT_FILENO, service);
            syscall_lib::write_str(
                syscall_lib::STDOUT_FILENO,
                ") — F.4 will issue init.cmd write\n",
            );
            Ok(SupervisorReply::Ack)
        }

        fn restart(&mut self, _service: &str) -> Result<SupervisorReply, SupervisorError> {
            Ok(SupervisorReply::Ack)
        }

        fn await_ready(
            &mut self,
            service: &str,
            timeout_ms: u64,
        ) -> Result<SupervisorReply, SupervisorError> {
            // Poll the IPC service registry up to `timeout_ms` waiting
            // for the named service to register. `init` spawns the
            // session services in parallel based on dependency-graph
            // order; session_manager has to wait patiently because we
            // are racing init's manifest walker. Each poll sleeps
            // `AWAIT_POLL_MS` before re-probing, so the worst-case
            // runtime is `timeout_ms + AWAIT_POLL_MS`.
            //
            // `timeout_ms == 0` reverts to the original nonblocking-
            // probe shape — useful for callers that just want a
            // snapshot.
            let deadline_polls = (timeout_ms / (AWAIT_POLL_MS as u64)).saturating_add(1);
            for _ in 0..deadline_polls {
                if is_service_registered(service) {
                    return Ok(SupervisorReply::ReadyState { ready: true });
                }
                if timeout_ms == 0 {
                    break;
                }
                let _ = syscall_lib::nanosleep_for(0, (AWAIT_POLL_MS as u32) * 1_000_000);
            }
            Ok(SupervisorReply::ReadyState { ready: false })
        }

        fn on_exit_observed(&mut self, _service: &str) -> Result<SupervisorReply, SupervisorError> {
            Ok(SupervisorReply::ExitObserved {
                exit_code: 0,
                signaled: false,
            })
        }
    }
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "session_manager: PANIC\n");
    syscall_lib::exit(101)
}
