//! Phase 56 Track E.4 — `m3ctl`, the minimal control-socket client.
//!
//! Phase 57 Track I.2 extends `m3ctl` with three session-control verbs
//! (`session-state` / `session-stop` / `session-restart`) that target
//! the `session_manager` daemon's separate control socket. Verb parsing
//! lives in the library (`src/lib.rs`); the binary is a thin shell
//! that:
//!
//! 1. delegates argv parsing to [`m3ctl::parse_verb`],
//! 2. looks up the right service-registry endpoint per the parsed
//!    [`m3ctl::ParsedVerb`] variant,
//! 3. encodes the verb via the corresponding `kernel-core` codec,
//! 4. issues an `ipc_call_buf`, drains the staged reply, decodes the
//!    typed reply payload, and prints a human-readable summary.
//!
//! # Verbs implemented
//!
//! Phase 56 (display-control surface):
//!
//! * `m3ctl version` — prints the protocol version
//! * `m3ctl list-surfaces` — prints one `SurfaceId` per line
//! * `m3ctl frame-stats` — prints the rolling window of frame
//!   composition samples
//! * `m3ctl focus <id>` — moves keyboard focus to the surface
//! * `m3ctl register-bind <mask> <keycode>` — registers a keybind
//! * `m3ctl unregister-bind <mask> <keycode>` — unregisters a keybind
//! * `m3ctl subscribe <kind>` — sends a subscribe verb (returns Ack)
//!
//! Phase 57 (session-control surface, F.5 → I.2):
//!
//! * `m3ctl session-state` — prints the current session state
//! * `m3ctl session-stop` — graceful shutdown (falls through to text-fallback)
//! * `m3ctl session-restart` — graceful stop + start
//!
//! # Engineering discipline
//!
//! No `unwrap` / `expect` / `panic!` outside test code. Every fallible
//! syscall is checked and reported via `syscall_lib::write_str`.
//!
//! # Service-lookup retry
//!
//! Mirrors the `display_server::input::lookup_with_backoff` shape (8
//! attempts, 5 ms between) so this binary can be invoked at any point
//! during boot without racing the target daemon's register.
#![cfg_attr(feature = "os-binary", no_std)]
#![cfg_attr(feature = "os-binary", no_main)]
#![cfg_attr(feature = "os-binary", feature(alloc_error_handler))]

#[cfg(feature = "os-binary")]
extern crate alloc;

#[cfg(feature = "os-binary")]
mod os_binary {
    use alloc::format;
    use alloc::vec;
    use alloc::vec::Vec;
    use core::alloc::Layout;

    use kernel_core::display::control::{
        ControlError, ControlErrorCode, ControlEvent, SurfaceRoleTag, decode_event, encode_command,
    };
    use kernel_core::session::SessionState;
    use kernel_core::session_control::{
        ControlReply, SessionControlError, decode_reply, encode_verb,
    };
    use m3ctl::{
        DISPLAY_CONTROL_SERVICE_NAME, LABEL_DISPLAY_CTL_CMD, LABEL_SESSION_CTL_CMD, ParseError,
        ParsedVerb, SESSION_CONTROL_SERVICE_NAME, WIFI_CONTROL_SERVICE_NAME,
        WIFI_NOT_ASSOCIATED_MSG, format_wifi_status, parse_verb,
    };
    use syscall_lib::STDOUT_FILENO;
    use syscall_lib::heap::BrkAllocator;
    use wifi_core::control::{WIFI_STATUS, WifiStatus};

    #[global_allocator]
    static ALLOCATOR: BrkAllocator = BrkAllocator::new();

    #[alloc_error_handler]
    fn alloc_error(_layout: Layout) -> ! {
        syscall_lib::write_str(STDOUT_FILENO, "m3ctl: alloc error\n");
        syscall_lib::exit(99)
    }

    /// Maximum buffer size — matches the kernel's `MAX_BULK_LEN`.
    const MAX_BULK_BYTES: usize = 4096;

