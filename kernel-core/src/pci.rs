//! Pure-logic PCI data structures and decoders (host-testable).
//!
//! This module contains the format-level definitions used by the kernel PCI
//! subsystem:
//!
//! * [`McfgEntry`] — a single MCFG ACPI table entry (Enhanced Configuration
//!   Access Mechanism, ECAM, allocation) describing the MMIO base for a
//!   segment group and bus range.
//! * [`parse_mcfg`] — reads the ACPI MCFG table body (after the standard
//!   44-byte SDT header + 8 bytes of reserved) and returns the embedded
//!   allocations. Pure, host-testable.
//! * [`ecam_offset`] / [`mcfg_find_base`] — ECAM address math.
//! * PCI capability list iteration (`capability_walk`) and MSI/MSI-X layout
//!   offsets — the kernel side wraps these with actual MMIO reads.
//!
//! Nothing in this module touches hardware — see `kernel/src/pci/mod.rs` and
//! `kernel/src/acpi/mod.rs` for the MMIO wrappers.

// The PCI Memory-Mapped Configuration Space table (signature `MCFG`) carries
// one or more allocation entries after the SDT header and 8 reserved bytes.
// Each allocation is 16 bytes: base address (u64), PCI segment group (u16),
// start bus (u8), end bus (u8), reserved (u32). See PCI Firmware Spec §4.

/// Size of the per-allocation entry in the MCFG table body.
pub const MCFG_ENTRY_SIZE: usize = 16;

/// Byte offset from the start of the MCFG table at which the allocation
/// entries begin: 36 bytes of common SDT header, then 8 bytes of reserved.
pub const MCFG_ENTRIES_OFFSET: usize = 36 + 8;

/// A single MCFG allocation: one ECAM-mapped region covering `[start_bus ..=
/// end_bus]` of `segment_group`, based at `base_address`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct McfgEntry {
    /// Physical base address of the ECAM region (16 MiB per bus).
    pub base_address: u64,
    /// PCI segment group number.
    pub segment_group: u16,
    /// First bus number covered by this allocation.
    pub start_bus: u8,
    /// Last bus number covered by this allocation (inclusive).
    pub end_bus: u8,
}

impl McfgEntry {
    /// Parse a single 16-byte entry.  Returns `None` if `bytes.len() < 16`.
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < MCFG_ENTRY_SIZE {
            return None;
        }
        let base_address = u64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]);
        let segment_group = u16::from_le_bytes([bytes[8], bytes[9]]);
        let start_bus = bytes[10];
        let end_bus = bytes[11];
        // bytes[12..16] reserved
        Some(Self {
            base_address,
            segment_group,
            start_bus,
            end_bus,
        })
    }

    /// Returns `true` if this entry covers `(segment_group, bus)`.
    pub fn covers(&self, segment_group: u16, bus: u8) -> bool {
        self.segment_group == segment_group && bus >= self.start_bus && bus <= self.end_bus
    }

    /// Returns the physical address of the extended configuration space for
    /// `(bus, device, function, offset)` under this allocation.  The caller
    /// must have verified [`Self::covers`] first.  `offset` must be < 4096.
    pub fn ecam_address(&self, bus: u8, device: u8, function: u8, offset: u16) -> u64 {
        self.base_address + ecam_offset(self.start_bus, bus, device, function, offset)
    }
}

/// Byte offset within a MCFG allocation for a BDF/offset tuple.
///
/// ECAM layout: `(bus - start_bus) << 20 | device << 15 | function << 12 |
/// offset`. Each bus occupies 1 MiB, each device 32 KiB, each function 4 KiB.
pub fn ecam_offset(start_bus: u8, bus: u8, device: u8, function: u8, offset: u16) -> u64 {
    let bus_off = (bus as u64).wrapping_sub(start_bus as u64);
    (bus_off << 20) | ((device as u64) << 15) | ((function as u64) << 12) | (offset as u64 & 0xFFF)
}

/// Parse the MCFG table body and fill `out` with as many allocations as fit.
/// Returns the number of entries parsed.
///
/// `table_bytes` must start at the beginning of the MCFG SDT header (i.e. the
/// full table including the 36-byte `AcpiSdtHeader`). The table's `length`
/// field has already been validated by the caller (see kernel-side
/// `validate_sdt`).
pub fn parse_mcfg(table_bytes: &[u8], out: &mut [McfgEntry]) -> usize {
    if table_bytes.len() < MCFG_ENTRIES_OFFSET {
        return 0;
    }
    let entries_region = &table_bytes[MCFG_ENTRIES_OFFSET..];
    let mut count = 0;
    let mut off = 0;
    while off + MCFG_ENTRY_SIZE <= entries_region.len() && count < out.len() {
        if let Some(entry) = McfgEntry::from_bytes(&entries_region[off..off + MCFG_ENTRY_SIZE]) {
            out[count] = entry;
            count += 1;
        }
        off += MCFG_ENTRY_SIZE;
    }
    count
}

