//! Shared NIC descriptor-ring engine + `Descriptor` trait — Phase 79 Track A.0.
//!
//! Extracted from the Phase 55b ring-3 e1000 driver so the Intel families added
//! in Phase 79 (e1000e, igb, igc) share one copy of the ring math — BAL/BAH/LEN
//! splitting, head/tail pre-post, the DD-bit drain contract — instead of four
//! divergent copies. Only the **descriptor encode/decode** differs per family;
//! the control flow does not.
//!
//! The seam is the [`NicDescriptors`] trait:
//!
//! * [`Legacy16`] wraps the 16-byte legacy descriptor (`E1000RxDesc` /
//!   `E1000TxDesc`) used by the classic e1000 and by **e1000e** verbatim.
//! * `Advanced` (added in Track B) implements the same trait over the igb/igc
//!   advanced read/write-back descriptor union.
//!
//! All Intel descriptors here are 16 bytes, so a generic ring can stride by a
//! single constant; the per-family difference is entirely in the bit layout the
//! trait methods encode/decode.

use kernel_core::e1000::{E1000RxDesc, E1000TxDesc, rx_status, tx_cmd, tx_status};

/// Per-family descriptor encode/decode + RX/TX completion semantics.
///
/// The ring engine and the init / hot-path helpers are generic over a type
/// implementing this trait. Implementations are zero-sized marker types
/// (`Legacy16`, `Advanced`) — the trait carries the descriptor *layouts* as
/// associated types and the bit logic as associated functions.
pub trait NicDescriptors {
    /// Receive descriptor wire layout.
    type RxDesc: Copy + Default;
    /// Transmit descriptor wire layout.
    type TxDesc: Copy + Default;

    /// Size in bytes of one RX descriptor (the ring stride).
    const RX_DESC_SIZE: usize;
    /// Size in bytes of one TX descriptor.
    const TX_DESC_SIZE: usize;

    /// True when hardware has written a packet into this RX slot (DD set).
    fn rx_done(desc: &Self::RxDesc) -> bool;
    /// Length in bytes of the received packet (hardware-written).
    fn rx_len(desc: &Self::RxDesc) -> u16;
    /// Build an RX descriptor pointing at `buf_iova`, ready to hand to hardware.
    fn rx_init(buf_iova: u64) -> Self::RxDesc;

    /// True when hardware is done with this TX slot (DD set on a programmed slot).
    fn tx_done(desc: &Self::TxDesc) -> bool;
    /// True when this TX slot is safe to overwrite (never programmed, or DD set).
    fn tx_slot_free(desc: &Self::TxDesc) -> bool;
    /// Build a single-buffer TX descriptor for a `len`-byte packet at `buf_iova`
    /// (EOP + insert-FCS + report-status semantics).
    fn encode_tx(buf_iova: u64, len: u16) -> Self::TxDesc;
}

/// Legacy 16-byte Intel descriptor family — classic e1000 (82540EM) and e1000e.
pub struct Legacy16;

impl NicDescriptors for Legacy16 {
    type RxDesc = E1000RxDesc;
    type TxDesc = E1000TxDesc;
    const RX_DESC_SIZE: usize = 16;
    const TX_DESC_SIZE: usize = 16;

    #[inline]
    fn rx_done(desc: &E1000RxDesc) -> bool {
        desc.status & rx_status::DD != 0
    }
    #[inline]
    fn rx_len(desc: &E1000RxDesc) -> u16 {
        desc.length
    }
    #[inline]
    fn rx_init(buf_iova: u64) -> E1000RxDesc {
        E1000RxDesc {
            buffer_addr: buf_iova,
            ..E1000RxDesc::default()
        }
    }
    #[inline]
    fn tx_done(desc: &E1000TxDesc) -> bool {
        desc.status & tx_status::DD != 0
    }
    #[inline]
    fn tx_slot_free(desc: &E1000TxDesc) -> bool {
        desc.cmd == 0 || (desc.status & tx_status::DD != 0)
    }
    #[inline]
    fn encode_tx(buf_iova: u64, len: u16) -> E1000TxDesc {
        E1000TxDesc {
            buffer_addr: buf_iova,
            length: len,
            cmd: tx_cmd::EOP | tx_cmd::IFCS | tx_cmd::RS,
            ..E1000TxDesc::default()
        }
    }
}

