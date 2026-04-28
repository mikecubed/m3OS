//! Phase 57 Track F.5 — Control-socket verb codec for `session_manager`.
//!
//! `session_manager` (F.2 / F.4) supervises the graphical session;
//! F.5 exposes a small out-of-band control surface so `m3ctl` (and only
//! `m3ctl`) can query state and request graceful stop / restart without
//! booting a debugger or killing the daemon.
//!
//! # Verbs
//!
//! - [`ControlVerb::SessionState`]   — return the current
//!   [`crate::session::SessionState`]
//! - [`ControlVerb::SessionStop`]    — graceful shutdown that falls
//!   through to [`crate::session::SessionState::TextFallback`]
//! - [`ControlVerb::SessionRestart`] — graceful stop + start
//!
//! Per the Phase 57 F.5 task list:
//!
//! > Access control follows the Phase 56 m3ctl precedent: capability-
//! > based — the connecting peer must hold the `session_manager`
//! > control-socket cap, granted to `m3ctl` at session-manager startup
//! > and to no other process.
//!
//! [`ControlSocketCap`] is the value-typed token; its only constructor
//! [`ControlSocketCap::granted_for_m3ctl_only`] documents the policy.
//! [`dispatch_authenticated`] requires a `Some(&cap)` reference; an
//! anonymous caller sees [`SessionControlError::CapabilityMissing`].
//!
//! # Why a separate module from `session_supervisor`
//!
//! `session_supervisor` (F.3) is the **internal** verb surface
//! `session_manager` issues *to init* (start/stop/restart/await-ready/
//! on-exit-observed). `session_control` (F.5) is the **external** verb
//! surface `m3ctl` issues *to session_manager* (session-state /
//! session-stop / session-restart). Different actors, different cap,
//! different direction — separate modules even though both share the
//! "tag-prefixed bytes" wire shape. SOLID SRP and ISP.
//!
//! # No new syscall
//!
//! Per the F.5 task list (and the broader Phase 57 / Phase 56
//! capability-discipline): F.5 reuses init's existing IPC service
//! registry. `session_manager` registers the control endpoint under a
//! dedicated service name (`"session-control"` — see the `userspace/
//! session_manager/src/control.rs` consumer) and `m3ctl` looks it up
//! the same way it looks up `display-control`. The cap that gates the
//! control verbs is held locally by `m3ctl` after the boot-time grant;
//! the codec in this module does not encode the cap into the wire
//! payload.

#![allow(clippy::needless_lifetimes)] // explicit lifetimes document borrow

use crate::session::SessionState;

// ---------------------------------------------------------------------------
// Wire constants — versioned so future verbs can extend the codec
// without breaking deployed `m3ctl` instances.
// ---------------------------------------------------------------------------

/// Verb tags. Stable; reordering is a wire-incompatible change. The
/// integration test
/// `kernel-core/tests/phase57_f5_session_control.rs` latches these
/// byte values so a future reorder fails CI before deployment.
const TAG_VERB_SESSION_STATE: u8 = 0x01;
const TAG_VERB_SESSION_STOP: u8 = 0x02;
const TAG_VERB_SESSION_RESTART: u8 = 0x03;

/// Reply tags.
const TAG_REPLY_STATE: u8 = 0x01;
const TAG_REPLY_ACK: u8 = 0x02;
const TAG_REPLY_ERROR: u8 = 0x03;

/// Session-state discriminants used in the `State` reply payload.
const STATE_TAG_BOOTING: u8 = 0x01;
const STATE_TAG_RUNNING: u8 = 0x02;
const STATE_TAG_RECOVERING: u8 = 0x03;
const STATE_TAG_TEXT_FALLBACK: u8 = 0x04;

/// Error codes carried inside a [`ControlReply::Error`].
const ERR_CAPABILITY_MISSING: u8 = 0x01;
const ERR_MALFORMED_REQUEST: u8 = 0x02;
const ERR_INTERNAL: u8 = 0x03;

