//! Ethernet frame parsing and construction (P16-T013 through P16-T016).

use alloc::vec::Vec;

use super::virtio_net::MacAddr;

// ===========================================================================
// EtherType constants
// ===========================================================================

pub const ETHERTYPE_ARP: u16 = 0x0806;
pub const ETHERTYPE_IPV4: u16 = 0x0800;

/// Broadcast MAC address.
pub const MAC_BROADCAST: MacAddr = [0xFF; 6];

// ===========================================================================
// EthernetFrame (P16-T013)
// ===========================================================================

/// Parsed Ethernet frame header.
#[derive(Debug, Clone)]
pub struct EthernetFrame {
    pub dst: MacAddr,
    pub src: MacAddr,
    pub ethertype: u16,
    pub payload: Vec<u8>,
}

// ===========================================================================
// Parse / Build (P16-T014, P16-T015)
// ===========================================================================

/// Parse a raw Ethernet frame into header fields and payload.
///
/// Returns `None` if the frame is too short for a valid header (14 bytes).
pub fn parse(raw: &[u8]) -> Option<EthernetFrame> {
    if raw.len() < 14 {
        return None;
    }

    let mut dst = [0u8; 6];
    let mut src = [0u8; 6];
    dst.copy_from_slice(&raw[0..6]);
    src.copy_from_slice(&raw[6..12]);
    let ethertype = u16::from_be_bytes([raw[12], raw[13]]);
    let payload = raw[14..].to_vec();

    Some(EthernetFrame {
        dst,
        src,
        ethertype,
        payload,
    })
}

/// Construct a raw Ethernet frame from header fields and payload.
pub fn build(dst: MacAddr, src: MacAddr, ethertype: u16, payload: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(14 + payload.len());
    frame.extend_from_slice(&dst);
    frame.extend_from_slice(&src);
    frame.extend_from_slice(&ethertype.to_be_bytes());
    frame.extend_from_slice(payload);
    frame
}
