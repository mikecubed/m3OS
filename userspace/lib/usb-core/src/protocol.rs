//! Typed IPC protocol between the xHCI host server and USB class drivers.
//!
//! # Message model
//!
//! m3OS IPC is a synchronous rendezvous: the caller issues a `call`, and the
//! server `reply_recv`s. All message types here fit within a single IPC
//! payload (small, fixed-size structures). The key rule for data transfers:
//!
//! > **Setup packets and descriptor bytes travel inline.**
//! > **Report / bulk / isochronous transfer buffers travel as page-capability
//! > grants** (a [`PageGrant`] carrying a cap handle + byte length). This keeps
//! > the IPC payload within the kernel's message-size budget (typically 128–512
//! > bytes) even for large HID report streams.
//!
//! # Lifecycle
//!
//! 1. The xHCI server enumerates a device and identifies its class/subclass/
//!    protocol from the parsed configuration tree.
//! 2. It sends an [`AttachNotice`] to the matching class-driver daemon.
//! 3. The class driver calls [`UsbClient::configure_endpoints`] to activate its
//!    endpoints, then begins submitting transfers via
//!    [`UsbClient::submit_transfer`].
//! 4. On device removal the server sends an [`AttachNotice`] with
//!    `attached = false`; the class driver tears down its state.

extern crate alloc;

use alloc::vec::Vec;

// ---------------------------------------------------------------------------
// Page-grant handle (Phase 74 model)
// ---------------------------------------------------------------------------

/// A capability handle identifying a page-grant: a region of shared memory
/// the kernel has mapped into both the sender's and receiver's address spaces.
///
/// Transfer buffers (`SubmitTransfer`) carry a `PageGrant` instead of an
/// inline `Vec<u8>`. The receiver maps the grant, reads or writes the data,
/// then revokes the capability via `sys_cap_revoke`.
///
/// In this crate the type is a newtype around `u32` (the raw capability index);
/// the kernel validates the handle on every syscall.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageGrant {
    /// Raw capability table index.
    pub cap: u32,
    /// Byte length of the mapped region.
    pub len: usize,
}

// ---------------------------------------------------------------------------
// AttachNotice — device arrival / departure notification
// ---------------------------------------------------------------------------

/// Notification sent by the xHCI server to a class driver when a USB device
/// matching the driver's class/subclass/protocol is attached or detached.
///
/// Class drivers are static long-lived daemons; this message is how the host
/// binds them to a concrete physical device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AttachNotice {
    /// 1-based root-hub port number the device is attached to.
    pub port: u8,
    /// Slot ID the xHCI controller assigned to the device.
    pub slot_id: u8,
    /// `bInterfaceClass` from the device configuration.
    pub interface_class: u8,
    /// `bInterfaceSubClass` from the device configuration.
    pub interface_sub_class: u8,
    /// `bInterfaceProtocol` from the device configuration.
    pub interface_protocol: u8,
    /// `true` if the device was just attached; `false` if it was detached.
    pub attached: bool,
    /// Device Context Index of the interface's interrupt-IN endpoint, or `0`
    /// if the interface has none. Phase 78c: the server resolves this from the
    /// enumerated configuration so a HID class driver can poll the endpoint
    /// without a separate `GetDescriptors` round-trip.
    pub ep_in_dci: u8,
    /// `wMaxPacketSize` of that interrupt-IN endpoint (0 if `ep_in_dci == 0`).
    pub ep_in_mps: u16,
    /// `bInterval` of that interrupt-IN endpoint (0 if `ep_in_dci == 0`).
    pub ep_in_interval: u8,
    /// `bInterfaceNumber` of the HID interface — the `wIndex` a class driver
    /// uses for `SET_PROTOCOL` / `SET_IDLE`.
    pub interface_num: u8,
    /// `idVendor` from the device descriptor — lets a class driver match a
    /// specific device (e.g. `0x0bda` for Realtek) without a `GetDescriptors`
    /// round-trip. Phase 96.
    pub vendor_id: u16,
    /// `idProduct` from the device descriptor (e.g. `0x8156` for RTL8156).
    /// Phase 96.
    pub product_id: u16,
    /// Device Context Index of the interface's bulk-IN endpoint, or `0` if the
    /// interface has none. Phase 96 (USB-Ethernet / bulk-class drivers).
    pub bulk_in_dci: u8,
    /// `wMaxPacketSize` of that bulk-IN endpoint (0 if `bulk_in_dci == 0`).
    pub bulk_in_mps: u16,
    /// Device Context Index of the interface's bulk-OUT endpoint, or `0`.
    pub bulk_out_dci: u8,
    /// `wMaxPacketSize` of that bulk-OUT endpoint (0 if `bulk_out_dci == 0`).
    pub bulk_out_mps: u16,
}

// ---------------------------------------------------------------------------
// TopoPort — one root-hub port's live status, for the Topology diagnostic
// ---------------------------------------------------------------------------

/// Live status of a single root-hub port on a brought-up controller, returned
/// by [`UsbRequest::Topology`]. Used by `ure`'s bare-metal heartbeat to surface
/// — at the bottom of the framebuffer scroll, where no serial console exists —
/// *which controller* a connected device sits on and *what speed* it trained
/// to. A SuperSpeed-only device (e.g. an RTL8156 on a USB-C port's TCSS lanes)
/// that shows `ccs` but never surfaces as an enumerated device localizes the
/// failure to enumeration; an entirely absent controller localizes it to
/// discovery / bring-up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TopoPort {
    /// Index of the owning brought-up controller (0 = primary).
    pub ctrl: u8,
    /// 1-based root-hub port number.
    pub port: u8,
    /// Packed status flags: bit0 = CCS (current connect status), bit1 = PED
    /// (port enabled), bit2 = PP (port power); bits 4–7 (the high nibble) = the
    /// `PORTSC` Port Speed field (xHCI default PSI: 1 = Full, 2 = Low, 3 = High,
    /// 4 = Super).
    pub flags: u8,
}

