//! Phase 56 display-service protocol surfaces.
//!
//! [`protocol`] is the single source of truth for the wire format used
//! between `display_server`, graphical clients (A.3), the `m3ctl` control
//! socket (A.8), and kernel-side input-event plumbing (A.4). Declaring
//! every message type, opcode, and byte layout here — once — is the
//! Phase 56 DRY discipline: no parallel definitions across `display_server`
//! and each client library.

pub mod buffer;
pub mod compose;
pub mod control;
pub mod cursor;
pub mod fb_owner;
pub mod frame_tick;
pub mod layer;
pub mod layout;
pub mod protocol;
pub mod stats;
pub mod surface;
