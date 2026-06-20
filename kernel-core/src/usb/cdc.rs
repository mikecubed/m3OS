//! USB CDC (Communications Device Class) — functional descriptor parsing and
//! NCM Transfer Block (NTB) framing (CDC spec §5.2.3, NCM spec §3.2).
//!
//! # CDC Functional Descriptors
//!
//! Within a configuration blob, CDC class-specific (functional) descriptors use
//! `bDescriptorType = 0x24` (`CS_INTERFACE`) and carry a `bDescriptorSubtype`
//! in the third byte that identifies which functional descriptor it is. The host
//! calls [`find_ethernet_functional_desc`] to extract the MAC address string
//! index and maximum segment size from an Ethernet Networking functional
//! descriptor (subtype `0x0F`), and [`has_ncm_functional_desc`] to test whether
//! the device advertises NCM capability (subtype `0x1A`).
//!
//! # CDC-NCM NTB-16 Framing (NCM spec §3.2)
//!
//! NCM (Network Control Model) aggregates one or more Ethernet datagrams into a
//! single USB bulk transfer called an NCM Transfer Block (NTB). The 16-bit
//! variant (NTB-16) uses 16-bit offsets and lengths throughout:
//!
//! * [`NTH16`] — the fixed 12-byte NTB transfer header at offset 0; carries the
//!   block length and the offset of the first NDP16.
//! * [`NDP16`] — the datagram pointer table; carries `(wDatagramIndex,
//!   wDatagramLength)` pairs terminated by a `(0, 0)` sentinel.
//!
//! Use [`build_ntb16`] to serialise an NTB from a slice of datagram payloads,
//! and [`parse_ntb16`] to deserialise one.

extern crate alloc;

use alloc::vec::Vec;

// ---------------------------------------------------------------------------
// CDC class constants (USB CDC spec §5.2.3)
// ---------------------------------------------------------------------------

/// `bDescriptorType` for a CDC class-specific (functional) descriptor.
///
/// All CDC functional descriptors embedded in a configuration blob carry this
/// type code in their second byte (offset 1).
pub const CS_INTERFACE: u8 = 0x24;

/// `bDescriptorSubtype` for the CDC Header functional descriptor (§5.2.3.1).
pub const CDC_SUBTYPE_HEADER: u8 = 0x00;

/// `bDescriptorSubtype` for the CDC Union functional descriptor (§5.2.3.8).
pub const CDC_SUBTYPE_UNION: u8 = 0x06;

/// `bDescriptorSubtype` for the Ethernet Networking functional descriptor
/// (CDC §5.4 / ECM spec Table 3).
pub const CDC_SUBTYPE_ETHERNET: u8 = 0x0F;

/// `bDescriptorSubtype` for the NCM (Network Control Model) functional
/// descriptor (NCM spec §5.2.1).
pub const CDC_SUBTYPE_NCM: u8 = 0x1A;

// ---------------------------------------------------------------------------
// USB-Ethernet device-match registry (Phase 92e Track G.3)
//
// A USB-Ethernet `AttachNotice` is routed to exactly one driver: the
// vendor-native `ure` (Realtek RTL815x register map) when the VID:PID matches a
// known vendor part, or the class-compliant CDC driver (this crate) for a
// standard CDC-ECM/NCM interface. Folding the two behind one registry is the
// Phase 92e G.3 deliverable — a USB-Ethernet device selects its driver from one
// table instead of two unrelated binaries racing to claim it.
// ---------------------------------------------------------------------------

/// USB vendor ID for Realtek USB peripherals (`0x0BDA`).
///
/// Distinct from the *PCI* Realtek vendor ID (`0x10EC`, see
/// [`crate::nic_ids::VENDOR_REALTEK`]) — the USB-Ethernet dongles enumerate
/// under the USB-IF vendor ID.
pub const USB_VENDOR_REALTEK: u16 = 0x0BDA;

/// Realtek USB-Ethernet product IDs served by the vendor-native `ure` driver
/// (the RTL815x USB family). The `ure` binary itself is not yet on `main` (it
/// lives on the Phase 96 `docs/96-bare-metal-usb-ethernet` branch); this table
/// is the routing verdict the shared registry returns for it.
pub const URE_PRODUCT_IDS: &[u16] = &[
    0x8156, // RTL8156  — 2.5GbE
    0x8157, // RTL8156B — 2.5GbE
    0x8152, // RTL8152  — USB 2.0 10/100
    0x8153, // RTL8153  — USB 3.0 1GbE
];

/// `bInterfaceClass` for the CDC Communications interface — the control
/// interface that carries the CDC functional descriptors and the notification
/// interrupt-IN endpoint (USB CDC spec Table 15).
pub const CDC_COMM_CLASS: u8 = 0x02;

/// `bInterfaceClass` for the CDC Data interface — carries the bulk IN/OUT pair
/// over which Ethernet frames (ECM) or NTBs (NCM) move (USB CDC spec Table 18).
pub const CDC_DATA_CLASS: u8 = 0x0A;

/// `bInterfaceSubClass` of the CDC Communications interface for the Ethernet
/// Networking Control Model (ECM, CDC spec Table 16).
pub const CDC_SUBCLASS_ECM: u8 = 0x06;

/// `bInterfaceSubClass` of the CDC Communications interface for the Network
/// Control Model (NCM, NCM spec §3.1).
pub const CDC_SUBCLASS_NCM: u8 = 0x0D;

/// Vendor-native USB-Ethernet driver families — drivers with a device-specific
/// register map rather than the class-compliant CDC datapath.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsbNetVendor {
    /// Realtek RTL815x USB-Ethernet (the Phase 96 `ure` driver). The binary is
    /// not on `main` yet, so this verdict is *routed-to* (logged by the
    /// `usb-net` daemon) but the actual hand-off lands with the Phase 96 merge.
    Realtek,
}

/// Which USB-Ethernet driver the shared registry selects for a device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsbNetDriver {
    /// A vendor-native driver, matched by VID:PID (the native datapath; the
    /// class-compliant interface on the same composite device is ignored).
    Vendor(UsbNetVendor),
    /// The class-compliant CDC driver. The ECM-vs-NCM variant is refined from
    /// the configuration blob with [`refine_cdc_variant`].
    Cdc,
}

/// The CDC data-model variant, refined from the configuration blob once a
/// device has been classified as [`UsbNetDriver::Cdc`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CdcVariant {
    /// Ethernet Control Model — one Ethernet frame per bulk transfer.
    Ecm,
    /// Network Control Model — multiple frames aggregated per NTB
    /// ([`build_ntb16`] / [`parse_ntb16`]).
    Ncm,
}

