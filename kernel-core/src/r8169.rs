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
/// PHYAR (32-bit) — MDIO access to the on-chip PHY (Linux `r8169_mdio_*`).
/// Write: `PHYAR_FLAG | (reg << 16) | val`, poll until `PHYAR_FLAG` self-clears.
/// Read: `(reg << 16)`, poll until `PHYAR_FLAG` sets, value in the low 16 bits.
pub const REG_PHYAR: u32 = 0x60;
/// PHYAR command/ready flag (bit 31): set on write, cleared by hw when done;
/// set by hw on a read completion.
pub const PHYAR_FLAG: u32 = 0x8000_0000;
/// PHYstatus (8-bit @ 0x6C) — MAC's view of the link: `PHYSTATUS_LINK` (bit 1)
/// plus per-speed bits. Read-only; a simple MMIO read (no MDIO).
pub const REG_PHYSTATUS: u32 = 0x6C;
/// PHYstatus link-up bit.
pub const PHYSTATUS_LINK: u8 = 0x02;
/// BMCR (PHY register 0) value to enable + restart auto-negotiation and keep
/// the PHY powered up: ANE (0x1000) | RESTART_AN (0x0200) — `0x1200` plus the
/// speed-select bits Linux leaves; the captured RTL8125B bring-up writes
/// `0x9240` (reset-clear + ANE + restart-AN + duplex/speed defaults).
pub const BMCR_AUTONEG_RESTART: u16 = 0x9240;
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

// ---------------------------------------------------------------------------
// RxConfig / TxConfig values.
//
// The classic 8169 accepts frames with just the low accept bits set, but the
// 8125 RX *DMA engine* will not move frames into host memory unless the
// fetch-default and DMA-burst fields are also programmed (Linux
// `rtl_init_rxcfg`: `RxConfig = RX_FETCH_DFLT_8125 | RX_DMA_BURST | accepts`).
// Omitting them is silent: link comes up but the RX ring never drains.
// ---------------------------------------------------------------------------

/// RxConfig accept bits: AcceptBroadcast (0x08) | AcceptMulticast (0x04) |
/// AcceptMyPhys (0x02). The low-nibble accept mask Linux ORs in via
/// `rtl_set_rx_mode`.
pub const RX_CONFIG_ACCEPT: u32 = 0x0E;
/// 8125 RxConfig fetch-default field (`8 << 27`).
pub const RX_FETCH_DFLT_8125: u32 = 8 << 27;
/// RxConfig DMA-burst field, unlimited (`7 << 8`). Shared 8125/8168 encoding.
pub const RX_DMA_BURST: u32 = 7 << 8;
/// TxConfig DMA-burst (unlimited, `7 << 8`) + standard inter-frame-gap
/// (`3 << 24`). The soft reset clears TxConfig's writable fields, so the 8125
/// TX engine needs these reprogrammed before frames will be fetched/sent.
pub const TX_DMA_BURST: u32 = 7 << 8;
pub const TX_INTERFRAMEGAP: u32 = 3 << 24;

/// Full RxConfig value for an 8125 part: fetch default + unlimited DMA burst +
/// the broadcast/multicast/my-phys accept bits.
#[inline]
pub fn rxconfig_8125() -> u32 {
    RX_FETCH_DFLT_8125 | RX_DMA_BURST | RX_CONFIG_ACCEPT
}

/// MISC register (`0xF0`, 32-bit). Holds the RXDV gate on 8168g+/8125 parts.
pub const REG_MISC: u32 = 0xF0;
/// RXDV-gated-enable bit (`1 << 19`) in [`REG_MISC`]. While set, the 8125 MAC
/// gates the receive-data-valid signal and **drops every inbound frame before
/// it reaches the RX ring** — Linux `rtl_disable_rxdvgate` clears it during
/// hardware start. The classic 8169 bring-up never touches this, so an 8125
/// driven by the shared path links up but receives nothing until it is cleared.
pub const RXDV_GATED_EN: u32 = 1 << 19;

/// TxConfig value for an 8125 part: unlimited DMA burst + standard IFG. The
/// hardware-version (XID) bits are read-only, so this only sets writable fields.
#[inline]
pub fn txconfig_8125() -> u32 {
    TX_DMA_BURST | TX_INTERFRAMEGAP
}

