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
//! Steps 2–3 (BSR pre-read + Evaluate Context) are **Low/Full speed only**.
//! High Speed (fixed EP0 MPS=64) and SuperSpeed (fixed EP0 MPS=512) skip
//! directly from Enable Slot to a single Address Device (BSR=0).
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
    EP_CERR_3, EP_TYPE_BULK_IN, EP_TYPE_BULK_OUT, EP_TYPE_CONTROL, EP_TYPE_INTERRUPT_IN,
    EP_TYPE_INTERRUPT_OUT, add_flags, ep_context_dword0_interval, ep_context_dword1,
    ep_tr_dequeue_ptr, slot_context_dword0, slot_context_dword1,
};
use crate::usb::xhci::port::{PortSpeed, ep0_max_packet_for_speed};
use crate::usb::xhci::trb::{COMPLETION_SUCCESS, dci};

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
    /// Low/Full speed only.
    AddressDeviceBsr,
    /// Issue Evaluate Context to update the EP0 Max Packet Size using the
    /// `bMaxPacketSize0` value read during [`AddressDeviceBsr`].
    /// Low/Full speed only.
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

/// Snapshot of an endpoint context within the Configure Endpoint input context.
///
/// The production driver copies these fields into the DMA Input Context buffer
/// at the appropriate slot for each endpoint's DCI.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EndpointContextSnapshot {
    /// The Device Context Index for this endpoint.
    pub dci: u8,
    /// Endpoint Context dword 0 (Interval field; other fields zero for non-isoch).
    pub ep_dword0: u32,
    /// Endpoint Context dword 1 (EP type, CErr, Max Packet Size).
    pub ep_dword1: u32,
    /// TR Dequeue Pointer (ring IOVA | DCS).
    pub ep_dequeue_ptr: u64,
}

/// Snapshot of the Input Context fields the enumeration machine programmes
/// during Address Device, Evaluate Context, and Configure Endpoint commands.
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
    /// Additional endpoint contexts added by Configure Endpoint.
    /// Empty for Address Device and Evaluate Context snapshots.
    pub endpoint_contexts: Vec<EndpointContextSnapshot>,
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

    /// Perform a GET_DESCRIPTOR(Device) control IN transfer for `len` bytes.
    ///
    /// For the BSR pre-read (Low/Full speed), `len` is 8 — the minimum needed
    /// to learn `bMaxPacketSize0` with the initial EP0 MPS still in effect.
    /// For the post-address full read, `len` is 18.
    ///
    /// Returns the raw descriptor bytes, or `None` on timeout.
    fn get_device_descriptor(&mut self, slot_id: u8, len: u16) -> Option<Vec<u8>>;

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
    /// `ctx.endpoint_contexts` contains one entry per interface endpoint.
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
    /// The port speed (determines initial EP0 MPS and BSR two-step necessity).
    pub speed: Option<PortSpeed>,
    /// EP0 Max Packet Size learned from `bMaxPacketSize0` (after the BSR read).
    pub ep0_mps: u16,
    /// The full Device Descriptor, once read.
    pub device_descriptor: Option<DeviceDescriptor>,
    /// The parsed configuration tree, once read.
    pub parsed_config: Option<ParsedConfig>,
    /// The `wTotalLength` from the short config read, forwarded to the full read.
    pub config_total_length: u16,
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
        endpoint_contexts: Vec::new(),
    }
}

