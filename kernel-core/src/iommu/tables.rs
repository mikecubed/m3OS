//! ACPI DMAR (Intel) and IVRS (AMD) structure decoders.
//!
//! Implementation lands in Phase 55a Track A.0. This module parses
//! firmware-provided byte buffers into typed Rust structures; no MMIO,
//! no hardware access, and no kernel-only dependencies. The decoders
//! are host-testable via `cargo test -p kernel-core`.
//!
//! # Tables decoded
//!
//! - **DMAR** — *DMA Remapping Reporting Table* (Intel VT-d). Its body
//!   carries a list of sub-tables enumerated by a 16-bit `type` field:
//!   DRHD (0), RMRR (1), ATSR (2), RHSA (3), ANDD (4, skipped). DRHD is
//!   the primary record; RMRR, ATSR, and RHSA carry auxiliary information.
//! - **IVRS** — *I/O Virtualization Reporting Structure* (AMD IOMMU).
//!   Its body carries a list of IVHD blocks (types 10h, 11h, 40h).
//!
//! # Return shape
//!
//! Both decoders return a `*Tables` aggregate so callers can see every
//! sub-table kind at once. The acceptance criterion names
//! `decode_dmar -> Result<Vec<DmaRemappingUnit>, _>`, but the aggregate
//! form gives the same information plus the auxiliary RMRR / ATSR /
//! RHSA lists. `DmarTables::drhds` is the `Vec<DmaRemappingUnit>` the
//! acceptance criterion names; callers that only need that list can
//! project it off the aggregate directly.
//!
//! # Endianness
//!
//! ACPI tables are little-endian on x86. All multi-byte field decoding
//! uses explicit `u16::from_le_bytes` / `u32::from_le_bytes` /
//! `u64::from_le_bytes` — no pointer casts, no `#[repr(packed)]` reads.
//!
//! # Unknown sub-tables
//!
//! Unknown sub-table `type` values are skipped by advancing the cursor
//! by the sub-table `length` field; `unknown_subtables` on the returned
//! aggregate counts how many such skips occurred. Malformed skip
//! lengths (zero, or larger than the remaining buffer) are errors.

use alloc::vec::Vec;

// ---------------------------------------------------------------------------
// ACPI SDT header — shared by DMAR and IVRS
// ---------------------------------------------------------------------------

/// Size of the standard ACPI System Descriptor Table header in bytes.
pub const ACPI_SDT_HEADER_LEN: usize = 36;

/// Offset from the start of the table at which the DMAR body begins:
/// 36 bytes of SDT header, 1 byte host-addr-width, 1 byte flags,
/// 10 reserved bytes → 48 bytes, then sub-tables.
pub const DMAR_BODY_OFFSET: usize = ACPI_SDT_HEADER_LEN + 1 + 1 + 10;

/// Offset from the start of the IVRS table at which the IVHD blocks
/// begin: 36 bytes of SDT header, 4 bytes `iv_info`, 8 bytes reserved
/// → 48 bytes.
pub const IVRS_BODY_OFFSET: usize = ACPI_SDT_HEADER_LEN + 4 + 8;

/// Minimum size of a DMAR / IVRS sub-table header (type + length).
pub const SUBTABLE_HEADER_LEN: usize = 4;

// DMAR sub-table type codes.
const DMAR_TYPE_DRHD: u16 = 0;
const DMAR_TYPE_RMRR: u16 = 1;
const DMAR_TYPE_ATSR: u16 = 2;
const DMAR_TYPE_RHSA: u16 = 3;

// IVRS block type codes.
const IVHD_TYPE_10H: u8 = 0x10;
const IVHD_TYPE_11H: u8 = 0x11;
const IVHD_TYPE_40H: u8 = 0x40;

// IVMD (I/O Virtualization Memory Definition) sub-table type codes.
const IVMD_TYPE_ALL: u8 = 0x20;
const IVMD_TYPE_SELECT: u8 = 0x21;
const IVMD_TYPE_RANGE: u8 = 0x22;

/// Fixed header length of an IVMD sub-table in bytes. Every IVMD shares
/// the same 32-byte prefix before its optional vendor-specific tail.
pub const IVMD_FIXED_LEN: usize = 32;

// IVHD device-entry type codes.
const IVHD_ENTRY_SELECT: u8 = 2;
const IVHD_ENTRY_START_RANGE: u8 = 3;
const IVHD_ENTRY_END_RANGE: u8 = 4;
const IVHD_ENTRY_ALIAS_SELECT: u8 = 66;
const IVHD_ENTRY_ALIAS_START_RANGE: u8 = 67;

// ---------------------------------------------------------------------------
// Public types — DMAR
// ---------------------------------------------------------------------------

/// Header portion of the DMAR table (the 48 bytes preceding the sub-tables).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DmarHeader {
    /// 4-byte ACPI signature (`"DMAR"`).
    pub signature: [u8; 4],
    /// Total length of the table in bytes, including header and body.
    pub length: u32,
    /// Table revision.
    pub revision: u8,
    /// One-byte checksum so the sum of every byte is 0 mod 256.
    pub checksum: u8,
    /// Maximum DMA address width supported (bits - 1, per VT-d spec).
    pub host_addr_width: u8,
    /// Feature flags (interrupt remapping, x2apic opt-out, DMA-control-opt-in).
    pub flags: u8,
}

/// A single device-scope entry that appears inside DRHD, RMRR, and ATSR
/// bodies. Describes a PCI device or hierarchy the enclosing sub-table
/// applies to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceScope {
    /// Type of the device scope entry (1 = PCI endpoint, 2 = PCI bridge,
    /// 3 = I/O APIC, 4 = HPET, 5 = ACPI namespace device).
    pub scope_type: u8,
    /// Total length of the entry in bytes, including the 6-byte header
    /// and the variable path bytes.
    pub length: u8,
    /// Enumeration identifier (I/O APIC id, HPET number, 0 for PCI).
    pub enumeration_id: u8,
    /// PCI start bus number.
    pub start_bus: u8,
    /// `(device, function)` pairs following the header (each 2 bytes).
    pub path: Vec<(u8, u8)>,
}

/// DMA Remapping Hardware Unit Definition (DRHD, sub-table type 0).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DmaRemappingUnit {
    /// Flags byte (bit 0 = INCLUDE_PCI_ALL).
    pub flags: u8,
    /// PCI segment group this unit belongs to.
    pub segment: u16,
    /// Physical address of the VT-d register file.
    pub register_base_address: u64,
    /// Device-scope entries enumerating the devices under this unit.
    pub device_scopes: Vec<DeviceScope>,
}

/// Reserved Memory Region Reporting entry (RMRR, sub-table type 1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReservedMemoryRegion {
    /// PCI segment group.
    pub segment: u16,
    /// Base physical address of the reserved region.
    pub base_addr: u64,
    /// Limit physical address (inclusive) of the reserved region.
    pub limit_addr: u64,
    /// Device-scope entries.
    pub device_scopes: Vec<DeviceScope>,
}

/// Address Translation Services Reporting entry (ATSR, sub-table type 2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AtsrEntry {
    /// Flags byte (bit 0 = ALL_PORTS).
    pub flags: u8,
    /// PCI segment group.
    pub segment: u16,
    /// Device-scope entries.
    pub device_scopes: Vec<DeviceScope>,
}

/// Remapping Hardware Static Affinity entry (RHSA, sub-table type 3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RhsaEntry {
    /// Physical address of the remapping hardware unit register base.
    pub register_base_address: u64,
    /// NUMA proximity domain the unit is affine to.
    pub proximity_domain: u32,
}

/// Container returned by [`decode_dmar`] holding every sub-table kind.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DmarTables {
    /// Parsed DMAR header (signature, length, flags, host address width).
    pub header: Option<DmarHeader>,
    /// DRHD entries.
    pub drhds: Vec<DmaRemappingUnit>,
    /// RMRR entries.
    pub rmrrs: Vec<ReservedMemoryRegion>,
    /// ATSR entries.
    pub atsrs: Vec<AtsrEntry>,
    /// RHSA entries.
    pub rhsas: Vec<RhsaEntry>,
    /// Count of sub-tables whose `type` code the decoder did not recognize.
    /// Skipped rather than fatal; kept as a counter for observability.
    pub unknown_subtables: u32,
}

// ---------------------------------------------------------------------------
// Public types — IVRS
// ---------------------------------------------------------------------------