/// Maximum number of bytes in a serialized session-step name (used in
/// the `Recovering` payload). 32 mirrors the supervisor's
/// `MAX_SERVICE_NAME_BYTES` so the codec rejects values init cannot
/// observe.
pub const MAX_STEP_NAME_BYTES: usize = 32;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Verbs `m3ctl` may issue to `session_manager`'s control socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlVerb {
    /// Return the current [`SessionState`].
    SessionState,
    /// Graceful shutdown: stop every declared graphical service in
    /// reverse start order, release the framebuffer back to the kernel
    /// console, transition to [`SessionState::TextFallback`]. Same
    /// motion as the F.4 boot-time text-fallback escalation; the only
    /// difference is the trigger.
    SessionStop,
    /// Graceful stop + start: do the F.5 `SessionStop` motion, reset
    /// the recovery counters (so the new attempt sees a fresh retry
    /// budget per step), and re-drive the F.1 boot sequence.
    SessionRestart,
}

/// Replies from `session_manager` back to `m3ctl`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlReply {
    /// `SessionState` query reply.
    State { state: SessionState },
    /// `SessionStop` / `SessionRestart` succeeded.
    Ack,
    /// Verb rejected. The variant carries the typed error.
    Error(SessionControlError),
}

/// Typed error surface returned by the F.5 control-socket dispatcher.
/// No stringly-typed variants; callers can match every case.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionControlError {
    /// The dispatcher was invoked without a [`ControlSocketCap`].
    CapabilityMissing,
    /// The wire payload could not be parsed (truncated buffer,
    /// unknown tag, oversized step name in a `Recovering` reply).
    MalformedRequest,
    /// The backend reported an internal error executing the verb.
    /// Reserved for backends that fail the underlying motion (e.g. a
    /// supervisor error during stop). Phase 57 backends always return
    /// `Ok(())` from `session_stop` / `session_restart` — the
    /// rollback policy swallows individual stop errors per the
    /// F.4 motion — so this variant is currently unused but reserved
    /// for forward-compat without a wire-incompatible change.
    Internal,
}

/// Capability tag granted to `m3ctl` and to no other process. The only
/// constructor is named for the policy it enforces:
/// [`ControlSocketCap::granted_for_m3ctl_only`].
///
/// The cap is a value-type marker; possessing one demonstrates the
/// holder has been granted the F.5 control surface.
/// `session_manager` mints it during F.5 startup and (per the broader
/// Phase 56 / Phase 57 IPC-pivot transport) hands it only to `m3ctl`.
#[derive(Debug, Clone, Copy)]
pub struct ControlSocketCap {
    // Private field so external callers cannot construct.
    _granted: (),
}

impl ControlSocketCap {
    /// Mint a capability granted only to `m3ctl`. The name documents
    /// the policy; this is the sole constructor.
    pub const fn granted_for_m3ctl_only() -> Self {
        Self { _granted: () }
    }
}

// ---------------------------------------------------------------------------
// Backend trait — what the F.5 dispatcher needs the daemon to provide.
// ---------------------------------------------------------------------------

/// Adapter trait the F.5 dispatcher consumes. `session_manager`
/// implements it by reading the daemon's tracked `SessionState` and
/// invoking the F.4 rollback / boot-sequence drivers.
///
/// SOLID DI: the dispatcher depends on the trait, not on the daemon.
/// Tests substitute a recording backend so dispatch is host-testable.
pub trait SessionControlBackend {
    /// Return the daemon's current [`SessionState`]. Called for the
    /// `SessionState` verb.
    fn current_state(&mut self) -> SessionState;

    /// Initiate a graceful stop. Returns `Ok(())` on success or a
    /// typed error if the underlying motion failed in a
    /// surface-worthy way (no Phase 57 backend currently fails this).
    fn session_stop(&mut self) -> Result<(), SessionControlError>;

    /// Initiate a graceful restart. Returns `Ok(())` on success or a
    /// typed error.
    fn session_restart(&mut self) -> Result<(), SessionControlError>;
}

// ---------------------------------------------------------------------------
// Wire codec
// ---------------------------------------------------------------------------

