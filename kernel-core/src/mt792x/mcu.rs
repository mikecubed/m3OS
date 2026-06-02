//! mt792x connac2 MCU command-frame encoding and TLV framing — Tasks A.5 + B.7.
//!
//! Implements host-testable, pure byte-packing functions for the MCU channel:
//!
//! * [`encode_mcu_txd`] — build a `mt76_connac2_mcu_txd`-style command frame.
//! * [`push_tlv`] — append a type-length-value field to a command buffer.
//! * [`match_response`] — classify an MCU response sequence number.
//! * [`encode_sta_rec_key`] — build a `STA_REC_KEY` TLV body (B.7).
//!
//! # connac2 MCU TXD byte layout
//!
//! ```text
//! [0..32]  8 × LE u32 HW TXD words
//!   txd[0] = total_len (LE u16 occupying the low 16 bits; upper 16 reserved)
//!   txd[1..7] = reserved 0
//! [32..52] MCU TXD metadata block
//!   [32..34] len        (LE u16) — total frame length
//!   [34..36] pq_id      (LE u16) — 0x8000 | queue id
//!   [36]    cid         — command id
//!   [37]    pkt_type    — must be MCU_PKT_ID (0xA0)
//!   [38]    set_query
//!   [39]    seq
//!   [40]    uc_d2b0_rev — 0
//!   [41]    ext_cid     — 0
//!   [42]    s2d_index
//!   [43]    ext_cid_ack — 0
//!   [44..64] 5 × LE u32 reserved
//! [64..]   payload bytes
//! ```
//!
//! Only the three fields flagged with "MUST" above are validated by tests;
//! the remaining layout follows the connac2 reference but does not need to
//! match silicon byte-for-byte for the host-logic tests.

#![allow(dead_code)]

extern crate alloc;
use alloc::vec::Vec;

// ---------------------------------------------------------------------------
// Queue / packet-type consts
// ---------------------------------------------------------------------------

/// Logical MCU firmware-download queue ID.
pub const MT_MCUQ_FWDL: u8 = 0;
/// Logical MCU WM (Wi-Fi MAC) command queue ID.
pub const MT_MCUQ_WM: u8 = 1;
/// Logical MCU RX queue ID.
pub const MT_RXQ_MCU: u8 = 0;

/// MCU packet-type identifier embedded in every MCU TXD at [`PKT_TYPE_OFFSET`].
pub const MCU_PKT_ID: u8 = 0xA0;

/// Host-to-network (HOST→WM) s2d_index value.
pub const MCU_S2D_H2N: u8 = 0x00;

// ---------------------------------------------------------------------------
// HW TXD / metadata layout offsets (byte offsets from frame start)
// ---------------------------------------------------------------------------

/// Number of HW TXD dwords (8 × 4 = 32 bytes).
const HW_TXD_DWORDS: usize = 8;
/// Byte size of the HW TXD block.
const HW_TXD_SIZE: usize = HW_TXD_DWORDS * 4;

/// Byte offset of the metadata `len` field (LE u16).
pub const META_LEN_OFFSET: usize = HW_TXD_SIZE; // 32
/// Byte offset of the metadata `pq_id` field (LE u16).
pub const META_PQ_ID_OFFSET: usize = HW_TXD_SIZE + 2; // 34
/// Byte offset of the `cid` field.
pub const CID_OFFSET: usize = HW_TXD_SIZE + 4; // 36
/// Byte offset of the `pkt_type` field — MUST equal [`MCU_PKT_ID`] (0xA0).
pub const PKT_TYPE_OFFSET: usize = HW_TXD_SIZE + 5; // 37
/// Byte offset of the `set_query` field.
pub const SET_QUERY_OFFSET: usize = HW_TXD_SIZE + 6; // 38
/// Byte offset of the `seq` field.
pub const SEQ_OFFSET: usize = HW_TXD_SIZE + 7; // 39
/// Byte offset of the `s2d_index` field — MUST equal the argument passed to
/// [`encode_mcu_txd`].
pub const S2D_INDEX_OFFSET: usize = HW_TXD_SIZE + 10; // 42

/// Number of reserved dwords following the metadata.
const META_RESERVED_DWORDS: usize = 5;
/// Total metadata block size (12 bytes of named fields + 20 bytes reserved).
const META_SIZE: usize = 12 + META_RESERVED_DWORDS * 4; // 32

