//! Realtek r8169 / r8125 pure-logic support (Phase 79 Track C/D).
//!
//! This module is `no_std` + host-testable. It captures every piece of the
//! Realtek bring-up that does not touch hardware directly:
//!
//! * MMIO register offsets and descriptor bit layout (Track C.1),
//! * the C+ descriptor-ring builder (Track C.1),
//! * the XID -> `MacVersion` chip-versioning table (Track C.2),
//! * the per-version soft-reset poll predicate (Track C.2),
//! * the 32-bit "V2" interrupt-block register offsets for the 8125 (Track D.1),
//! * the `rtl_nic` firmware-header validator and load-path result (Track D.1).
//!
//! The hardware-touching bring-up (claim, BAR map, register writes, IRQ loop)
//! lives in the `r8169` / `r8125` driver binaries and consumes the constants
//! and builders defined here so the bit-level logic stays host-tested.

#![allow(dead_code)]

// ---------------------------------------------------------------------------
// MMIO register offsets (per Linux drivers/net/ethernet/realtek/r8169_main.c).
// ---------------------------------------------------------------------------

/// MAC address, 6 bytes (read/write under Cfg9346 unlock).
pub const REG_MAC0: u32 = 0x00;
/// Multicast address register, 8 bytes.
pub const REG_MAR0: u32 = 0x08;
/// TX descriptor ring base, low 32 bits.
pub const REG_TX_DESC_START_ADDR_LOW: u32 = 0x20;
/// TX descriptor ring base, high 32 bits.
pub const REG_TX_DESC_START_ADDR_HIGH: u32 = 0x24;
/// TX poll doorbell (8-bit). Write `TX_POLL_NPQ` to nudge the normal queue.
pub const REG_TX_POLL: u32 = 0x38;
/// Interrupt mask (16-bit, classic 8169 path).
pub const REG_INTR_MASK: u32 = 0x3C;
/// Interrupt status (16-bit, classic 8169 path).
pub const REG_INTR_STATUS: u32 = 0x3E;
/// ChipCmd (8-bit): RST / RxEnb / TxEnb.
pub const REG_CHIP_CMD: u32 = 0x37;
/// TxConfig (32-bit) — its high bits carry the XID chip-version field.
pub const REG_TX_CONFIG: u32 = 0x40;
/// RxConfig (32-bit).
pub const REG_RX_CONFIG: u32 = 0x44;
/// Cfg9346 (8-bit) EEPROM/config lock: write `CFG9346_UNLOCK` / `CFG9346_LOCK`.
pub const REG_CFG9346: u32 = 0x50;
/// CPlusCmd (16-bit) — enables the C+ descriptor mode.
pub const REG_CPLUS_CMD: u32 = 0xE0;
/// RxMaxSize (16-bit) — max accepted RX frame size.
pub const REG_RX_MAX_SIZE: u32 = 0xDA;
/// Max transmit packet size (8-bit).
pub const REG_MTPS: u32 = 0xEC;
/// RX descriptor ring base, low 32 bits.
pub const REG_RX_DESC_START_ADDR_LOW: u32 = 0xE4;
/// RX descriptor ring base, high 32 bits.
pub const REG_RX_DESC_START_ADDR_HIGH: u32 = 0xE8;

/// ChipCmd bits.
pub const CHIP_CMD_RST: u8 = 0x10;
pub const CHIP_CMD_RX_ENB: u8 = 0x08;
pub const CHIP_CMD_TX_ENB: u8 = 0x04;

/// TxPoll: poll the Normal-Priority Queue for newly-owned TX descriptors.
pub const TX_POLL_NPQ: u8 = 0x40;

/// Cfg9346 unlock / lock values bracketing config-register writes.
pub const CFG9346_UNLOCK: u8 = 0xC0;
pub const CFG9346_LOCK: u8 = 0x00;

// ---------------------------------------------------------------------------
// 8125 "V2" 32-bit interrupt block (Track D.1).
//
// The classic 8169 uses the 16-bit IntrMask (0x3C) / IntrStatus (0x3E). The
// 8125/8126 move the interrupt block to a 32-bit "V2" layout; the driver
// version-branches on `MacVersion::is_8125` to pick the register set.
// ---------------------------------------------------------------------------

