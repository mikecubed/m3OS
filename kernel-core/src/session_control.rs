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
/// Phase 64 — return per-service `(name, ServiceState, restart_count,
/// step_failures)` quads instead of a single session-wide state.
const TAG_VERB_SESSION_STATE_DETAILED: u8 = 0x04;
/// Phase 64a — restart a single declared session service by name.
/// Distinct from `SESSION_RESTART` which restarts the whole graphical
/// session. The wire payload is `[tag][name_len: u8][name: name_len bytes]`.
///
/// **Public** so consumers that need to peek at the leading verb byte
/// before paying for a full `decode_verb` call (e.g.
/// `session_manager`'s deferred-reply dispatcher in `control.rs`,
/// which routes this verb through an async path) can reference one
/// canonical constant. Duplicating the value would silently break
/// routing on a future tag renumber.
pub const TAG_VERB_SESSION_RESTART_SERVICE: u8 = 0x05;

/// Reply tags.
const TAG_REPLY_STATE: u8 = 0x01;
const TAG_REPLY_ACK: u8 = 0x02;
const TAG_REPLY_ERROR: u8 = 0x03;
/// Phase 64 — `ServiceStates` reply carrying per-service quads
/// (`name`, `state`, `restart_count`, `step_failures`).
const TAG_REPLY_SERVICE_STATES: u8 = 0x04;

/// Session-state discriminants used in the `State` reply payload.
const STATE_TAG_BOOTING: u8 = 0x01;
const STATE_TAG_RUNNING: u8 = 0x02;
const STATE_TAG_RECOVERING: u8 = 0x03;
const STATE_TAG_TEXT_FALLBACK: u8 = 0x04;

/// Per-child service-state discriminants for the Phase 64
/// `ServiceStates` reply. Distinct value space from the session-wide
/// `STATE_TAG_*` above because the two types are orthogonal (see
/// `userspace/session_manager/src/table.rs`'s module doc).
pub const PER_SVC_STARTING: u8 = 0x01;
pub const PER_SVC_RUNNING: u8 = 0x02;
pub const PER_SVC_STOPPING: u8 = 0x03;
pub const PER_SVC_RESTARTING: u8 = 0x04;
pub const PER_SVC_FAILED: u8 = 0x05;

/// Maximum number of per-service entries the [`ControlReply::ServiceStates`]
/// reply carries. Sized comfortably above the declared session-step
/// count ([`crate::session_supervisor::DECLARED_SESSION_STEP_NAMES`] —
/// 6 in Phase 71 after `greeter` was inserted) so a wire-incompatible
/// change isn't needed when a track adds one more step. Allocation-free.
pub const MAX_SERVICE_STATE_ENTRIES: usize = 8;

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
    /// Phase 64 — return per-service `(name, ServiceState,
    /// restart_count, step_failures)` quads from the supervisor's
    /// `ServiceTable`. Distinct from the [`Self::SessionState`] verb
    /// (which still returns the session-wide [`SessionState`])
    /// because the two questions — "what is the graphical session
    /// doing?" and "what is each supervised child doing?" — have
    /// distinct, orthogonal answer types.
    SessionStateDetailed,
    /// Phase 64a — restart one declared session service by name. The
    /// supervisor delegates to init's `/run/init.cmd` `restart <name>`
    /// verb, which performs a clean stop + start through the manifest.
    /// Distinct from [`Self::SessionRestart`], which restarts the
    /// entire graphical session.
    ///
    /// `name` is a fixed 32-byte buffer; `name_len` bounds the valid
    /// bytes. Empty names (`name_len == 0`) are rejected by the
    /// dispatcher.
    SessionRestartService {
        name: [u8; MAX_STEP_NAME_BYTES],
        name_len: u8,
    },
}

impl ControlVerb {
    /// Construct a [`Self::SessionRestartService`] from a `&str`,
    /// returning `MalformedRequest` if the name exceeds
    /// [`MAX_STEP_NAME_BYTES`] or is empty. The helper centralizes the
    /// length-check so callers (codec encoders, m3ctl arg parser,
    /// tests) cannot diverge.
    pub fn new_session_restart_service(service: &str) -> Result<Self, SessionControlError> {
        let bytes = service.as_bytes();
        if bytes.is_empty() || bytes.len() > MAX_STEP_NAME_BYTES {
            return Err(SessionControlError::MalformedRequest);
        }
        let mut name = [0u8; MAX_STEP_NAME_BYTES];
        name[..bytes.len()].copy_from_slice(bytes);
        Ok(ControlVerb::SessionRestartService {
            name,
            name_len: bytes.len() as u8,
        })
    }

