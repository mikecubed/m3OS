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
/// The count is rounded **up** to the next power of two via
/// [`u8::next_power_of_two`]; callers that need exact counts must pass values
/// that are already powers of two (the kernel-side caller gates on
/// [`u8::is_power_of_two`]).
///
/// Returns the full Message Control value with the MME field replaced.
pub fn msi_encode_mme(mc: u16, enable_count: u8) -> u16 {
    let count = enable_count.clamp(1, 32);
    // log2 of the count rounded up to the next power of two (capped at 5 to
    // keep the 3-bit MME field valid — 2^5 = 32 vectors maximum).
    let log2 = (count.next_power_of_two().trailing_zeros() as u16).min(5);
    (mc & !MSI_CTRL_MME_MASK) | (log2 << 4)
}

// ---------------------------------------------------------------------------
// BAR decoding (Phase 55 C.1)
// ---------------------------------------------------------------------------
//
// PCI BARs (type 0 header, offset 0x10..0x28) encode three things in their low
// bits:
//
//   * bit 0: 0 = memory space BAR, 1 = I/O (port) BAR.
//   * bits 2:1 (memory BAR only): BAR width — `00` = 32-bit, `10` = 64-bit.
//     `01` is reserved (legacy "below-1 MiB"), `11` is reserved.
//   * bit 3 (memory BAR only): prefetchable.
//
// The address portion of the BAR lives in the upper bits:
//   * memory BAR: `raw & 0xFFFF_FFF0` gives the low 32 bits of the base.
//   * I/O BAR: `raw & 0xFFFF_FFFC` gives the port base.
//
// 64-bit memory BARs occupy *two* consecutive BAR slots: the low slot's upper
// bits are the base's low 32 bits, and the next slot's full 32 bits are the
// base's high 32 bits.
//
// Size is determined by the standard write-ones / read-back algorithm:
// write `0xFFFFFFFF` into the BAR, read it back, mask off the type bits, and
// `size = !(readback) + 1`. The caller does the two register pokes; pure math
// lives here so it can be tested on the host.

/// Low-bit mask for memory-BAR address bits (strips type + prefetchable).
pub const BAR_MEM_ADDR_MASK: u32 = 0xFFFF_FFF0;
/// Low-bit mask for I/O-BAR address bits (strips the I/O flag + reserved bit 1).
pub const BAR_IO_ADDR_MASK: u32 = 0xFFFF_FFFC;

/// BAR "type" decoded from the raw BAR register.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BarType {
    /// 32-bit memory-mapped BAR — occupies one BAR slot.
    Memory32 { prefetchable: bool },
    /// 64-bit memory-mapped BAR — occupies this slot plus the next.
    Memory64 { prefetchable: bool },
    /// I/O port BAR — occupies one BAR slot.
    Io,
}

impl BarType {
    /// Decode the low bits of a BAR register.
    ///
    /// Returns `None` if the BAR uses a reserved width encoding (type field
    /// `01` or `11` on a memory BAR). Callers should treat reserved as "skip
    /// this slot" — it is not necessarily a driver error but it cannot be
    /// mapped.
    pub fn decode(raw: u32) -> Option<Self> {
        if raw & 0x1 != 0 {
            // I/O BAR — type bits 2:1 are reserved / ignored.
            return Some(BarType::Io);
        }
        let width = (raw >> 1) & 0x3;
        let prefetchable = (raw >> 3) & 0x1 != 0;
        match width {
            0b00 => Some(BarType::Memory32 { prefetchable }),
            0b10 => Some(BarType::Memory64 { prefetchable }),
            // 0b01 (reserved, historically "below 1 MiB") and 0b11 (reserved).
            _ => None,
        }
    }

    /// True if this BAR occupies two consecutive BAR slots.
    pub fn is_64bit(&self) -> bool {
        matches!(self, BarType::Memory64 { .. })
    }

    /// True if this BAR is memory-mapped (not I/O).
    pub fn is_memory(&self) -> bool {
        matches!(self, BarType::Memory32 { .. } | BarType::Memory64 { .. })
    }