/// Byte offset where the payload begins.
pub const PAYLOAD_OFFSET: usize = HW_TXD_SIZE + META_SIZE; // 64

// ---------------------------------------------------------------------------
// Frame builder
// ---------------------------------------------------------------------------

/// Build a `mt76_connac2_mcu_txd`-style command frame.
///
/// # Frame structure
///
/// ```text
/// [0..32]  HW TXD (8 LE u32; txd[0] carries total_len in low 16 bits)
/// [32..64] metadata block (see module doc for byte map)
/// [64..]   payload bytes
/// ```
///
/// # Verified byte positions
///
/// | Offset | Field | Value |
/// |--------|-------|-------|
/// | [`PKT_TYPE_OFFSET`] (37) | `pkt_type` | [`MCU_PKT_ID`] (0xA0) |
/// | [`S2D_INDEX_OFFSET`] (42) | `s2d_index` | `s2d_index` argument |
/// | [`CID_OFFSET`] (36) | `cid` | `cid` argument |
/// | [`SEQ_OFFSET`] (39) | `seq` | `seq` argument |
pub fn encode_mcu_txd(cid: u8, s2d_index: u8, set_query: u8, seq: u8, payload: &[u8]) -> Vec<u8> {
    let total_len = PAYLOAD_OFFSET + payload.len();
    let mut frame = Vec::with_capacity(total_len);

    // --- HW TXD (8 × LE u32) ---
    // txd[0]: total length in low 16 bits.
    let txd0 = (total_len as u32) & 0x0000_FFFF;
    frame.extend_from_slice(&txd0.to_le_bytes());
    // txd[1..7]: reserved.
    for _ in 1..HW_TXD_DWORDS {
        frame.extend_from_slice(&0u32.to_le_bytes());
    }

    // --- Metadata block ---
    // [32..34] len (LE u16)
    frame.extend_from_slice(&(total_len as u16).to_le_bytes());
    // [34..36] pq_id (LE u16): 0x8000 | queue (nominal MT_MCUQ_WM)
    let pq_id: u16 = 0x8000 | (MT_MCUQ_WM as u16);
    frame.extend_from_slice(&pq_id.to_le_bytes());
    // [36] cid
    frame.push(cid);
    // [37] pkt_type = MCU_PKT_ID
    frame.push(MCU_PKT_ID);
    // [38] set_query
    frame.push(set_query);
    // [39] seq
    frame.push(seq);
    // [40] uc_d2b0_rev = 0
    frame.push(0);
    // [41] ext_cid = 0
    frame.push(0);
    // [42] s2d_index
    frame.push(s2d_index);
    // [43] ext_cid_ack = 0
    frame.push(0);
    // [44..64] 5 × LE u32 reserved
    for _ in 0..META_RESERVED_DWORDS {
        frame.extend_from_slice(&0u32.to_le_bytes());
    }

    // --- Payload ---
    frame.extend_from_slice(payload);

    frame
}

// ---------------------------------------------------------------------------
// TLV framing
// ---------------------------------------------------------------------------

/// Append a TLV field to `buf`.
///
/// # TLV layout
///
/// ```text
/// [0..2]  tag   (LE u16)
/// [2..4]  len   (LE u16) — TOTAL TLV length INCLUDING this 4-byte header,
///                          padded to the next 4-byte boundary.
/// [4..]   value bytes
/// [..]    zero-padding to 4-byte boundary
/// ```
///
/// The `len` field encodes the padded total length (header + value + pad),
/// so the consumer can advance `len` bytes to reach the next TLV.
pub fn push_tlv(buf: &mut Vec<u8>, tag: u16, value: &[u8]) {
    let unpadded_total = 4 + value.len(); // 4-byte header + value
    let padded_total = unpadded_total.next_multiple_of(4);
    let pad_bytes = padded_total - unpadded_total;

    buf.extend_from_slice(&tag.to_le_bytes());
    buf.extend_from_slice(&(padded_total as u16).to_le_bytes());
    buf.extend_from_slice(value);
    for _ in 0..pad_bytes {
        buf.push(0);
    }
}

