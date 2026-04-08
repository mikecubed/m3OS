use super::capability::Capability;

/// A small, register-sized IPC message.
#[derive(Debug, Clone, Copy, Default)]
pub struct Message {
    /// Operation identifier, chosen by convention between sender and receiver.
    pub label: u64,
    /// Inline data payload — up to 4 machine words.
    pub data: [u64; 4],
    /// Optional capability transferred with this message.
    pub cap: Option<Capability>,
}

impl Message {
    /// Construct a label-only message (data fields zeroed, no capability).
    pub const fn new(label: u64) -> Self {
        Message {
            label,
            data: [0; 4],
            cap: None,
        }
    }

    /// Construct a message with one data word.
    pub const fn with1(label: u64, d0: u64) -> Self {
        Message {
            label,
            data: [d0, 0, 0, 0],
            cap: None,
        }
    }

    /// Construct a message with two data words.
    pub const fn with2(label: u64, d0: u64, d1: u64) -> Self {
        Message {
            label,
            data: [d0, d1, 0, 0],
            cap: None,
        }
    }

    /// Attach a capability to this message.
    pub const fn with_cap(mut self, cap: Capability) -> Self {
        self.cap = Some(cap);
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
}
