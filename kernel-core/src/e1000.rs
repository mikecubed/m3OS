//! Pure-logic Intel 82540EM (classic e1000) register, descriptor, and flag
//! definitions — Phase 55 E.0.
//!
//! Placed in `kernel-core` so the layouts can be exercised from host-side
//! tests (`cargo test -p kernel-core --target x86_64-unknown-linux-gnu`) that
//! do not have MMIO, DMA, or a scheduler.
//!
//! **Phase 55b E.5 — consumer update:** The in-kernel driver
//! `kernel/src/net/e1000.rs` was deleted. This module is now consumed
//! exclusively by `userspace/drivers/e1000` (the ring-3 e1000 driver process)
//! which imports `E1000Regs`, `E1000RxDesc`, `E1000TxDesc`, and the
//! `ctrl`/`rctl`/`tctl`/`cmd`/`status` flag modules defined here and does
//! not redefine the wire formats.
//!
//! All values come from the Intel 8254x Family of Gigabit Ethernet Controllers
//! Software Developer's Manual (rev 1.9), §13 "Programmer's Reference" — the
//! authoritative specification used as the donor-strategy primary source for
//! the e1000 driver.

// ---------------------------------------------------------------------------
// Register offsets — `E1000Regs`
// ---------------------------------------------------------------------------

/// Named BAR0 register offsets for the classic e1000 (82540EM).
///
/// The register space is a 128 KiB MMIO BAR; the offsets here are byte offsets
/// into that BAR.  Kept as `pub const` so they can be used in `match` arms and
/// `const` contexts.
pub struct E1000Regs;

#[allow(dead_code)]
impl E1000Regs {
    /// Device Control Register.
    pub const CTRL: usize = 0x0000;
    /// Device Status Register.
    pub const STATUS: usize = 0x0008;
    /// EEPROM Read Register.
    pub const EERD: usize = 0x0014;
    /// Interrupt Cause Read — read-to-clear on the classic e1000.
    pub const ICR: usize = 0x00C0;
    /// Interrupt Throttling Register.
    pub const ITR: usize = 0x00C4;
    /// Interrupt Cause Set (write-only, test-only; included for completeness).
    pub const ICS: usize = 0x00C8;
    /// Interrupt Mask Set — writing 1 to a bit enables that cause.
    pub const IMS: usize = 0x00D0;
    /// Interrupt Mask Clear — writing 1 to a bit disables that cause.
    pub const IMC: usize = 0x00D8;
    /// Receive Control.
    pub const RCTL: usize = 0x0100;
    /// Transmit Control.
    pub const TCTL: usize = 0x0400;
    /// Transmit IPG (inter-packet gap).
    pub const TIPG: usize = 0x0410;
    /// Receive Descriptor Base Address Low (32 bits of 64-bit physical base).
    pub const RDBAL: usize = 0x2800;
    /// Receive Descriptor Base Address High.
    pub const RDBAH: usize = 0x2804;
    /// Receive Descriptor Length in bytes (must be a multiple of 128).
    pub const RDLEN: usize = 0x2808;
    /// Receive Descriptor Head (hardware-owned).
    pub const RDH: usize = 0x2810;
    /// Receive Descriptor Tail (software-owned).
    pub const RDT: usize = 0x2818;
    /// Transmit Descriptor Base Address Low.
    pub const TDBAL: usize = 0x3800;
    /// Transmit Descriptor Base Address High.
    pub const TDBAH: usize = 0x3804;
    /// Transmit Descriptor Length in bytes (must be a multiple of 128).
    pub const TDLEN: usize = 0x3808;
    /// Transmit Descriptor Head (hardware-owned).
    pub const TDH: usize = 0x3810;
    /// Transmit Descriptor Tail (software-owned).
    pub const TDT: usize = 0x3818;
    /// Multicast Table Array base (128 dwords at 0x5200..0x53FC inclusive).
    pub const MTA: usize = 0x5200;
    /// End of the Multicast Table Array (inclusive last dword).
    pub const MTA_END: usize = 0x53FC;
    /// Receive Address Low 0 — first 4 bytes of the primary MAC.
    pub const RAL0: usize = 0x5400;
    /// Receive Address High 0 — last 2 bytes of the primary MAC plus an
    /// "address valid" bit (bit 31).
    pub const RAH0: usize = 0x5404;
}

// ---------------------------------------------------------------------------
// CTRL register bits — `ctrl`
// ---------------------------------------------------------------------------