/// Header portion of the IVRS table (the 48 bytes preceding the IVHD blocks).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IvrsHeader {
    /// 4-byte ACPI signature (`"IVRS"`).
    pub signature: [u8; 4],
    /// Total length of the table in bytes.
    pub length: u32,
    /// Table revision.
    pub revision: u8,
    /// One-byte checksum.
    pub checksum: u8,
    /// IVinfo field — virtual-address size, physical-address size, etc.
    pub iv_info: u32,
}

/// Device entry inside an IVHD block. Each variant carries the fields
/// relevant to its entry type; unsupported types cause decoding to
/// return `IvrsParseError::InvalidDeviceScope` so callers see bad data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IvhdDeviceEntry {
    /// Type 2 — single device specified by 16-bit `device_id`.
    Select { device_id: u16, data_setting: u8 },
    /// Type 3 — start of a device-id range.
    StartRange { device_id: u16, data_setting: u8 },
    /// Type 4 — end of a device-id range. Must follow a `StartRange`.
    EndRange { device_id: u16, data_setting: u8 },
    /// Type 66 (0x42) — single device aliased to another device.
    AliasSelect {
        device_id: u16,
        data_setting: u8,
        alias_device_id: u16,
    },
    /// Type 67 (0x43) — start of a range aliased to a source device.
    AliasStartRange {
        device_id: u16,
        data_setting: u8,
        alias_device_id: u16,
    },
}

/// A single IVHD block. Types 10h / 11h / 40h share the header shape
/// described in the acceptance criteria; future types would extend this
/// struct or introduce an enum variant rather than mutate the existing
/// fields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IvhdBlock {
    /// Block type (0x10, 0x11, or 0x40).
    pub block_type: u8,
    /// Flags byte from the IVHD header.
    pub flags: u8,
    /// Length of the block in bytes (as declared in the block header).
    pub length: u16,
    /// PCI BDF identifying the IOMMU device itself.
    pub device_id: u16,
    /// Offset into the device's capability list where the IOMMU
    /// capability lives.
    pub capability_offset: u16,
    /// Physical address of the IOMMU MMIO register base.
    pub iommu_base_address: u64,
    /// PCI segment group.
    pub pci_segment: u16,
    /// IOMMU info bitfield (MSI, HT unit id, etc.).
    pub iommu_info: u16,
    /// IOMMU feature / attribute information (4 bytes; spec-defined
    /// content varies by block type).
    pub iommu_feature_info: u32,
    /// Parsed device entries that follow the block header.
    pub device_entries: Vec<IvhdDeviceEntry>,
}

/// Which device(s) an IVMD sub-table applies to.
///
/// Per AMD IOMMU spec §5.2.2, a memory definition may scope to every
/// device on the platform ([`IvmdKind::All`]), a single 16-bit BDF
/// ([`IvmdKind::Select`]), or an inclusive range of BDFs
/// ([`IvmdKind::Range`]). The discriminator comes from the IVMD `type`
/// byte (0x20 / 0x21 / 0x22).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IvmdKind {
    /// Type 0x20 — applies to every device. `device_id` / `aux_data` ignored.
    All,
    /// Type 0x21 — applies to exactly one device identified by its BDF.
    Select { device_id: u16 },
    /// Type 0x22 — applies to every device whose BDF lies in the inclusive
    /// range `start_device_id..=end_device_id`.
    Range {
        start_device_id: u16,
        end_device_id: u16,
    },
}

/// A single IVMD sub-table entry — a firmware-declared memory range that
/// must be identity-mapped in every domain owned by the matching devices.
///
/// The raw `flags` byte is preserved verbatim. Bit 0 marks a required
/// unity map; bit 3 marks an exclusion range. For Phase 55a reserved-
/// region extraction every IVMD, regardless of flag bits, is treated as
/// a firmware-owned region.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IvrsMemDefinition {
    /// Which device scope the range applies to.
    pub kind: IvmdKind,
    /// Raw flags byte straight from the IVMD header. Bit 0 = Unity,
    /// bit 1 = IR (interrupt remapping exclusion), bit 2 = IW
    /// (exclusion), bit 3 = exclusion range. Unknown bits are preserved.
    pub flags: u8,
    /// Physical start address of the unity range.
    pub start_addr: u64,
    /// Length of the unity range in bytes.
    pub length: u64,
}

/// Container returned by [`decode_ivrs`] holding the IVRS header and
/// every IVHD block.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IvrsTables {
    /// Parsed IVRS header.
    pub header: Option<IvrsHeader>,
    /// IVHD blocks in table order.
    pub ivhd_blocks: Vec<IvhdBlock>,
    /// IVMD sub-tables in table order — firmware-declared unity maps.
    pub ivmds: Vec<IvrsMemDefinition>,
    /// Count of blocks whose `type` byte the decoder did not recognize.
    pub unknown_blocks: u32,
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Error kinds returned by [`decode_dmar`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DmarParseError {
    /// Input shorter than a DMAR header, or the header's declared
    /// `length` field is inconsistent with the buffer.
    TruncatedHeader,
    /// One-byte ACPI checksum does not sum the table bytes to 0 mod 256.
    InvalidChecksum,
    /// Revision byte outside the range this decoder understands.
    UnknownRevision,
    /// A sub-table's declared length exceeds the remaining buffer, is
    /// zero, or is smaller than its type-specific minimum.
    TruncatedSubTable,
    /// A device-scope entry inside DRHD / RMRR / ATSR has an invalid
    /// length (zero, smaller than 6, or larger than the sub-table
    /// remaining), or its path has an odd number of bytes.
    InvalidDeviceScope,
}

/// Error kinds returned by [`decode_ivrs`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IvrsParseError {
    /// Input shorter than an IVRS header, or the header's declared
    /// `length` field is inconsistent with the buffer.
    TruncatedHeader,
    /// One-byte ACPI checksum does not sum to 0 mod 256.
    InvalidChecksum,
    /// Revision byte outside the range this decoder understands.
    UnknownRevision,
    /// A block's declared length exceeds the remaining buffer, is
    /// zero, or is smaller than its header.
    TruncatedSubTable,
    /// An IVHD device entry has an invalid length, unknown type, or
    /// otherwise fails to parse.
    InvalidDeviceScope,
}

// ---------------------------------------------------------------------------
// Public API — decoders
// ---------------------------------------------------------------------------

/// Decode a DMAR table.
///
/// `bytes` should start at the first byte of the ACPI SDT header for
/// the `DMAR` table. The function validates the checksum, parses the
/// header, then walks the sub-table list until the declared table
/// length is consumed.
///
/// Unknown sub-table types are skipped and counted in
/// [`DmarTables::unknown_subtables`].
pub fn decode_dmar(bytes: &[u8]) -> Result<DmarTables, DmarParseError> {
    if bytes.len() < DMAR_BODY_OFFSET {
        return Err(DmarParseError::TruncatedHeader);
    }
    let header = parse_dmar_header(bytes)?;
    let total_len = header.length as usize;
    if total_len < DMAR_BODY_OFFSET || total_len > bytes.len() {
        return Err(DmarParseError::TruncatedHeader);
    }
    if header.revision == 0 {
        return Err(DmarParseError::UnknownRevision);
    }
    verify_checksum_dmar(&bytes[..total_len])?;

    let mut tables = DmarTables {
        header: Some(header),
        ..Default::default()
    };
    let body = &bytes[DMAR_BODY_OFFSET..total_len];
    let mut cursor = 0usize;
    while cursor < body.len() {
        if body.len() - cursor < SUBTABLE_HEADER_LEN {
            return Err(DmarParseError::TruncatedSubTable);
        }
        let ty = u16::from_le_bytes([body[cursor], body[cursor + 1]]);
        let len = u16::from_le_bytes([body[cursor + 2], body[cursor + 3]]) as usize;
        if len < SUBTABLE_HEADER_LEN || len > body.len() - cursor {
            return Err(DmarParseError::TruncatedSubTable);
        }
        let sub = &body[cursor..cursor + len];
        match ty {
            DMAR_TYPE_DRHD => tables.drhds.push(parse_drhd(sub)?),
            DMAR_TYPE_RMRR => tables.rmrrs.push(parse_rmrr(sub)?),
            DMAR_TYPE_ATSR => tables.atsrs.push(parse_atsr(sub)?),
            DMAR_TYPE_RHSA => tables.rhsas.push(parse_rhsa(sub)?),
            _ => tables.unknown_subtables = tables.unknown_subtables.saturating_add(1),
        }
        cursor += len;
    }
    Ok(tables)
}

