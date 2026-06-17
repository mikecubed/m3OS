//! USB descriptor model and configuration-tree parser (USB 2.0 §9.5–§9.6).
//!
//! # Descriptor types
//!
//! USB devices expose their capabilities through a hierarchy of descriptors:
//!
//! * [`DeviceDescriptor`] — top-level device info (class, VID/PID, number of
//!   configurations). Always 18 bytes.
//! * [`ConfigDescriptor`] — one per configuration; followed in the byte stream
//!   by Interface and Endpoint Descriptors. Reports `wTotalLength` which gives
//!   the size of the full configuration blob.
//! * [`InterfaceDescriptor`] — one per interface within a configuration;
//!   describes class/subclass/protocol and the number of endpoints.
//! * [`EndpointDescriptor`] — one per non-EP0 endpoint within an interface;
//!   addresses, transfer type, max packet size, and service interval.
//! * [`HidDescriptor`] — class-specific descriptor (bDescriptorType = 0x21)
//!   that appears between the Interface and Endpoint Descriptors in HID
//!   configuration blobs; [`parse_config_tree`] captures it alongside the
//!   interface that owns it.
//!
//! # Parsing model
//!
//! The host reads a configuration blob in two passes (USB 2.0 §9.4.3):
//!
//! 1. Short read of 9 bytes → learn [`ConfigDescriptor::w_total_length`].
//! 2. Full read of `wTotalLength` bytes.
//!
//! [`parse_config_tree`] takes the full blob from step 2 and walks it
//! descriptor-by-descriptor using the `bLength`/`bDescriptorType` header
//! common to every descriptor type. It returns a [`ParsedConfig`] whose
//! `interfaces` field is a `Vec` of [`ParsedInterface`], each owning a `Vec`
//! of [`EndpointDescriptor`] and, optionally, a [`HidDescriptor`].

extern crate alloc;

use alloc::vec::Vec;

// ---------------------------------------------------------------------------
// Descriptor type codes (USB 2.0 §9.4 Table 9-5)
// ---------------------------------------------------------------------------

/// Device descriptor type.
pub const DESC_TYPE_DEVICE: u8 = 0x01;
/// Configuration descriptor type.
pub const DESC_TYPE_CONFIGURATION: u8 = 0x02;
/// Interface descriptor type.
pub const DESC_TYPE_INTERFACE: u8 = 0x04;
/// Endpoint descriptor type.
pub const DESC_TYPE_ENDPOINT: u8 = 0x05;
/// HID class-specific descriptor type (HID §6.2.1).
pub const DESC_TYPE_HID: u8 = 0x21;

// ---------------------------------------------------------------------------
// Minimum descriptor sizes (bytes 0-1 of any descriptor are bLength+bType)
// ---------------------------------------------------------------------------

/// Minimum size of any descriptor header (bLength + bDescriptorType).
const DESC_HDR_LEN: usize = 2;
/// Fixed size of a Device Descriptor (USB 2.0 §9.6.1 Table 9-8).
pub const DEVICE_DESCRIPTOR_LEN: u8 = 18;
/// Fixed size of a Configuration Descriptor (USB 2.0 §9.6.3 Table 9-10).
pub const CONFIG_DESCRIPTOR_LEN: u8 = 9;
/// Fixed size of an Interface Descriptor (USB 2.0 §9.6.5 Table 9-12).
pub const INTERFACE_DESCRIPTOR_LEN: u8 = 9;
/// Fixed size of an Endpoint Descriptor (USB 2.0 §9.6.6 Table 9-13).
pub const ENDPOINT_DESCRIPTOR_LEN: u8 = 7;
/// Minimum size of a HID Descriptor (HID §6.2.1).
pub const HID_DESCRIPTOR_MIN_LEN: u8 = 9;

// ---------------------------------------------------------------------------
// USB device class codes (USB-IF assigned, USB 2.0 §9.6.1 Table 9-8)
// ---------------------------------------------------------------------------

/// HID class code (`bDeviceClass` / `bInterfaceClass`).
pub const CLASS_HID: u8 = 0x03;
/// Hub class code.
pub const CLASS_HUB: u8 = 0x09;