impl TopoPort {
    /// CCS — a device is currently connected to this port.
    pub const fn ccs(self) -> bool {
        self.flags & 0x01 != 0
    }
    /// PED — the port is enabled (link trained).
    pub const fn ped(self) -> bool {
        self.flags & 0x02 != 0
    }
    /// The `PORTSC` Port Speed field (default PSI value).
    pub const fn speed_psi(self) -> u8 {
        (self.flags >> 4) & 0x0F
    }
    /// Pack `(ccs, ped, pp, speed_psi)` into the wire flag byte.
    pub const fn pack(ccs: bool, ped: bool, pp: bool, speed_psi: u8) -> u8 {
        (ccs as u8) | ((ped as u8) << 1) | ((pp as u8) << 2) | ((speed_psi & 0x0F) << 4)
    }
}

// ---------------------------------------------------------------------------
// UsbRequest — messages from a class driver to the xHCI server
// ---------------------------------------------------------------------------

/// A request from a class driver to the xHCI host server.
///
/// Requests are issued synchronously via `sys_ipc_call`; the matching
/// [`UsbReply`] is the rendezvous response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UsbRequest {
    /// Retrieve the parsed descriptor set for a device.
    ///
    /// Returns [`UsbReply::Descriptors`] with the raw Device + Configuration
    /// blobs so the class driver can inspect class-specific fields.
    GetDescriptors {
        /// Target slot ID (from the [`AttachNotice`]).
        slot_id: u8,
    },

    /// Activate the endpoints described by the parsed configuration for a
    /// device. The server issues a Configure Endpoint command to the xHCI
    /// controller and then maps each endpoint's transfer ring so the class
    /// driver can submit transfers.
    ///
    /// Returns [`UsbReply::EndpointsConfigured`] on success.
    ConfigureEndpoints {
        /// Target slot ID.
        slot_id: u8,
        /// `bConfigurationValue` of the configuration to activate (typically 1).
        configuration_value: u8,
    },

    /// Issue a USB control transfer on EP0.
    ///
    /// The setup packet is encoded inline (8 bytes, fits trivially in an IPC
    /// payload). The response data — if any — is returned inline for short
    /// descriptors (≤ 64 bytes) or as a page-grant for larger buffers.
    ///
    /// Returns [`UsbReply::ControlData`].
    ControlRequest {
        /// Target slot ID.
        slot_id: u8,
        /// Raw setup packet bytes (8 bytes, little-endian, USB 2.0 §9.3).
        setup: [u8; 8],
        /// Expected response length (0 for OUT-only control transfers).
        length: u16,
    },

    /// Issue a USB control transfer on EP0 that carries an **OUT data stage**.
    ///
    /// Unlike [`UsbRequest::ControlRequest`] (which only allocates a data
    /// buffer for IN transfers), this variant ships the host-to-device payload
    /// inline so the server can copy it into the data-stage DMA buffer. Used by
    /// the `ure` driver's OCP register *writes* (vendor request `bRequest=0x05`,
    /// `bmRequestType=0x40`) for chip init / RX-TX enable. Phase 96.
    ///
    /// `setup[6..8]` (`wLength`) MUST equal `data.len()` — the server fails the
    /// transfer closed on a mismatch. Returns [`UsbReply::ControlData`] with an
    /// empty `data` field (the status is carried by `completion_code`).
    ControlWrite {
        /// Target slot ID.
        slot_id: u8,
        /// Raw setup packet bytes (8 bytes, little-endian, USB 2.0 §9.3).
        /// `bmRequestType` bit 7 (D2H) MUST be clear — this is an OUT transfer.
        setup: [u8; 8],
        /// Host-to-device data-stage payload (inline; `wLength` bytes).
        data: Vec<u8>,
    },

    /// Submit an interrupt or bulk transfer on a non-EP0 endpoint.
    ///
    /// The transfer buffer is carried as a [`PageGrant`] rather than inline
    /// bytes, keeping the IPC payload small even for full-size HID reports.
    ///
    /// Returns [`UsbReply::TransferComplete`].
    SubmitTransfer {
        /// Target slot ID.
        slot_id: u8,
        /// Device Context Index of the target endpoint.
        dci: u8,
        /// Page-grant carrying the transfer buffer.
        ///
        /// For IN endpoints (device-to-host) the server writes received data
        /// into the mapped page and the class driver reads it after the reply.
        /// For OUT endpoints the class driver writes the data before calling.
        grant: PageGrant,
    },

    /// Pull the next attached device the server has enumerated, starting at
    /// `cursor` (0-based index into the server's device table).
    ///
    /// Phase 78c: class daemons (HID, hub) discover their bound devices by
    /// walking this cursor until the reply carries `None`. Replaces a live
    /// push-notification channel for the no-hotplug 1.0 path — devices are
    /// present at boot, so a pull is sufficient and simpler.
    ///
    /// Returns [`UsbReply::Attach`].
    NextAttach {
        /// 0-based index into the server's enumerated-device table.
        cursor: u8,
    },

    /// Non-blocking read of the most recent interrupt-IN report the server has
    /// captured for `(slot_id, dci)`.
    ///
    /// The server arms the endpoint (enqueues a Normal TRB) on the first poll
    /// and re-arms it after every IRQ-delivered report. HID reports are tiny
    /// (≤ 8 bytes), so the report is returned **inline** rather than via a
    /// page-grant. If no new report is buffered the reply carries empty data.
    ///
    /// Returns [`UsbReply::InterruptReport`].
    PollInterruptIn {
        /// Target slot ID.
        slot_id: u8,
        /// Device Context Index of the interrupt-IN endpoint to poll.
        dci: u8,
        /// Maximum bytes to return (the endpoint's `wMaxPacketSize`).
        len: u16,
    },

    /// Non-blocking poll of a **bulk-IN** endpoint for a received buffer (e.g. a
    /// USB-Ethernet RX frame batch). Like [`UsbRequest::PollInterruptIn`] but
    /// the server arms the endpoint with a frame-sized (`len`) Normal TRB; the
    /// captured buffer is returned **inline** (frames fit within `USB_MSG_MAX`).
    /// Returns [`UsbReply::BulkData`]. Phase 96.
    PollBulkIn {
        /// Target slot ID.
        slot_id: u8,
        /// Device Context Index of the bulk-IN endpoint to poll.
        dci: u8,
        /// Frame-sized buffer length to arm / cap the returned bytes at.
        len: u16,
    },

    /// Submit a **bulk-OUT** transfer (e.g. a USB-Ethernet TX frame) and block
    /// for completion. The payload travels inline (a frame + Realtek TX
    /// descriptor fits within `USB_MSG_MAX`). Returns
    /// [`UsbReply::TransferComplete`] with the byte count. Phase 96.
    SubmitBulkOut {
        /// Target slot ID.
        slot_id: u8,
        /// Device Context Index of the bulk-OUT endpoint.
        dci: u8,
        /// The frame bytes (Realtek TX descriptor already prepended by the driver).
        data: Vec<u8>,
    },

    /// Diagnostic: snapshot the host's live root-hub topology across every
    /// brought-up controller. No target — the server reads each controller's
    /// `PORTSC` registers and returns [`UsbReply::Topology`]. Bare-metal
    /// debugging aid (Phase 96): a class driver with no device to claim (e.g.
    /// `ure` when the NIC didn't enumerate) polls this each heartbeat so the
    /// controller/port/speed picture survives the framebuffer scroll.
    Topology,
}

