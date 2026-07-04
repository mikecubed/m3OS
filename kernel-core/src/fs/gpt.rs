//! Phase 106 C.4 — pure-logic **GPT builder + parser** for the on-device
//! partition-aware installer.
//!
//! The host side lays combined images with the `gpt` crate
//! (`create_combined_gpt_disk` in xtask); the ring-3 installer cannot use it
//! (`std`-only), and nothing in the tree could *write* a GPT on-device. This
//! module supplies that as host-testable pure logic:
//!
//! - [`parse_gpt`] — read a disk's protective MBR + primary GPT, **CRC-verified**
//!   (stricter than the kernel's mount-time `gpt_ext2_scan`, which only
//!   pattern-matches — an installer must fail closed on a corrupt source), and
//!   return the ESP / Linux partition spans.
//! - [`GptPlan::for_target`] — size a fresh dual-partition layout to a target
//!   disk: the ESP keeps the source's span (a raw FAT copy stays valid — the
//!   BPB's geometry is partition-relative and its `hidden sectors` field is the
//!   partition start LBA, both unchanged), the Linux partition grows from the
//!   source's start LBA to the target's last usable LBA. This is the point of
//!   C.4: a raw `dd` copy wastes everything past the image size.
//! - [`build_gpt`] — serialize the plan into the on-disk structures
//!   (protective MBR, primary + backup headers, entry arrays, CRC32s);
//!   [`GptImage::sector_writes`] yields the exact `(lba, sector)` writes.
//!
//! Layout is the standard 128×128-byte entry table: LBAs 0 (protective MBR),
//! 1 (primary header), 2–33 (entries), `N-33..N-2` (backup entries), `N-1`
//! (backup header); usable span `34..=N-34`.

use alloc::vec;
use alloc::vec::Vec;

/// Logical sector size the GPT structures assume (LBA 512 — every m3OS block
/// backend and the QEMU nvme/usb defaults).
pub const SECTOR_BYTES: usize = 512;

/// On-disk size of one partition entry (the standard minimum; both the `gpt`
/// crate and every firmware default to it).
pub const GPT_ENTRY_BYTES: usize = 128;

/// Entries in the table this builder lays down (the standard default).
pub const GPT_ENTRY_COUNT: u32 = 128;

/// Sectors one entry table occupies (128 × 128 / 512).
pub const GPT_ENTRY_SECTORS: u64 = (GPT_ENTRY_COUNT as u64 * GPT_ENTRY_BYTES as u64) / 512;

/// First usable LBA behind the primary GPT (MBR + header + entry table).
pub const GPT_FIRST_USABLE: u64 = 2 + GPT_ENTRY_SECTORS;

/// Sectors reserved at the disk tail for the backup GPT (entry table + header).
pub const GPT_BACKUP_SECTORS: u64 = GPT_ENTRY_SECTORS + 1;

/// Encode a GUID's canonical fields into on-disk (mixed-endian) byte order:
/// the first three fields little-endian, the final eight bytes verbatim.
const fn guid(d1: u32, d2: u16, d3: u16, d4: [u8; 8]) -> [u8; 16] {
    let a = d1.to_le_bytes();
    let b = d2.to_le_bytes();
    let c = d3.to_le_bytes();
    [
        a[0], a[1], a[2], a[3], b[0], b[1], c[0], c[1], d4[0], d4[1], d4[2], d4[3], d4[4], d4[5],
        d4[6], d4[7],
    ]
}

/// EFI System Partition type GUID (`C12A7328-F81F-11D2-BA4B-00A0C93EC93B`),
/// on-disk byte order. Matches `gpt::partition_types::EFI` (the combined
/// image's ESP).
pub const GUID_TYPE_ESP: [u8; 16] = guid(
    0xC12A_7328,
    0xF81F,
    0x11D2,
    [0xBA, 0x4B, 0x00, 0xA0, 0xC9, 0x3E, 0xC9, 0x3B],
);

/// Linux filesystem data type GUID (`0FC63DAF-8483-4772-8E79-3D69D8477DE4`),
/// on-disk byte order. Matches `gpt::partition_types::LINUX_FS` (the combined
/// image's rootfs).
pub const GUID_TYPE_LINUX_FS: [u8; 16] = guid(
    0x0FC6_3DAF,
    0x8483,
    0x4772,
    [0x8E, 0x79, 0x3D, 0x69, 0xD8, 0x47, 0x7D, 0xE4],
);