    /// Maximum size of an encoded session reply. The largest variant
    /// is the Phase 64 `ServiceStates` reply: up to 8 per-service
    /// quads — `(name, state, restart_count, step_failures)`. Each
    /// quad is 42 bytes on the wire: 1 byte name_len + ≤32 bytes
    /// name + 1 byte state tag + 4 bytes restart_count + 4 bytes
    /// step_failures. Plus a 2-byte header → 338 bytes. We round to
    /// 384 to leave headroom and to stay in lock-step with
    /// `session_manager::control::MAX_CONTROL_BUF`.
    const SESSION_REPLY_MAX: usize = 384;

    /// Service-lookup retry attempts before giving up. Same shape as
    /// `display_server::input::lookup_with_backoff`.
    const SERVICE_LOOKUP_ATTEMPTS: u32 = 8;

    /// Backoff between service-lookup attempts (5 ms).
    const SERVICE_LOOKUP_BACKOFF_NS: u32 = 5_000_000;

    syscall_lib::entry_point!(program_main);

    fn program_main(args: &[&str]) -> i32 {
        let verb = match args.get(1) {
            Some(v) => *v,
            None => {
                print_usage();
                return 2;
            }
        };
        let rest: &[&str] = if args.len() >= 2 { &args[2..] } else { &[] };

        let parsed = match parse_verb(verb, rest) {
            Ok(p) => p,
            Err(err) => {
                print_str("m3ctl: ");
                print_str(parse_error_label(&err));
                print_str("\n");
                print_usage();
                return 2;
            }
        };

        match parsed {
            ParsedVerb::Display(cmd) => dispatch_display(cmd),
            ParsedVerb::Session(verb) => dispatch_session(verb),
            ParsedVerb::LockScreen => dispatch_lock(),
            ParsedVerb::WifiStatus => dispatch_wifi_status(),
        }
    }

    // -----------------------------------------------------------------------
    // Phase 81 D.2 — Wi-Fi status dispatch
    // -----------------------------------------------------------------------

    /// `m3ctl wifi status` — query the mt792x driver's userspace control
    /// endpoint for the current association status and print it.
    ///
    /// When the `wifi.control` service is absent (no Wi-Fi driver, or the radio
    /// has not associated) or the driver reports `NotAssociated`, prints
    /// "wifi: not associated" and exits 0 — a read-only diagnostic should not
    /// error just because Wi-Fi is down.
    fn dispatch_wifi_status() -> i32 {
        let handle = match lookup_with_backoff(WIFI_CONTROL_SERVICE_NAME) {
            Some(h) => h,
            None => {
                print_str(WIFI_NOT_ASSOCIATED_MSG);
                print_str("\n");
                return 0;
            }
        };

        let reply_label = syscall_lib::ipc_call_buf(handle, WIFI_STATUS as u64, 0, &[]);
        if reply_label == u64::MAX {
            print_str(WIFI_NOT_ASSOCIATED_MSG);
            print_str("\n");
            return 0;
        }

        let mut reply_buf = vec![0u8; MAX_BULK_BYTES];
        let n = syscall_lib::ipc_take_pending_bulk(&mut reply_buf);
        if n == u64::MAX || n == 0 {
            print_str(WIFI_NOT_ASSOCIATED_MSG);
            print_str("\n");
            return 0;
        }

        match WifiStatus::decode(&reply_buf[..n as usize]) {
            Some(status) if !status.ssid.is_empty() => {
                print_str(&format_wifi_status(&status));
                0
            }
            // Empty SSID or undecodable reply ⇒ not associated.
            _ => {
                print_str(WIFI_NOT_ASSOCIATED_MSG);
                print_str("\n");
                0
            }
        }
    }

    /// Phase 73 — spawn `/bin/lockscreen` directly. We don't extend
    /// the wire protocol with a `Lock` verb because the lockscreen is
    /// a regular compositor client; the compositor handles the
    /// exclusive-keyboard grant via `LayerConfig::keyboard_interactivity`.
    fn dispatch_lock() -> i32 {
        let pid = syscall_lib::fork();
        if pid < 0 {
            print_str("m3ctl: fork failed\n");
            return 1;
        }
        if pid == 0 {
            let path = b"/bin/lockscreen\0";
            let argv: [*const u8; 2] = [path.as_ptr(), core::ptr::null()];
            let envp: [*const u8; 1] = [core::ptr::null()];
            let _ = syscall_lib::execve(path, &argv, &envp);
            print_str("m3ctl: execve lockscreen failed\n");
            syscall_lib::exit(127);
        }
        0
    }

