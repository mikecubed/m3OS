//! Phase 77 Track E — AMD microcode container parsing (pure logic).
//!
//! Parses the AMD `amd-ucode` container format (the `microcode_amd*.bin`
//! linux-firmware blobs) far enough to (a) map the running CPU's signature to
//! an equivalence id and (b) locate the matching patch and its revision. The
//! actual MSR application (write the patch address to `MSR_AMD64_PATCH_LOADER`
//! `0xC0010020`, read back the level from `0x8B`) lives in the kernel; this
//! module is host-testable and does no I/O.
//!
//! Container layout (little-endian):
//! ```text
//! u32 magic = "DMA\0" (0x00414d44)
//! section { u32 type; u32 length; u8 payload[length] } ...
//!   type 0 = equivalence table: payload is N×16-byte equiv entries
//!            { u32 installed_cpu; u32 errata_mask; u32 errata_compare;
//!              u16 equiv_id; u16 reserved } terminated by a zero entry
//!   type 1 = microcode patch: payload begins with the patch header
//!            { u32 data_code; u32 patch_id; u16 data_id; u8 data_len;
//!              u8 init_flag; u32 checksum; u32 nb_dev_id; u32 sb_dev_id;
//!              u16 processor_rev_id; ... }
//! ```

/// AMD container magic ("DMA\0" little-endian).
pub const AMD_CONTAINER_MAGIC: u32 = 0x0041_4d44;

const SECTION_TYPE_EQUIV_TABLE: u32 = 0;
const SECTION_TYPE_PATCH: u32 = 1;
const EQUIV_ENTRY_LEN: usize = 16;
/// Offset of `patch_id` (the revision) within a patch payload.
const PATCH_OFF_PATCH_ID: usize = 4;
/// Offset of `processor_rev_id` (the equiv id this patch applies to).
const PATCH_OFF_PROCESSOR_REV_ID: usize = 0x18;

fn read_u32(blob: &[u8], off: usize) -> Option<u32> {
    let b = blob.get(off..off + 4)?;
    Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

fn read_u16(blob: &[u8], off: usize) -> Option<u16> {
    let b = blob.get(off..off + 2)?;
    Some(u16::from_le_bytes([b[0], b[1]]))
}

/// A located AMD microcode patch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AmdPatch {
    /// Byte offset of the patch payload (the patch header) within the blob.
    pub data_offset: usize,
    /// Length of the patch payload in bytes.
    pub data_len: usize,
    /// The patch revision (`patch_id`); compare against the running level.
    pub patch_id: u32,
    /// The equivalence id this patch targets.
    pub processor_rev_id: u16,
}

/// Validate the container magic.
pub fn is_amd_container(blob: &[u8]) -> bool {
    read_u32(blob, 0) == Some(AMD_CONTAINER_MAGIC)
}

/// Map the running CPU's `CPUID Fn0000_0001 EAX` signature (`installed_cpu`) to
/// its equivalence id by scanning the container's equivalence table. Returns
/// `None` if the magic is wrong, the table is malformed, or no entry matches.
pub fn find_amd_equiv_id(blob: &[u8], installed_cpu: u32) -> Option<u16> {
    if !is_amd_container(blob) {
        return None;
    }
    // First section must be the equivalence table.
    let sec_type = read_u32(blob, 4)?;
    let sec_len = read_u32(blob, 8)? as usize;
    if sec_type != SECTION_TYPE_EQUIV_TABLE {
        return None;
    }
    let table_start = 12usize;
    let table_end = table_start.checked_add(sec_len)?;
    if table_end > blob.len() {
        return None;
    }
    let mut off = table_start;
    while off + EQUIV_ENTRY_LEN <= table_end {
        let entry_cpu = read_u32(blob, off)?;
        if entry_cpu == 0 {
            break; // zero terminator
        }
        if entry_cpu == installed_cpu {
            return read_u16(blob, off + 12);
        }
        off += EQUIV_ENTRY_LEN;
    }
    None
}