/// Build the Configure Endpoint Input Context snapshot.
///
/// Adds an endpoint context for each endpoint in each interface of
/// `parsed_config`. Sets Add Flags for the Slot Context (A0) plus each
/// endpoint's DCI bit. Sets Context Entries to the maximum DCI in use so
/// the controller knows how far to read.
///
/// The placeholder `ep0_ring_iova` and `ep0_mps` from `ctx` are still present
/// (the Slot and EP0 entries are always present in the snapshot), but the
/// key output is `endpoint_contexts` (the new interface endpoints) and the
/// updated `add_flags` / `slot_dword0.context_entries`.
fn build_configure_endpoint_ctx(ctx: &EnumContext) -> InputContextSnapshot {
    let speed_psi = match ctx.speed {
        Some(PortSpeed::Full) => crate::usb::xhci::port::PSI_FULL_SPEED,
        Some(PortSpeed::Low) => crate::usb::xhci::port::PSI_LOW_SPEED,
        Some(PortSpeed::High) => crate::usb::xhci::port::PSI_HIGH_SPEED,
        Some(PortSpeed::Super) => crate::usb::xhci::port::PSI_SUPER_SPEED,
        None => crate::usb::xhci::port::PSI_FULL_SPEED,
    };

    // Collect endpoint DCIs and their context data.
    let mut ep_snapshots: Vec<EndpointContextSnapshot> = Vec::new();
    let mut max_dci: u8 = 1; // always at least EP0

    if let Some(parsed) = &ctx.parsed_config {
        for iface in &parsed.interfaces {
            for ep in &iface.endpoints {
                let ep_num = ep.endpoint_number();
                let is_in = ep.is_in();
                let ep_dci = dci(ep_num, is_in);
                if ep_dci > max_dci {
                    max_dci = ep_dci;
                }

                // Determine xHCI EP type from USB transfer type + direction.
                // bmAttributes bits 1:0: 0=Control, 1=Isoch, 2=Bulk, 3=Interrupt.
                let xhci_ep_type = match (ep.transfer_type(), is_in) {
                    (0, _) => EP_TYPE_CONTROL,
                    (2, false) => EP_TYPE_BULK_OUT,
                    (2, true) => EP_TYPE_BULK_IN,
                    (3, false) => EP_TYPE_INTERRUPT_OUT,
                    (3, true) => EP_TYPE_INTERRUPT_IN,
                    // Default to Interrupt IN for unknown types; production
                    // code should log and skip.
                    _ => EP_TYPE_INTERRUPT_IN,
                };

                // Convert bInterval to the xHCI Endpoint-Context Interval field
                // (xHCI 1.2 §6.2.3.6, Table 6-12).
                //   HS/SS: bInterval already counts 125 µs microframes as
                //          2^(bInterval-1), so Interval = bInterval - 1,
                //          clamped 0..=15.
                //   FS/LS: bInterval counts 1 ms frames, but the Interval field
                //          encodes the period as 2^Interval × 125 µs microframes.
                //          Since 1 frame = 8 microframes = 2^3, the conversion is
                //          Interval = 3 + floor(log2(bInterval)), clamped to the
                //          valid FS range 3..=10 (equivalent to Linux's
                //          `fls(8 * bInterval) - 1`).
                // The field is only meaningful for periodic (interrupt/isoch)
                // endpoints; the controller ignores it for control/bulk.
                let xhci_interval = match ctx.speed {
                    Some(PortSpeed::High) | Some(PortSpeed::Super) => {
                        ep.b_interval.saturating_sub(1).min(15)
                    }
                    _ => {
                        let bi = ep.b_interval.max(1);
                        let log2 = u8::BITS - bi.leading_zeros() - 1; // floor(log2)
                        (3 + log2).clamp(3, 10) as u8
                    }
                };

                // Placeholder ring IOVA — production driver allocates a real ring.
                // DCS=1 is always set by ep_tr_dequeue_ptr.
                let ep_ptr = ep_tr_dequeue_ptr(0);

                ep_snapshots.push(EndpointContextSnapshot {
                    dci: ep_dci,
                    ep_dword0: ep_context_dword0_interval(xhci_interval),
                    ep_dword1: ep_context_dword1(xhci_ep_type, EP_CERR_3, ep.w_max_packet_size),
                    ep_dequeue_ptr: ep_ptr,
                });
            }
        }
    }

    // Build Add Flags: A0 (Slot) + one bit per new endpoint DCI.
    // A1 (EP0, DCI 1) must NOT be included — EP0 was already configured by
    // Address Device. Re-adding it via Configure Endpoint would cause the
    // controller to validate the stale TR Dequeue Pointer and reject the
    // command with TRB Error (completion code 5). See xHCI §4.6.6 / §6.2.5.1.
    let mut dci_list: Vec<u8> = alloc::vec![0]; // A0 (Slot) only; no DCI 1
    for snap in &ep_snapshots {
        dci_list.push(snap.dci);
    }
    let af = add_flags(&dci_list);

    // Slot Context dword 0: context_entries = max_dci.
    let slot_dw0 = slot_context_dword0(0, speed_psi, max_dci);
    let slot_dw1 = slot_context_dword1(ctx.port);
    let ep0_dw1 = ep_context_dword1(EP_TYPE_CONTROL, EP_CERR_3, ctx.ep0_mps);
    let ep0_ptr = ep_tr_dequeue_ptr(ctx.ep0_ring_iova);

    InputContextSnapshot {
        add_flags: af,
        slot_dword0: slot_dw0,
        slot_dword1: slot_dw1,
        ep0_dword1: ep0_dw1,
        ep0_dequeue_ptr: ep0_ptr,
        endpoint_contexts: ep_snapshots,
    }
}