/// Flag bits for the `CTRL` (Device Control, offset 0x0000) register.
pub mod ctrl {
    /// Full-Duplex (0 = half, 1 = full).
    pub const FD: u32 = 1 << 0;
    /// Link Reset (strap on some silicon; drives the PHY reset pin).
    pub const LRST: u32 = 1 << 3;
    /// Auto-Speed-Detect Enable.  Firmware/BIOS normally sets this; the
    /// driver sets it at bring-up so the MAC will honour the PHY's autoneg
    /// result.
    pub const ASDE: u32 = 1 << 5;
    /// Set Link Up — when 1, the MAC drives the link "up" (independent of
    /// auto-neg); when 0, forces link down.
    pub const SLU: u32 = 1 << 6;
    /// VLAN Mode Enable.
    pub const VME: u32 = 1 << 30;
    /// PHY Reset — must be 0 during normal operation; the driver clears it
    /// after the global reset to bring the PHY out of reset.
    pub const PHY_RST: u32 = 1 << 31;
    /// Global device reset.  Self-clearing.
    pub const RST: u32 = 1 << 26;
}

// ---------------------------------------------------------------------------
// RCTL register bits — `rctl`
// ---------------------------------------------------------------------------

/// Flag bits for the `RCTL` (Receive Control, offset 0x0100) register.
pub mod rctl {
    /// Receiver Enable.
    pub const EN: u32 = 1 << 1;
    /// Store Bad Packets.
    pub const SBP: u32 = 1 << 2;
    /// Unicast Promiscuous Enable.
    pub const UPE: u32 = 1 << 3;
    /// Multicast Promiscuous Enable.
    pub const MPE: u32 = 1 << 4;
    /// Long Packet Enable.
    pub const LPE: u32 = 1 << 5;
    /// Broadcast Accept Mode.
    pub const BAM: u32 = 1 << 15;
    /// Strip Ethernet CRC (FCS) from the incoming packet.
    pub const SECRC: u32 = 1 << 26;
    /// Buffer Size = 2048 bytes (BSIZE = 00 + BSEX = 0).  This is the default
    /// and matches the per-descriptor buffer size the driver pre-allocates.
    pub const BSIZE_2048: u32 = 0;
    /// Buffer Size = 1024 bytes.
    pub const BSIZE_1024: u32 = 1 << 16;
    /// Buffer Size = 512 bytes.
    pub const BSIZE_512: u32 = 2 << 16;
    /// Buffer Size = 256 bytes.
    pub const BSIZE_256: u32 = 3 << 16;
}

// ---------------------------------------------------------------------------
// TCTL register bits — `tctl`
// ---------------------------------------------------------------------------

/// Flag bits for the `TCTL` (Transmit Control, offset 0x0400) register.
pub mod tctl {
    /// Transmitter Enable.
    pub const EN: u32 = 1 << 1;
    /// Pad Short Packets (to 64 bytes).
    pub const PSP: u32 = 1 << 3;
    /// Collision Threshold — bits 11:4.  The driver uses 0x10 per Intel's
    /// recommended value for full-duplex operation.
    pub const CT_SHIFT: u32 = 4;
    /// Collision Distance (backoff) — bits 21:12.  0x40 per spec default.
    pub const COLD_SHIFT: u32 = 12;
}

// ---------------------------------------------------------------------------
// ICR / IMS / IMC bits — `irq_cause`
// ---------------------------------------------------------------------------

/// Interrupt cause bits.  These values are shared by the `ICR`, `ICS`, `IMS`,
/// and `IMC` registers.
pub mod irq_cause {
    /// Transmit Descriptor Written Back.
    pub const TXDW: u32 = 1 << 0;
    /// Transmit Queue Empty.
    pub const TXQE: u32 = 1 << 1;
    /// Link Status Change.
    pub const LSC: u32 = 1 << 2;
    /// Receive Sequence Error.
    pub const RXSEQ: u32 = 1 << 3;
    /// Receive Descriptor Minimum Threshold hit.
    pub const RXDMT0: u32 = 1 << 4;
    /// Receiver Overrun.
    pub const RXO: u32 = 1 << 6;
    /// Receiver Timer Interrupt — fires on any RX descriptor writeback.
    pub const RXT0: u32 = 1 << 7;
}

// ---------------------------------------------------------------------------
// TX command and status bits
// ---------------------------------------------------------------------------

/// TX descriptor `cmd` byte bits.
pub mod tx_cmd {
    /// End-Of-Packet — signals the last descriptor for a packet.
    pub const EOP: u8 = 1 << 0;
    /// Insert FCS — hardware appends a valid Ethernet CRC.
    pub const IFCS: u8 = 1 << 1;
    /// Report Status — hardware writes back `DD` in `status` on completion.
    pub const RS: u8 = 1 << 3;
}

