//! USB device enumeration state machine (xHCI-hosted, xHCI 1.2b §4.3 + USB 2.0 §9).
//!
//! # Overview
//!
//! xHCI replaces the classic USB SET_ADDRESS request with the **Address Device**
//! command, and mandates a two-step EP0 Max Packet Size negotiation for Full/Low
//! speed devices:
//!
//! 1. **Enable Slot** → receive a Slot ID.
//! 2. **Address Device (BSR=1)** — "default state" mode: the controller programmes
//!    the slot with an initial EP0 context but **does not** assign a USB address
//!    yet, letting the host read the first 8 bytes of the Device Descriptor to
//!    learn the true `bMaxPacketSize0`.
//! 3. **Evaluate Context** — update the EP0 context with the correct MPS.
//! 4. **Address Device (BSR=0)** — the controller now assigns the USB address.
//! 5. **Get Device Descriptor** (full 18-byte read via GET_DESCRIPTOR control transfer).
//! 6. **Get Config Short** (9-byte read → learn `wTotalLength`).
//! 7. **Get Config Full** (`wTotalLength`-byte read → full configuration blob).
//! 8. **Set Configuration** (SET_CONFIGURATION to `bConfigurationValue`).
//! 9. **Configure Endpoint** → activate the interface endpoints.
//! 10. **Configured** — device ready for class-driver use.
//!
//! The machine also handles **Error** and **Timeout** terminal states.
//!
//! # Testing
//!
//! [`EnumState`] is driven via the [`UsbHostOps`] trait, which the test suite
//! implements with a mock that injects canned completions and descriptor blobs.
//! No MMIO or DMA is involved; the entire machine is host-testable.

extern crate alloc;

use alloc::vec::Vec;

use crate::usb::descriptor::{ConfigDescriptor, DeviceDescriptor, ParsedConfig, parse_config_tree};
use crate::usb::xhci::context::{
    EP_CERR_3, EP_TYPE_CONTROL, add_flags, ep_context_dword1, ep_tr_dequeue_ptr,
    slot_context_dword0, slot_context_dword1,
};
use crate::usb::xhci::port::{PortSpeed, ep0_max_packet_for_speed};
use crate::usb::xhci::trb::COMPLETION_SUCCESS;

// ---------------------------------------------------------------------------
// Completion codes (re-export the subset we inspect)
// ---------------------------------------------------------------------------

/// Completion code returned by the host-ops mock / real controller for success.
pub use crate::usb::xhci::trb::COMPLETION_SUCCESS as COMPLETE_OK;

// ---------------------------------------------------------------------------
// Enumeration state
// ---------------------------------------------------------------------------

/// States of the USB device enumeration state machine.
///
/// The machine advances forward on success and transitions to [`EnumState::Error`]
/// or [`EnumState::Timeout`] on failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnumState {
    /// Initial state: issue an Enable Slot command to obtain a Slot ID.
    EnableSlot,
    /// Issue Address Device (BSR=1) to enter default-state without assigning
    /// a USB address, so we can read the first 8 bytes of the Device
    /// Descriptor and learn the true EP0 `bMaxPacketSize0`.
    AddressDeviceBsr,
    /// Issue Evaluate Context to update the EP0 Max Packet Size using the
    /// `bMaxPacketSize0` value read during [`AddressDeviceBsr`].
    ///
    /// [`AddressDeviceBsr`]: EnumState::AddressDeviceBsr
    EvaluateContext,
    /// Issue Address Device (BSR=0) to assign the USB address.
    AddressDevice,
    /// Issue a GET_DESCRIPTOR(Device) control transfer to read all 18 bytes.
    GetDeviceDescriptor,
    /// Issue a short GET_DESCRIPTOR(Configuration, 0) to learn `wTotalLength`.
    GetConfigShort,
    /// Issue the full GET_DESCRIPTOR(Configuration, 0) of `wTotalLength` bytes.
    GetConfigFull,
    /// Issue SET_CONFIGURATION to activate the first configuration.
    SetConfiguration,
    /// Issue a Configure Endpoint command to activate the interface endpoints.
    ConfigureEndpoint,
    /// Terminal success state: device is fully configured and ready for use.
    Configured,
    /// Terminal error state: a command or transfer completed with a non-success
    /// completion code.
    Error {
        /// The xHCI completion code that triggered the error.
        code: u8,
    },
    /// Terminal error state: a command or transfer timed out.
    Timeout,
}