    fn parse_error_label(err: &ParseError) -> &str {
        match err {
            ParseError::UnknownVerb(_) => "unknown verb",
            ParseError::MissingArgument(msg) => msg,
            ParseError::BadArgument(msg) => msg,
            ParseError::UnknownEventKind(_) => {
                "subscribe: kind must be one of \
                 surface-created | surface-destroyed | focus-changed | bind-triggered"
            }
        }
    }

    // -----------------------------------------------------------------------
    // Phase 56 — display-control dispatch
    // -----------------------------------------------------------------------

    fn dispatch_display(cmd: kernel_core::display::control::ControlCommand) -> i32 {
        let handle = match lookup_with_backoff(DISPLAY_CONTROL_SERVICE_NAME) {
            Some(h) => h,
            None => {
                print_str("m3ctl: failed to look up display-control service\n");
                return 1;
            }
        };

        let mut req_buf = [0u8; 64];
        let req_len = match encode_command(&cmd, &mut req_buf) {
            Ok(n) => n,
            Err(_) => {
                print_str("m3ctl: failed to encode command\n");
                return 1;
            }
        };

        let reply_label =
            syscall_lib::ipc_call_buf(handle, LABEL_DISPLAY_CTL_CMD, 0, &req_buf[..req_len]);
        if reply_label == u64::MAX {
            print_str("m3ctl: ipc_call_buf failed\n");
            return 1;
        }

        let mut reply_buf = vec![0u8; MAX_BULK_BYTES];
        let n = syscall_lib::ipc_take_pending_bulk(&mut reply_buf);
        if n == u64::MAX {
            print_str("m3ctl: ipc_take_pending_bulk failed\n");
            return 1;
        }
        if n == 0 {
            let fallback = synthetic_display_reply_for(&cmd);
            print_event(&fallback);
            return 0;
        }

        let used = n as usize;
        match decode_event(&reply_buf[..used]) {
            Ok((ev, _)) => {
                print_event(&ev);
                // Phase 72b Track K.8 — Subscribe is the entry point
                // for a long-lived event stream. After the initial
                // Ack lands, poll `EventPull` until SIGINT and print
                // each delivered event. Other verbs return their
                // single reply and exit.
                if matches!(
                    cmd,
                    kernel_core::display::control::ControlCommand::Subscribe { .. }
                ) {
                    return poll_subscription_events(handle, &mut reply_buf);
                }
                0
            }
            Err(err) => {
                print_str("m3ctl: failed to decode reply: ");
                print_str(control_error_label(err));
                print_str("\n");
                1
            }
        }
    }

    /// Phase 72b Track K.8 — `m3ctl subscribe` polling loop. After
    /// the initial Subscribe Ack, repeatedly call `EventPull` and
    /// print each delivered event. An empty queue returns `Ack`; the
    /// loop sleeps briefly between empty polls so it doesn't burn the
    /// CPU. SIGINT (Ctrl+C) tears the process down via the shell.
    fn poll_subscription_events(handle: u32, reply_buf: &mut [u8]) -> i32 {
        use kernel_core::display::control::ControlCommand;
        let mut req_buf = [0u8; 8];
        let req_len = match encode_command(&ControlCommand::EventPull, &mut req_buf) {
            Ok(n) => n,
            Err(_) => {
                print_str("m3ctl: failed to encode EventPull\n");
                return 1;
            }
        };
        loop {
            let label =
                syscall_lib::ipc_call_buf(handle, LABEL_DISPLAY_CTL_CMD, 0, &req_buf[..req_len]);
            if label == u64::MAX {
                print_str("m3ctl: subscribe: transport error\n");
                return 1;
            }
            let n = syscall_lib::ipc_take_pending_bulk(reply_buf);
            if n != u64::MAX && n > 0 {
                let used = n as usize;
                if let Ok((ev, _)) = decode_event(&reply_buf[..used]) {
                    if !matches!(ev, ControlEvent::Ack) {
                        print_event(&ev);
                        continue;
                    }
                }
            }
            // Empty queue / Ack — back off so we don't pin the CPU.
            // 100 ms keeps `m3ctl subscribe` responsive enough that
            // events surface within ~one frame of the firing event.
            let _ = syscall_lib::nanosleep_for(0, 100_000_000);
        }
    }

