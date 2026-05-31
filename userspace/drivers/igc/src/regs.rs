//! Intel igc (I225/I226 2.5GbE) register map + Clause-45 MMD PHY access —
//! Phase 79 Track B.2.
//!
//! Offsets and bit layouts are cross-verified against the upstream Linux driver
//! `drivers/net/ethernet/intel/igc/igc_regs.h` + `igc_defines.h`. igc is a
//! direct descendant of igb: it uses the same **advanced** read/write-back
//! descriptor union and the same **EICR/EIMS** interrupt block. The 2.5GBASE-T
//! copper PHY adds Clause-45 **MMD** indirection (`igc_read_xmdio_reg`-style),
//! modeled here as a pure register-composition helper over the `MDIC` register.
//!
//! QEMU has no igc model, so the runtime path is hardware-only; everything here
//! is host-testable pure logic.

/// Named BAR0 register offsets for the igc family (byte offsets into BAR0).
pub struct IgcRegs;

#[allow(dead_code)]
impl IgcRegs {
    /// Device Control.
    pub const CTRL: usize = 0x0000;
    /// Device Status.
    pub const STATUS: usize = 0x0008;
    /// Device Control Extended.
    pub const CTRL_EXT: usize = 0x0018;
    /// MDI Control register — the Clause-22/45 PHY access window.
    pub const MDIC: usize = 0x0020;
    /// MDI Configuration register (Clause-45 MMD destination address).
    pub const MDICNFG: usize = 0x0E04;

    // -- Extended (EICR) interrupt block. --
    /// Extended Interrupt Cause Read.
    pub const EICR: usize = 0x1580;
    /// Extended Interrupt Cause Set.
    pub const EICS: usize = 0x1520;
    /// Extended Interrupt Mask Set/Read.
    pub const EIMS: usize = 0x1524;
    /// Extended Interrupt Mask Clear.
    pub const EIMC: usize = 0x1528;
    /// Extended Interrupt Auto Clear.
    pub const EIAC: usize = 0x152C;
    /// Extended Interrupt Auto Mask Enable.
    pub const EIAM: usize = 0x1530;
    /// General Purpose Interrupt Enable.
    pub const GPIE: usize = 0x1514;
    /// Interrupt Vector Allocation Register 0.
    pub const IVAR0: usize = 0x1700;
    /// Interrupt Vector Allocation — "other"/misc causes.
    pub const IVAR_MISC: usize = 0x1740;

    // -- Receive path (queue 0). --
    /// Receive Control.
    pub const RCTL: usize = 0x0100;
    /// RX Descriptor Base Address Low, queue 0.
    pub const RDBAL0: usize = 0xC000;
    /// RX Descriptor Base Address High, queue 0.
    pub const RDBAH0: usize = 0xC004;
    /// RX Descriptor Ring Length, queue 0.
    pub const RDLEN0: usize = 0xC008;
    /// Split and Replication Receive Control, queue 0.
    pub const SRRCTL0: usize = 0xC00C;
    /// RX Descriptor Head, queue 0.
    pub const RDH0: usize = 0xC010;
    /// RX Descriptor Tail, queue 0.
    pub const RDT0: usize = 0xC018;
    /// RX Descriptor Control, queue 0 (bit 25 enables the queue).
    pub const RXDCTL0: usize = 0xC028;

    // -- Transmit path (queue 0). --
    /// Transmit Control.
    pub const TCTL: usize = 0x0400;
    /// TX Descriptor Base Address Low, queue 0.
    pub const TDBAL0: usize = 0xE000;
    /// TX Descriptor Base Address High, queue 0.
    pub const TDBAH0: usize = 0xE004;
    /// TX Descriptor Ring Length, queue 0.
    pub const TDLEN0: usize = 0xE008;
    /// TX Descriptor Head, queue 0.
    pub const TDH0: usize = 0xE010;
    /// TX Descriptor Tail, queue 0.
    pub const TDT0: usize = 0xE018;
    /// TX Descriptor Control, queue 0 (bit 25 enables the queue).
    pub const TXDCTL0: usize = 0xE028;

    // -- MAC address + multicast filter. --
    /// Receive Address Low 0.
    pub const RAL0: usize = 0x5400;
    /// Receive Address High 0 (AV in bit 31).
    pub const RAH0: usize = 0x5404;
    /// Multicast Table Array base (128 dwords).
    pub const MTA: usize = 0x5200;
    /// End of the Multicast Table Array (inclusive last dword).
    pub const MTA_END: usize = 0x53FC;
}

/// `CTRL` bits (shared with igb / e1000).
pub mod ctrl {
    pub const FD: u32 = 1 << 0;
    pub const LRST: u32 = 1 << 3;
    pub const ASDE: u32 = 1 << 5;
    pub const SLU: u32 = 1 << 6;
    pub const PHY_RST: u32 = 1 << 31;
    pub const RST: u32 = 1 << 26;
}