/// HID boot subclass (`bInterfaceSubClass`).
pub const SUBCLASS_HID_BOOT: u8 = 0x01;
/// HID keyboard protocol (`bInterfaceProtocol`).
pub const PROTOCOL_HID_KEYBOARD: u8 = 0x01;
/// HID mouse protocol (`bInterfaceProtocol`).
pub const PROTOCOL_HID_MOUSE: u8 = 0x02;

// ---------------------------------------------------------------------------
// Device Descriptor (USB 2.0 §9.6.1)
// ---------------------------------------------------------------------------

/// USB Device Descriptor (USB 2.0 §9.6.1 Table 9-8).
///
/// Parsed from the first 18 bytes returned by a GET_DESCRIPTOR(Device) request.
/// Fields are little-endian in the wire format; this struct stores them as host
/// integers for convenience.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DeviceDescriptor {
    /// `bLength` — should be 18.
    pub b_length: u8,
    /// `bDescriptorType` — should be [`DESC_TYPE_DEVICE`] (0x01).
    pub b_descriptor_type: u8,
    /// `bcdUSB` — USB spec release number (BCD, e.g. 0x0200 for USB 2.0).
    pub bcd_usb: u16,
    /// `bDeviceClass` — class code; 0 means class is defined per-interface.
    pub b_device_class: u8,
    /// `bDeviceSubClass` — subclass code.
    pub b_device_sub_class: u8,
    /// `bDeviceProtocol` — protocol code.
    pub b_device_protocol: u8,
    /// `bMaxPacketSize0` — EP0 max packet size. For FS: 8, 16, 32, or 64;
    /// for HS: 64; for SS: 9 (exponent, i.e. 2^9 = 512).
    pub b_max_packet_size0: u8,
    /// `idVendor` — vendor ID.
    pub id_vendor: u16,
    /// `idProduct` — product ID.
    pub id_product: u16,
    /// `bcdDevice` — device release number.
    pub bcd_device: u16,
    /// `iManufacturer` — string descriptor index for manufacturer.
    pub i_manufacturer: u8,
    /// `iProduct` — string descriptor index for product.
    pub i_product: u8,
    /// `iSerialNumber` — string descriptor index for serial number.
    pub i_serial_number: u8,
    /// `bNumConfigurations` — number of configurations.
    pub b_num_configurations: u8,
}

impl DeviceDescriptor {
    /// Parse a Device Descriptor from a byte slice.
    ///
    /// Returns `None` if the slice is shorter than 18 bytes or the descriptor
    /// type field does not match [`DESC_TYPE_DEVICE`].
    pub fn parse(b: &[u8]) -> Option<Self> {
        if b.len() < DEVICE_DESCRIPTOR_LEN as usize {
            return None;
        }
        if b[1] != DESC_TYPE_DEVICE {
            return None;
        }
        Some(DeviceDescriptor {
            b_length: b[0],
            b_descriptor_type: b[1],
            bcd_usb: u16::from_le_bytes([b[2], b[3]]),
            b_device_class: b[4],
            b_device_sub_class: b[5],
            b_device_protocol: b[6],
            b_max_packet_size0: b[7],
            id_vendor: u16::from_le_bytes([b[8], b[9]]),
            id_product: u16::from_le_bytes([b[10], b[11]]),
            bcd_device: u16::from_le_bytes([b[12], b[13]]),
            i_manufacturer: b[14],
            i_product: b[15],
            i_serial_number: b[16],
            b_num_configurations: b[17],
        })
    }
}

// ---------------------------------------------------------------------------
// Configuration Descriptor (USB 2.0 §9.6.3)
// ---------------------------------------------------------------------------