/// Select the driver for a USB-Ethernet device from the identifying fields a
/// class driver already has on its `AttachNotice` — no `GetDescriptors`
/// round-trip required.
///
/// Resolution order (vendor-native beats class-compliant: a composite RTL8156
/// exposes BOTH a vendor-specific interface and a CDC/RNDIS one, but only the
/// vendor datapath actually carries traffic, so the vendor match wins):
///
/// 1. **VID:PID** — a Realtek (`0x0BDA`) device whose product ID is in
///    [`URE_PRODUCT_IDS`] routes to [`UsbNetVendor::Realtek`] (`ure`).
/// 2. **Interface class** — a CDC Communications interface (`0x02`, subclass
///    ECM `0x06` or NCM `0x0D`) or a CDC Data interface (`0x0A`) routes to
///    [`UsbNetDriver::Cdc`].
///
/// Returns `None` for a device that is neither — the `usb-net` daemon ignores
/// it (it is not a USB-Ethernet device, or uses an unsupported model such as
/// RNDIS, which is deferred).
pub fn match_usb_net_driver(
    vendor_id: u16,
    product_id: u16,
    interface_class: u8,
    interface_sub_class: u8,
) -> Option<UsbNetDriver> {
    // 1. Vendor-native match takes priority over the class match so a known
    //    composite dongle is driven by its native datapath.
    if vendor_id == USB_VENDOR_REALTEK && URE_PRODUCT_IDS.contains(&product_id) {
        return Some(UsbNetDriver::Vendor(UsbNetVendor::Realtek));
    }

    // 2. Class-compliant CDC. A CDC Communications interface is only a
    //    USB-Ethernet device for the ECM/NCM subclasses (ACM `0x02` is a serial
    //    modem, not Ethernet); a CDC Data interface (`0x0A`) is unambiguous.
    match interface_class {
        CDC_COMM_CLASS
            if interface_sub_class == CDC_SUBCLASS_ECM
                || interface_sub_class == CDC_SUBCLASS_NCM =>
        {
            Some(UsbNetDriver::Cdc)
        }
        CDC_DATA_CLASS => Some(UsbNetDriver::Cdc),
        _ => None,
    }
}

/// Refine a [`UsbNetDriver::Cdc`] device into its [`CdcVariant`] from the full
/// configuration blob: [`CdcVariant::Ncm`] if it advertises an NCM functional
/// descriptor, otherwise [`CdcVariant::Ecm`].
pub fn refine_cdc_variant(config: &[u8]) -> CdcVariant {
    if has_ncm_functional_desc(config) {
        CdcVariant::Ncm
    } else {
        CdcVariant::Ecm
    }
}

/// Build the `GET_DESCRIPTOR(String)` SETUP packet that reads the MAC-address
/// string descriptor a CDC-ECM device points to via its
/// [`EthernetFunctionalDesc::mac_string_index`].
///
/// The reply is a UTF-16LE string descriptor carrying the MAC as 12 ASCII hex
/// characters; pass it to [`parse_ecm_mac`].
///
/// Wire layout (8 bytes, USB 2.0 §9.4.3):
///
/// | Offset | Value | Field |
/// |--------|-------|-------|
/// | 0 | `0x80` | bmRequestType: IN \| Standard \| Device |
/// | 1 | `0x06` | bRequest: GET_DESCRIPTOR |
/// | 2 | `index` | wValue lo: string descriptor index |
/// | 3 | `0x03` | wValue hi: STRING descriptor type |
/// | 4–5 | `0x0409` | wIndex: English (US) language ID |
/// | 6–7 | `0x00FF` | wLength: 255 (max string descriptor) |
pub const fn get_string_descriptor_setup(index: u8) -> [u8; 8] {
    [
        0x80,  // bmRequestType: device-to-host, standard, device
        0x06,  // bRequest: GET_DESCRIPTOR
        index, // wValue lo: string index
        0x03,  // wValue hi: STRING descriptor type
        0x09, 0x04, // wIndex: langid 0x0409 (English-US)
        0xFF, 0x00, // wLength: 255
    ]
}

/// Parse a 6-byte MAC address out of a USB String descriptor whose payload is
/// the 12 ASCII hex characters CDC-ECM uses for `iMACAddress` (ECM spec §5.4).
///
/// The descriptor is `[bLength, bDescriptorType=0x03, <UTF-16LE chars…>]`; each
/// of the 12 hex characters is one UTF-16 code unit (ASCII byte + `0x00`).
///
/// Returns `None` if the descriptor is too short, is not a String descriptor,
/// contains a non-ASCII code unit, or contains a non-hex character.
pub fn parse_ecm_mac(string_desc: &[u8]) -> Option<[u8; 6]> {
    // 2-byte header + 12 UTF-16LE code units (24 bytes) for the hex MAC.
    const STRING_DESC_TYPE: u8 = 0x03;
    if string_desc.len() < 2 + 24 || string_desc[1] != STRING_DESC_TYPE {
        return None;
    }
    let mut nibbles = [0u8; 12];
    for (i, nib) in nibbles.iter_mut().enumerate() {
        let lo = string_desc[2 + i * 2];
        let hi = string_desc[2 + i * 2 + 1];
        // The MAC string is pure ASCII hex, so the UTF-16 high byte is always 0.
        if hi != 0 {
            return None;
        }
        *nib = match lo {
            b'0'..=b'9' => lo - b'0',
            b'a'..=b'f' => lo - b'a' + 10,
            b'A'..=b'F' => lo - b'A' + 10,
            _ => return None,
        };
    }
    let mut mac = [0u8; 6];
    for (i, octet) in mac.iter_mut().enumerate() {
        *octet = (nibbles[i * 2] << 4) | nibbles[i * 2 + 1];
    }
    Some(mac)
}

// ---------------------------------------------------------------------------
// NCM NTB-16 header signatures (NCM spec §3.2)
// ---------------------------------------------------------------------------

/// NTH16 `dwSignature`: ASCII "NCMH" in little-endian (NCM spec §3.2.1).
///
/// `0x484D434E` == `b'N' | b'C'<<8 | b'M'<<16 | b'H'<<24`.
pub const NTH16_SIGNATURE: u32 = 0x484D_434E;

/// NDP16 `dwSignature`: ASCII "NCM0" in little-endian (NCM spec §3.2.2).
///
/// `0x304D434E` == `b'N' | b'C'<<8 | b'M'<<16 | b'0'<<24`.
pub const NDP16_SIGNATURE: u32 = 0x304D_434E;

/// Fixed byte length of the NTH16 header (NCM spec §3.2.1 Table 3-1).
pub const NTH16_LEN: u16 = 12;

// ---------------------------------------------------------------------------
// Ethernet Networking functional descriptor
// ---------------------------------------------------------------------------