/// Encode a verb into `dst`. Returns the number of bytes written.
///
/// Layout: a single tag byte. The verbs do not carry payload data;
/// future verbs that do extend the layout in a backwards-compatible way
/// (length prefix + variable bytes).
pub fn encode_verb(verb: &ControlVerb, dst: &mut [u8]) -> Result<usize, SessionControlError> {
    if dst.is_empty() {
        return Err(SessionControlError::MalformedRequest);
    }
    let tag = match verb {
        ControlVerb::SessionState => TAG_VERB_SESSION_STATE,
        ControlVerb::SessionStop => TAG_VERB_SESSION_STOP,
        ControlVerb::SessionRestart => TAG_VERB_SESSION_RESTART,
    };
    dst[0] = tag;
    Ok(1)
}

/// Decode a verb from `src`.
pub fn decode_verb(src: &[u8]) -> Result<ControlVerb, SessionControlError> {
    if src.is_empty() {
        return Err(SessionControlError::MalformedRequest);
    }
    match src[0] {
        TAG_VERB_SESSION_STATE => Ok(ControlVerb::SessionState),
        TAG_VERB_SESSION_STOP => Ok(ControlVerb::SessionStop),
        TAG_VERB_SESSION_RESTART => Ok(ControlVerb::SessionRestart),
        _ => Err(SessionControlError::MalformedRequest),
    }
}

/// Encode a reply into `dst`. Returns bytes written.
///
/// `State { Recovering { step_name, retry_count } }` carries the step
/// name length (1 byte) + name bytes (up to [`MAX_STEP_NAME_BYTES`]) +
/// retry count (4 bytes LE u32). Other state variants use a fixed
/// 2-byte payload (tag + state-tag).
pub fn encode_reply(reply: &ControlReply, dst: &mut [u8]) -> Result<usize, SessionControlError> {
    if dst.is_empty() {
        return Err(SessionControlError::MalformedRequest);
    }
    match reply {
        ControlReply::State { state } => {
            if dst.len() < 2 {
                return Err(SessionControlError::MalformedRequest);
            }
            dst[0] = TAG_REPLY_STATE;
            match state {
                SessionState::Booting => {
                    dst[1] = STATE_TAG_BOOTING;
                    Ok(2)
                }
                SessionState::Running => {
                    dst[1] = STATE_TAG_RUNNING;
                    Ok(2)
                }
                SessionState::TextFallback => {
                    dst[1] = STATE_TAG_TEXT_FALLBACK;
                    Ok(2)
                }
                SessionState::Recovering {
                    step_name,
                    retry_count,
                } => {
                    let name_bytes = step_name.as_bytes();
                    if name_bytes.len() > MAX_STEP_NAME_BYTES {
                        return Err(SessionControlError::MalformedRequest);
                    }
                    let total = 2 + 1 + name_bytes.len() + 4;
                    if dst.len() < total {
                        return Err(SessionControlError::MalformedRequest);
                    }
                    dst[1] = STATE_TAG_RECOVERING;
                    // Cast safe: bound check above caps `name_bytes.len()`
                    // at `MAX_STEP_NAME_BYTES` (32).
                    dst[2] = name_bytes.len() as u8;
                    dst[3..3 + name_bytes.len()].copy_from_slice(name_bytes);
                    let off = 3 + name_bytes.len();
                    dst[off..off + 4].copy_from_slice(&retry_count.to_le_bytes());
                    Ok(total)
                }
            }
        }
        ControlReply::Ack => {
            dst[0] = TAG_REPLY_ACK;
            Ok(1)
        }
        ControlReply::Error(err) => {
            if dst.len() < 2 {
                return Err(SessionControlError::MalformedRequest);
            }
            dst[0] = TAG_REPLY_ERROR;
            dst[1] = session_control_error_to_byte(*err);
            Ok(2)
        }
    }
}