/// CRC32 (IEEE 802.3, reflected, poly `0xEDB88320`) — the checksum GPT
/// headers and entry arrays carry. Bitwise (no table): the largest input is
/// the 16 KiB entry array, far off any hot path.
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

/// GPT build/parse failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GptError {
    /// The target disk cannot hold the GPT structures plus both partitions.
    DiskTooSmall,
    /// A partition span is empty, overlapping, or outside the usable region.
    BadSpan,
}

/// One partition's inclusive LBA span.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GptSpan {
    pub first_lba: u64,
    pub last_lba: u64,
}

impl GptSpan {
    /// Sector count of the span (inclusive bounds).
    pub fn sectors(&self) -> u64 {
        self.last_lba - self.first_lba + 1
    }
}

/// A planned dual-partition (ESP + Linux rootfs) layout for one disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GptPlan {
    pub total_sectors: u64,
    pub esp: GptSpan,
    pub linux: GptSpan,
}

impl GptPlan {
    /// Plan the install layout for a `total_sectors` target: keep the source's
    /// ESP span and Linux start LBA, grow the Linux partition to the target's
    /// last usable LBA (`total - 34`).
    pub fn for_target(
        total_sectors: u64,
        source_esp: GptSpan,
        linux_first_lba: u64,
    ) -> Result<GptPlan, GptError> {
        let last_usable = total_sectors
            .checked_sub(GPT_BACKUP_SECTORS + 1)
            .ok_or(GptError::DiskTooSmall)?;
        if source_esp.first_lba < GPT_FIRST_USABLE || source_esp.last_lba < source_esp.first_lba {
            return Err(GptError::BadSpan);
        }
        if linux_first_lba <= source_esp.last_lba {
            return Err(GptError::BadSpan);
        }
        // The grown rootfs must have room behind the ESP; a one-sector
        // partition is useless but geometry-valid — the ext2 formatter is the
        // real minimum-size arbiter and rejects degenerate volumes itself.
        if linux_first_lba > last_usable {
            return Err(GptError::DiskTooSmall);
        }
        Ok(GptPlan {
            total_sectors,
            esp: source_esp,
            linux: GptSpan {
                first_lba: linux_first_lba,
                last_lba: last_usable,
            },
        })
    }
}

/// GUIDs a build stamps into the structures. The builder has no entropy
/// source — the caller supplies them (the installer derives from the clock;
/// tests use fixed bytes).
#[derive(Debug, Clone, Copy)]
pub struct GptGuids {
    pub disk: [u8; 16],
    pub esp: [u8; 16],
    pub linux: [u8; 16],
}

/// The serialized on-disk structures of one [`GptPlan`].
pub struct GptImage {
    plan: GptPlan,
    protective_mbr: [u8; SECTOR_BYTES],
    primary_header: [u8; SECTOR_BYTES],
    backup_header: [u8; SECTOR_BYTES],
    /// The 128-entry table (16 KiB) — identical bytes for primary and backup.
    entries: Vec<u8>,
}

impl GptImage {
    /// Every `(lba, sector)` write laying this GPT onto the disk: protective
    /// MBR, primary header + entries, backup entries + header. Partition
    /// *contents* are the caller's business.
    pub fn sector_writes(&self) -> impl Iterator<Item = (u64, &[u8])> {
        let entry_sector = |i: u64| -> &[u8] {
            let off = i as usize * SECTOR_BYTES;
            &self.entries[off..off + SECTOR_BYTES]
        };
        let backup_entries_lba = self.plan.total_sectors - 1 - GPT_ENTRY_SECTORS;
        core::iter::once((0u64, &self.protective_mbr[..]))
            .chain(core::iter::once((1u64, &self.primary_header[..])))
            .chain((0..GPT_ENTRY_SECTORS).map(move |i| (2 + i, entry_sector(i))))
            .chain((0..GPT_ENTRY_SECTORS).map(move |i| (backup_entries_lba + i, entry_sector(i))))
            .chain(core::iter::once((
                self.plan.total_sectors - 1,
                &self.backup_header[..],
            )))
    }
}

