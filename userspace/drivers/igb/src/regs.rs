//! Intel igb (82575/82576/I210/I211/I350/I354) register map — Phase 79 Track B.1.
//!
//! Offsets and bit layouts are cross-verified against the upstream Linux
//! driver `drivers/net/ethernet/intel/igb/e1000_regs.h` and `e1000_defines.h`.
//! igb shares the device-control / receive-address layout with the classic
//! e1000, but its **interrupt block moves to the EICR/EIMS/EIAC registers**
//! (a multi-vector design we drive single-vector for 1.0), and its descriptor
//! rings are the **advanced** read/write-back union (the `Advanced` impl of
//! `driver_runtime::NicDescriptors`).
//!
//! Everything here is a `pub const` so it is usable in `match` arms / `const`
//! contexts and host-testable without any MMIO.

/// Named BAR0 register offsets for the igb family (byte offsets into BAR0).
pub struct IgbRegs;

#[allow(dead_code)]
impl IgbRegs {
    /// Device Control.
    pub const CTRL: usize = 0x0000;
    /// Device Status.
    pub const STATUS: usize = 0x0008;
    /// Device Control Extended (`CTRL_EXT`).
    pub const CTRL_EXT: usize = 0x0018;

    // -- Legacy interrupt registers (kept for completeness; igb uses EICR). --
    /// Interrupt Cause Read (legacy ICR — read-to-clear).
    pub const ICR: usize = 0x01500;
    /// Interrupt Mask Set/Read (legacy IMS).
    pub const IMS: usize = 0x01508;
    /// Interrupt Mask Clear (legacy IMC).
    pub const IMC: usize = 0x01510;

    // -- Extended (MSI-X capable) interrupt block — the igb interrupt path. --
    /// Extended Interrupt Cause Read — read-to-clear when EIAME/auto-clear off.
    pub const EICR: usize = 0x01580;
    /// Extended Interrupt Cause Set (test-only write).
    pub const EICS: usize = 0x01520;
    /// Extended Interrupt Mask Set/Read — write 1 to a bit to enable a vector.
    pub const EIMS: usize = 0x01524;
    /// Extended Interrupt Mask Clear — write 1 to disable a vector.
    pub const EIMC: usize = 0x01528;
    /// Extended Interrupt Auto Clear — bits auto-cleared on EICR read.
    pub const EIAC: usize = 0x0152C;
    /// Extended Interrupt Auto Mask Enable.
    pub const EIAM: usize = 0x01530;
    /// General Purpose Interrupt Enable — maps causes to EICR bits.
    pub const GPIE: usize = 0x01514;
    /// Interrupt Vector Allocation Registers base (IVAR0..).
    pub const IVAR0: usize = 0x01700;
    /// Interrupt Vector Allocation — "other" (link/misc) causes.
    pub const IVAR_MISC: usize = 0x01740;

    // -- Receive path (queue 0). --
    /// Receive Control.
    pub const RCTL: usize = 0x00100;
    /// Split and Replication Receive Control, queue 0 (`SRRCTL0`).
    pub const SRRCTL0: usize = 0x0C00C;
    /// RX Descriptor Base Address Low, queue 0.
    pub const RDBAL0: usize = 0x0C000;
    /// RX Descriptor Base Address High, queue 0.
    pub const RDBAH0: usize = 0x0C004;
    /// RX Descriptor Ring Length, queue 0 (bytes; multiple of 128).
    pub const RDLEN0: usize = 0x0C008;
    /// RX Descriptor Head, queue 0 (hardware-owned).
    pub const RDH0: usize = 0x0C010;
    /// RX Descriptor Tail, queue 0 (software-owned).
    pub const RDT0: usize = 0x0C018;
    /// RX Descriptor Control, queue 0 (`RXDCTL0`) — bit 25 enables the queue.
    pub const RXDCTL0: usize = 0x0C028;

    // -- Transmit path (queue 0). --
    /// Transmit Control.
    pub const TCTL: usize = 0x00400;
    /// TX Descriptor Base Address Low, queue 0.
    pub const TDBAL0: usize = 0x0E000;
    /// TX Descriptor Base Address High, queue 0.
    pub const TDBAH0: usize = 0x0E004;
    /// TX Descriptor Ring Length, queue 0 (bytes; multiple of 128).
    pub const TDLEN0: usize = 0x0E008;
    /// TX Descriptor Head, queue 0 (hardware-owned).
    pub const TDH0: usize = 0x0E010;
    /// TX Descriptor Tail, queue 0 (software-owned).
    pub const TDT0: usize = 0x0E018;
    /// TX Descriptor Control, queue 0 (`TXDCTL0`) — bit 25 enables the queue.
    pub const TXDCTL0: usize = 0x0E028;

    // -- MAC address + multicast filter. --
    /// Receive Address Low 0 — first 4 bytes of the primary MAC.
    pub const RAL0: usize = 0x05400;
    /// Receive Address High 0 — last 2 bytes + AV (bit 31).
    pub const RAH0: usize = 0x05404;
    /// Multicast Table Array base (128 dwords).
    pub const MTA: usize = 0x05200;
    /// End of the Multicast Table Array (inclusive last dword).
    pub const MTA_END: usize = 0x053FC;
}

/// `CTRL` bits (shared layout with the classic e1000).
pub mod ctrl {
    /// Full-Duplex.
    pub const FD: u32 = 1 << 0;
    /// Set Link Up.
    pub const SLU: u32 = 1 << 6;
    /// Auto-Speed-Detect Enable.
    pub const ASDE: u32 = 1 << 5;
    /// Link Reset.
    pub const LRST: u32 = 1 << 3;
    /// PHY Reset.
    pub const PHY_RST: u32 = 1 << 31;
    /// Global device reset (self-clearing).
    pub const RST: u32 = 1 << 26;
}

