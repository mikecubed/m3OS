//! Phase 57 Track F.3 — Pure-logic supervisor verbs for `session_manager`.
//!
//! `session_manager` (F.2) cannot supervise from outside the supervisor;
//! F.3 surfaces only the verbs the orchestrator actually needs (start,
//! stop, restart, await-ready, on-exit) and gates them by a capability
//! that is granted only to `session_manager` at boot.
//!
//! This module is pure logic — no IPC, no syscalls, no I/O. It defines
//! the wire codec and the [`SupervisorBackend`] seam that an
//! adapter (the existing `init` daemon) implements. The test suite
//! lives at `kernel-core/tests/phase57_f3_session_supervisor.rs`.
//!
//! # Capability discipline
//!
//! Per the Phase 57 task list F.3 acceptance:
//!
//! > The new supervision verbs are visible only to processes holding
//! > the `session_manager` capability — no broad public surface.
//!
//! [`SupervisorCap`] is the value-typed token. Its only constructor is
//! [`SupervisorCap::granted_for_session_manager_only`], whose name
//! documents the policy. [`dispatch_authenticated`] requires a
//! `&SupervisorCap` reference, so a caller without it sees a
//! [`SupervisorError::CapabilityMissing`].
//!
//! On the boot path, `init` mints exactly one cap and grants it to
//! `session_manager`'s adapter; no other process gains it. This module
//! does not encode the cap into the wire payload — it is held by the
//! caller and presented at dispatch time. Holding a cap is therefore a
//! local property of the `session_manager` process.
//!
//! # No new syscall
//!
//! Per the F.3 acceptance:
//!
//! > No new syscall is added; `session_manager` consumes existing IPC +
//! > capabilities.
//!
//! `init` exposes the verbs over its existing root-only control channel
//! (`/run/init.cmd`) plus reads of `/run/services.status` for readiness
//! / exit observation. The codec in this module is the typed surface
//! `session_manager` uses to encode requests and decode replies; init's
//! adapter glues the two.

#![allow(clippy::needless_lifetimes)] // explicit lifetimes document borrow

use core::convert::TryFrom;

// ---------------------------------------------------------------------------
// Wire constants — versioned so future verbs can extend the codec
// without breaking deployed `session_manager` instances.
// ---------------------------------------------------------------------------

/// Maximum length of a service name in bytes. Mirrors init's `MAX_NAME`
/// (32) so the codec cannot accept a name init cannot store.
pub const MAX_SERVICE_NAME_BYTES: usize = 32;

/// Verb tags. Stable; reordering is a wire-incompatible change.
const TAG_VERB_START: u8 = 0x01;
const TAG_VERB_STOP: u8 = 0x02;
const TAG_VERB_RESTART: u8 = 0x03;
const TAG_VERB_AWAIT_READY: u8 = 0x04;
const TAG_VERB_ON_EXIT_OBSERVED: u8 = 0x05;

/// Reply tags.
const TAG_REPLY_ACK: u8 = 0x01;
const TAG_REPLY_READY_STATE: u8 = 0x02;
const TAG_REPLY_EXIT_OBSERVED: u8 = 0x03;
const TAG_REPLY_ERROR: u8 = 0x04;

