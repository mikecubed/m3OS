//! `WakeKind` tag and ABI encode/decode for the bound-notification recv path.
//!
//! # Design rationale
//!
//! The IPC recv path must distinguish two wake sources without colliding with
//! the negative-errno values that already occupy the syscall return channel:
//!
//! - A peer sent a message → [`WakeKind::Message`] carrying the peer's label.
//! - A bound notification was signalled → [`WakeKind::Notification`] carrying
//!   the drained notification bitset.
//!
//! A dedicated 1-byte "recv kind" out-register carries the tag so that the
//! syscall return value remains unambiguous. [`RECV_KIND_MESSAGE`] and
//! [`RECV_KIND_NOTIFICATION`] define the two legal tag values.
//!
//! # Encoding rules
//!
//! | Wake source  | `kind` byte            | `IpcMessage.label` | `IpcMessage.data[0]` |
//! |--------------|------------------------|--------------------|----------------------|
//! | Message      | `RECV_KIND_MESSAGE`    | peer-provided      | message payload      |
//! | Notification | `RECV_KIND_NOTIFICATION` | `0`              | drained bits         |
//!
//! Negative errnos remain in the syscall return channel; they are never mixed
//! with the kind tag.

use super::message::Message;

/// Recv-kind tag: the wake came from a regular IPC message.
pub const RECV_KIND_MESSAGE: u8 = 0;

/// Recv-kind tag: the wake came from a drained notification bitset.
pub const RECV_KIND_NOTIFICATION: u8 = 1;

/// The source of a recv-path wake.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeKind {
    /// A peer sent a message; the `u64` is the peer-provided label.
    Message(u64),
    /// A bound notification was signalled; the `u64` is the drained bit mask.
    Notification(u64),
}

/// Encode a [`WakeKind`] into a `(kind tag, `[`Message`]`)` pair suitable for
/// returning from the recv syscall.
///
/// The caller places `kind` in the dedicated kind out-register and copies
/// the [`Message`] into the userspace IPC buffer.
pub fn encode_wake_kind(wake: WakeKind) -> (u8, Message) {
    match wake {
        WakeKind::Message(label) => (RECV_KIND_MESSAGE, Message::new(label)),
        WakeKind::Notification(bits) => {
            let mut msg = Message::new(0);
            msg.data[0] = bits;
            (RECV_KIND_NOTIFICATION, msg)
        }
    }
}

/// Classify a recv-path priority check based on drained notification bits.
///
/// Called by [`endpoint::recv_msg_with_notif`] as the **first** action: the
/// drained pending bits are the sole input. When they are non-zero the
/// notification takes priority over any queued endpoint sender and the recv
/// returns immediately with [`RECV_KIND_NOTIFICATION`]. When the bits are
/// zero the caller proceeds to inspect the endpoint sender queue, returning
/// [`RECV_KIND_MESSAGE`] if a sender is present or blocking otherwise.
///
/// Extracting the rule here makes it testable via `kernel_core` in contexts
/// where the real `notification::drain_bits` global state is unavailable (e.g.
/// the standalone QEMU integration-test binary in `kernel/tests/bound_recv.rs`).
pub const fn classify_recv(pending_bits: u64) -> u8 {
    if pending_bits != 0 {
        RECV_KIND_NOTIFICATION
    } else {
        RECV_KIND_MESSAGE
    }
}