    /// True if this BAR is prefetchable (memory BARs only).
    pub fn is_prefetchable(&self) -> bool {
        match self {
            BarType::Memory32 { prefetchable } | BarType::Memory64 { prefetchable } => {
                *prefetchable
            }
            BarType::Io => false,
        }
    }
}

/// Extract the low 32 bits of the BAR base address from the raw register value.
///
/// For memory BARs this strips the type + prefetchable bits; for I/O BARs it
/// strips the I/O flag and the reserved bit 1. The caller combines the result
/// with the high 32 bits of a 64-bit BAR (via [`combine_bar_64`]) as needed.
pub fn bar_base_low(raw: u32, bar_type: BarType) -> u32 {
    match bar_type {
        BarType::Memory32 { .. } | BarType::Memory64 { .. } => raw & BAR_MEM_ADDR_MASK,
        BarType::Io => raw & BAR_IO_ADDR_MASK,
    }
}

/// Combine a 64-bit BAR's low and high 32-bit halves into a full 64-bit base.
///
/// `raw_low` is the raw BAR register at the low slot; `raw_high` is the raw
/// 32 bits at `low_slot + 1`. This stripping the type bits out of the low half
/// before shifting the high half in.
pub fn combine_bar_64(raw_low: u32, raw_high: u32) -> u64 {
    ((raw_low & BAR_MEM_ADDR_MASK) as u64) | ((raw_high as u64) << 32)
}

/// Decode a BAR size from a sizing read-back.
///
/// The standard PCI BAR sizing dance:
///
///   1. Save the original BAR value.
///   2. Write `0xFFFFFFFF` to the BAR.
///   3. Read it back.
///   4. Restore the original BAR value.
///   5. Pass `(raw_original, raw_sizing_readback)` to this function.
///
/// For 64-bit memory BARs, the caller passes the 64-bit readback
/// (low half from the low slot + high half from the next slot) via
/// [`decode_bar_size_64`] instead.
///
/// Returns the BAR size in bytes, or `0` if the BAR is unimplemented (readback
/// returned all zeros after masking).
pub fn decode_bar_size_32(raw_original: u32, raw_readback: u32) -> u32 {
    let bar_type = match BarType::decode(raw_original) {
        Some(t) => t,
        None => return 0,
    };
    let mask = match bar_type {
        BarType::Memory32 { .. } | BarType::Memory64 { .. } => BAR_MEM_ADDR_MASK,
        BarType::Io => BAR_IO_ADDR_MASK,
    };
    let masked = raw_readback & mask;
    if masked == 0 {
        // BAR not implemented.
        return 0;
    }
    (!masked).wrapping_add(1)
}

/// Decode a 64-bit BAR size from a sizing read-back across both slots.
///
/// `raw_low` is the original BAR register; `readback_low` and `readback_high`
/// are the 32-bit values read back after writing `0xFFFFFFFF` to both the low
/// and high slots. The result is the size in bytes, or `0` if unimplemented.
pub fn decode_bar_size_64(raw_low: u32, readback_low: u32, readback_high: u32) -> u64 {
    // Ensure we were actually given a 64-bit memory BAR.
    match BarType::decode(raw_low) {
        Some(BarType::Memory64 { .. }) => {}
        _ => return 0,
    }
    let combined = ((readback_low & BAR_MEM_ADDR_MASK) as u64) | ((readback_high as u64) << 32);
    if combined == 0 {
        return 0;
    }
    (!combined).wrapping_add(1)
}

// ---------------------------------------------------------------------------
// MSI IDT-vector allocation (pure arithmetic — kernel side owns the static)
// ---------------------------------------------------------------------------

/// A minimal bump-style allocator for IDT vector ranges used by MSI / MSI-X.
///
/// MSI requires contiguous, aligned vectors: when the device asks for `count`
/// vectors the starting vector must be a multiple of `count`.  The kernel-side
/// `MSI_POOL` wraps this in a `Mutex` and passes the hardware-global `base`
/// and `top`.
pub struct MsiVectorAllocator {
    next: u8,
    top: u8,
}

