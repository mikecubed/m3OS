//! mt792x connac2 firmware parsers — Task A.4.
//!
//! Implements host-testable, bounds-checked parsers for the two firmware blobs
//! the mt792x driver loads:
//!
//! * **ROM-patch** (`mt76_connac2_patch_hdr` + `mt76_connac2_patch_sec`):
//!   big-endian multi-byte fields; parsed with `from_be_bytes`.
//! * **RAM code** (`mt76_connac2_fw_trailer` + `mt76_connac2_fw_region`):
//!   little-endian multi-byte fields; parsed with `from_le_bytes`; the trailer
//!   is located at the **end** of the blob and the region table immediately
//!   precedes it.
//!
//! # Safety
//!
//! No `unsafe`, no `transmute`. Every access is bounds-checked; every out-of-
//! range condition maps to a [`FirmwareError`] variant so the driver can
//! degrade gracefully rather than panicking on a corrupt or truncated blob.
//!
//! # C layout reference (mt76 upstream)
//!
//! ```c
//! struct mt76_connac2_patch_hdr {          // BIG-ENDIAN multi-byte fields
//!   char     build_date[16];               // @ 0x00
//!   char     platform[4];                  // @ 0x10
//!   __be32   hw_sw_ver;                    // @ 0x14
//!   __be32   patch_ver;                    // @ 0x18
//!   __be16   checksum;                     // @ 0x1C
//!   u16      reserved;                     // @ 0x1E
//!   struct { __be32 patch_ver; __be32 subsys; __be32 feature;
//!             __be32 n_region;  __be32 crc;
//!             u32 reserved[11]; } desc;    // @ 0x20  (5+11 = 64 bytes)
//! } __packed;                              // total: 16+4+4+4+2+2+64 = 96 bytes
//!
//! struct mt76_connac2_patch_sec {          // BIG-ENDIAN; n_region entries after hdr
//!   __be32 type; __be32 offs; __be32 size;
//!   union { __be32 spec[13]; struct { __be32 addr; __be32 len;
//!           __be32 sec_key_idx; __be32 align_len;
//!           u32 reserved[9]; } info; };    // 12 + 52 = 64 bytes
//! } __packed;
//!
//! struct mt76_connac2_fw_trailer {         // LITTLE-ENDIAN; at END of RAM blob
//!   u8 chip_id; u8 eco_code; u8 n_region; u8 format_ver;
//!   u8 format_flag; u8 reserved[2];
//!   char fw_ver[10]; char build_date[15]; __le32 crc;
//! } __packed;                              // 1+1+1+1+1+2+10+15+4 = 36 bytes
//!
//! struct mt76_connac2_fw_region {          // LITTLE-ENDIAN; n_region before trailer
//!   __le32 decomp_crc; __le32 decomp_len; __le32 decomp_blk_sz;
//!   u8 reserved[4]; __le32 addr; __le32 len;
//!   u8 feature_set; u8 reserved1[15];
//! } __packed;                              // 4+4+4+4+4+4+1+15 = 40 bytes
//! ```

#![allow(dead_code)]

extern crate alloc;
use alloc::vec::Vec;

// ---------------------------------------------------------------------------
// Layout constants
// ---------------------------------------------------------------------------

/// `mt76_connac2_patch_hdr` total size: 96 bytes.
const PATCH_HDR_SIZE: usize = 96;
/// Byte offset of `hw_sw_ver` (BE u32) within the patch header.
const PATCH_HDR_HW_SW_VER_OFF: usize = 0x14;
/// Byte offset of `patch_ver` (BE u32) within the patch header.
const PATCH_HDR_PATCH_VER_OFF: usize = 0x18;
/// Byte offset of `desc.n_region` (BE u32) within the patch header.
/// desc starts at 0x20; n_region is the fourth field (4*3 = 12 bytes in).
const PATCH_HDR_N_REGION_OFF: usize = 0x20 + 12;

/// `mt76_connac2_patch_sec` total size: 64 bytes.
const PATCH_SEC_SIZE: usize = 64;
/// Byte offset of `type` (BE u32) within a patch_sec entry.
const PATCH_SEC_TYPE_OFF: usize = 0x00;
/// Byte offset of `offs` (BE u32) — the section's byte offset within the blob.
const PATCH_SEC_OFFS_OFF: usize = 0x04;
/// Byte offset of `size` (BE u32) — the section's byte length within the blob.
const PATCH_SEC_SIZE_OFF: usize = 0x08;
/// Byte offset of `info.addr` (BE u32) — the section's MCU load address.
const PATCH_SEC_ADDR_OFF: usize = 0x0C;
/// Byte offset of `info.len` (BE u32) — the destination length.
const PATCH_SEC_LEN_OFF: usize = 0x10;