/// Encode one 128-byte partition entry.
fn encode_entry(
    buf: &mut [u8],
    type_guid: &[u8; 16],
    unique: &[u8; 16],
    span: GptSpan,
    name: &str,
) {
    buf[0..16].copy_from_slice(type_guid);
    buf[16..32].copy_from_slice(unique);
    buf[32..40].copy_from_slice(&span.first_lba.to_le_bytes());
    buf[40..48].copy_from_slice(&span.last_lba.to_le_bytes());
    // attributes (48..56) stay 0
    for (i, c) in name.encode_utf16().take(36).enumerate() {
        buf[56 + i * 2..58 + i * 2].copy_from_slice(&c.to_le_bytes());
    }
}

/// Encode a GPT header sector. `my_lba`/`alt_lba` distinguish primary from
/// backup; `entries_lba` is where this copy's entry table lives.
fn encode_header(
    plan: &GptPlan,
    disk_guid: &[u8; 16],
    my_lba: u64,
    alt_lba: u64,
    entries_lba: u64,
    entries_crc: u32,
) -> [u8; SECTOR_BYTES] {
    let mut h = [0u8; SECTOR_BYTES];
    h[0..8].copy_from_slice(b"EFI PART");
    h[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes()); // revision 1.0
    h[12..16].copy_from_slice(&92u32.to_le_bytes()); // header size
    // 16..20 = header CRC, filled below
    h[24..32].copy_from_slice(&my_lba.to_le_bytes());
    h[32..40].copy_from_slice(&alt_lba.to_le_bytes());
    h[40..48].copy_from_slice(&GPT_FIRST_USABLE.to_le_bytes());
    h[48..56].copy_from_slice(&(plan.total_sectors - GPT_BACKUP_SECTORS - 1).to_le_bytes());
    h[56..72].copy_from_slice(disk_guid);
    h[72..80].copy_from_slice(&entries_lba.to_le_bytes());
    h[80..84].copy_from_slice(&GPT_ENTRY_COUNT.to_le_bytes());
    h[84..88].copy_from_slice(&(GPT_ENTRY_BYTES as u32).to_le_bytes());
    h[88..92].copy_from_slice(&entries_crc.to_le_bytes());
    let hdr_crc = crc32(&h[0..92]);
    h[16..20].copy_from_slice(&hdr_crc.to_le_bytes());
    h
}

/// Serialize `plan` into its on-disk GPT structures.
pub fn build_gpt(plan: &GptPlan, guids: &GptGuids) -> Result<GptImage, GptError> {
    // Sanity beyond `for_target` (callers may construct plans directly).
    let last_usable = plan
        .total_sectors
        .checked_sub(GPT_BACKUP_SECTORS + 1)
        .ok_or(GptError::DiskTooSmall)?;
    if last_usable < GPT_FIRST_USABLE {
        return Err(GptError::DiskTooSmall);
    }
    for span in [plan.esp, plan.linux] {
        if span.first_lba < GPT_FIRST_USABLE
            || span.last_lba < span.first_lba
            || span.last_lba > last_usable
        {
            return Err(GptError::BadSpan);
        }
    }
    if plan.linux.first_lba <= plan.esp.last_lba {
        return Err(GptError::BadSpan);
    }

    let mut entries = vec![0u8; GPT_ENTRY_COUNT as usize * GPT_ENTRY_BYTES];
    encode_entry(
        &mut entries[0..GPT_ENTRY_BYTES],
        &GUID_TYPE_ESP,
        &guids.esp,
        plan.esp,
        "boot",
    );
    encode_entry(
        &mut entries[GPT_ENTRY_BYTES..2 * GPT_ENTRY_BYTES],
        &GUID_TYPE_LINUX_FS,
        &guids.linux,
        plan.linux,
        "root",
    );
    let entries_crc = crc32(&entries);

    let primary_header =
        encode_header(plan, &guids.disk, 1, plan.total_sectors - 1, 2, entries_crc);
    let backup_header = encode_header(
        plan,
        &guids.disk,
        plan.total_sectors - 1,
        1,
        plan.total_sectors - 1 - GPT_ENTRY_SECTORS,
        entries_crc,
    );

    // Protective MBR: one 0xEE entry spanning the whole disk (clamped to the
    // u32 the MBR format can express), boot signature 0x55AA.
    let mut protective_mbr = [0u8; SECTOR_BYTES];
    let e = &mut protective_mbr[446..462];
    e[0] = 0x00; // not bootable
    e[1] = 0x00;
    e[2] = 0x02;
    e[3] = 0x00; // CHS start 0/0/2
    e[4] = 0xEE; // protective GPT type
    e[5] = 0xFF;
    e[6] = 0xFF;
    e[7] = 0xFF; // CHS end (maxed)
    e[8..12].copy_from_slice(&1u32.to_le_bytes()); // starts at LBA 1
    let clamped = u32::try_from(plan.total_sectors - 1).unwrap_or(u32::MAX);
    e[12..16].copy_from_slice(&clamped.to_le_bytes());
    protective_mbr[510] = 0x55;
    protective_mbr[511] = 0xAA;

    Ok(GptImage {
        plan: *plan,
        protective_mbr,
        primary_header,
        backup_header,
        entries,
    })
}