/// Decode a reply from `src`. Note: the `Recovering` step-name borrow
/// requires a `'static` lifetime in the `SessionState` payload, which
/// the wire decoder cannot supply because the bytes live in the source
/// buffer — the decoder maps a recovering reply onto a single fixed
/// `&'static "<recovering>"` placeholder. This matches the codec's
/// purpose: the wire format is for control-flow signaling, not for
/// transmitting the step-name string back to the operator. F.5 defers
/// "recovering with full step-name fidelity over the wire" to a later
/// memo if the operator UX requires it.
pub fn decode_reply(src: &[u8]) -> Result<ControlReply, SessionControlError> {
    if src.is_empty() {
        return Err(SessionControlError::MalformedRequest);
    }
    match src[0] {
        TAG_REPLY_STATE => {
            if src.len() < 2 {
                return Err(SessionControlError::MalformedRequest);
            }
            let state = match src[1] {
                STATE_TAG_BOOTING => SessionState::Booting,
                STATE_TAG_RUNNING => SessionState::Running,
                STATE_TAG_TEXT_FALLBACK => SessionState::TextFallback,
                STATE_TAG_RECOVERING => {
                    if src.len() < 3 {
                        return Err(SessionControlError::MalformedRequest);
                    }
                    let name_len = src[2] as usize;
                    if name_len > MAX_STEP_NAME_BYTES {
                        return Err(SessionControlError::MalformedRequest);
                    }
                    let off_name_end = 3 + name_len;
                    if src.len() < off_name_end + 4 {
                        return Err(SessionControlError::MalformedRequest);
                    }
                    // The wire bytes can encode the name but we cannot
                    // borrow them as `&'static str` (the decoder cannot
                    // promote source bytes to a static lifetime). The
                    // SessionState requires `&'static`, so a future
                    // codec extension that wants full fidelity must
                    // wrap `SessionState` in a non-static "wire" type;
                    // for F.5 we map every recovering-reply onto the
                    // fixed placeholder name and preserve the retry
                    // count.
                    let arr: [u8; 4] = [
                        src[off_name_end],
                        src[off_name_end + 1],
                        src[off_name_end + 2],
                        src[off_name_end + 3],
                    ];
                    let retry_count = u32::from_le_bytes(arr);
                    SessionState::Recovering {
                        step_name: WIRE_RECOVERING_STEP_NAME,
                        retry_count,
                    }
                }
                _ => return Err(SessionControlError::MalformedRequest),
            };
            Ok(ControlReply::State { state })
        }
        TAG_REPLY_ACK => Ok(ControlReply::Ack),
        TAG_REPLY_ERROR => {
            if src.len() < 2 {
                return Err(SessionControlError::MalformedRequest);
            }
            let err = byte_to_session_control_error(src[1])?;
            Ok(ControlReply::Error(err))
        }
        _ => Err(SessionControlError::MalformedRequest),
    }
}

/// Placeholder step-name used by `decode_reply` when reconstructing a
/// `Recovering` state. See `decode_reply`'s doc comment for the
/// rationale.
const WIRE_RECOVERING_STEP_NAME: &str = "<recovering>";

fn session_control_error_to_byte(err: SessionControlError) -> u8 {
    match err {
        SessionControlError::CapabilityMissing => ERR_CAPABILITY_MISSING,
        SessionControlError::MalformedRequest => ERR_MALFORMED_REQUEST,
        SessionControlError::Internal => ERR_INTERNAL,
    }
}

fn byte_to_session_control_error(b: u8) -> Result<SessionControlError, SessionControlError> {
    match b {
        ERR_CAPABILITY_MISSING => Ok(SessionControlError::CapabilityMissing),
        ERR_MALFORMED_REQUEST => Ok(SessionControlError::MalformedRequest),
        ERR_INTERNAL => Ok(SessionControlError::Internal),
        _ => Err(SessionControlError::MalformedRequest),
    }
}

// ---------------------------------------------------------------------------
// Authenticated dispatcher
// ---------------------------------------------------------------------------