/// IMR_V2_CLEAR (32-bit): write 1s to mask (clear) interrupt sources.
pub const REG_IMR_V2_CLEAR: u32 = 0x150;
/// ISR_V2 (32-bit): interrupt status; write-1-to-clear.
pub const REG_ISR_V2: u32 = 0x154;
/// IMR_V2_SET (32-bit): write 1s to unmask (set) interrupt sources.
pub const REG_IMR_V2_SET: u32 = 0x158;
/// INT_CFG0_8125 (8-bit): 8125 interrupt configuration.
pub const REG_INT_CFG0_8125: u32 = 0x34;

/// INT_CFG0 enable bit used by the 8125 path.
pub const INT_CFG0_ENABLE: u8 = 0x08;

// ---------------------------------------------------------------------------
// C+ descriptor bit layout (Track C.1).
//
// Each descriptor is 16 bytes:
//   opts1   (u32 @ 0)  : OWN/EOR/FS/LS + 14-bit frame length
//   opts2   (u32 @ 4)  : VLAN / offload
//   addr_lo (u32 @ 8)  : buffer IOVA low 32 bits
//   addr_hi (u32 @ 12) : buffer IOVA high 32 bits
// ---------------------------------------------------------------------------

/// Descriptor size in bytes.
pub const DESC_SIZE: usize = 16;
/// Ring base-address alignment required by the C+ engine.
pub const RING_ALIGN: usize = 256;

/// opts1: NIC owns the descriptor.
pub const DESC_OWN: u32 = 0x8000_0000;
/// opts1: end of ring (wrap back to slot 0 after this one).
pub const DESC_EOR: u32 = 0x4000_0000;
/// opts1: first segment of a frame.
pub const DESC_FS: u32 = 0x2000_0000;
/// opts1: last segment of a frame.
pub const DESC_LS: u32 = 0x1000_0000;
/// opts1: mask of the 14-bit frame-length field.
pub const DESC_FRAME_LEN_MASK: u32 = 0x0000_3FFF;

/// Encode the `opts1` word for a descriptor.
///
/// `is_last_slot` sets EOR on the final ring entry. `own` marks NIC ownership
/// (true for freshly-posted RX buffers and queued TX frames). `fs`/`ls` mark
/// single-buffer frames (both true for the common case). `frame_len` is the
/// buffer length, clamped to the 14-bit field.
#[inline]
pub fn encode_opts1(own: bool, is_last_slot: bool, fs: bool, ls: bool, frame_len: u32) -> u32 {
    let mut w = frame_len & DESC_FRAME_LEN_MASK;
    if own {
        w |= DESC_OWN;
    }
    if is_last_slot {
        w |= DESC_EOR;
    }
    if fs {
        w |= DESC_FS;
    }
    if ls {
        w |= DESC_LS;
    }
    w
}

/// Returns true if the NIC still owns this descriptor (OWN set).
#[inline]
pub fn desc_is_owned_by_nic(opts1: u32) -> bool {
    opts1 & DESC_OWN != 0
}

/// Extract the frame length from a completed RX descriptor's `opts1`.
#[inline]
pub fn desc_frame_len(opts1: u32) -> u16 {
    (opts1 & DESC_FRAME_LEN_MASK) as u16
}

/// True if `ring_len_bytes` is a usable C+ ring length: a non-zero multiple of
/// `DESC_SIZE` whose byte length is also `RING_ALIGN`-aligned (so the wrap stays
/// aligned given a page-aligned base).
#[inline]
pub fn ring_len_is_valid(ring_len_bytes: usize) -> bool {
    ring_len_bytes != 0
        && ring_len_bytes.is_multiple_of(DESC_SIZE)
        && ring_len_bytes.is_multiple_of(RING_ALIGN)
}

/// True if `addr` satisfies the 256-byte ring-base alignment requirement.
#[inline]
pub fn ring_base_is_aligned(addr: u64) -> bool {
    (addr as usize).is_multiple_of(RING_ALIGN)
}

