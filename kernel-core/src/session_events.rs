//! Phase 64b — wire codec for init→session_manager exit-event push
//! notifications.
//!
//! Init reaps every supervised child in its `handle_child_exit` path.
//! Before Phase 64b `session_manager` learned about each exit by
//! polling `/run/services.status` every 500 ms, which means worst-case
//! observation latency was ~500 ms. Phase 64b reduces that to
//! interrupt-driven latency by having init `ipc_send_buf` a small
//! payload to `session_manager`'s `"session-events"` IPC endpoint
//! immediately after the reap. `session_manager`'s event loop drains
//! the events on each tick and uses them to wake any parked
//! deferred-reply machinery without waiting on the status-file poll.
//!
//! The wire is intentionally tiny and fixed-layout so the codec is
//! host-testable and `no_std`. No allocation.

/// IPC label `session_manager` accepts on the `"session-events"`
/// endpoint when the bulk carries a serialized [`ServiceExitEvent`].
/// Distinct from `session_control::LABEL_CTL_CMD` so a future
/// in-process multiplexer can route messages from a single endpoint
/// without parsing the bulk.
pub const LABEL_SESSION_EVENT_EXIT: u64 = 1;

/// Service-registry name `session_manager` registers its
/// push-notification endpoint under. Init looks this up lazily on the
/// first reap-loop iteration and caches the cap; an early reap before
/// `session_manager` registers simply finds no endpoint and skips
/// notification (the receiver's status-file fallback still observes
/// the exit later).
pub const SESSION_EVENTS_SERVICE_NAME: &str = "session-events";

/// Maximum service-name bytes carried by a [`ServiceExitEvent`].
/// Matches `MAX_STEP_NAME_BYTES` so any name init's manifest accepts
/// fits on the wire without truncation.
pub const MAX_EVENT_NAME_BYTES: usize = 32;

/// Maximum encoded byte size of a [`ServiceExitEvent`].
/// `[pid: 4][exit_code: 4][signaled: 1][name_len: 1][name: ≤32]`.
pub const MAX_EXIT_EVENT_BYTES: usize = 4 + 4 + 1 + 1 + MAX_EVENT_NAME_BYTES;

/// One reaped-child exit observation pushed by init to
/// `session_manager`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServiceExitEvent {
    /// PID that exited. `> 0` for any real reap; `0` is invalid and
    /// rejected by the decoder.
    pub pid: i32,
    /// Exit code as `waitpid` reports it. For a normal exit the value
    /// is the exit status (0..=255); for a signal death the value is
    /// the negative signal number (init encodes signal death as
    /// `stopped:-N` in `/run/services.status` and that same `-N` is
    /// what this field carries).
    pub exit_code: i32,
    /// `true` when the child died from a signal rather than calling
    /// `exit(2)`. Mirrors init's `signaled` flag in
    /// `Self::handle_child_exit`.
    pub signaled: bool,
    /// Service name as it appears in init's manifest (e.g. `"kbd"` or
    /// `"audio_server"`). Bytes 0..`name_len` are valid; remaining
    /// bytes in the on-wire buffer are zero-padded.
    pub name: [u8; MAX_EVENT_NAME_BYTES],
    /// Valid bytes in [`Self::name`]. Bounded by
    /// [`MAX_EVENT_NAME_BYTES`].
    pub name_len: u8,
}

/// Errors returned by the codec. Kept open-ended via `non_exhaustive`
/// so future codec extensions can add variants without breaking
/// pattern-matching callers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SessionEventError {
    /// The destination buffer is shorter than `MAX_EXIT_EVENT_BYTES`,
    /// the source buffer is shorter than the declared payload, or the
    /// declared `name_len` exceeds `MAX_EVENT_NAME_BYTES`.
    Malformed,
}

impl ServiceExitEvent {
    /// Construct an event from a `&str` service name, returning
    /// `Malformed` if the name exceeds [`MAX_EVENT_NAME_BYTES`] or is
    /// empty. PID `0` is rejected (init's reap path filters it).
    pub fn new(
        pid: i32,
        exit_code: i32,
        signaled: bool,
        service_name: &str,
    ) -> Result<Self, SessionEventError> {
        if pid <= 0 {
            return Err(SessionEventError::Malformed);
        }
        let bytes = service_name.as_bytes();
        if bytes.is_empty() || bytes.len() > MAX_EVENT_NAME_BYTES {
            return Err(SessionEventError::Malformed);
        }
        let mut name = [0u8; MAX_EVENT_NAME_BYTES];
        name[..bytes.len()].copy_from_slice(bytes);
        Ok(Self {
            pid,
            exit_code,
            signaled,
            name,
            name_len: bytes.len() as u8,
        })
    }

    /// Borrow the service name as a `&str`, or `None` if `name_len` is
    /// corrupt or the bytes are not valid UTF-8.
    pub fn name_as_str(&self) -> Option<&str> {
        let len = (self.name_len as usize).min(MAX_EVENT_NAME_BYTES);
        core::str::from_utf8(&self.name[..len]).ok()
    }
}

/// Encode `event` into `dst`. Returns bytes written, or `Malformed`
/// if `dst` is too small or `name_len` is out of range.
pub fn encode_exit_event(
    event: &ServiceExitEvent,
    dst: &mut [u8],
) -> Result<usize, SessionEventError> {
    let name_len = event.name_len as usize;
    if name_len == 0 || name_len > MAX_EVENT_NAME_BYTES {
        return Err(SessionEventError::Malformed);
    }
    let total = 10 + name_len;
    if dst.len() < total {
        return Err(SessionEventError::Malformed);
    }
    dst[0..4].copy_from_slice(&event.pid.to_le_bytes());
    dst[4..8].copy_from_slice(&event.exit_code.to_le_bytes());
    dst[8] = if event.signaled { 1 } else { 0 };
    dst[9] = event.name_len;
    dst[10..10 + name_len].copy_from_slice(&event.name[..name_len]);
    Ok(total)
}