// ---------------------------------------------------------------------------
// Input context representation for tests
// ---------------------------------------------------------------------------

/// Snapshot of the Input Context fields the enumeration machine programmes
/// during Address Device and Evaluate Context commands.
///
/// In production code this would be a DMA buffer; here it is a simple struct
/// for host-test assertion.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InputContextSnapshot {
    /// Add Flags dword (A0..A31) from the Input Control Context.
    pub add_flags: u32,
    /// Slot Context dword 0 (route string, speed, context entries).
    pub slot_dword0: u32,
    /// Slot Context dword 1 (root hub port number).
    pub slot_dword1: u32,
    /// EP0 Context dword 1 (EP type, CErr, Max Packet Size).
    pub ep0_dword1: u32,
    /// EP0 TR Dequeue Pointer (ring IOVA | DCS).
    pub ep0_dequeue_ptr: u64,
}

// ---------------------------------------------------------------------------
// Host operations trait (abstraction for TDD mock + production impl)
// ---------------------------------------------------------------------------

/// The interface the enumeration machine uses to issue xHCI commands and
/// control transfers. Production code provides an implementation backed by
/// real DMA rings; tests provide a mock.
pub trait UsbHostOps {
    /// Issue an Enable Slot command. Returns the allocated Slot ID on success.
    fn enable_slot(&mut self) -> Result<u8, u8>;

    /// Issue an Address Device command (BSR controlled by `bsr`).
    ///
    /// `ctx` describes the Input Context to be programmed. The implementation
    /// records the context for inspection in tests. Returns the completion code
    /// (`COMPLETION_SUCCESS` = 1 on success).
    fn address_device(&mut self, slot_id: u8, ctx: &InputContextSnapshot, bsr: bool) -> u8;

    /// Issue an Evaluate Context command to update the EP0 MPS.
    ///
    /// `ctx` is the updated Input Context. Returns the completion code.
    fn evaluate_context(&mut self, slot_id: u8, ctx: &InputContextSnapshot) -> u8;

    /// Perform a GET_DESCRIPTOR(Device) control IN transfer.
    ///
    /// Returns the (up to 18) raw descriptor bytes, or `None` on timeout.
    fn get_device_descriptor(&mut self, slot_id: u8) -> Option<Vec<u8>>;

    /// Perform a short GET_DESCRIPTOR(Configuration, 0) for `len` bytes.
    ///
    /// Returns the raw bytes (at least 9), or `None` on timeout.
    fn get_config_short(&mut self, slot_id: u8, len: u16) -> Option<Vec<u8>>;

    /// Perform a full GET_DESCRIPTOR(Configuration, 0) for `len` bytes.
    ///
    /// Returns the raw bytes, or `None` on timeout.
    fn get_config_full(&mut self, slot_id: u8, len: u16) -> Option<Vec<u8>>;

    /// Perform a SET_CONFIGURATION control OUT transfer.
    ///
    /// Returns the completion code.
    fn set_configuration(&mut self, slot_id: u8, value: u8) -> u8;

    /// Issue a Configure Endpoint command.
    ///
    /// `ctx` describes the updated Input Context (Slot + all new endpoints).
    /// Returns the completion code.
    fn configure_endpoint(&mut self, slot_id: u8, ctx: &InputContextSnapshot) -> u8;
}

// ---------------------------------------------------------------------------
// Enumeration context (live state threaded through the machine)
// ---------------------------------------------------------------------------

/// Live context accumulated during enumeration.
#[derive(Debug, Clone, Default)]
pub struct EnumContext {
    /// The xHCI Slot ID assigned by Enable Slot.
    pub slot_id: u8,
    /// The port speed (determines initial EP0 MPS).
    pub speed: Option<PortSpeed>,
    /// EP0 Max Packet Size learned from `bMaxPacketSize0` (after the BSR read).
    pub ep0_mps: u16,
    /// The full Device Descriptor, once read.
    pub device_descriptor: Option<DeviceDescriptor>,
    /// The parsed configuration tree, once read.
    pub parsed_config: Option<ParsedConfig>,
    /// The fake IOVA of the EP0 transfer ring (supplied by the caller / mock).
    pub ep0_ring_iova: u64,
    /// The root-hub port number (1-based).
    pub port: u8,
}

// ---------------------------------------------------------------------------
// State machine driver
// ---------------------------------------------------------------------------

