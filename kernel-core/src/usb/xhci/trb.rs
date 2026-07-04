//! xHCI Transfer Request Block (TRB) layout, encoders/decoders, and the
//! cycle-bit ring state machines (xHCI 1.2b §4.11, §6.4).
//!
//! A TRB is the fundamental work item the driver and controller exchange over
//! the command ring, transfer rings, and event ring. Every TRB is exactly
//! 16 bytes — four little-endian dwords — laid out as:
//!
//! | dword | field                                          |
//! |-------|------------------------------------------------|
//! | 0 + 1 | `parameter` (64-bit, type-specific)            |
//! | 2     | `status`    (32-bit, type-specific)            |
//! | 3     | `control`   (32-bit: type, cycle, flags, ...)  |
//!
//! The `control` dword always carries the **Cycle bit** (bit 0) and the **TRB
//! Type** (bits 15:10); the meaning of the remaining bits depends on the type.
//!
//! This module provides the encoders the driver needs to *produce* command
//! TRBs, the decoders it needs to *consume* event TRBs, the Device Context
//! Index ([`dci`]) formula reused throughout endpoint setup, and the two
//! cycle-bit state machines:
//!
//! * [`ProducerRing`] — the driver's enqueue pointer + producer cycle state for
//!   a command/transfer ring that terminates in a Link TRB.
//! * [`EventConsumer`] — the driver's dequeue pointer + Consumer Cycle State
//!   (CCS) for a (possibly multi-segment) event ring.
//!
//! No MMIO, no DMA: these are plain value types and arithmetic.

// ---------------------------------------------------------------------------
// TRB layout
// ---------------------------------------------------------------------------

/// Size of a single TRB in bytes (xHCI §4.11.1).
pub const TRB_SIZE: usize = 16;

/// A 16-byte Transfer Request Block (xHCI §4.11.1).
///
/// The field grouping matches the in-memory little-endian dword layout exactly:
/// `parameter` covers dwords 0 and 1, `status` covers dword 2, and `control`
/// covers dword 3. This lets the driver `transmute`/copy the struct directly
/// into a DMA-resident ring slot.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
#[repr(C)]
pub struct Trb {
    /// Dwords 0+1 — the TRB Parameter component (e.g. a buffer pointer, a TRB
    /// pointer, or immediate setup data), interpretation depends on the type.
    pub parameter: u64,
    /// Dword 2 — the TRB Status component (transfer length, completion code,
    /// etc.), interpretation depends on the type.
    pub status: u32,
    /// Dword 3 — the TRB Control component. Always holds the Cycle bit (bit 0)
    /// and TRB Type (bits 15:10); remaining bits are type-specific.
    pub control: u32,
}

// Compile-time guard: a TRB is exactly 16 bytes (xHCI §4.11.1).
const _: () = assert!(core::mem::size_of::<Trb>() == TRB_SIZE);

// ---------------------------------------------------------------------------
// Control-dword bit positions / masks
// ---------------------------------------------------------------------------

/// Cycle bit — `control` bit 0 (xHCI §4.9.1). Distinguishes producer-owned from
/// consumer-owned TRBs as the enqueue/dequeue pointers lap the ring.
pub const TRB_CYCLE_BIT: u32 = 1 << 0;
/// Toggle Cycle (TC) bit for Link TRBs — `control` bit 1 (xHCI §6.4.4.1).
pub const TRB_LINK_TOGGLE_CYCLE_BIT: u32 = 1 << 1;
/// Shift of the TRB Type field within `control` (bits 15:10, xHCI §4.11.1).
pub const TRB_TYPE_SHIFT: u32 = 10;
/// Mask of the TRB Type field after shifting (6-bit field).
pub const TRB_TYPE_MASK: u32 = 0x3F;

/// Shift of the Slot Type field in an Enable Slot Command TRB (`control` bits
/// 20:16, xHCI §6.4.3.5).
pub const ENABLE_SLOT_TYPE_SHIFT: u32 = 16;
/// Mask of the Slot Type field (5 bits).
pub const ENABLE_SLOT_TYPE_MASK: u32 = 0x1F;

// ---------------------------------------------------------------------------
// TRB type IDs (xHCI §6.4.6 Table 6-91)
// ---------------------------------------------------------------------------

/// Normal — transfer-ring data TRB.
pub const TRB_TYPE_NORMAL: u8 = 1;
/// Setup Stage — control-transfer setup packet.
pub const TRB_TYPE_SETUP_STAGE: u8 = 2;
/// Data Stage — control-transfer data phase.
pub const TRB_TYPE_DATA_STAGE: u8 = 3;
/// Status Stage — control-transfer status phase.
pub const TRB_TYPE_STATUS_STAGE: u8 = 4;
/// Isoch — isochronous transfer-ring data TRB (xHCI §6.4.1.3). Carries the
/// SIA / Frame ID / TBC / TLBPC fields a periodic isochronous endpoint needs.
pub const TRB_TYPE_ISOCH: u8 = 5;
/// Link — chains ring segments / wraps the ring.
pub const TRB_TYPE_LINK: u8 = 6;
/// No Op (transfer ring).
pub const TRB_TYPE_NO_OP_TRANSFER: u8 = 8;
/// Enable Slot Command.
pub const TRB_TYPE_ENABLE_SLOT: u8 = 9;
/// Disable Slot Command.
pub const TRB_TYPE_DISABLE_SLOT: u8 = 10;
/// Address Device Command.
pub const TRB_TYPE_ADDRESS_DEVICE: u8 = 11;
/// Configure Endpoint Command.
pub const TRB_TYPE_CONFIGURE_ENDPOINT: u8 = 12;
/// Evaluate Context Command.
pub const TRB_TYPE_EVALUATE_CONTEXT: u8 = 13;
/// Reset Endpoint Command (xHCI §4.6.8) — clears a Halted endpoint back to
/// Stopped so its ring can be restarted (the transfer-error / STALL recovery
/// path).
pub const TRB_TYPE_RESET_ENDPOINT: u8 = 14;
/// Stop Endpoint Command (xHCI §4.6.9) — stops a Running endpoint so its
/// ring can be safely repointed (flushing an abandoned/orphaned TD).
pub const TRB_TYPE_STOP_ENDPOINT: u8 = 15;
/// Set TR Dequeue Pointer Command (xHCI §4.6.10) — repoints a Stopped
/// endpoint's transfer-ring dequeue pointer (with its Dequeue Cycle State),
/// discarding everything the controller had not yet consumed.
pub const TRB_TYPE_SET_TR_DEQUEUE: u8 = 16;
/// No Op Command.
pub const TRB_TYPE_NO_OP_COMMAND: u8 = 23;
/// Transfer Event.
pub const TRB_TYPE_TRANSFER_EVENT: u8 = 32;
/// Command Completion Event.
pub const TRB_TYPE_COMMAND_COMPLETION: u8 = 33;
/// Port Status Change Event.
pub const TRB_TYPE_PORT_STATUS_CHANGE: u8 = 34;
/// Host Controller Event.
pub const TRB_TYPE_HOST_CONTROLLER: u8 = 37;

/// Typed enumeration of the TRB types this module recognises (xHCI §6.4.6).
///
/// Only the subset relevant to Phase 78a bring-up is enumerated; unknown raw
/// type values decode to `None` via [`TrbType::from_raw`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum TrbType {
    /// Normal transfer TRB.
    Normal = TRB_TYPE_NORMAL,
    /// Setup Stage TRB.
    SetupStage = TRB_TYPE_SETUP_STAGE,
    /// Data Stage TRB.
    DataStage = TRB_TYPE_DATA_STAGE,
    /// Status Stage TRB.
    StatusStage = TRB_TYPE_STATUS_STAGE,
    /// Isochronous transfer TRB.
    Isoch = TRB_TYPE_ISOCH,
    /// Link TRB.
    Link = TRB_TYPE_LINK,
    /// No Op (transfer ring) TRB.
    NoOpTransfer = TRB_TYPE_NO_OP_TRANSFER,
    /// Enable Slot command.
    EnableSlot = TRB_TYPE_ENABLE_SLOT,
    /// Disable Slot command.
    DisableSlot = TRB_TYPE_DISABLE_SLOT,
    /// Address Device command.
    AddressDevice = TRB_TYPE_ADDRESS_DEVICE,
    /// Configure Endpoint command.
    ConfigureEndpoint = TRB_TYPE_CONFIGURE_ENDPOINT,
    /// Evaluate Context command.
    EvaluateContext = TRB_TYPE_EVALUATE_CONTEXT,
    /// Reset Endpoint command.
    ResetEndpoint = TRB_TYPE_RESET_ENDPOINT,
    /// Stop Endpoint command.
    StopEndpoint = TRB_TYPE_STOP_ENDPOINT,
    /// Set TR Dequeue Pointer command.
    SetTrDequeue = TRB_TYPE_SET_TR_DEQUEUE,
    /// No Op command.
    NoOpCommand = TRB_TYPE_NO_OP_COMMAND,
    /// Transfer Event.
    TransferEvent = TRB_TYPE_TRANSFER_EVENT,
    /// Command Completion Event.
    CommandCompletion = TRB_TYPE_COMMAND_COMPLETION,
    /// Port Status Change Event.
    PortStatusChange = TRB_TYPE_PORT_STATUS_CHANGE,
    /// Host Controller Event.
    HostController = TRB_TYPE_HOST_CONTROLLER,
}

