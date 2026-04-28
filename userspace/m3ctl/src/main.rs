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
        ParsedVerb, SESSION_CONTROL_SERVICE_NAME, parse_verb,
    };
    use syscall_lib::STDOUT_FILENO;
    use syscall_lib::heap::BrkAllocator;

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
    /// is a `Recovering` state with a 32-byte step name + 4-byte retry
    /// count + small header. 64 leaves headroom.
    const SESSION_REPLY_MAX: usize = 64;

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
        }
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

        let mut req_buf = [0u8; 8];
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
            // `ControlEvent` is `#[non_exhaustive]`; future variants
            // print a typed marker rather than panicking.
            _ => {
                print_str("(unknown event variant)\n");
            }
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
             Session verbs (Phase 57 I.2):\n  \
               session-state                   Print session_manager's current state\n  \
               session-stop                    Graceful shutdown (falls through to text-fallback)\n  \
               session-restart                 Graceful stop + start\n",
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