// ---------------------------------------------------------------------------
// UsbReply — responses from the xHCI server to a class driver
// ---------------------------------------------------------------------------

/// Reply from the xHCI host server to a class driver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UsbReply {
    /// Reply to [`UsbRequest::GetDescriptors`].
    Descriptors {
        /// Raw Device Descriptor bytes (18 bytes).
        device: Vec<u8>,
        /// Raw Configuration Descriptor blob (`wTotalLength` bytes).
        config: Vec<u8>,
    },

    /// Reply to [`UsbRequest::ConfigureEndpoints`]: endpoints are active.
    EndpointsConfigured {
        /// The slot ID that was configured.
        slot_id: u8,
    },

    /// Reply to [`UsbRequest::ControlRequest`].
    ControlData {
        /// Response data bytes (inline, ≤ 64 bytes).
        data: Vec<u8>,
        /// xHCI completion code (`1` = success).
        completion_code: u8,
    },

    /// Reply to [`UsbRequest::SubmitTransfer`].
    TransferComplete {
        /// Number of bytes actually transferred.
        transferred: usize,
        /// xHCI completion code (`1` = success).
        completion_code: u8,
    },

    /// Reply to [`UsbRequest::NextAttach`]: the device at the requested cursor,
    /// or `None` if the cursor is past the end of the table.
    Attach {
        /// The enumerated device, or `None` when the cursor is exhausted.
        notice: Option<AttachNotice>,
    },

    /// Reply to [`UsbRequest::PollInterruptIn`].
    ///
    /// `data` is the captured interrupt-IN report (inline, ≤ `len` bytes); an
    /// **empty** `data` with `completion_code == 0` means "no new report yet".
    InterruptReport {
        /// The captured report bytes (empty = no report pending).
        data: Vec<u8>,
        /// xHCI completion code of the last completed transfer (`1` = success,
        /// `0` = none captured yet).
        completion_code: u8,
    },

    /// Reply to [`UsbRequest::PollBulkIn`]. `data` is the captured bulk-IN
    /// buffer (inline, ≤ the polled `len`); an **empty** `data` with
    /// `completion_code == 0` means "nothing captured yet". Phase 96.
    BulkData {
        /// The captured bulk-IN bytes (empty = nothing pending).
        data: Vec<u8>,
        /// xHCI completion code of the last completed transfer (`1` = success,
        /// `0` = none captured yet).
        completion_code: u8,
    },

    /// The request failed for a reason not captured by a specific variant.
    Error {
        /// Stable numeric error code (for logging / matching). Wire-safe — a
        /// `&'static str` cannot be reconstructed on the decode side.
        code: u16,
    },

    /// Reply to [`UsbRequest::Topology`]: the live root-hub picture.
    Topology {
        /// Number of xHCI controllers discovered via PCI class enumeration
        /// (may exceed `port_counts.len()` if a controller failed bring-up and
        /// was skipped — that gap is itself the diagnostic).
        discovered: u8,
        /// `max_ports` (`HCSPARAMS1.MaxPorts`) for each *brought-up* controller,
        /// in controller-index order. `len()` is the up count.
        port_counts: Vec<u8>,
        /// One entry per **connected** root-hub port (CCS set) across all
        /// brought-up controllers. An empty list with `port_counts` non-empty
        /// means the controllers came up but saw no device.
        ports: Vec<TopoPort>,
    },
}

// ---------------------------------------------------------------------------
// UsbClient — thin client-side API surface
// ---------------------------------------------------------------------------

/// Thin client API for USB class drivers communicating with the xHCI server.
///
/// This struct holds the server endpoint capability and provides typed
/// wrappers around the raw [`UsbRequest`] / [`UsbReply`] exchange. The actual
/// `sys_ipc_call` plumbing is **not** implemented here (it requires the
/// running kernel IPC syscalls); this crate defines the interface and
/// host-tests the message-construction logic.
///
/// # Example usage (illustrative, not compiled against a live server)
///
/// ```rust
/// # use usb_core::protocol::{AttachNotice, UsbClient};
/// # fn example(slot_id: u8) {
/// let client = UsbClient::new(slot_id, /*server_cap=*/ 42);
/// let _req = client.configure_endpoints_request(1);
/// // caller sends _req via sys_ipc_call to the xHCI server
/// # }
/// ```
#[derive(Debug)]
pub struct UsbClient {
    /// The slot ID of the device this client is bound to.
    pub slot_id: u8,
    /// Capability index of the xHCI server endpoint.
    pub server_cap: u32,
}