impl TrbType {
    /// Decode a raw 6-bit TRB Type field into a [`TrbType`], or `None` for an
    /// unrecognised value.
    pub const fn from_raw(raw: u8) -> Option<TrbType> {
        match raw {
            TRB_TYPE_NORMAL => Some(TrbType::Normal),
            TRB_TYPE_SETUP_STAGE => Some(TrbType::SetupStage),
            TRB_TYPE_DATA_STAGE => Some(TrbType::DataStage),
            TRB_TYPE_STATUS_STAGE => Some(TrbType::StatusStage),
            TRB_TYPE_ISOCH => Some(TrbType::Isoch),
            TRB_TYPE_LINK => Some(TrbType::Link),
            TRB_TYPE_NO_OP_TRANSFER => Some(TrbType::NoOpTransfer),
            TRB_TYPE_ENABLE_SLOT => Some(TrbType::EnableSlot),
            TRB_TYPE_DISABLE_SLOT => Some(TrbType::DisableSlot),
            TRB_TYPE_ADDRESS_DEVICE => Some(TrbType::AddressDevice),
            TRB_TYPE_CONFIGURE_ENDPOINT => Some(TrbType::ConfigureEndpoint),
            TRB_TYPE_EVALUATE_CONTEXT => Some(TrbType::EvaluateContext),
            TRB_TYPE_RESET_ENDPOINT => Some(TrbType::ResetEndpoint),
            TRB_TYPE_STOP_ENDPOINT => Some(TrbType::StopEndpoint),
            TRB_TYPE_SET_TR_DEQUEUE => Some(TrbType::SetTrDequeue),
            TRB_TYPE_NO_OP_COMMAND => Some(TrbType::NoOpCommand),
            TRB_TYPE_TRANSFER_EVENT => Some(TrbType::TransferEvent),
            TRB_TYPE_COMMAND_COMPLETION => Some(TrbType::CommandCompletion),
            TRB_TYPE_PORT_STATUS_CHANGE => Some(TrbType::PortStatusChange),
            TRB_TYPE_HOST_CONTROLLER => Some(TrbType::HostController),
            _ => None,
        }
    }

    /// The raw 6-bit type value for this variant.
    pub const fn raw(self) -> u8 {
        self as u8
    }
}

// ---------------------------------------------------------------------------
// Completion codes (xHCI §6.4.5 Table 6-90) — only the values needed so far
// ---------------------------------------------------------------------------

/// Success completion code.
pub const COMPLETION_SUCCESS: u8 = 1;

/// Short Packet completion code (xHCI §6.4.5) — the device transferred fewer
/// bytes than the TRB length; the residual reports how many were missing.
pub const COMPLETION_SHORT_PACKET: u8 = 13;

/// Missed Service Error completion code (xHCI §6.4.5) — an isochronous service
/// interval elapsed before the controller could service the endpoint. Isoch
/// has no retry, so the affected interval's data is simply dropped; the driver
/// resynchronises on the next interval rather than treating it as fatal.
pub const COMPLETION_MISSED_SERVICE_ERROR: u8 = 26;

/// Context State Error completion code (xHCI §6.4.5) — the command targeted
/// an endpoint whose state didn't require it (e.g. Stop Endpoint on an
/// already-Stopped/Halted endpoint, Reset Endpoint on a non-Halted one). The
/// endpoint-recovery sequence tolerates this: it means that step was simply
/// unnecessary, not that recovery failed.
pub const COMPLETION_CONTEXT_STATE_ERROR: u8 = 19;

// ---------------------------------------------------------------------------
// Generic field accessors
// ---------------------------------------------------------------------------

/// Read the Cycle bit (`control` bit 0) of a TRB.
pub const fn trb_cycle(trb: &Trb) -> bool {
    trb.control & TRB_CYCLE_BIT != 0
}

/// Read the raw 6-bit TRB Type field (`control` bits 15:10) of a TRB.
pub const fn trb_type_raw(trb: &Trb) -> u8 {
    ((trb.control >> TRB_TYPE_SHIFT) & TRB_TYPE_MASK) as u8
}

/// Compose a `control` dword's TRB-Type + Cycle bits.
const fn control_type_cycle(trb_type: u8, cycle: bool) -> u32 {
    ((trb_type as u32) << TRB_TYPE_SHIFT) | (cycle as u32)
}

// ---------------------------------------------------------------------------
// Encoders
// ---------------------------------------------------------------------------

impl Trb {
    /// Build a **Link TRB** (xHCI §6.4.4.1).
    ///
    /// `next_segment_iova` is the (already 16-byte aligned) device address of
    /// the next ring segment — it occupies the whole `parameter` field.
    /// `toggle_cycle` sets the Toggle Cycle (TC) flag (`control` bit 1), which
    /// the controller uses to flip its consumer cycle state when it follows
    /// this link; the driver sets it on the link that wraps to the start of the
    /// ring. `cycle` is the producer cycle bit to stamp.
    pub const fn link(next_segment_iova: u64, toggle_cycle: bool, cycle: bool) -> Trb {
        let control = control_type_cycle(TRB_TYPE_LINK, cycle)
            | if toggle_cycle {
                TRB_LINK_TOGGLE_CYCLE_BIT
            } else {
                0
            };
        Trb {
            parameter: next_segment_iova,
            status: 0,
            control,
        }
    }

    /// Build an **Enable Slot Command TRB** (xHCI §6.4.3.5).
    ///
    /// `slot_type` is the protocol Slot Type (`control` bits 20:16), obtained
    /// from the Supported Protocol extended capability for the port's protocol
    /// (typically 0 for USB). `cycle` is the producer cycle bit.
    pub const fn enable_slot(slot_type: u8, cycle: bool) -> Trb {
        let control = ((slot_type as u32 & ENABLE_SLOT_TYPE_MASK) << ENABLE_SLOT_TYPE_SHIFT)
            | control_type_cycle(TRB_TYPE_ENABLE_SLOT, cycle);
        Trb {
            parameter: 0,
            status: 0,
            control,
        }
    }

    /// Build a **Disable Slot Command TRB** (xHCI §6.4.3.6).
    ///
    /// Frees the device slot `slot_id` that Enable Slot allocated: the
    /// controller releases the slot's Device Context and returns the slot to
    /// the available pool. The matching teardown for [`Trb::enable_slot`] — used
    /// on hot-plug detach and re-enumeration so slot IDs / DCBAA entries are not
    /// leaked (Phase 92 Track H.3). `cycle` is the producer cycle bit.
    pub const fn disable_slot(slot_id: u8, cycle: bool) -> Trb {
        Trb {
            parameter: 0,
            status: 0,
            control: control_type_cycle(TRB_TYPE_DISABLE_SLOT, cycle)
                | ((slot_id as u32) << CMD_SLOT_ID_SHIFT),
        }
    }

    /// Build a **No Op Command TRB** (xHCI §6.4.3.10) — used during bring-up to
    /// confirm the command ring and event ring are wired up correctly.
    pub const fn no_op_command(cycle: bool) -> Trb {
        Trb {
            parameter: 0,
            status: 0,
            control: control_type_cycle(TRB_TYPE_NO_OP_COMMAND, cycle),
        }
    }
}

// ---------------------------------------------------------------------------
// SetupPacket — USB SETUP transaction payload (USB 2.0 §9.3)
// ---------------------------------------------------------------------------

/// Standard USB SETUP packet (8 bytes), as defined in USB 2.0 §9.3.
///
/// xHCI embeds the 8 setup bytes into the `parameter` field of a Setup Stage
/// TRB (the low 32 bits carry bytes 0–3, the high 32 bits carry bytes 4–7).
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct SetupPacket {
    /// `bmRequestType` (byte 0): direction, type, and recipient.
    pub bm_request_type: u8,
    /// `bRequest` (byte 1): the request identifier.
    pub b_request: u8,
    /// `wValue` (bytes 2–3, little-endian): request-specific value.
    pub w_value: u16,
    /// `wIndex` (bytes 4–5, little-endian): request-specific index.
    pub w_index: u16,
    /// `wLength` (bytes 6–7, little-endian): expected data-stage byte count.
    pub w_length: u16,
}

// --- bmRequestType bit fields (USB 2.0 §9.3 Table 9-2) ---

/// `bmRequestType` for a Host-to-Device (OUT) Standard Device request.
pub const BM_REQUEST_TYPE_H2D_STD_DEV: u8 = 0x00;
/// `bmRequestType` for a Device-to-Host (IN) Standard Device request.
pub const BM_REQUEST_TYPE_D2H_STD_DEV: u8 = 0x80;