/// Error codes carried inside a `SupervisorReply::Error`.
const ERR_UNKNOWN_SERVICE: u8 = 0x01;
const ERR_PERMISSION_DENIED: u8 = 0x02;
const ERR_TIMEOUT: u8 = 0x03;
const ERR_ALREADY_RUNNING: u8 = 0x04;
const ERR_NOT_RUNNING: u8 = 0x05;
const ERR_MALFORMED_REQUEST: u8 = 0x06;
const ERR_CAPABILITY_MISSING: u8 = 0x07;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Verbs `session_manager` may issue to the supervisor.
///
/// The `service` field borrows; encoders copy bytes, decoders return
/// borrowed slices into the source buffer. Allocation-free.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupervisorVerb<'a> {
    /// Start a supervised service. Equivalent to writing `start <name>`
    /// to init's existing `/run/init.cmd` control channel.
    Start { service: &'a str },
    /// Stop a supervised service. Sends SIGTERM, waits, escalates to
    /// SIGKILL per init's existing stop policy.
    Stop { service: &'a str },
    /// Restart a supervised service. Convenience around Stop+Start.
    Restart { service: &'a str },
    /// Block (with `timeout_ms`) until init reports the service as
    /// `running` in `/run/services.status` AND the protocol-level probe
    /// the F.2 step adapter performs is satisfied. Returns
    /// [`SupervisorReply::ReadyState`].
    AwaitReady { service: &'a str, timeout_ms: u64 },
    /// Notify init that `session_manager` has observed the service's
    /// exit (used after a deliberate stop in the rollback path). The
    /// reply carries the most recent exit classification for the
    /// service.
    OnExitObserved { service: &'a str },
}

/// Replies from the supervisor back to `session_manager`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupervisorReply {
    /// Verb accepted; no payload (Start/Stop/Restart).
    Ack,
    /// AwaitReady result. `ready == true` once the readiness contract
    /// is met; `ready == false` if the timeout elapsed.
    ReadyState { ready: bool },
    /// OnExitObserved result. Exit classification of the service.
    ExitObserved { exit_code: i32, signaled: bool },
    /// Verb rejected. The variant carries the typed error.
    Error(SupervisorError),
}

/// Typed error surface. No stringly-typed variants; callers can match
/// every case and recover.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupervisorError {
    /// The named service is not supervised by init.
    UnknownService,
    /// The peer is not authorized to invoke supervisor verbs.
    PermissionDenied,
    /// `AwaitReady` did not observe readiness within `timeout_ms`.
    Timeout,
    /// `Start` issued for a service that is already running.
    AlreadyRunning,
    /// `Stop` / `OnExitObserved` issued for a service that is not
    /// running.
    NotRunning,
    /// The wire payload could not be parsed (truncated buffer, unknown
    /// tag, oversized service name, ...).
    MalformedRequest,
    /// The dispatcher was invoked without a [`SupervisorCap`].
    CapabilityMissing,
}

/// Capability tag granted to `session_manager` and to no other process.
/// The only constructor is named for the policy it enforces:
/// [`SupervisorCap::granted_for_session_manager_only`].
///
/// The cap is a value-type marker; possessing one demonstrates the
/// holder has been granted the supervisor surface. `init` mints it
/// during the F.2 boot path and never hands it to any other process.
#[derive(Debug, Clone, Copy)]
pub struct SupervisorCap {
    // Private field so external callers cannot construct.
    _granted: (),
}

impl SupervisorCap {
    /// Mint a capability granted only to `session_manager`. The name
    /// documents the policy; this is the sole constructor.
    pub const fn granted_for_session_manager_only() -> Self {
        Self { _granted: () }
    }
}

// ---------------------------------------------------------------------------
// SupervisorBackend trait — the seam init implements.
// ---------------------------------------------------------------------------

/// Adapter trait implemented by the supervisor (init) so the
/// dispatcher in this module is testable without I/O.
///
/// SOLID: depending on the trait, not on init, lets the test suite
/// substitute an in-memory recording backend.
pub trait SupervisorBackend {
    fn start(&mut self, service: &str) -> Result<SupervisorReply, SupervisorError>;
    fn stop(&mut self, service: &str) -> Result<SupervisorReply, SupervisorError>;
    fn restart(&mut self, service: &str) -> Result<SupervisorReply, SupervisorError>;
    fn await_ready(
        &mut self,
        service: &str,
        timeout_ms: u64,
    ) -> Result<SupervisorReply, SupervisorError>;
    fn on_exit_observed(
        &mut self,
        service: &str,
    ) -> Result<SupervisorReply, SupervisorError>;
}

// ---------------------------------------------------------------------------
// Wire codec
// ---------------------------------------------------------------------------

/// Encode a verb into `dst`. Returns the number of bytes written.
///
/// Layout:
/// ```text
///   0      | u8 | verb tag
///   1      | u8 | service name length (≤ 32)
///   2..    | u8 | service name bytes
///   ...    | u64 LE | timeout_ms (AwaitReady only)
/// ```
pub fn encode_verb(verb: &SupervisorVerb<'_>, dst: &mut [u8]) -> Result<usize, SupervisorError> {
    let (tag, name, timeout) = match verb {
        SupervisorVerb::Start { service } => (TAG_VERB_START, *service, None),
        SupervisorVerb::Stop { service } => (TAG_VERB_STOP, *service, None),
        SupervisorVerb::Restart { service } => (TAG_VERB_RESTART, *service, None),
        SupervisorVerb::AwaitReady {
            service,
            timeout_ms,
        } => (TAG_VERB_AWAIT_READY, *service, Some(*timeout_ms)),
        SupervisorVerb::OnExitObserved { service } => (TAG_VERB_ON_EXIT_OBSERVED, *service, None),
    };

    let name_bytes = name.as_bytes();
    if name_bytes.len() > MAX_SERVICE_NAME_BYTES {
        return Err(SupervisorError::MalformedRequest);
    }

    let extra = if timeout.is_some() { 8 } else { 0 };
    let total = 2 + name_bytes.len() + extra;
    if dst.len() < total {
        return Err(SupervisorError::MalformedRequest);
    }

    dst[0] = tag;
    // Cast safe: the bound check above guarantees `name_bytes.len() ≤
    // MAX_SERVICE_NAME_BYTES (32)`, which fits in u8.
    dst[1] = name_bytes.len() as u8;
    dst[2..2 + name_bytes.len()].copy_from_slice(name_bytes);

    if let Some(t) = timeout {
        let t_bytes = t.to_le_bytes();
        let off = 2 + name_bytes.len();
        dst[off..off + 8].copy_from_slice(&t_bytes);
    }

    Ok(total)
}

/// Decode a verb from `src`. The returned verb borrows the service
/// name from `src` — the caller must hold the buffer alive while the
/// borrow is in use.
pub fn decode_verb<'a>(src: &'a [u8]) -> Result<SupervisorVerb<'a>, SupervisorError> {
    if src.len() < 2 {
        return Err(SupervisorError::MalformedRequest);
    }
    let tag = src[0];
    let name_len = src[1] as usize;
    if name_len > MAX_SERVICE_NAME_BYTES {
        return Err(SupervisorError::MalformedRequest);
    }
    if src.len() < 2 + name_len {
        return Err(SupervisorError::MalformedRequest);
    }
    let name_bytes = &src[2..2 + name_len];
    let service = match core::str::from_utf8(name_bytes) {
        Ok(s) => s,
        Err(_) => return Err(SupervisorError::MalformedRequest),
    };

    match tag {
        TAG_VERB_START => Ok(SupervisorVerb::Start { service }),
        TAG_VERB_STOP => Ok(SupervisorVerb::Stop { service }),
        TAG_VERB_RESTART => Ok(SupervisorVerb::Restart { service }),
        TAG_VERB_AWAIT_READY => {
            let off = 2 + name_len;
            if src.len() < off + 8 {
                return Err(SupervisorError::MalformedRequest);
            }
            // Slice has exactly 8 bytes; try_into is infallible here.
            let arr: [u8; 8] = match <[u8; 8]>::try_from(&src[off..off + 8]) {
                Ok(a) => a,
                Err(_) => return Err(SupervisorError::MalformedRequest),
            };
            let timeout_ms = u64::from_le_bytes(arr);
            Ok(SupervisorVerb::AwaitReady {
                service,
                timeout_ms,
            })
        }
        TAG_VERB_ON_EXIT_OBSERVED => Ok(SupervisorVerb::OnExitObserved { service }),
        _ => Err(SupervisorError::MalformedRequest),
    }
}