impl UsbClient {
    /// Construct a new client bound to `slot_id`, reaching the server via
    /// `server_cap`.
    pub const fn new(slot_id: u8, server_cap: u32) -> Self {
        UsbClient {
            slot_id,
            server_cap,
        }
    }

    /// Build a [`UsbRequest::GetDescriptors`] message for this device.
    ///
    /// The caller sends the returned message via `sys_ipc_call` and decodes
    /// the response as [`UsbReply::Descriptors`].
    pub fn get_descriptors_request(&self) -> UsbRequest {
        UsbRequest::GetDescriptors {
            slot_id: self.slot_id,
        }
    }

    /// Build a [`UsbRequest::ConfigureEndpoints`] message.
    ///
    /// `configuration_value` is the `bConfigurationValue` from the device's
    /// Configuration Descriptor (typically 1 for single-configuration devices).
    pub fn configure_endpoints_request(&self, configuration_value: u8) -> UsbRequest {
        UsbRequest::ConfigureEndpoints {
            slot_id: self.slot_id,
            configuration_value,
        }
    }

    /// Build a [`UsbRequest::ControlRequest`] message.
    ///
    /// `setup` is the raw 8-byte SETUP packet (USB 2.0 §9.3 layout,
    /// little-endian). `length` is the expected data-stage byte count.
    pub fn control_request(&self, setup: [u8; 8], length: u16) -> UsbRequest {
        UsbRequest::ControlRequest {
            slot_id: self.slot_id,
            setup,
            length,
        }
    }

    /// Build a [`UsbRequest::ControlWrite`] message (OUT control transfer with
    /// an inline data stage).
    ///
    /// `setup` is the raw 8-byte SETUP packet; `data` is the host-to-device
    /// payload whose length must equal the SETUP packet's `wLength`.
    pub fn control_write_request(&self, setup: [u8; 8], data: Vec<u8>) -> UsbRequest {
        UsbRequest::ControlWrite {
            slot_id: self.slot_id,
            setup,
            data,
        }
    }

    /// Build a [`UsbRequest::SubmitTransfer`] message for `dci` using a
    /// page-capability grant.
    ///
    /// The grant must have been obtained from the kernel via a prior
    /// `sys_page_grant` call. The xHCI server maps it, programmes a TRB on the
    /// endpoint's transfer ring, and writes the result (for IN endpoints) back
    /// into the mapped page before replying.
    pub fn submit_transfer_request(&self, dci: u8, grant: PageGrant) -> UsbRequest {
        UsbRequest::SubmitTransfer {
            slot_id: self.slot_id,
            dci,
            grant,
        }
    }
}

// ---------------------------------------------------------------------------
// Wire transport — IPC service name, labels, and byte codec
// ---------------------------------------------------------------------------

/// Well-known service name the xHCI host server registers and class drivers
/// look up (`ipc_lookup_service`).
pub const USB_SERVICE_NAME: &str = "usb";

/// IPC label for a client→server [`UsbRequest`] call. The request kind is the
/// first byte of the bulk payload, so a single label carries every request.
pub const USB_REQ_LABEL: u64 = 1;

/// IPC label the server replies with; the [`UsbReply`] travels as reply bulk.
pub const USB_REPLY_LABEL: u64 = 1;

/// Upper bound on an encoded request/reply. Phase 96 raised this from 1024 to
/// 4096 so a `BulkData`/`SubmitBulkOut` payload carrying a full Ethernet frame
/// (≤ 1522 B) plus its Realtek descriptor and the wire-codec overhead fits
/// inline. Clients/servers size their bulk buffers to this. Well within the
/// kernel's `MAX_BULK_LEN` (81920).
pub const USB_MSG_MAX: usize = 4096;

// --- request tags ---
const REQ_GET_DESCRIPTORS: u8 = 1;
const REQ_CONFIGURE_ENDPOINTS: u8 = 2;
const REQ_CONTROL: u8 = 3;
const REQ_SUBMIT_TRANSFER: u8 = 4;
const REQ_NEXT_ATTACH: u8 = 5;
const REQ_POLL_INTERRUPT_IN: u8 = 6;
const REQ_CONTROL_WRITE: u8 = 7;
const REQ_POLL_BULK_IN: u8 = 8;
const REQ_SUBMIT_BULK_OUT: u8 = 9;
const REQ_TOPOLOGY: u8 = 10;

// --- reply tags ---
const REP_DESCRIPTORS: u8 = 1;
const REP_ENDPOINTS_CONFIGURED: u8 = 2;
const REP_CONTROL_DATA: u8 = 3;
const REP_TRANSFER_COMPLETE: u8 = 4;
const REP_ATTACH: u8 = 5;
const REP_INTERRUPT_REPORT: u8 = 6;
const REP_ERROR: u8 = 7;
const REP_BULK_DATA: u8 = 8;
const REP_TOPOLOGY: u8 = 9;

#[inline]
fn put_u16(v: &mut Vec<u8>, x: u16) {
    v.extend_from_slice(&x.to_le_bytes());
}
#[inline]
fn put_u32(v: &mut Vec<u8>, x: u32) {
    v.extend_from_slice(&x.to_le_bytes());
}
#[inline]
fn put_bytes(v: &mut Vec<u8>, b: &[u8]) {
    // The length prefix is a u16, so a body of 64 KiB or more would wrap and
    // desync the frame on decode. Every live producer is bounded well below
    // this (HID reports <= wMaxPacketSize, ControlData <= a u16 `length`, and
    // both ends size their bulk buffers to USB_MSG_MAX); the assert documents
    // and guards that invariant should a future (Phase 90) descriptor path
    // grow the payload past the prefix width.
    debug_assert!(
        b.len() <= u16::MAX as usize,
        "put_bytes: body length exceeds the u16 wire prefix"
    );
    put_u16(v, b.len() as u16);
    v.extend_from_slice(b);
}