/// Returns `true` if this speed requires the BSR two-step (Low and Full speed).
///
/// High Speed and SuperSpeed have a fixed, known EP0 MPS so they can skip
/// the BSR pre-read and Evaluate Context steps.
fn speed_needs_bsr(speed: PortSpeed) -> bool {
    matches!(speed, PortSpeed::Low | PortSpeed::Full)
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
                        match ctx.speed {
                            Some(speed) => {
                                ctx.ep0_mps = ep0_max_packet_for_speed(speed);
                                // High/Super: known fixed MPS → skip BSR two-step.
                                if speed_needs_bsr(speed) {
                                    state = EnumState::AddressDeviceBsr;
                                } else {
                                    state = EnumState::AddressDevice;
                                }
                            }
                            None => {
                                // Unknown port speed: cannot enumerate safely.
                                state = EnumState::Error { code: 0xFE };
                            }
                        }
                    }
                    Err(code) => {
                        state = EnumState::Error { code };
                    }
                }
            }

            EnumState::AddressDeviceBsr => {
                // Low/Full speed only: BSR=1 to enter default state without
                // assigning a USB address yet.
                let input_ctx = build_address_device_ctx(&ctx);
                let cc = ops.address_device(ctx.slot_id, &input_ctx, true);
                if cc == COMPLETION_SUCCESS {
                    state = EnumState::EvaluateContext;
                } else {
                    state = EnumState::Error { code: cc };
                }
            }

            EnumState::EvaluateContext => {
                // Low/Full speed only: read the first 8 bytes of the Device
                // Descriptor — the minimum needed to learn bMaxPacketSize0
                // while EP0 MPS is still at its initial (8-byte) value.
                let bytes = match ops.get_device_descriptor(ctx.slot_id, 8) {
                    Some(b) => b,
                    None => {
                        state = EnumState::Timeout;
                        continue;
                    }
                };
                // Parse bMaxPacketSize0 (byte 7 of the Device Descriptor).
                if bytes.len() >= 8 {
                    let raw_mps = bytes[7];
                    // For Full/Low speed, bMaxPacketSize0 is a direct byte count.
                    ctx.ep0_mps = raw_mps as u16;
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
                // BSR=0: assign the USB address.
                // For High/Super speed the correct MPS was set in EnableSlot.
                let input_ctx = build_address_device_ctx(&ctx);
                let cc = ops.address_device(ctx.slot_id, &input_ctx, false);
                if cc == COMPLETION_SUCCESS {
                    state = EnumState::GetDeviceDescriptor;
                } else {
                    state = EnumState::Error { code: cc };
                }
            }

            EnumState::GetDeviceDescriptor => {
                // Full 18-byte Device Descriptor read.
                let bytes = match ops.get_device_descriptor(ctx.slot_id, 18) {
                    Some(b) => b,
                    None => {
                        state = EnumState::Timeout;
                        continue;
                    }
                };
                // For SuperSpeed the bMaxPacketSize0 field is an exponent;
                // update ep0_mps now that we have the full descriptor.
                if let Some(PortSpeed::Super) = ctx.speed
                    && bytes.len() >= 8
                {
                    let raw_mps = bytes[7] as u32;
                    ctx.ep0_mps = 1u16.checked_shl(raw_mps).unwrap_or(512);
                }
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
                    // Store wTotalLength explicitly for the next step.
                    ctx.config_total_length = cfg_hdr.w_total_length;
                    state = EnumState::GetConfigFull;
                } else {
                    state = EnumState::Error { code: 0xFF };
                }
            }

            EnumState::GetConfigFull => {
                let total = if ctx.config_total_length > 0 {
                    ctx.config_total_length
                } else {
                    9
                };
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
                // Build a context that activates each interface endpoint.
                let input_ctx = build_configure_endpoint_ctx(&ctx);
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
    use crate::usb::xhci::context::{
        EP_TYPE_BULK_IN, EP_TYPE_BULK_OUT, EP_TYPE_CONTROL, EP_TYPE_INTERRUPT_IN, ep_interval,
        ep_max_packet_size, ep_type, slot_context_entries,
    };
    use crate::usb::xhci::trb::dci as compute_dci;

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
        /// "EvaluateContext", "AddressDevice", "GetDeviceDescriptor(8)",
        /// "GetDeviceDescriptor(18)", "GetConfigShort", "GetConfigFull",
        /// "SetConfiguration", "ConfigureEndpoint".
        call_log: Vec<alloc::string::String>,
        /// Input Context snapshots captured from address_device / evaluate_context
        /// / configure_endpoint calls.
        ctx_snapshots: Vec<InputContextSnapshot>,
        /// If `true`, the mock times out on the first GET_DESCRIPTOR call.
        timeout_descriptor: bool,
        /// If non-zero, address_device BSR=false returns this code.
        address_device_fail_code: u8,
        /// Override the device descriptor returned (default: KEYBOARD_DEVICE_DESCRIPTOR).
        device_descriptor_override: Option<Vec<u8>>,
        /// Override the config blob returned by get_config_short / get_config_full.
        config_blob_override: Option<Vec<u8>>,
    }

    impl UsbHostOps for MockOps {
        fn enable_slot(&mut self) -> Result<u8, u8> {
            self.call_log.push("EnableSlot".into());
            Ok(1) // Always returns slot 1.
        }

        fn address_device(&mut self, _slot_id: u8, ctx: &InputContextSnapshot, bsr: bool) -> u8 {
            self.call_log.push(if bsr {
                "AddressDeviceBsr".into()
            } else {
                "AddressDevice".into()
            });
            self.ctx_snapshots.push(ctx.clone());
            if !bsr && self.address_device_fail_code != 0 {
                return self.address_device_fail_code;
            }
            COMPLETION_SUCCESS
        }

        fn evaluate_context(&mut self, _slot_id: u8, ctx: &InputContextSnapshot) -> u8 {
            self.call_log.push("EvaluateContext".into());
            self.ctx_snapshots.push(ctx.clone());
            COMPLETION_SUCCESS
        }

        fn get_device_descriptor(&mut self, _slot_id: u8, len: u16) -> Option<Vec<u8>> {
            self.call_log
                .push(alloc::format!("GetDeviceDescriptor({})", len));
            if self.timeout_descriptor {
                None
            } else {
                let full = self
                    .device_descriptor_override
                    .as_deref()
                    .unwrap_or(KEYBOARD_DEVICE_DESCRIPTOR);
                // Return only `len` bytes (clamped to actual length).
                let take = (len as usize).min(full.len());
                Some(full[..take].to_vec())
            }
        }

        fn get_config_short(&mut self, _slot_id: u8, _len: u16) -> Option<Vec<u8>> {
            self.call_log.push("GetConfigShort".into());
            let blob = self
                .config_blob_override
                .as_deref()
                .unwrap_or(BOOT_KEYBOARD_CONFIG_BLOB);
            // Return just the first 9 bytes of the config blob.
            Some(blob[..9].to_vec())
        }

        fn get_config_full(&mut self, _slot_id: u8, _len: u16) -> Option<Vec<u8>> {
            self.call_log.push("GetConfigFull".into());
            let blob = self
                .config_blob_override
                .as_deref()
                .unwrap_or(BOOT_KEYBOARD_CONFIG_BLOB);
            Some(blob.to_vec())
        }

        fn set_configuration(&mut self, _slot_id: u8, _value: u8) -> u8 {
            self.call_log.push("SetConfiguration".into());
            COMPLETION_SUCCESS
        }

        fn configure_endpoint(&mut self, _slot_id: u8, ctx: &InputContextSnapshot) -> u8 {
            self.call_log.push("ConfigureEndpoint".into());
            self.ctx_snapshots.push(ctx.clone());
            COMPLETION_SUCCESS
        }
    }

    // Helper: find a snapshot by index in ctx_snapshots (order: BSR, EvalCtx,
    // AddressDevice, ConfigureEndpoint).
    fn run_fs_keyboard() -> (EnumState, EnumContext, MockOps) {
        let mut ops = MockOps::default();
        let ctx = EnumContext {
            speed: Some(PortSpeed::Full),
            ep0_ring_iova: 0x0010_0000,
            port: 1,
            ..Default::default()
        };
        let (state, final_ctx) = run_enumeration(EnumState::EnableSlot, ctx, &mut ops);
        (state, final_ctx, ops)
    }

    // -----------------------------------------------------------------------
    // Full enumeration test — full-speed boot keyboard
    // -----------------------------------------------------------------------

    #[test]
    fn full_speed_keyboard_reaches_configured() {
        let (final_state, final_ctx, _ops) = run_fs_keyboard();
        assert_eq!(final_state, EnumState::Configured);
        assert_eq!(final_ctx.slot_id, 1);
    }

    #[test]
    fn enumeration_calls_in_correct_order_full_speed() {
        let (_state, _ctx, ops) = run_fs_keyboard();
        // Expected call order for Full Speed (BSR two-step):
        assert_eq!(
            ops.call_log,
            &[
                "EnableSlot",
                "AddressDeviceBsr",
                "GetDeviceDescriptor(8)", // 8-byte short read in EvaluateContext
                "EvaluateContext",
                "AddressDevice",
                "GetDeviceDescriptor(18)", // 18-byte full read in GetDeviceDescriptor
                "GetConfigShort",
                "GetConfigFull",
                "SetConfiguration",
                "ConfigureEndpoint",
            ]
        );
    }

    // -----------------------------------------------------------------------
    // M1: High Speed skips BSR two-step
    // -----------------------------------------------------------------------

    #[test]
    fn high_speed_skips_bsr_and_evaluate_context() {
        let mut ops = MockOps::default();
        // Use a HS device descriptor with bMaxPacketSize0 = 64.
        let hs_descriptor: Vec<u8> = vec![
            0x12, 0x01, 0x00, 0x02, // bLength, bDescriptorType, bcdUSB
            0x00, 0x00, 0x00, // bDeviceClass, SubClass, Protocol
            0x40, // bMaxPacketSize0 = 64
            0xB4, 0x04, 0x10, 0x00, // idVendor, idProduct
            0x00, 0x01, // bcdDevice
            0x01, 0x02, 0x03, // iManufacturer, iProduct, iSerial
            0x01, // bNumConfigurations
        ];
        ops.device_descriptor_override = Some(hs_descriptor);
        let ctx = EnumContext {
            speed: Some(PortSpeed::High),
            ep0_ring_iova: 0x0010_0000,
            port: 1,
            ..Default::default()
        };
        let (final_state, _final_ctx) = run_enumeration(EnumState::EnableSlot, ctx, &mut ops);
        assert_eq!(final_state, EnumState::Configured);

        // Must NOT contain AddressDeviceBsr or EvaluateContext.
        assert!(
            !ops.call_log.iter().any(|s| s == "AddressDeviceBsr"),
            "HS must not issue AddressDeviceBsr"
        );
        assert!(
            !ops.call_log.iter().any(|s| s == "EvaluateContext"),
            "HS must not issue EvaluateContext"
        );

        // First address_device call must be BSR=false (AddressDevice).
        assert!(
            ops.call_log.iter().any(|s| s == "AddressDevice"),
            "HS must issue AddressDevice"
        );
    }

    #[test]
    fn high_speed_ep0_mps_is_64() {
        let mut ops = MockOps::default();
        let hs_descriptor: Vec<u8> = vec![
            0x12, 0x01, 0x00, 0x02, 0x00, 0x00, 0x00, 0x40, // bMaxPacketSize0 = 64
            0xB4, 0x04, 0x10, 0x00, 0x00, 0x01, 0x01, 0x02, 0x03, 0x01,
        ];
        ops.device_descriptor_override = Some(hs_descriptor);
        let ctx = EnumContext {
            speed: Some(PortSpeed::High),
            ep0_ring_iova: 0x0010_0000,
            port: 1,
            ..Default::default()
        };
        let (_, final_ctx) = run_enumeration(EnumState::EnableSlot, ctx, &mut ops);
        // EP0 MPS for High Speed must be 64 (set in EnableSlot, not from a read).
        assert_eq!(final_ctx.ep0_mps, 64);
        // The AddressDevice snapshot (index 0 — no BSR snap) must encode MPS=64.
        let snap = &ops.ctx_snapshots[0];
        assert_eq!(ep_max_packet_size(snap.ep0_dword1), 64);
    }

    #[test]
    fn super_speed_skips_bsr_and_evaluate_context() {
        let mut ops = MockOps::default();
        // SS descriptor: bMaxPacketSize0 = 9 (exponent → 2^9 = 512).
        let ss_descriptor: Vec<u8> = vec![
            0x12, 0x01, 0x00, 0x03, // bcdUSB = 3.00
            0x00, 0x00, 0x00, 0x09, // bMaxPacketSize0 = 9 (exponent)
            0xB4, 0x04, 0x10, 0x00, 0x00, 0x01, 0x01, 0x02, 0x03, 0x01,
        ];
        ops.device_descriptor_override = Some(ss_descriptor);
        let ctx = EnumContext {
            speed: Some(PortSpeed::Super),
            ep0_ring_iova: 0x0010_0000,
            port: 1,
            ..Default::default()
        };
        let (final_state, _final_ctx) = run_enumeration(EnumState::EnableSlot, ctx, &mut ops);
        assert_eq!(final_state, EnumState::Configured);

        assert!(
            !ops.call_log.iter().any(|s| s == "AddressDeviceBsr"),
            "SS must not issue AddressDeviceBsr"
        );
        assert!(
            !ops.call_log.iter().any(|s| s == "EvaluateContext"),
            "SS must not issue EvaluateContext"
        );
    }

    #[test]
    fn super_speed_ep0_mps_is_512() {
        let mut ops = MockOps::default();
        // SS descriptor: bMaxPacketSize0 = 9 → 2^9 = 512.
        let ss_descriptor: Vec<u8> = vec![
            0x12, 0x01, 0x00, 0x03, 0x00, 0x00, 0x00, 0x09, // bMaxPacketSize0 = 9 (exponent)
            0xB4, 0x04, 0x10, 0x00, 0x00, 0x01, 0x01, 0x02, 0x03, 0x01,
        ];
        ops.device_descriptor_override = Some(ss_descriptor);
        let ctx = EnumContext {
            speed: Some(PortSpeed::Super),
            ep0_ring_iova: 0x0010_0000,
            port: 1,
            ..Default::default()
        };
        let (_, final_ctx) = run_enumeration(EnumState::EnableSlot, ctx, &mut ops);
        // EP0 MPS for SuperSpeed must be 512 (set in EnableSlot, refined in
        // GetDeviceDescriptor from the exponent).
        assert_eq!(final_ctx.ep0_mps, 512);
    }

    // -----------------------------------------------------------------------
    // M2: Descriptor read length assertions
    // -----------------------------------------------------------------------

    #[test]
    fn full_speed_first_descriptor_read_is_8_bytes() {
        let (_state, _ctx, ops) = run_fs_keyboard();
        // The EvaluateContext phase must request exactly 8 bytes.
        let first_desc_call = ops
            .call_log
            .iter()
            .find(|s| s.starts_with("GetDeviceDescriptor"))
            .expect("must have at least one GetDeviceDescriptor call");
        assert_eq!(first_desc_call.as_str(), "GetDeviceDescriptor(8)");
    }

    #[test]
    fn full_speed_second_descriptor_read_is_18_bytes() {
        let (_state, _ctx, ops) = run_fs_keyboard();
        // The GetDeviceDescriptor state must request exactly 18 bytes.
        let desc_calls: Vec<_> = ops
            .call_log
            .iter()
            .filter(|s| s.starts_with("GetDeviceDescriptor"))
            .collect();
        assert_eq!(desc_calls.len(), 2);
        assert_eq!(desc_calls[1].as_str(), "GetDeviceDescriptor(18)");
    }

    // -----------------------------------------------------------------------
    // M3: Configure Endpoint adds interface endpoint contexts
    // -----------------------------------------------------------------------

    #[test]
    fn configure_endpoint_adds_boot_keyboard_ep1_in() {
        let (_state, _ctx, ops) = run_fs_keyboard();

        // The last ctx_snapshot is from ConfigureEndpoint.
        // Order: [AddressDeviceBsr(0), EvaluateContext(1), AddressDevice(2), ConfigureEndpoint(3)]
        let cfg_snap = ops
            .ctx_snapshots
            .last()
            .expect("must have configure_endpoint snapshot");

        // Boot keyboard: EP1 IN → DCI = 2*1 + 1 = 3.
        let expected_dci = compute_dci(1, true);
        assert_eq!(expected_dci, 3);

        // Add flags must include bit for DCI 3 (A3 = 1 << 3 = 8).
        assert_ne!(
            cfg_snap.add_flags & (1 << expected_dci),
            0,
            "Add Flags must include DCI {} (bit {}); add_flags=0x{:X}",
            expected_dci,
            expected_dci,
            cfg_snap.add_flags
        );

        // Context Entries in Slot Context dword 0 must be >= DCI 3.
        let ctx_entries = slot_context_entries(cfg_snap.slot_dword0);
        assert!(
            ctx_entries >= expected_dci,
            "Context Entries ({}) must be >= DCI {} (EP1 IN)",
            ctx_entries,
            expected_dci
        );

        // endpoint_contexts must contain exactly one entry for DCI 3.
        assert_eq!(cfg_snap.endpoint_contexts.len(), 1);
        let ep_snap = &cfg_snap.endpoint_contexts[0];
        assert_eq!(ep_snap.dci, expected_dci);

        // EP type must be Interrupt IN.
        assert_eq!(
            ep_type(ep_snap.ep_dword1),
            EP_TYPE_INTERRUPT_IN,
            "endpoint must be Interrupt IN"
        );

        // Max Packet Size must match the descriptor (8 bytes for boot keyboard).
        assert_eq!(ep_max_packet_size(ep_snap.ep_dword1), 8);

        // Interval: the boot keyboard's interrupt EP has bInterval=10 (10 ms,
        // Full speed). Per xHCI §6.2.3.6 the FS conversion is
        // 3 + floor(log2(10)) = 3 + 3 = 6 (2^6 = 64 microframes = 8 ms), NOT
        // floor(log2(10)) = 3. This pins the frame→microframe (+3) term.
        assert_eq!(
            ep_interval(ep_snap.ep_dword0),
            6,
            "FS interrupt bInterval=10 must encode xHCI Interval=6"
        );
    }

    // -----------------------------------------------------------------------
    // M3b: Bulk endpoint EP types map correctly in ConfigureEndpoint
    // -----------------------------------------------------------------------

    /// Config blob for a High-Speed device with three endpoints:
    ///   EP1 IN  Interrupt (DCI 3), MPS 8,   bInterval 1
    ///   EP2 OUT Bulk      (DCI 4), MPS 512,  bInterval 0
    ///   EP2 IN  Bulk      (DCI 5), MPS 512,  bInterval 0
    ///
    /// Layout: Config(9) + Interface(9) + EP1-IN-Interrupt(7) + EP2-OUT-Bulk(7) + EP2-IN-Bulk(7)
    /// wTotalLength = 39 = 0x0027
    const BULK_DEVICE_CONFIG_BLOB: &[u8] = &[
        // Config descriptor (9 bytes)
        0x09, 0x02, 0x27, 0x00, 0x01, 0x01, 0x00, 0xA0, 0x32,
        // Interface descriptor (9 bytes) — class FF, 3 endpoints
        0x09, 0x04, 0x00, 0x00, 0x03, 0xFF, 0x00, 0x00, 0x00,
        // EP1 IN Interrupt (7 bytes): address=0x81, bmAttributes=0x03, MPS=8, bInterval=1
        0x07, 0x05, 0x81, 0x03, 0x08, 0x00, 0x01,
        // EP2 OUT Bulk (7 bytes): address=0x02, bmAttributes=0x02, MPS=512, bInterval=0
        0x07, 0x05, 0x02, 0x02, 0x00, 0x02, 0x00,
        // EP2 IN Bulk (7 bytes): address=0x82, bmAttributes=0x02, MPS=512, bInterval=0
        0x07, 0x05, 0x82, 0x02, 0x00, 0x02, 0x00,
    ];

    /// HS device descriptor (bMaxPacketSize0=64, skips BSR two-step).
    const BULK_DEVICE_DESCRIPTOR: &[u8] = &[
        0x12, 0x01, 0x00, 0x02, // bLength, bDescriptorType, bcdUSB=2.00
        0x00, 0x00, 0x00, 0x40, // bDeviceClass, SubClass, Protocol, bMaxPacketSize0=64
        0xAB, 0xCD, 0x01, 0x00, // idVendor, idProduct
        0x00, 0x01, 0x01, 0x02, 0x03, 0x01, // bcdDevice, iManuf, iProd, iSerial, bNumConfigs
    ];

    #[test]
    fn configure_endpoint_maps_bulk_out_and_bulk_in_ep_types() {
        let mut ops = MockOps {
            device_descriptor_override: Some(BULK_DEVICE_DESCRIPTOR.to_vec()),
            config_blob_override: Some(BULK_DEVICE_CONFIG_BLOB.to_vec()),
            ..Default::default()
        };
        let ctx = EnumContext {
            speed: Some(PortSpeed::High),
            ep0_ring_iova: 0x0010_0000,
            port: 1,
            ..Default::default()
        };
        let (final_state, _final_ctx) = run_enumeration(EnumState::EnableSlot, ctx, &mut ops);
        assert_eq!(final_state, EnumState::Configured);

        // The last snapshot is ConfigureEndpoint.
        // HS path: [AddressDevice(0), ConfigureEndpoint(1)]
        let cfg_snap = ops
            .ctx_snapshots
            .last()
            .expect("must have configure_endpoint snapshot");

        // Three interface endpoints: DCI 3 (EP1 IN Interrupt), 4 (EP2 OUT Bulk), 5 (EP2 IN Bulk).
        assert_eq!(
            cfg_snap.endpoint_contexts.len(),
            3,
            "must have exactly 3 endpoint contexts"
        );

        // Sort by DCI to get a deterministic lookup order.
        let ep_by_dci = |target_dci: u8| {
            cfg_snap
                .endpoint_contexts
                .iter()
                .find(|e| e.dci == target_dci)
                .unwrap_or_else(|| panic!("no endpoint context with DCI {}", target_dci))
        };

        // --- EP1 IN Interrupt: DCI = 2*1 + 1 = 3 ---
        let ep1_in = ep_by_dci(compute_dci(1, true)); // DCI 3
        assert_eq!(
            ep_type(ep1_in.ep_dword1),
            EP_TYPE_INTERRUPT_IN,
            "EP1 IN must be Interrupt IN"
        );
        assert_eq!(ep_max_packet_size(ep1_in.ep_dword1), 8);

        // --- EP2 OUT Bulk: DCI = 2*2 + 0 = 4 ---
        let ep2_out = ep_by_dci(compute_dci(2, false)); // DCI 4
        assert_eq!(
            ep_type(ep2_out.ep_dword1),
            EP_TYPE_BULK_OUT,
            "EP2 OUT must be Bulk OUT (EP_TYPE_BULK_OUT = {})",
            EP_TYPE_BULK_OUT
        );
        assert_eq!(
            ep_max_packet_size(ep2_out.ep_dword1),
            512,
            "EP2 OUT bulk MPS must be 512"
        );

        // --- EP2 IN Bulk: DCI = 2*2 + 1 = 5 ---
        let ep2_in = ep_by_dci(compute_dci(2, true)); // DCI 5
        assert_eq!(
            ep_type(ep2_in.ep_dword1),
            EP_TYPE_BULK_IN,
            "EP2 IN must be Bulk IN (EP_TYPE_BULK_IN = {})",
            EP_TYPE_BULK_IN
        );
        assert_eq!(
            ep_max_packet_size(ep2_in.ep_dword1),
            512,
            "EP2 IN bulk MPS must be 512"
        );

        // Add Flags: A0 (Slot) + A3 (DCI 3) + A4 (DCI 4) + A5 (DCI 5), NOT A1 (EP0).
        assert_ne!(cfg_snap.add_flags & (1 << 3), 0, "A3 (DCI 3) must be set");
        assert_ne!(cfg_snap.add_flags & (1 << 4), 0, "A4 (DCI 4) must be set");
        assert_ne!(cfg_snap.add_flags & (1 << 5), 0, "A5 (DCI 5) must be set");
        assert_eq!(
            cfg_snap.add_flags & (1 << 1),
            0,
            "A1 (EP0) must NOT be set in Configure Endpoint"
        );
    }

    #[test]
    fn configure_endpoint_add_flags_does_not_include_only_slot_ep0() {
        let (_state, _ctx, ops) = run_fs_keyboard();
        let cfg_snap = ops.ctx_snapshots.last().unwrap();

        // The old broken behaviour was add_flags == 0x3 (Slot+EP0 only).
        // After the fix, bit 3 (DCI 3) must also be set.
        assert_ne!(
            cfg_snap.add_flags, 0x3,
            "Configure Endpoint must add more than just Slot+EP0"
        );
    }

    #[test]
    fn configure_endpoint_add_flags_excludes_ep0_a1() {
        // Per xHCI §4.6.6 / §6.2.5.1, Configure Endpoint must NOT include A1
        // (EP0, bit 1). EP0 was already configured by Address Device; re-adding
        // it causes the controller to validate the stale TR Dequeue Pointer.
        let (_state, _ctx, ops) = run_fs_keyboard();
        let cfg_snap = ops.ctx_snapshots.last().unwrap();

        // A0 (bit 0, Slot) must be set.
        assert_ne!(
            cfg_snap.add_flags & (1 << 0),
            0,
            "Configure Endpoint add_flags must include A0 (Slot); add_flags=0x{:X}",
            cfg_snap.add_flags
        );
        // A1 (bit 1, EP0) must NOT be set.
        assert_eq!(
            cfg_snap.add_flags & (1 << 1),
            0,
            "Configure Endpoint add_flags must NOT include A1 (EP0); add_flags=0x{:X}",
            cfg_snap.add_flags
        );
        // A3 (bit 3, DCI 3 = EP1 IN) must be set.
        assert_ne!(
            cfg_snap.add_flags & (1 << 3),
            0,
            "Configure Endpoint add_flags must include A3 (DCI 3 = EP1 IN); add_flags=0x{:X}",
            cfg_snap.add_flags
        );
    }

    // -----------------------------------------------------------------------
    // m3: Unknown port speed yields Error, not silent Full-speed fallback
    // -----------------------------------------------------------------------

    #[test]
    fn unknown_port_speed_yields_error() {
        let mut ops = MockOps::default();
        let ctx = EnumContext {
            speed: None, // unknown speed
            ep0_ring_iova: 0x0010_0000,
            port: 1,
            ..Default::default()
        };
        let (final_state, _) = run_enumeration(EnumState::EnableSlot, ctx, &mut ops);
        assert!(
            matches!(final_state, EnumState::Error { code: 0xFE }),
            "unknown speed must produce Error{{code: 0xFE}}, got {:?}",
            final_state
        );
    }

    // -----------------------------------------------------------------------
    // m2: SuperSpeed checked_shl does not overflow on raw_mps >= 16
    // -----------------------------------------------------------------------

    #[test]
    fn super_speed_malformed_mps_exponent_does_not_overflow() {
        let mut ops = MockOps::default();
        // Craft a descriptor with raw_mps = 255 (would overflow u16 shift).
        let ss_descriptor: Vec<u8> = vec![
            0x12, 0x01, 0x00, 0x03, 0x00, 0x00, 0x00, 0xFF, // raw_mps = 255 — malformed
            0xB4, 0x04, 0x10, 0x00, 0x00, 0x01, 0x01, 0x02, 0x03, 0x01,
        ];
        ops.device_descriptor_override = Some(ss_descriptor);
        let ctx = EnumContext {
            speed: Some(PortSpeed::Super),
            ep0_ring_iova: 0x0010_0000,
            port: 1,
            ..Default::default()
        };
        // Must not panic; fallback to 512.
        let (final_state, final_ctx) = run_enumeration(EnumState::EnableSlot, ctx, &mut ops);
        assert_eq!(final_state, EnumState::Configured);
        // checked_shl(255) on u16 returns None → fallback is 512.
        assert_eq!(final_ctx.ep0_mps, 512);
    }

    // -----------------------------------------------------------------------
    // m1: GetConfigShort uses explicit config_total_length field
    // -----------------------------------------------------------------------

    #[test]
    fn config_total_length_forwarded_correctly() {
        let (_state, final_ctx, _ops) = run_fs_keyboard();
        // wTotalLength from BOOT_KEYBOARD_CONFIG_BLOB = 0x0022 = 34.
        assert_eq!(
            final_ctx.config_total_length, 34,
            "config_total_length must carry wTotalLength from GetConfigShort"
        );
    }

    // -----------------------------------------------------------------------
    // Existing tests (adapted for new mock signature)
    // -----------------------------------------------------------------------

    #[test]
    fn address_device_input_context_add_flags() {
        let (_state, _ctx, ops) = run_fs_keyboard();
        // The first snapshot (index 0) is from AddressDeviceBsr.
        let snap = &ops.ctx_snapshots[0];
        // Add Flags must be 0x3 (A0 = Slot, A1 = EP0).
        assert_eq!(snap.add_flags, 0x3);
    }

    #[test]
    fn address_device_input_context_ep0_type_and_mps() {
        let (_state, _ctx, ops) = run_fs_keyboard();
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
        let (state, final_ctx, _ops) = run_fs_keyboard();
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
