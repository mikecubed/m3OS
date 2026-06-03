//! ATA opcodes, H2D Register FIS command encoders, and the `IDENTIFY DEVICE`
//! response parser (Phase 82 Track A.4).
//!
//! The H2D Register FIS is the single command channel to the drive. The
//! `fis_type = 0x27` byte is validated by QEMU's `ich9-ahci` and every real HBA
//! (a zero/wrong type makes the HBA reject the command), and an LBA byte split
//! error or a missing C-bit yields a misaddressed transfer or a control update
//! the drive ignores. Encoding each command once in a host-tested function —
//! with `fis_type = 0x27`, `device = 1 << 6` (LBA48), and the C-bit hard-wired —
//! guarantees every command in the driver's Track C data path is well-formed.

#![allow(dead_code)] // the out-of-tree `ahci` driver crate consumes these.

use super::ahci::{FIS_H2D_C_BIT, FIS_TYPE_REG_H2D, FisRegH2D};

// ===========================================================================
// ATA command opcodes (include/linux/ata.h)
// ===========================================================================

/// READ DMA EXT — 48-bit-LBA DMA read.
pub const ATA_CMD_READ_DMA_EXT: u8 = 0x25;
/// WRITE DMA EXT — 48-bit-LBA DMA write.
pub const ATA_CMD_WRITE_DMA_EXT: u8 = 0x35;
/// IDENTIFY DEVICE — return the 256-word device-identification block.
pub const ATA_CMD_IDENTIFY: u8 = 0xEC;
/// IDENTIFY PACKET DEVICE — the ATAPI variant (out of 1.0 scope; here for
/// completeness so the classifier and a future ATAPI path share the constant).
pub const ATA_CMD_IDENTIFY_PACKET: u8 = 0xA1;
/// FLUSH CACHE EXT — force the drive's volatile write cache to media. Required
/// for write durability (a `WRITE DMA EXT` completion only reaches the cache).
pub const ATA_CMD_FLUSH_CACHE_EXT: u8 = 0xEA;

/// The `device` register value selecting LBA addressing mode (bit 6).
pub const ATA_DEVICE_LBA: u8 = 1 << 6;

// ===========================================================================
// A.4 — H2D Register FIS encoders
// ===========================================================================

/// Build the H2D Register FIS for a `READ DMA EXT` (`write == false`) or
/// `WRITE DMA EXT` (`write == true`) command.
///
/// Hard-wires `fis_type = 0x27`, `device = 1 << 6` (LBA48 mode), and the C-bit;
/// splits the 48-bit `lba` across `lba0..lba5`; and packs the 16-bit `sectors`
/// count across `countl`/`counth`. `debug_assert`-guards `sectors == 0` because
/// an LBA48 count of 0 means **65536** sectors, an implicit-large-transfer
/// footgun this driver forbids.
#[inline]
pub fn encode_rw_fis(write: bool, lba: u64, sectors: u16) -> FisRegH2D {
    debug_assert!(sectors != 0, "encode_rw_fis: LBA48 count 0 means 65536");
    let mut fis = FisRegH2D {
        fis_type: FIS_TYPE_REG_H2D,
        pm_c: FIS_H2D_C_BIT,
        command: if write {
            ATA_CMD_WRITE_DMA_EXT
        } else {
            ATA_CMD_READ_DMA_EXT
        },
        device: ATA_DEVICE_LBA,
        ..FisRegH2D::default()
    };
    let lba = lba & 0xFFFF_FFFF_FFFF; // 48-bit
    fis.lba0 = (lba & 0xFF) as u8;
    fis.lba1 = ((lba >> 8) & 0xFF) as u8;
    fis.lba2 = ((lba >> 16) & 0xFF) as u8;
    fis.lba3 = ((lba >> 24) & 0xFF) as u8;
    fis.lba4 = ((lba >> 32) & 0xFF) as u8;
    fis.lba5 = ((lba >> 40) & 0xFF) as u8;
    fis.countl = (sectors & 0xFF) as u8;
    fis.counth = ((sectors >> 8) & 0xFF) as u8;
    fis
}

