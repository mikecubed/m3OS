//! Compositor clipboard store (Phase 105 Track B.2).
//!
//! `display_server` is the only process that can broker a clipboard —
//! clients share no writable memory, so a copy is a `SetClipboard` offer
//! stored here, and a paste is a `RequestClipboard` answered from it. The
//! store is pure logic (a bounded byte buffer + its MIME tag + the owning
//! client token) so its cap/clear/ownership rules are host-tested without
//! the compositor.

use alloc::vec::Vec;

use crate::display::protocol::MimeTag;

/// Maximum clipboard offer size. A larger `SetClipboard` is rejected
/// (the offer is left unchanged), never silently truncated — a paste must
/// return exactly what was copied or nothing.
pub const MAX_CLIPBOARD_BYTES: usize = 64 * 1024;

/// The last clipboard offer, or empty.
#[derive(Debug, Clone, Default)]
pub struct ClipboardStore {
    tag: MimeTag,
    data: Vec<u8>,
    /// The `client_token` that published the current offer, so a
    /// departing client's offer can be dropped on `Goodbye`.
    owner: Option<u32>,
}

impl ClipboardStore {
    pub fn new() -> ClipboardStore {
        ClipboardStore::default()
    }

    /// Whether an offer is currently held.
    pub fn has_offer(&self) -> bool {
        !self.data.is_empty()
    }

    /// The current offer's MIME tag.
    pub fn tag(&self) -> MimeTag {
        self.tag
    }

    /// The current offer's bytes (empty when there is no offer).
    pub fn bytes(&self) -> &[u8] {
        &self.data
    }

    /// Store a new offer from `owner`. Returns `false` (and leaves the
    /// existing offer untouched) if `bytes` exceeds [`MAX_CLIPBOARD_BYTES`].
    /// A zero-length offer clears the clipboard.
    pub fn set(&mut self, tag: MimeTag, bytes: &[u8], owner: u32) -> bool {
        if bytes.len() > MAX_CLIPBOARD_BYTES {
            return false;
        }
        if bytes.is_empty() {
            self.clear();
            return true;
        }
        self.tag = tag;
        self.data.clear();
        self.data.extend_from_slice(bytes);
        self.owner = Some(owner);
        true
    }

    /// Drop the offer and free its buffer.
    pub fn clear(&mut self) {
        self.tag = MimeTag::default();
        self.data = Vec::new();
        self.owner = None;
    }

    /// Drop the offer only if it was published by `owner` (called on that
    /// client's `Goodbye`). Returns whether an offer was dropped.
    pub fn clear_owned_by(&mut self, owner: u32) -> bool {
        if self.owner == Some(owner) && self.has_offer() {
            self.clear();
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_and_read_back() {
        let mut c = ClipboardStore::new();
        assert!(!c.has_offer());
        assert!(c.set(MimeTag::TextPlainUtf8, b"hello", 7));
        assert!(c.has_offer());
        assert_eq!(c.bytes(), b"hello");
        assert_eq!(c.tag(), MimeTag::TextPlainUtf8);
    }

    #[test]
    fn oversize_offer_is_rejected_not_truncated() {
        let mut c = ClipboardStore::new();
        c.set(MimeTag::TextPlainUtf8, b"keep", 1);
        let big = alloc::vec![0u8; MAX_CLIPBOARD_BYTES + 1];
        assert!(!c.set(MimeTag::TextPlainUtf8, &big, 2));
        // Previous offer survives.
        assert_eq!(c.bytes(), b"keep");
    }

    #[test]
    fn max_size_offer_is_accepted() {
        let mut c = ClipboardStore::new();
        let exact = alloc::vec![0xABu8; MAX_CLIPBOARD_BYTES];
        assert!(c.set(MimeTag::TextPlainUtf8, &exact, 1));
        assert_eq!(c.bytes().len(), MAX_CLIPBOARD_BYTES);
    }

    #[test]
    fn zero_length_offer_clears() {
        let mut c = ClipboardStore::new();
        c.set(MimeTag::TextPlainUtf8, b"data", 1);
        assert!(c.set(MimeTag::TextPlainUtf8, b"", 1));
        assert!(!c.has_offer());
        assert_eq!(c.bytes(), b"");
    }

    #[test]
    fn goodbye_drops_only_the_owners_offer() {
        let mut c = ClipboardStore::new();
        c.set(MimeTag::TextPlainUtf8, b"mine", 42);
        // A different client's Goodbye leaves the offer.
        assert!(!c.clear_owned_by(7));
        assert!(c.has_offer());
        // The owner's Goodbye drops it.
        assert!(c.clear_owned_by(42));
        assert!(!c.has_offer());
        // Re-dropping is a no-op.
        assert!(!c.clear_owned_by(42));
    }
}