/// Parsed Ethernet Networking functional descriptor (CDC ECM spec Table 3).
///
/// Wire layout (offset from start of the descriptor):
///
/// | Offset | Size | Field |
/// |--------|------|-------|
/// | 0 | 1 | `bLength` |
/// | 1 | 1 | `bDescriptorType` (= [`CS_INTERFACE`] = 0x24) |
/// | 2 | 1 | `bDescriptorSubtype` (= [`CDC_SUBTYPE_ETHERNET`] = 0x0F) |
/// | 3 | 1 | `iMACAddress` — string descriptor index of the MAC address |
/// | 4 | 4 | `bmEthernetStatistics` — bitmask of supported statistics (LE) |
/// | 8 | 2 | `wMaxSegmentSize` — maximum Ethernet segment size in bytes (LE) |
/// | 10 | 2 | `wNumberMCFilters` — number of multicast filters (LE) |
/// | 12 | 1 | `bNumberPowerFilters` — number of power-on pattern filters |
///
/// The minimum descriptor length to extract the fields this struct captures
/// (through `wMaxSegmentSize` at offset 8) is 10 bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EthernetFunctionalDesc {
    /// `iMACAddress` — string descriptor index carrying the MAC address in
    /// ASCII-encoded hex (12 characters, e.g. `"0A1B2C3D4E5F"`).
    pub mac_string_index: u8,
    /// `wMaxSegmentSize` — maximum Ethernet segment size the device supports,
    /// in bytes. Typically 1514 (1500-byte payload + 14-byte Ethernet header).
    pub max_segment_size: u16,
}

/// Minimum byte length of the Ethernet Networking functional descriptor needed
/// to extract [`EthernetFunctionalDesc`] fields (through `wMaxSegmentSize`).
const ETHERNET_FUNC_DESC_MIN_LEN: usize = 10;

/// Walk a USB configuration blob and return the first Ethernet Networking
/// functional descriptor found (CS_INTERFACE / subtype 0x0F).
///
/// # Arguments
///
/// * `config` — the raw full-configuration blob starting with the
///   Configuration Descriptor. The caller must have already read the complete
///   `wTotalLength` bytes from the device.
///
/// # Returns
///
/// `Some(EthernetFunctionalDesc)` if a valid Ethernet Networking functional
/// descriptor is present in the blob; `None` if absent, truncated, or if
/// `bLength` would read out-of-bounds.
pub fn find_ethernet_functional_desc(config: &[u8]) -> Option<EthernetFunctionalDesc> {
    let mut pos = 0usize;
    while pos + 2 <= config.len() {
        let b_length = config[pos] as usize;
        let b_type = config[pos + 1];

        // Guard against infinite loops on a zero-length descriptor.
        if b_length == 0 {
            break;
        }

        let end = pos + b_length;
        if end > config.len() {
            // Truncated descriptor — stop walking.
            break;
        }

        // A CS_INTERFACE descriptor needs at least a subtype byte (offset 2).
        if b_type == CS_INTERFACE && b_length >= 3 {
            let subtype = config[pos + 2];
            if subtype == CDC_SUBTYPE_ETHERNET {
                // Require at least ETHERNET_FUNC_DESC_MIN_LEN bytes so we can
                // read iMACAddress (offset 3) and wMaxSegmentSize (offset 8).
                if b_length < ETHERNET_FUNC_DESC_MIN_LEN || end > config.len() {
                    return None;
                }
                let mac_string_index = config[pos + 3];
                // bytes 4..8 are bmEthernetStatistics — skip.
                let max_segment_size = u16::from_le_bytes([config[pos + 8], config[pos + 9]]);
                return Some(EthernetFunctionalDesc {
                    mac_string_index,
                    max_segment_size,
                });
            }
        }

        pos += b_length;
    }
    None
}

/// Return `true` if the configuration blob contains a CDC NCM functional
/// descriptor (CS_INTERFACE / subtype [`CDC_SUBTYPE_NCM`] = 0x1A).
///
/// A device that advertises this descriptor supports the NCM data model and
/// NTB framing handled by [`build_ntb16`] / [`parse_ntb16`].
pub fn has_ncm_functional_desc(config: &[u8]) -> bool {
    let mut pos = 0usize;
    while pos + 2 <= config.len() {
        let b_length = config[pos] as usize;
        let b_type = config[pos + 1];

        if b_length == 0 {
            break;
        }

        let end = pos + b_length;
        if end > config.len() {
            break;
        }

        if b_type == CS_INTERFACE && b_length >= 3 {
            let subtype = config[pos + 2];
            if subtype == CDC_SUBTYPE_NCM {
                return true;
            }
        }

        pos += b_length;
    }
    false
}

// ---------------------------------------------------------------------------
// NTB-16 header + datagram pointer structures
// ---------------------------------------------------------------------------

/// NTB Transfer Header for the 16-bit variant (NTH16, NCM spec §3.2.1).
///
/// # Wire layout (12 bytes, all little-endian)
///
/// | Offset | Size | Field |
/// |--------|------|-------|
/// | 0 | 4 | `dwSignature` = [`NTH16_SIGNATURE`] ("NCMH" LE) |
/// | 4 | 2 | `wHeaderLength` = 12 |
/// | 6 | 2 | `wSequence` — rolling sequence counter |
/// | 8 | 2 | `wBlockLength` — total byte length of the NTB |
/// | 10 | 2 | `wNdpIndex` — byte offset of the first NDP16 from start of NTB |
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NTH16 {
    /// `dwSignature` — must equal [`NTH16_SIGNATURE`].
    pub dw_signature: u32,
    /// `wHeaderLength` — must equal [`NTH16_LEN`] (12).
    pub w_header_length: u16,
    /// `wSequence` — rolling transfer sequence number, wraps at 0xFFFF.
    pub w_sequence: u16,
    /// `wBlockLength` — total byte count of this NTB, including all headers,
    /// datagram payloads, and the NDP16.
    pub w_block_length: u16,
    /// `wNdpIndex` — byte offset from the start of the NTB to the first NDP16.
    pub w_ndp_index: u16,
}

