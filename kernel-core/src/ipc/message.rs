use super::capability::{CapHandle, Capability};

/// Maximum number of capability handles transferred in a single IPC message
/// via the Phase 74 `cap_slots` field. Sized at 2 so a typical server reply
/// can return both a fresh endpoint capability and a related notification
/// capability without forcing a second `sys_cap_grant` round-trip.
pub const CAP_SLOTS_PER_MSG: usize = 2;

/// A small, register-sized IPC message.
///
/// Phase 74 adds the [`Message::cap_slots`] / [`Message::n_caps`] fields for
/// inline capability grants on the rendezvous path. `n_caps = 0` (the
/// default) reproduces the pre-Phase-74 wire form exactly — no caller
/// already on the simple `sys_ipc_call` path observes any behaviour change.
#[derive(Debug, Clone, Copy, Default)]
pub struct Message {
    /// Operation identifier, chosen by convention between sender and receiver.
    pub label: u64,
    /// Inline data payload — up to 4 machine words.
    pub data: [u64; 4],
    /// Optional capability transferred with this message (kernel-internal
    /// fast path; preserved for Phase 6 / Phase 50 callers).
    pub cap: Option<Capability>,
    /// Phase 74: capability handles the sender hands to the receiver as part
    /// of this IPC. Valid entries are `cap_slots[..n_caps as usize]`. The
    /// remaining slots are ignored.
    pub cap_slots: [CapHandle; CAP_SLOTS_PER_MSG],
    /// Phase 74: count of valid entries in [`Message::cap_slots`]. `0`
    /// (the default) preserves pre-Phase-74 semantics — no capability
    /// transfer happens at delivery time.
    pub n_caps: u8,
}

impl Message {
    /// Construct a label-only message (data fields zeroed, no capability).
    pub const fn new(label: u64) -> Self {
        Message {
            label,
            data: [0; 4],
            cap: None,
            cap_slots: [0; CAP_SLOTS_PER_MSG],
            n_caps: 0,
        }
    }

    /// Construct a message with one data word.
    pub const fn with1(label: u64, d0: u64) -> Self {
        Message {
            label,
            data: [d0, 0, 0, 0],
            cap: None,
            cap_slots: [0; CAP_SLOTS_PER_MSG],
            n_caps: 0,
        }
    }

    /// Construct a message with two data words.
    pub const fn with2(label: u64, d0: u64, d1: u64) -> Self {
        Message {
            label,
            data: [d0, d1, 0, 0],
            cap: None,
            cap_slots: [0; CAP_SLOTS_PER_MSG],
            n_caps: 0,
        }
    }

    /// Attach a capability to this message.
    pub const fn with_cap(mut self, cap: Capability) -> Self {
        self.cap = Some(cap);
        self
    }

    /// Encode the receiver's reply capability handle into the reserved IPC
    /// metadata word.
    ///
    /// `data[3]` is reserved by call-shaped deliveries so userspace servers can
    /// reply through the exact one-shot cap the kernel inserted. Protocol data
    /// must not use this word for request payloads.
    pub const fn with_reply_cap_handle(mut self, handle: u32) -> Self {
        self.data[3] = handle as u64;
        self
    }

