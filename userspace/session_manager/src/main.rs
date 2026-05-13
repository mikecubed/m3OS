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
mod init_proxy;
mod recover;
mod runtime;

// Phase 64: pure-logic types (`Pid`, `ServiceState`, `ServiceTable`)
// live in the `session_manager` library crate so they host-test under
// `cargo test -p session_manager --target x86_64-unknown-linux-gnu`.
// The binary picks them up through the crate name.
#[allow(unused_imports)]
use session_manager::table::{Pid, ServiceState, ServiceTable};

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
    use session_manager::init_status::{InitServiceState, init_service_name};
    use session_manager::table::{Pid, ServiceState, ServiceTable};

    use crate::init_proxy;

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

    /// Phase 64 Track A.2: production adapter that satisfies the F.3
    /// `SupervisorBackend` trait by talking to init through its
    /// existing IPC registry **and** owning the per-service
    /// [`ServiceTable`] introduced in Phase 64.
    ///
    /// - `start(name)` is a no-op (init does the actual spawn).
    /// - `await_ready(name, timeout_ms)` polls the IPC registry until
    ///   the service registers, then queries
    ///   [`syscall_lib::ipc_lookup_service_owner_pid`] to record the
    ///   child's PID in the table — this is the moment a per-child
    ///   `ServiceState::Starting` transitions to `Running`.
    /// - `stop(name)` reads the recorded PID and drives the host-
    ///   tested `lifecycle::stop_service` via [`stop_service_blocking`],
    ///   delivering SIGTERM, the 5 s grace, SIGKILL, and the `kill(0)`
    ///   probe. Phase 57's logging-only `stop` is gone.
    /// - `restart(name)` is currently a no-op at this layer — the
    ///   Phase 57 control dispatcher's `session_restart` triggers a
    ///   whole-session rollback + re-drive rather than a per-service
    ///   restart. A typed per-service-restart verb is a future
    ///   extension (see the Phase 64 design doc "Remaining" list).
    pub struct InitSupervisorBackend {
        table: ServiceTable,
    }

    impl InitSupervisorBackend {
        pub fn new() -> Self {
            let mut table = ServiceTable::new();
            // Pre-populate one entry per declared step so the table
            // shape is stable from the first event-loop iteration
            // onward; `m3ctl session-state` then sees every service
            // (in `Starting` until ready).
            for name in kernel_core::session_supervisor::declared_session_step_names()
                .iter()
                .copied()
            {
                table.insert(name);
            }
            Self { table }
        }

        // The per-service table is read via the `services_snapshot`
        // override on the `SupervisorBackend` trait below; no
        // out-of-band accessor is required.
    }

    /// Polling interval for [`InitSupervisorBackend::await_ready`].
    /// Mirrors `kernel_core::session::RETRY_BACKOFF_MS` (200 ms) but
    /// quoted directly here so this module is the single sleep site.
    const AWAIT_POLL_MS: u32 = 200;

    impl SupervisorBackend for InitSupervisorBackend {
        fn start(&mut self, service: &str) -> Result<SupervisorReply, SupervisorError> {
            // Init drives the actual spawn via its manifest walker.
            // The table entry was pre-populated by `new`; mark it
            // explicitly as `Starting` in case a prior failed attempt
            // left it in another state.
            self.table.update_state(service, ServiceState::Starting);
            Ok(SupervisorReply::Ack)
        }

        fn stop(&mut self, service: &str) -> Result<SupervisorReply, SupervisorError> {
            // Phase 64a — durable stop via init delegation. The Phase 64
            // shipped path signalled the child directly (SIGTERM via
            // `runtime::stop_service_blocking`), but init would respawn
            // the SIGTERM-signaled child within `restart_delay_secs` per
            // its `restart=on-failure` policy. The fix is to delegate
            // to init's `/run/init.cmd` `stop <name>` verb, which sets
            // `restart_policy = Never` before signalling so the stop
            // is durable.
            let init_name = init_service_name(service);
            if init_name.is_empty() {
                syscall_lib::write_str(
                    syscall_lib::STDOUT_FILENO,
                    "session_manager: lifecycle.stop: unknown service '",
                );
                syscall_lib::write_str(syscall_lib::STDOUT_FILENO, service);
                syscall_lib::write_str(syscall_lib::STDOUT_FILENO, "'\n");
                return Err(SupervisorError::UnknownService);
            }
            self.table.update_state(service, ServiceState::Stopping);
            syscall_lib::write_str(
                syscall_lib::STDOUT_FILENO,
                "session_manager: lifecycle.stop: '",
            );
            syscall_lib::write_str(syscall_lib::STDOUT_FILENO, service);
            syscall_lib::write_str(
                syscall_lib::STDOUT_FILENO,
                "': delegating to init (Phase 64a)\n",
            );
            if init_proxy::cmd_stop(init_name).is_err() {
                syscall_lib::write_str(
                    syscall_lib::STDOUT_FILENO,
                    "session_manager: lifecycle.stop: '",
                );
                syscall_lib::write_str(syscall_lib::STDOUT_FILENO, service);
                syscall_lib::write_str(syscall_lib::STDOUT_FILENO, "': init.cmd write failed\n");
                self.table.update_state(service, ServiceState::Failed);
                return Err(SupervisorError::NotRunning);
            }
            // Wait up to ~6 s for init's stop motion (5 s SIGTERM grace
            // + 1 s SIGKILL reap) to land in /run/services.status. Init
            // writes the status file every ~1 s, so 12 × 500 ms covers
            // the worst case.
            const STATUS_POLL_ITERS: u32 = 12;
            const STATUS_POLL_NS: u32 = 500_000_000;
            let mut observed_terminal = false;
            for _ in 0..STATUS_POLL_ITERS {
                if let Some(status) = init_proxy::read_service_status(init_name) {
                    if status.state.is_terminal() {
                        observed_terminal = true;
                        break;
                    }
                }
                let _ = syscall_lib::nanosleep_for(0, STATUS_POLL_NS);
            }
            if !observed_terminal {
                syscall_lib::write_str(
                    syscall_lib::STDOUT_FILENO,
                    "session_manager: lifecycle.stop: '",
                );
                syscall_lib::write_str(syscall_lib::STDOUT_FILENO, service);
                syscall_lib::write_str(
                    syscall_lib::STDOUT_FILENO,
                    "': timed out waiting for init to reap\n",
                );
                self.table.update_state(service, ServiceState::Failed);
                return Err(SupervisorError::NotRunning);
            }
            self.table.update_pid(service, None);
            // Init's `stop` sets restart_policy=Never, so the service
            // will not respawn until an operator issues a `restart` or
            // re-enables the manifest entry. Mark `Starting` with
            // `pid=None` — the table semantics for "not running but
            // not failed". A future codec extension can introduce a
            // dedicated `Stopped` tag if operator UX needs it.
            self.table.update_state(service, ServiceState::Starting);
            Ok(SupervisorReply::Ack)
        }

        fn restart(&mut self, service: &str) -> Result<SupervisorReply, SupervisorError> {
            // Phase 64a — per-service restart via init delegation.
            // Writes `restart <init_name>` to /run/init.cmd; init's
            // handler stops the service, resets restart_count to 0,
            // and re-starts it through the normal manifest-driven
            // path. We observe completion by polling
            // /run/services.status until the service is `running`
            // again with a non-zero PID (and ideally a different PID
            // than before, to confirm a genuine restart rather than a
            // stale snapshot). The whole motion can take 1–6 s
            // depending on init's restart_delay backoff.
            let init_name = init_service_name(service);
            if init_name.is_empty() {
                return Err(SupervisorError::UnknownService);
            }
            let prior_pid = self.table.get_pid(service).map(|p| p.0).unwrap_or(0);
            self.table.update_state(service, ServiceState::Restarting);
            syscall_lib::write_str(
                syscall_lib::STDOUT_FILENO,
                "session_manager: lifecycle.restart: '",
            );
            syscall_lib::write_str(syscall_lib::STDOUT_FILENO, service);
            syscall_lib::write_str(
                syscall_lib::STDOUT_FILENO,
                "': delegating to init (Phase 64a)\n",
            );
            if init_proxy::cmd_restart(init_name).is_err() {
                self.table.update_state(service, ServiceState::Failed);
                return Err(SupervisorError::NotRunning);
            }
            // Wait for init to converge. Worst case: 5 s SIGTERM grace
            // + 1 s SIGKILL reap + restart_delay (up to 60 s for high
            // restart_count, but normally 1 s). Cap the wait at ~15 s
            // total — beyond that the operator should investigate via
            // `m3ctl session-state --detailed`.
            const STATUS_POLL_ITERS: u32 = 30;
            const STATUS_POLL_NS: u32 = 500_000_000;
            let mut observed_new_pid = false;
            for _ in 0..STATUS_POLL_ITERS {
                if let Some(status) = init_proxy::read_service_status(init_name) {
                    if matches!(status.state, InitServiceState::Running)
                        && status.pid > 0
                        && (prior_pid == 0 || status.pid != prior_pid)
                    {
                        self.table.update_pid(service, Some(Pid(status.pid)));
                        self.table.update_state(service, ServiceState::Running);
                        observed_new_pid = true;
                        break;
                    }
                }
                let _ = syscall_lib::nanosleep_for(0, STATUS_POLL_NS);
            }
            if !observed_new_pid {
                syscall_lib::write_str(
                    syscall_lib::STDOUT_FILENO,
                    "session_manager: lifecycle.restart: '",
                );
                syscall_lib::write_str(syscall_lib::STDOUT_FILENO, service);
                syscall_lib::write_str(
                    syscall_lib::STDOUT_FILENO,
                    "': timed out waiting for init restart to converge\n",
                );
                self.table.update_state(service, ServiceState::Failed);
                return Err(SupervisorError::NotRunning);
            }
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
                    // Phase 64: only declare the service `Running` if
                    // we can also pin down its owner PID. A registered
                    // service whose PID lookup fails (kernel-owned,
                    // private name gated, or registry corruption) would
                    // otherwise leave the table in a self-inconsistent
                    // state — `Running` with `pid=None` makes a later
                    // `stop()` an idempotent no-op while operators see
                    // a healthy service. Treat the missing PID as
                    // not-ready so the sequencer keeps polling until
                    // either the PID surfaces or the budget escalates
                    // to text-fallback.
                    let registry_name = ipc_service_name(service);
                    let pid = if registry_name.is_empty() {
                        None
                    } else {
                        syscall_lib::ipc_lookup_service_owner_pid(registry_name)
                    };
                    match pid {
                        Some(pid) => {
                            self.table.update_pid(service, Some(Pid(pid)));
                            self.table.update_state(service, ServiceState::Running);
                            return Ok(SupervisorReply::ReadyState { ready: true });
                        }
                        None => {
                            syscall_lib::write_str(
                                syscall_lib::STDOUT_FILENO,
                                "session_manager: await_ready: '",
                            );
                            syscall_lib::write_str(syscall_lib::STDOUT_FILENO, service);
                            syscall_lib::write_str(
                                syscall_lib::STDOUT_FILENO,
                                "': registered but owner PID lookup failed; keeping Starting\n",
                            );
                            // Fall through to the poll-and-retry path.
                        }
                    }
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

        /// Phase 64 — populate one `ServiceStateEntry` per `ServiceTable`
        /// entry for the `SessionStateDetailed` control verb. Stops at
        /// `MAX_SERVICE_STATE_ENTRIES` to keep the reply allocation-free.
        fn services_snapshot(
            &mut self,
        ) -> (
            u8,
            [kernel_core::session_control::ServiceStateEntry;
                kernel_core::session_control::MAX_SERVICE_STATE_ENTRIES],
        ) {
            use kernel_core::session_control::{
                MAX_SERVICE_STATE_ENTRIES, MAX_STEP_NAME_BYTES, PER_SVC_FAILED, PER_SVC_RESTARTING,
                PER_SVC_RUNNING, PER_SVC_STARTING, PER_SVC_STOPPING, ServiceStateEntry,
            };

            let mut entries = [ServiceStateEntry::empty(); MAX_SERVICE_STATE_ENTRIES];
            let mut count: usize = 0;
            for entry in self.table.iter() {
                if count >= MAX_SERVICE_STATE_ENTRIES {
                    break;
                }
                let bytes = entry.name.as_bytes();
                let name_len = bytes.len().min(MAX_STEP_NAME_BYTES);
                entries[count].name_len = name_len as u8;
                entries[count].name[..name_len].copy_from_slice(&bytes[..name_len]);
                entries[count].state_tag = match entry.state {
                    ServiceState::Starting => PER_SVC_STARTING,
                    ServiceState::Running => PER_SVC_RUNNING,
                    ServiceState::Stopping => PER_SVC_STOPPING,
                    ServiceState::Restarting => PER_SVC_RESTARTING,
                    ServiceState::Failed => PER_SVC_FAILED,
                };
                entries[count].restart_count = entry.restart_count;
                entries[count].step_failures = entry.step_failures;
                count += 1;
            }
            (count as u8, entries)
        }
    }
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "session_manager: PANIC\n");
    syscall_lib::exit(101)
}