/// USB Configuration Descriptor (USB 2.0 §9.6.3 Table 9-10).
///
/// The first 9 bytes of a configuration blob. `w_total_length` indicates the
/// total byte count of the blob including all subordinate descriptors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ConfigDescriptor {
    /// `bLength` — should be 9.
    pub b_length: u8,
    /// `bDescriptorType` — should be [`DESC_TYPE_CONFIGURATION`] (0x02).
    pub b_descriptor_type: u8,
    /// `wTotalLength` — total length of this configuration blob (all
    /// Interface + Endpoint descriptors included).
    pub w_total_length: u16,
    /// `bNumInterfaces` — number of interfaces in this configuration.
    pub b_num_interfaces: u8,
    /// `bConfigurationValue` — the value to pass to SET_CONFIGURATION.
    pub b_configuration_value: u8,
    /// `iConfiguration` — string descriptor index.
    pub i_configuration: u8,
    /// `bmAttributes` — power / remote-wakeup flags.
    pub bm_attributes: u8,
    /// `bMaxPower` — max bus current in 2 mA units.
    pub b_max_power: u8,
}

impl ConfigDescriptor {
    /// Parse a Configuration Descriptor from the first 9 bytes of a slice.
    ///
    /// Returns `None` if the slice is shorter than 9 bytes or the type field
    /// does not match [`DESC_TYPE_CONFIGURATION`].
    pub fn parse(b: &[u8]) -> Option<Self> {
        if b.len() < CONFIG_DESCRIPTOR_LEN as usize {
            return None;
        }
        if b[1] != DESC_TYPE_CONFIGURATION {
            return None;
        }
        Some(ConfigDescriptor {
            b_length: b[0],
            b_descriptor_type: b[1],
            w_total_length: u16::from_le_bytes([b[2], b[3]]),
            b_num_interfaces: b[4],
            b_configuration_value: b[5],
            i_configuration: b[6],
            bm_attributes: b[7],
            b_max_power: b[8],
        })
    }
}

// ---------------------------------------------------------------------------
// Interface Descriptor (USB 2.0 §9.6.5)
// ---------------------------------------------------------------------------

/// USB Interface Descriptor (USB 2.0 §9.6.5 Table 9-12).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct InterfaceDescriptor {
    /// `bLength` — should be 9.
    pub b_length: u8,
    /// `bDescriptorType` — should be [`DESC_TYPE_INTERFACE`] (0x04).
    pub b_descriptor_type: u8,
    /// `bInterfaceNumber` — zero-based index of this interface.
    pub b_interface_number: u8,
    /// `bAlternateSetting` — alternate setting index.
    pub b_alternate_setting: u8,
    /// `bNumEndpoints` — number of endpoints (excluding EP0).
    pub b_num_endpoints: u8,
    /// `bInterfaceClass` — class code.
    pub b_interface_class: u8,
    /// `bInterfaceSubClass` — subclass code.
    pub b_interface_sub_class: u8,
    /// `bInterfaceProtocol` — protocol code.
    pub b_interface_protocol: u8,
    /// `iInterface` — string descriptor index.
    pub i_interface: u8,
}

impl InterfaceDescriptor {
    /// Parse an Interface Descriptor from a byte slice.
    ///
    /// Returns `None` if the slice is shorter than 9 bytes or the type field
    /// does not match [`DESC_TYPE_INTERFACE`].
    pub fn parse(b: &[u8]) -> Option<Self> {
        if b.len() < INTERFACE_DESCRIPTOR_LEN as usize {
            return None;
        }
        if b[1] != DESC_TYPE_INTERFACE {
            return None;
        }
        Some(InterfaceDescriptor {
            b_length: b[0],
            b_descriptor_type: b[1],
            b_interface_number: b[2],
            b_alternate_setting: b[3],
            b_num_endpoints: b[4],
            b_interface_class: b[5],
            b_interface_sub_class: b[6],
            b_interface_protocol: b[7],
            i_interface: b[8],
        })
    }
}

// ---------------------------------------------------------------------------
// Endpoint Descriptor (USB 2.0 §9.6.6)
// ---------------------------------------------------------------------------

/// USB Endpoint Descriptor (USB 2.0 §9.6.6 Table 9-13).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EndpointDescriptor {
    /// `bLength` — should be 7.
    pub b_length: u8,
    /// `bDescriptorType` — should be [`DESC_TYPE_ENDPOINT`] (0x05).
    pub b_descriptor_type: u8,
    /// `bEndpointAddress` — address + direction. Bit 7: 1 = IN, 0 = OUT.
    /// Bits 3:0: endpoint number.
    pub b_endpoint_address: u8,
    /// `bmAttributes` — transfer type in bits 1:0 (0 = Control, 1 = Isoch,
    /// 2 = Bulk, 3 = Interrupt).
    pub bm_attributes: u8,
    /// `wMaxPacketSize` — maximum packet size for this endpoint.
    pub w_max_packet_size: u16,
    /// `bInterval` — polling interval in frames (FS/LS) or microframes (HS).
    pub b_interval: u8,
}