impl MsiVectorAllocator {
    pub const fn new(base: u8, top: u8) -> Self {
        Self { next: base, top }
    }

    /// Reserve `count` consecutive vectors aligned to `count`.  `count` must
    /// be a power of two in 1..=32.  Returns the first vector, or `None` if
    /// the pool is exhausted.
    pub fn allocate(&mut self, count: u8) -> Option<u8> {
        if count == 0 || !count.is_power_of_two() || count > 32 {
            return None;
        }
        let mask = count.wrapping_sub(1);
        let aligned_start = (self.next.checked_add(mask)?) & !mask;
        let end = (aligned_start as u16) + count as u16;
        if end > self.top as u16 {
            return None;
        }
        self.next = end as u8;
        Some(aligned_start)
    }
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

    #[test]
    fn msi_vector_allocator_aligns_to_count() {
        let mut pool = MsiVectorAllocator::new(0x60, 0xF0);
        // count=1: no alignment constraint — start at 0x60.
        assert_eq!(pool.allocate(1), Some(0x60));
        assert_eq!(pool.allocate(1), Some(0x61));
        // count=4: next aligned-to-4 is 0x64.
        assert_eq!(pool.allocate(4), Some(0x64));
        // Allocator advanced past end of the 4-wide block.
        assert_eq!(pool.allocate(1), Some(0x68));
        // count=8: next aligned-to-8 is 0x70.
        assert_eq!(pool.allocate(8), Some(0x70));
    }

    #[test]
    fn msi_vector_allocator_rejects_non_power_of_two_and_oversize() {
        let mut pool = MsiVectorAllocator::new(0x60, 0xF0);
        assert_eq!(pool.allocate(0), None);
        assert_eq!(pool.allocate(3), None);
        assert_eq!(pool.allocate(64), None);
    }

    #[test]
    fn msi_vector_allocator_returns_none_when_full() {
        let mut pool = MsiVectorAllocator::new(0xE0, 0xF0);
        assert_eq!(pool.allocate(8), Some(0xE0));
        assert_eq!(pool.allocate(8), Some(0xE8));
        // Only 0x10 vectors in the pool, exhausted now.
        assert_eq!(pool.allocate(1), None);
    }

    // ------------------------------------------------------------------
    // BAR decoder tests — Phase 55 C.1 acceptance item 7.
    //
    // Coverage:
    //   (1) 32-bit MMIO BAR type + size.
    //   (2) PIO BAR type + size.
    //   (3) 64-bit MMIO BAR across two slots (upper-half decoding).
    //   (4) Prefetchable flag + zero-size ("unimplemented") BAR handling.
    // ------------------------------------------------------------------

    #[test]
    fn bar_decode_32bit_mmio_type_and_size() {
        // 32-bit memory BAR at 0xFEBF_0000, size 0x1000 (4 KiB).
        // Low bits: bit 0 = 0 (memory), bits 2:1 = 00 (32-bit), bit 3 = 0 (non-prefetchable).
        let raw = 0xFEBF_0000;
        let bar_type = BarType::decode(raw).expect("valid 32-bit memory BAR");
        assert_eq!(
            bar_type,
            BarType::Memory32 {
                prefetchable: false
            }
        );
        assert!(bar_type.is_memory());
        assert!(!bar_type.is_64bit());
        assert!(!bar_type.is_prefetchable());
        assert_eq!(bar_base_low(raw, bar_type), 0xFEBF_0000);

        // Size readback: write-ones then read back 0xFFFFF000 -> size = 0x1000.
        // Sizing reads back `!size + 1` in the address bits, so 0xFFFF_F000 -> 0x1000.
        let readback = 0xFFFF_F000;
        assert_eq!(decode_bar_size_32(raw, readback), 0x1000);
    }