/// Build the H2D Register FIS for `IDENTIFY DEVICE` (`0xEC`). Non-LBA: the data
/// returns via the PRDT, so the LBA / count fields stay zero.
#[inline]
pub fn encode_identify_fis() -> FisRegH2D {
    FisRegH2D {
        fis_type: FIS_TYPE_REG_H2D,
        pm_c: FIS_H2D_C_BIT,
        command: ATA_CMD_IDENTIFY,
        ..FisRegH2D::default()
    }
}

/// Build the H2D Register FIS for `FLUSH CACHE EXT` (`0xEA`). A non-data
/// command: the driver issues it with `PRDTL == 0` and waits for completion
/// before reporting a write durable. LBA / count stay zero.
#[inline]
pub fn encode_flush_fis() -> FisRegH2D {
    FisRegH2D {
        fis_type: FIS_TYPE_REG_H2D,
        pm_c: FIS_H2D_C_BIT,
        command: ATA_CMD_FLUSH_CACHE_EXT,
        device: ATA_DEVICE_LBA,
        ..FisRegH2D::default()
    }
}

// ===========================================================================
// A.4 — IDENTIFY DEVICE response parser
// ===========================================================================

/// The fields the driver extracts from a 256-word `IDENTIFY DEVICE` block to
/// size the block device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AtaIdentify {
    /// Total addressable sectors (LBA48), assembled from words 100–103.
    pub lba48_sectors: u64,
    /// Logical sector size in bytes (word 106 / words 117–118; default 512).
    pub logical_sector_bytes: u32,
    /// `true` when the 48-bit Address feature set is supported (word 83 bit 10).
    pub supports_lba48: bool,
    /// `true` when FLUSH CACHE EXT is supported (word 83 bit 13).
    pub has_flush_ext: bool,
}

impl AtaIdentify {
    /// Computed capacity in bytes: `lba48_sectors * logical_sector_bytes`.
    #[inline]
    pub const fn capacity_bytes(&self) -> u64 {
        self.lba48_sectors * self.logical_sector_bytes as u64
    }
}

/// Word 83 — "Commands and feature sets supported".
const WORD_CMDSET83: usize = 83;
/// Word 83 bit 10 — 48-bit Address feature set supported.
const CMDSET83_LBA48: u16 = 1 << 10;
/// Word 83 bit 13 — FLUSH CACHE EXT supported.
const CMDSET83_FLUSH_EXT: u16 = 1 << 13;

/// Word 106 — "Physical / Logical Sector Size".
const WORD_SECTOR_SIZE: usize = 106;
/// Word 106 bit 14 must be 1 and bit 15 must be 0 for the word to be valid.
const SECTOR_SIZE_VALID_SET: u16 = 1 << 14;
const SECTOR_SIZE_VALID_CLEAR: u16 = 1 << 15;
/// Word 106 bit 12 — "Logical Sector longer than 256 words" (use words 117–118).
const SECTOR_SIZE_LONG_LOGICAL: u16 = 1 << 12;

/// Default logical sector size when the drive does not report a larger one.
pub const DEFAULT_LOGICAL_SECTOR_BYTES: u32 = 512;