/// Maximum sensible n_region for a ROM-patch blob.
const PATCH_MAX_REGIONS: u32 = 64;

/// `mt76_connac2_fw_trailer` size: 36 bytes.
const FW_TRAILER_SIZE: usize = 36;
/// Byte offset of `n_region` (u8) within the trailer (counted from trailer start).
const FW_TRAILER_N_REGION_OFF: usize = 2;
/// Byte offset of `chip_id` (u8) within the trailer.
const FW_TRAILER_CHIP_ID_OFF: usize = 0;

/// `mt76_connac2_fw_region` size: 40 bytes.
const FW_REGION_SIZE: usize = 40;
/// Byte offset of `addr` (LE u32) within a fw_region entry.
const FW_REGION_ADDR_OFF: usize = 16;
/// Byte offset of `len` (LE u32) within a fw_region entry.
const FW_REGION_LEN_OFF: usize = 20;
/// Byte offset of `feature_set` (u8) within a fw_region entry.
const FW_REGION_FEATURE_SET_OFF: usize = 24;

// ---------------------------------------------------------------------------
// Public constants
// ---------------------------------------------------------------------------

/// Target IOVA for the ROM-patch load (CPU address in the MCU's address map).
pub const MCU_PATCH_ADDRESS: u32 = 0x200000;

/// Scatter-DMA chunk size used when uploading firmware to the MCU (4 KiB).
pub const FW_SCATTER_CHUNK: usize = 4096;

// `feature_set` bit flags, matching upstream mt76
// (`drivers/net/wireless/mediatek/mt76/mt76_connac_mcu.h`):
//   FW_FEATURE_SET_ENCRYPT   BIT(0)
//   FW_FEATURE_SET_KEY_IDX   GENMASK(2, 1)
//   FW_FEATURE_ENCRY_MODE    BIT(4)
//   FW_FEATURE_OVERRIDE_ADDR BIT(5)

/// `feature_set` flag: the region image is encrypted (retail blobs).
pub const FW_FEATURE_SET_ENCRYPT: u8 = 1 << 0;
/// `feature_set` mask: encryption key index (`GENMASK(2, 1)`).
pub const FW_FEATURE_SET_KEY_IDX: u8 = 0b110;
/// `feature_set` flag: encryption mode selector.
pub const FW_FEATURE_ENCRY_MODE: u8 = 1 << 4;
/// `feature_set` flag: region specifies an override load address (`BIT(5)`).
pub const FW_FEATURE_OVERRIDE_ADDR: u8 = 1 << 5;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Reasons a firmware blob may be rejected by the parsers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FirmwareError {
    /// The blob is shorter than the minimum required for even the header.
    TooShort,
    /// A magic / version field did not match the expected pattern.
    BadMagic,
    /// The `n_region` field is zero, exceeds the implementation cap, or
    /// its associated region table extends outside the blob.
    BadRegionCount,
    /// An integrated checksum check failed.
    BadChecksum,
    /// A region's declared address or length is not properly aligned.
    UnalignedRegion,
    /// A region table or trailer reference falls outside the blob bounds.
    TrailerOutOfBounds,
}

// ---------------------------------------------------------------------------
// ROM-patch parser
// ---------------------------------------------------------------------------

/// Parsed fields from a `mt76_connac2_patch_hdr`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PatchHdr {
    /// Combined hardware/software version (big-endian u32 at offset 0x14).
    pub hw_sw_ver: u32,
    /// Patch version (big-endian u32 at offset 0x18).
    pub patch_ver: u32,
    /// Number of patch sections (`desc.n_region`, BE u32 in the desc sub-struct).
    pub n_region: u32,
}