/// A parsed (and CRC-verified) primary GPT.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParsedGpt {
    /// Backup-header LBA — the disk's last meaningful sector (the raw-copy
    /// span the C.3 installer already derives).
    pub alt_lba: u64,
    pub first_usable: u64,
    pub last_usable: u64,
    /// First EFI-System-Partition entry, if any.
    pub esp: Option<GptSpan>,
    /// First Linux-filesystem entry, if any.
    pub linux: Option<GptSpan>,
}

/// Parse the primary GPT through `read_sector` (LBA → 512-byte buffer;
/// `false` = read failure). Returns `None` on any structural or CRC
/// mismatch — the installer must fail closed on a corrupt source rather
/// than derive a bogus layout from it.
pub fn parse_gpt(
    read_sector: &mut dyn FnMut(u64, &mut [u8; SECTOR_BYTES]) -> bool,
) -> Option<ParsedGpt> {
    let mut lba0 = [0u8; SECTOR_BYTES];
    if !read_sector(0, &mut lba0) || lba0[510] != 0x55 || lba0[511] != 0xAA || lba0[450] != 0xEE {
        return None;
    }
    let mut hdr = [0u8; SECTOR_BYTES];
    if !read_sector(1, &mut hdr) || &hdr[0..8] != b"EFI PART" {
        return None;
    }
    let header_size = u32::from_le_bytes(hdr[12..16].try_into().ok()?) as usize;
    if !(92..=SECTOR_BYTES).contains(&header_size) {
        return None;
    }
    let stored_hdr_crc = u32::from_le_bytes(hdr[16..20].try_into().ok()?);
    let mut hdr_for_crc = [0u8; SECTOR_BYTES];
    hdr_for_crc[..header_size].copy_from_slice(&hdr[..header_size]);
    hdr_for_crc[16..20].fill(0);
    if crc32(&hdr_for_crc[..header_size]) != stored_hdr_crc {
        return None;
    }

    let alt_lba = u64::from_le_bytes(hdr[32..40].try_into().ok()?);
    let first_usable = u64::from_le_bytes(hdr[40..48].try_into().ok()?);
    let last_usable = u64::from_le_bytes(hdr[48..56].try_into().ok()?);
    let entries_lba = u64::from_le_bytes(hdr[72..80].try_into().ok()?);
    let num_entries = u32::from_le_bytes(hdr[80..84].try_into().ok()?);
    let entry_size = u32::from_le_bytes(hdr[84..88].try_into().ok()?) as usize;
    let stored_entries_crc = u32::from_le_bytes(hdr[88..92].try_into().ok()?);
    if entry_size != GPT_ENTRY_BYTES || num_entries == 0 || num_entries > 1024 || entries_lba < 2 {
        return None;
    }

    let table_bytes = num_entries as usize * entry_size;
    let table_sectors = table_bytes.div_ceil(SECTOR_BYTES);
    let mut table = vec![0u8; table_sectors * SECTOR_BYTES];
    for s in 0..table_sectors {
        let mut sec = [0u8; SECTOR_BYTES];
        if !read_sector(entries_lba + s as u64, &mut sec) {
            return None;
        }
        table[s * SECTOR_BYTES..(s + 1) * SECTOR_BYTES].copy_from_slice(&sec);
    }
    if crc32(&table[..table_bytes]) != stored_entries_crc {
        return None;
    }

    let mut esp = None;
    let mut linux = None;
    for i in 0..num_entries as usize {
        let ent = &table[i * entry_size..(i + 1) * entry_size];
        let type_guid: [u8; 16] = ent[0..16].try_into().ok()?;
        if type_guid == [0u8; 16] {
            continue;
        }
        let span = GptSpan {
            first_lba: u64::from_le_bytes(ent[32..40].try_into().ok()?),
            last_lba: u64::from_le_bytes(ent[40..48].try_into().ok()?),
        };
        if span.last_lba < span.first_lba {
            return None;
        }
        if type_guid == GUID_TYPE_ESP && esp.is_none() {
            esp = Some(span);
        } else if type_guid == GUID_TYPE_LINUX_FS && linux.is_none() {
            linux = Some(span);
        }
    }

    Some(ParsedGpt {
        alt_lba,
        first_usable,
        last_usable,
        esp,
        linux,
    })
}