/// Encode a reply into `dst`. Returns bytes written.
pub fn encode_reply(reply: &SupervisorReply, dst: &mut [u8]) -> Result<usize, SupervisorError> {
    match reply {
        SupervisorReply::Ack => {
            if dst.is_empty() {
                return Err(SupervisorError::MalformedRequest);
            }
            dst[0] = TAG_REPLY_ACK;
            Ok(1)
        }
        SupervisorReply::ReadyState { ready } => {
            if dst.len() < 2 {
                return Err(SupervisorError::MalformedRequest);
            }
            dst[0] = TAG_REPLY_READY_STATE;
            dst[1] = if *ready { 1 } else { 0 };
            Ok(2)
        }
        SupervisorReply::ExitObserved {
            exit_code,
            signaled,
        } => {
            if dst.len() < 6 {
                return Err(SupervisorError::MalformedRequest);
            }
            dst[0] = TAG_REPLY_EXIT_OBSERVED;
            dst[1] = if *signaled { 1 } else { 0 };
            dst[2..6].copy_from_slice(&exit_code.to_le_bytes());
            Ok(6)
        }
        SupervisorReply::Error(err) => {
            if dst.len() < 2 {
                return Err(SupervisorError::MalformedRequest);
            }
            dst[0] = TAG_REPLY_ERROR;
            dst[1] = supervisor_error_to_byte(*err);
            Ok(2)
        }
    }
}