/// Find the MCFG allocation that covers `(segment_group, bus)`, if any.
pub fn mcfg_find_base(entries: &[McfgEntry], segment_group: u16, bus: u8) -> Option<McfgEntry> {
    entries
        .iter()
        .copied()
        .find(|e| e.covers(segment_group, bus))
}

// ---------------------------------------------------------------------------
// PCI capability list iteration
// ---------------------------------------------------------------------------

/// PCI status register bit 4: capabilities list present.
pub const PCI_STATUS_CAP_LIST: u16 = 1 << 4;

/// PCI configuration offset holding the capabilities pointer (type 0 header).
pub const PCI_CAPABILITIES_POINTER: u8 = 0x34;

/// PCI configuration offset of the command register.
pub const PCI_COMMAND: u8 = 0x04;

/// PCI configuration offset of the status register.
pub const PCI_STATUS: u8 = 0x06;

/// MSI capability ID.
pub const CAP_ID_MSI: u8 = 0x05;

/// MSI-X capability ID.
pub const CAP_ID_MSIX: u8 = 0x11;

/// MSI Message Control register offset from the MSI capability base.
pub const MSI_MESSAGE_CONTROL: u8 = 0x02;
/// MSI Message Address (low 32 bits).
pub const MSI_MESSAGE_ADDRESS: u8 = 0x04;
/// MSI Message Address (upper 32 bits) — present only when 64-bit bit is set.
pub const MSI_MESSAGE_ADDRESS_HIGH: u8 = 0x08;

/// MSI Message Control: 64-bit address capable.
pub const MSI_CTRL_64BIT: u16 = 1 << 7;
/// MSI Message Control: MSI enable.
pub const MSI_CTRL_ENABLE: u16 = 1 << 0;
/// MSI Message Control: per-vector masking capable.
pub const MSI_CTRL_PER_VECTOR_MASK: u16 = 1 << 8;
/// MSI Message Control: multiple-message enable field mask (bits 6:4).
pub const MSI_CTRL_MME_MASK: u16 = 0x0070;
/// MSI Message Control: multiple-message capable field mask (bits 3:1).
pub const MSI_CTRL_MMC_MASK: u16 = 0x000E;

/// Compute MSI Data register offset: bytes 8 (32-bit addr) or 12 (64-bit).
pub fn msi_data_offset(is_64bit: bool) -> u8 {
    if is_64bit { 0x0C } else { 0x08 }
}

/// Compute MSI Mask register offset when per-vector masking is supported.
pub fn msi_mask_offset(is_64bit: bool) -> u8 {
    if is_64bit { 0x10 } else { 0x0C }
}

/// Compute MSI Pending register offset when per-vector masking is supported.
pub fn msi_pending_offset(is_64bit: bool) -> u8 {
    if is_64bit { 0x14 } else { 0x10 }
}

/// MSI-X Message Control register offset from the MSI-X capability base.
pub const MSIX_MESSAGE_CONTROL: u8 = 0x02;
/// MSI-X Table offset/BIR.
pub const MSIX_TABLE_OFFSET: u8 = 0x04;
/// MSI-X Pending Bit Array offset/BIR.
pub const MSIX_PBA_OFFSET: u8 = 0x08;

/// MSI-X Message Control: MSI-X enable bit.
pub const MSIX_CTRL_ENABLE: u16 = 1 << 15;
/// MSI-X Message Control: function mask bit.
pub const MSIX_CTRL_FN_MASK: u16 = 1 << 14;
/// MSI-X Message Control: table size field mask (bits 10:0); result is
/// `size - 1` per the spec.
pub const MSIX_CTRL_TABLE_SIZE_MASK: u16 = 0x07FF;

/// Split a 32-bit MSI-X `Table Offset / BIR` (or PBA) field into `(bir,
/// offset_in_bytes)`. Low 3 bits are the BAR index (BIR), the remaining 29
/// bits are a byte offset aligned to 8.
pub fn msix_decode_offset_bir(raw: u32) -> (u8, u32) {
    let bir = (raw & 0x7) as u8;
    let offset = raw & !0x7;
    (bir, offset)
}