// --- Standard bRequest codes (USB 2.0 §9.4 Table 9-4) ---

/// `bRequest` for GET_DESCRIPTOR.
pub const B_REQUEST_GET_DESCRIPTOR: u8 = 0x06;
/// `bRequest` for SET_CONFIGURATION.
pub const B_REQUEST_SET_CONFIGURATION: u8 = 0x09;

// --- Standard descriptor type codes (USB 2.0 §9.4.3 Table 9-5) ---

/// Descriptor type for Device Descriptor.
pub const DESCRIPTOR_TYPE_DEVICE: u8 = 0x01;
/// Descriptor type for Configuration Descriptor.
pub const DESCRIPTOR_TYPE_CONFIGURATION: u8 = 0x02;

impl SetupPacket {
    /// Encode the packet as a little-endian 64-bit value suitable for the
    /// `parameter` field of a Setup Stage TRB (xHCI §6.4.1.2.1).
    ///
    /// Layout:
    /// * Bits  7:0  — `bmRequestType`
    /// * Bits 15:8  — `bRequest`
    /// * Bits 31:16 — `wValue`
    /// * Bits 47:32 — `wIndex`
    /// * Bits 63:48 — `wLength`
    pub const fn as_u64(self) -> u64 {
        (self.bm_request_type as u64)
            | ((self.b_request as u64) << 8)
            | ((self.w_value as u64) << 16)
            | ((self.w_index as u64) << 32)
            | ((self.w_length as u64) << 48)
    }

    /// Build a `GET_DESCRIPTOR(Device)` request (USB 2.0 §9.4.3).
    ///
    /// Requests `length` bytes of the Device Descriptor; 18 is the full size.
    pub const fn get_device_descriptor(length: u16) -> Self {
        SetupPacket {
            bm_request_type: BM_REQUEST_TYPE_D2H_STD_DEV,
            b_request: B_REQUEST_GET_DESCRIPTOR,
            w_value: (DESCRIPTOR_TYPE_DEVICE as u16) << 8,
            w_index: 0,
            w_length: length,
        }
    }

    /// Build a `GET_DESCRIPTOR(Configuration, index)` request (USB 2.0 §9.4.3).
    ///
    /// `index` selects which configuration (0-based). Requests `length` bytes;
    /// a first short read typically requests 9 bytes to learn `wTotalLength`.
    pub const fn get_config_descriptor(index: u8, length: u16) -> Self {
        SetupPacket {
            bm_request_type: BM_REQUEST_TYPE_D2H_STD_DEV,
            b_request: B_REQUEST_GET_DESCRIPTOR,
            w_value: ((DESCRIPTOR_TYPE_CONFIGURATION as u16) << 8) | (index as u16),
            w_index: 0,
            w_length: length,
        }
    }