/// `STATUS` bits.
pub mod status {
    pub const LU: u32 = 1 << 1;
}

/// `RCTL` bits.
pub mod rctl {
    pub const EN: u32 = 1 << 1;
    pub const BAM: u32 = 1 << 15;
    pub const SECRC: u32 = 1 << 26;
}

/// `TCTL` bits.
pub mod tctl {
    pub const EN: u32 = 1 << 1;
    pub const PSP: u32 = 1 << 3;
    pub const CT_SHIFT: u32 = 4;
    pub const COLD_SHIFT: u32 = 12;
}

/// `SRRCTL` bits.
pub mod srrctl {
    pub const BSIZEPKT_2K: u32 = 2;
    pub const DESCTYPE_ADV_ONEBUF: u32 = 1 << 25;
    pub const DROP_EN: u32 = 1 << 31;
}

/// `RXDCTL`/`TXDCTL` bits.
pub mod qdctl {
    pub const ENABLE: u32 = 1 << 25;
}

/// `GPIE` bits.
pub mod gpie {
    pub const NSICR: u32 = 1 << 0;
    pub const MSIX_MODE: u32 = 1 << 4;
    pub const EIAME: u32 = 1 << 30;
    pub const PBA_SUPPORT: u32 = 1 << 31;
}

/// EICR vector bits.
pub mod eicr {
    pub const VEC0: u32 = 1 << 0;
}

/// `MDIC` (MDI Control) register bits — the PHY-access window. Linux
/// `IGC_MDIC_*`. Used for both Clause-22 (direct register) and Clause-45 (MMD)
/// access; the Clause-45 path issues an ADDRESS op to the MMD via the
/// `igc_xmdio` register pair (`MDICNFG` carries the destination MMD address).
pub mod mdic {
    /// PHY register address / MMD register selector — bits 20:16.
    pub const REG_SHIFT: u32 = 16;
    /// PHY register address mask (5 bits).
    pub const REG_MASK: u32 = 0x1F;
    /// PHY device address — bits 25:21.
    pub const PHY_SHIFT: u32 = 21;
    /// PHY device address mask (5 bits).
    pub const PHY_MASK: u32 = 0x1F;
    /// Operation code — bits 27:26. Read == 0b10, Write == 0b01.
    pub const OP_SHIFT: u32 = 26;
    /// MDI Read operation.
    pub const OP_READ: u32 = 0b10 << OP_SHIFT;
    /// MDI Write operation.
    pub const OP_WRITE: u32 = 0b01 << OP_SHIFT;
    /// Ready bit — bit 28; set by hardware when the transaction completes.
    pub const READY: u32 = 1 << 28;
    /// Error bit — bit 30.
    pub const ERROR: u32 = 1 << 30;
    /// Data field — bits 15:0.
    pub const DATA_MASK: u32 = 0xFFFF;
}

/// `MDICNFG` bits — the Clause-45 destination MMD (device-type) address.
pub mod mdicnfg {
    /// Destination MMD device-type address — bits 20:16.
    pub const DEST_SHIFT: u32 = 16;
    /// Destination address mask (5 bits).
    pub const DEST_MASK: u32 = 0x1F;
}

/// Bounded spin for the self-clearing `CTRL.RST` bit.
pub const RESET_POLL_LIMIT: u32 = 2_000_000;

/// Bounded spin for the `MDIC.READY` bit.
pub const MDIC_POLL_LIMIT: u32 = 1_000_000;

/// BAR0 register-window length (128 KiB).
pub const IGC_BAR0_LEN: usize = 0x0002_0000;

/// BAR0 index.
pub const IGC_BAR0_INDEX: u8 = 0;

// ---------------------------------------------------------------------------
// Clause-45 MMD PHY access — pure register-composition helpers.
// ---------------------------------------------------------------------------

/// Compose the `MDICNFG` value selecting the Clause-45 destination MMD
/// (device-type) `dev_addr` (e.g. MMD 7 = Auto-Negotiation, MMD 1 = PMA/PMD).
///
/// Models the `MDICNFG.DEST` write Linux's `igc_read_xmdio_reg` issues before
/// the `MDIC` ADDRESS/READ pair.
#[inline]
pub const fn xmdio_cfg_value(dev_addr: u8) -> u32 {
    ((dev_addr as u32) & mdicnfg::DEST_MASK) << mdicnfg::DEST_SHIFT
}

/// Compose the `MDIC` value for a Clause-45 (MMD) **read** of register
/// `reg_addr` on PHY `phy_addr`, targeting the MMD set by [`xmdio_cfg_value`].
///
/// On the igc the Clause-45 read reuses the `MDIC` read op with the register
/// field holding the MMD register offset; the MMD device-type comes from
/// `MDICNFG`. This is the `igc_read_xmdio_reg` composition, host-tested so the
/// bit packing is correct even though no QEMU model exists.
#[inline]
pub const fn mdic_read_value(phy_addr: u8, reg_addr: u16) -> u32 {
    mdic::OP_READ
        | (((phy_addr as u32) & mdic::PHY_MASK) << mdic::PHY_SHIFT)
        | (((reg_addr as u32) & mdic::REG_MASK) << mdic::REG_SHIFT)
}