/// Decode an IVRS table.
///
/// `bytes` should start at the first byte of the ACPI SDT header for
/// the `IVRS` table. The function validates the checksum, parses the
/// header, then walks the IVHD block list.
pub fn decode_ivrs(bytes: &[u8]) -> Result<IvrsTables, IvrsParseError> {
    if bytes.len() < IVRS_BODY_OFFSET {
        return Err(IvrsParseError::TruncatedHeader);
    }
    let header = parse_ivrs_header(bytes)?;
    let total_len = header.length as usize;
    if total_len < IVRS_BODY_OFFSET || total_len > bytes.len() {
        return Err(IvrsParseError::TruncatedHeader);
    }
    if header.revision == 0 {
        return Err(IvrsParseError::UnknownRevision);
    }
    verify_checksum_ivrs(&bytes[..total_len])?;

    let mut tables = IvrsTables {
        header: Some(header),
        ..Default::default()
    };
    let body = &bytes[IVRS_BODY_OFFSET..total_len];
    let mut cursor = 0usize;
    while cursor < body.len() {
        // An IVHD block starts with: type(1), flags(1), length(2).
        if body.len() - cursor < 4 {
            return Err(IvrsParseError::TruncatedSubTable);
        }
        let block_type = body[cursor];
        let block_len = u16::from_le_bytes([body[cursor + 2], body[cursor + 3]]) as usize;
        if block_len < 4 || block_len > body.len() - cursor {
            return Err(IvrsParseError::TruncatedSubTable);
        }
        let block_bytes = &body[cursor..cursor + block_len];
        match block_type {
            IVHD_TYPE_10H | IVHD_TYPE_11H | IVHD_TYPE_40H => {
                tables.ivhd_blocks.push(parse_ivhd_block(block_bytes)?);
            }
            IVMD_TYPE_ALL | IVMD_TYPE_SELECT | IVMD_TYPE_RANGE => {
                tables.ivmds.push(parse_ivmd(block_bytes)?);
            }
            _ => tables.unknown_blocks = tables.unknown_blocks.saturating_add(1),
        }
        cursor += block_len;
    }
    Ok(tables)
}

// ---------------------------------------------------------------------------
// Internal parsers — DMAR
// ---------------------------------------------------------------------------

fn parse_dmar_header(bytes: &[u8]) -> Result<DmarHeader, DmarParseError> {
    if bytes.len() < DMAR_BODY_OFFSET {
        return Err(DmarParseError::TruncatedHeader);
    }
    let signature = [bytes[0], bytes[1], bytes[2], bytes[3]];
    let length = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
    let revision = bytes[8];
    let checksum = bytes[9];
    // bytes[10..36] are OEM fields; we keep only what we need.
    let host_addr_width = bytes[ACPI_SDT_HEADER_LEN];
    let flags = bytes[ACPI_SDT_HEADER_LEN + 1];
    Ok(DmarHeader {
        signature,
        length,
        revision,
        checksum,
        host_addr_width,
        flags,
    })
}

fn verify_checksum_dmar(bytes: &[u8]) -> Result<(), DmarParseError> {
    let mut sum: u8 = 0;
    for &b in bytes {
        sum = sum.wrapping_add(b);
    }
    if sum == 0 {
        Ok(())
    } else {
        Err(DmarParseError::InvalidChecksum)
    }
}

/// Parse a DRHD body (sub-table type 0). `sub` includes the 4-byte
/// sub-table header.
fn parse_drhd(sub: &[u8]) -> Result<DmaRemappingUnit, DmarParseError> {
    // Header(4) + flags(1) + reserved(1) + segment(2) + register_base(8) = 16.
    const DRHD_FIXED_LEN: usize = SUBTABLE_HEADER_LEN + 12;
    if sub.len() < DRHD_FIXED_LEN {
        return Err(DmarParseError::TruncatedSubTable);
    }
    let flags = sub[SUBTABLE_HEADER_LEN];
    // sub[SUBTABLE_HEADER_LEN + 1] is reserved.
    let segment = u16::from_le_bytes([sub[SUBTABLE_HEADER_LEN + 2], sub[SUBTABLE_HEADER_LEN + 3]]);
    let register_base_address = u64::from_le_bytes([
        sub[SUBTABLE_HEADER_LEN + 4],
        sub[SUBTABLE_HEADER_LEN + 5],
        sub[SUBTABLE_HEADER_LEN + 6],
        sub[SUBTABLE_HEADER_LEN + 7],
        sub[SUBTABLE_HEADER_LEN + 8],
        sub[SUBTABLE_HEADER_LEN + 9],
        sub[SUBTABLE_HEADER_LEN + 10],
        sub[SUBTABLE_HEADER_LEN + 11],
    ]);
    let scopes = parse_device_scopes(&sub[DRHD_FIXED_LEN..])?;
    Ok(DmaRemappingUnit {
        flags,
        segment,
        register_base_address,
        device_scopes: scopes,
    })
}

/// Parse an RMRR body (sub-table type 1).
fn parse_rmrr(sub: &[u8]) -> Result<ReservedMemoryRegion, DmarParseError> {
    // Header(4) + reserved(2) + segment(2) + base(8) + limit(8) = 24.
    const RMRR_FIXED_LEN: usize = SUBTABLE_HEADER_LEN + 20;
    if sub.len() < RMRR_FIXED_LEN {
        return Err(DmarParseError::TruncatedSubTable);
    }
    let segment = u16::from_le_bytes([sub[SUBTABLE_HEADER_LEN + 2], sub[SUBTABLE_HEADER_LEN + 3]]);
    let base_addr = u64::from_le_bytes([
        sub[SUBTABLE_HEADER_LEN + 4],
        sub[SUBTABLE_HEADER_LEN + 5],
        sub[SUBTABLE_HEADER_LEN + 6],
        sub[SUBTABLE_HEADER_LEN + 7],
        sub[SUBTABLE_HEADER_LEN + 8],
        sub[SUBTABLE_HEADER_LEN + 9],
        sub[SUBTABLE_HEADER_LEN + 10],
        sub[SUBTABLE_HEADER_LEN + 11],
    ]);
    let limit_addr = u64::from_le_bytes([
        sub[SUBTABLE_HEADER_LEN + 12],
        sub[SUBTABLE_HEADER_LEN + 13],
        sub[SUBTABLE_HEADER_LEN + 14],
        sub[SUBTABLE_HEADER_LEN + 15],
        sub[SUBTABLE_HEADER_LEN + 16],
        sub[SUBTABLE_HEADER_LEN + 17],
        sub[SUBTABLE_HEADER_LEN + 18],
        sub[SUBTABLE_HEADER_LEN + 19],
    ]);
    let scopes = parse_device_scopes(&sub[RMRR_FIXED_LEN..])?;
    Ok(ReservedMemoryRegion {
        segment,
        base_addr,
        limit_addr,
        device_scopes: scopes,
    })
}

/// Parse an ATSR body (sub-table type 2).
fn parse_atsr(sub: &[u8]) -> Result<AtsrEntry, DmarParseError> {
    // Header(4) + flags(1) + reserved(1) + segment(2) = 8.
    const ATSR_FIXED_LEN: usize = SUBTABLE_HEADER_LEN + 4;
    if sub.len() < ATSR_FIXED_LEN {
        return Err(DmarParseError::TruncatedSubTable);
    }
    let flags = sub[SUBTABLE_HEADER_LEN];
    let segment = u16::from_le_bytes([sub[SUBTABLE_HEADER_LEN + 2], sub[SUBTABLE_HEADER_LEN + 3]]);
    let scopes = parse_device_scopes(&sub[ATSR_FIXED_LEN..])?;
    Ok(AtsrEntry {
        flags,
        segment,
        device_scopes: scopes,
    })
}