/// Build a C+ descriptor ring into `out`.
///
/// `out` must be at least `count * DESC_SIZE` bytes; the caller allocates DMA
/// memory satisfying [`ring_base_is_aligned`]. `buf_iovas` gives the buffer IOVA
/// for each slot. `own` marks each slot NIC-owned (RX rings post all buffers to
/// the NIC; TX rings start host-owned with `own = false`). `frame_len` is the
/// per-buffer length to advertise.
///
/// The last slot gets EOR; every slot gets FS|LS (single-buffer frames).
///
/// Returns the number of descriptors written, or 0 on a size mismatch.
pub fn build_ring(out: &mut [u8], buf_iovas: &[u64], own: bool, frame_len: u32) -> usize {
    let count = buf_iovas.len();
    if count == 0 || out.len() < count * DESC_SIZE {
        return 0;
    }
    for (i, &iova) in buf_iovas.iter().enumerate() {
        let is_last = i == count - 1;
        let opts1 = encode_opts1(own, is_last, true, true, frame_len);
        let base = i * DESC_SIZE;
        out[base..base + 4].copy_from_slice(&opts1.to_le_bytes());
        // opts2 (VLAN/offload) — zero.
        out[base + 4..base + 8].copy_from_slice(&0u32.to_le_bytes());
        out[base + 8..base + 12].copy_from_slice(&((iova & 0xFFFF_FFFF) as u32).to_le_bytes());
        out[base + 12..base + 16].copy_from_slice(&((iova >> 32) as u32).to_le_bytes());
    }
    count
}

/// Read the `opts1` word of descriptor `slot` from a ring buffer.
#[inline]
pub fn read_opts1(ring: &[u8], slot: usize) -> u32 {
    let base = slot * DESC_SIZE;
    u32::from_le_bytes([ring[base], ring[base + 1], ring[base + 2], ring[base + 3]])
}

// ---------------------------------------------------------------------------
// XID chip-versioning (Track C.2).
//
// Realtek encodes the silicon revision in the high bits of TxConfig (0x40).
// Linux's rtl8169_get_mac_version computes
//     xid = (RTL_R32(tp, TxConfig) >> 20) & 0xfcf
// and walks an ordered {mask, val, ver} table, taking the first entry where
// (xid & mask) == val. The masks 0x7cf / 0x7c8 in the brief are the common
// distinguishing masks for the 8168/8125 families. We port the subset covering
// the device IDs we claim.
// ---------------------------------------------------------------------------

/// Compute the raw XID field from a TxConfig register value.
#[inline]
pub fn xid_from_tx_config(tx_config: u32) -> u32 {
    (tx_config >> 20) & 0xFCF
}

/// Realtek MAC silicon version. The enum value is the Linux `RTL_GIGA_MAC_VER_*`
/// number for traceability.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MacVersion {
    /// RTL8169/8168/8125 family revision (Linux `RTL_GIGA_MAC_VER_*` number).
    Ver(u8),
    /// Unknown / unmatched XID.
    Unknown,
}

impl MacVersion {
    /// The Linux MAC-version number, or 0 for `Unknown`.
    #[inline]
    pub fn number(self) -> u8 {
        match self {
            MacVersion::Ver(n) => n,
            MacVersion::Unknown => 0,
        }
    }

    /// True for the 8125/8126 2.5G+ parts (V2 interrupt block, firmware
    /// mandatory). These map to Linux versions 60+ (RTL_GIGA_MAC_VER_61..).
    #[inline]
    pub fn is_8125(self) -> bool {
        matches!(self, MacVersion::Ver(n) if n >= 60)
    }

    /// True if this version requires a firmware blob. Per Linux: all 8168G and
    /// later (version >= 40) and all 8125/8126 require firmware. Earlier parts
    /// (8169/8168A..F) bring up without firmware.
    #[inline]
    pub fn requires_firmware(self) -> bool {
        match self {
            MacVersion::Ver(n) => n >= 40,
            MacVersion::Unknown => false,
        }
    }
}

/// One row of the XID -> version table: `(xid & mask) == value => Ver(version)`.
#[derive(Clone, Copy)]
struct XidEntry {
    mask: u32,
    value: u32,
    version: u8,
}