/// TxPoll (8168 and earlier): poll the Normal-Priority Queue for newly-owned
/// TX descriptors. 8-bit register at `0x38`.
pub const TX_POLL_NPQ: u8 = 0x40;
/// TxPoll for 8125/8126: a *different* 16-bit doorbell register (`0x90`); write
/// the queue-0 bit to kick transmission. Linux `rtl8169_doorbell` branches here
/// for `rtl_is_8125` — using the classic `0x38` doorbell on an 8125 leaves
/// posted TX descriptors un-transmitted.
pub const REG_TX_POLL_8125: u32 = 0x90;
/// 8125 TxPoll queue-0 doorbell bit.
pub const TX_POLL_8125_Q0: u16 = 0x0001;

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
// GPHY-OCP PHY access (8168g and later, incl. 8125/8126).
//
// The classic 8168 reaches its PHY through `PHYAR` (0x60). The 8168g+ and all
// 8125/8126 parts reach the PHY through a *GPHY-OCP* window instead: a single
// 32-bit register at `GPHY_OCP` (0xB8). Linux `r8168_phy_ocp_{read,write}` +
// `r8168g_mdio_{read,write}` (`r8169_main.c`).
//
// Command word layout (32-bit, written to / read from `GPHY_OCP`):
//   bit 31      : OCPAR_FLAG — busy/command. WRITE sets it then hw clears it;
//                 READ is issued with it clear and hw sets it when data ready.
//   bits 30..16 : the *word* address = (byte OCP address >> 1)
//   bits 15..0  : data (16-bit)
//
// Because every OCP byte address is even (`base + reg*2`), Linux encodes the
// word address as `byte_addr << 15` (== `(byte_addr >> 1) << 16`). We keep the
// same trick so the bit math matches the driver exactly.
//
// Page model (MDIO compatibility): writing MDIO "register 0x1f" selects a page
// and only updates a base address (no bus cycle): `ocp_base = page<<4` (or the
// standard base `0xA400` for page 0). In a non-standard page, in-page MDIO
// registers are offset by 0x10, so the OCP byte address is
// `ocp_base + (reg - 0x10) * 2`; in the standard page it is `ocp_base + reg*2`.
// ---------------------------------------------------------------------------

/// GPHY-OCP window register (32-bit) used by 8168g+/8125 PHY access.
pub const REG_GPHY_OCP: u32 = 0xB8;
/// MAC-OCP data register (`OCPDR`, 32-bit). MAC-OCP registers (`0xC000`..
/// `0xFFFF` — RX/TX FIFO, DMA, power, feature gates) are reached through this
/// window. The command-word encoding is identical to the GPHY-OCP window
/// ([`gphy_ocp_write_cmd`]/[`gphy_ocp_read_cmd`]); only the register offset
/// differs and MAC-OCP needs no busy-poll (the access completes immediately).
pub const REG_OCPDR: u32 = 0xB0;
/// GPHY-OCP busy/command flag (bit 31).
pub const GPHY_OCP_FLAG: u32 = 0x8000_0000;
/// Standard PHY OCP base (MDIO page 0).
pub const OCP_STD_PHY_BASE: u32 = 0xA400;

/// Resolve an MDIO page-select value into the OCP base address.
///
/// Mirrors `r8168g_mdio_write` page handling: page 0 → the standard base,
/// otherwise `page << 4`.
#[inline]
pub fn ocp_base_for_page(page: u16) -> u32 {
    if page == 0 {
        OCP_STD_PHY_BASE
    } else {
        (page as u32) << 4
    }
}

/// Map an in-page MDIO register number to its OCP *byte* address within the
/// page identified by `ocp_base`. In a non-standard page the register is offset
/// by 0x10 (so MDIO reg 0x10 maps to the page's first OCP word).
#[inline]
pub fn phy_ocp_addr(ocp_base: u32, reg: u32) -> u32 {
    let reg = if ocp_base != OCP_STD_PHY_BASE {
        reg.wrapping_sub(0x10)
    } else {
        reg
    };
    ocp_base + reg * 2
}

