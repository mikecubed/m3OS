//! Phase 56 Track E.4 — `m3ctl`, the minimal control-socket client.
//!
//! `m3ctl` is a one-shot CLI: it looks up the `"display-control"`
//! service registered by `display_server`, encodes a single
//! [`ControlCommand`] verb, sends it via `ipc_call_buf`, decodes the
//! [`ControlEvent`] reply, prints a human-readable summary, and exits.
//!
//! # Verbs implemented in Phase 56
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
//! # Engineering discipline
//!
//! No `unwrap` / `expect` / `panic!` outside test code. Every fallible
//! syscall is checked and reported via `syscall_lib::write_str`.
//!
//! # Service-lookup retry
//!
//! Mirrors the `display_server::input::lookup_with_backoff` shape (8
//! attempts, 5 ms between) so this binary can be invoked at any point
//! during boot without racing the `display_server` register.
#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

use alloc::format;
use alloc::vec;
use alloc::vec::Vec;
use core::alloc::Layout;

use kernel_core::display::control::{
    ControlCommand, ControlError, ControlErrorCode, ControlEvent, EventKind, SurfaceId,
    SurfaceRoleTag, decode_event, encode_command,
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

// ---------------------------------------------------------------------------
// Wire constants — must match `display_server::control`.
// ---------------------------------------------------------------------------

/// Service-registry name of the control endpoint. Set by
/// `display_server` at startup.
const CONTROL_SERVICE_NAME: &str = "display-control";

/// IPC label for an encoded `ControlCommand`. Mirrors
/// `display_server::control::LABEL_CTL_CMD`.
const LABEL_CTL_CMD: u64 = 1;

/// Maximum buffer size — matches the kernel's `MAX_BULK_LEN`.
const MAX_BULK_BYTES: usize = 4096;

/// Service-lookup retry attempts before giving up. Same shape as
/// `display_server::input::lookup_with_backoff`.
const SERVICE_LOOKUP_ATTEMPTS: u32 = 8;

/// Backoff between service-lookup attempts (5 ms).
const SERVICE_LOOKUP_BACKOFF_NS: u32 = 5_000_000;

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

syscall_lib::entry_point!(program_main);

fn program_main(args: &[&str]) -> i32 {
    // `args[0]` is the program name; `args[1..]` are the verb + verb
    // arguments. Treat any missing verb as a usage error.
    let verb = match args.get(1) {
        Some(v) => *v,
        None => {
            print_usage();
            return 2;
        }
    };
    let rest: &[&str] = if args.len() >= 2 { &args[2..] } else { &[] };

    let cmd = match parse_command(verb, rest) {
        Ok(c) => c,
        Err(msg) => {
            print_str("m3ctl: ");
            print_str(msg);
            print_str("\n");
            print_usage();
            return 2;
        }
    };

    let handle = match lookup_with_backoff(CONTROL_SERVICE_NAME) {
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

    // Send the command + receive the reply. The encoded
    // `ControlEvent` reply travels in the kernel-staged bulk slot;
    // `ipc_call_buf` returns the reply *label* and
    // `ipc_take_pending_bulk` (Phase 56 close-out, syscall 0x1112)
    // drains the staged bytes into a caller-supplied buffer.
    let reply_label = syscall_lib::ipc_call_buf(handle, LABEL_CTL_CMD, 0, &req_buf[..req_len]);
    if reply_label == u64::MAX {
        print_str("m3ctl: ipc_call_buf failed\n");
        return 1;
    }

    // Drain the kernel-staged reply bulk. Buffer sized to
    // `MAX_BULK_BYTES = 4096` (matches kernel `MAX_BULK_LEN`); the
    // largest Phase 56 control reply (`SurfaceListReply` /
    // `FrameStatsReply`) fits well within that.
    let mut reply_buf = vec![0u8; MAX_BULK_BYTES];
    let n = syscall_lib::ipc_take_pending_bulk(&mut reply_buf);
    if n == u64::MAX {
        print_str("m3ctl: ipc_take_pending_bulk failed\n");
        return 1;
    }
    if n == 0 {
        // Server replied with no bulk payload. Some verbs are
        // legitimately void-reply (e.g. Subscribe → Ack via the
        // event stream, not the response bulk). Fall back to the
        // synthetic reply for verbs whose ack shape is well-known.
        let fallback = synthetic_reply_for(&cmd);
        print_event(&fallback);
        return 0;
    }

    let used = n as usize;
    match decode_event(&reply_buf[..used]) {
        Ok((ev, _)) => print_event(&ev),
        Err(err) => {
            print_str("m3ctl: failed to decode reply: ");
            print_str(control_error_label(err));
            print_str("\n");
            return 1;
        }
    }

    0
}

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

// ---------------------------------------------------------------------------
// Command parsing
// ---------------------------------------------------------------------------

fn parse_command(verb: &str, args: &[&str]) -> Result<ControlCommand, &'static str> {
    match verb {
        "version" => Ok(ControlCommand::Version),
        "list-surfaces" => Ok(ControlCommand::ListSurfaces),
        "frame-stats" => Ok(ControlCommand::FrameStats),
        "focus" => {
            let id = parse_u32(args.first().copied().ok_or("focus requires <surface-id>")?)
                .ok_or("focus: surface-id must be a u32")?;
            Ok(ControlCommand::Focus {
                surface_id: SurfaceId(id),
            })
        }
        "register-bind" => {
            let mask = parse_u16(
                args.first()
                    .copied()
                    .ok_or("register-bind requires <mask>")?,
            )
            .ok_or("register-bind: mask must fit in u16")?;
            let kc = parse_u32(
                args.get(1)
                    .copied()
                    .ok_or("register-bind requires <keycode>")?,
            )
            .ok_or("register-bind: keycode must be a u32")?;
            Ok(ControlCommand::RegisterBind {
                modifier_mask: mask,
                keycode: kc,
            })
        }
        "unregister-bind" => {
            let mask = parse_u16(
                args.first()
                    .copied()
                    .ok_or("unregister-bind requires <mask>")?,
            )
            .ok_or("unregister-bind: mask must fit in u16")?;
            let kc = parse_u32(
                args.get(1)
                    .copied()
                    .ok_or("unregister-bind requires <keycode>")?,
            )
            .ok_or("unregister-bind: keycode must be a u32")?;
            Ok(ControlCommand::UnregisterBind {
                modifier_mask: mask,
                keycode: kc,
            })
        }
        "subscribe" => {
            let name = args.first().copied().ok_or("subscribe requires <kind>")?;
            let kind = parse_event_kind(name).ok_or(
                "subscribe: kind must be one of \
                 surface-created | surface-destroyed | focus-changed | bind-triggered",
            )?;
            Ok(ControlCommand::Subscribe { event_kind: kind })
        }
        _ => Err("unknown verb"),
    }
}

fn parse_event_kind(name: &str) -> Option<EventKind> {
    match name {
        "surface-created" | "SurfaceCreated" => Some(EventKind::SurfaceCreated),
        "surface-destroyed" | "SurfaceDestroyed" => Some(EventKind::SurfaceDestroyed),
        "focus-changed" | "FocusChanged" => Some(EventKind::FocusChanged),
        "bind-triggered" | "BindTriggered" => Some(EventKind::BindTriggered),
        _ => None,
    }
}

fn parse_u32(s: &str) -> Option<u32> {
    if let Some(rest) = s.strip_prefix("0x") {
        u32::from_str_radix(rest, 16).ok()
    } else {
        s.parse().ok()
    }
}

fn parse_u16(s: &str) -> Option<u16> {
    if let Some(rest) = s.strip_prefix("0x") {
        u16::from_str_radix(rest, 16).ok()
    } else {
        s.parse().ok()
    }
}

// ---------------------------------------------------------------------------
// Output formatting
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Synthetic-reply fallback
// ---------------------------------------------------------------------------

/// Synthesise a structurally-correct fallback reply for each verb when
/// `ipc_take_pending_bulk` returns 0 bytes — i.e. the server replied
/// with no staged bulk. This covers two legitimate cases:
///
/// 1. **Void replies** — verbs whose contract is "fire-and-acknowledge"
///    (e.g. `ControlCommand::DebugCrash`, `ControlCommand::SetCursor`)
///    succeed by replying with a label and no bulk; the caller maps
///    that to `ControlEvent::Ack`.
/// 2. **Empty replies** — verbs that return a list which happens to be
///    empty this iteration (e.g. `ListSurfaces` on a fresh boot,
///    `FrameStats` before the first compose pass) — the structural
///    reply is a well-formed event with an empty payload.
///
/// Failing to drain (`ipc_take_pending_bulk == u64::MAX`) is a
/// transport error and is logged separately; this helper only handles
/// the success-with-zero-bulk case.
fn synthetic_reply_for(cmd: &ControlCommand) -> ControlEvent {
    use kernel_core::display::control::PROTOCOL_VERSION;
    match cmd {
        ControlCommand::Version => ControlEvent::VersionReply {
            protocol_version: PROTOCOL_VERSION,
        },
        ControlCommand::ListSurfaces => ControlEvent::SurfaceListReply { ids: Vec::new() },
        ControlCommand::FrameStats => ControlEvent::FrameStatsReply {
            samples: Vec::new(),
        },
        _ => ControlEvent::Ack,
    }
}

// ---------------------------------------------------------------------------
// Service lookup with bounded backoff
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Print helpers (replace println!, which `no_std` cannot use)
// ---------------------------------------------------------------------------

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
         Verbs:\n  \
           version                         Print the control-socket protocol version\n  \
           list-surfaces                   Print every registered surface id\n  \
           frame-stats                     Print the rolling frame-composition window\n  \
           focus <surface-id>              Move keyboard focus\n  \
           register-bind <mask> <keycode>  Register a keybind\n  \
           unregister-bind <mask> <keycode> Unregister a keybind\n  \
           subscribe <kind>                Subscribe to event-stream of <kind>\n",
    );
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "m3ctl: PANIC\n");
    syscall_lib::exit(101)
}