// ---------------------------------------------------------------------------
// Host tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// In-memory 512-byte-sector disk.
    struct MemDisk {
        data: Vec<u8>,
    }

    impl MemDisk {
        fn new(total_sectors: u64) -> Self {
            MemDisk {
                data: vec![0u8; total_sectors as usize * SECTOR_BYTES],
            }
        }

        fn write_image(&mut self, img: &GptImage) {
            for (lba, sec) in img.sector_writes() {
                let off = lba as usize * SECTOR_BYTES;
                self.data[off..off + SECTOR_BYTES].copy_from_slice(sec);
            }
        }

        fn reader(&self) -> impl FnMut(u64, &mut [u8; SECTOR_BYTES]) -> bool {
            |lba, buf| {
                let off = lba as usize * SECTOR_BYTES;
                if off + SECTOR_BYTES > self.data.len() {
                    return false;
                }
                buf.copy_from_slice(&self.data[off..off + SECTOR_BYTES]);
                true
            }
        }
    }

    const GUIDS: GptGuids = GptGuids {
        disk: [0xD1; 16],
        esp: [0xE5; 16],
        linux: [0x17; 16],
    };

    fn test_plan(total: u64) -> GptPlan {
        GptPlan::for_target(
            total,
            GptSpan {
                first_lba: 34,
                last_lba: 8225,
            },
            8226,
        )
        .expect("plan")
    }

    #[test]
    fn crc32_known_vectors() {
        // The canonical IEEE 802.3 check value.
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
        assert_eq!(crc32(b"\x00"), 0xD202_EF8D);
    }

    #[test]
    fn plan_grows_linux_partition_to_target() {
        let plan = test_plan(1_000_000);
        assert_eq!(plan.esp.first_lba, 34);
        assert_eq!(plan.esp.last_lba, 8225);
        assert_eq!(plan.linux.first_lba, 8226);
        // Last usable = total - backup(33) - 1.
        assert_eq!(plan.linux.last_lba, 1_000_000 - 34);
    }

    #[test]
    fn plan_rejects_degenerate_targets() {
        let esp = GptSpan {
            first_lba: 34,
            last_lba: 8225,
        };
        // No room for the linux partition behind the backup GPT
        // (last usable = 8225 < linux first 8226). One sector more and a
        // single-sector partition is geometry-valid — the ext2 formatter is
        // the real minimum-size arbiter.
        assert_eq!(
            GptPlan::for_target(8226 + 33, esp, 8226).unwrap_err(),
            GptError::DiskTooSmall
        );
        assert!(GptPlan::for_target(8226 + 34, esp, 8226).is_ok());
        // ESP inside the primary GPT region.
        assert_eq!(
            GptPlan::for_target(
                1_000_000,
                GptSpan {
                    first_lba: 33,
                    last_lba: 8225
                },
                8226
            )
            .unwrap_err(),
            GptError::BadSpan
        );
        // Linux overlapping the ESP.
        assert_eq!(
            GptPlan::for_target(1_000_000, esp, 8225).unwrap_err(),
            GptError::BadSpan
        );
        // Tiny disk underflows the backup reservation.
        assert_eq!(
            GptPlan::for_target(10, esp, 8226).unwrap_err(),
            GptError::DiskTooSmall
        );
    }

    #[test]
    fn build_parse_round_trip() {
        let plan = test_plan(1_000_000);
        let img = build_gpt(&plan, &GUIDS).expect("build");
        let mut disk = MemDisk::new(1_000_000);
        disk.write_image(&img);

        let mut read = disk.reader();
        let parsed = parse_gpt(&mut read).expect("parse");
        assert_eq!(parsed.alt_lba, 999_999);
        assert_eq!(parsed.first_usable, 34);
        assert_eq!(parsed.last_usable, 1_000_000 - 34);
        assert_eq!(parsed.esp, Some(plan.esp));
        assert_eq!(parsed.linux, Some(plan.linux));
    }

    #[test]
    fn sector_writes_cover_exact_layout() {
        let plan = test_plan(100_000);
        let img = build_gpt(&plan, &GUIDS).expect("build");
        let lbas: Vec<u64> = img.sector_writes().map(|(lba, _)| lba).collect();
        // MBR + primary header + 32 entries + 32 backup entries + backup header.
        assert_eq!(lbas.len(), 2 + 32 + 32 + 1);
        assert_eq!(lbas[0], 0);
        assert_eq!(lbas[1], 1);
        assert_eq!(lbas[2..34], (2..34).collect::<Vec<u64>>()[..]);
        assert_eq!(
            lbas[34..66],
            (100_000 - 33..100_000 - 1).collect::<Vec<u64>>()[..]
        );
        assert_eq!(lbas[66], 99_999);
        for (_, sec) in img.sector_writes() {
            assert_eq!(sec.len(), SECTOR_BYTES);
        }
    }

    #[test]
    fn kernel_probe_replica_finds_the_ext2_partition() {
        // Replay the kernel's `gpt_ext2_scan` (protective-MBR 0xEE → "EFI
        // PART" → entry walk → ext2 magic at first_lba + 2) against a built
        // image, exactly as `VFS_MOUNT_EXT2_ROOT` will against the installed
        // disk.
        let plan = test_plan(1_000_000);
        let img = build_gpt(&plan, &GUIDS).expect("build");
        let mut disk = MemDisk::new(1_000_000);
        disk.write_image(&img);
        // Plant the ext2 magic where a formatted rootfs would carry it:
        // superblock at partition byte offset 1024 (= LBA first+2), magic LE
        // 0xEF53 at superblock offset 56.
        let sb_off = (plan.linux.first_lba + 2) as usize * SECTOR_BYTES;
        disk.data[sb_off + 56] = 0x53;
        disk.data[sb_off + 57] = 0xEF;

        let mut read = disk.reader();
        // --- kernel gpt_ext2_scan replica ---
        let found = {
            let mut lba0 = [0u8; 512];
            assert!(read(0, &mut lba0));
            assert_eq!((lba0[510], lba0[511], lba0[450]), (0x55, 0xAA, 0xEE));
            let mut hdr = [0u8; 512];
            assert!(read(1, &mut hdr));
            assert_eq!(&hdr[0..8], b"EFI PART");
            let part_lba = u64::from_le_bytes(hdr[72..80].try_into().unwrap());
            let esize = u32::from_le_bytes(hdr[84..88].try_into().unwrap()) as usize;
            assert_eq!(esize, 128);
            let num_entries = u32::from_le_bytes(hdr[80..84].try_into().unwrap()) as u64;
            let scan_sectors = ((num_entries * esize as u64).div_ceil(512)).min(256);
            let mut hit = None;
            'scan: for sec in 0..scan_sectors {
                let mut ent = [0u8; 512];
                assert!(read(part_lba + sec, &mut ent));
                for k in 0..4usize {
                    let off = k * 128;
                    let first = u64::from_le_bytes(ent[off + 32..off + 40].try_into().unwrap());
                    if first != 0 {
                        let mut sb = [0u8; 512];
                        if read(first + 2, &mut sb) && sb[56] == 0x53 && sb[57] == 0xEF {
                            hit = Some(first);
                            break 'scan;
                        }
                    }
                }
            }
            hit
        };
        assert_eq!(found, Some(plan.linux.first_lba));
    }

    #[test]
    fn parse_rejects_corruption() {
        let plan = test_plan(1_000_000);
        let img = build_gpt(&plan, &GUIDS).expect("build");
        let mut disk = MemDisk::new(1_000_000);
        disk.write_image(&img);

        // Flip one byte inside the header (past the CRC field).
        let mut hdr_bad = disk.data.clone();
        hdr_bad[512 + 40] ^= 0x01;
        let mut read = |lba: u64, buf: &mut [u8; 512]| {
            let off = lba as usize * 512;
            buf.copy_from_slice(&hdr_bad[off..off + 512]);
            true
        };
        assert_eq!(parse_gpt(&mut read), None);

        // Flip one byte inside the entry table.
        let mut ent_bad = disk.data.clone();
        ent_bad[2 * 512 + 32] ^= 0x01;
        let mut read = |lba: u64, buf: &mut [u8; 512]| {
            let off = lba as usize * 512;
            buf.copy_from_slice(&ent_bad[off..off + 512]);
            true
        };
        assert_eq!(parse_gpt(&mut read), None);

        // Break the protective MBR type byte.
        let mut mbr_bad = disk.data.clone();
        mbr_bad[450] = 0x83;
        let mut read = |lba: u64, buf: &mut [u8; 512]| {
            let off = lba as usize * 512;
            buf.copy_from_slice(&mbr_bad[off..off + 512]);
            true
        };
        assert_eq!(parse_gpt(&mut read), None);
    }

    #[test]
    fn protective_mbr_clamps_oversize_disks() {
        // > u32::MAX sectors must clamp the MBR size field, not wrap.
        let plan = GptPlan::for_target(
            0x1_0000_0000u64 + 4096,
            GptSpan {
                first_lba: 34,
                last_lba: 8225,
            },
            8226,
        )
        .expect("plan");
        let img = build_gpt(&plan, &GUIDS).expect("build");
        let e = &img.protective_mbr[446..462];
        assert_eq!(e[4], 0xEE);
        assert_eq!(u32::from_le_bytes(e[12..16].try_into().unwrap()), u32::MAX);
        assert_eq!(
            (img.protective_mbr[510], img.protective_mbr[511]),
            (0x55, 0xAA)
        );
    }

    /// Falsifiable external check: if the host has `sgdisk`, a built image
    /// must verify clean and report exactly the planned partitions. Skips
    /// silently when the tool is absent (same posture as the C.5
    /// `e2fsck_accepts_formatted_image_when_available` test).
    #[test]
    fn sgdisk_accepts_built_gpt_when_available() {
        use std::io::Write as _;
        use std::process::Command;

        let which = Command::new("sh")
            .args(["-c", "command -v sgdisk"])
            .output();
        let Ok(out) = which else { return };
        if !out.status.success() {
            return; // no sgdisk on this host — skip
        }

        let total = 200_000u64; // ~100 MiB — keeps the temp file small
        let plan = test_plan(total);
        let img = build_gpt(&plan, &GUIDS).expect("build");
        let mut disk = MemDisk::new(total);
        disk.write_image(&img);

        let dir = std::env::temp_dir();
        let path = dir.join(format!("m3os-c4-gpt-{}.img", std::process::id()));
        let mut f = std::fs::File::create(&path).expect("temp image");
        f.write_all(&disk.data).expect("write image");
        drop(f);

        let verify = Command::new("sgdisk")
            .args(["--verify", path.to_str().expect("utf8 path")])
            .output()
            .expect("run sgdisk --verify");
        let stdout = String::from_utf8_lossy(&verify.stdout).into_owned();
        let print = Command::new("sgdisk")
            .args(["-p", path.to_str().expect("utf8 path")])
            .output()
            .expect("run sgdisk -p");
        let table = String::from_utf8_lossy(&print.stdout).into_owned();
        let _ = std::fs::remove_file(&path);

        assert!(
            verify.status.success() && stdout.contains("No problems found"),
            "sgdisk --verify rejected the image:\n{stdout}\n{}",
            String::from_utf8_lossy(&verify.stderr),
        );
        // `sgdisk -p` prints one row per partition: start, end, code, name.
        // EF00 = ESP, 8300 = Linux filesystem.
        assert!(table.contains("EF00"), "missing ESP row:\n{table}");
        assert!(table.contains("8300"), "missing Linux row:\n{table}");
        assert!(
            table.contains(&plan.linux.last_lba.to_string()),
            "linux last LBA not in table:\n{table}"
        );
    }
}