/// Ordered XID table (first match wins), modelled on the subset of
/// `rtl_chip_infos` / `rtl8169_get_mac_version` covering our claimed device IDs.
/// Most distinguishing comparisons use mask 0x7cf; family-group comparisons use
/// 0x7c8 (the brief's two masks).
const XID_TABLE: &[XidEntry] = &[
    // --- 8125 / 8126 family (2.5G+, V2 interrupts, firmware mandatory). ---
    XidEntry {
        mask: 0x7cf,
        value: 0x649,
        version: 65,
    }, // RTL8126A
    XidEntry {
        mask: 0x7cf,
        value: 0x64a,
        version: 64,
    }, // RTL8125D
    XidEntry {
        mask: 0x7cf,
        value: 0x641,
        version: 63,
    }, // RTL8125B
    XidEntry {
        mask: 0x7cf,
        value: 0x609,
        version: 61,
    }, // RTL8125A
    // Family fallback: any 0x6xx with the 8125 group bits -> treat as 8125A.
    XidEntry {
        mask: 0x7c8,
        value: 0x608,
        version: 61,
    },
    // --- 8168 G/H/EP family (firmware required, version >= 40). ---
    XidEntry {
        mask: 0x7cf,
        value: 0x540,
        version: 42,
    }, // RTL8168GU / 8411B
    XidEntry {
        mask: 0x7cf,
        value: 0x500,
        version: 40,
    }, // RTL8168G
    XidEntry {
        mask: 0x7cf,
        value: 0x541,
        version: 51,
    }, // RTL8168EP
    XidEntry {
        mask: 0x7cf,
        value: 0x549,
        version: 52,
    }, // RTL8168FP/8117
    // --- 8168 E/F family (no firmware). ---
    XidEntry {
        mask: 0x7cf,
        value: 0x4c0,
        version: 34,
    }, // RTL8168E-VL
    XidEntry {
        mask: 0x7cf,
        value: 0x2c0,
        version: 33,
    }, // RTL8168E
    XidEntry {
        mask: 0x7cf,
        value: 0x480,
        version: 36,
    }, // RTL8168F
    // --- 8168 B/C/D + classic 8169 (no firmware). ---
    XidEntry {
        mask: 0x7cf,
        value: 0x380,
        version: 17,
    }, // RTL8168C
    XidEntry {
        mask: 0x7cf,
        value: 0x300,
        version: 11,
    }, // RTL8168B
    XidEntry {
        mask: 0x7cf,
        value: 0x100,
        version: 2,
    }, // RTL8169s
    XidEntry {
        mask: 0x7c8,
        value: 0x000,
        version: 1,
    }, // RTL8169 (oldest)
];

/// Map a TxConfig register value to a [`MacVersion`] using the XID table.
pub fn mac_version_from_tx_config(tx_config: u32) -> MacVersion {
    mac_version_from_xid(xid_from_tx_config(tx_config))
}

/// Map a raw XID field to a [`MacVersion`] using the ordered table.
pub fn mac_version_from_xid(xid: u32) -> MacVersion {
    for e in XID_TABLE {
        if xid & e.mask == e.value {
            return MacVersion::Ver(e.version);
        }
    }
    MacVersion::Unknown
}

// ---------------------------------------------------------------------------
// Per-version soft reset (Track C.2).
//
// Soft reset writes CHIP_CMD_RST to ChipCmd (0x37); the bit self-clears when
// the reset completes. The driver polls a bounded number of iterations. This
// predicate captures the pure "is the reset done?" check so the poll loop is
// host-testable.
// ---------------------------------------------------------------------------

/// Number of poll iterations the bring-up code should spin before declaring the
/// soft reset timed out (matches Linux's 100 * 10us budget).
pub const SOFT_RESET_POLL_MAX: u32 = 100;

/// True once the ChipCmd RST bit has self-cleared (reset complete).
#[inline]
pub fn soft_reset_complete(chip_cmd: u8) -> bool {
    chip_cmd & CHIP_CMD_RST == 0
}

// ---------------------------------------------------------------------------
// Firmware-header validation + load path (Track D.1).
//
// Linux's rtl8169_fw.c `rtl_fw_format` header:
//   u8  version[RTL_VER_SIZE];   // RTL_VER_SIZE = 32, NUL-padded version string
//   union {
//     struct { __le32 fw_offset; __le32 fw_reg; } digital;
//     ...
//   };
// Followed by the firmware payload (an array of __le32 instructions for the
// PHY/MAC). We validate the header structure and payload sanity; if the blob is
// absent or corrupt we SKIP firmware load with a degraded-link warning rather
// than panicking.
// ---------------------------------------------------------------------------

