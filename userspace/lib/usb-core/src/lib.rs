//! `usb-core` — host↔class IPC protocol for USB class drivers (Phase 78b).
//!
//! This crate provides the typed IPC protocol layer between the xHCI host
//! server (Phase 78b/78c) and ring-3 USB class drivers (HID keyboard, hub,
//! etc.). It re-exports the descriptor and enumeration types from `kernel-core`
//! so class drivers need only depend on this single crate.
//!
//! # Design constraints
//!
//! * **No `std`** (unless running tests): the crate targets ring-3 `no_std`
//!   daemons that use only `alloc`.
//! * **Page-grant model**: large data transfers (e.g. HID report buffers)
//!   cross IPC as Phase 74 **page-capability grants** — a capability handle
//!   plus a byte length — rather than inline byte payloads, which would blow
//!   the IPC message-size budget. Only small setup packets and descriptor bytes
//!   travel inline.
//! * **Static class daemons**: class drivers are long-lived daemons. The xHCI
//!   server locates them by well-known endpoint name and sends an
//!   [`AttachNotice`] when a matching device arrives. The daemon then calls
//!   back through [`UsbClient`] to configure the device and submit transfers.
//! * **No live IPC send/recv in this crate**: this crate defines the types and
//!   client surface; the actual syscall plumbing lives in the xHCI server
//!   (Phase 78b Track B). Only the message-construction logic is host-tested
//!   here.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

// Re-export the pure-logic USB types that class drivers need.
pub use kernel_core::usb::descriptor::{
    CLASS_HID, CLASS_HUB, ConfigDescriptor, DESC_TYPE_CONFIGURATION, DESC_TYPE_DEVICE,
    DESC_TYPE_ENDPOINT, DESC_TYPE_HID, DESC_TYPE_INTERFACE, DeviceDescriptor, EndpointDescriptor,
    HidDescriptor, InterfaceDescriptor, PROTOCOL_HID_KEYBOARD, PROTOCOL_HID_MOUSE, ParsedConfig,
    ParsedInterface, SUBCLASS_HID_BOOT, TRANSFER_TYPE_INTERRUPT, parse_config_tree,
};
pub use kernel_core::usb::enumerate::{
    EnumContext, EnumState, InputContextSnapshot, UsbHostOps, run_enumeration,
};
pub use kernel_core::usb::xhci::port::{
    EP0_MPS_HIGH, EP0_MPS_LOW_FULL, EP0_MPS_SUPER, PortSpeed, ep0_max_packet_for_speed,
};

pub mod protocol;