/// Parse an RHSA body (sub-table type 3).
fn parse_rhsa(sub: &[u8]) -> Result<RhsaEntry, DmarParseError> {
    // Header(4) + reserved(4) + register_base(8) + proximity_domain(4) = 20.
    const RHSA_FIXED_LEN: usize = SUBTABLE_HEADER_LEN + 16;
    if sub.len() < RHSA_FIXED_LEN {
        return Err(DmarParseError::TruncatedSubTable);
    }
    let register_base_address = u64::from_le_bytes([
        sub[SUBTABLE_HEADER_LEN + 4],
        sub[SUBTABLE_HEADER_LEN + 5],
        sub[SUBTABLE_HEADER_LEN + 6],
        sub[SUBTABLE_HEADER_LEN + 7],
        sub[SUBTABLE_HEADER_LEN + 8],
        sub[SUBTABLE_HEADER_LEN + 9],
        sub[SUBTABLE_HEADER_LEN + 10],
        sub[SUBTABLE_HEADER_LEN + 11],
    ]);
    let proximity_domain = u32::from_le_bytes([
        sub[SUBTABLE_HEADER_LEN + 12],
        sub[SUBTABLE_HEADER_LEN + 13],
        sub[SUBTABLE_HEADER_LEN + 14],
        sub[SUBTABLE_HEADER_LEN + 15],
    ]);
    Ok(RhsaEntry {
        register_base_address,
        proximity_domain,
    })
}