// ---------------------------------------------------------------------------
// Advanced descriptor family — igb / igc (Phase 79 Track B).
// ---------------------------------------------------------------------------
//
// igb (82575/82576/I210/I211/I350/I354) and igc (I225/I226) reject the legacy
// 16-byte descriptor and require the **advanced** read/write-back union modeled
// on Linux `drivers/net/ethernet/intel/igb/e1000_82575.h`
// (`union e1000_adv_rx_desc`, `union e1000_adv_tx_desc`). Both descriptors are
// 16 bytes, so they ride the same generic ring engine as `Legacy16`; only the
// bit layout the trait methods encode/decode differs.
//
// The driver only ever sees two states per descriptor: the **read** format it
// writes before handing the slot to hardware, and the **write-back** format the
// hardware overwrites it with on completion. We model the union as a single
// `#[repr(C)]` struct of two `u64`s and project the read / write-back fields
// onto it with pure bit logic — the same trick Linux's anonymous unions encode.

/// Advanced TX command/type-length (`cmd_type_len`) field bits — Linux
/// `E1000_ADVTXD_*`.
pub mod adv_tx {
    /// Advanced data descriptor type (`DTYP`) — bits 23:20 == 0b0011.
    pub const DTYP_DATA: u32 = 3 << 20;
    /// Descriptor extension — selects the advanced (vs legacy) layout.
    pub const DCMD_DEXT: u32 = 1 << 29;
    /// End-Of-Packet (`EOP`).
    pub const DCMD_EOP: u32 = 1 << 24;
    /// Insert FCS (`IFCS`).
    pub const DCMD_IFCS: u32 = 1 << 25;
    /// Report Status (`RS`) — hardware writes back `DD` on completion.
    pub const DCMD_RS: u32 = 1 << 27;
    /// Low 16 bits hold the per-buffer data length (`DTALEN`).
    pub const DTALEN_MASK: u32 = 0x0000_FFFF;
    /// Shift of the payload length (`PAYLEN`) inside `olinfo_status`.
    pub const OLINFO_PAYLEN_SHIFT: u32 = 14;
}

/// Advanced TX write-back status bits (`status` field, low 32 of upper qword).
pub mod adv_tx_wb {
    /// Descriptor Done — hardware is finished with the slot.
    pub const DD: u32 = 1 << 0;
}

/// Advanced RX write-back status/error bits (`status_error`, low 32 of upper
/// qword) — Linux `E1000_RXD_STAT_*`.
pub mod adv_rx_wb {
    /// Descriptor Done — hardware has written a packet into the slot.
    pub const DD: u32 = 1 << 0;
    /// End-Of-Packet — last descriptor of the received frame.
    pub const EOP: u32 = 1 << 1;
}

/// Advanced RX descriptor union (16 bytes).
///
/// * **read** layout: `pkt_addr` (lower qword) + `hdr_addr` (upper qword).
///   With header-split disabled the driver leaves `hdr_addr == 0` and points
///   `pkt_addr` at the packet buffer.
/// * **write-back** layout: the lower qword holds RSS/packet-type info (ignored
///   here); the upper qword packs `status_error` (low 32) + `length` (bits
///   47:32) + `vlan` (bits 63:48). DD/EOP live in `status_error`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct AdvRxDesc {
    /// Read: packet-buffer IOVA. Write-back: RSS hash / packet-type info.
    pub lo: u64,
    /// Read: header-buffer IOVA (0 when header-split is off). Write-back:
    /// `status_error | (length << 32) | (vlan << 48)`.
    pub hi: u64,
}

impl AdvRxDesc {
    /// `status_error` dword from a write-back descriptor (low 32 of `hi`).
    #[inline]
    pub const fn status_error(&self) -> u32 {
        (self.hi & 0xFFFF_FFFF) as u32
    }
    /// Packet length (bits 47:32 of `hi`).
    #[inline]
    pub const fn length(&self) -> u16 {
        ((self.hi >> 32) & 0xFFFF) as u16
    }
}

/// Advanced TX descriptor union (16 bytes).
///
/// * **read** layout: `buffer_addr` (lower qword) + `cmd_type_len` (low 32 of
///   upper qword) + `olinfo_status` (high 32 of upper qword).
/// * **write-back** layout: the lower qword is reserved; the upper qword's low
///   32 bits hold the completion `status` (DD in bit 0).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct AdvTxDesc {
    /// Read: buffer IOVA. Write-back: reserved.
    pub buffer_addr: u64,
    /// Read: `cmd_type_len` (low 32) + `olinfo_status` (high 32).
    /// Write-back: `status` (low 32; DD in bit 0) + reserved (high 32).
    pub cmd_olinfo: u64,
}