/// `STATUS` bits.
pub mod status {
    /// Link Up.
    pub const LU: u32 = 1 << 1;
    /// PF reset done (igb global reset completion) — `STATUS.PF_RST_DONE`.
    pub const PF_RST_DONE: u32 = 1 << 21;
}

/// `RCTL` bits (shared layout with the classic e1000).
pub mod rctl {
    /// Receiver Enable.
    pub const EN: u32 = 1 << 1;
    /// Broadcast Accept Mode.
    pub const BAM: u32 = 1 << 15;
    /// Strip Ethernet CRC.
    pub const SECRC: u32 = 1 << 26;
    /// Buffer size 2048 (BSIZE=00, the advanced default for legacy-descriptor
    /// sizing; igb uses SRRCTL for the real packet-buffer sizing).
    pub const BSIZE_2048: u32 = 0;
}

/// `TCTL` bits (shared layout with the classic e1000).
pub mod tctl {
    /// Transmitter Enable.
    pub const EN: u32 = 1 << 1;
    /// Pad Short Packets.
    pub const PSP: u32 = 1 << 3;
    /// Collision Threshold shift.
    pub const CT_SHIFT: u32 = 4;
    /// Collision Distance shift.
    pub const COLD_SHIFT: u32 = 12;
}

/// `SRRCTL` (per-queue split/replication receive control) bits.
pub mod srrctl {
    /// Packet-buffer size in KB (bits 6:0). 2 == 2 KiB buffers.
    pub const BSIZEPKT_2K: u32 = 2;
    /// Descriptor type: advanced, one buffer (no header split) == 1 in bits
    /// 27:25.
    pub const DESCTYPE_ADV_ONEBUF: u32 = 1 << 25;
    /// Drop enable — drop packets when the ring is full rather than wedging.
    pub const DROP_EN: u32 = 1 << 31;
}

/// `RXDCTL` / `TXDCTL` (per-queue descriptor control) bits.
pub mod qdctl {
    /// Queue Enable.
    pub const ENABLE: u32 = 1 << 25;
}

/// `GPIE` (General Purpose Interrupt Enable) bits.
pub mod gpie {
    /// Non-selective (legacy/MSI) interrupt — required for single-vector INTx/MSI.
    pub const NSICR: u32 = 1 << 0;
    /// Multiple MSI-X. Left clear for single-vector 1.0.
    pub const MSIX_MODE: u32 = 1 << 4;
    /// EIMS auto-mask on assertion.
    pub const EIAME: u32 = 1 << 30;
    /// PBA support (write 1 to clear pending bit array on EIMS write).
    pub const PBA_SUPPORT: u32 = 1 << 31;
}

/// EICR / EIMS / EIMC / EIAC vector bits. For the single-vector 1.0 path the
/// driver routes RX queue 0 + TX queue 0 + the "other" (link) cause all onto
/// vector 0 via the IVAR registers, so a single EICR bit signals all events.
pub mod eicr {
    /// Vector 0 — the single MSI/INTx vector the 1.0 driver uses.
    pub const VEC0: u32 = 1 << 0;
}

/// Bounded spin for the self-clearing `CTRL.RST` bit.
pub const RESET_POLL_LIMIT: u32 = 2_000_000;

/// BAR0 register-window length for the igb family (128 KiB; the register file
/// extends past 0xE028 so the map must cover the queue-control block).
pub const IGB_BAR0_LEN: usize = 0x0002_0000;

/// BAR0 index — igb exposes its register file as BAR0.
pub const IGB_BAR0_INDEX: u8 = 0;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_offsets_match_linux_igb_regs() {
        // Cross-checked against drivers/net/ethernet/intel/igb/e1000_regs.h.
        assert_eq!(IgbRegs::CTRL, 0x0000);
        assert_eq!(IgbRegs::STATUS, 0x0008);
        assert_eq!(IgbRegs::EICR, 0x01580);
        assert_eq!(IgbRegs::EIMS, 0x01524);
        assert_eq!(IgbRegs::EIMC, 0x01528);
        assert_eq!(IgbRegs::EIAC, 0x0152C);
        assert_eq!(IgbRegs::GPIE, 0x01514);
        assert_eq!(IgbRegs::RDBAL0, 0x0C000);
        assert_eq!(IgbRegs::RDLEN0, 0x0C008);
        assert_eq!(IgbRegs::RDH0, 0x0C010);
        assert_eq!(IgbRegs::RDT0, 0x0C018);
        assert_eq!(IgbRegs::RXDCTL0, 0x0C028);
        assert_eq!(IgbRegs::TDBAL0, 0x0E000);
        assert_eq!(IgbRegs::TDLEN0, 0x0E008);
        assert_eq!(IgbRegs::TDT0, 0x0E018);
        assert_eq!(IgbRegs::TXDCTL0, 0x0E028);
        assert_eq!(IgbRegs::RAL0, 0x05400);
        assert_eq!(IgbRegs::RAH0, 0x05404);
    }

    #[test]
    fn ctrl_reset_is_bit_26_and_status_link_is_bit_1() {
        assert_eq!(ctrl::RST, 1 << 26);
        assert_eq!(status::LU, 1 << 1);
    }

    #[test]
    fn queue_enable_is_bit_25() {
        assert_eq!(qdctl::ENABLE, 1 << 25);
    }

    #[test]
    fn srrctl_advanced_onebuf_desctype_is_bit_25() {
        assert_eq!(srrctl::DESCTYPE_ADV_ONEBUF, 1 << 25);
        assert_eq!(srrctl::BSIZEPKT_2K, 2);
    }
}