/// Size of the version string field in the firmware header.
pub const RTL_FW_VER_SIZE: usize = 32;
/// Header size: version[32] + fw_offset(4) + fw_reg(4).
pub const RTL_FW_HEADER_SIZE: usize = RTL_FW_VER_SIZE + 8;
/// Firmware payload is an array of 32-bit instruction words.
pub const RTL_FW_INSTR_SIZE: usize = 4;
/// Upper bound on a sane firmware blob (guards against a corrupt giant length).
pub const RTL_FW_MAX_LEN: usize = 64 * 1024;

/// Parsed, validated firmware header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FirmwareHeader {
    /// Number of 32-bit instruction words in the payload.
    pub instr_count: usize,
    /// Byte length of the instruction payload.
    pub payload_len: usize,
}

/// Reason a firmware blob failed validation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FirmwareError {
    /// Blob is shorter than the fixed header.
    TooShort,
    /// Version string is empty / all NUL (not a real rtl_nic blob).
    EmptyVersion,
    /// Payload size is not a whole number of 32-bit instructions.
    UnalignedPayload,
    /// Payload is empty or exceeds the sane upper bound.
    BadPayloadLen,
}

/// Validate an `rtl_nic` firmware blob's header + payload framing.
///
/// This does *not* execute the firmware; it only confirms the blob is a
/// structurally-sane `rtl_fw_format` image so the loader can decide whether to
/// program it or fall back to a degraded link.
pub fn validate_firmware_header(blob: &[u8]) -> Result<FirmwareHeader, FirmwareError> {
    if blob.len() < RTL_FW_HEADER_SIZE {
        return Err(FirmwareError::TooShort);
    }
    // Version string must contain at least one non-NUL byte.
    if blob[..RTL_FW_VER_SIZE].iter().all(|&b| b == 0) {
        return Err(FirmwareError::EmptyVersion);
    }
    let payload_len = blob.len() - RTL_FW_HEADER_SIZE;
    if payload_len == 0 || payload_len > RTL_FW_MAX_LEN {
        return Err(FirmwareError::BadPayloadLen);
    }
    if !payload_len.is_multiple_of(RTL_FW_INSTR_SIZE) {
        return Err(FirmwareError::UnalignedPayload);
    }
    Ok(FirmwareHeader {
        instr_count: payload_len / RTL_FW_INSTR_SIZE,
        payload_len,
    })
}

/// Outcome of the firmware load path. The bring-up code matches on this: a
/// `Loaded` programs the blob, any other variant emits a degraded-link warning
/// sentinel and continues (never panics).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FirmwareLoad {
    /// Blob validated; program these many instruction words.
    Loaded(FirmwareHeader),
    /// No blob was staged for this chip — degrade gracefully.
    Absent,
    /// Blob present but malformed — degrade gracefully.
    Corrupt(FirmwareError),
    /// This MAC version does not need firmware — skip without warning.
    NotRequired,
}

impl FirmwareLoad {
    /// True if the loader should emit a degraded-link warning sentinel.
    #[inline]
    pub fn is_degraded(self) -> bool {
        matches!(self, FirmwareLoad::Absent | FirmwareLoad::Corrupt(_))
    }
}