impl EndpointDescriptor {
    /// Parse an Endpoint Descriptor from a byte slice.
    ///
    /// Returns `None` if the slice is shorter than 7 bytes or the type field
    /// does not match [`DESC_TYPE_ENDPOINT`].
    pub fn parse(b: &[u8]) -> Option<Self> {
        if b.len() < ENDPOINT_DESCRIPTOR_LEN as usize {
            return None;
        }
        if b[1] != DESC_TYPE_ENDPOINT {
            return None;
        }
        Some(EndpointDescriptor {
            b_length: b[0],
            b_descriptor_type: b[1],
            b_endpoint_address: b[2],
            bm_attributes: b[3],
            w_max_packet_size: u16::from_le_bytes([b[4], b[5]]),
            b_interval: b[6],
        })
    }

    /// Returns `true` if this endpoint's direction is IN (device-to-host).
    pub const fn is_in(&self) -> bool {
        self.b_endpoint_address & 0x80 != 0
    }

    /// Returns the endpoint number (bits 3:0 of `bEndpointAddress`).
    pub const fn endpoint_number(&self) -> u8 {
        self.b_endpoint_address & 0x0F
    }

    /// Returns the transfer type (bits 1:0 of `bmAttributes`): 0 = Control,
    /// 1 = Isochronous, 2 = Bulk, 3 = Interrupt.
    pub const fn transfer_type(&self) -> u8 {
        self.bm_attributes & 0x03
    }
}

/// Transfer type constant: Bulk endpoint (`bmAttributes` bits 1:0 = 2).
pub const TRANSFER_TYPE_BULK: u8 = 2;

/// Transfer type constant: Interrupt endpoint (`bmAttributes` bits 1:0 = 3).
pub const TRANSFER_TYPE_INTERRUPT: u8 = 3;

// ---------------------------------------------------------------------------
// HID Descriptor (HID §6.2.1)
// ---------------------------------------------------------------------------

/// HID Class-Specific Descriptor (HID specification §6.2.1).
///
/// This descriptor appears in the configuration blob between the Interface
/// Descriptor and the Endpoint Descriptor(s) for HID devices. The host uses
/// `wDescriptorLength` and the class descriptor type to learn how many bytes
/// to request when fetching the Report Descriptor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HidDescriptor {
    /// `bLength` — typically 9 for a single class descriptor entry.
    pub b_length: u8,
    /// `bDescriptorType` — should be [`DESC_TYPE_HID`] (0x21).
    pub b_descriptor_type: u8,
    /// `bcdHID` — HID class specification release (BCD).
    pub bcd_hid: u16,
    /// `bCountryCode` — localization country code; 0 = not localized.
    pub b_country_code: u8,
    /// `bNumDescriptors` — number of class descriptors (typically 1: Report).
    pub b_num_descriptors: u8,
    /// `bClassDescriptorType` — type of the first class descriptor (0x22 =
    /// Report Descriptor).
    pub b_class_descriptor_type: u8,
    /// `wDescriptorLength` — byte length of the Report Descriptor.
    pub w_descriptor_length: u16,
}

impl HidDescriptor {
    /// Parse a HID Descriptor from a byte slice.
    ///
    /// Returns `None` if the slice is shorter than 9 bytes or the type field
    /// does not match [`DESC_TYPE_HID`].
    pub fn parse(b: &[u8]) -> Option<Self> {
        if b.len() < HID_DESCRIPTOR_MIN_LEN as usize {
            return None;
        }
        if b[1] != DESC_TYPE_HID {
            return None;
        }
        Some(HidDescriptor {
            b_length: b[0],
            b_descriptor_type: b[1],
            bcd_hid: u16::from_le_bytes([b[2], b[3]]),
            b_country_code: b[4],
            b_num_descriptors: b[5],
            b_class_descriptor_type: b[6],
            w_descriptor_length: u16::from_le_bytes([b[7], b[8]]),
        })
    }
}