/// Parse the `mt76_connac2_patch_hdr` from the start of `blob`.
///
/// Validates that the blob is large enough to contain the header plus all
/// `n_region` patch-section entries. Returns [`FirmwareError::TooShort`] when
/// the header itself doesn't fit, and [`FirmwareError::BadRegionCount`] when
/// `n_region` is zero, exceeds [`PATCH_MAX_REGIONS`], or the computed
/// section table extends past the end of the blob.
pub fn parse_patch_header(blob: &[u8]) -> Result<PatchHdr, FirmwareError> {
    if blob.len() < PATCH_HDR_SIZE {
        return Err(FirmwareError::TooShort);
    }

    let hw_sw_ver = u32::from_be_bytes([
        blob[PATCH_HDR_HW_SW_VER_OFF],
        blob[PATCH_HDR_HW_SW_VER_OFF + 1],
        blob[PATCH_HDR_HW_SW_VER_OFF + 2],
        blob[PATCH_HDR_HW_SW_VER_OFF + 3],
    ]);
    let patch_ver = u32::from_be_bytes([
        blob[PATCH_HDR_PATCH_VER_OFF],
        blob[PATCH_HDR_PATCH_VER_OFF + 1],
        blob[PATCH_HDR_PATCH_VER_OFF + 2],
        blob[PATCH_HDR_PATCH_VER_OFF + 3],
    ]);
    let n_region = u32::from_be_bytes([
        blob[PATCH_HDR_N_REGION_OFF],
        blob[PATCH_HDR_N_REGION_OFF + 1],
        blob[PATCH_HDR_N_REGION_OFF + 2],
        blob[PATCH_HDR_N_REGION_OFF + 3],
    ]);

    if n_region == 0 || n_region > PATCH_MAX_REGIONS {
        return Err(FirmwareError::BadRegionCount);
    }

    // Verify the blob holds the header + all patch_sec entries.
    let sections_size = (n_region as usize)
        .checked_mul(PATCH_SEC_SIZE)
        .ok_or(FirmwareError::BadRegionCount)?;
    let required = PATCH_HDR_SIZE
        .checked_add(sections_size)
        .ok_or(FirmwareError::BadRegionCount)?;
    if blob.len() < required {
        return Err(FirmwareError::BadRegionCount);
    }

    Ok(PatchHdr {
        hw_sw_ver,
        patch_ver,
        n_region,
    })
}

/// A parsed `mt76_connac2_patch_sec` entry.
///
/// Every section carries its **own** MCU load address (`addr`) — the ROM-patch
/// download must `PATCH_START_REQ` each section at `addr` and scatter-upload the
/// slice `blob[offs..offs + size]`, not a single fixed `MCU_PATCH_ADDRESS` for
/// all sections.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PatchSec {
    /// Section type discriminator (`type` field).
    pub typ: u32,
    /// Byte offset of this section's data within the patch blob.
    pub offs: u32,
    /// Byte length of this section's data within the patch blob.
    pub size: u32,
    /// MCU load address for this section (`info.addr`).
    pub addr: u32,
    /// Destination length (`info.len`).
    pub len: u32,
}

/// Parse the `n_region` `mt76_connac2_patch_sec` entries that follow the
/// 96-byte patch header.
///
/// All multi-byte fields are **big-endian** (`from_be_bytes`). Each section's
/// `[offs, offs + size)` slice is bounds-checked against `blob.len()` so a
/// corrupt or hostile blob yields a [`FirmwareError`] rather than an out-of-
/// bounds slice at download time. `n_region` is the value returned by
/// [`parse_patch_header`].
pub fn parse_patch_sections(blob: &[u8], n_region: u32) -> Result<Vec<PatchSec>, FirmwareError> {
    if n_region == 0 || n_region > PATCH_MAX_REGIONS {
        return Err(FirmwareError::BadRegionCount);
    }
    let sections_size = (n_region as usize)
        .checked_mul(PATCH_SEC_SIZE)
        .ok_or(FirmwareError::BadRegionCount)?;
    let required = PATCH_HDR_SIZE
        .checked_add(sections_size)
        .ok_or(FirmwareError::BadRegionCount)?;
    if blob.len() < required {
        return Err(FirmwareError::BadRegionCount);
    }

    let be32 = |off: usize| -> u32 {
        u32::from_be_bytes([blob[off], blob[off + 1], blob[off + 2], blob[off + 3]])
    };

    let mut secs = Vec::with_capacity(n_region as usize);
    for i in 0..n_region as usize {
        let base = PATCH_HDR_SIZE + i * PATCH_SEC_SIZE;
        let typ = be32(base + PATCH_SEC_TYPE_OFF);
        let offs = be32(base + PATCH_SEC_OFFS_OFF);
        let size = be32(base + PATCH_SEC_SIZE_OFF);
        let addr = be32(base + PATCH_SEC_ADDR_OFF);
        let len = be32(base + PATCH_SEC_LEN_OFF);

        // The section's [offs, offs + size) slice must lie within the blob.
        let end = (offs as usize)
            .checked_add(size as usize)
            .ok_or(FirmwareError::TrailerOutOfBounds)?;
        if end > blob.len() {
            return Err(FirmwareError::TrailerOutOfBounds);
        }
        secs.push(PatchSec {
            typ,
            offs,
            size,
            addr,
            len,
        });
    }
    Ok(secs)
}