/// Parse an `IDENTIFY DEVICE` response (256 little-endian words) into the
/// capacity / LBA48 / flush / sector-size facts the block device needs.
///
/// * `lba48_sectors` — assembled from words 100–103 (word 100 = bits 15:0 … word
///   103 = bits 63:48).
/// * `logical_sector_bytes` — word 106 if it indicates a >256-word logical
///   sector (words 117–118 give the size in words; ×2 for bytes); otherwise
///   [`DEFAULT_LOGICAL_SECTOR_BYTES`] (512, which is what QEMU `ide-hd`
///   reports).
/// * `supports_lba48` / `has_flush_ext` — word-83 command-set bits.
pub fn parse_identify(buf: &[u16; 256]) -> AtaIdentify {
    let lba48_sectors = (buf[100] as u64)
        | ((buf[101] as u64) << 16)
        | ((buf[102] as u64) << 32)
        | ((buf[103] as u64) << 48);

    let w106 = buf[WORD_SECTOR_SIZE];
    let word_valid = (w106 & SECTOR_SIZE_VALID_SET) != 0 && (w106 & SECTOR_SIZE_VALID_CLEAR) == 0;
    let logical_sector_bytes = if word_valid && (w106 & SECTOR_SIZE_LONG_LOGICAL) != 0 {
        // Words 117–118 carry the logical sector size in 16-bit words.
        let words = (buf[117] as u32) | ((buf[118] as u32) << 16);
        words.saturating_mul(2).max(DEFAULT_LOGICAL_SECTOR_BYTES)
    } else {
        DEFAULT_LOGICAL_SECTOR_BYTES
    };

    let cmdset83 = buf[WORD_CMDSET83];
    AtaIdentify {
        lba48_sectors,
        logical_sector_bytes,
        supports_lba48: cmdset83 & CMDSET83_LBA48 != 0,
        has_flush_ext: cmdset83 & CMDSET83_FLUSH_EXT != 0,
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fis_type_is_h2d() {
        assert_eq!(encode_rw_fis(false, 0, 1).fis_type, 0x27);
        assert_eq!(encode_rw_fis(true, 0, 1).fis_type, 0x27);
        assert_eq!(encode_identify_fis().fis_type, 0x27);
        assert_eq!(encode_flush_fis().fis_type, 0x27);
    }

    #[test]
    fn rw_fis_lba48_split() {
        let fis = encode_rw_fis(false, 0x01_0203_0405, 8);
        assert_eq!(fis.command, 0x25); // READ DMA EXT
        assert_eq!(fis.device, 0x40); // LBA48 mode
        assert_eq!(fis.lba0, 0x05);
        assert_eq!(fis.lba1, 0x04);
        assert_eq!(fis.lba2, 0x03);
        assert_eq!(fis.lba3, 0x02);
        assert_eq!(fis.lba4, 0x01);
        assert_eq!(fis.lba5, 0x00);
        assert_eq!(fis.countl, 8);
        assert_eq!(fis.counth, 0);
        // The C-bit must be set.
        assert_eq!(fis.pm_c & 0x80, 0x80);

        // The write variant flips only the opcode.
        let w = encode_rw_fis(true, 0x01_0203_0405, 8);
        assert_eq!(w.command, 0x35); // WRITE DMA EXT
        assert_eq!(w.device, 0x40);
        assert_eq!(w.lba0, 0x05);
        assert_eq!(w.pm_c & 0x80, 0x80);

        // A multi-block count straddling the byte boundary packs correctly.
        let big = encode_rw_fis(false, 0, 0x0140); // 320 sectors
        assert_eq!(big.countl, 0x40);
        assert_eq!(big.counth, 0x01);
    }

    #[test]
    #[should_panic(expected = "65536")]
    fn rw_fis_rejects_zero_count() {
        // An LBA48 count of 0 means 65536 sectors — a debug-asserted footgun.
        let _ = encode_rw_fis(false, 0, 0);
    }

    #[test]
    fn identify_fis() {
        let fis = encode_identify_fis();
        assert_eq!(fis.command, 0xEC);
        assert_eq!(fis.pm_c & 0x80, 0x80); // C-bit
        // Non-LBA: no addressing fields set.
        assert_eq!(fis.lba0, 0);
        assert_eq!(fis.lba5, 0);
        assert_eq!(fis.countl, 0);
        assert_eq!(fis.counth, 0);
    }

    #[test]
    fn flush_fis_is_non_data() {
        let fis = encode_flush_fis();
        assert_eq!(fis.command, 0xEA);
        assert_eq!(fis.pm_c & 0x80, 0x80); // C-bit
        // No LBA / count payload — the driver issues it with PRDTL == 0.
        assert_eq!(fis.lba0, 0);
        assert_eq!(fis.lba1, 0);
        assert_eq!(fis.lba2, 0);
        assert_eq!(fis.lba3, 0);
        assert_eq!(fis.lba4, 0);
        assert_eq!(fis.lba5, 0);
        assert_eq!(fis.countl, 0);
        assert_eq!(fis.counth, 0);
    }

    /// Build a synthetic IDENTIFY block: `sectors` LBA48 capacity, 512-byte
    /// logical sectors (word 106 = bit 14 only), with the LBA48 + FLUSH-EXT
    /// command-set bits set.
    fn synthetic_identify(sectors: u64) -> [u16; 256] {
        let mut buf = [0u16; 256];
        buf[100] = (sectors & 0xFFFF) as u16;
        buf[101] = ((sectors >> 16) & 0xFFFF) as u16;
        buf[102] = ((sectors >> 32) & 0xFFFF) as u16;
        buf[103] = ((sectors >> 48) & 0xFFFF) as u16;
        // Word 83: 48-bit Address feature set (bit 10) + FLUSH CACHE EXT (bit 13),
        // plus the mandatory bit 14 == 1 / bit 15 == 0 validity for word 83.
        buf[83] = (1 << 14) | CMDSET83_LBA48 | CMDSET83_FLUSH_EXT;
        // Word 106: valid (bit 14 set, bit 15 clear), standard 512-byte sectors
        // (bit 12 clear) — exactly what QEMU `ide-hd` reports.
        buf[106] = 1 << 14;
        buf
    }

    #[test]
    fn parse_identify_capacity() {
        let buf = synthetic_identify(0x0010_0000); // 1 Mi sectors
        let id = parse_identify(&buf);
        assert_eq!(id.lba48_sectors, 0x0010_0000);
        assert!(id.supports_lba48);
        assert!(id.has_flush_ext);
        assert_eq!(id.logical_sector_bytes, 512);
        // Computed capacity = sectors * sector bytes.
        assert_eq!(id.capacity_bytes(), 0x0010_0000 * 512);

        // A full 48-bit capacity assembles across all four words.
        let buf2 = synthetic_identify(0x0001_0203_0405);
        let id2 = parse_identify(&buf2);
        assert_eq!(id2.lba48_sectors, 0x0001_0203_0405);
    }

    #[test]
    fn parse_identify_default_512() {
        // Word 106 indicating standard 512-byte sectors → default 512.
        let buf = synthetic_identify(2048);
        let id = parse_identify(&buf);
        assert_eq!(id.logical_sector_bytes, 512);

        // A drive with no valid word 106 (all zero) also defaults to 512.
        let mut buf2 = synthetic_identify(2048);
        buf2[106] = 0;
        assert_eq!(parse_identify(&buf2).logical_sector_bytes, 512);
    }

    #[test]
    fn parse_identify_large_logical_sector() {
        // Word 106 valid + bit 12 set → use words 117–118 (×2 for bytes).
        let mut buf = synthetic_identify(1000);
        buf[106] = (1 << 14) | (1 << 12);
        buf[117] = 2048; // 2048 words = 4096 bytes
        buf[118] = 0;
        let id = parse_identify(&buf);
        assert_eq!(id.logical_sector_bytes, 4096);
        assert_eq!(id.capacity_bytes(), 1000 * 4096);
    }

    #[test]
    fn parse_identify_flush_capability_gating() {
        // Without the FLUSH-EXT command-set bit, has_flush_ext is false.
        let mut buf = synthetic_identify(64);
        buf[83] = (1 << 14) | CMDSET83_LBA48; // LBA48 but no flush-ext
        let id = parse_identify(&buf);
        assert!(id.supports_lba48);
        assert!(!id.has_flush_ext);
    }
}