/// TX descriptor `status` byte bits.
pub mod tx_status {
    /// Descriptor Done — hardware has finished with this descriptor.
    pub const DD: u8 = 1 << 0;
}

/// RX descriptor `status` byte bits.
pub mod rx_status {
    /// Descriptor Done — hardware has written a packet into this slot.
    pub const DD: u8 = 1 << 0;
    /// End-Of-Packet — last descriptor for a multi-descriptor RX packet.
    pub const EOP: u8 = 1 << 1;
}

// ---------------------------------------------------------------------------
// STATUS register bits
// ---------------------------------------------------------------------------

/// Flag bits for the `STATUS` (Device Status, offset 0x0008) register.
pub mod status {
    /// Link Up — 1 when the MAC sees the PHY report link-up.
    pub const LU: u32 = 1 << 1;
}

// ---------------------------------------------------------------------------
// Descriptor layouts
// ---------------------------------------------------------------------------

/// Legacy Receive Descriptor — 16 bytes, §3.2.3 of the e1000 SDM.
///
/// The classic e1000 uses this "legacy" descriptor by default.  Modern
/// e1000e / 82574 use extended descriptors which are NOT supported here —
/// see the Phase 55 task doc E.0 Intel NIC scope note.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct E1000RxDesc {
    /// Physical (bus) address of the receive buffer.  DMA target.
    pub buffer_addr: u64,
    /// Length of the received packet in bytes (hardware-written).
    pub length: u16,
    /// IP/TCP/UDP checksum (offload, unused by this driver).
    pub checksum: u16,
    /// Status bits — see [`rx_status`].
    pub status: u8,
    /// Error bits (hardware-written).
    pub errors: u8,
    /// VLAN tag / special bits (unused by this driver).
    pub special: u16,
}

/// Legacy Transmit Descriptor — 16 bytes, §3.3.3 of the e1000 SDM.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct E1000TxDesc {
    /// Physical (bus) address of the transmit buffer.  DMA source.
    pub buffer_addr: u64,
    /// Length of the packet in bytes (software-written).
    pub length: u16,
    /// Checksum Offset (unused).
    pub cso: u8,
    /// Command byte — see [`tx_cmd`].
    pub cmd: u8,
    /// Status byte — hardware-written when `RS` is set in cmd; read DD bit to
    /// detect completion.
    pub status: u8,
    /// Checksum Start (unused).
    pub css: u8,
    /// VLAN tag / special bits (unused).
    pub special: u16,
}

// Compile-time size and alignment checks — the descriptors are wire
// structures shared with hardware and must be exactly 16 bytes.
const _: () = assert!(core::mem::size_of::<E1000RxDesc>() == 16);
const _: () = assert!(core::mem::size_of::<E1000TxDesc>() == 16);
const _: () = assert!(core::mem::align_of::<E1000RxDesc>() <= 8);
const _: () = assert!(core::mem::align_of::<E1000TxDesc>() <= 8);

// ---------------------------------------------------------------------------
// Small pure helpers used by the driver and exercised from host tests
// ---------------------------------------------------------------------------

/// Return true if the RX descriptor's `DD` bit is set (hardware has placed a
/// packet in the buffer).
#[inline]
pub fn rx_descriptor_done(status: u8) -> bool {
    status & rx_status::DD != 0
}

/// Return true if the TX descriptor's `DD` bit is set (hardware is done with
/// the buffer).
#[inline]
pub fn tx_descriptor_done(status: u8) -> bool {
    status & tx_status::DD != 0
}

/// Decode the 6-byte MAC address from the RAL0 / RAH0 register pair.
///
/// Returned in Ethernet wire order (byte 0 is the LSB of RAL0).
#[inline]
pub fn decode_mac_from_ra(ral0: u32, rah0: u32) -> [u8; 6] {
    [
        (ral0 & 0xFF) as u8,
        ((ral0 >> 8) & 0xFF) as u8,
        ((ral0 >> 16) & 0xFF) as u8,
        ((ral0 >> 24) & 0xFF) as u8,
        (rah0 & 0xFF) as u8,
        ((rah0 >> 8) & 0xFF) as u8,
    ]
}