impl AdvTxDesc {
    /// The `cmd_type_len` dword (low 32 of `cmd_olinfo`) as written by the
    /// driver — also where the write-back `status` lands.
    #[inline]
    pub const fn cmd_type_len(&self) -> u32 {
        (self.cmd_olinfo & 0xFFFF_FFFF) as u32
    }
    /// The `olinfo_status` dword (high 32 of `cmd_olinfo`).
    #[inline]
    pub const fn olinfo_status(&self) -> u32 {
        (self.cmd_olinfo >> 32) as u32
    }
    /// Write-back `status` dword — the hardware overwrites the low 32 bits of
    /// `cmd_olinfo` (the slot the driver wrote `cmd_type_len` into) with the
    /// completion status; DD is bit 0.
    #[inline]
    pub const fn wb_status(&self) -> u32 {
        (self.cmd_olinfo & 0xFFFF_FFFF) as u32
    }
}

/// Advanced read/write-back descriptor family — igb / igc.
pub struct Advanced;

impl NicDescriptors for Advanced {
    type RxDesc = AdvRxDesc;
    type TxDesc = AdvTxDesc;
    const RX_DESC_SIZE: usize = 16;
    const TX_DESC_SIZE: usize = 16;

    #[inline]
    fn rx_done(desc: &AdvRxDesc) -> bool {
        desc.status_error() & adv_rx_wb::DD != 0
    }
    #[inline]
    fn rx_len(desc: &AdvRxDesc) -> u16 {
        desc.length()
    }
    #[inline]
    fn rx_init(buf_iova: u64) -> AdvRxDesc {
        // Read format: pkt_addr in the lower qword, hdr_addr (= 0, header-split
        // off) in the upper qword.
        AdvRxDesc {
            lo: buf_iova,
            hi: 0,
        }
    }
    #[inline]
    fn tx_done(desc: &AdvTxDesc) -> bool {
        desc.wb_status() & adv_tx_wb::DD != 0
    }
    #[inline]
    fn tx_slot_free(desc: &AdvTxDesc) -> bool {
        // A never-programmed slot is all-zero (cmd_type_len == 0); a programmed
        // slot is free once hardware writes back DD into the same low dword.
        desc.cmd_type_len() == 0 || (desc.wb_status() & adv_tx_wb::DD != 0)
    }
    #[inline]
    fn encode_tx(buf_iova: u64, len: u16) -> AdvTxDesc {
        AdvTxDesc {
            buffer_addr: buf_iova,
            cmd_olinfo: encode_adv_tx_cmd_olinfo(len),
        }
    }
}

/// Compose the `(olinfo_status << 32) | cmd_type_len` qword for a
/// single-buffer advanced TX descriptor of `len` bytes.
///
/// `cmd_type_len` = DTYP(data) | DEXT | EOP | IFCS | RS | DTALEN(len).
/// `olinfo_status` carries the total payload length in `PAYLEN` (bits 14+),
/// which for a single-descriptor packet equals `len`.
#[inline]
pub const fn encode_adv_tx_cmd_olinfo(len: u16) -> u64 {
    let cmd_type_len = adv_tx::DTYP_DATA
        | adv_tx::DCMD_DEXT
        | adv_tx::DCMD_EOP
        | adv_tx::DCMD_IFCS
        | adv_tx::DCMD_RS
        | ((len as u32) & adv_tx::DTALEN_MASK);
    let olinfo_status = (len as u32) << adv_tx::OLINFO_PAYLEN_SHIFT;
    ((olinfo_status as u64) << 32) | (cmd_type_len as u64)
}

// ---------------------------------------------------------------------------
// Shared ring math — pure helpers reused by every Intel NIC family.
// ---------------------------------------------------------------------------

/// Split a 64-bit IOVA into the `(low32, high32)` pair the `*DBAL` / `*DBAH`
/// registers expect (hardware reads the low half first).
#[inline]
pub const fn split_iova(iova: u64) -> (u32, u32) {
    ((iova & 0xFFFF_FFFF) as u32, (iova >> 32) as u32)
}