/// NDP (Datagram Pointer) for the 16-bit variant (NDP16, NCM spec §3.2.2).
///
/// Follows the datagram payloads in the NTB. Contains an array of
/// `(wDatagramIndex, wDatagramLength)` pairs, terminated by a `(0, 0)` entry.
///
/// # Wire layout (variable, all little-endian)
///
/// | Offset | Size | Field |
/// |--------|------|-------|
/// | 0 | 4 | `dwSignature` = [`NDP16_SIGNATURE`] ("NCM0" LE) |
/// | 4 | 2 | `wLength` — byte length of this NDP16 (header + all pointer pairs) |
/// | 6 | 2 | `wNextNdpIndex` — offset of the next NDP16, or 0 if this is last |
/// | 8 | n×4 | Array of `(wDatagramIndex u16 LE, wDatagramLength u16 LE)` pairs |
///
/// The pair array is terminated by a `(0, 0)` entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NDP16 {
    /// `dwSignature` — must equal [`NDP16_SIGNATURE`].
    pub dw_signature: u32,
    /// `wLength` — total byte length of this NDP16 (8 header bytes + 4 bytes
    /// per pointer pair, including the `(0,0)` terminator).
    pub w_length: u16,
    /// `wNextNdpIndex` — byte offset to the next NDP16 in the NTB, or `0` if
    /// this is the last (and typically only) NDP16.
    pub w_next_ndp_index: u16,
}

// ---------------------------------------------------------------------------
// NTB-16 build + parse
// ---------------------------------------------------------------------------

/// Serialise an NTB-16 carrying the given datagrams into a `Vec<u8>`.
///
/// Returns `None` if the resulting NTB would not fit the 16-bit NTB-16 framing —
/// i.e. its total `wBlockLength` (and therefore any datagram index/length or
/// `wNdpIndex`) would exceed [`u16::MAX`]. Without this guard the `usize → u16`
/// casts below would silently wrap and emit a malformed NTB.
///
/// # Layout produced
///
/// ```text
/// [NTH16 — 12 bytes]
/// [datagram 0 payload]
/// [datagram 1 payload]
/// …
/// [NDP16 header — 8 bytes]
/// [pointer pair 0: (index_0 u16 LE, len_0 u16 LE)]
/// [pointer pair 1: (index_1 u16 LE, len_1 u16 LE)]
/// …
/// [terminator: (0x0000, 0x0000)]
/// ```
///
/// # Arguments
///
/// * `seq` — the `wSequence` value to embed in the NTH16 header.
/// * `datagrams` — the Ethernet frames to aggregate. An empty slice produces
///   an NTB with a single NDP16 containing only the `(0,0)` terminator.
pub fn build_ntb16(seq: u16, datagrams: &[&[u8]]) -> Option<Vec<u8>> {
    // --- Compute layout offsets ----------------------------------------
    //
    // NTH16 occupies bytes 0..12.
    // Datagram payloads are placed contiguously starting at offset 12.
    // NDP16 follows immediately after all payloads.
    //
    // NDP16 size: 8 bytes (fixed header) + 4 bytes per datagram + 4 bytes
    // for the (0,0) terminator.

    let nth_len = NTH16_LEN as usize; // 12
    let ndp_header_len: usize = 8;
    // Each datagram contributes one (index, length) pair (4 bytes) plus the
    // mandatory (0,0) terminator at the end.
    let ndp_pointer_bytes: usize = (datagrams.len() + 1) * 4;
    let ndp_total_len: usize = ndp_header_len + ndp_pointer_bytes;

    // Total payload bytes.
    let payload_bytes: usize = datagrams.iter().map(|d| d.len()).sum();

    // NDP16 starts right after the payloads.
    let ndp_offset: usize = nth_len + payload_bytes;

    // Total NTB size.
    let block_length: usize = ndp_offset + ndp_total_len;

    // NTB-16 encodes wBlockLength, wNdpIndex, every datagram (index, length)
    // pair, and the NDP16 wLength as u16. `block_length` is the largest of these
    // quantities, so once it fits in a u16 every `as u16` cast below is exact;
    // otherwise the frame would silently wrap into a malformed NTB — refuse it.
    if block_length > u16::MAX as usize {
        return None;
    }

    // --- Serialise ------------------------------------------------------
    let mut out = Vec::with_capacity(block_length);

    // NTH16
    out.extend_from_slice(&NTH16_SIGNATURE.to_le_bytes()); // dwSignature
    out.extend_from_slice(&NTH16_LEN.to_le_bytes()); //        wHeaderLength
    out.extend_from_slice(&seq.to_le_bytes()); //              wSequence
    out.extend_from_slice(&(block_length as u16).to_le_bytes()); // wBlockLength
    out.extend_from_slice(&(ndp_offset as u16).to_le_bytes()); //  wNdpIndex

    // Datagram payloads; record the byte offset of each.
    let mut datagram_offsets: Vec<u16> = Vec::with_capacity(datagrams.len());
    for dgram in datagrams {
        datagram_offsets.push(out.len() as u16);
        out.extend_from_slice(dgram);
    }

    // NDP16 header
    out.extend_from_slice(&NDP16_SIGNATURE.to_le_bytes()); // dwSignature
    out.extend_from_slice(&(ndp_total_len as u16).to_le_bytes()); // wLength
    out.extend_from_slice(&0u16.to_le_bytes()); //                   wNextNdpIndex = 0

    // Datagram pointer pairs.
    for (i, dgram) in datagrams.iter().enumerate() {
        out.extend_from_slice(&datagram_offsets[i].to_le_bytes()); // wDatagramIndex
        out.extend_from_slice(&(dgram.len() as u16).to_le_bytes()); // wDatagramLength
    }
    // Terminating (0, 0) pair.
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());

    debug_assert_eq!(out.len(), block_length, "NTB-16 output length mismatch");
    Some(out)
}

