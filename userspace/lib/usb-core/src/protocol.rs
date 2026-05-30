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
}

// ---------------------------------------------------------------------------
// UsbRequest — messages from a class driver to the xHCI server
// ---------------------------------------------------------------------------

/// A request from a class driver to the xHCI host server.
///
/// Requests are issued synchronously via `sys_ipc_call`; the matching
/// [`UsbReply`] is the rendezvous response.
#[derive(Debug, Clone)]
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
}

// ---------------------------------------------------------------------------
// UsbReply — responses from the xHCI server to a class driver
// ---------------------------------------------------------------------------

/// Reply from the xHCI host server to a class driver.
#[derive(Debug, Clone)]
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

    /// The request failed for a reason not captured by a specific variant.
    Error {
        /// Human-readable error description (short, for logging only).
        message: &'static str,
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
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attach_notice_fields() {
        let notice = AttachNotice {
            port: 2,
            slot_id: 5,
            interface_class: 0x03,
            interface_sub_class: 0x01,
            interface_protocol: 0x01,
            attached: true,
        };
        assert!(notice.attached);
        assert_eq!(notice.interface_class, 0x03);
        assert_eq!(notice.interface_protocol, 0x01);
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
        };
        assert!(!notice.attached);
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