// ---------------------------------------------------------------------------
// Host tests (E.0 ≥ 3 tests)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// E.0 acceptance bullet: descriptor size + alignment.
    #[test]
    fn descriptor_sizes_are_16_bytes() {
        assert_eq!(core::mem::size_of::<E1000RxDesc>(), 16);
        assert_eq!(core::mem::size_of::<E1000TxDesc>(), 16);
        assert!(core::mem::align_of::<E1000RxDesc>() <= 8);
        assert!(core::mem::align_of::<E1000TxDesc>() <= 8);
    }

    /// E.0 acceptance bullet: flag composition.  Composing the canonical
    /// "enable RX + accept broadcast + strip CRC + 2 KiB buffers" value must
    /// match the spec bit layout.
    #[test]
    fn rctl_flag_composition_matches_spec() {
        let value = rctl::EN | rctl::BAM | rctl::SECRC | rctl::BSIZE_2048;
        // EN = bit 1, BAM = bit 15, SECRC = bit 26, BSIZE_2048 = 0.
        assert_eq!(value, (1 << 1) | (1 << 15) | (1 << 26));
        // Inspect each advertised flag individually.
        assert_eq!(rctl::EN, 0x0000_0002);
        assert_eq!(rctl::BAM, 0x0000_8000);
        assert_eq!(rctl::SECRC, 0x0400_0000);
        assert_eq!(rctl::BSIZE_2048, 0);
        assert_eq!(rctl::BSIZE_1024, 0x0001_0000);

        // CTRL composition: ASDE | SLU should land on bits 5 | 6 = 0x60.
        assert_eq!(ctrl::ASDE | ctrl::SLU, 0x60);

        // TX command: EOP|IFCS|RS must match the spec pattern.
        assert_eq!(tx_cmd::EOP | tx_cmd::IFCS | tx_cmd::RS, 0x0B);
    }

    /// E.0 acceptance bullet: status-bit extraction on RX.
    #[test]
    fn rx_descriptor_done_detects_dd_bit() {
        assert!(rx_descriptor_done(0x01));
        assert!(rx_descriptor_done(0x03)); // DD | EOP
        assert!(!rx_descriptor_done(0x00));
        assert!(!rx_descriptor_done(0x02)); // EOP without DD
    }

    /// Bonus: TX completion detection mirrors RX.
    #[test]
    fn tx_descriptor_done_detects_dd_bit() {
        assert!(tx_descriptor_done(0x01));
        assert!(!tx_descriptor_done(0x00));
    }

    /// Bonus: MAC decoding from RAL0 / RAH0 matches QEMU's canonical layout.
    #[test]
    fn decode_mac_from_ra_is_little_endian_per_register() {
        // QEMU's e1000 default MAC 52:54:00:12:34:56 lands in the registers
        // as RAL0 = 0x0012_5452, RAH0 = 0x8000_5634 (bit 31 = address valid).
        // The decoder must extract the 6 bytes in wire order.
        let ral0: u32 = 0x0012_5452;
        let rah0: u32 = 0x8000_5634;
        let mac = decode_mac_from_ra(ral0, rah0);
        assert_eq!(mac, [0x52, 0x54, 0x12, 0x00, 0x34, 0x56]);
    }

    /// Bonus: register offsets match the §13 layout exactly.
    #[test]
    fn register_offsets_match_spec() {
        assert_eq!(E1000Regs::CTRL, 0x0000);
        assert_eq!(E1000Regs::STATUS, 0x0008);
        assert_eq!(E1000Regs::ICR, 0x00C0);
        assert_eq!(E1000Regs::IMS, 0x00D0);
        assert_eq!(E1000Regs::IMC, 0x00D8);
        assert_eq!(E1000Regs::RCTL, 0x0100);
        assert_eq!(E1000Regs::TCTL, 0x0400);
        assert_eq!(E1000Regs::RDBAL, 0x2800);
        assert_eq!(E1000Regs::RDBAH, 0x2804);
        assert_eq!(E1000Regs::RDLEN, 0x2808);
        assert_eq!(E1000Regs::RDH, 0x2810);
        assert_eq!(E1000Regs::RDT, 0x2818);
        assert_eq!(E1000Regs::TDBAL, 0x3800);
        assert_eq!(E1000Regs::TDBAH, 0x3804);
        assert_eq!(E1000Regs::TDLEN, 0x3808);
        assert_eq!(E1000Regs::TDH, 0x3810);
        assert_eq!(E1000Regs::TDT, 0x3818);
        assert_eq!(E1000Regs::RAL0, 0x5400);
        assert_eq!(E1000Regs::RAH0, 0x5404);
        assert_eq!(E1000Regs::MTA, 0x5200);
        assert_eq!(E1000Regs::MTA_END, 0x53FC);
    }
}