/// A forward-only cursor over an encoded message. Every accessor returns
/// `None` on truncation, so a malformed message decodes to `None` rather than
/// panicking.
struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}
impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }
    fn u8(&mut self) -> Option<u8> {
        let b = *self.buf.get(self.pos)?;
        self.pos += 1;
        Some(b)
    }
    fn u16(&mut self) -> Option<u16> {
        let end = self.pos.checked_add(2)?;
        let s = self.buf.get(self.pos..end)?;
        self.pos = end;
        Some(u16::from_le_bytes([s[0], s[1]]))
    }
    fn u32(&mut self) -> Option<u32> {
        let end = self.pos.checked_add(4)?;
        let s = self.buf.get(self.pos..end)?;
        self.pos = end;
        Some(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
    }
    fn bytes(&mut self) -> Option<Vec<u8>> {
        let n = self.u16()? as usize;
        let end = self.pos.checked_add(n)?;
        let s = self.buf.get(self.pos..end)?;
        self.pos = end;
        Some(s.to_vec())
    }
}

impl AttachNotice {
    /// Fixed encoded length of an [`AttachNotice`] on the wire.
    pub const WIRE_LEN: usize = 21;

    fn encode_into(&self, out: &mut Vec<u8>) {
        out.push(self.port);
        out.push(self.slot_id);
        out.push(self.interface_class);
        out.push(self.interface_sub_class);
        out.push(self.interface_protocol);
        out.push(self.attached as u8);
        out.push(self.ep_in_dci);
        put_u16(out, self.ep_in_mps);
        out.push(self.ep_in_interval);
        out.push(self.interface_num);
        put_u16(out, self.vendor_id);
        put_u16(out, self.product_id);
        out.push(self.bulk_in_dci);
        put_u16(out, self.bulk_in_mps);
        out.push(self.bulk_out_dci);
        put_u16(out, self.bulk_out_mps);
    }

    fn read(r: &mut Reader) -> Option<Self> {
        Some(AttachNotice {
            port: r.u8()?,
            slot_id: r.u8()?,
            interface_class: r.u8()?,
            interface_sub_class: r.u8()?,
            interface_protocol: r.u8()?,
            attached: r.u8()? != 0,
            ep_in_dci: r.u8()?,
            ep_in_mps: r.u16()?,
            ep_in_interval: r.u8()?,
            interface_num: r.u8()?,
            vendor_id: r.u16()?,
            product_id: r.u16()?,
            bulk_in_dci: r.u8()?,
            bulk_in_mps: r.u16()?,
            bulk_out_dci: r.u8()?,
            bulk_out_mps: r.u16()?,
        })
    }

    /// Encode to a fresh byte vector (for sending as IPC bulk).
    pub fn encode(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(Self::WIRE_LEN);
        self.encode_into(&mut v);
        v
    }

    /// Decode from the start of `buf`. Returns `None` on truncation.
    pub fn decode(buf: &[u8]) -> Option<Self> {
        Self::read(&mut Reader::new(buf))
    }
}

impl UsbRequest {
    /// Encode this request to IPC-bulk bytes (tag byte + fields).
    pub fn encode(&self) -> Vec<u8> {
        let mut v = Vec::new();
        match self {
            UsbRequest::GetDescriptors { slot_id } => {
                v.push(REQ_GET_DESCRIPTORS);
                v.push(*slot_id);
            }
            UsbRequest::ConfigureEndpoints {
                slot_id,
                configuration_value,
            } => {
                v.push(REQ_CONFIGURE_ENDPOINTS);
                v.push(*slot_id);
                v.push(*configuration_value);
            }
            UsbRequest::ControlRequest {
                slot_id,
                setup,
                length,
            } => {
                v.push(REQ_CONTROL);
                v.push(*slot_id);
                v.extend_from_slice(setup);
                put_u16(&mut v, *length);
            }
            UsbRequest::ControlWrite {
                slot_id,
                setup,
                data,
            } => {
                v.push(REQ_CONTROL_WRITE);
                v.push(*slot_id);
                v.extend_from_slice(setup);
                put_bytes(&mut v, data);
            }
            UsbRequest::SubmitTransfer {
                slot_id,
                dci,
                grant,
            } => {
                v.push(REQ_SUBMIT_TRANSFER);
                v.push(*slot_id);
                v.push(*dci);
                put_u32(&mut v, grant.cap);
                put_u32(&mut v, grant.len as u32);
            }
            UsbRequest::NextAttach { cursor } => {
                v.push(REQ_NEXT_ATTACH);
                v.push(*cursor);
            }
            UsbRequest::PollInterruptIn { slot_id, dci, len } => {
                v.push(REQ_POLL_INTERRUPT_IN);
                v.push(*slot_id);
                v.push(*dci);
                put_u16(&mut v, *len);
            }
            UsbRequest::PollBulkIn { slot_id, dci, len } => {
                v.push(REQ_POLL_BULK_IN);
                v.push(*slot_id);
                v.push(*dci);
                put_u16(&mut v, *len);
            }
            UsbRequest::SubmitBulkOut { slot_id, dci, data } => {
                v.push(REQ_SUBMIT_BULK_OUT);
                v.push(*slot_id);
                v.push(*dci);
                put_bytes(&mut v, data);
            }
            UsbRequest::Topology => {
                v.push(REQ_TOPOLOGY);
            }
        }
        v
    }