    /// Borrow the service-name slice from a
    /// [`Self::SessionRestartService`] verb, or `None` for other verbs
    /// or when the stored `name_len` is corrupt. Centralizes the
    /// length-clamp so consumers cannot read past `name_len`.
    pub fn restart_service_name(&self) -> Option<&str> {
        match self {
            ControlVerb::SessionRestartService { name, name_len } => {
                let len = (*name_len as usize).min(MAX_STEP_NAME_BYTES);
                core::str::from_utf8(&name[..len]).ok()
            }
            _ => None,
        }
    }
}

/// One per-service entry in a [`ControlReply::ServiceStates`] reply.
///
/// Allocation-free: the service name is held in a fixed 32-byte buffer
/// indexed by `name_len`. `state_tag` is one of the `PER_SVC_*`
/// constants — keeping the encoded discriminant alongside the entry
/// means a future kernel-core enum change does not break the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServiceStateEntry {
    /// Bytes 0..name_len are the service name (UTF-8). Remaining bytes
    /// are zero-padded so `PartialEq` does not look at uninitialized
    /// memory.
    pub name: [u8; MAX_STEP_NAME_BYTES],
    /// Number of valid bytes in [`Self::name`]. Bounded by
    /// `MAX_STEP_NAME_BYTES`.
    pub name_len: u8,
    /// One of the `PER_SVC_*` constants
    /// ([`PER_SVC_STARTING`] ... [`PER_SVC_FAILED`]).
    pub state_tag: u8,
    /// Number of full restart attempts since boot — sourced from
    /// `ServiceTable::ServiceEntry::restart_count`.
    pub restart_count: u32,
    /// Number of step failures within the current restart attempt —
    /// sourced from `ServiceTable::ServiceEntry::step_failures`.
    pub step_failures: u32,
}

impl ServiceStateEntry {
    /// Empty placeholder — `name_len == 0` and every field zeroed.
    /// Used to pad the [`ControlReply::ServiceStates`] fixed-size
    /// buffer when fewer than [`MAX_SERVICE_STATE_ENTRIES`] entries
    /// are populated.
    pub const fn empty() -> Self {
        Self {
            name: [0u8; MAX_STEP_NAME_BYTES],
            name_len: 0,
            state_tag: 0,
            restart_count: 0,
            step_failures: 0,
        }
    }

    /// Borrow the service name as a `&str`, or `None` if the bytes are
    /// not valid UTF-8 (the encoder rejects non-UTF-8 names; the
    /// decoder calls this to surface a typed view).
    pub fn name_as_str(&self) -> Option<&str> {
        let len = (self.name_len as usize).min(MAX_STEP_NAME_BYTES);
        core::str::from_utf8(&self.name[..len]).ok()
    }
}

/// Replies from `session_manager` back to `m3ctl`.
///
/// The `ServiceStates` variant intentionally holds the entry array
/// inline (≈ 336 bytes on 64-bit hosts) rather than behind a `Box`:
/// the codec is `no_std` and the enum value lives on the stack of one
/// dispatch call, so the size cost is bounded and the avoidance of
/// `alloc` outside the variant's lifetime is the right trade-off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(clippy::large_enum_variant)]
pub enum ControlReply {
    /// `SessionState` query reply.
    State { state: SessionState },
    /// `SessionStop` / `SessionRestart` succeeded.
    Ack,
    /// Verb rejected. The variant carries the typed error.
    Error(SessionControlError),
    /// Phase 64 — per-service `ServiceTable` snapshot returned by the
    /// [`ControlVerb::SessionStateDetailed`] verb. `entry_count` valid
    /// entries occupy slots `0..entry_count` of `entries`; remaining
    /// slots are [`ServiceStateEntry::empty`].
    ServiceStates {
        entry_count: u8,
        entries: [ServiceStateEntry; MAX_SERVICE_STATE_ENTRIES],
    },
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