    /// Build a `SET_CONFIGURATION` request (USB 2.0 §9.4.7).
    ///
    /// `value` is the `bConfigurationValue` from the desired Configuration
    /// Descriptor. Typically 1 for single-configuration devices.
    pub const fn set_configuration(value: u8) -> Self {
        SetupPacket {
            bm_request_type: BM_REQUEST_TYPE_H2D_STD_DEV,
            b_request: B_REQUEST_SET_CONFIGURATION,
            w_value: value as u16,
            w_index: 0,
            w_length: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Transfer TRB control-dword bits (xHCI §6.4.1)
// ---------------------------------------------------------------------------

/// Shift of the Transfer Type field in a Setup Stage TRB control dword (bits
/// 17:16, xHCI §6.4.1.2.1). Encodes the expected data-stage direction: 0 = no
/// data, 2 = OUT, 3 = IN.
const SETUP_TT_SHIFT: u32 = 16;
/// Transfer Type — No Data Stage (control write with no data phase).
pub const SETUP_TT_NO_DATA: u32 = 0;
/// Transfer Type — OUT Data Stage.
pub const SETUP_TT_OUT: u32 = 2;
/// Transfer Type — IN Data Stage.
pub const SETUP_TT_IN: u32 = 3;

/// Immediate Data bit in Setup Stage TRB control dword (bit 6, xHCI §6.4.1.2.1).
/// Must be set to 1 for Setup Stage TRBs so the controller reads the setup
/// data directly from the TRB `parameter` field.
const SETUP_IDT_BIT: u32 = 1 << 6;

/// Shift of the Transfer Length field in a Setup Stage TRB status dword (bits
/// 16:0, xHCI §6.4.1.2.1). For Setup Stage TRBs this is always 8.
const SETUP_TRB_LENGTH: u32 = 8;

/// Direction bit in a Data Stage TRB control dword (bit 16, xHCI §6.4.1.2.2).
/// `1` = IN (device-to-host), `0` = OUT (host-to-device).
const DATA_DIR_BIT: u32 = 1 << 16;

/// Mask of the TRB Transfer Length field in a Data or Normal TRB status dword
/// (bits 16:0, xHCI §6.4.1.1/1.2.2). For a single-TRB transfer the TD Size
/// field (bits 21:17) and Interrupter Target (bits 31:22) are left zero, so
/// masking `len` to the low 17 bits yields TD Size = 0 as required.
const DATA_TRB_TRANSFER_LENGTH_MASK: u32 = 0x0001_FFFF;

// ---------------------------------------------------------------------------
// Command TRB control-dword bits (xHCI §6.4.3)
// ---------------------------------------------------------------------------

/// Shift of the Slot ID in a command TRB control dword (bits 31:24).
const CMD_SLOT_ID_SHIFT: u32 = 24;
/// Shift of the Endpoint ID (DCI) in an endpoint-targeting command TRB
/// control dword (bits 20:16 — Stop Endpoint / Reset Endpoint / Set TR
/// Dequeue Pointer, xHCI §6.4.3).
const CMD_ENDPOINT_ID_SHIFT: u32 = 16;
/// BSR (Block Set Address Request) bit in an Address Device Command TRB
/// control dword (bit 9, xHCI §6.4.3.4). When set the controller skips
/// assigning a USB address; used during the EP0 MPS two-step.
const ADDRESS_DEVICE_BSR_BIT: u32 = 1 << 9;

// ---------------------------------------------------------------------------
// TRB builders — control-transfer stage TRBs (xHCI §6.4.1)
// ---------------------------------------------------------------------------

impl Trb {
    /// Build a **Setup Stage TRB** (xHCI §6.4.1.2.1) for the EP0 transfer ring.
    ///
    /// `setup` is encoded directly into `parameter`. `transfer_type` selects
    /// the data-stage direction: use [`SETUP_TT_IN`], [`SETUP_TT_OUT`], or
    /// [`SETUP_TT_NO_DATA`]. `cycle` is the producer cycle bit.
    pub const fn setup_stage(setup: &SetupPacket, transfer_type: u32, cycle: bool) -> Trb {
        Trb {
            parameter: setup.as_u64(),
            status: SETUP_TRB_LENGTH,
            control: control_type_cycle(TRB_TYPE_SETUP_STAGE, cycle)
                | SETUP_IDT_BIT
                | ((transfer_type & 0x3) << SETUP_TT_SHIFT),
        }
    }

    /// Build a **Data Stage TRB** (xHCI §6.4.1.2.2) for the EP0 transfer ring.
    ///
    /// `buf_iova` is the device-visible address of the data buffer. `len` is
    /// the number of bytes to transfer. `dir_in` selects the direction
    /// (`true` = IN / device-to-host). `cycle` is the producer cycle bit.
    pub const fn data_stage(buf_iova: u64, len: u32, dir_in: bool, cycle: bool) -> Trb {
        Trb {
            parameter: buf_iova,
            status: len & DATA_TRB_TRANSFER_LENGTH_MASK,
            control: control_type_cycle(TRB_TYPE_DATA_STAGE, cycle)
                | if dir_in { DATA_DIR_BIT } else { 0 },
        }
    }

    /// Build a **Status Stage TRB** (xHCI §6.4.1.2.3) for the EP0 transfer ring.
    ///
    /// `dir_in` should be the **opposite** direction of the data stage (`true`
    /// for an OUT status after an IN data stage, i.e. a handshake toward the
    /// host). For a no-data-stage transfer, `dir_in = true` (xHCI §4.11.2.2).
    /// `cycle` is the producer cycle bit.
    ///
    /// The IOC (Interrupt On Completion) bit is always set so the controller
    /// generates a Transfer Event when the status phase completes (xHCI
    /// §6.4.1.2.3). Do **not** set IOC on Setup or Data Stage TRBs — only
    /// the terminal Status Stage needs it.
    pub const fn status_stage(dir_in: bool, cycle: bool) -> Trb {
        const IOC_BIT: u32 = 1 << 5;
        Trb {
            parameter: 0,
            status: 0,
            control: control_type_cycle(TRB_TYPE_STATUS_STAGE, cycle)
                | if dir_in { DATA_DIR_BIT } else { 0 }
                | IOC_BIT,
        }
    }

    /// Build a **Normal TRB** (xHCI §6.4.1.1) for a bulk or interrupt transfer
    /// ring.
    ///
    /// `buf_iova` is the device-visible address of the transfer buffer and
    /// `len` the number of bytes the controller may write (for an IN endpoint)
    /// or read (OUT). `cycle` is the producer cycle bit. Both **IOC** (Interrupt
    /// On Completion, bit 5) and **ISP** (Interrupt on Short Packet, bit 2) are
    /// set so the controller posts a Transfer Event when the endpoint completes
    /// — including a short report, where the residual reports the byte count.
    /// This is the TRB the HID interrupt-IN poll uses to receive boot reports.
    pub const fn normal(buf_iova: u64, len: u32, cycle: bool) -> Trb {
        const IOC_BIT: u32 = 1 << 5;
        const ISP_BIT: u32 = 1 << 2;
        Trb {
            parameter: buf_iova,
            status: len & DATA_TRB_TRANSFER_LENGTH_MASK,
            control: control_type_cycle(TRB_TYPE_NORMAL, cycle) | IOC_BIT | ISP_BIT,
        }
    }

    /// Build an **Isochronous TRB** (xHCI §6.4.1.3) for an isochronous transfer
    /// ring (USB audio / video).
    ///
    /// `buf_iova` is the device-visible address of the PCM / frame buffer and
    /// `len` the byte count for this service interval. When `sia` is `true` the
    /// **SIA** (Start Isoch ASAP, control bit 31) flag is set and the controller
    /// schedules the TD on the next available (micro)frame — `frame_id` is
    /// ignored. When `sia` is `false` the 11-bit **Frame ID** (control bits
    /// 30:20) selects the target (micro)frame for precise scheduling. **IOC** is
    /// set so each interval posts a Transfer Event; **ISP** is set so a short IN
    /// transfer (UVC frame end) still reports its residual. TBC (bits 8:7) and
    /// TLBPC (bits 19:16) are left zero — correct for full-speed single-packet
    /// service (no bursts); a high-speed/SuperSpeed bursting endpoint would set
    /// them from `bMaxBurst`/`Mult`.
    pub const fn isoch(buf_iova: u64, len: u32, frame_id: u16, sia: bool, cycle: bool) -> Trb {
        const IOC_BIT: u32 = 1 << 5;
        const ISP_BIT: u32 = 1 << 2;
        const SIA_BIT: u32 = 1 << 31;
        const FRAME_ID_SHIFT: u32 = 20;
        const FRAME_ID_MASK: u32 = 0x7FF; // 11-bit field
        let sched = if sia {
            SIA_BIT
        } else {
            ((frame_id as u32) & FRAME_ID_MASK) << FRAME_ID_SHIFT
        };
        Trb {
            parameter: buf_iova,
            status: len & DATA_TRB_TRANSFER_LENGTH_MASK,
            control: control_type_cycle(TRB_TYPE_ISOCH, cycle) | IOC_BIT | ISP_BIT | sched,
        }
    }

    // -----------------------------------------------------------------------
    // Command TRB builders (xHCI §6.4.3)
    // -----------------------------------------------------------------------

    /// Build an **Address Device Command TRB** (xHCI §6.4.3.4).
    ///
    /// `input_ctx_iova` is the device-visible address of the Input Context.
    /// `slot_id` identifies the slot (assigned by Enable Slot). `bsr = true`
    /// sets the Block Set Address Request flag so the controller updates the
    /// slot state without assigning a USB address — used during the two-step
    /// EP0 Max Packet Size negotiation. `cycle` is the producer cycle bit.
    pub const fn address_device(input_ctx_iova: u64, slot_id: u8, bsr: bool, cycle: bool) -> Trb {
        Trb {
            parameter: input_ctx_iova,
            status: 0,
            control: control_type_cycle(TRB_TYPE_ADDRESS_DEVICE, cycle)
                | ((slot_id as u32) << CMD_SLOT_ID_SHIFT)
                | if bsr { ADDRESS_DEVICE_BSR_BIT } else { 0 },
        }
    }

    /// Build a **Configure Endpoint Command TRB** (xHCI §6.4.3.5).
    ///
    /// `input_ctx_iova` is the device-visible address of the Input Context
    /// describing the endpoints to add or drop. `slot_id` is the target device
    /// slot. `cycle` is the producer cycle bit.
    pub const fn configure_endpoint(input_ctx_iova: u64, slot_id: u8, cycle: bool) -> Trb {
        Trb {
            parameter: input_ctx_iova,
            status: 0,
            control: control_type_cycle(TRB_TYPE_CONFIGURE_ENDPOINT, cycle)
                | ((slot_id as u32) << CMD_SLOT_ID_SHIFT),
        }
    }

    /// Build an **Evaluate Context Command TRB** (xHCI §6.4.3.6).
    ///
    /// Used to update the EP0 Max Packet Size after reading the first 8 bytes
    /// of the Device Descriptor (the BSR two-step). `input_ctx_iova` is the
    /// Input Context address; `slot_id` is the target slot.
    pub const fn evaluate_context(input_ctx_iova: u64, slot_id: u8, cycle: bool) -> Trb {
        Trb {
            parameter: input_ctx_iova,
            status: 0,
            control: control_type_cycle(TRB_TYPE_EVALUATE_CONTEXT, cycle)
                | ((slot_id as u32) << CMD_SLOT_ID_SHIFT),
        }
    }

    /// Build a **Stop Endpoint Command TRB** (xHCI §6.4.3.3) for (`slot_id`,
    /// `dci`). Stops a Running endpoint so its ring can be repointed; part of
    /// the transfer-abandonment recovery path (SP bit left 0 — a full stop,
    /// not suspend).
    pub const fn stop_endpoint(slot_id: u8, dci: u8, cycle: bool) -> Trb {
        Trb {
            parameter: 0,
            status: 0,
            control: control_type_cycle(TRB_TYPE_STOP_ENDPOINT, cycle)
                | ((dci as u32) << CMD_ENDPOINT_ID_SHIFT)
                | ((slot_id as u32) << CMD_SLOT_ID_SHIFT),
        }
    }

    /// Build a **Reset Endpoint Command TRB** (xHCI §6.4.3.4) for (`slot_id`,
    /// `dci`). Clears a Halted endpoint (a device STALL halts it) back to
    /// Stopped. TSP is left 0 so the endpoint's transfer state (data
    /// toggle / sequence number) is reset — the pairing behaviour for a
    /// device-side `CLEAR_FEATURE(ENDPOINT_HALT)`, which resets the device's
    /// toggle too.
    pub const fn reset_endpoint(slot_id: u8, dci: u8, cycle: bool) -> Trb {
        Trb {
            parameter: 0,
            status: 0,
            control: control_type_cycle(TRB_TYPE_RESET_ENDPOINT, cycle)
                | ((dci as u32) << CMD_ENDPOINT_ID_SHIFT)
                | ((slot_id as u32) << CMD_SLOT_ID_SHIFT),
        }
    }

    /// Build a **Set TR Dequeue Pointer Command TRB** (xHCI §6.4.3.9) for
    /// (`slot_id`, `dci`): repoint the Stopped endpoint's dequeue to
    /// `new_dequeue_iova` with Dequeue Cycle State `dcs`, discarding every
    /// TD the controller had not yet consumed (the orphan-flush half of the
    /// recovery path).
    pub const fn set_tr_dequeue(
        new_dequeue_iova: u64,
        dcs: bool,
        slot_id: u8,
        dci: u8,
        cycle: bool,
    ) -> Trb {
        Trb {
            parameter: (new_dequeue_iova & TRB_POINTER_MASK) | dcs as u64,
            status: 0,
            control: control_type_cycle(TRB_TYPE_SET_TR_DEQUEUE, cycle)
                | ((dci as u32) << CMD_ENDPOINT_ID_SHIFT)
                | ((slot_id as u32) << CMD_SLOT_ID_SHIFT),
        }
    }
}

// ---------------------------------------------------------------------------
// Event decoders
// ---------------------------------------------------------------------------

/// Mask that clears the low 4 bits of a TRB pointer — TRB pointers are always
/// 16-byte aligned, and the low nibble is reserved in event TRBs.
const TRB_POINTER_MASK: u64 = !0xF;
/// Shift of the Completion Code within an event TRB's `status` dword (bits
/// 31:24, xHCI §6.4.2).
const COMPLETION_CODE_SHIFT: u32 = 24;
/// Mask of the residual / remaining Transfer Length in a Transfer Event's
/// `status` dword (bits 23:0).
const TRANSFER_LENGTH_MASK: u32 = 0x00FF_FFFF;
/// Shift of the Slot ID within an event TRB's `control` dword (bits 31:24).
const SLOT_ID_SHIFT: u32 = 24;
/// Shift of the Endpoint ID within a Transfer Event's `control` dword (bits
/// 20:16).
const ENDPOINT_ID_SHIFT: u32 = 16;
/// Mask of the Endpoint ID field (5 bits).
const ENDPOINT_ID_MASK: u32 = 0x1F;
/// Shift of the Port ID within a Port Status Change Event's `parameter` field
/// (bits 31:24 of the low dword).
const PORT_ID_SHIFT: u64 = 24;

/// Decoded **Command Completion Event** (xHCI §6.4.2.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandCompletionEvent {
    /// Device address of the Command TRB that generated this event
    /// (`parameter` with the low nibble masked off).
    pub command_trb_pointer: u64,
    /// Completion Code (`status` bits 31:24). [`COMPLETION_SUCCESS`] means OK.
    pub completion_code: u8,
    /// Slot ID the command targeted (`control` bits 31:24).
    pub slot_id: u8,
    /// Cycle bit of the event TRB (`control` bit 0).
    pub cycle: bool,
}

/// Decoded **Transfer Event** (xHCI §6.4.2.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransferEvent {
    /// Device address of the transfer TRB (or the buffer, for event-data TRBs)
    /// that generated this event (`parameter` with the low nibble masked off).
    pub trb_pointer: u64,
    /// Residual / remaining transfer length (`status` bits 23:0).
    pub residual_transfer_length: u32,
    /// Completion Code (`status` bits 31:24).
    pub completion_code: u8,
    /// Endpoint ID / Device Context Index that produced the event (`control`
    /// bits 20:16).
    pub endpoint_id: u8,
    /// Slot ID owning the endpoint (`control` bits 31:24).
    pub slot_id: u8,
    /// Cycle bit of the event TRB (`control` bit 0).
    pub cycle: bool,
}

/// Decoded **Port Status Change Event** (xHCI §6.4.2.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortStatusChangeEvent {
    /// 1-based root-hub port number whose status changed (`parameter` bits
    /// 31:24 of the low dword).
    pub port_id: u8,
    /// Completion Code (`status` bits 31:24).
    pub completion_code: u8,
    /// Cycle bit of the event TRB (`control` bit 0).
    pub cycle: bool,
}