/// Decide the firmware-load outcome for `version` given an optional blob.
///
/// * Versions that do not require firmware -> `NotRequired` (no warning).
/// * Required but no blob staged -> `Absent` (degraded).
/// * Required and blob present -> validate; `Loaded` or `Corrupt` (degraded).
///
/// Crucially this *never* panics on absent/corrupt firmware, satisfying the
/// Track D.1 "degraded-link warning sentinel, NOT panic" requirement.
pub fn resolve_firmware(version: MacVersion, blob: Option<&[u8]>) -> FirmwareLoad {
    if !version.requires_firmware() {
        return FirmwareLoad::NotRequired;
    }
    match blob {
        None => FirmwareLoad::Absent,
        Some(b) => match validate_firmware_header(b) {
            Ok(h) => FirmwareLoad::Loaded(h),
            Err(e) => FirmwareLoad::Corrupt(e),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;

    // --- Track C.1: descriptor bit layout + ring builder ---

    #[test]
    fn opts1_bit_placement() {
        let w = encode_opts1(true, true, true, true, 0x1234);
        assert_eq!(w & DESC_OWN, DESC_OWN);
        assert_eq!(w & DESC_EOR, DESC_EOR);
        assert_eq!(w & DESC_FS, DESC_FS);
        assert_eq!(w & DESC_LS, DESC_LS);
        assert_eq!(desc_frame_len(w), 0x1234);
        assert_eq!(DESC_OWN, 0x8000_0000);
        assert_eq!(DESC_EOR, 0x4000_0000);
        assert_eq!(DESC_FS, 0x2000_0000);
        assert_eq!(DESC_LS, 0x1000_0000);
    }

    #[test]
    fn opts1_clears_unset_flags() {
        let w = encode_opts1(false, false, true, false, 100);
        assert_eq!(w & DESC_OWN, 0);
        assert_eq!(w & DESC_EOR, 0);
        assert_eq!(w & DESC_FS, DESC_FS);
        assert_eq!(w & DESC_LS, 0);
        assert!(!desc_is_owned_by_nic(w));
    }

    #[test]
    fn frame_len_is_clamped_to_14_bits() {
        let w = encode_opts1(false, false, false, false, 0xFFFF);
        assert_eq!(desc_frame_len(w), 0x3FFF);
    }

    #[test]
    fn ring_builder_produces_aligned_correct_ring() {
        // 16 descriptors so the byte length (256) is RING_ALIGN-aligned; back it
        // with a 256-aligned store (mimics a DMA allocation).
        #[repr(align(256))]
        struct Aligned([u8; 16 * DESC_SIZE]);
        let mut backing = Aligned([0u8; 16 * DESC_SIZE]);
        let mut iovas = [0u64; 16];
        for (i, slot) in iovas.iter_mut().enumerate() {
            *slot = 0x1000u64 + (i as u64) * 0x1000;
        }
        // Make the last slot carry a 64-bit IOVA so the high word is exercised.
        iovas[15] = 0x1_0000_4000;
        let n = build_ring(&mut backing.0, &iovas, true, 2048);
        assert_eq!(n, 16);
        assert!(ring_base_is_aligned(backing.0.as_ptr() as u64));
        assert!(ring_len_is_valid(backing.0.len()));

        // Slot 0: OWN|FS|LS, NOT EOR, len 2048, addr_lo 0x1000, addr_hi 0.
        let o0 = read_opts1(&backing.0, 0);
        assert!(desc_is_owned_by_nic(o0));
        assert_eq!(o0 & DESC_EOR, 0);
        assert_eq!(o0 & DESC_FS, DESC_FS);
        assert_eq!(o0 & DESC_LS, DESC_LS);
        assert_eq!(desc_frame_len(o0), 2048);
        assert_eq!(
            u32::from_le_bytes([backing.0[8], backing.0[9], backing.0[10], backing.0[11]]),
            0x1000
        );

        // Last slot (15): EOR set, high addr word carries the 64-bit IOVA.
        let o15 = read_opts1(&backing.0, 15);
        assert_eq!(o15 & DESC_EOR, DESC_EOR);
        let base = 15 * DESC_SIZE;
        let lo = u32::from_le_bytes([
            backing.0[base + 8],
            backing.0[base + 9],
            backing.0[base + 10],
            backing.0[base + 11],
        ]);
        let hi = u32::from_le_bytes([
            backing.0[base + 12],
            backing.0[base + 13],
            backing.0[base + 14],
            backing.0[base + 15],
        ]);
        assert_eq!(lo, 0x4000);
        assert_eq!(hi, 0x1);

        // Only the last slot has EOR.
        for slot in 0..15 {
            assert_eq!(read_opts1(&backing.0, slot) & DESC_EOR, 0);
        }
    }

    #[test]
    fn ring_builder_rejects_size_mismatch() {
        let mut small = [0u8; DESC_SIZE]; // room for 1 desc
        let iovas = [0x1000u64, 0x2000];
        assert_eq!(build_ring(&mut small, &iovas, true, 64), 0);
        assert_eq!(build_ring(&mut small, &[], true, 64), 0);
    }

    #[test]
    fn ring_validators() {
        assert!(ring_len_is_valid(RING_ALIGN));
        assert!(ring_len_is_valid(2 * RING_ALIGN));
        assert!(!ring_len_is_valid(0));
        assert!(!ring_len_is_valid(DESC_SIZE)); // multiple of 16 but not 256
        assert!(!ring_len_is_valid(RING_ALIGN - 1));
        assert!(ring_base_is_aligned(0x1_0000));
        assert!(ring_base_is_aligned(256));
        assert!(!ring_base_is_aligned(0x1_0010));
    }

    // --- Track C.2: XID -> MacVersion + soft reset ---

    #[test]
    fn xid_extraction() {
        // XID lives at (TxConfig >> 20) & 0xfcf.
        let tx = 0x540u32 << 20;
        assert_eq!(xid_from_tx_config(tx), 0x540);
        // Bits outside the 0xfcf mask are dropped.
        let tx2 = (0x540u32 | 0x30) << 20;
        assert_eq!(xid_from_tx_config(tx2), 0x540);
    }

    #[test]
    fn xid_to_mac_version_representative_set() {
        // 8169 classic.
        assert_eq!(mac_version_from_xid(0x000), MacVersion::Ver(1));
        // 8168B.
        assert_eq!(mac_version_from_xid(0x300), MacVersion::Ver(11));
        // 8168G (first firmware-required GbE).
        assert_eq!(mac_version_from_xid(0x500), MacVersion::Ver(40));
        // 8168GU.
        assert_eq!(mac_version_from_xid(0x540), MacVersion::Ver(42));
        // 8125A (2.5G, V2 interrupts).
        assert_eq!(mac_version_from_xid(0x609), MacVersion::Ver(61));
        // 8125B.
        assert_eq!(mac_version_from_xid(0x641), MacVersion::Ver(63));
        // 8126A.
        assert_eq!(mac_version_from_xid(0x649), MacVersion::Ver(65));
    }

    #[test]
    fn mac_version_through_tx_config() {
        let tx = 0x609u32 << 20;
        let v = mac_version_from_tx_config(tx);
        assert_eq!(v, MacVersion::Ver(61));
        assert!(v.is_8125());
        assert!(v.requires_firmware());
    }

    #[test]
    fn version_classification() {
        // Classic GbE: no firmware, not 8125.
        let v2 = MacVersion::Ver(2);
        assert!(!v2.requires_firmware());
        assert!(!v2.is_8125());
        // 8168G: firmware required, not 8125.
        let v40 = MacVersion::Ver(40);
        assert!(v40.requires_firmware());
        assert!(!v40.is_8125());
        // 8125: firmware + V2.
        let v61 = MacVersion::Ver(61);
        assert!(v61.requires_firmware());
        assert!(v61.is_8125());
        // Unknown: conservative defaults.
        assert!(!MacVersion::Unknown.requires_firmware());
        assert!(!MacVersion::Unknown.is_8125());
        assert_eq!(MacVersion::Unknown.number(), 0);
    }

    #[test]
    fn unknown_xid() {
        // An XID matching no group at all.
        assert_eq!(mac_version_from_xid(0x7ff), MacVersion::Unknown);
    }

    #[test]
    fn soft_reset_poll_predicate() {
        assert!(!soft_reset_complete(CHIP_CMD_RST | CHIP_CMD_RX_ENB));
        assert!(soft_reset_complete(CHIP_CMD_RX_ENB | CHIP_CMD_TX_ENB));
        assert!(soft_reset_complete(0));
        assert_eq!(SOFT_RESET_POLL_MAX, 100);
    }

    // --- Track D.1: V2 interrupt register offsets ---

    #[test]
    fn v2_interrupt_register_offsets() {
        assert_eq!(REG_IMR_V2_CLEAR, 0x150);
        assert_eq!(REG_ISR_V2, 0x154);
        assert_eq!(REG_IMR_V2_SET, 0x158);
        assert_eq!(REG_INT_CFG0_8125, 0x34);
        // Classic 16-bit block is distinct from the V2 block.
        assert_eq!(REG_INTR_MASK, 0x3C);
        assert_eq!(REG_INTR_STATUS, 0x3E);
    }

    #[test]
    fn classic_register_offsets() {
        assert_eq!(REG_TX_DESC_START_ADDR_LOW, 0x20);
        assert_eq!(REG_TX_DESC_START_ADDR_HIGH, 0x24);
        assert_eq!(REG_RX_DESC_START_ADDR_LOW, 0xE4);
        assert_eq!(REG_RX_DESC_START_ADDR_HIGH, 0xE8);
        assert_eq!(REG_TX_POLL, 0x38);
        assert_eq!(TX_POLL_NPQ, 0x40);
        assert_eq!(REG_CFG9346, 0x50);
        assert_eq!(CFG9346_UNLOCK, 0xC0);
        assert_eq!(CFG9346_LOCK, 0x00);
        assert_eq!(REG_CHIP_CMD, 0x37);
        assert_eq!(CHIP_CMD_RST, 0x10);
    }

    // --- Track D.1: firmware-header validation + load path ---

    fn fake_fw(version: &str, instr: &[u32]) -> Vec<u8> {
        let mut v = vec![0u8; RTL_FW_VER_SIZE];
        let vb = version.as_bytes();
        let n = vb.len().min(RTL_FW_VER_SIZE);
        v[..n].copy_from_slice(&vb[..n]);
        // fw_offset + fw_reg (8 bytes, contents irrelevant to framing).
        v.extend_from_slice(&[0u8; 8]);
        for w in instr {
            v.extend_from_slice(&w.to_le_bytes());
        }
        v
    }

    #[test]
    fn validate_good_firmware() {
        let blob = fake_fw("rtl8125a-3", &[0x0001_0002, 0x0003_0004, 0x0005_0006]);
        let h = validate_firmware_header(&blob).expect("valid");
        assert_eq!(h.instr_count, 3);
        assert_eq!(h.payload_len, 12);
    }

    #[test]
    fn validate_rejects_short_blob() {
        let blob = [0xAAu8; RTL_FW_HEADER_SIZE - 1];
        assert_eq!(
            validate_firmware_header(&blob),
            Err(FirmwareError::TooShort)
        );
    }

    #[test]
    fn validate_rejects_empty_version() {
        let blob = fake_fw("", &[0x1]);
        assert_eq!(
            validate_firmware_header(&blob),
            Err(FirmwareError::EmptyVersion)
        );
    }

    #[test]
    fn validate_rejects_empty_payload() {
        let blob = fake_fw("rtl8125a-3", &[]);
        assert_eq!(
            validate_firmware_header(&blob),
            Err(FirmwareError::BadPayloadLen)
        );
    }

    #[test]
    fn validate_rejects_unaligned_payload() {
        let mut blob = fake_fw("rtl8125a-3", &[0x1]);
        blob.push(0xFF); // payload now 5 bytes, not a multiple of 4
        assert_eq!(
            validate_firmware_header(&blob),
            Err(FirmwareError::UnalignedPayload)
        );
    }

    #[test]
    fn resolve_firmware_not_required() {
        // Classic GbE never loads firmware, even if a blob is present.
        assert_eq!(
            resolve_firmware(MacVersion::Ver(2), None),
            FirmwareLoad::NotRequired
        );
        let blob = fake_fw("x", &[0x1]);
        assert_eq!(
            resolve_firmware(MacVersion::Ver(2), Some(&blob)),
            FirmwareLoad::NotRequired
        );
    }

    #[test]
    fn resolve_firmware_absent_degrades_not_panics() {
        let r = resolve_firmware(MacVersion::Ver(61), None);
        assert_eq!(r, FirmwareLoad::Absent);
        assert!(r.is_degraded());
    }

    #[test]
    fn resolve_firmware_corrupt_degrades_not_panics() {
        let bad = [0u8; 4]; // too short, all NUL
        let r = resolve_firmware(MacVersion::Ver(61), Some(&bad));
        assert!(matches!(r, FirmwareLoad::Corrupt(_)));
        assert!(r.is_degraded());
    }

    #[test]
    fn resolve_firmware_loaded() {
        let blob = fake_fw("rtl8125a-3", &[0xAABB_CCDD, 0x1122_3344]);
        let r = resolve_firmware(MacVersion::Ver(61), Some(&blob));
        match r {
            FirmwareLoad::Loaded(h) => assert_eq!(h.instr_count, 2),
            other => panic!("expected Loaded, got {other:?}"),
        }
        assert!(!r.is_degraded());
    }
}