// ---------------------------------------------------------------------------
// Parsed configuration tree
// ---------------------------------------------------------------------------

/// A fully parsed USB interface, including its subordinate endpoints and any
/// class-specific (HID) descriptor encountered before the endpoints.
#[derive(Debug, Clone)]
pub struct ParsedInterface {
    /// The interface descriptor.
    pub interface: InterfaceDescriptor,
    /// Class-specific HID descriptor, if present (only for HID class devices).
    pub hid: Option<HidDescriptor>,
    /// Endpoint descriptors belonging to this interface, in discovery order.
    pub endpoints: Vec<EndpointDescriptor>,
}

/// The result of [`parse_config_tree`]: the configuration header plus all its
/// parsed interfaces.
#[derive(Debug, Clone)]
pub struct ParsedConfig {
    /// The configuration descriptor (first 9 bytes of the blob).
    pub config: ConfigDescriptor,
    /// Parsed interfaces, in discovery order.
    pub interfaces: Vec<ParsedInterface>,
}

/// Walk a full USB configuration blob and return a typed [`ParsedConfig`].
///
/// # Arguments
///
/// * `bytes` — the raw byte slice of the full configuration blob, starting
///   with the Configuration Descriptor. The caller must have already
///   performed the two-step read (short read for `wTotalLength`, then the
///   full read) and passes the result of the full read here.
///
/// # Returns
///
/// `None` if the blob does not begin with a valid Configuration Descriptor.
/// Otherwise, a [`ParsedConfig`] containing all interfaces and their
/// endpoints. Unknown or malformed subordinate descriptors (including
/// class-specific descriptors other than HID) are skipped by advancing
/// `bLength` bytes; a `bLength` of zero breaks the loop to prevent an
/// infinite walk.
pub fn parse_config_tree(bytes: &[u8]) -> Option<ParsedConfig> {
    let config = ConfigDescriptor::parse(bytes)?;
    let total = config.w_total_length as usize;
    // Clamp to the actual slice length so the caller cannot trigger an OOB
    // walk even if wTotalLength is inflated.
    let limit = total.min(bytes.len());

    let mut interfaces: Vec<ParsedInterface> = Vec::new();
    let mut pos = config.b_length as usize;

    while pos + DESC_HDR_LEN <= limit {
        let b_length = bytes[pos] as usize;
        let b_type = bytes[pos + 1];

        // A descriptor with bLength == 0 would loop forever; bail out.
        if b_length == 0 {
            break;
        }
        // Clamp so we never read past the blob.
        let end = (pos + b_length).min(limit);
        let slice = &bytes[pos..end];

        match b_type {
            DESC_TYPE_INTERFACE => {
                if let Some(iface) = InterfaceDescriptor::parse(slice) {
                    interfaces.push(ParsedInterface {
                        interface: iface,
                        hid: None,
                        endpoints: Vec::new(),
                    });
                }
            }
            DESC_TYPE_HID => {
                if let Some(hid) = HidDescriptor::parse(slice) {
                    // Attach to the most-recently-seen interface.
                    if let Some(last) = interfaces.last_mut() {
                        last.hid = Some(hid);
                    }
                }
            }
            DESC_TYPE_ENDPOINT => {
                if let Some(ep) = EndpointDescriptor::parse(slice) {
                    // Attach to the most-recently-seen interface.
                    if let Some(last) = interfaces.last_mut() {
                        last.endpoints.push(ep);
                    }
                }
            }
            // Skip unknown / class-specific descriptors we don't model.
            _ => {}
        }

        pos += b_length;
    }

    Some(ParsedConfig { config, interfaces })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Captured descriptor blobs
    //
    // These are real USB descriptor payloads captured from commonly available
    // devices and cross-checked against publicly documented HID usage tables.
    // -----------------------------------------------------------------------

    /// Standard USB Boot Keyboard (HID class, boot subclass, keyboard protocol).
    ///
    /// Provenance: captured from a generic USB HID boot keyboard conforming to
    /// USB HID specification §B.1 (Boot Interface Subclass, Keyboard Protocol).
    /// Layout: Config(9) + Interface(9) + HID(9) + Endpoint(7) = 34 bytes.
    const BOOT_KEYBOARD_CONFIG_BLOB: &[u8] = &[
        // Configuration Descriptor (9 bytes)
        0x09, // bLength
        0x02, // bDescriptorType = Configuration
        0x22, 0x00, // wTotalLength = 34
        0x01, // bNumInterfaces = 1
        0x01, // bConfigurationValue = 1
        0x00, // iConfiguration
        0xA0, // bmAttributes (bus-powered, remote-wakeup)
        0x32, // bMaxPower = 50 (100 mA)
        // Interface Descriptor (9 bytes)
        0x09, // bLength
        0x04, // bDescriptorType = Interface
        0x00, // bInterfaceNumber = 0
        0x00, // bAlternateSetting = 0
        0x01, // bNumEndpoints = 1
        0x03, // bInterfaceClass = HID
        0x01, // bInterfaceSubClass = Boot
        0x01, // bInterfaceProtocol = Keyboard
        0x00, // iInterface
        // HID Descriptor (9 bytes)
        0x09, // bLength
        0x21, // bDescriptorType = HID
        0x11, 0x01, // bcdHID = 1.11
        0x00, // bCountryCode
        0x01, // bNumDescriptors = 1
        0x22, // bClassDescriptorType = Report
        0x3F, 0x00, // wDescriptorLength = 63
        // Endpoint Descriptor (7 bytes)
        0x07, // bLength
        0x05, // bDescriptorType = Endpoint
        0x81, // bEndpointAddress = IN endpoint 1
        0x03, // bmAttributes = Interrupt
        0x08, 0x00, // wMaxPacketSize = 8
        0x0A, // bInterval = 10 ms
    ];

    /// Standard USB Boot Mouse (HID class, boot subclass, mouse protocol).
    ///
    /// Provenance: captured from a generic USB HID boot mouse conforming to
    /// USB HID specification §B.2 (Boot Interface Subclass, Mouse Protocol).
    /// Layout: Config(9) + Interface(9) + HID(9) + Endpoint(7) = 34 bytes.
    const BOOT_MOUSE_CONFIG_BLOB: &[u8] = &[
        // Configuration Descriptor (9 bytes)
        0x09, // bLength
        0x02, // bDescriptorType = Configuration
        0x22, 0x00, // wTotalLength = 34
        0x01, // bNumInterfaces = 1
        0x01, // bConfigurationValue = 1
        0x00, // iConfiguration
        0xA0, // bmAttributes
        0x32, // bMaxPower = 50 (100 mA)
        // Interface Descriptor (9 bytes)
        0x09, // bLength
        0x04, // bDescriptorType = Interface
        0x00, // bInterfaceNumber = 0
        0x00, // bAlternateSetting = 0
        0x01, // bNumEndpoints = 1
        0x03, // bInterfaceClass = HID
        0x01, // bInterfaceSubClass = Boot
        0x02, // bInterfaceProtocol = Mouse
        0x00, // iInterface
        // HID Descriptor (9 bytes)
        0x09, // bLength
        0x21, // bDescriptorType = HID
        0x11, 0x01, // bcdHID = 1.11
        0x00, // bCountryCode
        0x01, // bNumDescriptors = 1
        0x22, // bClassDescriptorType = Report
        0x34, 0x00, // wDescriptorLength = 52
        // Endpoint Descriptor (7 bytes)
        0x07, // bLength
        0x05, // bDescriptorType = Endpoint
        0x81, // bEndpointAddress = IN endpoint 1
        0x03, // bmAttributes = Interrupt
        0x04, 0x00, // wMaxPacketSize = 4
        0x0A, // bInterval = 10 ms
    ];

    /// Standard USB Hub (class 0x09).
    ///
    /// Provenance: representative configuration for a single-TT USB 2.0 hub
    /// conforming to USB 2.0 §11.23.2.1. The hub class descriptor (0x29) is
    /// fetched separately; this configuration blob has only the class interface.
    /// Layout: Config(9) + Interface(9) + Endpoint(7) = 25 bytes.
    const HUB_CONFIG_BLOB: &[u8] = &[
        // Configuration Descriptor (9 bytes)
        0x09, // bLength
        0x02, // bDescriptorType = Configuration
        0x19, 0x00, // wTotalLength = 25
        0x01, // bNumInterfaces = 1
        0x01, // bConfigurationValue = 1
        0x00, // iConfiguration
        0xE0, // bmAttributes (bus-powered, self-powered, remote-wakeup)
        0x00, // bMaxPower = 0 (self-powered)
        // Interface Descriptor (9 bytes)
        0x09, // bLength
        0x04, // bDescriptorType = Interface
        0x00, // bInterfaceNumber = 0
        0x00, // bAlternateSetting = 0
        0x01, // bNumEndpoints = 1
        0x09, // bInterfaceClass = Hub
        0x00, // bInterfaceSubClass
        0x00, // bInterfaceProtocol (full/low speed hub)
        0x00, // iInterface
        // Endpoint Descriptor (7 bytes)
        0x07, // bLength
        0x05, // bDescriptorType = Endpoint
        0x81, // bEndpointAddress = IN endpoint 1
        0x03, // bmAttributes = Interrupt
        0x01, 0x00, // wMaxPacketSize = 1
        0x0C, // bInterval = 12 ms
    ];

    // -----------------------------------------------------------------------
    // parse_config_tree correctness tests
    // -----------------------------------------------------------------------

    #[test]
    fn parse_boot_keyboard_class_fields() {
        let cfg = parse_config_tree(BOOT_KEYBOARD_CONFIG_BLOB).expect("keyboard blob must parse");
        assert_eq!(cfg.interfaces.len(), 1);
        let iface = &cfg.interfaces[0].interface;
        assert_eq!(iface.b_interface_class, CLASS_HID);
        assert_eq!(iface.b_interface_sub_class, SUBCLASS_HID_BOOT);
        assert_eq!(iface.b_interface_protocol, PROTOCOL_HID_KEYBOARD);
    }

    #[test]
    fn parse_boot_keyboard_endpoint() {
        let cfg = parse_config_tree(BOOT_KEYBOARD_CONFIG_BLOB).expect("keyboard blob must parse");
        let ep_list = &cfg.interfaces[0].endpoints;
        assert_eq!(ep_list.len(), 1);
        let ep = &ep_list[0];
        // IN endpoint 1.
        assert_eq!(ep.b_endpoint_address, 0x81);
        assert!(ep.is_in());
        assert_eq!(ep.endpoint_number(), 1);
        // Interrupt transfer type.
        assert_eq!(ep.transfer_type(), TRANSFER_TYPE_INTERRUPT);
        assert_eq!(ep.bm_attributes, 0x03);
        assert_eq!(ep.w_max_packet_size, 8);
        assert_eq!(ep.b_interval, 10);
    }

    #[test]
    fn parse_boot_keyboard_hid_descriptor() {
        let cfg = parse_config_tree(BOOT_KEYBOARD_CONFIG_BLOB).expect("keyboard blob must parse");
        let hid = cfg.interfaces[0]
            .hid
            .as_ref()
            .expect("HID descriptor must be present");
        assert_eq!(hid.b_descriptor_type, DESC_TYPE_HID);
        assert_eq!(hid.bcd_hid, 0x0111);
        assert_eq!(hid.w_descriptor_length, 63);
    }

    #[test]
    fn parse_boot_mouse_class_fields() {
        let cfg = parse_config_tree(BOOT_MOUSE_CONFIG_BLOB).expect("mouse blob must parse");
        assert_eq!(cfg.interfaces.len(), 1);
        let iface = &cfg.interfaces[0].interface;
        assert_eq!(iface.b_interface_class, CLASS_HID);
        assert_eq!(iface.b_interface_sub_class, SUBCLASS_HID_BOOT);
        assert_eq!(iface.b_interface_protocol, PROTOCOL_HID_MOUSE);
    }

    #[test]
    fn parse_boot_mouse_endpoint() {
        let cfg = parse_config_tree(BOOT_MOUSE_CONFIG_BLOB).expect("mouse blob must parse");
        let ep = &cfg.interfaces[0].endpoints[0];
        assert_eq!(ep.b_endpoint_address, 0x81);
        assert!(ep.is_in());
        assert_eq!(ep.transfer_type(), TRANSFER_TYPE_INTERRUPT);
        assert_eq!(ep.w_max_packet_size, 4);
        assert_eq!(ep.b_interval, 10);
    }

    #[test]
    fn parse_hub_class_and_endpoint() {
        let cfg = parse_config_tree(HUB_CONFIG_BLOB).expect("hub blob must parse");
        assert_eq!(cfg.interfaces.len(), 1);
        let iface = &cfg.interfaces[0].interface;
        assert_eq!(iface.b_interface_class, CLASS_HUB);
        assert!(cfg.interfaces[0].hid.is_none());
        let ep = &cfg.interfaces[0].endpoints[0];
        assert_eq!(ep.b_endpoint_address, 0x81);
        assert_eq!(ep.transfer_type(), TRANSFER_TYPE_INTERRUPT);
        assert_eq!(ep.w_max_packet_size, 1);
    }

    #[test]
    fn config_descriptor_w_total_length() {
        let keyboard_cfg = parse_config_tree(BOOT_KEYBOARD_CONFIG_BLOB).unwrap();
        assert_eq!(keyboard_cfg.config.w_total_length, 34);
        let mouse_cfg = parse_config_tree(BOOT_MOUSE_CONFIG_BLOB).unwrap();
        assert_eq!(mouse_cfg.config.w_total_length, 34);
        let hub_cfg = parse_config_tree(HUB_CONFIG_BLOB).unwrap();
        assert_eq!(hub_cfg.config.w_total_length, 25);
    }

    #[test]
    fn parse_returns_none_for_empty_slice() {
        assert!(parse_config_tree(&[]).is_none());
    }

    #[test]
    fn parse_returns_none_for_wrong_type() {
        // Wrong bDescriptorType (0x01 = Device, not Configuration).
        let bad = &mut BOOT_KEYBOARD_CONFIG_BLOB.to_vec();
        bad[1] = 0x01;
        assert!(parse_config_tree(bad).is_none());
    }

    #[test]
    fn device_descriptor_parse() {
        // A minimal 18-byte Device Descriptor.
        let raw: &[u8] = &[
            0x12, // bLength
            0x01, // bDescriptorType = Device
            0x00, 0x02, // bcdUSB = 0x0200
            0x00, // bDeviceClass
            0x00, // bDeviceSubClass
            0x00, // bDeviceProtocol
            0x40, // bMaxPacketSize0 = 64
            0x6D, 0x04, // idVendor = 0x046D (Logitech)
            0x01, 0xC5, // idProduct
            0x72, 0x01, // bcdDevice
            0x01, // iManufacturer
            0x02, // iProduct
            0x00, // iSerialNumber
            0x01, // bNumConfigurations
        ];
        let dev = DeviceDescriptor::parse(raw).expect("must parse");
        assert_eq!(dev.bcd_usb, 0x0200);
        assert_eq!(dev.b_max_packet_size0, 64);
        assert_eq!(dev.id_vendor, 0x046D);
        assert_eq!(dev.b_num_configurations, 1);
    }

    #[test]
    fn endpoint_descriptor_direction_helpers() {
        // IN endpoint.
        let in_ep = EndpointDescriptor {
            b_endpoint_address: 0x82, // EP 2 IN
            bm_attributes: TRANSFER_TYPE_INTERRUPT,
            ..Default::default()
        };
        assert!(in_ep.is_in());
        assert_eq!(in_ep.endpoint_number(), 2);

        // OUT endpoint.
        let out_ep = EndpointDescriptor {
            b_endpoint_address: 0x01, // EP 1 OUT
            bm_attributes: 0x02,      // Bulk
            ..Default::default()
        };
        assert!(!out_ep.is_in());
        assert_eq!(out_ep.endpoint_number(), 1);
    }
}
