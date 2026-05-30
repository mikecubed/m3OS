//! xHCI (eXtensible Host Controller Interface) pure-logic foundation
//! (Phase 78a).
//!
//! This module collects the format-level definitions for an xHCI 1.2b host
//! controller that have *no* hardware dependency and can therefore be exercised
//! on the host:
//!
//! * [`regs`] — Capability-register decoders ([`Hcsparams1`](regs::Hcsparams1),
//!   [`Hcsparams2`](regs::Hcsparams2), [`Hccparams1`](regs::Hccparams1)) plus the
//!   `CAPLENGTH`/`HCIVERSION`/`DBOFF`/`RTSOFF` field extractors and the
//!   operational/runtime/doorbell base-offset helpers.
//! * [`trb`] — the 16-byte Transfer Request Block layout, type/cycle decoders,
//!   command + event encoders/decoders, the Device Context Index (DCI) formula,
//!   and the producer/consumer cycle-bit state machines for the command ring
//!   and the event ring.
//! * [`context`] — Slot/Endpoint/Input context offset math for both the 32-byte
//!   and 64-byte (CSZ=1) context layouts, plus Input Control Context add/drop
//!   flag helpers and Slot Context field encoders.
//! * [`port`] — PORTSC register field accessors, RW1C-safe write helpers, and
//!   the protocol-speed-ID → port-speed → EP0 max-packet-size mapping.
//!
//! Nothing here touches MMIO or DMA; the in-kernel / ring-3 driver layers
//! (Phase 78b/78c) sit on top of these primitives.

pub mod context;
pub mod port;
pub mod regs;
pub mod trb;

pub use context::{ADD_FLAG_EP0, ADD_FLAG_SLOT, context_entry_size};
pub use port::{PortSpeed, Portsc, ep0_max_packet_for_speed, port_speed_from_psi};
pub use regs::{Hccparams1, Hcsparams1, Hcsparams2};
pub use trb::{
    EventConsumer, ProducerRing, TRB_SIZE, Trb, TrbType, dci, event_trb_type, trb_cycle,
    trb_type_raw,
};
