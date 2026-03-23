//! IPC message type.
// Some constructors remain unused; keep dead-code allowance until all paths are exercised.
#![allow(dead_code)]
//!
//! A [`Message`] is the unit of information transferred in a single IPC
//! rendezvous.  It is designed to fit entirely in CPU registers: one word for
//! the label (method ID) and four words of inline data.
//!
//! # Design rationale
//!
//! Synchronous rendezvous IPC copies the message directly through the kernel —
//! no intermediate buffer, no allocation on the hot path.  Keeping the message
//! small (≤ 5 × u64 = 40 bytes) means the transfer fits in callee-saved
//! registers and avoids touching the heap at all.
//!
//! For large data transfers (framebuffer blocks, file reads) the intended
//! pattern is a shared-memory page grant (Phase 7+), with IPC carrying only
//! the control word signalling "data ready".

/// A small, register-sized IPC message.
///
/// `label` identifies the operation (similar to a method selector in Mach or
/// a message tag in seL4).  `data` carries up to four 64-bit words of inline
/// payload — enough for most control operations.
///
/// Capability grants are deferred to Phase 7+.  For now, if a server needs to
/// share memory with a client it must use a pre-arranged shared address.
#[derive(Debug, Clone, Copy, Default)]
pub struct Message {
    /// Operation identifier, chosen by convention between sender and receiver.
    pub label: u64,
    /// Inline data payload — up to 4 machine words.
    pub data: [u64; 4],
}

impl Message {
    /// Construct a label-only message (data fields zeroed).
    pub const fn new(label: u64) -> Self {
        Message {
            label,
            data: [0; 4],
        }
    }

    /// Construct a message with one data word.
    pub const fn with1(label: u64, d0: u64) -> Self {
        Message {
            label,
            data: [d0, 0, 0, 0],
        }
    }

    /// Construct a message with two data words.
    pub const fn with2(label: u64, d0: u64, d1: u64) -> Self {
        Message {
            label,
            data: [d0, d1, 0, 0],
        }
    }
}