// ---------------------------------------------------------------------------
// RAM-code (fw_trailer) parser
// ---------------------------------------------------------------------------

/// A parsed `mt76_connac2_fw_region` entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FwRegion {
    /// Load address for this region in the MCU's address space.
    pub addr: u32,
    /// Byte length of the region data.
    pub len: u32,
    /// Feature flags (e.g. [`FW_FEATURE_OVERRIDE_ADDR`]).
    pub feature_set: u8,
    /// Region type (0 when not modelled; reserved for future use).
    pub typ: u32,
}

/// A parsed RAM-code image: trailer metadata plus the per-region descriptors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FwImage {
    /// Number of regions declared in the trailer.
    pub n_region: u8,
    /// Chip ID from the trailer.
    pub chip_id: u8,
    /// Parsed region descriptors (length == `n_region`).
    pub regions: Vec<FwRegion>,
}

/// Parse the `mt76_connac2_fw_trailer` and region table from the **end** of
/// `blob`.
///
/// Layout (from the end of the blob, working backwards):
/// ```text
/// [... firmware body ...][fw_region × n_region][fw_trailer (36 B)]
/// ```
///
/// # Bounds checking
///
/// * `blob.len() < FW_TRAILER_SIZE` → [`FirmwareError::TooShort`].
/// * `n_region == 0` → [`FirmwareError::BadRegionCount`].
/// * Region table start < 0 or outside the firmware body →
///   [`FirmwareError::TrailerOutOfBounds`].
/// * Any region whose declared `len` exceeds the remaining firmware body
///   before the region table → [`FirmwareError::TrailerOutOfBounds`].
pub fn parse_fw_trailer(blob: &[u8]) -> Result<FwImage, FirmwareError> {
    if blob.len() < FW_TRAILER_SIZE {
        return Err(FirmwareError::TooShort);
    }

    // Trailer is at the very end of the blob.
    let trailer_start = blob.len() - FW_TRAILER_SIZE;
    let trailer = &blob[trailer_start..];

    let chip_id = trailer[FW_TRAILER_CHIP_ID_OFF];
    let n_region = trailer[FW_TRAILER_N_REGION_OFF];

    if n_region == 0 {
        return Err(FirmwareError::BadRegionCount);
    }

    // Region table immediately precedes the trailer.
    let region_table_size = (n_region as usize)
        .checked_mul(FW_REGION_SIZE)
        .ok_or(FirmwareError::TrailerOutOfBounds)?;
    let region_table_start = trailer_start
        .checked_sub(region_table_size)
        .ok_or(FirmwareError::TrailerOutOfBounds)?;

    // The firmware body is everything before the region table.
    let fw_body_end = region_table_start;

    let mut regions = Vec::with_capacity(n_region as usize);
    for i in 0..n_region as usize {
        let base = region_table_start + i * FW_REGION_SIZE;
        // Ensure we can read the full entry (should always hold given the
        // bounds check above, but be explicit).
        if base + FW_REGION_SIZE > trailer_start {
            return Err(FirmwareError::TrailerOutOfBounds);
        }

        let addr = u32::from_le_bytes([
            blob[base + FW_REGION_ADDR_OFF],
            blob[base + FW_REGION_ADDR_OFF + 1],
            blob[base + FW_REGION_ADDR_OFF + 2],
            blob[base + FW_REGION_ADDR_OFF + 3],
        ]);
        let len = u32::from_le_bytes([
            blob[base + FW_REGION_LEN_OFF],
            blob[base + FW_REGION_LEN_OFF + 1],
            blob[base + FW_REGION_LEN_OFF + 2],
            blob[base + FW_REGION_LEN_OFF + 3],
        ]);
        let feature_set = blob[base + FW_REGION_FEATURE_SET_OFF];

        // Validate: region's declared len must not exceed the firmware body.
        if len as usize > fw_body_end {
            return Err(FirmwareError::TrailerOutOfBounds);
        }

        regions.push(FwRegion {
            addr,
            len,
            feature_set,
            typ: 0,
        });
    }

    Ok(FwImage {
        n_region,
        chip_id,
        regions,
    })
}