/// Encode the `GPHY_OCP` command word for a *write* to OCP byte address `addr`.
/// `addr` must be even (all real OCP addresses are). Equivalent to Linux's
/// `OCPAR_FLAG | (addr << 15) | data`.
#[inline]
pub fn gphy_ocp_write_cmd(addr: u32, data: u16) -> u32 {
    GPHY_OCP_FLAG | (addr << 15) | data as u32
}

/// Encode the `GPHY_OCP` command word for a *read* from OCP byte address
/// `addr` (flag clear; hw sets the flag when the data is ready).
#[inline]
pub fn gphy_ocp_read_cmd(addr: u32) -> u32 {
    addr << 15
}

/// True while a GPHY-OCP transaction is in flight (busy flag set). A write
/// completes when this goes false; a read completes when this goes true.
#[inline]
pub fn gphy_ocp_busy(reg_val: u32) -> bool {
    reg_val & GPHY_OCP_FLAG != 0
}

/// Extract the 16-bit data from a completed GPHY-OCP read.
#[inline]
pub fn gphy_ocp_read_data(reg_val: u32) -> u16 {
    (reg_val & 0xFFFF) as u16
}

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
    /// The `fw_info` 8-bit checksum over the whole blob is non-zero.
    BadChecksum,
    /// `fw_start`/`fw_len` point outside the blob or overlap the header.
    BadFwRegion,
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
///
/// ⚠️ **Format hazard (Phase 83 — before staging a real blob):** this path
/// validates with [`validate_firmware_header`] (the simple
/// `version[32] + fw_offset/fw_reg + raw __le32 payload` framing), but the
/// actual loader [`parse_rtl_fw`] / [`run_phy_action`] expects the real Linux
/// `rtl_nic` `fw_info` framing (`magic[4]` + `version[32]` + `fw_start@0x24` +
/// `fw_len@0x28` + 8-bit `chksum@0x2c`). The two are **incompatible**: fed a
/// real `rtl_nic/*.fw` blob, `resolve_firmware` reads the version/payload from
/// the wrong offsets and can report `Loaded`/`Corrupt` inconsistently with what
/// `parse_rtl_fw` actually accepts. This is currently latent because
/// `firmware_blob()` is hardwired to `None` in the r8125 driver. Before E.2
/// stages a blob, **unify the two** — make `resolve_firmware` validate via
/// `parse_rtl_fw` (or delete this legacy header path) so there is a single
/// source of truth for blob framing.
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

// ---------------------------------------------------------------------------
// Real `rtl_fw` `fw_info` parser + PHY-action interpreter (Track D.1 loader).
//
// The on-disk `rtl_nic/*.fw` blob (after decompression) is Linux's `fw_info`
// format:
//   magic:    u32       @ 0x00   (0 for the PHY/MAC-MCU patch blobs)
//   version:  [u8; 32]  @ 0x04   (NUL-padded ASCII, e.g. "rtl8125b-2_0.0.2 …")
//   fw_start: u32 LE    @ 0x24   (byte offset of the PHY-action code)
//   fw_len:   u32 LE    @ 0x28   (byte length of the code)
//   chksum:   u8        @ 0x2C   (8-bit checksum: sum of ALL blob bytes == 0)
//   if_is_fw: u8        @ 0x2D
//   pad:      [u8; 2]   @ 0x2E
// The code at `fw_start` is an array of `__le32` instructions interpreted by
// [`run_phy_action`] (mirrors Linux `rtl_fw_write_firmware`). Verified against
// the real `rtl8125b-2.fw` (sum-mod-256 == 0, fw_start 0x70, fw_len 0x320,
// 200 instructions).
// ---------------------------------------------------------------------------

/// Size of the `fw_info` header preceding the firmware code.
pub const RTL_FW_INFO_SIZE: usize = 48;

/// A parsed, checksum-validated `rtl_fw` image: the version string and the
/// `__le32` PHY-action code slice (still encoded; interpret via [`run_phy_action`]).
#[derive(Clone, Copy, Debug)]
pub struct RtlFwImage<'a> {
    /// NUL-padded version field (`version[32]`).
    pub version: &'a [u8],
    /// The PHY-action code bytes (`fw_len` bytes, a multiple of 4).
    pub code: &'a [u8],
}