/// Initial RX tail (`RDT`) value after pre-posting every descriptor: one short
/// of head, i.e. `ring_size - 1` (Intel: `RDH == RDT` means "ring empty").
#[inline]
pub const fn initial_rdt(ring_size: usize) -> u32 {
    (ring_size as u32).wrapping_sub(1)
}

/// Byte length of a descriptor ring — the `RDLEN` / `TDLEN` register value.
#[inline]
pub const fn ring_len_bytes(ring_size: usize, desc_size: usize) -> usize {
    ring_size * desc_size
}

/// True when a `(ring_size, desc_size)` pairing satisfies Intel's ring
/// constraints: the slot count is a multiple of 8, the byte length is a
/// multiple of 128 (the `RDLEN`/`TDLEN` hardware gate), and the ring fits the
/// 4096-descriptor maximum.
#[inline]
pub const fn ring_len_is_valid(ring_size: usize, desc_size: usize) -> bool {
    ring_size.is_multiple_of(8) && (ring_size * desc_size).is_multiple_of(128) && ring_size <= 4096
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy16_descriptor_sizes_are_16_bytes() {
        assert_eq!(Legacy16::RX_DESC_SIZE, 16);
        assert_eq!(Legacy16::TX_DESC_SIZE, 16);
        assert_eq!(
            core::mem::size_of::<<Legacy16 as NicDescriptors>::RxDesc>(),
            16
        );
        assert_eq!(
            core::mem::size_of::<<Legacy16 as NicDescriptors>::TxDesc>(),
            16
        );
    }

    #[test]
    fn ring_len_gates_match_intel_multiple_of_128() {
        // The canonical 256-slot ring used by the e1000 driver.
        assert!(ring_len_is_valid(256, Legacy16::RX_DESC_SIZE));
        assert_eq!(ring_len_bytes(256, 16), 256 * 16);
        assert!(ring_len_bytes(256, 16).is_multiple_of(128));
        // A non-multiple-of-8 slot count is rejected.
        assert!(!ring_len_is_valid(255, 16));
        // Over the 4096 cap is rejected.
        assert!(!ring_len_is_valid(8192, 16));
    }

    #[test]
    fn split_iova_low_high_match_intel_ordering() {
        let (lo, hi) = split_iova(0xDEAD_BEEF_CAFE_F00D);
        assert_eq!(lo, 0xCAFE_F00D);
        assert_eq!(hi, 0xDEAD_BEEF);
        assert_eq!(((hi as u64) << 32) | (lo as u64), 0xDEAD_BEEF_CAFE_F00D);
    }

    #[test]
    fn initial_rdt_is_ring_size_minus_one() {
        assert_eq!(initial_rdt(256), 255);
        assert_eq!(initial_rdt(8), 7);
    }

    #[test]
    fn legacy16_rx_round_trip() {
        let mut d = Legacy16::rx_init(0x1_0000);
        assert_eq!(d.buffer_addr, 0x1_0000);
        assert!(!Legacy16::rx_done(&d));
        d.status = rx_status::DD;
        d.length = 64;
        assert!(Legacy16::rx_done(&d));
        assert_eq!(Legacy16::rx_len(&d), 64);
    }

    #[test]
    fn legacy16_encode_tx_sets_eop_ifcs_rs() {
        let d = Legacy16::encode_tx(0x2_0000, 100);
        assert_eq!(d.buffer_addr, 0x2_0000);
        assert_eq!(d.length, 100);
        assert_eq!(d.cmd, tx_cmd::EOP | tx_cmd::IFCS | tx_cmd::RS);
        assert_eq!(d.status, 0);
        // A fresh (never-programmed) slot is free; a programmed-but-not-DD slot is not.
        assert!(Legacy16::tx_slot_free(&E1000TxDesc::default()));
        assert!(!Legacy16::tx_slot_free(&d));
    }

    // -- Advanced (igb/igc) descriptor tests --------------------------------

    #[test]
    fn advanced_descriptor_sizes_are_16_bytes() {
        assert_eq!(Advanced::RX_DESC_SIZE, 16);
        assert_eq!(Advanced::TX_DESC_SIZE, 16);
        assert_eq!(
            core::mem::size_of::<<Advanced as NicDescriptors>::RxDesc>(),
            16
        );
        assert_eq!(
            core::mem::size_of::<<Advanced as NicDescriptors>::TxDesc>(),
            16
        );
    }

    #[test]
    fn advanced_rx_init_is_read_format_with_pkt_addr_and_zero_hdr() {
        // Read format: pkt_addr lower qword, hdr_addr (header-split off) upper.
        let d = Advanced::rx_init(0xCAFE_F00D_0000);
        assert_eq!(d.lo, 0xCAFE_F00D_0000);
        assert_eq!(d.hi, 0, "header-split off => hdr_addr == 0");
        // A fresh read descriptor must not look "done".
        assert!(!Advanced::rx_done(&d));
    }

    #[test]
    fn advanced_rx_writeback_decode_status_and_length() {
        // Write-back upper qword: status_error (low 32) | length (47:32) |
        // vlan (63:48). DD|EOP in status_error, length 1514, vlan 0.
        let status_error = adv_rx_wb::DD | adv_rx_wb::EOP;
        let length: u64 = 1514;
        let mut d = AdvRxDesc::default();
        d.hi = (status_error as u64) | (length << 32);
        assert!(Advanced::rx_done(&d));
        assert_eq!(Advanced::rx_len(&d), 1514);
        // DD clear => not done.
        d.hi = (adv_rx_wb::EOP as u64) | (length << 32);
        assert!(!Advanced::rx_done(&d));
    }

    #[test]
    fn advanced_encode_tx_packs_cmd_type_len_and_olinfo() {
        let d = Advanced::encode_tx(0x1234_5678_9000, 100);
        assert_eq!(d.buffer_addr, 0x1234_5678_9000);
        let cmd = d.cmd_type_len();
        let olinfo = d.olinfo_status();
        // DTALEN low 16 bits == frame length.
        assert_eq!(cmd & adv_tx::DTALEN_MASK, 100);
        // DTYP(data) | DEXT | EOP | IFCS | RS all set.
        assert_ne!(cmd & adv_tx::DTYP_DATA, 0);
        assert_ne!(cmd & adv_tx::DCMD_DEXT, 0);
        assert_ne!(cmd & adv_tx::DCMD_EOP, 0);
        assert_ne!(cmd & adv_tx::DCMD_IFCS, 0);
        assert_ne!(cmd & adv_tx::DCMD_RS, 0);
        // PAYLEN in olinfo_status == frame length, shifted by 14.
        assert_eq!(olinfo >> adv_tx::OLINFO_PAYLEN_SHIFT, 100);
    }

    #[test]
    fn advanced_tx_done_and_slot_free_via_writeback_status() {
        // Fresh slot is free (cmd_type_len == 0).
        assert!(Advanced::tx_slot_free(&AdvTxDesc::default()));
        // Programmed but not yet completed: not free, not done.
        let mut d = Advanced::encode_tx(0xABCD_0000, 64);
        assert!(!Advanced::tx_slot_free(&d));
        assert!(!Advanced::tx_done(&d));
        // Hardware writes DD into the low dword of cmd_olinfo on completion.
        d.cmd_olinfo = (d.cmd_olinfo & !0xFFFF_FFFF) | (adv_tx_wb::DD as u64);
        assert!(Advanced::tx_done(&d));
        assert!(Advanced::tx_slot_free(&d));
    }

    #[test]
    fn advanced_dtyp_data_matches_linux_3_in_bits_23_20() {
        // Linux E1000_ADVTXD_DTYP_DATA == 0x3 << 20.
        assert_eq!(adv_tx::DTYP_DATA, 3u32 << 20);
    }

    #[test]
    fn encode_adv_tx_cmd_olinfo_round_trips_through_descriptor() {
        let qword = encode_adv_tx_cmd_olinfo(1500);
        let d = AdvTxDesc {
            buffer_addr: 0,
            cmd_olinfo: qword,
        };
        assert_eq!(d.cmd_type_len() & adv_tx::DTALEN_MASK, 1500);
        assert_eq!(d.olinfo_status() >> adv_tx::OLINFO_PAYLEN_SHIFT, 1500);
    }

    #[test]
    fn advanced_ring_satisfies_intel_ring_gates() {
        // The advanced descriptor is 16 bytes, so the same 256-slot ring math
        // applies — the byte length is still a multiple of 128.
        assert!(ring_len_is_valid(256, Advanced::RX_DESC_SIZE));
        assert!(ring_len_bytes(256, Advanced::TX_DESC_SIZE).is_multiple_of(128));
    }
}