    /// Decode a request from IPC-bulk bytes. Returns `None` on a bad tag or
    /// truncation.
    pub fn decode(buf: &[u8]) -> Option<Self> {
        let mut r = Reader::new(buf);
        Some(match r.u8()? {
            REQ_GET_DESCRIPTORS => UsbRequest::GetDescriptors { slot_id: r.u8()? },
            REQ_CONFIGURE_ENDPOINTS => UsbRequest::ConfigureEndpoints {
                slot_id: r.u8()?,
                configuration_value: r.u8()?,
            },
            REQ_CONTROL => {
                let slot_id = r.u8()?;
                let mut setup = [0u8; 8];
                for b in &mut setup {
                    *b = r.u8()?;
                }
                let length = r.u16()?;
                UsbRequest::ControlRequest {
                    slot_id,
                    setup,
                    length,
                }
            }
            REQ_CONTROL_WRITE => {
                let slot_id = r.u8()?;
                let mut setup = [0u8; 8];
                for b in &mut setup {
                    *b = r.u8()?;
                }
                let data = r.bytes()?;
                UsbRequest::ControlWrite {
                    slot_id,
                    setup,
                    data,
                }
            }
            REQ_SUBMIT_TRANSFER => UsbRequest::SubmitTransfer {
                slot_id: r.u8()?,
                dci: r.u8()?,
                grant: PageGrant {
                    cap: r.u32()?,
                    len: r.u32()? as usize,
                },
            },
            REQ_NEXT_ATTACH => UsbRequest::NextAttach { cursor: r.u8()? },
            REQ_POLL_INTERRUPT_IN => UsbRequest::PollInterruptIn {
                slot_id: r.u8()?,
                dci: r.u8()?,
                len: r.u16()?,
            },
            REQ_POLL_BULK_IN => UsbRequest::PollBulkIn {
                slot_id: r.u8()?,
                dci: r.u8()?,
                len: r.u16()?,
            },
            REQ_SUBMIT_BULK_OUT => UsbRequest::SubmitBulkOut {
                slot_id: r.u8()?,
                dci: r.u8()?,
                data: r.bytes()?,
            },
            REQ_TOPOLOGY => UsbRequest::Topology,
            _ => return None,
        })
    }
}

impl UsbReply {
    /// Encode this reply to IPC reply-bulk bytes (tag byte + fields).
    pub fn encode(&self) -> Vec<u8> {
        let mut v = Vec::new();
        match self {
            UsbReply::Descriptors { device, config } => {
                v.push(REP_DESCRIPTORS);
                put_bytes(&mut v, device);
                put_bytes(&mut v, config);
            }
            UsbReply::EndpointsConfigured { slot_id } => {
                v.push(REP_ENDPOINTS_CONFIGURED);
                v.push(*slot_id);
            }
            UsbReply::ControlData {
                data,
                completion_code,
            } => {
                v.push(REP_CONTROL_DATA);
                v.push(*completion_code);
                put_bytes(&mut v, data);
            }
            UsbReply::TransferComplete {
                transferred,
                completion_code,
            } => {
                v.push(REP_TRANSFER_COMPLETE);
                v.push(*completion_code);
                put_u32(&mut v, *transferred as u32);
            }
            UsbReply::Attach { notice } => {
                v.push(REP_ATTACH);
                match notice {
                    Some(n) => {
                        v.push(1);
                        n.encode_into(&mut v);
                    }
                    None => v.push(0),
                }
            }
            UsbReply::InterruptReport {
                data,
                completion_code,
            } => {
                v.push(REP_INTERRUPT_REPORT);
                v.push(*completion_code);
                put_bytes(&mut v, data);
            }
            UsbReply::BulkData {
                data,
                completion_code,
            } => {
                v.push(REP_BULK_DATA);
                v.push(*completion_code);
                put_bytes(&mut v, data);
            }
            UsbReply::Error { code } => {
                v.push(REP_ERROR);
                put_u16(&mut v, *code);
            }
            UsbReply::Topology {
                discovered,
                port_counts,
                ports,
            } => {
                v.push(REP_TOPOLOGY);
                v.push(*discovered);
                put_bytes(&mut v, port_counts);
                put_u16(&mut v, ports.len() as u16);
                for p in ports {
                    v.push(p.ctrl);
                    v.push(p.port);
                    v.push(p.flags);
                }
            }
        }
        v
    }