/// Parse a contiguous run of device-scope entries.
///
/// Each entry is: type(1) + length(1) + reserved(2) + enumeration_id(1)
/// + start_bus(1) + (length - 6) bytes of (device, function) pairs.
fn parse_device_scopes(mut bytes: &[u8]) -> Result<Vec<DeviceScope>, DmarParseError> {
    let mut out = Vec::new();
    while !bytes.is_empty() {
        if bytes.len() < 6 {
            return Err(DmarParseError::InvalidDeviceScope);
        }
        let scope_type = bytes[0];
        let length = bytes[1] as usize;
        if length < 6 || length > bytes.len() {
            return Err(DmarParseError::InvalidDeviceScope);
        }
        let enumeration_id = bytes[4];
        let start_bus = bytes[5];
        let path_bytes = &bytes[6..length];
        if !path_bytes.len().is_multiple_of(2) {
            return Err(DmarParseError::InvalidDeviceScope);
        }
        let mut path = Vec::with_capacity(path_bytes.len() / 2);
        let mut i = 0;
        while i < path_bytes.len() {
            path.push((path_bytes[i], path_bytes[i + 1]));
            i += 2;
        }
        out.push(DeviceScope {
            scope_type,
            length: length as u8,
            enumeration_id,
            start_bus,
            path,
        });
        bytes = &bytes[length..];
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Internal parsers — IVRS
// ---------------------------------------------------------------------------

fn parse_ivrs_header(bytes: &[u8]) -> Result<IvrsHeader, IvrsParseError> {
    if bytes.len() < IVRS_BODY_OFFSET {
        return Err(IvrsParseError::TruncatedHeader);
    }
    let signature = [bytes[0], bytes[1], bytes[2], bytes[3]];
    let length = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
    let revision = bytes[8];
    let checksum = bytes[9];
    let iv_info = u32::from_le_bytes([
        bytes[ACPI_SDT_HEADER_LEN],
        bytes[ACPI_SDT_HEADER_LEN + 1],
        bytes[ACPI_SDT_HEADER_LEN + 2],
        bytes[ACPI_SDT_HEADER_LEN + 3],
    ]);
    Ok(IvrsHeader {
        signature,
        length,
        revision,
        checksum,
        iv_info,
    })
}

fn verify_checksum_ivrs(bytes: &[u8]) -> Result<(), IvrsParseError> {
    let mut sum: u8 = 0;
    for &b in bytes {
        sum = sum.wrapping_add(b);
    }
    if sum == 0 {
        Ok(())
    } else {
        Err(IvrsParseError::InvalidChecksum)
    }
}

/// Parse a single IVHD block. `bytes` covers the entire block, starting
/// at the `type` byte.
///
/// IVHD 10h / 11h / 40h share the same 24-byte header shape used by the
/// acceptance criteria:
///
/// - type(1) + flags(1) + length(2) + device_id(2) + capability_offset(2)
/// - iommu_base(8) + pci_segment(2) + iommu_info(2) + iommu_feature_info(4)
///
/// Total = 24 bytes. Device entries follow.
fn parse_ivhd_block(bytes: &[u8]) -> Result<IvhdBlock, IvrsParseError> {
    const IVHD_FIXED_LEN: usize = 24;
    if bytes.len() < IVHD_FIXED_LEN {
        return Err(IvrsParseError::TruncatedSubTable);
    }
    let block_type = bytes[0];
    let flags = bytes[1];
    let length = u16::from_le_bytes([bytes[2], bytes[3]]);
    let device_id = u16::from_le_bytes([bytes[4], bytes[5]]);
    let capability_offset = u16::from_le_bytes([bytes[6], bytes[7]]);
    let iommu_base_address = u64::from_le_bytes([
        bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15],
    ]);
    let pci_segment = u16::from_le_bytes([bytes[16], bytes[17]]);
    let iommu_info = u16::from_le_bytes([bytes[18], bytes[19]]);
    let iommu_feature_info = u32::from_le_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
    let device_entries = parse_ivhd_device_entries(&bytes[IVHD_FIXED_LEN..])?;
    Ok(IvhdBlock {
        block_type,
        flags,
        length,
        device_id,
        capability_offset,
        iommu_base_address,
        pci_segment,
        iommu_info,
        iommu_feature_info,
        device_entries,
    })
}

/// Parse the run of IVHD device entries that follow the fixed header.
///
/// Entry layout by type:
/// - 2 (Select), 3 (Start Range), 4 (End Range): 4 bytes total —
///   type(1) + device_id(2) + data_setting(1).
/// - 66 (Alias Select), 67 (Alias Start Range): 8 bytes total —
///   type(1) + device_id(2) + data_setting(1) + reserved(1) +
///   alias_device_id(2) + reserved(1).
fn parse_ivhd_device_entries(mut bytes: &[u8]) -> Result<Vec<IvhdDeviceEntry>, IvrsParseError> {
    let mut out = Vec::new();
    while !bytes.is_empty() {
        let entry_type = bytes[0];
        match entry_type {
            IVHD_ENTRY_SELECT | IVHD_ENTRY_START_RANGE | IVHD_ENTRY_END_RANGE => {
                if bytes.len() < 4 {
                    return Err(IvrsParseError::InvalidDeviceScope);
                }
                let device_id = u16::from_le_bytes([bytes[1], bytes[2]]);
                let data_setting = bytes[3];
                let entry = match entry_type {
                    IVHD_ENTRY_SELECT => IvhdDeviceEntry::Select {
                        device_id,
                        data_setting,
                    },
                    IVHD_ENTRY_START_RANGE => IvhdDeviceEntry::StartRange {
                        device_id,
                        data_setting,
                    },
                    _ => IvhdDeviceEntry::EndRange {
                        device_id,
                        data_setting,
                    },
                };
                out.push(entry);
                bytes = &bytes[4..];
            }
            IVHD_ENTRY_ALIAS_SELECT | IVHD_ENTRY_ALIAS_START_RANGE => {
                if bytes.len() < 8 {
                    return Err(IvrsParseError::InvalidDeviceScope);
                }
                let device_id = u16::from_le_bytes([bytes[1], bytes[2]]);
                let data_setting = bytes[3];
                // bytes[4] reserved.
                let alias_device_id = u16::from_le_bytes([bytes[5], bytes[6]]);
                // bytes[7] reserved.
                let entry = if entry_type == IVHD_ENTRY_ALIAS_SELECT {
                    IvhdDeviceEntry::AliasSelect {
                        device_id,
                        data_setting,
                        alias_device_id,
                    }
                } else {
                    IvhdDeviceEntry::AliasStartRange {
                        device_id,
                        data_setting,
                        alias_device_id,
                    }
                };
                out.push(entry);
                bytes = &bytes[8..];
            }
            _ => {
                // Unknown entry — surface to the caller. The acceptance
                // criteria enumerate five entry types; anything else is
                // data the decoder cannot interpret safely.
                return Err(IvrsParseError::InvalidDeviceScope);
            }
        }
    }
    Ok(out)
}

/// Parse a single IVMD sub-table (types 0x20 / 0x21 / 0x22). `bytes`
/// covers the whole sub-table starting at the type byte.
///
/// AMD IOMMU spec §5.2.2 defines the 32-byte fixed header:
///
/// ```text
/// offset 0  type (1)
/// offset 1  flags (1)
/// offset 2  length (2)
/// offset 4  device_id (2)
/// offset 6  aux_data (2)
/// offset 8  reserved (8)
/// offset 16 start_addr (8)
/// offset 24 length (8)
/// ```
///
/// Anything past offset 32 is vendor-specific padding and is ignored —
/// the outer cursor walk honors the block's declared length for the
/// advance, so callers see the full region regardless.
fn parse_ivmd(bytes: &[u8]) -> Result<IvrsMemDefinition, IvrsParseError> {
    if bytes.len() < IVMD_FIXED_LEN {
        return Err(IvrsParseError::TruncatedSubTable);
    }
    let ivmd_type = bytes[0];
    let flags = bytes[1];
    // bytes[2..4] = length (already validated by the caller).
    let device_id = u16::from_le_bytes([bytes[4], bytes[5]]);
    let aux_data = u16::from_le_bytes([bytes[6], bytes[7]]);
    // bytes[8..16] = reserved.
    let start_addr = u64::from_le_bytes([
        bytes[16], bytes[17], bytes[18], bytes[19], bytes[20], bytes[21], bytes[22], bytes[23],
    ]);
    let length = u64::from_le_bytes([
        bytes[24], bytes[25], bytes[26], bytes[27], bytes[28], bytes[29], bytes[30], bytes[31],
    ]);
    let kind = match ivmd_type {
        IVMD_TYPE_ALL => IvmdKind::All,
        IVMD_TYPE_SELECT => IvmdKind::Select { device_id },
        IVMD_TYPE_RANGE => IvmdKind::Range {
            start_device_id: device_id,
            end_device_id: aux_data,
        },
        // Caller only dispatches known types here; defensive fallback
        // surfaces a spec violation rather than silently ignoring it.
        _ => return Err(IvrsParseError::TruncatedSubTable),
    };
    Ok(IvrsMemDefinition {
        kind,
        flags,
        start_addr,
        length,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
mod tests {
    use super::*;
    use alloc::vec;
    use proptest::prelude::*;

    // -----------------------------------------------------------------
    // Test helpers — synthesize blobs the decoders must accept.
    // -----------------------------------------------------------------

    fn push_u16(out: &mut Vec<u8>, v: u16) {
        out.extend_from_slice(&v.to_le_bytes());
    }
    fn push_u32(out: &mut Vec<u8>, v: u32) {
        out.extend_from_slice(&v.to_le_bytes());
    }
    fn push_u64(out: &mut Vec<u8>, v: u64) {
        out.extend_from_slice(&v.to_le_bytes());
    }

    fn make_sdt_header(signature: &[u8; 4], revision: u8) -> Vec<u8> {
        let mut hdr = Vec::with_capacity(ACPI_SDT_HEADER_LEN);
        hdr.extend_from_slice(signature);
        push_u32(&mut hdr, 0);
        hdr.push(revision);
        hdr.push(0);
        hdr.extend_from_slice(b"M3OSTS");
        hdr.extend_from_slice(b"M3TABLE1");
        push_u32(&mut hdr, 1);
        push_u32(&mut hdr, 0x4D33_4F53);
        push_u32(&mut hdr, 1);
        debug_assert_eq!(hdr.len(), ACPI_SDT_HEADER_LEN);
        hdr
    }

    fn finalize_table(bytes: &mut [u8]) {
        let len = bytes.len() as u32;
        bytes[4..8].copy_from_slice(&len.to_le_bytes());
        bytes[9] = 0;
        let sum: u8 = bytes.iter().fold(0u8, |acc, &b| acc.wrapping_add(b));
        bytes[9] = 0u8.wrapping_sub(sum);
    }

    fn make_dmar_prefix(revision: u8, host_addr_width: u8, flags: u8) -> Vec<u8> {
        let mut out = make_sdt_header(b"DMAR", revision);
        out.push(host_addr_width);
        out.push(flags);
        out.extend_from_slice(&[0u8; 10]);
        out
    }

    fn make_ivrs_prefix(revision: u8, iv_info: u32) -> Vec<u8> {
        let mut out = make_sdt_header(b"IVRS", revision);
        push_u32(&mut out, iv_info);
        out.extend_from_slice(&[0u8; 8]);
        out
    }

    fn push_device_scope(out: &mut Vec<u8>, scope_type: u8, bus: u8, path: &[(u8, u8)]) {
        let length = 6 + path.len() * 2;
        out.push(scope_type);
        out.push(length as u8);
        out.push(0);
        out.push(0);
        out.push(0);
        out.push(bus);
        for &(dev, func) in path {
            out.push(dev);
            out.push(func);
        }
    }

    fn make_drhd(flags: u8, segment: u16, base: u64, scopes: &[(u8, u8, &[(u8, u8)])]) -> Vec<u8> {
        let mut out = Vec::new();
        push_u16(&mut out, DMAR_TYPE_DRHD);
        push_u16(&mut out, 0);
        out.push(flags);
        out.push(0);
        push_u16(&mut out, segment);
        push_u64(&mut out, base);
        for (stype, bus, path) in scopes {
            push_device_scope(&mut out, *stype, *bus, path);
        }
        let len = out.len() as u16;
        out[2..4].copy_from_slice(&len.to_le_bytes());
        out
    }

    fn make_rmrr(
        segment: u16,
        base_addr: u64,
        limit_addr: u64,
        scopes: &[(u8, u8, &[(u8, u8)])],
    ) -> Vec<u8> {
        let mut out = Vec::new();
        push_u16(&mut out, DMAR_TYPE_RMRR);
        push_u16(&mut out, 0);
        push_u16(&mut out, 0);
        push_u16(&mut out, segment);
        push_u64(&mut out, base_addr);
        push_u64(&mut out, limit_addr);
        for (stype, bus, path) in scopes {
            push_device_scope(&mut out, *stype, *bus, path);
        }
        let len = out.len() as u16;
        out[2..4].copy_from_slice(&len.to_le_bytes());
        out
    }

    fn make_atsr(flags: u8, segment: u16, scopes: &[(u8, u8, &[(u8, u8)])]) -> Vec<u8> {
        let mut out = Vec::new();
        push_u16(&mut out, DMAR_TYPE_ATSR);
        push_u16(&mut out, 0);
        out.push(flags);
        out.push(0);
        push_u16(&mut out, segment);
        for (stype, bus, path) in scopes {
            push_device_scope(&mut out, *stype, *bus, path);
        }
        let len = out.len() as u16;
        out[2..4].copy_from_slice(&len.to_le_bytes());
        out
    }

    fn make_rhsa(register_base: u64, proximity: u32) -> Vec<u8> {
        let mut out = Vec::new();
        push_u16(&mut out, DMAR_TYPE_RHSA);
        push_u16(&mut out, 20);
        push_u32(&mut out, 0);
        push_u64(&mut out, register_base);
        push_u32(&mut out, proximity);
        out
    }

    fn make_ivhd(
        block_type: u8,
        flags: u8,
        device_id: u16,
        cap: u16,
        base: u64,
        segment: u16,
        info: u16,
        feature: u32,
        device_entries: &[u8],
    ) -> Vec<u8> {
        let mut out = Vec::new();
        out.push(block_type);
        out.push(flags);
        push_u16(&mut out, 0);
        push_u16(&mut out, device_id);
        push_u16(&mut out, cap);
        push_u64(&mut out, base);
        push_u16(&mut out, segment);
        push_u16(&mut out, info);
        push_u32(&mut out, feature);
        out.extend_from_slice(device_entries);
        let len = out.len() as u16;
        out[2..4].copy_from_slice(&len.to_le_bytes());
        out
    }

    fn push_ivhd_short(out: &mut Vec<u8>, entry_type: u8, device_id: u16, data_setting: u8) {
        out.push(entry_type);
        push_u16(out, device_id);
        out.push(data_setting);
    }

    fn push_ivhd_alias(
        out: &mut Vec<u8>,
        entry_type: u8,
        device_id: u16,
        data_setting: u8,
        alias: u16,
    ) {
        out.push(entry_type);
        push_u16(out, device_id);
        out.push(data_setting);
        out.push(0);
        push_u16(out, alias);
        out.push(0);
    }

    /// Build a spec-shaped IVMD sub-table:
    ///   type(1) + flags(1) + length(2) + device_id(2) + aux_data(2)
    ///   + reserved(8) + start_addr(8) + length(8)  = 32 bytes.
    fn make_ivmd(
        ivmd_type: u8,
        flags: u8,
        device_id: u16,
        aux_data: u16,
        start_addr: u64,
        length: u64,
    ) -> Vec<u8> {
        let mut out = Vec::with_capacity(IVMD_FIXED_LEN);
        out.push(ivmd_type);
        out.push(flags);
        push_u16(&mut out, IVMD_FIXED_LEN as u16);
        push_u16(&mut out, device_id);
        push_u16(&mut out, aux_data);
        out.extend_from_slice(&[0u8; 8]);
        push_u64(&mut out, start_addr);
        push_u64(&mut out, length);
        debug_assert_eq!(out.len(), IVMD_FIXED_LEN);
        out
    }

    // -----------------------------------------------------------------
    // DMAR unit tests
    // -----------------------------------------------------------------

    #[test]
    fn dmar_decode_empty_table_no_subtables() {
        let mut bytes = make_dmar_prefix(1, 39, 0);
        finalize_table(&mut bytes);
        let tables = decode_dmar(&bytes).expect("empty DMAR decodes");
        assert!(tables.header.is_some());
        assert!(tables.drhds.is_empty());
        assert!(tables.rmrrs.is_empty());
        assert!(tables.atsrs.is_empty());
        assert!(tables.rhsas.is_empty());
        assert_eq!(tables.unknown_subtables, 0);
        let hdr = tables.header.unwrap();
        assert_eq!(&hdr.signature, b"DMAR");
        assert_eq!(hdr.host_addr_width, 39);
        assert_eq!(hdr.revision, 1);
    }

    #[test]
    fn dmar_decode_drhd_without_scopes() {
        let mut bytes = make_dmar_prefix(1, 48, 1);
        let drhd = make_drhd(0x01, 0, 0xFED9_0000, &[]);
        bytes.extend_from_slice(&drhd);
        finalize_table(&mut bytes);
        let tables = decode_dmar(&bytes).expect("decode DRHD");
        assert_eq!(tables.drhds.len(), 1);
        let entry = &tables.drhds[0];
        assert_eq!(entry.flags, 0x01);
        assert_eq!(entry.segment, 0);
        assert_eq!(entry.register_base_address, 0xFED9_0000);
        assert!(entry.device_scopes.is_empty());
    }

    #[test]
    fn dmar_decode_drhd_with_scopes() {
        let mut bytes = make_dmar_prefix(1, 48, 0);
        let scopes: &[(u8, u8, &[(u8, u8)])] = &[(1, 0x10, &[(0x1f, 0x02), (0x00, 0x00)])];
        let drhd = make_drhd(0, 1, 0x1000_0000, scopes);
        bytes.extend_from_slice(&drhd);
        finalize_table(&mut bytes);
        let tables = decode_dmar(&bytes).unwrap();
        assert_eq!(tables.drhds.len(), 1);
        let scope = &tables.drhds[0].device_scopes[0];
        assert_eq!(scope.scope_type, 1);
        assert_eq!(scope.start_bus, 0x10);
        assert_eq!(scope.path, vec![(0x1f, 0x02), (0x00, 0x00)]);
    }

    #[test]
    fn dmar_decode_rmrr() {
        let mut bytes = make_dmar_prefix(1, 48, 0);
        let rmrr = make_rmrr(0, 0x8000_0000, 0x8FFF_FFFF, &[(1, 0x00, &[(0x02, 0x00)])]);
        bytes.extend_from_slice(&rmrr);
        finalize_table(&mut bytes);
        let tables = decode_dmar(&bytes).unwrap();
        assert_eq!(tables.rmrrs.len(), 1);
        assert_eq!(tables.rmrrs[0].base_addr, 0x8000_0000);
        assert_eq!(tables.rmrrs[0].limit_addr, 0x8FFF_FFFF);
        assert_eq!(tables.rmrrs[0].device_scopes.len(), 1);
    }

    #[test]
    fn dmar_decode_atsr() {
        let mut bytes = make_dmar_prefix(1, 48, 0);
        let atsr = make_atsr(0x00, 0, &[(1, 0x00, &[])]);
        bytes.extend_from_slice(&atsr);
        finalize_table(&mut bytes);
        let tables = decode_dmar(&bytes).unwrap();
        assert_eq!(tables.atsrs.len(), 1);
        assert_eq!(tables.atsrs[0].device_scopes.len(), 1);
    }

    #[test]
    fn dmar_decode_rhsa() {
        let mut bytes = make_dmar_prefix(1, 48, 0);
        let rhsa = make_rhsa(0xFED9_0000, 2);
        bytes.extend_from_slice(&rhsa);
        finalize_table(&mut bytes);
        let tables = decode_dmar(&bytes).unwrap();
        assert_eq!(tables.rhsas.len(), 1);
        assert_eq!(tables.rhsas[0].register_base_address, 0xFED9_0000);
        assert_eq!(tables.rhsas[0].proximity_domain, 2);
    }

    #[test]
    fn dmar_unknown_subtable_is_counted_not_fatal() {
        let mut bytes = make_dmar_prefix(1, 48, 0);
        let mut unknown = Vec::new();
        push_u16(&mut unknown, 4);
        push_u16(&mut unknown, 8);
        unknown.extend_from_slice(&[0, 0, 0, 0]);
        bytes.extend_from_slice(&unknown);
        finalize_table(&mut bytes);
        let tables = decode_dmar(&bytes).unwrap();
        assert_eq!(tables.unknown_subtables, 1);
        assert!(tables.drhds.is_empty());
    }

    #[test]
    fn dmar_mixed_subtables_are_all_returned() {
        let mut bytes = make_dmar_prefix(1, 48, 0);
        bytes.extend_from_slice(&make_drhd(0, 0, 0xFED9_0000, &[]));
        bytes.extend_from_slice(&make_rmrr(0, 0x9000_0000, 0x9FFF_FFFF, &[]));
        bytes.extend_from_slice(&make_atsr(0, 0, &[]));
        bytes.extend_from_slice(&make_rhsa(0xFED9_0000, 0));
        finalize_table(&mut bytes);
        let tables = decode_dmar(&bytes).unwrap();
        assert_eq!(tables.drhds.len(), 1);
        assert_eq!(tables.rmrrs.len(), 1);
        assert_eq!(tables.atsrs.len(), 1);
        assert_eq!(tables.rhsas.len(), 1);
    }

    #[test]
    fn dmar_truncated_header_returns_error() {
        let bytes = [0u8; 10];
        assert_eq!(
            decode_dmar(&bytes).unwrap_err(),
            DmarParseError::TruncatedHeader
        );
    }

    #[test]
    fn dmar_invalid_checksum_returns_error() {
        let mut bytes = make_dmar_prefix(1, 48, 0);
        finalize_table(&mut bytes);
        bytes[9] = bytes[9].wrapping_add(1);
        assert_eq!(
            decode_dmar(&bytes).unwrap_err(),
            DmarParseError::InvalidChecksum
        );
    }

    #[test]
    fn dmar_unknown_revision_returns_error() {
        let mut bytes = make_dmar_prefix(0, 48, 0);
        finalize_table(&mut bytes);
        assert_eq!(
            decode_dmar(&bytes).unwrap_err(),
            DmarParseError::UnknownRevision
        );
    }

    #[test]
    fn dmar_truncated_subtable_returns_error() {
        let mut bytes = make_dmar_prefix(1, 48, 0);
        push_u16(&mut bytes, DMAR_TYPE_DRHD);
        push_u16(&mut bytes, 0xFFFF);
        finalize_table(&mut bytes);
        assert_eq!(
            decode_dmar(&bytes).unwrap_err(),
            DmarParseError::TruncatedSubTable
        );
    }

    #[test]
    fn dmar_invalid_device_scope_returns_error() {
        let mut bytes = make_dmar_prefix(1, 48, 0);
        let mut drhd = Vec::new();
        push_u16(&mut drhd, DMAR_TYPE_DRHD);
        push_u16(&mut drhd, 0);
        drhd.extend_from_slice(&[0u8; 12]);
        drhd.extend_from_slice(&[1u8, 3u8, 0u8]);
        let len = drhd.len() as u16;
        drhd[2..4].copy_from_slice(&len.to_le_bytes());
        bytes.extend_from_slice(&drhd);
        finalize_table(&mut bytes);
        assert_eq!(
            decode_dmar(&bytes).unwrap_err(),
            DmarParseError::InvalidDeviceScope
        );
    }

    // -----------------------------------------------------------------
    // IVRS unit tests
    // -----------------------------------------------------------------

    #[test]
    fn ivrs_decode_empty_table_no_blocks() {
        let mut bytes = make_ivrs_prefix(1, 0x1010);
        finalize_table(&mut bytes);
        let tables = decode_ivrs(&bytes).expect("empty IVRS decodes");
        assert!(tables.header.is_some());
        assert!(tables.ivhd_blocks.is_empty());
        assert_eq!(tables.unknown_blocks, 0);
        let hdr = tables.header.unwrap();
        assert_eq!(&hdr.signature, b"IVRS");
        assert_eq!(hdr.iv_info, 0x1010);
    }

    #[test]
    fn ivrs_decode_ivhd_10h_with_select() {
        let mut bytes = make_ivrs_prefix(1, 0);
        let mut entries = Vec::new();
        push_ivhd_short(&mut entries, IVHD_ENTRY_SELECT, 0x0030, 0);
        let block = make_ivhd(
            IVHD_TYPE_10H,
            0x40,
            0x0018,
            0x40,
            0xFEB8_0000,
            0,
            0,
            0,
            &entries,
        );
        bytes.extend_from_slice(&block);
        finalize_table(&mut bytes);
        let tables = decode_ivrs(&bytes).unwrap();
        assert_eq!(tables.ivhd_blocks.len(), 1);
        let blk = &tables.ivhd_blocks[0];
        assert_eq!(blk.block_type, IVHD_TYPE_10H);
        assert_eq!(blk.flags, 0x40);
        assert_eq!(blk.iommu_base_address, 0xFEB8_0000);
        assert_eq!(blk.device_entries.len(), 1);
        assert!(matches!(
            blk.device_entries[0],
            IvhdDeviceEntry::Select {
                device_id: 0x0030,
                data_setting: 0
            }
        ));
    }

    #[test]
    fn ivrs_decode_ivhd_11h_with_range() {
        let mut bytes = make_ivrs_prefix(1, 0);
        let mut entries = Vec::new();
        push_ivhd_short(&mut entries, IVHD_ENTRY_START_RANGE, 0x0100, 0);
        push_ivhd_short(&mut entries, IVHD_ENTRY_END_RANGE, 0x01FF, 0);
        let block = make_ivhd(
            IVHD_TYPE_11H,
            0,
            0x0018,
            0x40,
            0xFEB8_0000,
            0,
            0,
            0,
            &entries,
        );
        bytes.extend_from_slice(&block);
        finalize_table(&mut bytes);
        let tables = decode_ivrs(&bytes).unwrap();
        assert_eq!(tables.ivhd_blocks.len(), 1);
        let blk = &tables.ivhd_blocks[0];
        assert_eq!(blk.block_type, IVHD_TYPE_11H);
        assert_eq!(blk.device_entries.len(), 2);
        assert!(matches!(
            blk.device_entries[0],
            IvhdDeviceEntry::StartRange {
                device_id: 0x0100,
                ..
            }
        ));
        assert!(matches!(
            blk.device_entries[1],
            IvhdDeviceEntry::EndRange {
                device_id: 0x01FF,
                ..
            }
        ));
    }

    #[test]
    fn ivrs_decode_ivhd_40h_with_alias() {
        let mut bytes = make_ivrs_prefix(1, 0);
        let mut entries = Vec::new();
        push_ivhd_alias(&mut entries, IVHD_ENTRY_ALIAS_SELECT, 0x0200, 0, 0x0210);
        push_ivhd_alias(
            &mut entries,
            IVHD_ENTRY_ALIAS_START_RANGE,
            0x0300,
            0,
            0x0310,
        );
        let block = make_ivhd(
            IVHD_TYPE_40H,
            0x80,
            0x0018,
            0x40,
            0xFEB8_0000,
            0,
            0,
            0xDEAD_BEEF,
            &entries,
        );
        bytes.extend_from_slice(&block);
        finalize_table(&mut bytes);
        let tables = decode_ivrs(&bytes).unwrap();
        assert_eq!(tables.ivhd_blocks.len(), 1);
        let blk = &tables.ivhd_blocks[0];
        assert_eq!(blk.block_type, IVHD_TYPE_40H);
        assert_eq!(blk.iommu_feature_info, 0xDEAD_BEEF);
        assert_eq!(blk.device_entries.len(), 2);
        match blk.device_entries[0] {
            IvhdDeviceEntry::AliasSelect {
                device_id,
                alias_device_id,
                ..
            } => {
                assert_eq!(device_id, 0x0200);
                assert_eq!(alias_device_id, 0x0210);
            }
            _ => panic!("expected AliasSelect"),
        }
        match blk.device_entries[1] {
            IvhdDeviceEntry::AliasStartRange {
                device_id,
                alias_device_id,
                ..
            } => {
                assert_eq!(device_id, 0x0300);
                assert_eq!(alias_device_id, 0x0310);
            }
            _ => panic!("expected AliasStartRange"),
        }
    }

    #[test]
    fn ivrs_unknown_block_is_counted() {
        let mut bytes = make_ivrs_prefix(1, 0);
        // Use 0x50 — neither an IVHD nor an IVMD type — so the decoder
        // treats it as an unknown block.
        let mut unknown = Vec::new();
        unknown.push(0x50);
        unknown.push(0);
        push_u16(&mut unknown, 8);
        unknown.extend_from_slice(&[0u8; 4]);
        bytes.extend_from_slice(&unknown);
        finalize_table(&mut bytes);
        let tables = decode_ivrs(&bytes).unwrap();
        assert_eq!(tables.unknown_blocks, 1);
        assert!(tables.ivhd_blocks.is_empty());
        assert!(tables.ivmds.is_empty());
    }

    #[test]
    fn ivrs_truncated_header_returns_error() {
        let bytes = [0u8; 16];
        assert_eq!(
            decode_ivrs(&bytes).unwrap_err(),
            IvrsParseError::TruncatedHeader
        );
    }

    #[test]
    fn ivrs_invalid_checksum_returns_error() {
        let mut bytes = make_ivrs_prefix(1, 0);
        finalize_table(&mut bytes);
        bytes[9] = bytes[9].wrapping_add(1);
        assert_eq!(
            decode_ivrs(&bytes).unwrap_err(),
            IvrsParseError::InvalidChecksum
        );
    }

    #[test]
    fn ivrs_unknown_revision_returns_error() {
        let mut bytes = make_ivrs_prefix(0, 0);
        finalize_table(&mut bytes);
        assert_eq!(
            decode_ivrs(&bytes).unwrap_err(),
            IvrsParseError::UnknownRevision
        );
    }

    #[test]
    fn ivrs_truncated_block_returns_error() {
        let mut bytes = make_ivrs_prefix(1, 0);
        bytes.push(IVHD_TYPE_10H);
        bytes.push(0);
        push_u16(&mut bytes, 0xFFFF);
        finalize_table(&mut bytes);
        assert_eq!(
            decode_ivrs(&bytes).unwrap_err(),
            IvrsParseError::TruncatedSubTable
        );
    }

    #[test]
    fn ivrs_invalid_device_entry_returns_error() {
        let mut bytes = make_ivrs_prefix(1, 0);
        let mut entries = Vec::new();
        entries.extend_from_slice(&[5u8, 0u8, 0u8, 0u8]);
        let block = make_ivhd(IVHD_TYPE_10H, 0, 0, 0, 0, 0, 0, 0, &entries);
        bytes.extend_from_slice(&block);
        finalize_table(&mut bytes);
        assert_eq!(
            decode_ivrs(&bytes).unwrap_err(),
            IvrsParseError::InvalidDeviceScope
        );
    }

    // -----------------------------------------------------------------
    // IVMD unit tests
    // -----------------------------------------------------------------

    #[test]
    fn ivmd_decode_type_all_ignores_device_fields() {
        let mut bytes = make_ivrs_prefix(1, 0);
        let ivmd = make_ivmd(IVMD_TYPE_ALL, 0x01, 0xFFFF, 0xFFFF, 0xC000_0000, 0x1000);
        bytes.extend_from_slice(&ivmd);
        finalize_table(&mut bytes);
        let tables = decode_ivrs(&bytes).expect("IVMD_ALL decodes");
        assert_eq!(tables.ivmds.len(), 1);
        assert!(tables.ivhd_blocks.is_empty());
        let entry = &tables.ivmds[0];
        assert_eq!(entry.kind, IvmdKind::All);
        assert_eq!(entry.flags, 0x01);
        assert_eq!(entry.start_addr, 0xC000_0000);
        assert_eq!(entry.length, 0x1000);
    }

    #[test]
    fn ivmd_decode_type_select_captures_device_id() {
        let mut bytes = make_ivrs_prefix(1, 0);
        let ivmd = make_ivmd(IVMD_TYPE_SELECT, 0x09, 0x0134, 0, 0xFEE0_0000, 0x0010_0000);
        bytes.extend_from_slice(&ivmd);
        finalize_table(&mut bytes);
        let tables = decode_ivrs(&bytes).unwrap();
        assert_eq!(tables.ivmds.len(), 1);
        let entry = &tables.ivmds[0];
        assert_eq!(entry.kind, IvmdKind::Select { device_id: 0x0134 });
        assert_eq!(entry.flags, 0x09);
        assert_eq!(entry.start_addr, 0xFEE0_0000);
        assert_eq!(entry.length, 0x0010_0000);
    }

    #[test]
    fn ivmd_decode_type_range_captures_device_range() {
        let mut bytes = make_ivrs_prefix(1, 0);
        let ivmd = make_ivmd(IVMD_TYPE_RANGE, 0x00, 0x0100, 0x01FF, 0xD000_0000, 0x4000);
        bytes.extend_from_slice(&ivmd);
        finalize_table(&mut bytes);
        let tables = decode_ivrs(&bytes).unwrap();
        assert_eq!(tables.ivmds.len(), 1);
        let entry = &tables.ivmds[0];
        assert_eq!(
            entry.kind,
            IvmdKind::Range {
                start_device_id: 0x0100,
                end_device_id: 0x01FF,
            }
        );
        assert_eq!(entry.start_addr, 0xD000_0000);
        assert_eq!(entry.length, 0x4000);
    }

    #[test]
    fn ivmd_coexists_with_ivhd_blocks() {
        let mut bytes = make_ivrs_prefix(1, 0);
        // First: an IVHD 10h block with a Select entry.
        let mut entries = Vec::new();
        push_ivhd_short(&mut entries, IVHD_ENTRY_SELECT, 0x0018, 0);
        let block = make_ivhd(
            IVHD_TYPE_10H,
            0x40,
            0x0018,
            0x40,
            0xFEB8_0000,
            0,
            0,
            0,
            &entries,
        );
        bytes.extend_from_slice(&block);
        // Then: an IVMD SELECT entry.
        let ivmd = make_ivmd(IVMD_TYPE_SELECT, 0x01, 0x0020, 0, 0xE000_0000, 0x2000);
        bytes.extend_from_slice(&ivmd);
        finalize_table(&mut bytes);
        let tables = decode_ivrs(&bytes).expect("mixed IVHD + IVMD decodes");
        assert_eq!(tables.ivhd_blocks.len(), 1);
        assert_eq!(tables.ivmds.len(), 1);
        assert_eq!(tables.unknown_blocks, 0);
        assert_eq!(tables.ivmds[0].start_addr, 0xE000_0000);
    }

    #[test]
    fn ivmd_truncated_length_returns_error() {
        let mut bytes = make_ivrs_prefix(1, 0);
        // Declared length 16 is smaller than the 32-byte fixed IVMD header.
        bytes.push(IVMD_TYPE_ALL);
        bytes.push(0);
        push_u16(&mut bytes, 16);
        // Pad out to 16 bytes so the cursor walk doesn't bail on buffer overrun.
        bytes.extend_from_slice(&[0u8; 12]);
        finalize_table(&mut bytes);
        assert_eq!(
            decode_ivrs(&bytes).unwrap_err(),
            IvrsParseError::TruncatedSubTable
        );
    }

    #[test]
    fn ivmd_unknown_flag_bits_are_preserved() {
        let mut bytes = make_ivrs_prefix(1, 0);
        // All flag bits set — decoder must surface the raw byte verbatim
        // rather than masking it to known bits.
        let ivmd = make_ivmd(IVMD_TYPE_ALL, 0xFF, 0, 0, 0x1000, 0x2000);
        bytes.extend_from_slice(&ivmd);
        finalize_table(&mut bytes);
        let tables = decode_ivrs(&bytes).unwrap();
        assert_eq!(tables.ivmds.len(), 1);
        assert_eq!(tables.ivmds[0].flags, 0xFF);
    }

    #[test]
    fn ivmd_multiple_entries_are_returned_in_order() {
        let mut bytes = make_ivrs_prefix(1, 0);
        bytes.extend_from_slice(&make_ivmd(IVMD_TYPE_ALL, 0x01, 0, 0, 0x1000, 0x1000));
        bytes.extend_from_slice(&make_ivmd(
            IVMD_TYPE_SELECT,
            0x01,
            0x0030,
            0,
            0x2000,
            0x1000,
        ));
        bytes.extend_from_slice(&make_ivmd(
            IVMD_TYPE_RANGE,
            0x01,
            0x0100,
            0x01FF,
            0x3000,
            0x1000,
        ));
        finalize_table(&mut bytes);
        let tables = decode_ivrs(&bytes).unwrap();
        assert_eq!(tables.ivmds.len(), 3);
        assert_eq!(tables.ivmds[0].kind, IvmdKind::All);
        assert_eq!(tables.ivmds[1].kind, IvmdKind::Select { device_id: 0x0030 });
        assert_eq!(
            tables.ivmds[2].kind,
            IvmdKind::Range {
                start_device_id: 0x0100,
                end_device_id: 0x01FF,
            }
        );
    }

    // -----------------------------------------------------------------
    // Property tests — no panics, bounded output on arbitrary inputs.
    // -----------------------------------------------------------------

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        #[test]
        fn decode_dmar_is_panic_free_on_arbitrary_bytes(bytes in proptest::collection::vec(any::<u8>(), 0..512)) {
            let result = decode_dmar(&bytes);
            if let Ok(tables) = result {
                let max_items = bytes.len() / SUBTABLE_HEADER_LEN + 1;
                let total = tables.drhds.len()
                    + tables.rmrrs.len()
                    + tables.atsrs.len()
                    + tables.rhsas.len()
                    + tables.unknown_subtables as usize;
                prop_assert!(total <= max_items);
            }
        }

        #[test]
        fn decode_ivrs_is_panic_free_on_arbitrary_bytes(bytes in proptest::collection::vec(any::<u8>(), 0..512)) {
            let result = decode_ivrs(&bytes);
            if let Ok(tables) = result {
                let max_items = bytes.len() / 4 + 1;
                let total = tables.ivhd_blocks.len()
                    + tables.ivmds.len()
                    + tables.unknown_blocks as usize;
                prop_assert!(total <= max_items);
            }
        }

        /// Spot-check: inputs that explicitly seed IVMD-shaped bytes into
        /// random prefixes must not panic or consume unbounded resources.
        /// We prepend a small random prefix, then append a well-formed
        /// IVRS table carrying a random mix of IVHD and IVMD sub-tables.
        #[test]
        fn decode_ivrs_handles_mixed_ivhd_ivmd_blobs(
            prefix in proptest::collection::vec(any::<u8>(), 0..32),
            ivmd_count in 0usize..4,
            ivhd_count in 0usize..3,
        ) {
            let mut bytes = make_ivrs_prefix(1, 0);
            for i in 0..ivhd_count {
                let block = make_ivhd(
                    IVHD_TYPE_10H,
                    0,
                    0,
                    0,
                    0xFEB8_0000 + (i as u64) * 0x1000,
                    0,
                    0,
                    0,
                    &[],
                );
                bytes.extend_from_slice(&block);
            }
            for i in 0..ivmd_count {
                let ty = match i % 3 {
                    0 => IVMD_TYPE_ALL,
                    1 => IVMD_TYPE_SELECT,
                    _ => IVMD_TYPE_RANGE,
                };
                bytes.extend_from_slice(&make_ivmd(
                    ty,
                    (i as u8) & 0x0F,
                    i as u16,
                    (i as u16).wrapping_add(1),
                    (i as u64) * 0x1000,
                    0x1000,
                ));
            }
            finalize_table(&mut bytes);
            let tables = decode_ivrs(&bytes).expect("well-formed mixed blob decodes");
            prop_assert_eq!(tables.ivhd_blocks.len(), ivhd_count);
            prop_assert_eq!(tables.ivmds.len(), ivmd_count);

            // Also: random prefix + garbage must never panic.
            let mut garbage = prefix.clone();
            garbage.extend_from_slice(&bytes);
            let _ = decode_ivrs(&garbage);
        }
    }
}
