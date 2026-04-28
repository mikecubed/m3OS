//! `m3ctl` — Phase 56 / 57 control-socket client library.
//!
//! The binary is a thin shell that parses argv, dispatches via
//! [`parse_verb`], and prints the reply. Every parser path that does
//! not depend on `_start` lives here so it is host-testable via
//! `cargo test -p m3ctl --target x86_64-unknown-linux-gnu`.
//!
//! # Phase 57 Track I.2 — session control verbs
//!
//! Phase 57 closes the F.5 → I.2 client-side deferral by adding three
//! new verbs that reach the `session_manager` daemon's control socket
//! at `/run/m3os/session.sock`:
//!
//! - `m3ctl session-state`   — returns the current
//!   [`kernel_core::session::SessionState`] as a printable string.
//! - `m3ctl session-stop`    — graceful shutdown (falls through to
//!   `text-fallback`).
//! - `m3ctl session-restart` — graceful stop + start.
//!
//! All three reuse [`kernel_core::session_control`] for the codec — no
//! parallel byte definitions live in this crate (DRY).
//!
//! # Capability gate
//!
//! Per the Phase 57 F.5 design, the session control surface is gated
//! by [`kernel_core::session_control::ControlSocketCap`], minted at
//! `session_manager` startup and granted only to `m3ctl`. The binary
//! presents the cap implicitly — possession of the cap is the gate;
//! the parser surfaced here is cap-agnostic so it stays
//! host-testable.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::string::String;
use kernel_core::display::control::{ControlCommand, EventKind, SurfaceId};
use kernel_core::session_control::ControlVerb;

// ---------------------------------------------------------------------------
// Service-registry names + IPC labels — must match the daemons.
// ---------------------------------------------------------------------------

/// Service-registry name of the `display_server` control endpoint.
pub const DISPLAY_CONTROL_SERVICE_NAME: &str = "display-control";

/// Service-registry name of the `session_manager` control endpoint
/// (Phase 57 F.5). Mirrors
/// `userspace::session_manager::control::CONTROL_SERVICE_NAME`.
pub const SESSION_CONTROL_SERVICE_NAME: &str = "session-control";

/// IPC label for an encoded `display_server::control::ControlCommand`.
/// Mirrors `display_server::control::LABEL_CTL_CMD`.
pub const LABEL_DISPLAY_CTL_CMD: u64 = 1;

/// IPC label for an encoded
/// `kernel_core::session_control::ControlVerb`. Mirrors
/// `userspace::session_manager::control::LABEL_CTL_CMD`.
pub const LABEL_SESSION_CTL_CMD: u64 = 1;

// ---------------------------------------------------------------------------
// Parsed verb — unified across display and session targets.
// ---------------------------------------------------------------------------

/// Parsed CLI verb. Each variant carries the typed payload the binary
/// dispatcher needs to emit the correct IPC `call`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedVerb {
    /// A Phase 56 `display-control` verb. The binary looks up the
    /// `display-control` service, encodes via
    /// `kernel_core::display::control::encode_command`, and parses the
    /// reply via `decode_event`.
    Display(ControlCommand),
    /// A Phase 57 `session-control` verb. The binary looks up the
    /// `session-control` service, encodes via
    /// `kernel_core::session_control::encode_verb`, and parses the
    /// reply via `decode_reply`.
    Session(ControlVerb),
}