    // -----------------------------------------------------------------------
    // Phase 57 I.2 — session-control dispatch
    // -----------------------------------------------------------------------

    fn dispatch_session(verb: kernel_core::session_control::ControlVerb) -> i32 {
        let handle = match lookup_with_backoff(SESSION_CONTROL_SERVICE_NAME) {
            Some(h) => h,
            None => {
                print_str("m3ctl: failed to look up session-control service\n");
                return 1;
            }
        };

        // Phase 64a: `SessionRestartService` encodes as
        // `[tag][name_len][name…]`, up to 2 + `MAX_STEP_NAME_BYTES` = 34
        // bytes. 48 leaves headroom for future single-payload verbs.
        let mut req_buf = [0u8; 48];
        let req_len = match encode_verb(&verb, &mut req_buf) {
            Ok(n) => n,
            Err(_) => {
                print_str("m3ctl: failed to encode session verb\n");
                return 1;
            }
        };

        let reply_label =
            syscall_lib::ipc_call_buf(handle, LABEL_SESSION_CTL_CMD, 0, &req_buf[..req_len]);
        if reply_label == u64::MAX {
            print_str("m3ctl: ipc_call_buf failed\n");
            return 1;
        }

        let mut reply_buf = [0u8; SESSION_REPLY_MAX];
        let n = syscall_lib::ipc_take_pending_bulk(&mut reply_buf);
        if n == u64::MAX {
            print_str("m3ctl: ipc_take_pending_bulk failed\n");
            return 1;
        }
        if n == 0 {
            // Session control verbs always reply with bulk; no
            // synthetic fallback. Treat as transport-level error so
            // the operator sees the right diagnostic.
            print_str("m3ctl: session-control reply was empty\n");
            return 1;
        }

        let used = n as usize;
        match decode_reply(&reply_buf[..used]) {
            Ok(reply) => {
                print_session_reply(&reply);
                0
            }
            Err(err) => {
                print_str("m3ctl: failed to decode session reply: ");
                print_str(session_error_label(err));
                print_str("\n");
                1
            }
        }
    }

    fn print_session_reply(reply: &ControlReply) {
        match reply {
            ControlReply::State { state } => {
                print_str("state=");
                print_str(session_state_label(state));
                print_str("\n");
            }
            ControlReply::Ack => {
                print_str("ack\n");
            }
            ControlReply::Error(err) => {
                print_str("error: ");
                print_str(session_error_label(*err));
                print_str("\n");
            }
            // Phase 64 — `SessionStateDetailed` reply path. The legacy
            // `session-state` verb doesn't request this variant; a
            // future client adds the request and reuses this printer,
            // which exposes the full per-service quad so operators can
            // see when the restart-budget counters are getting close
            // to their limits.
            ControlReply::ServiceStates {
                entry_count,
                entries,
            } => {
                let count = (*entry_count as usize).min(entries.len());
                for entry in entries.iter().take(count) {
                    let name = entry.name_as_str().unwrap_or("?");
                    print_str("service ");
                    print_str(name);
                    print_str(" state=");
                    print_str(per_svc_state_label(entry.state_tag));
                    print_str(" restart_count=");
                    print_u32(entry.restart_count);
                    print_str(" step_failures=");
                    print_u32(entry.step_failures);
                    print_str("\n");
                }
                if count == 0 {
                    print_str("services: (empty)\n");
                }
            }
        }
    }