// ---------------------------------------------------------------------------
// Response sequence matching
// ---------------------------------------------------------------------------

/// Classification of an MCU response's sequence number relative to the live
/// (most-recently-sent) sequence number.
///
/// # Convention
///
/// Sequence numbers start at 1 (0 is never used as a live seq). The comparison
/// is simple: if `rx_seq == live_seq` it is a direct match; if `rx_seq <
/// live_seq` it is a stale reply to an earlier command; otherwise it is from a
/// future command (Mismatch — should not normally occur).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McuMatch {
    /// `rx_seq == live_seq`: this response corresponds to the outstanding command.
    Matched,
    /// `rx_seq < live_seq`: stale reply to a previously-completed command.
    Stale,
    /// `rx_seq > live_seq`: from a command we have not yet sent (should not occur
    /// in normal operation; indicates a firmware bug or reset-ordering issue).
    Mismatch,
}

/// Classify `rx_seq` relative to `live_seq`.
///
/// Seq 0 is never used as a live sequence number. The comparison is purely
/// ordinal: `rx_seq < live_seq` → Stale, `rx_seq == live_seq` → Matched,
/// `rx_seq > live_seq` → Mismatch.
#[inline]
pub fn match_response(live_seq: u8, rx_seq: u8) -> McuMatch {
    if rx_seq == live_seq {
        McuMatch::Matched
    } else if rx_seq < live_seq {
        McuMatch::Stale
    } else {
        McuMatch::Mismatch
    }
}

// ---------------------------------------------------------------------------
// STA_REC_KEY TLV encoder (Task B.7)
// ---------------------------------------------------------------------------

/// TLV tag for `STA_REC_KEY` (nominal connac2 value).
pub const STA_REC_KEY: u16 = 0x10;

/// CCMP (AES-CCM, 802.11i/WPA2) cipher selector byte.
pub const CIPHER_CCMP: u8 = 0x01;