/// Decode a TRB as a [`CommandCompletionEvent`].
///
/// The caller is responsible for first confirming the TRB type (see
/// [`event_trb_type`]); this function does not validate it.
pub const fn parse_command_completion(trb: &Trb) -> CommandCompletionEvent {
    CommandCompletionEvent {
        command_trb_pointer: trb.parameter & TRB_POINTER_MASK,
        completion_code: (trb.status >> COMPLETION_CODE_SHIFT) as u8,
        slot_id: (trb.control >> SLOT_ID_SHIFT) as u8,
        cycle: trb_cycle(trb),
    }
}

/// Decode a TRB as a [`TransferEvent`].
pub const fn parse_transfer_event(trb: &Trb) -> TransferEvent {
    TransferEvent {
        trb_pointer: trb.parameter & TRB_POINTER_MASK,
        residual_transfer_length: trb.status & TRANSFER_LENGTH_MASK,
        completion_code: (trb.status >> COMPLETION_CODE_SHIFT) as u8,
        endpoint_id: ((trb.control >> ENDPOINT_ID_SHIFT) & ENDPOINT_ID_MASK) as u8,
        slot_id: (trb.control >> SLOT_ID_SHIFT) as u8,
        cycle: trb_cycle(trb),
    }
}

/// Decode a TRB as a [`PortStatusChangeEvent`].
pub const fn parse_port_status_change(trb: &Trb) -> PortStatusChangeEvent {
    PortStatusChangeEvent {
        port_id: ((trb.parameter >> PORT_ID_SHIFT) & 0xFF) as u8,
        completion_code: (trb.status >> COMPLETION_CODE_SHIFT) as u8,
        cycle: trb_cycle(trb),
    }
}

/// Decode the type of an event TRB pulled off the event ring, or `None` for an
/// unrecognised type value.
pub const fn event_trb_type(trb: &Trb) -> Option<TrbType> {
    TrbType::from_raw(trb_type_raw(trb))
}

// ---------------------------------------------------------------------------
// Device Context Index (DCI) — xHCI §4.5.1
// ---------------------------------------------------------------------------

/// Compute the **Device Context Index** for an endpoint (xHCI §4.5.1).
///
/// The default control endpoint (endpoint number 0) always maps to DCI 1.
/// For every other endpoint, the DCI is `2 * endpoint_number + direction`,
/// where `direction` is 1 for IN and 0 for OUT. DCI 0 is reserved for the
/// Slot Context.
///
/// Examples: ep0 → 1; ep1 OUT → 2; ep1 IN → 3; ep2 IN → 5; ep15 IN → 31.
pub const fn dci(endpoint_number: u8, direction_in: bool) -> u8 {
    if endpoint_number == 0 {
        1
    } else {
        2 * endpoint_number + direction_in as u8
    }
}

// ---------------------------------------------------------------------------
// Producer cycle logic — command / transfer rings (xHCI §4.9.2)
// ---------------------------------------------------------------------------

/// Enqueue-pointer + producer-cycle state for a command or transfer ring whose
/// final slot is a Link TRB pointing back to the start.
///
/// The driver writes new TRBs at `enqueue` stamped with the current `cycle`
/// bit, then calls [`ProducerRing::advance`]. When the enqueue pointer reaches
/// the Link slot (the last slot, index `size - 1`), the Link TRB's Toggle Cycle
/// flag means the producer wraps to slot 0 and flips its cycle bit, so the
/// controller can always tell freshly-written TRBs from stale ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProducerRing {
    /// Total number of TRB slots in the ring, **including** the trailing Link
    /// TRB.
    pub size: usize,
    /// Index of the slot the next TRB will be written to.
    pub enqueue: usize,
    /// Producer Cycle State — the Cycle bit value to stamp on the next TRB.
    pub cycle: bool,
}

impl ProducerRing {
    /// Create a fresh producer ring. The producer cycle bit starts at `true`
    /// (1), matching the controller's initial Consumer Cycle State after reset
    /// (xHCI §4.9.2).
    pub const fn new(size: usize) -> ProducerRing {
        ProducerRing {
            size,
            enqueue: 0,
            cycle: true,
        }
    }

    /// Index of the Link-TRB slot (the last slot in the ring).
    pub const fn link_index(&self) -> usize {
        self.size - 1
    }

    /// Advance the enqueue pointer by one slot.
    ///
    /// If the pointer reaches the Link slot, it wraps back to slot 0 and the
    /// producer cycle bit toggles (the driver having set Toggle Cycle on the
    /// Link TRB). Returns `true` when a wrap occurred.
    pub fn advance(&mut self) -> bool {
        self.enqueue += 1;
        if self.enqueue >= self.link_index() {
            self.enqueue = 0;
            self.cycle = !self.cycle;
            true
        } else {
            false
        }
    }
}

// ---------------------------------------------------------------------------
// Event-ring consumer cycle logic (xHCI §4.9.4)
// ---------------------------------------------------------------------------

/// Maximum number of Event Ring Segment Table (ERST) segments this consumer
/// supports. Phase 78a only needs single- and dual-segment rings; the array is
/// sized to cover both without heap allocation.
pub const MAX_EVENT_SEGMENTS: usize = 2;

/// Dequeue-pointer + Consumer Cycle State (CCS) for the event ring (xHCI
/// §4.9.4).
///
/// The event ring is a list of one or more contiguous segments described by the
/// Event Ring Segment Table. Unlike command/transfer rings it has **no Link
/// TRBs**: the consumer walks each segment in order and, after the last TRB of
/// the last segment, wraps to the first TRB of the first segment and **toggles
/// CCS**. A TRB is only valid (controller-written) when its Cycle bit equals
/// the current CCS.
///
/// Crucially, crossing an *intra-table* segment boundary (from one segment to
/// the next within the table) advances the segment index but does **not**
/// toggle CCS — only wrapping past the final segment does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EventConsumer {
    /// Number of TRBs in each segment, indexed by segment.
    seg_sizes: [usize; MAX_EVENT_SEGMENTS],
    /// Number of valid entries in `seg_sizes` (1 or 2).
    seg_count: usize,
    /// Index of the segment the dequeue pointer is currently in.
    pub segment: usize,
    /// Index of the TRB within the current segment.
    pub index: usize,
    /// Consumer Cycle State — a TRB is valid only when its Cycle bit equals
    /// this. Starts at `true` (1) after reset.
    pub ccs: bool,
}

impl EventConsumer {
    /// Create an event-ring consumer over the given per-segment TRB counts.
    ///
    /// `seg_sizes` must contain 1 or 2 entries (single- or dual-segment ring);
    /// extra entries beyond [`MAX_EVENT_SEGMENTS`] are ignored. The dequeue
    /// pointer starts at segment 0 / index 0 with CCS = 1, matching the
    /// controller's post-reset state (xHCI §4.9.4).
    pub fn new(seg_sizes: &[usize]) -> EventConsumer {
        let seg_count = core::cmp::min(seg_sizes.len(), MAX_EVENT_SEGMENTS);
        let mut sizes = [0usize; MAX_EVENT_SEGMENTS];
        let mut i = 0;
        while i < seg_count {
            sizes[i] = seg_sizes[i];
            i += 1;
        }
        EventConsumer {
            seg_sizes: sizes,
            seg_count,
            segment: 0,
            index: 0,
            ccs: true,
        }
    }