// ---------------------------------------------------------------------------
// Patch semaphore model
// ---------------------------------------------------------------------------

/// State of the MCU patch semaphore, used to decide whether a ROM-patch
/// download is needed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatchSem {
    /// The MCU already has the ROM-patch loaded (semaphore = IsDl).
    IsDl,
    /// The semaphore was acquired successfully and the ROM-patch must be
    /// downloaded now.
    NotDlSemSuccess,
}

/// Return the number of patch sections to download given the semaphore state.
///
/// * `IsDl` → 0 (ROM-patch already present; skip the download).
/// * `NotDlSemSuccess` → `n_region` (download all sections).
#[inline]
pub fn patch_sections_to_download(sem: PatchSem, n_region: u32) -> u32 {
    match sem {
        PatchSem::IsDl => 0,
        PatchSem::NotDlSemSuccess => n_region,
    }
}

// ---------------------------------------------------------------------------
// Scatter-upload helpers
// ---------------------------------------------------------------------------

/// Compute the number of [`FW_SCATTER_CHUNK`]-sized DMA transfers needed for
/// `len` bytes (ceiling division; 0 bytes → 0 transfers).
#[inline]
pub fn scatter_chunk_count(len: usize) -> usize {
    len.div_ceil(FW_SCATTER_CHUNK)
}

// ---------------------------------------------------------------------------
// Firmware-set selector
// ---------------------------------------------------------------------------

/// Lifetime-bound pair of ROM-patch and RAM-code byte slices.
///
/// Used by the driver to pass the two blobs through the MCU upload path in a
/// single call. The `'a` lifetime is typically `'static` for firmware blobs
/// embedded in the driver binary.
pub struct FirmwareSet<'a> {
    /// The connac2 ROM-patch blob.
    pub rom_patch: &'a [u8],
    /// The connac2 RAM code blob.
    pub ram_code: &'a [u8],
}