    /// Phase 74: attach up to [`CAP_SLOTS_PER_MSG`] capability handles to
    /// this message. `handles.len()` must not exceed [`CAP_SLOTS_PER_MSG`];
    /// excess entries are silently truncated.
    pub fn with_cap_slots(mut self, handles: &[CapHandle]) -> Self {
        let n = core::cmp::min(handles.len(), CAP_SLOTS_PER_MSG);
        for (i, h) in handles.iter().take(n).enumerate() {
            self.cap_slots[i] = *h;
        }
        self.n_caps = n as u8;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_label_only() {
        let msg = Message::new(42);
        assert_eq!(msg.label, 42);
        assert_eq!(msg.data, [0; 4]);
    }

    #[test]
    fn with1_data() {
        let msg = Message::with1(1, 100);
        assert_eq!(msg.label, 1);
        assert_eq!(msg.data[0], 100);
        assert_eq!(msg.data[1], 0);
    }

    #[test]
    fn with2_data() {
        let msg = Message::with2(7, 10, 20);
        assert_eq!(msg.label, 7);
        assert_eq!(msg.data[0], 10);
        assert_eq!(msg.data[1], 20);
        assert_eq!(msg.data[2], 0);
        assert_eq!(msg.data[3], 0);
    }

    #[test]
    fn default_is_zeroed() {
        let msg = Message::default();
        assert_eq!(msg.label, 0);
        assert_eq!(msg.data, [0; 4]);
        assert!(msg.cap.is_none());
    }

    #[test]
    fn with_cap_attaches_capability() {
        use crate::types::EndpointId;
        let cap = Capability::Endpoint(EndpointId(5));
        let msg = Message::new(42).with_cap(cap);
        assert_eq!(msg.label, 42);
        assert_eq!(msg.cap, Some(cap));
    }

    #[test]
    fn constructors_have_no_cap() {
        let m1 = Message::new(1);
        let m2 = Message::with1(2, 10);
        let m3 = Message::with2(3, 10, 20);
        assert!(m1.cap.is_none());
        assert!(m2.cap.is_none());
        assert!(m3.cap.is_none());
    }

    #[test]
    fn with_reply_cap_handle_sets_data3() {
        let msg = Message::with2(7, 10, 20).with_reply_cap_handle(5);

        assert_eq!(msg.data, [10, 20, 0, 5]);
    }

    #[test]
    fn default_message_has_zero_cap_slots() {
        let msg = Message::new(42);
        assert_eq!(msg.n_caps, 0);
        assert_eq!(msg.cap_slots, [0; CAP_SLOTS_PER_MSG]);
    }

    #[test]
    fn with_cap_slots_records_handles_and_count() {
        let msg = Message::new(1).with_cap_slots(&[7, 13]);
        assert_eq!(msg.n_caps, 2);
        assert_eq!(msg.cap_slots, [7, 13]);
    }

    #[test]
    fn with_cap_slots_truncates_excess_handles() {
        let msg = Message::new(1).with_cap_slots(&[7, 13, 19, 23]);
        assert_eq!(msg.n_caps as usize, CAP_SLOTS_PER_MSG);
        assert_eq!(msg.cap_slots[0], 7);
        assert_eq!(msg.cap_slots[1], 13);
    }

    #[test]
    fn with_cap_slots_partial_fill_keeps_count() {
        let msg = Message::new(1).with_cap_slots(&[42]);
        assert_eq!(msg.n_caps, 1);
        assert_eq!(msg.cap_slots[0], 42);
        assert_eq!(msg.cap_slots[1], 0);
    }

    // Phase 74 Track A.3 acceptance — a host-side unit test exercising the
    // userspace `IpcMessage` wire format round-trip.
    //
    // The serialization format used by `kernel::ipc::read_cap_msg_from_user`
    // is:
    //   off  0..8   : label
    //   off  8..40  : data[0..4] (4 × u64)
    //   off 40..48  : cap_slots  (2 × u32)
    //   off 48      : n_caps     (u8)
    //   off 49..56  : padding
    //
    // This test rebuilds the layout in raw bytes and verifies the round
    // trip without pulling in kernel/userspace dependencies.
    #[test]
    fn cap_msg_wire_roundtrip() {
        const WIRE_LEN: usize = 56;
        let msg = Message::new(0xDEADBEEF).with_cap_slots(&[7, 13]);

        let mut wire = [0u8; WIRE_LEN];
        wire[0..8].copy_from_slice(&msg.label.to_ne_bytes());
        for (i, d) in msg.data.iter().enumerate() {
            let off = 8 + i * 8;
            wire[off..off + 8].copy_from_slice(&d.to_ne_bytes());
        }
        for (i, h) in msg.cap_slots.iter().enumerate() {
            let off = 40 + i * 4;
            wire[off..off + 4].copy_from_slice(&h.to_ne_bytes());
        }
        wire[48] = msg.n_caps;

        // Decode.
        let label = u64::from_ne_bytes(wire[0..8].try_into().unwrap());
        let mut data = [0u64; 4];
        for (i, d) in data.iter_mut().enumerate() {
            let off = 8 + i * 8;
            *d = u64::from_ne_bytes(wire[off..off + 8].try_into().unwrap());
        }
        let mut cap_slots = [0u32; CAP_SLOTS_PER_MSG];
        for (i, slot) in cap_slots.iter_mut().enumerate() {
            let off = 40 + i * 4;
            *slot = u32::from_ne_bytes(wire[off..off + 4].try_into().unwrap());
        }
        let n_caps = wire[48];

        assert_eq!(label, 0xDEADBEEF);
        assert_eq!(data, msg.data);
        assert_eq!(cap_slots, msg.cap_slots);
        assert_eq!(n_caps, msg.n_caps);
    }
}