impl RtlFwImage<'_> {
    /// Number of `__le32` PHY-action instructions in [`Self::code`].
    #[inline]
    pub fn instr_count(&self) -> usize {
        self.code.len() / 4
    }
}

/// Parse and checksum-validate a decompressed `rtl_nic` `fw_info` firmware blob.
///
/// Returns the version string and the PHY-action code slice. Never panics on a
/// malformed blob — every framing error maps to a [`FirmwareError`] so the
/// driver can degrade gracefully.
pub fn parse_rtl_fw(blob: &[u8]) -> Result<RtlFwImage<'_>, FirmwareError> {
    if blob.len() < RTL_FW_INFO_SIZE {
        return Err(FirmwareError::TooShort);
    }
    // 8-bit checksum: the `chksum` byte is set so the sum of every byte is 0.
    let sum = blob.iter().fold(0u8, |acc, &b| acc.wrapping_add(b));
    if sum != 0 {
        return Err(FirmwareError::BadChecksum);
    }
    let version = &blob[4..4 + RTL_FW_VER_SIZE];
    if version.iter().all(|&b| b == 0) {
        return Err(FirmwareError::EmptyVersion);
    }
    let fw_start = u32::from_le_bytes([blob[0x24], blob[0x25], blob[0x26], blob[0x27]]) as usize;
    let fw_len = u32::from_le_bytes([blob[0x28], blob[0x29], blob[0x2a], blob[0x2b]]) as usize;
    if fw_len == 0 || fw_len > RTL_FW_MAX_LEN || !fw_len.is_multiple_of(RTL_FW_INSTR_SIZE) {
        return Err(FirmwareError::UnalignedPayload);
    }
    // Code must sit after the header and inside the blob.
    let end = fw_start
        .checked_add(fw_len)
        .ok_or(FirmwareError::BadFwRegion)?;
    if fw_start < RTL_FW_INFO_SIZE || end > blob.len() {
        return Err(FirmwareError::BadFwRegion);
    }
    Ok(RtlFwImage {
        version,
        code: &blob[fw_start..end],
    })
}

/// Sink the PHY-action interpreter writes to. The driver implements this over
/// the real paged-MDIO / OCP register file; host tests implement it over a map.
///
/// `mdio_chg` selects the register space subsequent reads/writes target (the
/// firmware switches between the PHY MCU and the MAC MCU mid-stream). The
/// `target` value is the raw `data` field of a `PHY_MDIO_CHG` instruction.
pub trait PhyActionSink {
    /// Read PHY/MAC register `reg` (16-bit).
    fn read(&mut self, reg: u16) -> u16;
    /// Write `val` to PHY/MAC register `reg`.
    fn write(&mut self, reg: u16, val: u16);
    /// Switch the active MDIO target (PHY MCU vs MAC MCU).
    fn mdio_chg(&mut self, target: u16);
    /// Busy-wait `ms` milliseconds.
    fn delay_ms(&mut self, ms: u16);
}

/// PHY-action opcodes (top nibble of each `__le32`), per Linux `r8169_firmware.c`.
mod fw_op {
    pub const READ: u32 = 0x0;
    pub const DATA_OR: u32 = 0x1;
    pub const DATA_AND: u32 = 0x2;
    pub const BJMPN: u32 = 0x3;
    pub const MDIO_CHG: u32 = 0x4;
    pub const CLEAR_READCOUNT: u32 = 0x7;
    pub const WRITE: u32 = 0x8;
    pub const READCOUNT_EQ_SKIP: u32 = 0x9;
    pub const COMP_EQ_SKIPN: u32 = 0xa;
    pub const COMP_NEQ_SKIPN: u32 = 0xb;
    pub const WRITE_PREVIOUS: u32 = 0xc;
    pub const SKIPN: u32 = 0xd;
    pub const DELAY_MS: u32 = 0xe;
}