/// Parser-level error. Variants are *data*; callers `match` to surface
/// the right human-readable diagnostic. `String` is the parser's
/// surface-level error message — the binary prints it then exits with
/// code 2 (usage).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// The verb name was not recognized (e.g., a typo). Carries the
    /// offending string so the diagnostic can echo it back.
    UnknownVerb(String),
    /// A required argument was missing.
    MissingArgument(&'static str),
    /// An argument failed to parse (e.g., not a u32).
    BadArgument(&'static str),
    /// Unknown event-kind name to `subscribe`.
    UnknownEventKind(String),
}

// ---------------------------------------------------------------------------
// Top-level parser
// ---------------------------------------------------------------------------

/// Parse `verb` + `args` into a typed [`ParsedVerb`].
///
/// Phase 57 I.2 adds the three `session-*` verbs alongside the existing
/// Phase 56 `display-control` verbs. The session arms produce
/// [`ParsedVerb::Session`] payloads carrying the matching
/// [`ControlVerb`] discriminant; the binary's session dispatcher
/// encodes via [`kernel_core::session_control::encode_verb`].
pub fn parse_verb(verb: &str, args: &[&str]) -> Result<ParsedVerb, ParseError> {
    match verb {
        // Phase 57 I.2 — session control verbs. The three verbs carry
        // no arguments; extra args are tolerated (the parser is
        // permissive — argument parsing is per-verb).
        "session-state" => Ok(ParsedVerb::Session(ControlVerb::SessionState)),
        "session-stop" => Ok(ParsedVerb::Session(ControlVerb::SessionStop)),
        "session-restart" => Ok(ParsedVerb::Session(ControlVerb::SessionRestart)),
        // Phase 56 — display control verbs.
        "version" => Ok(ParsedVerb::Display(ControlCommand::Version)),
        "list-surfaces" => Ok(ParsedVerb::Display(ControlCommand::ListSurfaces)),
        "frame-stats" => Ok(ParsedVerb::Display(ControlCommand::FrameStats)),
        "focus" => {
            let id_str = args
                .first()
                .copied()
                .ok_or(ParseError::MissingArgument("focus requires <surface-id>"))?;
            let id = parse_u32(id_str)
                .ok_or(ParseError::BadArgument("focus: surface-id must be a u32"))?;
            Ok(ParsedVerb::Display(ControlCommand::Focus {
                surface_id: SurfaceId(id),
            }))
        }
        "register-bind" => {
            let mask_str = args
                .first()
                .copied()
                .ok_or(ParseError::MissingArgument("register-bind requires <mask>"))?;
            let mask = parse_u16(mask_str).ok_or(ParseError::BadArgument(
                "register-bind: mask must fit in u16",
            ))?;
            let kc_str = args.get(1).copied().ok_or(ParseError::MissingArgument(
                "register-bind requires <keycode>",
            ))?;
            let kc = parse_u32(kc_str).ok_or(ParseError::BadArgument(
                "register-bind: keycode must be a u32",
            ))?;
            Ok(ParsedVerb::Display(ControlCommand::RegisterBind {
                modifier_mask: mask,
                keycode: kc,
            }))
        }
        "unregister-bind" => {
            let mask_str = args.first().copied().ok_or(ParseError::MissingArgument(
                "unregister-bind requires <mask>",
            ))?;
            let mask = parse_u16(mask_str).ok_or(ParseError::BadArgument(
                "unregister-bind: mask must fit in u16",
            ))?;
            let kc_str = args.get(1).copied().ok_or(ParseError::MissingArgument(
                "unregister-bind requires <keycode>",
            ))?;
            let kc = parse_u32(kc_str).ok_or(ParseError::BadArgument(
                "unregister-bind: keycode must be a u32",
            ))?;
            Ok(ParsedVerb::Display(ControlCommand::UnregisterBind {
                modifier_mask: mask,
                keycode: kc,
            }))
        }
        "subscribe" => {
            let name = args
                .first()
                .copied()
                .ok_or(ParseError::MissingArgument("subscribe requires <kind>"))?;
            let kind = parse_event_kind(name)
                .ok_or_else(|| ParseError::UnknownEventKind(String::from(name)))?;
            Ok(ParsedVerb::Display(ControlCommand::Subscribe {
                event_kind: kind,
            }))
        }
        other => Err(ParseError::UnknownVerb(String::from(other))),
    }
}

/// Map a subscribe `<kind>` argument to the typed [`EventKind`].
pub fn parse_event_kind(name: &str) -> Option<EventKind> {
    match name {
        "surface-created" | "SurfaceCreated" => Some(EventKind::SurfaceCreated),
        "surface-destroyed" | "SurfaceDestroyed" => Some(EventKind::SurfaceDestroyed),
        "focus-changed" | "FocusChanged" => Some(EventKind::FocusChanged),
        "bind-triggered" | "BindTriggered" => Some(EventKind::BindTriggered),
        _ => None,
    }
}

/// Parse a `u32` written either as decimal or with a `0x` hex prefix.
pub fn parse_u32(s: &str) -> Option<u32> {
    if let Some(rest) = s.strip_prefix("0x") {
        u32::from_str_radix(rest, 16).ok()
    } else {
        s.parse().ok()
    }
}

/// Parse a `u16` written either as decimal or with a `0x` hex prefix.
pub fn parse_u16(s: &str) -> Option<u16> {
    if let Some(rest) = s.strip_prefix("0x") {
        u16::from_str_radix(rest, 16).ok()
    } else {
        s.parse().ok()
    }
}

#[cfg(test)]
mod tests;