/// Decode the MSI-X table size from the Message Control register value. The
/// raw field encodes `size - 1`, so we add 1.
pub fn msix_decode_table_size(mc: u16) -> u16 {
    (mc & MSIX_CTRL_TABLE_SIZE_MASK) + 1
}

/// Decode the MSI multiple-message capable count (raw field is log2 of count,
/// 0..=5).
pub fn msi_decode_mmc_count(mc: u16) -> u8 {
    let raw = ((mc & MSI_CTRL_MMC_MASK) >> 1) as u8;
    // Clamp to 5 (32 vectors max).
    let raw = if raw > 5 { 5 } else { raw };
    1u8 << raw
}

/// Encode the MSI multiple-message enable count into the MME field of the
/// Message Control register (raw field is log2 of count, 0..=5).
///
/// Returns the full Message Control value with the MME field replaced.
pub fn msi_encode_mme(mc: u16, enable_count: u8) -> u16 {
    let count = enable_count.clamp(1, 32);
    // log2 of the nearest power-of-two <= count.
    let log2 = (count.next_power_of_two().trailing_zeros() as u16).min(5);
    (mc & !MSI_CTRL_MME_MASK) | (log2 << 4)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_mcfg_table(entries: &[McfgEntry]) -> alloc::vec::Vec<u8> {
        extern crate alloc;
        use alloc::vec::Vec;
        let mut bytes = Vec::new();
        // 36 bytes of SDT header, zeroed — we do not care about the header for
        // parsing, the kernel validates it separately.
        bytes.resize(36, 0);
        // 8 bytes of MCFG reserved
        bytes.resize(36 + 8, 0);
        for e in entries {
            bytes.extend_from_slice(&e.base_address.to_le_bytes());
            bytes.extend_from_slice(&e.segment_group.to_le_bytes());
            bytes.push(e.start_bus);
            bytes.push(e.end_bus);
            bytes.extend_from_slice(&[0u8; 4]);
        }
        bytes
    }

    #[test]
    fn parse_mcfg_single_entry() {
        let entry = McfgEntry {
            base_address: 0xE000_0000,
            segment_group: 0,
            start_bus: 0,
            end_bus: 0xFF,
        };
        let bytes = make_mcfg_table(&[entry]);
        let mut out = [McfgEntry {
            base_address: 0,
            segment_group: 0,
            start_bus: 0,
            end_bus: 0,
        }; 4];
        let n = parse_mcfg(&bytes, &mut out);
        assert_eq!(n, 1);
        assert_eq!(out[0], entry);
    }

    #[test]
    fn parse_mcfg_multi_entry() {
        let entries = [
            McfgEntry {
                base_address: 0xE000_0000,
                segment_group: 0,
                start_bus: 0,
                end_bus: 0x3F,
            },
            McfgEntry {
                base_address: 0xF000_0000,
                segment_group: 1,
                start_bus: 0,
                end_bus: 0xFF,
            },
        ];
        let bytes = make_mcfg_table(&entries);
        let mut out = [McfgEntry {
            base_address: 0,
            segment_group: 0,
            start_bus: 0,
            end_bus: 0,
        }; 4];
        let n = parse_mcfg(&bytes, &mut out);
        assert_eq!(n, 2);
        assert_eq!(out[0], entries[0]);
        assert_eq!(out[1], entries[1]);
    }

    #[test]
    fn parse_mcfg_truncated_returns_zero() {
        let bytes = [0u8; 40];
        let mut out = [McfgEntry {
            base_address: 0,
            segment_group: 0,
            start_bus: 0,
            end_bus: 0,
        }; 2];
        let n = parse_mcfg(&bytes, &mut out);
        assert_eq!(n, 0);
    }

    #[test]
    fn ecam_offset_math() {
        // start_bus=0, bus=0, dev=0, func=0, offset=0 -> 0
        assert_eq!(ecam_offset(0, 0, 0, 0, 0), 0);
        // offset wraps in 12 bits
        assert_eq!(ecam_offset(0, 0, 0, 0, 0x100), 0x100);
        // func only shifts bits 14:12
        assert_eq!(ecam_offset(0, 0, 0, 7, 0), 0x7000);
        // device shifts bits 19:15
        assert_eq!(ecam_offset(0, 0, 31, 0, 0), 31 << 15);
        // bus subtracts start_bus; 0x20 - 0x10 -> 0x10 << 20
        assert_eq!(ecam_offset(0x10, 0x20, 0, 0, 0), 0x10 << 20);
    }

    #[test]
    fn ecam_address_adds_base() {
        let e = McfgEntry {
            base_address: 0xE000_0000,
            segment_group: 0,
            start_bus: 0,
            end_bus: 0xFF,
        };
        assert_eq!(e.ecam_address(0, 0, 0, 0), 0xE000_0000);
        assert_eq!(
            e.ecam_address(1, 2, 3, 0x40),
            0xE000_0000 + (1 << 20) + (2 << 15) + (3 << 12) + 0x40
        );
    }

    #[test]
    fn mcfg_find_base_matches_only_covered_ranges() {
        let entries = [
            McfgEntry {
                base_address: 0xE000_0000,
                segment_group: 0,
                start_bus: 0x00,
                end_bus: 0x3F,
            },
            McfgEntry {
                base_address: 0xE400_0000,
                segment_group: 0,
                start_bus: 0x40,
                end_bus: 0x7F,
            },
        ];
        assert_eq!(
            mcfg_find_base(&entries, 0, 0x00).unwrap().base_address,
            0xE000_0000
        );
        assert_eq!(
            mcfg_find_base(&entries, 0, 0x3F).unwrap().base_address,
            0xE000_0000
        );
        assert_eq!(
            mcfg_find_base(&entries, 0, 0x40).unwrap().base_address,
            0xE400_0000
        );
        // Different segment group — no match.
        assert!(mcfg_find_base(&entries, 1, 0x00).is_none());
        // Out of range.
        assert!(mcfg_find_base(&entries, 0, 0x80).is_none());
    }

    #[test]
    fn msix_decode_offset_bir_splits_low_3_bits() {
        // raw = 0x1000_0002: BIR=2, offset=0x1000_0000
        let (bir, off) = msix_decode_offset_bir(0x1000_0002);
        assert_eq!(bir, 2);
        assert_eq!(off, 0x1000_0000);
        // BIR must be in [0,7]
        let (bir, off) = msix_decode_offset_bir(0xDEAD_BEEF);
        assert_eq!(bir, 7);
        assert_eq!(off, 0xDEAD_BEE8);
    }

    #[test]
    fn msix_decode_table_size_adds_one() {
        // mc = 0x0003 => table_size = 4
        assert_eq!(msix_decode_table_size(0x0003), 4);
        // mc = 0x0000 => table_size = 1
        assert_eq!(msix_decode_table_size(0x0000), 1);
        // mask the upper bits
        assert_eq!(msix_decode_table_size(0xFFFF), 2048);
    }

    #[test]
    fn msi_decode_mmc_count_is_power_of_two() {
        // MMC field = 0 => 1
        assert_eq!(msi_decode_mmc_count(0x0000), 1);
        // MMC field = 1 => 2
        assert_eq!(msi_decode_mmc_count(0x0002), 2);
        // MMC field = 2 => 4
        assert_eq!(msi_decode_mmc_count(0x0004), 4);
        // MMC field = 5 => 32
        assert_eq!(msi_decode_mmc_count(0x000A), 32);
        // MMC field = 7 (invalid) => clamped to 32
        assert_eq!(msi_decode_mmc_count(0x000E), 32);
    }

    #[test]
    fn msi_encode_mme_replaces_field_only() {
        // Start with 64-bit capable MC, enable 1 message.
        let mc = MSI_CTRL_64BIT | MSI_CTRL_PER_VECTOR_MASK;
        let out = msi_encode_mme(mc, 1);
        assert_eq!(out & MSI_CTRL_MME_MASK, 0);
        assert_eq!(out & MSI_CTRL_64BIT, MSI_CTRL_64BIT);
        // Enable 4 messages -> log2(4) = 2 in field 6:4 -> 0x20.
        let out = msi_encode_mme(mc, 4);
        assert_eq!(out & MSI_CTRL_MME_MASK, 0x20);
    }

    #[test]
    fn msi_data_mask_pending_offsets_differ_for_64bit() {
        assert_eq!(msi_data_offset(false), 0x08);
        assert_eq!(msi_data_offset(true), 0x0C);
        assert_eq!(msi_mask_offset(false), 0x0C);
        assert_eq!(msi_mask_offset(true), 0x10);
        assert_eq!(msi_pending_offset(false), 0x10);
        assert_eq!(msi_pending_offset(true), 0x14);
    }
}