/// Maximum interpreter steps — a hard backstop against a corrupt blob whose
/// back-jumps never terminate. Real blobs run a few hundred steps.
pub const PHY_ACTION_MAX_STEPS: u32 = 200_000;

/// Run the PHY-action code against `sink` (mirrors Linux `rtl_fw_write_firmware`).
///
/// `code` is the `__le32` slice from [`parse_rtl_fw`]. Decoding per instruction:
/// `op = action >> 28`, `regno = (action >> 16) & 0x0fff`, `data = action & 0xffff`.
/// Returns the number of instructions executed (for diagnostics); bounded by
/// [`PHY_ACTION_MAX_STEPS`] so a malformed back-jump can never hang.
pub fn run_phy_action<S: PhyActionSink>(code: &[u8], sink: &mut S) -> u32 {
    let n = code.len() / 4;
    let at = |i: usize| -> u32 {
        u32::from_le_bytes([
            code[i * 4],
            code[i * 4 + 1],
            code[i * 4 + 2],
            code[i * 4 + 3],
        ])
    };
    let mut index: usize = 0;
    let mut predata: u16 = 0;
    let mut count: u32 = 0;
    let mut steps: u32 = 0;
    while index < n && steps < PHY_ACTION_MAX_STEPS {
        steps += 1;
        let action = at(index);
        let regno = ((action >> 16) & 0x0fff) as u16;
        let data = (action & 0xffff) as u16;
        match action >> 28 {
            fw_op::READ => {
                predata = sink.read(regno);
                count += 1;
                index += 1;
            }
            fw_op::DATA_OR => {
                predata |= data;
                index += 1;
            }
            fw_op::DATA_AND => {
                predata &= data;
                index += 1;
            }
            fw_op::BJMPN => {
                // Jump back `regno` instructions. Linux's PHY-action loop is
                // `for (index = 0; index < pa->size; )` with an EMPTY increment
                // clause — every opcode advances `index` itself (READ/WRITE do
                // `index += 1`; SKIPN does `index += regno + 1`), exactly as this
                // `while` loop does (there is no trailing `index += 1` after the
                // match). So Linux's `PHY_BJMPN: index -= regno` translates to a
                // bare `index - regno` here, with no `+1`. `saturating_sub`
                // clamps a malformed over-jump to 0; the step cap below still
                // bounds any resulting loop.
                index = index.saturating_sub(regno as usize);
            }
            fw_op::MDIO_CHG => {
                sink.mdio_chg(data);
                index += 1;
            }
            fw_op::CLEAR_READCOUNT => {
                count = 0;
                index += 1;
            }
            fw_op::WRITE => {
                sink.write(regno, data);
                index += 1;
            }
            fw_op::READCOUNT_EQ_SKIP => {
                index += if count == data as u32 { 2 } else { 1 };
            }
            fw_op::COMP_EQ_SKIPN => {
                if predata == data {
                    index += regno as usize;
                }
                index += 1;
            }
            fw_op::COMP_NEQ_SKIPN => {
                if predata != data {
                    index += regno as usize;
                }
                index += 1;
            }
            fw_op::WRITE_PREVIOUS => {
                sink.write(regno, predata);
                index += 1;
            }
            fw_op::SKIPN => {
                index += regno as usize + 1;
            }
            fw_op::DELAY_MS => {
                sink.delay_ms(data);
                index += 1;
            }
            // Unknown opcode — skip it rather than wedge (defensive).
            _ => index += 1,
        }
    }
    steps
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

    // --- D.1 loader: real fw_info parser + PHY-action interpreter ---

    /// Build a checksum-valid `fw_info` blob: 48-byte header, code immediately
    /// after it, and the `chksum` byte set so the whole-blob sum is 0.
    fn build_fw_info(version: &str, code: &[u32]) -> Vec<u8> {
        let mut b = vec![0u8; RTL_FW_INFO_SIZE];
        // magic stays 0 (bytes 0..4). version[32] at 0x04.
        let v = version.as_bytes();
        b[4..4 + v.len().min(RTL_FW_VER_SIZE)].copy_from_slice(&v[..v.len().min(RTL_FW_VER_SIZE)]);
        let fw_start = RTL_FW_INFO_SIZE as u32;
        let fw_len = (code.len() * 4) as u32;
        b[0x24..0x28].copy_from_slice(&fw_start.to_le_bytes());
        b[0x28..0x2c].copy_from_slice(&fw_len.to_le_bytes());
        // chksum placeholder at 0x2c == 0 for now; if_is_fw/pad stay 0.
        for &w in code {
            b.extend_from_slice(&w.to_le_bytes());
        }
        // Set chksum so the total sum is 0 mod 256.
        let sum = b.iter().fold(0u8, |a, &x| a.wrapping_add(x));
        b[0x2c] = b[0x2c].wrapping_sub(sum);
        b
    }

    #[test]
    fn parse_rtl_fw_accepts_valid_blob() {
        let blob = build_fw_info("rtl8125b-2_0.0.2", &[0x8000_1234, 0x4000_0001]);
        let img = parse_rtl_fw(&blob).expect("valid blob parses");
        assert_eq!(img.instr_count(), 2);
        assert!(img.version.starts_with(b"rtl8125b-2"));
        // Sum of all bytes is 0 (checksum holds).
        assert_eq!(blob.iter().fold(0u8, |a, &x| a.wrapping_add(x)), 0);
    }

    #[test]
    fn parse_rtl_fw_rejects_bad_checksum() {
        let mut blob = build_fw_info("rtl8125b-2", &[0x8000_0001]);
        blob[0x2c] = blob[0x2c].wrapping_add(1); // break the checksum
        assert_eq!(parse_rtl_fw(&blob).unwrap_err(), FirmwareError::BadChecksum);
    }

    #[test]
    fn parse_rtl_fw_rejects_short_and_bad_region() {
        assert_eq!(
            parse_rtl_fw(&[0u8; 8]).unwrap_err(),
            FirmwareError::TooShort
        );
        // fw_start/fw_len that run past the blob end.
        let mut blob = build_fw_info("v", &[0x8000_0001]);
        blob[0x28..0x2c].copy_from_slice(&0xFFFFu32.to_le_bytes()); // fw_len huge
        // Re-fix checksum so it fails on region, not checksum.
        blob[0x2c] = 0;
        let s = blob.iter().fold(0u8, |a, &x| a.wrapping_add(x));
        blob[0x2c] = blob[0x2c].wrapping_sub(s);
        assert!(matches!(
            parse_rtl_fw(&blob),
            Err(FirmwareError::BadFwRegion | FirmwareError::UnalignedPayload)
        ));
    }

    /// Mock sink recording writes and serving canned reads.
    struct MockPhy {
        writes: Vec<(u16, u16)>,
        reads: Vec<(u16, u16)>, // reg -> value to return
        mdio_target: u16,
        delays: u32,
    }
    impl PhyActionSink for MockPhy {
        fn read(&mut self, reg: u16) -> u16 {
            self.reads
                .iter()
                .find(|(r, _)| *r == reg)
                .map(|(_, v)| *v)
                .unwrap_or(0)
        }
        fn write(&mut self, reg: u16, val: u16) {
            self.writes.push((reg, val));
        }
        fn mdio_chg(&mut self, target: u16) {
            self.mdio_target = target;
        }
        fn delay_ms(&mut self, ms: u16) {
            self.delays += ms as u32;
        }
    }

    #[test]
    fn run_phy_action_executes_writes_and_mdio_chg() {
        // MDIO_CHG(1); WRITE reg=0x1f val=0xfc2; WRITE reg=0x28 val=0; DELAY 5ms.
        let code: Vec<u32> = vec![0x4000_0001, 0x801f_0fc2, 0x8028_0000, 0xe000_0005];
        let mut bytes = Vec::new();
        for w in &code {
            bytes.extend_from_slice(&w.to_le_bytes());
        }
        let mut phy = MockPhy {
            writes: Vec::new(),
            reads: Vec::new(),
            mdio_target: 0,
            delays: 0,
        };
        let steps = run_phy_action(&bytes, &mut phy);
        assert_eq!(steps, 4);
        assert_eq!(phy.mdio_target, 1);
        assert_eq!(phy.writes, vec![(0x1f, 0xfc2), (0x28, 0x0000)]);
        assert_eq!(phy.delays, 5);
    }

    #[test]
    fn run_phy_action_read_or_write_previous() {
        // READ reg=0x10 (returns 0x00f0); DATA_OR 0x0005; WRITE_PREVIOUS reg=0x10.
        let code: Vec<u32> = vec![0x0010_0000, 0x1000_0005, 0xc010_0000];
        let mut bytes = Vec::new();
        for w in &code {
            bytes.extend_from_slice(&w.to_le_bytes());
        }
        let mut phy = MockPhy {
            writes: Vec::new(),
            reads: vec![(0x10, 0x00f0)],
            mdio_target: 0,
            delays: 0,
        };
        run_phy_action(&bytes, &mut phy);
        // predata = 0x00f0 | 0x0005 = 0x00f5 written back to reg 0x10.
        assert_eq!(phy.writes, vec![(0x10, 0x00f5)]);
    }

    #[test]
    fn run_phy_action_bounded_against_infinite_backjump() {
        // idx0: WRITE reg=0 val=1; idx1: BJMPN regno=1.
        // Linux semantics (`index -= regno`): at idx1 the back-jump targets
        // idx1 - 1 = idx0, re-running the WRITE on every pass. The loop bounces
        // idx0→idx1→idx0… forever, so it must terminate on the step backstop
        // (not hang), and the WRITE runs many times (≈ cap/2 — two steps/pass).
        let code: Vec<u32> = vec![0x8000_0001, 0x3001_0000];
        let mut bytes = Vec::new();
        for w in &code {
            bytes.extend_from_slice(&w.to_le_bytes());
        }
        let mut phy = MockPhy {
            writes: Vec::new(),
            reads: Vec::new(),
            mdio_target: 0,
            delays: 0,
        };
        let steps = run_phy_action(&bytes, &mut phy);
        assert_eq!(steps, PHY_ACTION_MAX_STEPS); // hit the backstop, did not hang
        // The back-jump re-enters idx0 every pass, so the WRITE re-runs.
        assert!(phy.writes.len() > 1);
        assert!(phy.writes.iter().all(|&w| w == (0x0, 0x1)));
    }

    #[test]
    fn run_phy_action_backjump_targets_index_minus_regno() {
        // idx0: WRITE reg=0xa val=1
        // idx1: COMP_NEQ_SKIPN regno=2 data=0  (predata stays 0 → 0==0, no skip)
        // idx2: WRITE reg=0xb val=2
        // idx3: BJMPN regno=3 → Linux target = 3 - 3 = 0 (idx0), with no `+1`.
        // Each pass re-enters idx0, so BOTH writes repeat until the step cap —
        // proving the bare `index - regno` target. (The off-by-one `+1` impl
        // targeted idx1 and left (0xa,1) running exactly once.)
        let code: Vec<u32> = vec![0x800a_0001, 0xb002_0000, 0x800b_0002, 0x3003_0000];
        let mut bytes = Vec::new();
        for w in &code {
            bytes.extend_from_slice(&w.to_le_bytes());
        }
        let mut phy = MockPhy {
            writes: Vec::new(),
            reads: Vec::new(),
            mdio_target: 0,
            delays: 0,
        };
        run_phy_action(&bytes, &mut phy);
        // idx0 is re-entered every back-jump, so (0xa,1) and (0xb,2) both repeat.
        assert!(phy.writes.iter().filter(|&&w| w == (0xa, 0x1)).count() > 1);
        assert!(phy.writes.iter().filter(|&&w| w == (0xb, 0x2)).count() > 1);
    }

    #[test]
    fn run_phy_action_skipn_skips_forward() {
        // SKIPN regno=1 (skip next 1 + self); WRITE (skipped); WRITE reg=0x2 val=0x9.
        let code: Vec<u32> = vec![0xd001_0000, 0x8001_00aa, 0x8002_0009];
        let mut bytes = Vec::new();
        for w in &code {
            bytes.extend_from_slice(&w.to_le_bytes());
        }
        let mut phy = MockPhy {
            writes: Vec::new(),
            reads: Vec::new(),
            mdio_target: 0,
            delays: 0,
        };
        run_phy_action(&bytes, &mut phy);
        // SKIPN regno=1 advances index by 2, skipping the 0xaa write.
        assert_eq!(phy.writes, vec![(0x2, 0x9)]);
    }

    // --- GPHY-OCP PHY access (8125) ---

    #[test]
    fn gphy_ocp_page_base_resolution() {
        // Page 0 → standard base; non-zero page → page << 4.
        assert_eq!(ocp_base_for_page(0), OCP_STD_PHY_BASE);
        assert_eq!(ocp_base_for_page(0), 0xA400);
        assert_eq!(ocp_base_for_page(0xA44), 0xA440);
        assert_eq!(ocp_base_for_page(0xAC4), 0xAC40);
        assert_eq!(ocp_base_for_page(0xA43), 0xA430);
    }

    #[test]
    fn gphy_ocp_addr_standard_vs_paged() {
        // Standard page: BMCR (reg 0) maps straight to the base.
        assert_eq!(phy_ocp_addr(OCP_STD_PHY_BASE, 0x00), 0xA400);
        // Standard page: reg N → base + N*2 (no 0x10 offset).
        assert_eq!(phy_ocp_addr(OCP_STD_PHY_BASE, 0x02), 0xA404);
        // Non-standard page from the captured config: page 0xA44, reg 0x11.
        // base = 0xA440; reg -= 0x10 → 0x01; addr = 0xA440 + 2 = 0xA442.
        assert_eq!(phy_ocp_addr(ocp_base_for_page(0xA44), 0x11), 0xA442);
        // page 0xAC4, reg 0x13: base 0xAC40; (0x13-0x10)*2 = 6; addr 0xAC46.
        assert_eq!(phy_ocp_addr(ocp_base_for_page(0xAC4), 0x13), 0xAC46);
    }

    #[test]
    fn gphy_ocp_write_cmd_layout() {
        // BMCR (addr 0xA400) write of 0x9240. Linux: FLAG | (addr<<15) | data.
        // addr is even, so addr<<15 == (addr>>1)<<16 places the word address
        // in bits 30:16 and leaves bit 31 for the flag.
        let cmd = gphy_ocp_write_cmd(0xA400, 0x9240);
        assert_eq!(cmd & GPHY_OCP_FLAG, GPHY_OCP_FLAG); // busy/cmd set
        assert_eq!(cmd & 0xFFFF, 0x9240); // data in low 16
        assert_eq!((cmd >> 16) & 0x7FFF, 0xA400 >> 1); // word addr in 30:16
        assert_eq!(cmd, 0x8000_0000 | (0xA400u32 << 15) | 0x9240);
        // Sanity: fits in 32 bits and bit 31 is only the flag.
        assert_eq!(cmd, 0xD200_9240);
    }

    #[test]
    fn gphy_ocp_read_cmd_layout_and_busy() {
        // Read command: flag clear, word address in 30:16.
        let cmd = gphy_ocp_read_cmd(0xA400);
        assert_eq!(cmd & GPHY_OCP_FLAG, 0); // flag clear on issue
        assert_eq!((cmd >> 16) & 0x7FFF, 0xA400 >> 1);
        assert_eq!(cmd, 0x5200_0000);
        // Busy predicate keys off bit 31.
        assert!(gphy_ocp_busy(0x8000_1234));
        assert!(!gphy_ocp_busy(0x0000_1234));
        // Completed read: data is the low 16 bits of the register snapshot.
        assert_eq!(gphy_ocp_read_data(0x8000_ABCD), 0xABCD);
        assert_eq!(gphy_ocp_read_data(0x0000_5555), 0x5555);
    }

    #[test]
    fn gphy_ocp_high_mac_addr_fits_32_bits() {
        // A MAC-OCP address near the top of the range still fits (bit 31 free).
        let cmd = gphy_ocp_write_cmd(0xE092, 0x0004);
        assert_eq!(cmd & GPHY_OCP_FLAG, GPHY_OCP_FLAG);
        assert_eq!(cmd & 0xFFFF, 0x0004);
        assert_eq!((cmd >> 16) & 0x7FFF, 0xE092 >> 1);
    }
}