/// Return the expected firmware filename stem for a given PCI device ID.
///
/// The driver appends `.bin` (and, on Linux, decompresses `.zst`) to locate
/// the blob pair in the firmware search path.
///
/// Returns `None` for unrecognized device IDs.
pub fn select_firmware_set(chip_id: u16) -> Option<&'static str> {
    match chip_id {
        0x7961 | 0x7921 | 0x0608 => Some("mt7961"),
        0x7922 | 0x0616 => Some("mt7922"),
        0x7920 => Some("mt7920"),
        0x7902 => Some("mt7902"),
        0x7925 | 0x0717 => Some("mt7925"),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Synthetic fixture helpers
    // -----------------------------------------------------------------------

    /// Build a minimal valid patch blob with `n_region` patch_sec entries.
    ///
    /// The header has known `hw_sw_ver` and `patch_ver` encoded big-endian.
    fn make_patch_blob(hw_sw_ver: u32, patch_ver: u32, n_region: u32) -> Vec<u8> {
        let sec_count = n_region as usize;
        let total = PATCH_HDR_SIZE + sec_count * PATCH_SEC_SIZE;
        let mut blob = alloc::vec![0u8; total];

        // hw_sw_ver @ 0x14 (BE)
        blob[PATCH_HDR_HW_SW_VER_OFF..PATCH_HDR_HW_SW_VER_OFF + 4]
            .copy_from_slice(&hw_sw_ver.to_be_bytes());
        // patch_ver @ 0x18 (BE)
        blob[PATCH_HDR_PATCH_VER_OFF..PATCH_HDR_PATCH_VER_OFF + 4]
            .copy_from_slice(&patch_ver.to_be_bytes());
        // desc.n_region @ 0x20+12 (BE)
        blob[PATCH_HDR_N_REGION_OFF..PATCH_HDR_N_REGION_OFF + 4]
            .copy_from_slice(&n_region.to_be_bytes());

        blob
    }

    /// Build a minimal valid RAM-code blob with `n_region` fw_region entries.
    ///
    /// Each region gets `addr = base_addr + i * 0x1000` and `len = region_len`.
    fn make_fw_blob(chip_id: u8, n_region: u8, base_addr: u32, region_len: u32) -> Vec<u8> {
        // body + region_table + trailer
        let body_len = (n_region as usize) * (region_len as usize);
        let region_table_len = (n_region as usize) * FW_REGION_SIZE;
        let total = body_len + region_table_len + FW_TRAILER_SIZE;
        let mut blob = alloc::vec![0u8; total];

        let region_table_start = body_len;
        let trailer_start = body_len + region_table_len;

        // Write fw_trailer at the end.
        blob[trailer_start + FW_TRAILER_CHIP_ID_OFF] = chip_id;
        blob[trailer_start + FW_TRAILER_N_REGION_OFF] = n_region;

        // Write fw_region entries.
        for i in 0..n_region as usize {
            let base = region_table_start + i * FW_REGION_SIZE;
            let addr = base_addr + (i as u32) * 0x1000;
            let len = region_len;
            blob[base + FW_REGION_ADDR_OFF..base + FW_REGION_ADDR_OFF + 4]
                .copy_from_slice(&addr.to_le_bytes());
            blob[base + FW_REGION_LEN_OFF..base + FW_REGION_LEN_OFF + 4]
                .copy_from_slice(&len.to_le_bytes());
            blob[base + FW_REGION_FEATURE_SET_OFF] = FW_FEATURE_OVERRIDE_ADDR;
        }

        blob
    }

    /// Build a patch blob with a header (`n_region` sections) plus a body, and
    /// write each section's big-endian `type/offs/size/info.addr/info.len` so
    /// `parse_patch_sections` has real per-section addresses to recover.
    ///
    /// Section `i` is given `addr = 0x0020_0000 + i * 0x1000`, points at a
    /// `sec_size`-byte slice of the body at `offs = body_start + i * sec_size`.
    fn make_patch_blob_with_sections(n_region: u32, sec_size: u32) -> Vec<u8> {
        let sec_count = n_region as usize;
        let header_and_table = PATCH_HDR_SIZE + sec_count * PATCH_SEC_SIZE;
        let body_len = sec_count * sec_size as usize;
        let mut blob = alloc::vec![0u8; header_and_table + body_len];

        // desc.n_region @ 0x20+12 (BE).
        blob[PATCH_HDR_N_REGION_OFF..PATCH_HDR_N_REGION_OFF + 4]
            .copy_from_slice(&n_region.to_be_bytes());

        for i in 0..sec_count {
            let base = PATCH_HDR_SIZE + i * PATCH_SEC_SIZE;
            let offs = (header_and_table + i * sec_size as usize) as u32;
            let addr = 0x0020_0000u32 + (i as u32) * 0x1000;
            let be = |v: u32| v.to_be_bytes();
            blob[base + PATCH_SEC_TYPE_OFF..base + PATCH_SEC_TYPE_OFF + 4].copy_from_slice(&be(1));
            blob[base + PATCH_SEC_OFFS_OFF..base + PATCH_SEC_OFFS_OFF + 4]
                .copy_from_slice(&be(offs));
            blob[base + PATCH_SEC_SIZE_OFF..base + PATCH_SEC_SIZE_OFF + 4]
                .copy_from_slice(&be(sec_size));
            blob[base + PATCH_SEC_ADDR_OFF..base + PATCH_SEC_ADDR_OFF + 4]
                .copy_from_slice(&be(addr));
            blob[base + PATCH_SEC_LEN_OFF..base + PATCH_SEC_LEN_OFF + 4]
                .copy_from_slice(&be(sec_size));
        }
        blob
    }

    #[test]
    fn parse_synthetic_patch_sections() {
        let blob = make_patch_blob_with_sections(2, 256);
        let secs = parse_patch_sections(&blob, 2).expect("valid sections");
        assert_eq!(secs.len(), 2);
        // Each section carries its OWN load address (not a single fixed addr).
        assert_eq!(secs[0].addr, 0x0020_0000);
        assert_eq!(secs[1].addr, 0x0020_1000);
        assert_ne!(
            secs[0].addr, secs[1].addr,
            "per-section addresses must differ"
        );
        assert_eq!(secs[0].size, 256);
        assert_eq!(secs[1].len, 256);
        // offs/size must stay within the blob.
        assert!(secs[1].offs as usize + secs[1].size as usize <= blob.len());
    }

    #[test]
    fn patch_section_offs_past_blob_is_out_of_bounds() {
        let mut blob = make_patch_blob_with_sections(1, 64);
        // Corrupt section 0's size so offs+size overruns the blob.
        let base = PATCH_HDR_SIZE;
        blob[base + PATCH_SEC_SIZE_OFF..base + PATCH_SEC_SIZE_OFF + 4]
            .copy_from_slice(&0xFFFF_FFFFu32.to_be_bytes());
        assert_eq!(
            parse_patch_sections(&blob, 1),
            Err(FirmwareError::TrailerOutOfBounds)
        );
    }

    // -----------------------------------------------------------------------
    // parse_patch_header tests
    // -----------------------------------------------------------------------

    #[test]
    fn parse_synthetic_patch() {
        let blob = make_patch_blob(0xDEAD_BEEF, 0x0102_0304, 3);
        let hdr = parse_patch_header(&blob).expect("valid patch blob");
        assert_eq!(hdr.hw_sw_ver, 0xDEAD_BEEF);
        assert_eq!(hdr.patch_ver, 0x0102_0304);
        assert_eq!(hdr.n_region, 3);
    }

    #[test]
    fn patch_truncated_header_is_too_short() {
        let blob = alloc::vec![0u8; PATCH_HDR_SIZE - 1];
        assert_eq!(parse_patch_header(&blob), Err(FirmwareError::TooShort));
    }

    #[test]
    fn patch_zero_n_region_is_bad_region_count() {
        // n_region == 0 → BadRegionCount.
        let blob = make_patch_blob(0, 0, 0);
        assert_eq!(
            parse_patch_header(&blob),
            Err(FirmwareError::BadRegionCount)
        );
    }

    #[test]
    fn patch_n_region_exceeds_cap_is_bad_region_count() {
        // n_region > PATCH_MAX_REGIONS → BadRegionCount.
        let n = PATCH_MAX_REGIONS + 1;
        // Build just the header (no sections needed — should fail before checking size).
        let blob = make_patch_blob(0, 0, n);
        assert_eq!(
            parse_patch_header(&blob),
            Err(FirmwareError::BadRegionCount)
        );
    }

    #[test]
    fn patch_section_table_overflow_is_bad_region_count() {
        // n_region is valid but blob is too short to hold the sections.
        let mut blob = make_patch_blob(1, 1, 5);
        // Truncate to just the header (missing all 5 section entries).
        blob.truncate(PATCH_HDR_SIZE);
        assert_eq!(
            parse_patch_header(&blob),
            Err(FirmwareError::BadRegionCount)
        );
    }

    // -----------------------------------------------------------------------
    // parse_fw_trailer tests
    // -----------------------------------------------------------------------

    #[test]
    fn parse_synthetic_ram_trailer() {
        let blob = make_fw_blob(0x79, 2, 0x0010_0000, 512);
        let img = parse_fw_trailer(&blob).expect("valid RAM blob");
        assert_eq!(img.chip_id, 0x79);
        assert_eq!(img.n_region, 2);
        assert_eq!(img.regions.len(), 2);

        // Pin the literal upstream bit position (BIT(5) = 0x20) so a
        // regression in FW_FEATURE_OVERRIDE_ADDR is caught, not masked by a
        // self-referential comparison.
        assert_eq!(FW_FEATURE_OVERRIDE_ADDR, 0x20, "OVERRIDE_ADDR = BIT(5)");

        // Region 0.
        assert_eq!(img.regions[0].addr, 0x0010_0000);
        assert_eq!(img.regions[0].len, 512);
        assert_eq!(img.regions[0].feature_set, FW_FEATURE_OVERRIDE_ADDR);

        // Region 1.
        assert_eq!(img.regions[1].addr, 0x0010_1000);
        assert_eq!(img.regions[1].len, 512);
        assert_eq!(img.regions[1].feature_set, FW_FEATURE_OVERRIDE_ADDR);
    }

    #[test]
    fn fw_truncated_blob_is_too_short() {
        let blob = alloc::vec![0u8; FW_TRAILER_SIZE - 1];
        assert_eq!(parse_fw_trailer(&blob), Err(FirmwareError::TooShort));
    }

    #[test]
    fn fw_zero_n_region_is_bad_region_count() {
        // Trailer with n_region == 0.
        let mut blob = alloc::vec![0u8; FW_TRAILER_SIZE];
        // n_region is at offset 2 within the trailer (which is at the end).
        blob[FW_TRAILER_N_REGION_OFF] = 0;
        assert_eq!(parse_fw_trailer(&blob), Err(FirmwareError::BadRegionCount));
    }

    #[test]
    fn fw_region_table_before_blob_start_is_out_of_bounds() {
        // A trailer-only blob (36 bytes) with n_region=1 demands 40 bytes of
        // region table before the trailer, but there are 0 bytes of body —
        // region_table_start = 0 - 40 → underflow → TrailerOutOfBounds.
        let mut blob = alloc::vec![0u8; FW_TRAILER_SIZE];
        blob[FW_TRAILER_N_REGION_OFF] = 1; // demands 40 bytes before trailer
        assert_eq!(
            parse_fw_trailer(&blob),
            Err(FirmwareError::TrailerOutOfBounds)
        );
    }

    #[test]
    fn fw_region_len_exceeds_body_is_out_of_bounds() {
        // blob body is 100 bytes but region declares len = 200.
        let n_region: u8 = 1;
        let body_len: usize = 100;
        let region_table_len = FW_REGION_SIZE;
        let total = body_len + region_table_len + FW_TRAILER_SIZE;
        let mut blob = alloc::vec![0u8; total];

        let trailer_start = body_len + region_table_len;
        blob[trailer_start + FW_TRAILER_CHIP_ID_OFF] = 0x79;
        blob[trailer_start + FW_TRAILER_N_REGION_OFF] = n_region;

        // Write a region whose len (200) > body_len (100).
        let rbase = body_len;
        blob[rbase + FW_REGION_ADDR_OFF..rbase + FW_REGION_ADDR_OFF + 4]
            .copy_from_slice(&0x0010_0000u32.to_le_bytes());
        blob[rbase + FW_REGION_LEN_OFF..rbase + FW_REGION_LEN_OFF + 4]
            .copy_from_slice(&200u32.to_le_bytes());

        assert_eq!(
            parse_fw_trailer(&blob),
            Err(FirmwareError::TrailerOutOfBounds)
        );
    }

    #[test]
    fn adversarial_inputs_do_not_panic() {
        // Completely empty blob.
        assert!(parse_fw_trailer(&[]).is_err());
        assert!(parse_patch_header(&[]).is_err());
        // Single-byte blob.
        assert!(parse_fw_trailer(&[0xFF]).is_err());
        assert!(parse_patch_header(&[0xFF]).is_err());
        // Blob of all-0xFF bytes, just large enough for the trailer but
        // with n_region = 0xFF (very large).
        let big = alloc::vec![0xFFu8; 512];
        // Should return an error, never panic.
        let _ = parse_fw_trailer(&big);
        let _ = parse_patch_header(&big);
    }

    // -----------------------------------------------------------------------
    // Scatter / semaphore helpers
    // -----------------------------------------------------------------------

    #[test]
    fn chunking_4096() {
        assert_eq!(scatter_chunk_count(0), 0);
        assert_eq!(scatter_chunk_count(4096), 1);
        assert_eq!(scatter_chunk_count(4097), 2);
        assert_eq!(scatter_chunk_count(10000), 3); // ceil(10000/4096) = 3
    }

    #[test]
    fn patch_sem_branch() {
        assert_eq!(patch_sections_to_download(PatchSem::IsDl, 5), 0);
        assert_eq!(patch_sections_to_download(PatchSem::NotDlSemSuccess, 5), 5);
    }

    // -----------------------------------------------------------------------
    // select_firmware_set
    // -----------------------------------------------------------------------

    #[test]
    fn firmware_set_mapping() {
        assert_eq!(select_firmware_set(0x7961), Some("mt7961"));
        assert_eq!(select_firmware_set(0x7921), Some("mt7961"));
        assert_eq!(select_firmware_set(0x0608), Some("mt7961"));
        assert_eq!(select_firmware_set(0x7922), Some("mt7922"));
        assert_eq!(select_firmware_set(0x0616), Some("mt7922"));
        assert_eq!(select_firmware_set(0x7920), Some("mt7920"));
        assert_eq!(select_firmware_set(0x7902), Some("mt7902"));
        assert_eq!(select_firmware_set(0x7925), Some("mt7925"));
        assert_eq!(select_firmware_set(0x0717), Some("mt7925"));
        assert_eq!(select_firmware_set(0xFFFF), None);
        assert_eq!(select_firmware_set(0x100E), None); // Intel e1000
    }
}