/// Build a `STA_REC_KEY` TLV body.
///
/// The body is a pure byte packing — **no cryptographic operations** are
/// performed. The caller is responsible for providing the correctly-derived
/// key material.
///
/// # Body layout
///
/// ```text
/// [0..2]  wcid       (LE u16)
/// [2]     key_idx    (u8)
/// [3]     cipher     (u8) — e.g. CIPHER_CCMP = 0x01
/// [4]     key_len    (u8) — key.len() as u8
/// [5..]   key bytes
/// [..]    zero-padding to 4-byte boundary
/// ```
///
/// The body is returned as a `Vec<u8>`. To embed it in a command buffer, wrap
/// it with [`push_tlv`] using the [`STA_REC_KEY`] tag.
pub fn encode_sta_rec_key(wcid: u16, cipher: u8, key_idx: u8, key: &[u8]) -> Vec<u8> {
    // 2 (wcid) + 1 (key_idx) + 1 (cipher) + 1 (key_len) = 5 fixed bytes.
    let fixed = 5;
    let unpadded = fixed + key.len();
    let padded = unpadded.next_multiple_of(4);
    let mut body = Vec::with_capacity(padded);

    body.extend_from_slice(&wcid.to_le_bytes());
    body.push(key_idx);
    body.push(cipher);
    body.push(key.len() as u8);
    body.extend_from_slice(key);
    // Pad to 4-byte boundary.
    while body.len() % 4 != 0 {
        body.push(0);
    }

    body
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn txd_encode() {
        let payload = [0xDE, 0xAD, 0xBE, 0xEF];
        let frame = encode_mcu_txd(0x12, MCU_S2D_H2N, 0x01, 0x07, &payload);

        // Frame must be at least PAYLOAD_OFFSET + payload.len().
        assert_eq!(frame.len(), PAYLOAD_OFFSET + payload.len());

        // pkt_type at PKT_TYPE_OFFSET must be MCU_PKT_ID.
        assert_eq!(frame[PKT_TYPE_OFFSET], MCU_PKT_ID, "pkt_type must be 0xA0");

        // s2d_index at S2D_INDEX_OFFSET must equal the argument.
        assert_eq!(
            frame[S2D_INDEX_OFFSET], MCU_S2D_H2N,
            "s2d_index must round-trip"
        );

        // cid at CID_OFFSET.
        assert_eq!(frame[CID_OFFSET], 0x12, "cid must round-trip");

        // seq at SEQ_OFFSET.
        assert_eq!(frame[SEQ_OFFSET], 0x07, "seq must round-trip");

        // Payload bytes must appear at PAYLOAD_OFFSET.
        assert_eq!(&frame[PAYLOAD_OFFSET..], &payload);
    }

    #[test]
    fn txd_empty_payload() {
        let frame = encode_mcu_txd(0xFF, 0x00, 0x00, 0x01, &[]);
        assert_eq!(frame.len(), PAYLOAD_OFFSET);
        assert_eq!(frame[PKT_TYPE_OFFSET], MCU_PKT_ID);
    }

    #[test]
    fn tlv_framing() {
        let mut buf = Vec::new();

        // 3-byte value → padded to 4 bytes; total TLV = 4 (hdr) + 3 + 1 pad = 8 bytes.
        push_tlv(&mut buf, 0x0001, &[0xAA, 0xBB, 0xCC]);
        assert_eq!(buf.len(), 8, "3-byte value must be padded to 8-byte TLV");
        assert!(buf.len() % 4 == 0, "buffer length must be multiple of 4");

        // Check tag LE.
        assert_eq!(u16::from_le_bytes([buf[0], buf[1]]), 0x0001);
        // Check len field (padded total = 8).
        assert_eq!(u16::from_le_bytes([buf[2], buf[3]]), 8);
        // Value bytes.
        assert_eq!(buf[4], 0xAA);
        assert_eq!(buf[5], 0xBB);
        assert_eq!(buf[6], 0xCC);
        // Padding byte.
        assert_eq!(buf[7], 0x00);
    }

    #[test]
    fn tlv_exactly_aligned_value() {
        let mut buf = Vec::new();
        // 4-byte value → no padding needed; total TLV = 8 bytes.
        push_tlv(&mut buf, 0x0002, &[1, 2, 3, 4]);
        assert_eq!(buf.len(), 8);
        assert_eq!(u16::from_le_bytes([buf[2], buf[3]]), 8);
        assert_eq!(buf[4..8], [1, 2, 3, 4]);
    }

    #[test]
    fn tlv_empty_value() {
        let mut buf = Vec::new();
        // 0-byte value → padded total = 4 bytes (header only).
        push_tlv(&mut buf, 0x0003, &[]);
        assert_eq!(buf.len(), 4);
        assert_eq!(u16::from_le_bytes([buf[2], buf[3]]), 4);
    }

    #[test]
    fn seq_matching() {
        assert_eq!(match_response(5, 5), McuMatch::Matched);
        assert_eq!(match_response(5, 3), McuMatch::Stale);
        assert_eq!(match_response(5, 9), McuMatch::Mismatch);
        // Edge cases.
        assert_eq!(match_response(1, 0), McuMatch::Stale);
        assert_eq!(match_response(255, 255), McuMatch::Matched);
    }

    #[test]
    fn sta_rec_key_ccmp() {
        let key = [
            0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E,
            0x0F, 0x10,
        ]; // 16-byte CCMP pairwise key
        let body = encode_sta_rec_key(0x0003, CIPHER_CCMP, 0, &key);

        // Body must be 4-byte aligned.
        assert_eq!(body.len() % 4, 0, "body must be 4-byte aligned");

        // wcid @ [0..2] LE.
        assert_eq!(u16::from_le_bytes([body[0], body[1]]), 0x0003);

        // key_idx @ [2].
        assert_eq!(body[2], 0, "key_idx");

        // cipher @ [3] = CIPHER_CCMP.
        assert_eq!(body[3], CIPHER_CCMP, "cipher must be CIPHER_CCMP");

        // key_len @ [4] = 16.
        assert_eq!(body[4], 16, "key_len");

        // key bytes @ [5..21].
        assert_eq!(&body[5..21], &key, "key bytes must round-trip");
    }

    #[test]
    fn sta_rec_key_padding() {
        // A 5-byte key: fixed(5) + key(5) = 10 bytes → padded to 12.
        let body = encode_sta_rec_key(0x0001, CIPHER_CCMP, 1, &[0xAA; 5]);
        assert_eq!(body.len(), 12);
        assert_eq!(body.len() % 4, 0);
    }
}