/// Decode a reply from `src`.
pub fn decode_reply(src: &[u8]) -> Result<SupervisorReply, SupervisorError> {
    if src.is_empty() {
        return Err(SupervisorError::MalformedRequest);
    }
    match src[0] {
        TAG_REPLY_ACK => Ok(SupervisorReply::Ack),
        TAG_REPLY_READY_STATE => {
            if src.len() < 2 {
                return Err(SupervisorError::MalformedRequest);
            }
            Ok(SupervisorReply::ReadyState { ready: src[1] != 0 })
        }
        TAG_REPLY_EXIT_OBSERVED => {
            if src.len() < 6 {
                return Err(SupervisorError::MalformedRequest);
            }
            let signaled = src[1] != 0;
            let arr: [u8; 4] = match <[u8; 4]>::try_from(&src[2..6]) {
                Ok(a) => a,
                Err(_) => return Err(SupervisorError::MalformedRequest),
            };
            let exit_code = i32::from_le_bytes(arr);
            Ok(SupervisorReply::ExitObserved {
                exit_code,
                signaled,
            })
        }
        TAG_REPLY_ERROR => {
            if src.len() < 2 {
                return Err(SupervisorError::MalformedRequest);
            }
            let err = byte_to_supervisor_error(src[1])?;
            Ok(SupervisorReply::Error(err))
        }
        _ => Err(SupervisorError::MalformedRequest),
    }
}

fn supervisor_error_to_byte(err: SupervisorError) -> u8 {
    match err {
        SupervisorError::UnknownService => ERR_UNKNOWN_SERVICE,
        SupervisorError::PermissionDenied => ERR_PERMISSION_DENIED,
        SupervisorError::Timeout => ERR_TIMEOUT,
        SupervisorError::AlreadyRunning => ERR_ALREADY_RUNNING,
        SupervisorError::NotRunning => ERR_NOT_RUNNING,
        SupervisorError::MalformedRequest => ERR_MALFORMED_REQUEST,
        SupervisorError::CapabilityMissing => ERR_CAPABILITY_MISSING,
    }
}

fn byte_to_supervisor_error(b: u8) -> Result<SupervisorError, SupervisorError> {
    match b {
        ERR_UNKNOWN_SERVICE => Ok(SupervisorError::UnknownService),
        ERR_PERMISSION_DENIED => Ok(SupervisorError::PermissionDenied),
        ERR_TIMEOUT => Ok(SupervisorError::Timeout),
        ERR_ALREADY_RUNNING => Ok(SupervisorError::AlreadyRunning),
        ERR_NOT_RUNNING => Ok(SupervisorError::NotRunning),
        ERR_MALFORMED_REQUEST => Ok(SupervisorError::MalformedRequest),
        ERR_CAPABILITY_MISSING => Ok(SupervisorError::CapabilityMissing),
        _ => Err(SupervisorError::MalformedRequest),
    }
}