    /// Decode a reply from IPC reply-bulk bytes. Returns `None` on a bad tag
    /// or truncation.
    pub fn decode(buf: &[u8]) -> Option<Self> {
        let mut r = Reader::new(buf);
        Some(match r.u8()? {
            REP_DESCRIPTORS => UsbReply::Descriptors {
                device: r.bytes()?,
                config: r.bytes()?,
            },
            REP_ENDPOINTS_CONFIGURED => UsbReply::EndpointsConfigured { slot_id: r.u8()? },
            REP_CONTROL_DATA => {
                let completion_code = r.u8()?;
                let data = r.bytes()?;
                UsbReply::ControlData {
                    data,
                    completion_code,
                }
            }
            REP_TRANSFER_COMPLETE => UsbReply::TransferComplete {
                completion_code: r.u8()?,
                transferred: r.u32()? as usize,
            },
            REP_ATTACH => {
                let present = r.u8()?;
                let notice = if present != 0 {
                    Some(AttachNotice::read(&mut r)?)
                } else {
                    None
                };
                UsbReply::Attach { notice }
            }
            REP_INTERRUPT_REPORT => {
                let completion_code = r.u8()?;
                let data = r.bytes()?;
                UsbReply::InterruptReport {
                    data,
                    completion_code,
                }
            }
            REP_BULK_DATA => {
                let completion_code = r.u8()?;
                let data = r.bytes()?;
                UsbReply::BulkData {
                    data,
                    completion_code,
                }
            }
            REP_ERROR => UsbReply::Error { code: r.u16()? },
            REP_TOPOLOGY => {
                let discovered = r.u8()?;
                let port_counts = r.bytes()?;
                let n = r.u16()? as usize;
                let mut ports = Vec::with_capacity(n);
                for _ in 0..n {
                    ports.push(TopoPort {
                        ctrl: r.u8()?,
                        port: r.u8()?,
                        flags: r.u8()?,
                    });
                }
                UsbReply::Topology {
                    discovered,
                    port_counts,
                    ports,
                }
            }
            _ => return None,
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_kbd_notice() -> AttachNotice {
        AttachNotice {
            port: 2,
            slot_id: 5,
            interface_class: 0x03,
            interface_sub_class: 0x01,
            interface_protocol: 0x01,
            attached: true,
            ep_in_dci: 3,
            ep_in_mps: 8,
            ep_in_interval: 10,
            interface_num: 0,
            vendor_id: 0x046d,
            product_id: 0xc31c,
            bulk_in_dci: 0,
            bulk_in_mps: 0,
            bulk_out_dci: 0,
            bulk_out_mps: 0,
        }
    }

    fn sample_ure_notice() -> AttachNotice {
        AttachNotice {
            port: 1,
            slot_id: 3,
            interface_class: 0xff,
            interface_sub_class: 0xff,
            interface_protocol: 0xff,
            attached: true,
            ep_in_dci: 0,
            ep_in_mps: 0,
            ep_in_interval: 0,
            interface_num: 0,
            vendor_id: 0x0bda,
            product_id: 0x8156,
            bulk_in_dci: 5,
            bulk_in_mps: 512,
            bulk_out_dci: 4,
            bulk_out_mps: 512,
        }
    }

    #[test]
    fn attach_notice_ure_bulk_round_trip() {
        let notice = sample_ure_notice();
        let bytes = notice.encode();
        assert_eq!(bytes.len(), AttachNotice::WIRE_LEN);
        let decoded = AttachNotice::decode(&bytes).expect("decode");
        assert_eq!(decoded, notice);
        assert_eq!(decoded.vendor_id, 0x0bda);
        assert_eq!(decoded.product_id, 0x8156);
        assert_eq!(decoded.bulk_in_dci, 5);
        assert_eq!(decoded.bulk_out_dci, 4);
        assert_eq!(decoded.bulk_out_mps, 512);
    }

    #[test]
    fn attach_notice_fields() {
        let notice = sample_kbd_notice();
        assert!(notice.attached);
        assert_eq!(notice.interface_class, 0x03);
        assert_eq!(notice.interface_protocol, 0x01);
        assert_eq!(notice.ep_in_dci, 3);
    }

    #[test]
    fn attach_notice_detach() {
        let notice = AttachNotice {
            port: 1,
            slot_id: 2,
            interface_class: 0x09,
            interface_sub_class: 0x00,
            interface_protocol: 0x00,
            attached: false,
            ep_in_dci: 0,
            ep_in_mps: 0,
            ep_in_interval: 0,
            interface_num: 0,
            vendor_id: 0,
            product_id: 0,
            bulk_in_dci: 0,
            bulk_in_mps: 0,
            bulk_out_dci: 0,
            bulk_out_mps: 0,
        };
        assert!(!notice.attached);
    }

    #[test]
    fn attach_notice_wire_roundtrip() {
        let notice = sample_kbd_notice();
        let bytes = notice.encode();
        assert_eq!(bytes.len(), AttachNotice::WIRE_LEN);
        assert_eq!(AttachNotice::decode(&bytes), Some(notice));
        // Truncated input decodes to None, never panics.
        assert_eq!(AttachNotice::decode(&bytes[..3]), None);
    }

    #[test]
    fn request_wire_roundtrips() {
        let reqs = [
            UsbRequest::GetDescriptors { slot_id: 4 },
            UsbRequest::ConfigureEndpoints {
                slot_id: 4,
                configuration_value: 1,
            },
            UsbRequest::ControlRequest {
                slot_id: 4,
                setup: [0x21, 0x0B, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
                length: 0,
            },
            UsbRequest::SubmitTransfer {
                slot_id: 4,
                dci: 3,
                grant: PageGrant { cap: 9, len: 4096 },
            },
            UsbRequest::NextAttach { cursor: 1 },
            UsbRequest::PollInterruptIn {
                slot_id: 4,
                dci: 3,
                len: 8,
            },
            UsbRequest::ControlWrite {
                slot_id: 4,
                // OCP write: bmRequestType=0x40, bRequest=0x05, wValue=0xe813,
                // wIndex=MCU_TYPE_PLA|byte_en, wLength=2.
                setup: [0x40, 0x05, 0x13, 0xe8, 0x00, 0x01, 0x02, 0x00],
                data: alloc::vec![0x0c, 0x00],
            },
            UsbRequest::PollBulkIn {
                slot_id: 1,
                dci: 5,
                len: 2048,
            },
            UsbRequest::SubmitBulkOut {
                slot_id: 1,
                dci: 4,
                data: alloc::vec![0x00, 0x00, 0x40, 0x00, 0xde, 0xad, 0xbe, 0xef],
            },
        ];
        for r in reqs {
            let bytes = r.encode();
            let back = UsbRequest::decode(&bytes).expect("decode");
            assert_eq!(back, r);
        }
        assert_eq!(UsbRequest::decode(&[]), None);
        assert_eq!(UsbRequest::decode(&[0xFE]), None); // unknown tag
    }

    #[test]
    fn control_write_carries_inline_out_data() {
        let client = UsbClient::new(2, 7);
        let setup = [0x40u8, 0x05, 0x13, 0xe8, 0x00, 0x01, 0x02, 0x00];
        let req = client.control_write_request(setup, alloc::vec![0xab, 0xcd]);
        match &req {
            UsbRequest::ControlWrite {
                slot_id,
                setup: s,
                data,
            } => {
                assert_eq!(*slot_id, 2);
                assert_eq!(s[0] & 0x80, 0); // OUT direction (D2H bit clear)
                assert_eq!(s[1], 0x05); // OCP vendor request
                assert_eq!(data, &alloc::vec![0xab, 0xcd]);
            }
            _ => panic!("wrong variant"),
        }
        // Round-trips through the wire codec with the data stage intact.
        let back = UsbRequest::decode(&req.encode()).expect("decode");
        assert_eq!(back, req);
    }

    #[test]
    fn reply_wire_roundtrips() {
        let replies = [
            UsbReply::Descriptors {
                device: alloc::vec![1, 2, 3],
                config: alloc::vec![4, 5, 6, 7],
            },
            UsbReply::EndpointsConfigured { slot_id: 5 },
            UsbReply::ControlData {
                data: alloc::vec![],
                completion_code: 1,
            },
            UsbReply::TransferComplete {
                transferred: 8,
                completion_code: 1,
            },
            UsbReply::Attach {
                notice: Some(sample_kbd_notice()),
            },
            UsbReply::Attach { notice: None },
            UsbReply::InterruptReport {
                data: alloc::vec![0, 0, 0x04, 0, 0, 0, 0, 0],
                completion_code: 1,
            },
            UsbReply::InterruptReport {
                data: alloc::vec![],
                completion_code: 0,
            },
            UsbReply::BulkData {
                data: alloc::vec![0xaa, 0xbb, 0xcc],
                completion_code: 1,
            },
            UsbReply::BulkData {
                data: alloc::vec![],
                completion_code: 0,
            },
            UsbReply::Error { code: 19 },
            UsbReply::Topology {
                discovered: 2,
                port_counts: alloc::vec![16, 24],
                ports: alloc::vec![
                    TopoPort {
                        ctrl: 0,
                        port: 3,
                        flags: TopoPort::pack(true, true, true, 3),
                    },
                    TopoPort {
                        ctrl: 1,
                        port: 9,
                        flags: TopoPort::pack(true, false, true, 4),
                    },
                ],
            },
        ];
        for r in replies {
            let bytes = r.encode();
            let back = UsbReply::decode(&bytes).expect("decode");
            assert_eq!(back.encode(), bytes);
        }
        assert_eq!(UsbReply::decode(&[]), None);
    }

    #[test]
    fn topology_request_and_reply_roundtrip() {
        let req = UsbRequest::Topology;
        assert_eq!(UsbRequest::decode(&req.encode()), Some(req));

        let reply = UsbReply::Topology {
            discovered: 2,
            port_counts: alloc::vec![8, 22],
            ports: alloc::vec![TopoPort {
                ctrl: 1,
                port: 5,
                flags: TopoPort::pack(true, false, true, 4),
            }],
        };
        match UsbReply::decode(&reply.encode()).expect("decode") {
            UsbReply::Topology {
                discovered,
                port_counts,
                ports,
            } => {
                assert_eq!(discovered, 2);
                assert_eq!(port_counts, alloc::vec![8, 22]);
                assert_eq!(ports.len(), 1);
                assert_eq!(ports[0].ctrl, 1);
                assert_eq!(ports[0].port, 5);
                assert!(ports[0].ccs());
                assert!(!ports[0].ped());
                assert_eq!(ports[0].speed_psi(), 4); // SuperSpeed
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn interrupt_report_inline_decodes_to_report_bytes() {
        let report = alloc::vec![0x00, 0x00, 0x04, 0x05, 0x00, 0x00, 0x00, 0x00];
        let reply = UsbReply::InterruptReport {
            data: report.clone(),
            completion_code: 1,
        };
        match UsbReply::decode(&reply.encode()).unwrap() {
            UsbReply::InterruptReport {
                data,
                completion_code,
            } => {
                assert_eq!(data, report);
                assert_eq!(completion_code, 1);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn service_name_and_labels_are_stable() {
        assert_eq!(USB_SERVICE_NAME, "usb");
        assert_eq!(USB_REQ_LABEL, 1);
    }

    #[test]
    fn usb_client_get_descriptors_request() {
        let client = UsbClient::new(3, 10);
        let req = client.get_descriptors_request();
        match req {
            UsbRequest::GetDescriptors { slot_id } => assert_eq!(slot_id, 3),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn usb_client_configure_endpoints_request() {
        let client = UsbClient::new(7, 20);
        let req = client.configure_endpoints_request(1);
        match req {
            UsbRequest::ConfigureEndpoints {
                slot_id,
                configuration_value,
            } => {
                assert_eq!(slot_id, 7);
                assert_eq!(configuration_value, 1);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn usb_client_control_request_setup_bytes() {
        let client = UsbClient::new(1, 5);
        // GET_DESCRIPTOR(Device) setup packet: 0x80 0x06 0x00 0x01 0x00 0x00 0x12 0x00
        let setup = [0x80u8, 0x06, 0x00, 0x01, 0x00, 0x00, 0x12, 0x00];
        let req = client.control_request(setup, 18);
        match req {
            UsbRequest::ControlRequest {
                slot_id,
                setup: s,
                length,
            } => {
                assert_eq!(slot_id, 1);
                assert_eq!(s[0], 0x80); // bmRequestType: D2H Standard Device
                assert_eq!(s[1], 0x06); // bRequest: GET_DESCRIPTOR
                assert_eq!(length, 18);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn usb_client_submit_transfer_carries_grant() {
        let client = UsbClient::new(4, 99);
        let grant = PageGrant { cap: 7, len: 4096 };
        let req = client.submit_transfer_request(3, grant);
        match req {
            UsbRequest::SubmitTransfer {
                slot_id,
                dci,
                grant: g,
            } => {
                assert_eq!(slot_id, 4);
                assert_eq!(dci, 3);
                assert_eq!(g.cap, 7);
                assert_eq!(g.len, 4096);
                // Crucially: no inline byte buffer — transfer data lives in the grant.
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn page_grant_is_copy() {
        let grant = PageGrant { cap: 1, len: 64 };
        let copy = grant;
        assert_eq!(copy.cap, 1);
        assert_eq!(copy.len, 64);
    }

    #[test]
    fn usb_reply_transfer_complete_roundtrip() {
        let reply = UsbReply::TransferComplete {
            transferred: 8,
            completion_code: 1,
        };
        match reply {
            UsbReply::TransferComplete {
                transferred,
                completion_code,
            } => {
                assert_eq!(transferred, 8);
                assert_eq!(completion_code, 1);
            }
            _ => panic!("wrong variant"),
        }
    }
}