/// Build the initial Input Context snapshot for Address Device (BSR=0 or
/// BSR=1, the same context shape for both). Slot Context and EP0 context.
fn build_address_device_ctx(ctx: &EnumContext) -> InputContextSnapshot {
    let speed_psi = match ctx.speed {
        Some(PortSpeed::Full) => crate::usb::xhci::port::PSI_FULL_SPEED,
        Some(PortSpeed::Low) => crate::usb::xhci::port::PSI_LOW_SPEED,
        Some(PortSpeed::High) => crate::usb::xhci::port::PSI_HIGH_SPEED,
        Some(PortSpeed::Super) => crate::usb::xhci::port::PSI_SUPER_SPEED,
        None => crate::usb::xhci::port::PSI_FULL_SPEED,
    };
    let slot_dw0 = slot_context_dword0(0, speed_psi, 1); // context_entries = 1 (EP0 only)
    let slot_dw1 = slot_context_dword1(ctx.port);
    let ep0_dw1 = ep_context_dword1(EP_TYPE_CONTROL, EP_CERR_3, ctx.ep0_mps);
    let ep0_ptr = ep_tr_dequeue_ptr(ctx.ep0_ring_iova);
    InputContextSnapshot {
        add_flags: add_flags(&[0, 1]), // A0 (Slot) + A1 (EP0)
        slot_dword0: slot_dw0,
        slot_dword1: slot_dw1,
        ep0_dword1: ep0_dw1,
        ep0_dequeue_ptr: ep0_ptr,
    }
}