    /// Map a Phase 64 per-service state tag to a stable string label
    /// suitable for the `m3ctl session-state` output. Mirrors the
    /// `PER_SVC_*` constants in `kernel_core::session_control`.
    fn per_svc_state_label(tag: u8) -> &'static str {
        use kernel_core::session_control::{
            PER_SVC_FAILED, PER_SVC_RESTARTING, PER_SVC_RUNNING, PER_SVC_STARTING, PER_SVC_STOPPING,
        };
        match tag {
            x if x == PER_SVC_STARTING => "starting",
            x if x == PER_SVC_RUNNING => "running",
            x if x == PER_SVC_STOPPING => "stopping",
            x if x == PER_SVC_RESTARTING => "restarting",
            x if x == PER_SVC_FAILED => "failed",
            _ => "unknown",
        }
    }

    fn session_state_label(state: &SessionState) -> &'static str {
        match state {
            SessionState::Booting => "booting",
            SessionState::Running => "running",
            SessionState::Recovering { .. } => "recovering",
            SessionState::TextFallback => "text-fallback",
        }
    }

    fn session_error_label(err: SessionControlError) -> &'static str {
        match err {
            SessionControlError::CapabilityMissing => "capability-missing",
            SessionControlError::MalformedRequest => "malformed-request",
            SessionControlError::Internal => "internal",
        }
    }

    // -----------------------------------------------------------------------
    // Display reply formatting (preserved from Phase 56)
    // -----------------------------------------------------------------------

    fn control_error_label(err: ControlError) -> &'static str {
        match err {
            ControlError::UnknownVerb { .. } => "unknown-verb",
            ControlError::MalformedFrame => "malformed-frame",
            ControlError::BadArgs { .. } => "bad-args",
            // ControlError is `#[non_exhaustive]`; future variants surface
            // as a generic label rather than panicking.
            _ => "control-error",
        }
    }

    fn print_event(evt: &ControlEvent) {
        match evt {
            ControlEvent::VersionReply { protocol_version } => {
                print_str("protocol_version=");
                print_u32(*protocol_version);
                print_str("\n");
            }
            ControlEvent::SurfaceListReply { ids } => {
                if ids.is_empty() {
                    print_str("(no surfaces)\n");
                } else {
                    for id in ids {
                        print_str("surface ");
                        print_u32(id.0);
                        print_str("\n");
                    }
                }
            }
            ControlEvent::Ack => {
                print_str("ack\n");
            }
            ControlEvent::Error { code } => {
                print_str("error: ");
                print_str(error_code_str(*code));
                print_str("\n");
            }
            ControlEvent::FrameStatsReply { samples } => {
                if samples.is_empty() {
                    print_str("(no frame samples yet)\n");
                } else {
                    for s in samples {
                        print_str("frame ");
                        print_u64(s.frame_index);
                        print_str(" compose_us=");
                        print_u32(s.compose_micros);
                        print_str("\n");
                    }
                }
            }
            ControlEvent::SurfaceCreated { surface_id, role } => {
                print_str("surface-created id=");
                print_u32(surface_id.0);
                print_str(" role=");
                print_str(role_tag_str(*role));
                print_str("\n");
            }
            ControlEvent::SurfaceDestroyed { surface_id } => {
                print_str("surface-destroyed id=");
                print_u32(surface_id.0);
                print_str("\n");
            }
            ControlEvent::FocusChanged { focused } => {
                print_str("focus-changed ");
                match focused {
                    Some(id) => {
                        print_str("id=");
                        print_u32(id.0);
                    }
                    None => print_str("none"),
                }
                print_str("\n");
            }
            ControlEvent::BindTriggered {
                modifier_mask,
                keycode,
            } => {
                print_str("bind-triggered mask=0x");
                print_str(&format!("{:04x}", modifier_mask));
                print_str(" keycode=");
                print_u32(*keycode);
                print_str("\n");
            }
            ControlEvent::WindowListReply { entries } => {
                if entries.is_empty() {
                    print_str("(no windows)\n");
                } else {
                    for e in entries {
                        print_str("window id=");
                        print_u32(e.surface_id.0);
                        print_str(" ws=");
                        print_u32(e.workspace as u32);
                        print_str(" rect=(");
                        print_u32(e.rect.x as u32);
                        print_str(",");
                        print_u32(e.rect.y as u32);
                        print_str(" ");
                        print_u32(e.rect.w);
                        print_str("x");
                        print_u32(e.rect.h);
                        print_str(")");
                        if e.focused {
                            print_str(" *focused*");
                        }
                        print_str("\n");
                    }
                }
            }
            ControlEvent::WorkspaceListReply { entries } => {
                for e in entries {
                    print_str("workspace ");
                    print_u32(e.workspace as u32);
                    print_str(" policy=");
                    print_str(policy_kind_str(e.policy_kind));
                    print_str(" windows=");
                    print_u32(e.window_count);
                    if e.active {
                        print_str(" *active*");
                    }
                    print_str("\n");
                }
            }
            ControlEvent::WorkspaceChanged { workspace } => {
                print_str("workspace-changed ws=");
                print_u32(*workspace as u32);
                print_str("\n");
            }
            // `ControlEvent` is `#[non_exhaustive]`; future variants
            // print a typed marker rather than panicking.
            _ => {
                print_str("(unknown event variant)\n");
            }
        }
    }

    /// Phase 72 — map the wire-side `policy_kind` byte back to its
    /// canonical name for the `query workspaces` printout. Mirrors
    /// `m3ctl::parse_policy_kind` in the reverse direction.
    fn policy_kind_str(k: u8) -> &'static str {
        match k {
            0 => "master-stack",
            1 => "dwindle",
            2 => "spiral",
            3 => "grid",
            4 => "tabbed",
            5 => "fullscreen",
            _ => "unknown",
        }
    }

    fn error_code_str(code: ControlErrorCode) -> &'static str {
        match code {
            ControlErrorCode::UnknownVerb => "unknown-verb",
            ControlErrorCode::MalformedFrame => "malformed-frame",
            ControlErrorCode::BadArgs => "bad-args",
            ControlErrorCode::UnknownSurface => "unknown-surface",
            ControlErrorCode::ResourceExhausted => "resource-exhausted",
            _ => "unknown-error",
        }
    }

    fn role_tag_str(tag: SurfaceRoleTag) -> &'static str {
        match tag {
            SurfaceRoleTag::Toplevel => "toplevel",
            SurfaceRoleTag::Layer => "layer",
            SurfaceRoleTag::Cursor => "cursor",
        }
    }

    /// Synthesise a structurally-correct fallback display reply when
    /// `ipc_take_pending_bulk` returns 0 bytes (preserved from Phase 56
    /// E.4). Session verbs do not use this path — they always reply
    /// with bulk.
    fn synthetic_display_reply_for(
        cmd: &kernel_core::display::control::ControlCommand,
    ) -> ControlEvent {
        use kernel_core::display::control::PROTOCOL_VERSION;
        match cmd {
            kernel_core::display::control::ControlCommand::Version => ControlEvent::VersionReply {
                protocol_version: PROTOCOL_VERSION,
            },
            kernel_core::display::control::ControlCommand::ListSurfaces => {
                ControlEvent::SurfaceListReply { ids: Vec::new() }
            }
            kernel_core::display::control::ControlCommand::FrameStats => {
                ControlEvent::FrameStatsReply {
                    samples: Vec::new(),
                }
            }
            _ => ControlEvent::Ack,
        }
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

    fn print_str(s: &str) {
        syscall_lib::write_str(STDOUT_FILENO, s);
    }

    fn print_u32(v: u32) {
        print_str(&format!("{}", v));
    }

    fn print_u64(v: u64) {
        print_str(&format!("{}", v));
    }

    fn print_usage() {
        print_str(
            "Usage: m3ctl <verb> [args...]\n\
             \n\
             Display verbs (Phase 56):\n  \
               version                         Print the control-socket protocol version\n  \
               list-surfaces                   Print every registered surface id\n  \
               frame-stats                     Print the rolling frame-composition window\n  \
               focus <surface-id>              Move keyboard focus\n  \
               register-bind <mask> <keycode>  Register a keybind\n  \
               unregister-bind <mask> <keycode> Unregister a keybind\n  \
               subscribe <kind>                Subscribe to event-stream of <kind>\n\
             \n\
             Session verbs (Phase 57 I.2 + Phase 64a):\n  \
               session-state [--detailed]      Print session_manager's current state\n                                   \
                                              (with --detailed: per-service PID/state/restart_count/step_failures)\n  \
               session-stop                    Graceful shutdown (falls through to text-fallback)\n  \
               session-restart [<name>]        With no arg: graceful whole-session stop + start\n                                   \
                                              With <name>: restart a single declared service via init\n",
        );
    }

    #[panic_handler]
    fn panic(_: &core::panic::PanicInfo) -> ! {
        syscall_lib::write_str(STDOUT_FILENO, "m3ctl: PANIC\n");
        syscall_lib::exit(101)
    }
}

// When the `os-binary` feature is *not* set (e.g., during host tests),
// the file compiles as a normal `std` binary with a no-op main so
// cargo's bin-target build does not fail. Tests compile against the
// `lib` target only.
#[cfg(not(feature = "os-binary"))]
fn main() {}