/// Parse an NTB-16 and return the contained Ethernet datagrams.
///
/// Validates the NTH16 signature, locates the first NDP16 via `wNdpIndex`,
/// validates the NDP16 signature, and slices each datagram from the NTB
/// buffer according to the `(wDatagramIndex, wDatagramLength)` pointer pairs.
///
/// # Returns
///
/// `Some(Vec<Vec<u8>>)` on success, where each inner `Vec<u8>` is one Ethernet
/// datagram in the order they appear in the NDP16. Returns `None` if:
///
/// * The buffer is shorter than [`NTH16_LEN`].
/// * The `dwSignature` in the NTH16 is wrong.
/// * `wNdpIndex` points outside the buffer, or the NDP16 at that offset is
///   truncated.
/// * The `dwSignature` in the NDP16 is wrong.
/// * Any `(wDatagramIndex, wDatagramLength)` pair points outside the buffer.
///
/// # Note on chained NDPs
///
/// This implementation follows `wNdpIndex` in the NTH16 to locate the **first**
/// NDP16 and stops there; it does not chain through `wNextNdpIndex` in the NDP16
/// header. Full NDP chaining is deferred — the NCM spec permits multiple NDP16s
/// in a single NTB, but real-world CDC-ECM/NCM devices almost universally emit
/// exactly one NDP16 per NTB.
pub fn parse_ntb16(buf: &[u8]) -> Option<Vec<Vec<u8>>> {
    // --- Validate NTH16 ------------------------------------------------
    if buf.len() < NTH16_LEN as usize {
        return None;
    }

    let signature = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
    if signature != NTH16_SIGNATURE {
        return None;
    }

    // wNdpIndex: byte offset from start of NTB to NDP16 (offset 10).
    let ndp_index = u16::from_le_bytes([buf[10], buf[11]]) as usize;

    // --- Validate NDP16 header -----------------------------------------
    // NDP16 fixed header is 8 bytes.
    const NDP16_HEADER_LEN: usize = 8;
    if ndp_index + NDP16_HEADER_LEN > buf.len() {
        return None;
    }

    let ndp_sig = u32::from_le_bytes([
        buf[ndp_index],
        buf[ndp_index + 1],
        buf[ndp_index + 2],
        buf[ndp_index + 3],
    ]);
    if ndp_sig != NDP16_SIGNATURE {
        return None;
    }

    let ndp_length = u16::from_le_bytes([buf[ndp_index + 4], buf[ndp_index + 5]]) as usize;
    // wLength must be at least the fixed NDP16 header + one (0,0) terminator.
    if ndp_length < NDP16_HEADER_LEN + 4 {
        return None;
    }
    if ndp_index + ndp_length > buf.len() {
        return None;
    }

    // --- Walk pointer pairs --------------------------------------------
    // Pairs begin at offset 8 within the NDP16 (after the 8-byte header).
    let mut datagrams = Vec::new();
    let mut pair_pos = ndp_index + NDP16_HEADER_LEN;
    let ndp_end = ndp_index + ndp_length;

    while pair_pos + 4 <= ndp_end {
        let dgram_index = u16::from_le_bytes([buf[pair_pos], buf[pair_pos + 1]]) as usize;
        let dgram_len = u16::from_le_bytes([buf[pair_pos + 2], buf[pair_pos + 3]]) as usize;
        pair_pos += 4;

        // (0, 0) is the terminator.
        if dgram_index == 0 && dgram_len == 0 {
            break;
        }

        // Bounds-check: the datagram must lie entirely within the NTB buffer.
        let dgram_end = dgram_index.checked_add(dgram_len)?;
        if dgram_end > buf.len() {
            return None;
        }

        datagrams.push(buf[dgram_index..dgram_end].to_vec());
    }

    Some(datagrams)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Hand-crafted CDC configuration blobs
    //
    // These are synthetic fragments containing exactly the descriptors under
    // test. They deliberately omit fields not required to exercise the parser.
    // -----------------------------------------------------------------------

    /// A configuration blob fragment that contains:
    ///   1. A Configuration Descriptor (9 bytes, bDescriptorType = 0x02).
    ///   2. An Interface Descriptor (9 bytes, bDescriptorType = 0x04).
    ///   3. An Ethernet Networking functional descriptor (13 bytes,
    ///      bDescriptorType = 0x24 / bDescriptorSubtype = 0x0F).
    ///
    /// Ethernet functional descriptor byte layout:
    ///   offset 0 bLength = 13
    ///   offset 1 bDescriptorType = 0x24 (CS_INTERFACE)
    ///   offset 2 bDescriptorSubtype = 0x0F (Ethernet)
    ///   offset 3 iMACAddress = 0x05
    ///   offset 4..8 bmEthernetStatistics = 0x00000000
    ///   offset 8..10 wMaxSegmentSize = 0x05EA (1514 LE)
    ///   offset 10..12 wNumberMCFilters = 0x0000
    ///   offset 12 bNumberPowerFilters = 0x00
    const CDC_ECM_CONFIG_BLOB: &[u8] = &[
        // Configuration Descriptor (9 bytes)
        0x09, // bLength
        0x02, // bDescriptorType = Configuration
        0x27, 0x00, // wTotalLength = 39
        0x01, // bNumInterfaces = 1
        0x01, // bConfigurationValue = 1
        0x00, // iConfiguration
        0xC0, // bmAttributes
        0x00, // bMaxPower
        // Interface Descriptor (9 bytes)
        0x09, // bLength
        0x04, // bDescriptorType = Interface
        0x00, // bInterfaceNumber = 0
        0x00, // bAlternateSetting = 0
        0x01, // bNumEndpoints = 1
        0x02, // bInterfaceClass = CDC
        0x06, // bInterfaceSubClass = Ethernet Networking
        0x00, // bInterfaceProtocol
        0x00, // iInterface
        // Ethernet Networking functional descriptor (13 bytes)
        0x0D, // bLength = 13
        0x24, // bDescriptorType = CS_INTERFACE
        0x0F, // bDescriptorSubtype = Ethernet Networking
        0x05, // iMACAddress string index = 5
        0x00, 0x00, 0x00, 0x00, // bmEthernetStatistics
        0xEA, 0x05, // wMaxSegmentSize = 0x05EA = 1514 LE
        0x00, 0x00, // wNumberMCFilters
        0x00, // bNumberPowerFilters
    ];

    /// A configuration blob fragment containing both an Ethernet functional
    /// descriptor (0x0F) AND an NCM functional descriptor (0x1A).
    ///
    /// NCM functional descriptor (6 bytes):
    ///   offset 0 bLength = 6
    ///   offset 1 bDescriptorType = 0x24 (CS_INTERFACE)
    ///   offset 2 bDescriptorSubtype = 0x1A (NCM)
    ///   offset 3..5 bcdNcmVersion = 0x0100
    ///   offset 5 bmNetworkCapabilities = 0x00
    const CDC_NCM_CONFIG_BLOB: &[u8] = &[
        // Configuration Descriptor (9 bytes)
        0x09, // bLength
        0x02, // bDescriptorType = Configuration
        0x2D, 0x00, // wTotalLength = 45
        0x01, // bNumInterfaces = 1
        0x01, // bConfigurationValue = 1
        0x00, // iConfiguration
        0xC0, // bmAttributes
        0x00, // bMaxPower
        // Interface Descriptor (9 bytes)
        0x09, // bLength
        0x04, // bDescriptorType = Interface
        0x00, 0x00, 0x01, 0x02, 0x0D, 0x00, 0x00,
        // Ethernet Networking functional descriptor (13 bytes)
        0x0D, // bLength = 13
        0x24, // CS_INTERFACE
        0x0F, // Ethernet Networking
        0x07, // iMACAddress = 7
        0x00, 0x00, 0x00, 0x00, // bmEthernetStatistics
        0xEA, 0x05, // wMaxSegmentSize = 1514
        0x00, 0x00, // wNumberMCFilters
        0x00, // bNumberPowerFilters
        // NCM functional descriptor (6 bytes)
        0x06, // bLength = 6
        0x24, // CS_INTERFACE
        0x1A, // NCM
        0x00, 0x01, // bcdNcmVersion = 1.00
        0x00, // bmNetworkCapabilities
    ];

    // -----------------------------------------------------------------------
    // find_ethernet_functional_desc tests
    // -----------------------------------------------------------------------

    #[test]
    fn find_ethernet_functional_desc_extracts_mac_index_and_mss() {
        let desc = find_ethernet_functional_desc(CDC_ECM_CONFIG_BLOB)
            .expect("must find Ethernet functional descriptor");
        assert_eq!(
            desc.mac_string_index, 5,
            "iMACAddress string index must be 5"
        );
        assert_eq!(
            desc.max_segment_size, 1514,
            "wMaxSegmentSize must be 1514 (0x05EA)"
        );
    }

    #[test]
    fn find_ethernet_functional_desc_returns_none_when_absent() {
        // A blob with only a Configuration + Interface descriptor — no CDC
        // functional descriptor.
        let no_cdc_blob: &[u8] = &[
            0x09, 0x02, 0x12, 0x00, 0x01, 0x01, 0x00, 0xC0, 0x00, // Config
            0x09, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // Interface
        ];
        assert!(
            find_ethernet_functional_desc(no_cdc_blob).is_none(),
            "must return None when no Ethernet functional descriptor present"
        );
    }

    #[test]
    fn find_ethernet_functional_desc_returns_none_for_empty_slice() {
        assert!(find_ethernet_functional_desc(&[]).is_none());
    }

    #[test]
    fn find_ethernet_functional_desc_from_ncm_blob() {
        // The NCM blob also contains an Ethernet functional descriptor.
        let desc = find_ethernet_functional_desc(CDC_NCM_CONFIG_BLOB)
            .expect("NCM blob must also contain an Ethernet functional descriptor");
        assert_eq!(desc.mac_string_index, 7);
        assert_eq!(desc.max_segment_size, 1514);
    }

    #[test]
    fn find_ethernet_functional_desc_truncated_returns_none() {
        // Clip the blob so the Ethernet functional descriptor header is present
        // but the wMaxSegmentSize field at offset 8 is missing.
        // Configuration (9) + Interface (9) = 18 bytes before the functional
        // descriptor; add 8 bytes (through bmEthernetStatistics) but not the
        // wMaxSegmentSize bytes → total 26, missing byte 9 (offset 26 missing).
        let truncated = &CDC_ECM_CONFIG_BLOB[..26];
        // The Ethernet func descriptor starts at byte 18; bLength=13 → end=31.
        // The slice is 26, so end (31) > slice.len() (26) → None.
        assert!(
            find_ethernet_functional_desc(truncated).is_none(),
            "must return None for truncated Ethernet functional descriptor"
        );
    }

    // -----------------------------------------------------------------------
    // has_ncm_functional_desc tests
    // -----------------------------------------------------------------------

    #[test]
    fn has_ncm_functional_desc_true_when_present() {
        assert!(
            has_ncm_functional_desc(CDC_NCM_CONFIG_BLOB),
            "must return true when NCM functional descriptor is present"
        );
    }

    #[test]
    fn has_ncm_functional_desc_false_for_ecm_only_blob() {
        assert!(
            !has_ncm_functional_desc(CDC_ECM_CONFIG_BLOB),
            "must return false when no NCM functional descriptor is present"
        );
    }

    #[test]
    fn has_ncm_functional_desc_false_for_empty_slice() {
        assert!(!has_ncm_functional_desc(&[]));
    }

    // -----------------------------------------------------------------------
    // build_ntb16 / parse_ntb16 round-trip tests
    // -----------------------------------------------------------------------

    /// Two distinct datagrams used across NTB-16 tests.
    const DGRAM_A: &[u8] = &[0x01, 0x02, 0x03, 0x04, 0x05];
    const DGRAM_B: &[u8] = &[0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF];

    #[test]
    fn ntb16_round_trip_two_datagrams() {
        let ntb = build_ntb16(7, &[DGRAM_A, DGRAM_B]).expect("test NTB fits u16");
        let result = parse_ntb16(&ntb).expect("parse_ntb16 must succeed on a well-formed NTB");
        assert_eq!(result.len(), 2, "must recover exactly 2 datagrams");
        assert_eq!(result[0], DGRAM_A, "first datagram must match DGRAM_A");
        assert_eq!(result[1], DGRAM_B, "second datagram must match DGRAM_B");
    }

    #[test]
    fn ntb16_nth16_signature_present_in_built_bytes() {
        let ntb = build_ntb16(7, &[DGRAM_A, DGRAM_B]).expect("test NTB fits u16");
        // NTH16 dwSignature at bytes 0..4.
        let sig = u32::from_le_bytes([ntb[0], ntb[1], ntb[2], ntb[3]]);
        assert_eq!(sig, NTH16_SIGNATURE, "NTH16 dwSignature must be 0x484D434E");
        // Verify the ASCII encoding: "NCMH".
        assert_eq!(&ntb[0..4], b"NCMH", "signature bytes must spell 'NCMH'");
    }

    #[test]
    fn ntb16_ndp16_signature_present_in_built_bytes() {
        let ntb = build_ntb16(7, &[DGRAM_A, DGRAM_B]).expect("test NTB fits u16");
        // wNdpIndex at bytes 10..12.
        let ndp_offset = u16::from_le_bytes([ntb[10], ntb[11]]) as usize;
        let sig = u32::from_le_bytes([
            ntb[ndp_offset],
            ntb[ndp_offset + 1],
            ntb[ndp_offset + 2],
            ntb[ndp_offset + 3],
        ]);
        assert_eq!(sig, NDP16_SIGNATURE, "NDP16 dwSignature must be 0x304D434E");
        assert_eq!(
            &ntb[ndp_offset..ndp_offset + 4],
            b"NCM0",
            "NDP16 signature bytes must spell 'NCM0'"
        );
    }

    #[test]
    fn ntb16_sequence_number_encoded_correctly() {
        let ntb = build_ntb16(42, &[DGRAM_A]).expect("test NTB fits u16");
        let wseq = u16::from_le_bytes([ntb[6], ntb[7]]);
        assert_eq!(
            wseq, 42,
            "wSequence must reflect the supplied sequence number"
        );
    }

    #[test]
    fn ntb16_block_length_matches_buffer_length() {
        let ntb = build_ntb16(7, &[DGRAM_A, DGRAM_B]).expect("test NTB fits u16");
        let w_block_length = u16::from_le_bytes([ntb[8], ntb[9]]) as usize;
        assert_eq!(
            w_block_length,
            ntb.len(),
            "wBlockLength must equal the total NTB byte count"
        );
    }

    #[test]
    fn ntb16_header_length_is_twelve() {
        let ntb = build_ntb16(0, &[DGRAM_A]).expect("test NTB fits u16");
        let w_header_length = u16::from_le_bytes([ntb[4], ntb[5]]);
        assert_eq!(w_header_length, 12, "wHeaderLength must be 12");
    }

    #[test]
    fn ntb16_round_trip_single_datagram() {
        let dgram: &[u8] = &[0xDE, 0xAD, 0xBE, 0xEF];
        let ntb = build_ntb16(0, &[dgram]).expect("test NTB fits u16");
        let result = parse_ntb16(&ntb).expect("must parse");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], dgram);
    }

    #[test]
    fn ntb16_round_trip_empty_datagram_list() {
        let ntb = build_ntb16(0, &[]).expect("test NTB fits u16");
        let result = parse_ntb16(&ntb).expect("empty NTB must parse without error");
        assert!(result.is_empty(), "no datagrams in an empty NTB");
    }

    #[test]
    fn ntb16_oversized_block_returns_none() {
        // A datagram large enough that the total wBlockLength would exceed
        // u16::MAX must be refused, not silently truncated into a malformed NTB.
        let big = alloc::vec![0u8; 70_000];
        assert!(
            build_ntb16(0, &[big.as_slice()]).is_none(),
            "an NTB whose wBlockLength exceeds u16::MAX must yield None"
        );
    }

    #[test]
    fn ntb16_maximal_block_builds_and_round_trips() {
        // The largest NTB that still fits 16-bit framing must build and parse:
        // wBlockLength = 12 (NTH16) + payload + 8 (NDP16 hdr) + 4 (one pair)
        //              + 4 (terminator) == u16::MAX.
        let payload = u16::MAX as usize - 12 - 8 - 4 - 4;
        let big = alloc::vec![0xABu8; payload];
        let ntb = build_ntb16(0, &[big.as_slice()]).expect("a u16::MAX-sized NTB must build");
        assert_eq!(
            ntb.len(),
            u16::MAX as usize,
            "the maximal NTB must fill exactly wBlockLength"
        );
        let result = parse_ntb16(&ntb).expect("the maximal NTB must round-trip");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].len(), payload);
    }

    // -----------------------------------------------------------------------
    // parse_ntb16 malformed-input tests (must return None, never panic)
    // -----------------------------------------------------------------------

    #[test]
    fn parse_ntb16_bad_signature_returns_none() {
        let mut ntb = build_ntb16(1, &[DGRAM_A]).expect("test NTB fits u16");
        // Corrupt the NTH16 signature.
        ntb[0] = 0xFF;
        assert!(
            parse_ntb16(&ntb).is_none(),
            "bad NTH16 signature must yield None"
        );
    }

    #[test]
    fn parse_ntb16_truncated_returns_none() {
        // A slice shorter than the NTH16 header (12 bytes).
        let short: &[u8] = &[0x4E, 0x43, 0x4D, 0x48, 0x0C, 0x00, 0x00, 0x00];
        assert!(
            parse_ntb16(short).is_none(),
            "buffer shorter than NTH16 must yield None"
        );
    }

    #[test]
    fn parse_ntb16_ndp_out_of_range_returns_none() {
        let mut ntb = build_ntb16(1, &[DGRAM_A]).expect("test NTB fits u16");
        // Set wNdpIndex to a value beyond the buffer.
        let bad_offset = (ntb.len() as u16 + 100).to_le_bytes();
        ntb[10] = bad_offset[0];
        ntb[11] = bad_offset[1];
        assert!(
            parse_ntb16(&ntb).is_none(),
            "wNdpIndex pointing outside buffer must yield None"
        );
    }

    #[test]
    fn parse_ntb16_bad_ndp_signature_returns_none() {
        let mut ntb = build_ntb16(1, &[DGRAM_A]).expect("test NTB fits u16");
        // Locate and corrupt the NDP16 signature.
        let ndp_offset = u16::from_le_bytes([ntb[10], ntb[11]]) as usize;
        ntb[ndp_offset] = 0xFF;
        assert!(
            parse_ntb16(&ntb).is_none(),
            "bad NDP16 signature must yield None"
        );
    }

    #[test]
    fn parse_ntb16_datagram_index_out_of_range_returns_none() {
        let mut ntb = build_ntb16(1, &[DGRAM_A]).expect("test NTB fits u16");
        // Locate the first datagram pointer pair in the NDP16.
        // NDP16 starts at ntb[wNdpIndex]; pointer pairs start 8 bytes in.
        let ndp_offset = u16::from_le_bytes([ntb[10], ntb[11]]) as usize;
        let pair_offset = ndp_offset + 8; // first (wDatagramIndex, wDatagramLength) pair
        // Set wDatagramIndex to just beyond the buffer.
        let bad_index = (ntb.len() as u16 + 1).to_le_bytes();
        ntb[pair_offset] = bad_index[0];
        ntb[pair_offset + 1] = bad_index[1];
        assert!(
            parse_ntb16(&ntb).is_none(),
            "out-of-range datagram index must yield None"
        );
    }

    #[test]
    fn parse_ntb16_datagram_length_overrun_returns_none() {
        let mut ntb = build_ntb16(1, &[DGRAM_A]).expect("test NTB fits u16");
        // Locate the first datagram pointer pair in the NDP16.
        let ndp_offset = u16::from_le_bytes([ntb[10], ntb[11]]) as usize;
        let pair_offset = ndp_offset + 8;
        // Keep the wDatagramIndex valid but inflate wDatagramLength so that
        // index + length exceeds the buffer.
        let bad_len = (ntb.len() as u16 + 1).to_le_bytes();
        ntb[pair_offset + 2] = bad_len[0];
        ntb[pair_offset + 3] = bad_len[1];
        assert!(
            parse_ntb16(&ntb).is_none(),
            "datagram length overrun must yield None"
        );
    }

    #[test]
    fn parse_ntb16_empty_slice_returns_none() {
        assert!(parse_ntb16(&[]).is_none());
    }

    // -----------------------------------------------------------------------
    // USB-Ethernet device-match registry tests (Phase 92e Track G.3)
    // -----------------------------------------------------------------------

    #[test]
    fn match_realtek_rtl8156_routes_to_ure() {
        // The headline G.3 case: a Realtek 0x0bda:0x8156 routes to the vendor
        // `ure` driver even though its surfaced interface is vendor-specific.
        assert_eq!(
            match_usb_net_driver(USB_VENDOR_REALTEK, 0x8156, 0xFF, 0xFF),
            Some(UsbNetDriver::Vendor(UsbNetVendor::Realtek)),
            "RTL8156 must route to the vendor (ure) driver"
        );
    }

    #[test]
    fn match_realtek_family_pids_all_route_to_ure() {
        for &pid in URE_PRODUCT_IDS {
            assert_eq!(
                match_usb_net_driver(USB_VENDOR_REALTEK, pid, 0xFF, 0x00),
                Some(UsbNetDriver::Vendor(UsbNetVendor::Realtek)),
                "Realtek PID {pid:#06x} must route to ure"
            );
        }
    }

    #[test]
    fn match_realtek_vendor_wins_over_cdc_interface() {
        // A composite RTL8156 exposing a CDC interface (class 0x0a) must still
        // route to the vendor driver — the VID:PID match takes priority.
        assert_eq!(
            match_usb_net_driver(USB_VENDOR_REALTEK, 0x8156, CDC_DATA_CLASS, 0x00),
            Some(UsbNetDriver::Vendor(UsbNetVendor::Realtek)),
            "a known vendor device routes to ure regardless of interface class"
        );
    }

    #[test]
    fn match_cdc_ecm_comm_interface_routes_to_cdc() {
        // A non-vendor CDC Communications interface, ECM subclass, routes to CDC.
        assert_eq!(
            match_usb_net_driver(0x1234, 0x5678, CDC_COMM_CLASS, CDC_SUBCLASS_ECM),
            Some(UsbNetDriver::Cdc),
            "CDC comms / ECM subclass must route to the CDC driver"
        );
    }

    #[test]
    fn match_cdc_ncm_comm_interface_routes_to_cdc() {
        assert_eq!(
            match_usb_net_driver(0x1234, 0x5678, CDC_COMM_CLASS, CDC_SUBCLASS_NCM),
            Some(UsbNetDriver::Cdc),
            "CDC comms / NCM subclass must route to the CDC driver"
        );
    }

    #[test]
    fn match_cdc_data_interface_routes_to_cdc() {
        // The data interface (0x0a) is unambiguously a CDC data path.
        assert_eq!(
            match_usb_net_driver(0x1234, 0x5678, CDC_DATA_CLASS, 0x00),
            Some(UsbNetDriver::Cdc),
            "CDC data interface must route to the CDC driver"
        );
    }

    #[test]
    fn match_realtek_unknown_pid_falls_through_to_class() {
        // A Realtek VID but a PID outside the ure family, presenting a CDC ECM
        // interface, must fall through to the class match (not vendor).
        assert_eq!(
            match_usb_net_driver(USB_VENDOR_REALTEK, 0x9999, CDC_COMM_CLASS, CDC_SUBCLASS_ECM),
            Some(UsbNetDriver::Cdc),
            "an unknown Realtek PID with a CDC interface routes to CDC, not ure"
        );
    }

    #[test]
    fn match_non_ethernet_interfaces_return_none() {
        // CDC ACM (serial modem, subclass 0x02) is not Ethernet.
        assert_eq!(
            match_usb_net_driver(0x1234, 0x5678, CDC_COMM_CLASS, 0x02),
            None,
            "CDC ACM (serial) is not a USB-Ethernet device"
        );
        // HID, mass storage, audio — none are USB-Ethernet.
        assert_eq!(match_usb_net_driver(0x1234, 0x5678, 0x03, 0x01), None);
        assert_eq!(match_usb_net_driver(0x1234, 0x5678, 0x08, 0x06), None);
        assert_eq!(match_usb_net_driver(0x1234, 0x5678, 0x01, 0x01), None);
    }

    #[test]
    fn get_string_descriptor_setup_encodes_index() {
        let s = get_string_descriptor_setup(5);
        assert_eq!(s, [0x80, 0x06, 0x05, 0x03, 0x09, 0x04, 0xFF, 0x00]);
        // wValue hi must be the STRING descriptor type.
        assert_eq!(s[3], 0x03);
        // wLength = 255.
        assert_eq!(u16::from_le_bytes([s[6], s[7]]), 255);
    }

    #[test]
    fn parse_ecm_mac_decodes_hex_string_descriptor() {
        // String descriptor: bLength=26, bType=0x03, then "001A2B3C4D5E" UTF-16LE.
        let mut desc = alloc::vec![26u8, 0x03];
        for ch in b"001A2B3C4D5E" {
            desc.push(*ch);
            desc.push(0x00);
        }
        assert_eq!(
            parse_ecm_mac(&desc),
            Some([0x00, 0x1A, 0x2B, 0x3C, 0x4D, 0x5E]),
            "must decode the 12-hex-char MAC string into 6 octets"
        );
    }

    #[test]
    fn parse_ecm_mac_accepts_lowercase_hex() {
        let mut desc = alloc::vec![26u8, 0x03];
        for ch in b"aabbccddeeff" {
            desc.push(*ch);
            desc.push(0x00);
        }
        assert_eq!(
            parse_ecm_mac(&desc),
            Some([0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF])
        );
    }

    #[test]
    fn parse_ecm_mac_rejects_malformed() {
        // Too short.
        assert!(parse_ecm_mac(&[26u8, 0x03, b'0', 0x00]).is_none());
        // Wrong descriptor type.
        let mut wrong_type = alloc::vec![26u8, 0x02];
        for ch in b"001A2B3C4D5E" {
            wrong_type.push(*ch);
            wrong_type.push(0x00);
        }
        assert!(parse_ecm_mac(&wrong_type).is_none());
        // Non-hex character.
        let mut non_hex = alloc::vec![26u8, 0x03];
        for ch in b"00ZZ2B3C4D5E" {
            non_hex.push(*ch);
            non_hex.push(0x00);
        }
        assert!(parse_ecm_mac(&non_hex).is_none());
        // Non-ASCII UTF-16 code unit (high byte set).
        let mut wide = alloc::vec![26u8, 0x03];
        for ch in b"001A2B3C4D5E" {
            wide.push(*ch);
            wide.push(0x01);
        }
        assert!(parse_ecm_mac(&wide).is_none());
    }

    #[test]
    fn refine_cdc_variant_picks_ncm_then_ecm() {
        assert_eq!(
            refine_cdc_variant(CDC_NCM_CONFIG_BLOB),
            CdcVariant::Ncm,
            "a blob with an NCM functional descriptor refines to NCM"
        );
        assert_eq!(
            refine_cdc_variant(CDC_ECM_CONFIG_BLOB),
            CdcVariant::Ecm,
            "an ECM-only blob refines to ECM"
        );
        // An empty / non-CDC blob defaults to ECM (the simpler datapath).
        assert_eq!(refine_cdc_variant(&[]), CdcVariant::Ecm);
    }
}