/// Decode a `(kind tag, `[`Message`]`)` pair back into a [`WakeKind`].
///
/// An unknown `kind` byte falls back to [`WakeKind::Message`] so that
/// userspace code compiled against an older ABI degrades gracefully rather
/// than triggering undefined behaviour.
pub fn decode_wake_kind(kind: u8, msg: Message) -> WakeKind {
    match kind {
        RECV_KIND_NOTIFICATION => WakeKind::Notification(msg.data[0]),
        // RECV_KIND_MESSAGE (0) and any unknown future tag fall through here.
        _ => WakeKind::Message(msg.label),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- A.2 acceptance tests (committed red before encode/decode implementation) ---

    #[test]
    fn message_round_trips_label() {
        let label = 0xdead_beef_cafe_0001u64;
        let wake = WakeKind::Message(label);
        let (kind, msg) = encode_wake_kind(wake);
        assert_eq!(kind, RECV_KIND_MESSAGE);
        assert_eq!(decode_wake_kind(kind, msg), WakeKind::Message(label));
    }

    #[test]
    fn notification_round_trips_bits() {
        let bits = 0x0000_0000_0000_0003u64;
        let wake = WakeKind::Notification(bits);
        let (kind, msg) = encode_wake_kind(wake);
        assert_eq!(kind, RECV_KIND_NOTIFICATION);
        assert_eq!(decode_wake_kind(kind, msg), WakeKind::Notification(bits));
    }

    #[test]
    fn notification_bits_land_in_data0_and_label_is_zero() {
        let bits = 0xffff_0000_1234_5678u64;
        let (_, msg) = encode_wake_kind(WakeKind::Notification(bits));
        assert_eq!(msg.data[0], bits, "drained bits must appear in data[0]");
        assert_eq!(msg.label, 0, "label must be zero on a notification wake");
    }

    #[test]
    fn message_label_preserved_and_not_aliased_to_data0() {
        let label = 42u64;
        let (_, msg) = encode_wake_kind(WakeKind::Message(label));
        assert_eq!(msg.label, label, "peer label must be preserved");
        // data[0] is part of the message payload and is not a notification mask;
        // we only assert the label is correctly placed.
    }

    #[test]
    fn recv_kind_message_constant_is_zero() {
        assert_eq!(RECV_KIND_MESSAGE, 0, "message kind must be 0 per ABI spec");
    }

    #[test]
    fn recv_kind_notification_constant_is_one() {
        assert_eq!(
            RECV_KIND_NOTIFICATION, 1,
            "notification kind must be 1 per ABI spec"
        );
    }

    #[test]
    fn zero_label_message_round_trips() {
        let wake = WakeKind::Message(0);
        let (kind, msg) = encode_wake_kind(wake);
        assert_eq!(kind, RECV_KIND_MESSAGE);
        assert_eq!(decode_wake_kind(kind, msg), WakeKind::Message(0));
    }

    #[test]
    fn zero_bits_notification_round_trips() {
        let wake = WakeKind::Notification(0);
        let (kind, msg) = encode_wake_kind(wake);
        assert_eq!(kind, RECV_KIND_NOTIFICATION);
        assert_eq!(decode_wake_kind(kind, msg), WakeKind::Notification(0));
    }

    #[test]
    fn all_ones_label_round_trips() {
        let label = u64::MAX;
        let wake = WakeKind::Message(label);
        let (kind, msg) = encode_wake_kind(wake);
        assert_eq!(kind, RECV_KIND_MESSAGE);
        assert_eq!(decode_wake_kind(kind, msg), WakeKind::Message(label));
    }

    #[test]
    fn all_ones_bits_round_trips() {
        let bits = u64::MAX;
        let wake = WakeKind::Notification(bits);
        let (kind, msg) = encode_wake_kind(wake);
        assert_eq!(kind, RECV_KIND_NOTIFICATION);
        assert_eq!(decode_wake_kind(kind, msg), WakeKind::Notification(bits));
    }

    #[test]
    fn unknown_kind_byte_falls_back_to_message() {
        // An unknown kind tag (e.g. from a future ABI extension) must not panic.
        let msg = Message::new(99);
        let decoded = decode_wake_kind(0xff, msg);
        assert_eq!(decoded, WakeKind::Message(99));
    }

    #[test]
    fn mixed_interleaving_message_then_notification() {
        let m = WakeKind::Message(0xabcd);
        let n = WakeKind::Notification(0x0f);
        let (km, mm) = encode_wake_kind(m);
        let (kn, mn) = encode_wake_kind(n);
        assert_ne!(
            km, kn,
            "message and notification must have different kind tags"
        );
        assert_eq!(decode_wake_kind(km, mm), m);
        assert_eq!(decode_wake_kind(kn, mn), n);
    }

    // --- classify_recv (Track B shared seam) ---

    #[test]
    fn classify_recv_nonzero_bits_returns_notification() {
        assert_eq!(classify_recv(1), RECV_KIND_NOTIFICATION);
        assert_eq!(classify_recv(u64::MAX), RECV_KIND_NOTIFICATION);
        assert_eq!(classify_recv(0b1010_0101), RECV_KIND_NOTIFICATION);
    }

    #[test]
    fn classify_recv_zero_bits_returns_message() {
        assert_eq!(classify_recv(0), RECV_KIND_MESSAGE);
    }

    #[test]
    fn classify_recv_matches_encode_wake_kind_notification_path() {
        let bits: u64 = 0xdead_beef;
        let (kind, _) = encode_wake_kind(WakeKind::Notification(bits));
        assert_eq!(classify_recv(bits), kind);
    }
}