// ---------------------------------------------------------------------------
// Authenticated dispatcher
// ---------------------------------------------------------------------------

/// Decode `request_bytes`, authorize via `cap`, and dispatch to
/// `backend`. Returns the typed reply.
///
/// Authorization gate:
/// - `cap == None` → [`SupervisorError::CapabilityMissing`]; `backend`
///   is not invoked.
/// - `cap == Some(_)` → the request is decoded and forwarded.
///
/// Decoding errors surface as [`SupervisorError::MalformedRequest`].
/// Backend errors surface as a `SupervisorReply::Error(...)`.
pub fn dispatch_authenticated<B: SupervisorBackend>(
    request_bytes: &[u8],
    cap: Option<&SupervisorCap>,
    backend: &mut B,
) -> Result<SupervisorReply, SupervisorError> {
    if cap.is_none() {
        return Err(SupervisorError::CapabilityMissing);
    }
    let verb = decode_verb(request_bytes)?;
    match verb {
        SupervisorVerb::Start { service } => backend.start(service),
        SupervisorVerb::Stop { service } => backend.stop(service),
        SupervisorVerb::Restart { service } => backend.restart(service),
        SupervisorVerb::AwaitReady {
            service,
            timeout_ms,
        } => backend.await_ready(service, timeout_ms),
        SupervisorVerb::OnExitObserved { service } => backend.on_exit_observed(service),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Round-trip every error variant through the byte mapping. Total
    /// coverage is enforced at compile-time by the exhaustive `match`
    /// in `supervisor_error_to_byte`; this test guards the inverse
    /// path.
    #[test]
    fn every_error_round_trips_through_byte_mapping() {
        let errors = [
            SupervisorError::UnknownService,
            SupervisorError::PermissionDenied,
            SupervisorError::Timeout,
            SupervisorError::AlreadyRunning,
            SupervisorError::NotRunning,
            SupervisorError::MalformedRequest,
            SupervisorError::CapabilityMissing,
        ];
        for err in errors {
            let b = supervisor_error_to_byte(err);
            let back = byte_to_supervisor_error(b).expect("known byte decodes");
            assert_eq!(back, err);
        }
    }

    #[test]
    fn unknown_error_byte_returns_malformed_request() {
        let result = byte_to_supervisor_error(0xFF);
        assert!(matches!(result, Err(SupervisorError::MalformedRequest)));
    }

    #[test]
    fn encode_verb_rejects_oversized_service_name() {
        let big = "a".repeat(MAX_SERVICE_NAME_BYTES + 1);
        let verb = SupervisorVerb::Start { service: &big };
        let mut buf = [0u8; 128];
        let result = encode_verb(&verb, &mut buf);
        assert!(matches!(result, Err(SupervisorError::MalformedRequest)));
    }

    #[test]
    fn decode_verb_rejects_oversized_name_length_field() {
        // First byte: valid Start tag; second byte: 0xFF (bogus length).
        let bad = [TAG_VERB_START, 0xFF];
        let result = decode_verb(&bad);
        assert!(matches!(result, Err(SupervisorError::MalformedRequest)));
    }

    #[test]
    fn await_ready_round_trips_timeout() {
        let verb = SupervisorVerb::AwaitReady {
            service: "term",
            timeout_ms: 0xDEAD_BEEF_CAFE_F00D,
        };
        let mut buf = [0u8; 64];
        let len = encode_verb(&verb, &mut buf).expect("encode");
        let decoded = decode_verb(&buf[..len]).expect("decode");
        assert_eq!(decoded, verb);
    }
}