/// Locate the patch in the container whose `processor_rev_id` matches
/// `equiv_id`. Walks the type-1 sections that follow the equivalence table.
pub fn find_amd_patch(blob: &[u8], equiv_id: u16) -> Option<AmdPatch> {
    if !is_amd_container(blob) {
        return None;
    }
    let equiv_len = read_u32(blob, 8)? as usize;
    // Sections start right after the equivalence-table section.
    let mut off = 12usize.checked_add(equiv_len)?;
    while off + 8 <= blob.len() {
        let sec_type = read_u32(blob, off)?;
        let sec_len = read_u32(blob, off + 4)? as usize;
        let payload = off + 8;
        let payload_end = payload.checked_add(sec_len)?;
        if payload_end > blob.len() {
            return None;
        }
        if sec_type == SECTION_TYPE_PATCH
            && sec_len > PATCH_OFF_PROCESSOR_REV_ID + 2
            && read_u16(blob, payload + PATCH_OFF_PROCESSOR_REV_ID) == Some(equiv_id)
        {
            return Some(AmdPatch {
                data_offset: payload,
                data_len: sec_len,
                patch_id: read_u32(blob, payload + PATCH_OFF_PATCH_ID)?,
                processor_rev_id: equiv_id,
            });
        }
        off = payload_end;
    }
    None
}