/// Decode `request_bytes`, authorize via `cap`, and dispatch to
/// `backend`. Returns the typed reply.
///
/// Authorization gate:
/// - `cap == None` → [`SessionControlError::CapabilityMissing`];
///   `backend` is not invoked.
/// - `cap == Some(_)` → the request is decoded and forwarded.
///
/// Decoding errors surface as [`SessionControlError::MalformedRequest`]
/// without invoking `backend`. Backend errors surface as
/// [`ControlReply::Error(...)`].
pub fn dispatch_authenticated<B: SessionControlBackend>(
    request_bytes: &[u8],
    cap: Option<&ControlSocketCap>,
    backend: &mut B,
) -> Result<ControlReply, SessionControlError> {
    if cap.is_none() {
        return Err(SessionControlError::CapabilityMissing);
    }
    let verb = decode_verb(request_bytes)?;
    match verb {
        ControlVerb::SessionState => {
            let state = backend.current_state();
            Ok(ControlReply::State { state })
        }
        ControlVerb::SessionStop => match backend.session_stop() {
            Ok(()) => Ok(ControlReply::Ack),
            Err(e) => Ok(ControlReply::Error(e)),
        },
        ControlVerb::SessionRestart => match backend.session_restart() {
            Ok(()) => Ok(ControlReply::Ack),
            Err(e) => Ok(ControlReply::Error(e)),
        },
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Round-trip every error variant through the byte mapping.
    #[test]
    fn every_error_round_trips_through_byte_mapping() {
        let errors = [
            SessionControlError::CapabilityMissing,
            SessionControlError::MalformedRequest,
            SessionControlError::Internal,
        ];
        for err in errors {
            let b = session_control_error_to_byte(err);
            let back = byte_to_session_control_error(b).expect("known byte decodes");
            assert_eq!(back, err);
        }
    }

    #[test]
    fn unknown_error_byte_returns_malformed_request() {
        let result = byte_to_session_control_error(0xFF);
        assert!(matches!(result, Err(SessionControlError::MalformedRequest)));
    }

    #[test]
    fn state_reply_for_booting_round_trips() {
        let reply = ControlReply::State {
            state: SessionState::Booting,
        };
        let mut buf = [0u8; 16];
        let len = encode_reply(&reply, &mut buf).expect("encode");
        let decoded = decode_reply(&buf[..len]).expect("decode");
        assert_eq!(decoded, reply);
    }

    #[test]
    fn recovering_reply_preserves_retry_count_but_replaces_step_name() {
        // The wire format cannot promote source bytes to `&'static
        // str`; the codec replaces the step name with a fixed
        // placeholder on decode while preserving the retry count.
        let reply = ControlReply::State {
            state: SessionState::Recovering {
                step_name: "audio_server",
                retry_count: 7,
            },
        };
        let mut buf = [0u8; 64];
        let len = encode_reply(&reply, &mut buf).expect("encode");
        let decoded = decode_reply(&buf[..len]).expect("decode");
        match decoded {
            ControlReply::State {
                state:
                    SessionState::Recovering {
                        step_name,
                        retry_count,
                    },
            } => {
                assert_eq!(step_name, WIRE_RECOVERING_STEP_NAME);
                assert_eq!(retry_count, 7);
            }
            other => panic!("expected Recovering, got {:?}", other),
        }
    }

    #[test]
    fn encode_reply_rejects_oversized_step_name() {
        // Synthesize a SessionState::Recovering with a step_name longer
        // than MAX_STEP_NAME_BYTES. We can't actually construct a
        // `&'static str` longer than 32 bytes from a literal in this
        // file, but we can use a `'static` slice of a leaked Box.
        // Avoid Box::leak in the test (kernel-core tests run on host
        // with `std`, but leaks are still ugly). Instead: build a
        // string at compile time.
        let big: &'static str =
            "this_is_a_very_long_step_name_that_exceeds_the_maximum_thirty_two_bytes";
        let reply = ControlReply::State {
            state: SessionState::Recovering {
                step_name: big,
                retry_count: 0,
            },
        };
        let mut buf = [0u8; 256];
        let result = encode_reply(&reply, &mut buf);
        assert!(matches!(result, Err(SessionControlError::MalformedRequest)));
    }

    #[test]
    fn decode_reply_rejects_oversized_name_length_field() {
        // [TAG_REPLY_STATE, STATE_TAG_RECOVERING, 0xFF, ...] — name
        // length 0xFF > MAX_STEP_NAME_BYTES = 32.
        let bad = [TAG_REPLY_STATE, STATE_TAG_RECOVERING, 0xFF, 0, 0, 0, 0];
        let result = decode_reply(&bad);
        assert!(matches!(result, Err(SessionControlError::MalformedRequest)));
    }
}