/// Run the enumeration state machine to completion (or error/timeout).
///
/// `state` is the initial state; `ctx` carries accumulated context; `ops`
/// is the host-operations implementation. Returns the terminal [`EnumState`]
/// and the final [`EnumContext`].
pub fn run_enumeration(
    mut state: EnumState,
    mut ctx: EnumContext,
    ops: &mut dyn UsbHostOps,
) -> (EnumState, EnumContext) {
    loop {
        match state {
            EnumState::EnableSlot => {
                match ops.enable_slot() {
                    Ok(slot_id) => {
                        ctx.slot_id = slot_id;
                        // Set initial EP0 MPS from port speed.
                        let speed = ctx.speed.unwrap_or(PortSpeed::Full);
                        ctx.ep0_mps = ep0_max_packet_for_speed(speed);
                        state = EnumState::AddressDeviceBsr;
                    }
                    Err(code) => {
                        state = EnumState::Error { code };
                    }
                }
            }

            EnumState::AddressDeviceBsr => {
                let input_ctx = build_address_device_ctx(&ctx);
                let cc = ops.address_device(ctx.slot_id, &input_ctx, true);
                if cc == COMPLETION_SUCCESS {
                    state = EnumState::EvaluateContext;
                } else {
                    state = EnumState::Error { code: cc };
                }
            }

            EnumState::EvaluateContext => {
                // We need the first 8 bytes of the Device Descriptor to learn
                // bMaxPacketSize0. We piggyback a short GET_DESCRIPTOR here
                // (before the full Address Device) to update MPS.
                let bytes = match ops.get_device_descriptor(ctx.slot_id) {
                    Some(b) => b,
                    None => {
                        state = EnumState::Timeout;
                        continue;
                    }
                };
                // Parse bMaxPacketSize0 (byte 7 of the Device Descriptor).
                if bytes.len() >= 8 {
                    let raw_mps = bytes[7];
                    // For SuperSpeed bMaxPacketSize0 is the exponent; for
                    // others it is the raw byte count.
                    ctx.ep0_mps = match ctx.speed {
                        Some(PortSpeed::Super) => 1u16 << raw_mps,
                        _ => raw_mps as u16,
                    };
                }
                let input_ctx = build_address_device_ctx(&ctx);
                let cc = ops.evaluate_context(ctx.slot_id, &input_ctx);
                if cc == COMPLETION_SUCCESS {
                    state = EnumState::AddressDevice;
                } else {
                    state = EnumState::Error { code: cc };
                }
            }

            EnumState::AddressDevice => {
                let input_ctx = build_address_device_ctx(&ctx);
                let cc = ops.address_device(ctx.slot_id, &input_ctx, false);
                if cc == COMPLETION_SUCCESS {
                    state = EnumState::GetDeviceDescriptor;
                } else {
                    state = EnumState::Error { code: cc };
                }
            }

            EnumState::GetDeviceDescriptor => {
                let bytes = match ops.get_device_descriptor(ctx.slot_id) {
                    Some(b) => b,
                    None => {
                        state = EnumState::Timeout;
                        continue;
                    }
                };
                ctx.device_descriptor = DeviceDescriptor::parse(&bytes);
                state = EnumState::GetConfigShort;
            }

            EnumState::GetConfigShort => {
                // Short read: 9 bytes to learn wTotalLength.
                let bytes = match ops.get_config_short(ctx.slot_id, 9) {
                    Some(b) => b,
                    None => {
                        state = EnumState::Timeout;
                        continue;
                    }
                };
                if let Some(cfg_hdr) = ConfigDescriptor::parse(&bytes) {
                    let total = cfg_hdr.w_total_length;
                    state = EnumState::GetConfigFull;
                    // Store total length for the next step via a temporary parse.
                    // We re-parse from the short read's w_total_length.
                    ctx.parsed_config = Some(ParsedConfig {
                        config: cfg_hdr,
                        interfaces: alloc::vec![],
                    });
                    let _ = total; // used implicitly below via ctx.parsed_config
                } else {
                    state = EnumState::Error { code: 0xFF };
                }
            }

            EnumState::GetConfigFull => {
                let total = ctx
                    .parsed_config
                    .as_ref()
                    .map(|c| c.config.w_total_length)
                    .unwrap_or(9);
                let bytes = match ops.get_config_full(ctx.slot_id, total) {
                    Some(b) => b,
                    None => {
                        state = EnumState::Timeout;
                        continue;
                    }
                };
                ctx.parsed_config = parse_config_tree(&bytes);
                state = EnumState::SetConfiguration;
            }

            EnumState::SetConfiguration => {
                let cfg_value = ctx
                    .parsed_config
                    .as_ref()
                    .map(|c| c.config.b_configuration_value)
                    .unwrap_or(1);
                let cc = ops.set_configuration(ctx.slot_id, cfg_value);
                if cc == COMPLETION_SUCCESS {
                    state = EnumState::ConfigureEndpoint;
                } else {
                    state = EnumState::Error { code: cc };
                }
            }

            EnumState::ConfigureEndpoint => {
                // Build a context that includes the newly-configured endpoints.
                // For simplicity the snapshot uses the same slot+EP0 shape;
                // the production driver would add each endpoint here.
                let input_ctx = build_address_device_ctx(&ctx);
                let cc = ops.configure_endpoint(ctx.slot_id, &input_ctx);
                if cc == COMPLETION_SUCCESS {
                    state = EnumState::Configured;
                } else {
                    state = EnumState::Error { code: cc };
                }
            }

            // Terminal states — return immediately.
            EnumState::Configured | EnumState::Error { .. } | EnumState::Timeout => {
                return (state, ctx);
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
    use crate::usb::xhci::context::{EP_TYPE_CONTROL, ep_max_packet_size, ep_type};

    // -----------------------------------------------------------------------
    // Mock UsbHostOps
    // -----------------------------------------------------------------------

    /// Keyboard config blob — same as in descriptor.rs tests.
    const BOOT_KEYBOARD_CONFIG_BLOB: &[u8] = &[
        0x09, 0x02, 0x22, 0x00, 0x01, 0x01, 0x00, 0xA0, 0x32, // Config
        0x09, 0x04, 0x00, 0x00, 0x01, 0x03, 0x01, 0x01, 0x00, // Interface
        0x09, 0x21, 0x11, 0x01, 0x00, 0x01, 0x22, 0x3F, 0x00, // HID
        0x07, 0x05, 0x81, 0x03, 0x08, 0x00, 0x0A, // Endpoint
    ];

    /// A minimal 18-byte Device Descriptor (full-speed HID keyboard).
    const KEYBOARD_DEVICE_DESCRIPTOR: &[u8] = &[
        0x12, // bLength = 18
        0x01, // bDescriptorType = Device
        0x00, 0x02, // bcdUSB = 2.00
        0x00, // bDeviceClass
        0x00, // bDeviceSubClass
        0x00, // bDeviceProtocol
        0x08, // bMaxPacketSize0 = 8 (full-speed)
        0xB4, 0x04, // idVendor
        0x10, 0x00, // idProduct
        0x00, 0x01, // bcdDevice
        0x01, 0x02, 0x03, // iManufacturer/iProduct/iSerial
        0x01, // bNumConfigurations = 1
    ];

    /// Records what the mock was asked to do.
    #[derive(Debug, Default)]
    struct MockOps {
        /// Calls in arrival order: "EnableSlot", "AddressDeviceBsr",
        /// "EvaluateContext", "AddressDevice", "GetDeviceDescriptor",
        /// "GetConfigShort", "GetConfigFull", "SetConfiguration",
        /// "ConfigureEndpoint".
        call_log: Vec<&'static str>,
        /// Input Context snapshots captured from address_device / evaluate_context
        /// / configure_endpoint calls.
        ctx_snapshots: Vec<InputContextSnapshot>,
        /// If `true`, the mock times out on the first GET_DESCRIPTOR call.
        timeout_descriptor: bool,
        /// If non-zero, address_device BSR=false returns this code.
        address_device_fail_code: u8,
    }

    impl UsbHostOps for MockOps {
        fn enable_slot(&mut self) -> Result<u8, u8> {
            self.call_log.push("EnableSlot");
            Ok(1) // Always returns slot 1.
        }

        fn address_device(&mut self, _slot_id: u8, ctx: &InputContextSnapshot, bsr: bool) -> u8 {
            self.call_log.push(if bsr {
                "AddressDeviceBsr"
            } else {
                "AddressDevice"
            });
            self.ctx_snapshots.push(ctx.clone());
            if !bsr && self.address_device_fail_code != 0 {
                return self.address_device_fail_code;
            }
            COMPLETION_SUCCESS
        }

        fn evaluate_context(&mut self, _slot_id: u8, ctx: &InputContextSnapshot) -> u8 {
            self.call_log.push("EvaluateContext");
            self.ctx_snapshots.push(ctx.clone());
            COMPLETION_SUCCESS
        }

        fn get_device_descriptor(&mut self, _slot_id: u8) -> Option<Vec<u8>> {
            self.call_log.push("GetDeviceDescriptor");
            if self.timeout_descriptor {
                None
            } else {
                Some(KEYBOARD_DEVICE_DESCRIPTOR.to_vec())
            }
        }

        fn get_config_short(&mut self, _slot_id: u8, _len: u16) -> Option<Vec<u8>> {
            self.call_log.push("GetConfigShort");
            // Return just the first 9 bytes of the keyboard config blob.
            Some(BOOT_KEYBOARD_CONFIG_BLOB[..9].to_vec())
        }

        fn get_config_full(&mut self, _slot_id: u8, _len: u16) -> Option<Vec<u8>> {
            self.call_log.push("GetConfigFull");
            Some(BOOT_KEYBOARD_CONFIG_BLOB.to_vec())
        }

        fn set_configuration(&mut self, _slot_id: u8, _value: u8) -> u8 {
            self.call_log.push("SetConfiguration");
            COMPLETION_SUCCESS
        }

        fn configure_endpoint(&mut self, _slot_id: u8, ctx: &InputContextSnapshot) -> u8 {
            self.call_log.push("ConfigureEndpoint");
            self.ctx_snapshots.push(ctx.clone());
            COMPLETION_SUCCESS
        }
    }

    // -----------------------------------------------------------------------
    // Full enumeration test — full-speed boot keyboard
    // -----------------------------------------------------------------------

    #[test]
    fn full_speed_keyboard_reaches_configured() {
        let mut ops = MockOps::default();
        let ctx = EnumContext {
            speed: Some(PortSpeed::Full),
            ep0_ring_iova: 0x0010_0000,
            port: 1,
            ..Default::default()
        };
        let (final_state, final_ctx) = run_enumeration(EnumState::EnableSlot, ctx, &mut ops);
        assert_eq!(final_state, EnumState::Configured);
        assert_eq!(final_ctx.slot_id, 1);
    }

    #[test]
    fn enumeration_calls_in_correct_order() {
        let mut ops = MockOps::default();
        let ctx = EnumContext {
            speed: Some(PortSpeed::Full),
            ep0_ring_iova: 0x0010_0000,
            port: 2,
            ..Default::default()
        };
        run_enumeration(EnumState::EnableSlot, ctx, &mut ops);
        // Expected call order:
        assert_eq!(
            ops.call_log,
            &[
                "EnableSlot",
                "AddressDeviceBsr",
                "GetDeviceDescriptor", // EvaluateContext phase reads descriptor
                "EvaluateContext",
                "AddressDevice",
                "GetDeviceDescriptor", // GetDeviceDescriptor phase
                "GetConfigShort",
                "GetConfigFull",
                "SetConfiguration",
                "ConfigureEndpoint",
            ]
        );
    }

    #[test]
    fn address_device_input_context_add_flags() {
        let mut ops = MockOps::default();
        let ctx = EnumContext {
            speed: Some(PortSpeed::Full),
            ep0_ring_iova: 0x0010_0000,
            port: 1,
            ..Default::default()
        };
        run_enumeration(EnumState::EnableSlot, ctx, &mut ops);
        // The first snapshot (index 0) is from AddressDeviceBsr.
        let snap = &ops.ctx_snapshots[0];
        // Add Flags must be 0x3 (A0 = Slot, A1 = EP0).
        assert_eq!(snap.add_flags, 0x3);
    }

    #[test]
    fn address_device_input_context_ep0_type_and_mps() {
        let mut ops = MockOps::default();
        let ctx = EnumContext {
            speed: Some(PortSpeed::Full),
            ep0_ring_iova: 0x0010_0000,
            port: 1,
            ..Default::default()
        };
        run_enumeration(EnumState::EnableSlot, ctx, &mut ops);
        let snap = &ops.ctx_snapshots[0];
        // EP0 context dword1: EP Type = Control = 4, CErr = 3, MPS = 8 (FS initial).
        assert_eq!(ep_type(snap.ep0_dword1), EP_TYPE_CONTROL);
        assert_eq!(ep_max_packet_size(snap.ep0_dword1), 8);
    }

    #[test]
    fn address_device_input_context_dequeue_ptr_has_dcs() {
        let mut ops = MockOps::default();
        let ring_iova = 0x0020_0000u64;
        let ctx = EnumContext {
            speed: Some(PortSpeed::Full),
            ep0_ring_iova: ring_iova,
            port: 1,
            ..Default::default()
        };
        run_enumeration(EnumState::EnableSlot, ctx, &mut ops);
        let snap = &ops.ctx_snapshots[0];
        // DCS bit (bit 0) must be set.
        assert_eq!(snap.ep0_dequeue_ptr & 1, 1);
        assert_eq!(snap.ep0_dequeue_ptr & !1u64, ring_iova);
    }

    #[test]
    fn timeout_on_descriptor_yields_timeout_state() {
        let mut ops = MockOps {
            timeout_descriptor: true,
            ..Default::default()
        };
        let ctx = EnumContext {
            speed: Some(PortSpeed::Full),
            ep0_ring_iova: 0x0010_0000,
            port: 1,
            ..Default::default()
        };
        let (final_state, _) = run_enumeration(EnumState::EnableSlot, ctx, &mut ops);
        assert_eq!(final_state, EnumState::Timeout);
    }

    #[test]
    fn address_device_failure_yields_error_state() {
        let mut ops = MockOps {
            address_device_fail_code: 0x05, // TRB Error
            ..Default::default()
        };
        let ctx = EnumContext {
            speed: Some(PortSpeed::Full),
            ep0_ring_iova: 0x0010_0000,
            port: 1,
            ..Default::default()
        };
        let (final_state, _) = run_enumeration(EnumState::EnableSlot, ctx, &mut ops);
        assert!(matches!(final_state, EnumState::Error { code: 0x05 }));
    }

    #[test]
    fn parsed_config_available_after_configured() {
        let mut ops = MockOps::default();
        let ctx = EnumContext {
            speed: Some(PortSpeed::Full),
            ep0_ring_iova: 0x0010_0000,
            port: 1,
            ..Default::default()
        };
        let (state, final_ctx) = run_enumeration(EnumState::EnableSlot, ctx, &mut ops);
        assert_eq!(state, EnumState::Configured);
        let cfg = final_ctx
            .parsed_config
            .as_ref()
            .expect("config must be present");
        assert_eq!(cfg.interfaces.len(), 1);
        let iface = &cfg.interfaces[0].interface;
        // HID boot keyboard.
        assert_eq!(iface.b_interface_class, 0x03);
        assert_eq!(iface.b_interface_sub_class, 0x01);
        assert_eq!(iface.b_interface_protocol, 0x01);
    }

    #[test]
    fn slot_context_port_number_encoded() {
        let mut ops = MockOps::default();
        let ctx = EnumContext {
            speed: Some(PortSpeed::Full),
            ep0_ring_iova: 0x0010_0000,
            port: 3,
            ..Default::default()
        };
        run_enumeration(EnumState::EnableSlot, ctx, &mut ops);
        let snap = &ops.ctx_snapshots[0];
        // Root Hub Port Number at bits 23:16 of slot_dword1.
        assert_eq!((snap.slot_dword1 >> 16) & 0xFF, 3);
    }
}