    #[test]
    fn bar_decode_pio_type_and_size() {
        // I/O BAR at port 0xC000, size 0x20 (32 bytes).
        // Low bits: bit 0 = 1 (I/O). Bit 1 is reserved (0). Remaining bits are the port base.
        let raw = 0x0000_C001;
        let bar_type = BarType::decode(raw).expect("valid I/O BAR");
        assert_eq!(bar_type, BarType::Io);
        assert!(!bar_type.is_memory());
        assert!(!bar_type.is_64bit());
        assert!(!bar_type.is_prefetchable());
        assert_eq!(bar_base_low(raw, bar_type), 0x0000_C000);

        // Size readback: 0xFFFFFFE0 (masked) with I/O mask -> size 0x20.
        // Sizing readback `FFFF_FFE1` keeps bit 0 set (I/O marker); decode masks it off.
        let readback = 0xFFFF_FFE1;
        assert_eq!(decode_bar_size_32(raw, readback), 0x20);
    }

    #[test]
    fn bar_decode_64bit_mmio_type_and_upper_half() {
        // 64-bit memory BAR: base 0x1_FEBF_0000, size 0x1_0000_0000 (4 GiB).
        // Low slot: 0xFEBF_0000 with type bits 0b100 (memory, 64-bit, non-prefetchable).
        // High slot: 0x0000_0001.
        let raw_low = 0xFEBF_0004; // bits 2:1 = 10 (64-bit).
        let raw_high = 0x0000_0001;
        let bar_type = BarType::decode(raw_low).expect("valid 64-bit memory BAR");
        assert_eq!(
            bar_type,
            BarType::Memory64 {
                prefetchable: false
            }
        );
        assert!(bar_type.is_64bit());
        assert!(!bar_type.is_prefetchable());

        let base = combine_bar_64(raw_low, raw_high);
        assert_eq!(base, 0x0000_0001_FEBF_0000);

        // Sizing readback for a 4 GiB BAR: low half reads 0x0000_0000,
        // high half reads 0xFFFF_FFFF. Combined: 0xFFFF_FFFF_0000_0000 -> size 1 GiB * 4 = 0x1_0000_0000.
        let readback_low = 0x0000_0000;
        let readback_high = 0xFFFF_FFFF;
        assert_eq!(
            decode_bar_size_64(raw_low, readback_low, readback_high),
            0x0000_0001_0000_0000
        );
    }

    #[test]
    fn bar_decode_prefetchable_flag_and_zero_size_handling() {
        // Prefetchable 64-bit memory BAR.
        let raw = 0xE000_000C; // type bits 0b1100: memory, 64-bit, prefetchable.
        let bar_type = BarType::decode(raw).expect("valid prefetchable 64-bit BAR");
        assert_eq!(bar_type, BarType::Memory64 { prefetchable: true });
        assert!(bar_type.is_prefetchable());

        // 32-bit prefetchable.
        let raw32 = 0xD000_0008; // type bits 0b1000: memory, 32-bit, prefetchable.
        let bar_type32 = BarType::decode(raw32).expect("valid prefetchable 32-bit BAR");
        assert_eq!(bar_type32, BarType::Memory32 { prefetchable: true });
        assert!(bar_type32.is_prefetchable());

        // Zero-size / unimplemented BAR: sizing readback is all zeros in the
        // address bits. The decoder must return size = 0 (not panic or
        // overflow on !0 + 1).
        let raw_imp = 0x0000_0000; // memory BAR type, unimplemented.
        assert_eq!(decode_bar_size_32(raw_imp, 0x0000_0000), 0);

        // Reserved BAR width encoding (type field 0b01 or 0b11 on memory BAR)
        // decodes to None — caller must skip the slot.
        assert!(BarType::decode(0xFEBF_0002).is_none()); // reserved 0b01
        assert!(BarType::decode(0xFEBF_0006).is_none()); // reserved 0b11
        // decode_bar_size_32 on a reserved BAR returns 0 rather than panic.
        assert_eq!(decode_bar_size_32(0xFEBF_0002, 0xFFFF_F000), 0);
    }
}