    /// Phase 64 — return per-service `ServiceTable` snapshot for the
    /// [`ControlVerb::SessionStateDetailed`] verb. The default
    /// implementation returns an empty snapshot so existing Phase 57
    /// backends (and tests) continue to work without modification.
    /// Production `session_manager` overrides this to walk its
    /// `ServiceTable` and populate the entries.
    fn services_snapshot(&mut self) -> (u8, [ServiceStateEntry; MAX_SERVICE_STATE_ENTRIES]) {
        (0, [ServiceStateEntry::empty(); MAX_SERVICE_STATE_ENTRIES])
    }

    /// Phase 64a — restart one supervised service by name. The default
    /// implementation returns
    /// [`SessionControlError::Internal`] so backends that do not
    /// implement per-service restart (Phase 57 tests, the empty-default
    /// host-test stubs) reject the verb cleanly. Production
    /// `session_manager` overrides this to delegate to init via
    /// `/run/init.cmd`.
    fn session_restart_service(&mut self, _service: &str) -> Result<(), SessionControlError> {
        Err(SessionControlError::Internal)
    }
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
    match verb {
        ControlVerb::SessionState => {
            dst[0] = TAG_VERB_SESSION_STATE;
            Ok(1)
        }
        ControlVerb::SessionStop => {
            dst[0] = TAG_VERB_SESSION_STOP;
            Ok(1)
        }
        ControlVerb::SessionRestart => {
            dst[0] = TAG_VERB_SESSION_RESTART;
            Ok(1)
        }
        ControlVerb::SessionStateDetailed => {
            dst[0] = TAG_VERB_SESSION_STATE_DETAILED;
            Ok(1)
        }
        ControlVerb::SessionRestartService { name, name_len } => {
            let len = *name_len as usize;
            if len == 0 || len > MAX_STEP_NAME_BYTES {
                return Err(SessionControlError::MalformedRequest);
            }
            // Layout: [tag][name_len: u8][name: name_len bytes]
            let total = 2 + len;
            if dst.len() < total {
                return Err(SessionControlError::MalformedRequest);
            }
            dst[0] = TAG_VERB_SESSION_RESTART_SERVICE;
            dst[1] = *name_len;
            dst[2..2 + len].copy_from_slice(&name[..len]);
            Ok(total)
        }
    }
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
        TAG_VERB_SESSION_STATE_DETAILED => Ok(ControlVerb::SessionStateDetailed),
        TAG_VERB_SESSION_RESTART_SERVICE => {
            if src.len() < 2 {
                return Err(SessionControlError::MalformedRequest);
            }
            let name_len = src[1] as usize;
            if name_len == 0 || name_len > MAX_STEP_NAME_BYTES {
                return Err(SessionControlError::MalformedRequest);
            }
            if src.len() < 2 + name_len {
                return Err(SessionControlError::MalformedRequest);
            }
            let mut name = [0u8; MAX_STEP_NAME_BYTES];
            name[..name_len].copy_from_slice(&src[2..2 + name_len]);
            Ok(ControlVerb::SessionRestartService {
                name,
                name_len: name_len as u8,
            })
        }
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
        ControlReply::ServiceStates {
            entry_count,
            entries,
        } => {
            // Layout:
            //   [0] TAG_REPLY_SERVICE_STATES
            //   [1] entry_count (u8, ≤ MAX_SERVICE_STATE_ENTRIES)
            //   For each valid entry, in order:
            //     [..1] name_len (u8, ≤ MAX_STEP_NAME_BYTES)
            //     [..name_len] name bytes (UTF-8)
            //     [..1] state_tag (PER_SVC_*)
            //     [..4] restart_count (LE u32)
            //     [..4] step_failures (LE u32)
            let count = (*entry_count as usize).min(MAX_SERVICE_STATE_ENTRIES);
            // Pre-compute total length to bound-check `dst` once.
            let mut total = 2; // tag + count
            for entry in entries.iter().take(count) {
                let name_len = (entry.name_len as usize).min(MAX_STEP_NAME_BYTES);
                total += 1 + name_len + 1 + 4 + 4;
            }
            if dst.len() < total {
                return Err(SessionControlError::MalformedRequest);
            }
            dst[0] = TAG_REPLY_SERVICE_STATES;
            // Cast safe: count ≤ MAX_SERVICE_STATE_ENTRIES = 8 < u8::MAX.
            dst[1] = count as u8;
            let mut off = 2;
            for entry in entries.iter().take(count) {
                let name_len = (entry.name_len as usize).min(MAX_STEP_NAME_BYTES);
                if name_len > MAX_STEP_NAME_BYTES {
                    return Err(SessionControlError::MalformedRequest);
                }
                dst[off] = name_len as u8;
                off += 1;
                dst[off..off + name_len].copy_from_slice(&entry.name[..name_len]);
                off += name_len;
                dst[off] = entry.state_tag;
                off += 1;
                dst[off..off + 4].copy_from_slice(&entry.restart_count.to_le_bytes());
                off += 4;
                dst[off..off + 4].copy_from_slice(&entry.step_failures.to_le_bytes());
                off += 4;
            }
            Ok(total)
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
        TAG_REPLY_SERVICE_STATES => {
            // Mirror of the encoder. See `encode_reply` for the layout
            // contract. Decode fails on any malformed length so a
            // truncated transport cannot produce a partially-valid
            // `ControlReply::ServiceStates`.
            if src.len() < 2 {
                return Err(SessionControlError::MalformedRequest);
            }
            let count = src[1] as usize;
            if count > MAX_SERVICE_STATE_ENTRIES {
                return Err(SessionControlError::MalformedRequest);
            }
            let mut entries = [ServiceStateEntry::empty(); MAX_SERVICE_STATE_ENTRIES];
            let mut off = 2;
            for entry in entries.iter_mut().take(count) {
                if src.len() < off + 1 {
                    return Err(SessionControlError::MalformedRequest);
                }
                let name_len = src[off] as usize;
                off += 1;
                if name_len > MAX_STEP_NAME_BYTES {
                    return Err(SessionControlError::MalformedRequest);
                }
                if src.len() < off + name_len + 1 + 4 + 4 {
                    return Err(SessionControlError::MalformedRequest);
                }
                entry.name_len = name_len as u8;
                entry.name[..name_len].copy_from_slice(&src[off..off + name_len]);
                off += name_len;
                entry.state_tag = src[off];
                off += 1;
                let mut buf4 = [0u8; 4];
                buf4.copy_from_slice(&src[off..off + 4]);
                entry.restart_count = u32::from_le_bytes(buf4);
                off += 4;
                buf4.copy_from_slice(&src[off..off + 4]);
                entry.step_failures = u32::from_le_bytes(buf4);
                off += 4;
            }
            Ok(ControlReply::ServiceStates {
                entry_count: count as u8,
                entries,
            })
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
        ControlVerb::SessionStateDetailed => {
            let (entry_count, entries) = backend.services_snapshot();
            Ok(ControlReply::ServiceStates {
                entry_count,
                entries,
            })
        }
        ControlVerb::SessionRestartService { .. } => {
            let service = match verb.restart_service_name() {
                Some(s) => s,
                None => return Err(SessionControlError::MalformedRequest),
            };
            match backend.session_restart_service(service) {
                Ok(()) => Ok(ControlReply::Ack),
                Err(e) => Ok(ControlReply::Error(e)),
            }
        }
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

    // -----------------------------------------------------------------
    // Phase 64 — `SessionStateDetailed` verb + `ServiceStates` reply
    // -----------------------------------------------------------------

    /// Helper: build a `ServiceStateEntry` from a Rust `&str` for tests.
    fn entry(
        name: &str,
        state_tag: u8,
        restart_count: u32,
        step_failures: u32,
    ) -> ServiceStateEntry {
        let mut e = ServiceStateEntry::empty();
        let bytes = name.as_bytes();
        e.name_len = bytes.len() as u8;
        e.name[..bytes.len()].copy_from_slice(bytes);
        e.state_tag = state_tag;
        e.restart_count = restart_count;
        e.step_failures = step_failures;
        e
    }

    #[test]
    fn session_state_detailed_verb_round_trips() {
        let mut buf = [0u8; 8];
        let len = encode_verb(&ControlVerb::SessionStateDetailed, &mut buf).expect("encode");
        assert_eq!(&buf[..len], &[TAG_VERB_SESSION_STATE_DETAILED]);
        let v = decode_verb(&buf[..len]).expect("decode");
        assert_eq!(v, ControlVerb::SessionStateDetailed);
    }

    #[test]
    fn service_states_reply_round_trips() {
        let mut entries = [ServiceStateEntry::empty(); MAX_SERVICE_STATE_ENTRIES];
        entries[0] = entry("display_server", PER_SVC_RUNNING, 0, 0);
        entries[1] = entry("kbd_server", PER_SVC_RUNNING, 0, 0);
        entries[2] = entry("audio_server", PER_SVC_FAILED, 3, 0);
        entries[3] = entry("term", PER_SVC_RESTARTING, 1, 2);
        let reply = ControlReply::ServiceStates {
            entry_count: 4,
            entries,
        };
        let mut buf = [0u8; 256];
        let len = encode_reply(&reply, &mut buf).expect("encode");
        let decoded = decode_reply(&buf[..len]).expect("decode");
        assert_eq!(decoded, reply);
    }

    #[test]
    fn service_states_empty_reply_round_trips() {
        let reply = ControlReply::ServiceStates {
            entry_count: 0,
            entries: [ServiceStateEntry::empty(); MAX_SERVICE_STATE_ENTRIES],
        };
        let mut buf = [0u8; 64];
        let len = encode_reply(&reply, &mut buf).expect("encode");
        assert_eq!(len, 2);
        assert_eq!(buf[0], TAG_REPLY_SERVICE_STATES);
        assert_eq!(buf[1], 0);
        let decoded = decode_reply(&buf[..len]).expect("decode");
        assert_eq!(decoded, reply);
    }

    #[test]
    fn service_states_reply_rejects_oversized_name_in_encode() {
        let mut e = ServiceStateEntry::empty();
        e.name_len = 0xFF; // larger than MAX_STEP_NAME_BYTES
        e.state_tag = PER_SVC_RUNNING;
        let mut entries = [ServiceStateEntry::empty(); MAX_SERVICE_STATE_ENTRIES];
        entries[0] = e;
        let reply = ControlReply::ServiceStates {
            entry_count: 1,
            entries,
        };
        let mut buf = [0u8; 256];
        // The encoder bounds the per-entry name_len to MAX_STEP_NAME_BYTES
        // before writing; the resulting wire bytes are valid but use the
        // truncated length. The encoder MUST NOT silently produce a
        // wire that re-claims `name_len = 0xFF`.
        let len = encode_reply(&reply, &mut buf).expect("encode bounds to MAX_STEP_NAME_BYTES");
        // 2 bytes header + 1 byte name_len + 32 bytes name + 1 byte
        // state_tag + 4 bytes restart_count + 4 bytes step_failures.
        assert_eq!(len, 2 + 1 + MAX_STEP_NAME_BYTES + 1 + 4 + 4);
        // The decoded reply's name_len matches the truncated value.
        let decoded = decode_reply(&buf[..len]).expect("decode");
        if let ControlReply::ServiceStates { entries, .. } = decoded {
            assert_eq!(entries[0].name_len as usize, MAX_STEP_NAME_BYTES);
        } else {
            panic!("expected ServiceStates");
        }
    }

    #[test]
    fn service_states_reply_rejects_too_many_entries_on_decode() {
        // [TAG_REPLY_SERVICE_STATES, count=0xFF, ...] — count > MAX.
        let bad = [TAG_REPLY_SERVICE_STATES, 0xFF];
        let result = decode_reply(&bad);
        assert!(matches!(result, Err(SessionControlError::MalformedRequest)));
    }

    #[test]
    fn service_states_reply_rejects_truncated_buffer_on_decode() {
        // Header says 1 entry but only the header is present.
        let bad = [TAG_REPLY_SERVICE_STATES, 1];
        let result = decode_reply(&bad);
        assert!(matches!(result, Err(SessionControlError::MalformedRequest)));
    }

    /// Default `services_snapshot` returns 0 entries — Phase 57
    /// backends and existing tests are unaffected.
    #[test]
    fn default_services_snapshot_is_empty() {
        struct DummyBackend;
        impl SessionControlBackend for DummyBackend {
            fn current_state(&mut self) -> SessionState {
                SessionState::Running
            }
            fn session_stop(&mut self) -> Result<(), SessionControlError> {
                Ok(())
            }
            fn session_restart(&mut self) -> Result<(), SessionControlError> {
                Ok(())
            }
        }
        let mut b = DummyBackend;
        let (count, _entries) = b.services_snapshot();
        assert_eq!(count, 0);
    }

    // -----------------------------------------------------------------
    // Phase 64a — SessionRestartService codec + dispatcher
    // -----------------------------------------------------------------

    #[test]
    fn session_restart_service_verb_round_trips() {
        let verb = ControlVerb::new_session_restart_service("display_server").expect("ctor");
        let mut buf = [0u8; 64];
        let n = encode_verb(&verb, &mut buf).expect("encode");
        // Layout: tag + name_len + name. "display_server" is 14 bytes.
        assert_eq!(n, 2 + 14);
        assert_eq!(buf[0], TAG_VERB_SESSION_RESTART_SERVICE);
        assert_eq!(buf[1], 14);
        assert_eq!(&buf[2..2 + 14], b"display_server");
        let decoded = decode_verb(&buf[..n]).expect("decode");
        assert_eq!(decoded.restart_service_name(), Some("display_server"));
    }

    #[test]
    fn session_restart_service_ctor_rejects_empty_name() {
        let result = ControlVerb::new_session_restart_service("");
        assert!(matches!(result, Err(SessionControlError::MalformedRequest)));
    }

    #[test]
    fn session_restart_service_ctor_rejects_oversized_name() {
        let big = "x".repeat(MAX_STEP_NAME_BYTES + 1);
        let result = ControlVerb::new_session_restart_service(&big);
        assert!(matches!(result, Err(SessionControlError::MalformedRequest)));
    }

    #[test]
    fn session_restart_service_decode_rejects_zero_name_len() {
        let bad = [TAG_VERB_SESSION_RESTART_SERVICE, 0];
        let result = decode_verb(&bad);
        assert!(matches!(result, Err(SessionControlError::MalformedRequest)));
    }

    #[test]
    fn session_restart_service_decode_rejects_truncated_payload() {
        // Header claims 8 bytes of name but only 3 follow.
        let bad = [TAG_VERB_SESSION_RESTART_SERVICE, 8, b'a', b'b', b'c'];
        let result = decode_verb(&bad);
        assert!(matches!(result, Err(SessionControlError::MalformedRequest)));
    }

    #[test]
    fn dispatch_authenticated_forwards_session_restart_service_to_backend() {
        use core::cell::RefCell;
        struct RecBackend {
            calls: RefCell<alloc::vec::Vec<alloc::string::String>>,
        }
        impl SessionControlBackend for RecBackend {
            fn current_state(&mut self) -> SessionState {
                SessionState::Running
            }
            fn session_stop(&mut self) -> Result<(), SessionControlError> {
                Ok(())
            }
            fn session_restart(&mut self) -> Result<(), SessionControlError> {
                Ok(())
            }
            fn session_restart_service(
                &mut self,
                service: &str,
            ) -> Result<(), SessionControlError> {
                self.calls
                    .borrow_mut()
                    .push(alloc::string::String::from(service));
                Ok(())
            }
        }
        let cap = ControlSocketCap::granted_for_m3ctl_only();
        let mut backend = RecBackend {
            calls: RefCell::new(alloc::vec::Vec::new()),
        };
        let verb = ControlVerb::new_session_restart_service("term").unwrap();
        let mut buf = [0u8; 64];
        let n = encode_verb(&verb, &mut buf).unwrap();
        let reply = dispatch_authenticated(&buf[..n], Some(&cap), &mut backend).expect("ok");
        assert_eq!(reply, ControlReply::Ack);
        assert_eq!(backend.calls.borrow().as_slice(), &["term".to_string()]);
    }

    #[test]
    fn dispatch_authenticated_session_restart_service_surfaces_backend_error() {
        struct FailBackend;
        impl SessionControlBackend for FailBackend {
            fn current_state(&mut self) -> SessionState {
                SessionState::Running
            }
            fn session_stop(&mut self) -> Result<(), SessionControlError> {
                Ok(())
            }
            fn session_restart(&mut self) -> Result<(), SessionControlError> {
                Ok(())
            }
            fn session_restart_service(
                &mut self,
                _service: &str,
            ) -> Result<(), SessionControlError> {
                Err(SessionControlError::Internal)
            }
        }
        let cap = ControlSocketCap::granted_for_m3ctl_only();
        let mut backend = FailBackend;
        let verb = ControlVerb::new_session_restart_service("display_server").unwrap();
        let mut buf = [0u8; 64];
        let n = encode_verb(&verb, &mut buf).unwrap();
        let reply = dispatch_authenticated(&buf[..n], Some(&cap), &mut backend).expect("ok");
        assert_eq!(reply, ControlReply::Error(SessionControlError::Internal));
    }
}