/// Convenience: find the patch applicable to `installed_cpu` whose revision is
/// strictly newer than `current_level`. Returns `None` when there is no match
/// or the candidate is not newer (so the caller skips the MSR write).
pub fn find_applicable_amd_patch(
    blob: &[u8],
    installed_cpu: u32,
    current_level: u32,
) -> Option<AmdPatch> {
    let equiv_id = find_amd_equiv_id(blob, installed_cpu)?;
    let patch = find_amd_patch(blob, equiv_id)?;
    if patch.patch_id > current_level {
        Some(patch)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    extern crate alloc;
    use alloc::vec::Vec;

    /// Build a minimal AMD container: magic + equiv table (one real entry +
    /// zero terminator) + one patch section.
    fn build_container(installed_cpu: u32, equiv_id: u16, patch_id: u32) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&AMD_CONTAINER_MAGIC.to_le_bytes());
        // Equiv table section: 2 entries (one real + zero terminator) = 32 bytes.
        b.extend_from_slice(&SECTION_TYPE_EQUIV_TABLE.to_le_bytes());
        b.extend_from_slice(&32u32.to_le_bytes());
        // entry 0
        b.extend_from_slice(&installed_cpu.to_le_bytes());
        b.extend_from_slice(&0u32.to_le_bytes()); // errata_mask
        b.extend_from_slice(&0u32.to_le_bytes()); // errata_compare
        b.extend_from_slice(&equiv_id.to_le_bytes());
        b.extend_from_slice(&0u16.to_le_bytes()); // reserved
        // entry 1: zero terminator
        b.extend_from_slice(&[0u8; 16]);
        // Patch section: header large enough to carry processor_rev_id @0x18.
        let mut patch = alloc::vec![0u8; 0x20];
        patch[PATCH_OFF_PATCH_ID..PATCH_OFF_PATCH_ID + 4].copy_from_slice(&patch_id.to_le_bytes());
        patch[PATCH_OFF_PROCESSOR_REV_ID..PATCH_OFF_PROCESSOR_REV_ID + 2]
            .copy_from_slice(&equiv_id.to_le_bytes());
        b.extend_from_slice(&SECTION_TYPE_PATCH.to_le_bytes());
        b.extend_from_slice(&(patch.len() as u32).to_le_bytes());
        b.extend_from_slice(&patch);
        b
    }

    #[test]
    fn magic_check() {
        assert!(is_amd_container(&AMD_CONTAINER_MAGIC.to_le_bytes()));
        assert!(!is_amd_container(&[0, 0, 0, 0]));
        assert!(!is_amd_container(&[]));
    }

    #[test]
    fn equiv_id_lookup() {
        let c = build_container(0x00a0_0f10, 0xa010, 0x0a00_107a);
        assert_eq!(find_amd_equiv_id(&c, 0x00a0_0f10), Some(0xa010));
        // Unknown CPU signature → no equiv id.
        assert_eq!(find_amd_equiv_id(&c, 0x00b0_0000), None);
    }

    #[test]
    fn patch_lookup() {
        let c = build_container(0x00a0_0f10, 0xa010, 0x0a00_107a);
        let p = find_amd_patch(&c, 0xa010).expect("patch present");
        assert_eq!(p.patch_id, 0x0a00_107a);
        assert_eq!(p.processor_rev_id, 0xa010);
        // No patch for an unmatched equiv id.
        assert_eq!(find_amd_patch(&c, 0xffff), None);
    }

    #[test]
    fn applicable_only_when_newer() {
        let c = build_container(0x00a0_0f10, 0xa010, 0x0a00_107a);
        // Current level older → patch applies.
        assert!(find_applicable_amd_patch(&c, 0x00a0_0f10, 0x0a00_1000).is_some());
        // Current level equal → skip (not strictly newer).
        assert!(find_applicable_amd_patch(&c, 0x00a0_0f10, 0x0a00_107a).is_none());
        // Current level newer → skip.
        assert!(find_applicable_amd_patch(&c, 0x00a0_0f10, 0x0a00_1fff).is_none());
        // Unknown CPU → skip.
        assert!(find_applicable_amd_patch(&c, 0xdead_beef, 0).is_none());
    }

    #[test]
    fn truncated_blob_is_safe() {
        let c = build_container(0x00a0_0f10, 0xa010, 0x0a00_107a);
        for n in 0..c.len() {
            // No panic / OOB on any truncation.
            let _ = find_amd_equiv_id(&c[..n], 0x00a0_0f10);
            let _ = find_amd_patch(&c[..n], 0xa010);
        }
    }

    #[test]
    fn oversized_section_length_is_safe() {
        // A full-length (non-truncated) blob whose section-length field is
        // hostile (`u32::MAX`) must return None without panicking or reading
        // out of bounds — exercises the `*_end > blob.len()` guards directly
        // (the truncation sweep above only ever shrinks valid lengths).

        // Equiv-table section claiming a u32::MAX payload length.
        let mut b = Vec::new();
        b.extend_from_slice(&AMD_CONTAINER_MAGIC.to_le_bytes());
        b.extend_from_slice(&SECTION_TYPE_EQUIV_TABLE.to_le_bytes());
        b.extend_from_slice(&u32::MAX.to_le_bytes()); // hostile length
        b.extend_from_slice(&[0u8; 32]); // payload far smaller than claimed
        assert_eq!(find_amd_equiv_id(&b, 0x00a0_0f10), None);
        assert_eq!(find_amd_patch(&b, 0xa010), None);

        // Valid (empty) equiv table followed by a patch section with a hostile
        // length.
        let mut c = Vec::new();
        c.extend_from_slice(&AMD_CONTAINER_MAGIC.to_le_bytes());
        c.extend_from_slice(&SECTION_TYPE_EQUIV_TABLE.to_le_bytes());
        c.extend_from_slice(&16u32.to_le_bytes()); // one zero-terminator entry
        c.extend_from_slice(&[0u8; 16]); // zero-terminator equiv entry
        c.extend_from_slice(&SECTION_TYPE_PATCH.to_le_bytes());
        c.extend_from_slice(&u32::MAX.to_le_bytes()); // hostile patch length
        c.extend_from_slice(&[0u8; 8]); // truncated patch payload
        assert_eq!(find_amd_patch(&c, 0xa010), None);
    }
}