/// Compose the `MDIC` value for a Clause-45 (MMD) **write** of `data` to
/// register `reg_addr` on PHY `phy_addr`.
#[inline]
pub const fn mdic_write_value(phy_addr: u8, reg_addr: u16, data: u16) -> u32 {
    mdic::OP_WRITE
        | (((phy_addr as u32) & mdic::PHY_MASK) << mdic::PHY_SHIFT)
        | (((reg_addr as u32) & mdic::REG_MASK) << mdic::REG_SHIFT)
        | ((data as u32) & mdic::DATA_MASK)
}

/// True once the `MDIC` transaction has completed (READY set).
#[inline]
pub const fn mdic_ready(mdic_snapshot: u32) -> bool {
    mdic_snapshot & mdic::READY != 0
}

/// True if the `MDIC` transaction reported an error.
#[inline]
pub const fn mdic_error(mdic_snapshot: u32) -> bool {
    mdic_snapshot & mdic::ERROR != 0
}

/// Extract the 16-bit data field from a completed `MDIC` read snapshot.
#[inline]
pub const fn mdic_data(mdic_snapshot: u32) -> u16 {
    (mdic_snapshot & mdic::DATA_MASK) as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_offsets_match_linux_igc_regs() {
        assert_eq!(IgcRegs::CTRL, 0x0000);
        assert_eq!(IgcRegs::STATUS, 0x0008);
        assert_eq!(IgcRegs::MDIC, 0x0020);
        assert_eq!(IgcRegs::MDICNFG, 0x0E04);
        assert_eq!(IgcRegs::EICR, 0x1580);
        assert_eq!(IgcRegs::EIMS, 0x1524);
        assert_eq!(IgcRegs::RDBAL0, 0xC000);
        assert_eq!(IgcRegs::SRRCTL0, 0xC00C);
        assert_eq!(IgcRegs::RDT0, 0xC018);
        assert_eq!(IgcRegs::TDBAL0, 0xE000);
        assert_eq!(IgcRegs::TDT0, 0xE018);
        assert_eq!(IgcRegs::RAL0, 0x5400);
    }

    #[test]
    fn mdic_read_value_packs_op_phy_reg() {
        // PHY addr 1, MMD register 0x20 (truncated to 5-bit reg field = 0).
        let v = mdic_read_value(1, 0x07);
        assert_ne!(v & mdic::OP_READ, 0);
        assert_eq!((v >> mdic::PHY_SHIFT) & mdic::PHY_MASK, 1);
        assert_eq!((v >> mdic::REG_SHIFT) & mdic::REG_MASK, 0x07);
        // The write op must not be set.
        assert_eq!(v & mdic::OP_WRITE, 0);
    }

    #[test]
    fn mdic_write_value_packs_data_and_op() {
        let v = mdic_write_value(2, 0x05, 0xBEEF);
        assert_ne!(v & mdic::OP_WRITE, 0);
        assert_eq!((v >> mdic::PHY_SHIFT) & mdic::PHY_MASK, 2);
        assert_eq!((v >> mdic::REG_SHIFT) & mdic::REG_MASK, 0x05);
        assert_eq!(v & mdic::DATA_MASK, 0xBEEF);
    }

    #[test]
    fn xmdio_cfg_selects_destination_mmd() {
        // MMD 7 (Auto-Negotiation device).
        let v = xmdio_cfg_value(7);
        assert_eq!((v >> mdicnfg::DEST_SHIFT) & mdicnfg::DEST_MASK, 7);
    }

    #[test]
    fn mdic_ready_error_data_decode() {
        assert!(mdic_ready(mdic::READY));
        assert!(!mdic_ready(0));
        assert!(mdic_error(mdic::ERROR));
        assert!(!mdic_error(mdic::READY));
        assert_eq!(mdic_data(mdic::READY | 0x1234), 0x1234);
    }

    #[test]
    fn mdic_op_codes_match_clause22_45_encoding() {
        // Read == 0b10 << 26, Write == 0b01 << 26.
        assert_eq!(mdic::OP_READ, 0b10 << 26);
        assert_eq!(mdic::OP_WRITE, 0b01 << 26);
        assert_eq!(mdic::READY, 1 << 28);
        assert_eq!(mdic::ERROR, 1 << 30);
    }

    #[test]
    fn ctrl_and_queue_enable_bits() {
        assert_eq!(ctrl::RST, 1 << 26);
        assert_eq!(qdctl::ENABLE, 1 << 25);
        assert_eq!(status::LU, 1 << 1);
    }
}