    /// Number of TRBs in the segment the dequeue pointer currently sits in.
    const fn current_segment_size(&self) -> usize {
        self.seg_sizes[self.segment]
    }

    /// Whether the dequeue pointer is in the last segment of the ring.
    const fn in_last_segment(&self) -> bool {
        self.segment + 1 >= self.seg_count
    }

    /// Determine whether the TRB the dequeue pointer currently references is
    /// owned by the consumer, i.e. its Cycle bit matches the current CCS.
    /// The driver calls this before processing a TRB and stops when it returns
    /// `false`.
    pub const fn owns(&self, trb: &Trb) -> bool {
        trb_cycle(trb) == self.ccs
    }

    /// Advance the dequeue pointer past the current TRB.
    ///
    /// Moves to the next TRB within the current segment; at a segment boundary
    /// it advances to the next segment (CCS unchanged); past the final segment
    /// it wraps to segment 0 / index 0 and **toggles CCS**. Returns `true` only
    /// when the full-ring wrap (and CCS toggle) occurred.
    pub fn dequeue_step(&mut self) -> bool {
        self.index += 1;
        if self.index < self.current_segment_size() {
            // Still inside the current segment.
            return false;
        }
        // Reached the end of the current segment.
        if self.in_last_segment() {
            // Past the last segment: wrap to the start and toggle CCS.
            self.segment = 0;
            self.index = 0;
            self.ccs = !self.ccs;
            true
        } else {
            // Intra-table boundary: move to the next segment, keep CCS.
            self.segment += 1;
            self.index = 0;
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trb_is_16_bytes() {
        assert_eq!(TRB_SIZE, 16);
        assert_eq!(core::mem::size_of::<Trb>(), 16);
    }

    #[test]
    fn cycle_and_type_accessors() {
        let trb = Trb {
            parameter: 0,
            status: 0,
            // type = 9 (Enable Slot) in bits 15:10, cycle = 1.
            control: (9u32 << TRB_TYPE_SHIFT) | 1,
        };
        assert!(trb_cycle(&trb));
        assert_eq!(trb_type_raw(&trb), 9);
        assert_eq!(event_trb_type(&trb), Some(TrbType::EnableSlot));

        let no_cycle = Trb {
            control: 9u32 << TRB_TYPE_SHIFT,
            ..Default::default()
        };
        assert!(!trb_cycle(&no_cycle));
    }

    #[test]
    fn trbtype_roundtrip() {
        let all = [
            TrbType::Normal,
            TrbType::SetupStage,
            TrbType::DataStage,
            TrbType::StatusStage,
            TrbType::Link,
            TrbType::NoOpTransfer,
            TrbType::EnableSlot,
            TrbType::DisableSlot,
            TrbType::AddressDevice,
            TrbType::ConfigureEndpoint,
            TrbType::EvaluateContext,
            TrbType::NoOpCommand,
            TrbType::TransferEvent,
            TrbType::CommandCompletion,
            TrbType::PortStatusChange,
            TrbType::HostController,
        ];
        for t in all {
            assert_eq!(TrbType::from_raw(t.raw()), Some(t));
        }
        // Unknown values decode to None.
        assert_eq!(TrbType::from_raw(0), None);
        assert_eq!(TrbType::from_raw(63), None);
    }

    #[test]
    fn type_id_constants_match_spec() {
        assert_eq!(TRB_TYPE_NORMAL, 1);
        assert_eq!(TRB_TYPE_SETUP_STAGE, 2);
        assert_eq!(TRB_TYPE_DATA_STAGE, 3);
        assert_eq!(TRB_TYPE_STATUS_STAGE, 4);
        assert_eq!(TRB_TYPE_LINK, 6);
        assert_eq!(TRB_TYPE_NO_OP_TRANSFER, 8);
        assert_eq!(TRB_TYPE_ENABLE_SLOT, 9);
        assert_eq!(TRB_TYPE_DISABLE_SLOT, 10);
        assert_eq!(TRB_TYPE_ADDRESS_DEVICE, 11);
        assert_eq!(TRB_TYPE_CONFIGURE_ENDPOINT, 12);
        assert_eq!(TRB_TYPE_EVALUATE_CONTEXT, 13);
        assert_eq!(TRB_TYPE_NO_OP_COMMAND, 23);
        assert_eq!(TRB_TYPE_TRANSFER_EVENT, 32);
        assert_eq!(TRB_TYPE_COMMAND_COMPLETION, 33);
        assert_eq!(TRB_TYPE_PORT_STATUS_CHANGE, 34);
        assert_eq!(TRB_TYPE_HOST_CONTROLLER, 37);
        assert_eq!(COMPLETION_SUCCESS, 1);
    }

    #[test]
    fn encode_link() {
        let next = 0x1234_5000u64;
        let trb = Trb::link(next, true, true);
        assert_eq!(trb.parameter, next);
        assert_eq!(trb_type_raw(&trb), TRB_TYPE_LINK);
        assert!(trb_cycle(&trb));
        // Toggle Cycle = bit 1.
        assert_eq!(
            trb.control & TRB_LINK_TOGGLE_CYCLE_BIT,
            TRB_LINK_TOGGLE_CYCLE_BIT
        );

        // Without toggle / cycle.
        let trb2 = Trb::link(next, false, false);
        assert_eq!(trb2.control & TRB_LINK_TOGGLE_CYCLE_BIT, 0);
        assert!(!trb_cycle(&trb2));
    }

    #[test]
    fn encode_enable_slot() {
        let trb = Trb::enable_slot(0, true);
        assert_eq!(trb.parameter, 0);
        assert_eq!(trb_type_raw(&trb), TRB_TYPE_ENABLE_SLOT);
        assert!(trb_cycle(&trb));
        // Slot Type = 0.
        assert_eq!(
            (trb.control >> ENABLE_SLOT_TYPE_SHIFT) & ENABLE_SLOT_TYPE_MASK,
            0
        );

        // Slot Type = 5 in bits 20:16.
        let trb2 = Trb::enable_slot(5, false);
        assert_eq!(
            (trb2.control >> ENABLE_SLOT_TYPE_SHIFT) & ENABLE_SLOT_TYPE_MASK,
            5
        );
        assert!(!trb_cycle(&trb2));
    }

    #[test]
    fn encode_disable_slot() {
        // Slot ID rides bits 31:24, the same field Address Device / Configure
        // Endpoint use; type is Disable Slot (10); cycle propagates.
        let trb = Trb::disable_slot(7, true);
        assert_eq!(trb.parameter, 0);
        assert_eq!(trb.status, 0);
        assert_eq!(trb_type_raw(&trb), TRB_TYPE_DISABLE_SLOT);
        assert!(trb_cycle(&trb));
        assert_eq!((trb.control >> 24) & 0xFF, 7);

        let trb2 = Trb::disable_slot(31, false);
        assert_eq!(trb_type_raw(&trb2), TRB_TYPE_DISABLE_SLOT);
        assert!(!trb_cycle(&trb2));
        assert_eq!((trb2.control >> 24) & 0xFF, 31);
    }

    #[test]
    fn encode_no_op_command() {
        let trb = Trb::no_op_command(true);
        assert_eq!(trb_type_raw(&trb), TRB_TYPE_NO_OP_COMMAND);
        assert!(trb_cycle(&trb));
        assert_eq!(trb.parameter, 0);
        assert_eq!(trb.status, 0);
    }

    #[test]
    fn decode_command_completion() {
        let trb = Trb {
            parameter: 0xDEAD_BEE5, // low nibble 0x5 must be masked away
            status: (COMPLETION_SUCCESS as u32) << 24,
            control: (7u32 << SLOT_ID_SHIFT)
                | (TRB_TYPE_COMMAND_COMPLETION as u32) << TRB_TYPE_SHIFT
                | 1,
        };
        assert_eq!(event_trb_type(&trb), Some(TrbType::CommandCompletion));
        let ev = parse_command_completion(&trb);
        assert_eq!(ev.command_trb_pointer, 0xDEAD_BEE0);
        assert_eq!(ev.completion_code, COMPLETION_SUCCESS);
        assert_eq!(ev.slot_id, 7);
        assert!(ev.cycle);
    }

    #[test]
    fn decode_transfer_event() {
        let trb = Trb {
            parameter: 0x4000_0001,                // low nibble masked
            status: (0x13u32 << 24) | 0x00AB_CDEF, // completion=0x13, residual=0xABCDEF
            control: (9u32 << SLOT_ID_SHIFT)
                | (3u32 << ENDPOINT_ID_SHIFT) // endpoint id 3
                | (TRB_TYPE_TRANSFER_EVENT as u32) << TRB_TYPE_SHIFT
                | 1,
        };
        let ev = parse_transfer_event(&trb);
        assert_eq!(ev.trb_pointer, 0x4000_0000);
        assert_eq!(ev.residual_transfer_length, 0x00AB_CDEF);
        assert_eq!(ev.completion_code, 0x13);
        assert_eq!(ev.endpoint_id, 3);
        assert_eq!(ev.slot_id, 9);
        assert!(ev.cycle);
    }

    #[test]
    fn decode_port_status_change() {
        let trb = Trb {
            parameter: 0x05u64 << 24, // port id 5
            status: (COMPLETION_SUCCESS as u32) << 24,
            control: (TRB_TYPE_PORT_STATUS_CHANGE as u32) << TRB_TYPE_SHIFT, // cycle 0
        };
        assert_eq!(event_trb_type(&trb), Some(TrbType::PortStatusChange));
        let ev = parse_port_status_change(&trb);
        assert_eq!(ev.port_id, 5);
        assert_eq!(ev.completion_code, COMPLETION_SUCCESS);
        assert!(!ev.cycle);
    }

    #[test]
    fn dci_formula() {
        assert_eq!(dci(0, false), 1);
        assert_eq!(dci(0, true), 1); // ep0 always 1 regardless of direction
        assert_eq!(dci(1, true), 3);
        assert_eq!(dci(1, false), 2);
        assert_eq!(dci(2, true), 5);
        assert_eq!(dci(15, true), 31);
        assert_eq!(dci(15, false), 30);
    }

    #[test]
    fn producer_ring_wrap_and_toggle() {
        // size = 4 => slots 0,1,2 usable, slot 3 is the Link TRB.
        let mut ring = ProducerRing::new(4);
        assert_eq!(ring.enqueue, 0);
        assert!(ring.cycle);
        assert_eq!(ring.link_index(), 3);

        // advance #1: 0 -> 1
        assert!(!ring.advance());
        assert_eq!(ring.enqueue, 1);
        assert!(ring.cycle);
        // advance #2: 1 -> 2
        assert!(!ring.advance());
        assert_eq!(ring.enqueue, 2);
        assert!(ring.cycle);
        // advance #3: 2 -> reaches link slot (3) -> wrap to 0, toggle cycle
        assert!(ring.advance());
        assert_eq!(ring.enqueue, 0);
        assert!(!ring.cycle);

        // next lap toggles back
        ring.advance();
        ring.advance();
        assert!(ring.advance());
        assert_eq!(ring.enqueue, 0);
        assert!(ring.cycle);
    }

    #[test]
    fn event_consumer_single_segment_wrap() {
        // One segment of 3 TRBs. CCS starts true; wraps & toggles after 3 steps.
        let mut c = EventConsumer::new(&[3]);
        assert!(c.ccs);
        assert_eq!((c.segment, c.index), (0, 0));

        assert!(!c.dequeue_step()); // -> index 1
        assert_eq!((c.segment, c.index), (0, 1));
        assert!(c.ccs);
        assert!(!c.dequeue_step()); // -> index 2
        assert_eq!((c.segment, c.index), (0, 2));
        assert!(c.ccs);
        assert!(c.dequeue_step()); // past last segment -> wrap, toggle CCS
        assert_eq!((c.segment, c.index), (0, 0));
        assert!(!c.ccs);
    }

    #[test]
    fn event_consumer_two_segment_wrap() {
        // Two segments: sizes 2 and 2. Crossing the intra-table boundary
        // (seg0 -> seg1) must NOT toggle CCS; only wrapping past seg1 does.
        let mut c = EventConsumer::new(&[2, 2]);
        assert!(c.ccs);
        assert_eq!((c.segment, c.index), (0, 0));

        // seg0 index 0 -> 1
        assert!(!c.dequeue_step());
        assert_eq!((c.segment, c.index), (0, 1));
        assert!(c.ccs);

        // seg0 end -> seg1 start: segment advances, CCS unchanged.
        assert!(!c.dequeue_step());
        assert_eq!((c.segment, c.index), (1, 0));
        assert!(c.ccs);

        // seg1 index 0 -> 1
        assert!(!c.dequeue_step());
        assert_eq!((c.segment, c.index), (1, 1));
        assert!(c.ccs);

        // past seg1 (the last segment) -> full wrap, toggle CCS.
        assert!(c.dequeue_step());
        assert_eq!((c.segment, c.index), (0, 0));
        assert!(!c.ccs);
    }

    #[test]
    fn event_consumer_owns_matches_ccs() {
        let c = EventConsumer::new(&[4]);
        let owned = Trb {
            control: 1, // cycle = 1 == CCS(true)
            ..Default::default()
        };
        let stale = Trb {
            control: 0, // cycle = 0
            ..Default::default()
        };
        assert!(c.owns(&owned));
        assert!(!c.owns(&stale));
    }

    // -----------------------------------------------------------------------
    // SetupPacket tests
    // -----------------------------------------------------------------------

    #[test]
    fn setup_packet_get_device_descriptor() {
        let pkt = SetupPacket::get_device_descriptor(18);
        assert_eq!(pkt.bm_request_type, BM_REQUEST_TYPE_D2H_STD_DEV);
        assert_eq!(pkt.b_request, B_REQUEST_GET_DESCRIPTOR);
        // wValue high byte = DESCRIPTOR_TYPE_DEVICE = 0x01.
        assert_eq!(pkt.w_value, (DESCRIPTOR_TYPE_DEVICE as u16) << 8);
        assert_eq!(pkt.w_index, 0);
        assert_eq!(pkt.w_length, 18);
    }

    #[test]
    fn setup_packet_get_config_descriptor_short() {
        // Short read: request 9 bytes to learn wTotalLength.
        let pkt = SetupPacket::get_config_descriptor(0, 9);
        assert_eq!(pkt.bm_request_type, BM_REQUEST_TYPE_D2H_STD_DEV);
        assert_eq!(pkt.b_request, B_REQUEST_GET_DESCRIPTOR);
        // wValue high byte = 0x02 (Configuration), low = index 0.
        assert_eq!(pkt.w_value, (DESCRIPTOR_TYPE_CONFIGURATION as u16) << 8);
        assert_eq!(pkt.w_length, 9);
    }

    #[test]
    fn setup_packet_set_configuration() {
        let pkt = SetupPacket::set_configuration(1);
        assert_eq!(pkt.bm_request_type, BM_REQUEST_TYPE_H2D_STD_DEV);
        assert_eq!(pkt.b_request, B_REQUEST_SET_CONFIGURATION);
        assert_eq!(pkt.w_value, 1);
        assert_eq!(pkt.w_length, 0);
    }

    #[test]
    fn setup_packet_as_u64_layout() {
        // Manually verify the bit layout (USB 2.0 §9.3 / xHCI §6.4.1.2.1).
        let pkt = SetupPacket {
            bm_request_type: 0x80,
            b_request: 0x06,
            w_value: 0x0100,
            w_index: 0x0000,
            w_length: 18,
        };
        let raw = pkt.as_u64();
        assert_eq!(raw & 0xFF, 0x80); // byte 0
        assert_eq!((raw >> 8) & 0xFF, 0x06); // byte 1
        assert_eq!((raw >> 16) & 0xFFFF, 0x0100); // bytes 2-3
        assert_eq!((raw >> 32) & 0xFFFF, 0x0000); // bytes 4-5
        assert_eq!((raw >> 48) & 0xFFFF, 18); // bytes 6-7
    }

    // -----------------------------------------------------------------------
    // Control-transfer stage TRB builder tests
    // -----------------------------------------------------------------------

    #[test]
    fn setup_stage_trb_fields() {
        let setup = SetupPacket::get_device_descriptor(18);
        let trb = Trb::setup_stage(&setup, SETUP_TT_IN, true);
        // Type.
        assert_eq!(trb_type_raw(&trb), TRB_TYPE_SETUP_STAGE);
        assert!(trb_cycle(&trb));
        // IDT bit must be set (bit 6).
        assert_ne!(trb.control & SETUP_IDT_BIT, 0);
        // Transfer Type = IN (3) at bits 17:16.
        assert_eq!((trb.control >> SETUP_TT_SHIFT) & 0x3, SETUP_TT_IN);
        // Setup data in parameter.
        assert_eq!(trb.parameter, setup.as_u64());
        // Status = 8 (setup packet is always 8 bytes).
        assert_eq!(trb.status, 8);
    }

    #[test]
    fn data_stage_trb_in() {
        let buf = 0x0020_0000u64;
        let trb = Trb::data_stage(buf, 18, true, true);
        assert_eq!(trb_type_raw(&trb), TRB_TYPE_DATA_STAGE);
        assert!(trb_cycle(&trb));
        assert_eq!(trb.parameter, buf);
        assert_eq!(trb.status, 18);
        // DIR bit set for IN.
        assert_ne!(trb.control & DATA_DIR_BIT, 0);
    }

    #[test]
    fn data_stage_trb_out() {
        let trb = Trb::data_stage(0x100, 64, false, false);
        assert_eq!(trb_type_raw(&trb), TRB_TYPE_DATA_STAGE);
        assert!(!trb_cycle(&trb));
        // DIR bit clear for OUT.
        assert_eq!(trb.control & DATA_DIR_BIT, 0);
    }

    #[test]
    fn normal_trb_interrupt_in() {
        // Normal TRB used for an interrupt-IN HID poll: IOC + ISP set so a
        // full or short report both post a Transfer Event.
        let buf = 0x0030_0000u64;
        let trb = Trb::normal(buf, 8, true);
        assert_eq!(trb_type_raw(&trb), TRB_TYPE_NORMAL);
        assert!(trb_cycle(&trb));
        assert_eq!(trb.parameter, buf);
        assert_eq!(trb.status & 0x1_FFFF, 8);
        const IOC_BIT: u32 = 1 << 5;
        const ISP_BIT: u32 = 1 << 2;
        assert_ne!(trb.control & IOC_BIT, 0, "Normal TRB must set IOC");
        assert_ne!(trb.control & ISP_BIT, 0, "Normal TRB must set ISP");
    }

    #[test]
    fn isoch_trb_sia_start_asap() {
        // Isoch TRB scheduled Start-Isoch-ASAP (the fire-and-forget UAC audio
        // OUT path): SIA (bit 31) set, Frame ID field zero, IOC set so each
        // service interval posts a Transfer Event.
        let buf = 0x0040_0000u64;
        let trb = Trb::isoch(buf, 192, 0, true, true);
        assert_eq!(trb_type_raw(&trb), TRB_TYPE_ISOCH);
        assert_eq!(event_trb_type(&trb), Some(TrbType::Isoch));
        assert!(trb_cycle(&trb));
        assert_eq!(trb.parameter, buf);
        assert_eq!(trb.status & 0x1_FFFF, 192);
        const IOC_BIT: u32 = 1 << 5;
        const SIA_BIT: u32 = 1 << 31;
        const FRAME_ID_MASK: u32 = 0x7FF << 20;
        assert_ne!(trb.control & IOC_BIT, 0, "Isoch TRB must set IOC");
        assert_ne!(trb.control & SIA_BIT, 0, "SIA must be set when sia=true");
        assert_eq!(
            trb.control & FRAME_ID_MASK,
            0,
            "Frame ID must be ignored (zero) when SIA is set"
        );
    }

    #[test]
    fn isoch_trb_explicit_frame_id() {
        // With sia=false the 11-bit Frame ID selects the target (micro)frame and
        // SIA must be clear.
        let trb = Trb::isoch(0x0050_0000, 64, 0x2AB, false, false);
        assert_eq!(trb_type_raw(&trb), TRB_TYPE_ISOCH);
        assert!(!trb_cycle(&trb));
        const SIA_BIT: u32 = 1 << 31;
        assert_eq!(trb.control & SIA_BIT, 0, "SIA must be clear when sia=false");
        // Frame ID occupies control bits 30:20.
        assert_eq!((trb.control >> 20) & 0x7FF, 0x2AB);
    }

    #[test]
    fn status_stage_trb() {
        // After an IN data stage the status stage is OUT (dir_in = false).
        let trb = Trb::status_stage(false, true);
        assert_eq!(trb_type_raw(&trb), TRB_TYPE_STATUS_STAGE);
        assert!(trb_cycle(&trb));
        assert_eq!(trb.control & DATA_DIR_BIT, 0);
        // IOC (bit 5) must be set per xHCI §6.4.1.2.3 so a Transfer Event is
        // generated when the status phase completes.
        const IOC_BIT: u32 = 1 << 5;
        assert_ne!(
            trb.control & IOC_BIT,
            0,
            "Status Stage TRB must have IOC set (xHCI §6.4.1.2.3)"
        );

        // No-data-stage: status is IN (dir_in = true).
        let trb2 = Trb::status_stage(true, false);
        assert_ne!(trb2.control & DATA_DIR_BIT, 0);
        assert!(!trb_cycle(&trb2));
        // IOC must also be set on the no-data-stage status TRB.
        assert_ne!(
            trb2.control & IOC_BIT,
            0,
            "Status Stage TRB (no-data) must have IOC set"
        );
    }

    // -----------------------------------------------------------------------
    // Command TRB builder tests
    // -----------------------------------------------------------------------

    #[test]
    fn address_device_trb_bsr_false() {
        let ctx = 0x0040_0000u64;
        let trb = Trb::address_device(ctx, 5, false, true);
        assert_eq!(trb_type_raw(&trb), TRB_TYPE_ADDRESS_DEVICE);
        assert!(trb_cycle(&trb));
        assert_eq!(trb.parameter, ctx);
        // Slot ID at bits 31:24.
        assert_eq!((trb.control >> CMD_SLOT_ID_SHIFT) as u8, 5);
        // BSR must be clear.
        assert_eq!(trb.control & ADDRESS_DEVICE_BSR_BIT, 0);
    }

    #[test]
    fn address_device_trb_bsr_true() {
        let ctx = 0x0040_0000u64;
        let trb = Trb::address_device(ctx, 3, true, false);
        assert_eq!(trb_type_raw(&trb), TRB_TYPE_ADDRESS_DEVICE);
        assert!(!trb_cycle(&trb));
        // BSR must be set.
        assert_ne!(trb.control & ADDRESS_DEVICE_BSR_BIT, 0);
        assert_eq!((trb.control >> CMD_SLOT_ID_SHIFT) as u8, 3);
    }

    #[test]
    fn configure_endpoint_trb() {
        let ctx = 0x0050_0000u64;
        let trb = Trb::configure_endpoint(ctx, 7, true);
        assert_eq!(trb_type_raw(&trb), TRB_TYPE_CONFIGURE_ENDPOINT);
        assert!(trb_cycle(&trb));
        assert_eq!(trb.parameter, ctx);
        assert_eq!((trb.control >> CMD_SLOT_ID_SHIFT) as u8, 7);
    }

    #[test]
    fn stop_endpoint_trb() {
        let trb = Trb::stop_endpoint(4, 3, true);
        assert_eq!(trb_type_raw(&trb), TRB_TYPE_STOP_ENDPOINT);
        assert!(trb_cycle(&trb));
        assert_eq!(trb.parameter, 0);
        assert_eq!((trb.control >> CMD_SLOT_ID_SHIFT) as u8, 4);
        assert_eq!(((trb.control >> CMD_ENDPOINT_ID_SHIFT) & 0x1F) as u8, 3);
        // SP (suspend) bit 23 must be clear — a full stop.
        assert_eq!(trb.control & (1 << 23), 0);
    }

    #[test]
    fn reset_endpoint_trb() {
        let trb = Trb::reset_endpoint(6, 4, false);
        assert_eq!(trb_type_raw(&trb), TRB_TYPE_RESET_ENDPOINT);
        assert!(!trb_cycle(&trb));
        assert_eq!((trb.control >> CMD_SLOT_ID_SHIFT) as u8, 6);
        assert_eq!(((trb.control >> CMD_ENDPOINT_ID_SHIFT) & 0x1F) as u8, 4);
        // TSP bit 9 must be clear — transfer state (data toggle) resets.
        assert_eq!(trb.control & (1 << 9), 0);
    }

    #[test]
    fn set_tr_dequeue_trb_carries_pointer_and_dcs() {
        let deq = 0x0070_0040u64;
        let trb = Trb::set_tr_dequeue(deq, true, 2, 5, true);
        assert_eq!(trb_type_raw(&trb), TRB_TYPE_SET_TR_DEQUEUE);
        assert!(trb_cycle(&trb));
        // Pointer in the upper bits, DCS in bit 0.
        assert_eq!(trb.parameter & !0xF, deq);
        assert_eq!(trb.parameter & 1, 1);
        assert_eq!((trb.control >> CMD_SLOT_ID_SHIFT) as u8, 2);
        assert_eq!(((trb.control >> CMD_ENDPOINT_ID_SHIFT) & 0x1F) as u8, 5);
        // DCS=false leaves bit 0 clear; a misaligned pointer is masked.
        let trb2 = Trb::set_tr_dequeue(deq | 0x6, false, 2, 5, true);
        assert_eq!(trb2.parameter & 1, 0);
        assert_eq!(trb2.parameter & !0xF, deq);
    }

    #[test]
    fn evaluate_context_trb() {
        let ctx = 0x0060_0000u64;
        let trb = Trb::evaluate_context(ctx, 2, true);
        assert_eq!(trb_type_raw(&trb), TRB_TYPE_EVALUATE_CONTEXT);
        assert!(trb_cycle(&trb));
        assert_eq!(trb.parameter, ctx);
        assert_eq!((trb.control >> CMD_SLOT_ID_SHIFT) as u8, 2);
    }
}