/// Decode `src` into a [`ServiceExitEvent`].
pub fn decode_exit_event(src: &[u8]) -> Result<ServiceExitEvent, SessionEventError> {
    if src.len() < 10 {
        return Err(SessionEventError::Malformed);
    }
    let mut pid_bytes = [0u8; 4];
    pid_bytes.copy_from_slice(&src[0..4]);
    let pid = i32::from_le_bytes(pid_bytes);
    if pid <= 0 {
        return Err(SessionEventError::Malformed);
    }
    let mut exit_bytes = [0u8; 4];
    exit_bytes.copy_from_slice(&src[4..8]);
    let exit_code = i32::from_le_bytes(exit_bytes);
    let signaled = match src[8] {
        0 => false,
        1 => true,
        _ => return Err(SessionEventError::Malformed),
    };
    let name_len = src[9] as usize;
    if name_len == 0 || name_len > MAX_EVENT_NAME_BYTES {
        return Err(SessionEventError::Malformed);
    }
    if src.len() < 10 + name_len {
        return Err(SessionEventError::Malformed);
    }
    let mut name = [0u8; MAX_EVENT_NAME_BYTES];
    name[..name_len].copy_from_slice(&src[10..10 + name_len]);
    Ok(ServiceExitEvent {
        pid,
        exit_code,
        signaled,
        name,
        name_len: name_len as u8,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_normal_exit() {
        let event = ServiceExitEvent::new(42, 0, false, "term").expect("ctor");
        let mut buf = [0u8; MAX_EXIT_EVENT_BYTES];
        let n = encode_exit_event(&event, &mut buf).expect("encode");
        assert_eq!(n, 10 + 4);
        let decoded = decode_exit_event(&buf[..n]).expect("decode");
        assert_eq!(decoded, event);
        assert_eq!(decoded.name_as_str(), Some("term"));
    }

    #[test]
    fn round_trips_signal_exit_with_negative_code() {
        let event = ServiceExitEvent::new(99, -15, true, "audio_server").expect("ctor");
        let mut buf = [0u8; MAX_EXIT_EVENT_BYTES];
        let n = encode_exit_event(&event, &mut buf).expect("encode");
        let decoded = decode_exit_event(&buf[..n]).expect("decode");
        assert_eq!(decoded, event);
        assert_eq!(decoded.exit_code, -15);
        assert!(decoded.signaled);
    }

    #[test]
    fn ctor_rejects_zero_pid() {
        assert!(matches!(
            ServiceExitEvent::new(0, 0, false, "x"),
            Err(SessionEventError::Malformed)
        ));
    }

    #[test]
    fn ctor_rejects_negative_pid() {
        assert!(matches!(
            ServiceExitEvent::new(-5, 0, false, "x"),
            Err(SessionEventError::Malformed)
        ));
    }

    #[test]
    fn ctor_rejects_empty_name() {
        assert!(matches!(
            ServiceExitEvent::new(1, 0, false, ""),
            Err(SessionEventError::Malformed)
        ));
    }

    #[test]
    fn ctor_rejects_oversized_name() {
        let big = core::str::from_utf8(&[b'x'; MAX_EVENT_NAME_BYTES + 1]).unwrap();
        assert!(matches!(
            ServiceExitEvent::new(1, 0, false, big),
            Err(SessionEventError::Malformed)
        ));
    }

    #[test]
    fn decode_rejects_truncated_header() {
        assert!(matches!(
            decode_exit_event(&[1, 0, 0, 0, 0, 0, 0, 0, 0]),
            Err(SessionEventError::Malformed)
        ));
    }

    #[test]
    fn decode_rejects_zero_pid() {
        let buf = [0, 0, 0, 0, 0, 0, 0, 0, 0, 1, b'x'];
        assert!(matches!(
            decode_exit_event(&buf),
            Err(SessionEventError::Malformed)
        ));
    }

    #[test]
    fn decode_rejects_zero_name_len() {
        let buf = [1, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        assert!(matches!(
            decode_exit_event(&buf),
            Err(SessionEventError::Malformed)
        ));
    }

    #[test]
    fn decode_rejects_oversized_name_len() {
        let buf = [1, 0, 0, 0, 0, 0, 0, 0, 0, (MAX_EVENT_NAME_BYTES + 1) as u8];
        assert!(matches!(
            decode_exit_event(&buf),
            Err(SessionEventError::Malformed)
        ));
    }

    #[test]
    fn decode_rejects_invalid_signaled_byte() {
        let mut buf = [0u8; 11];
        buf[0..4].copy_from_slice(&1i32.to_le_bytes());
        buf[8] = 2; // invalid
        buf[9] = 1;
        buf[10] = b'x';
        assert!(matches!(
            decode_exit_event(&buf),
            Err(SessionEventError::Malformed)
        ));
    }

    #[test]
    fn decode_rejects_truncated_name_payload() {
        // Header claims 5 bytes of name but only 2 follow.
        let mut buf = [0u8; 12];
        buf[0..4].copy_from_slice(&7i32.to_le_bytes());
        buf[9] = 5;
        buf[10] = b'a';
        buf[11] = b'b';
        assert!(matches!(
            decode_exit_event(&buf),
            Err(SessionEventError::Malformed)
        ));
    }
}
