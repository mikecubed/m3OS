//! VT-d second-level page-table bit layouts and software walker — Phase 55a
//! Track C.2.
//!
//! Pure-logic, host-testable. This module encodes the 64-bit "second-level
//! page-table entry" shape the Intel VT-d 3.3 spec §9.8 defines for
//! legacy-mode (non-scalable) translation. The kernel-side driver
//! (`kernel/src/iommu/intel.rs`) installs these entries in pages backed by
//! the buddy allocator; the walker here is used by host-side tests to
//! confirm encode/decode round-trips match what a correct IOMMU would see.
//!
//! # SL-PTE (second-level page-table entry) bit layout
//!
//! Per Intel VT-d 3.3 Table §9.8 (legacy-mode second-level tables):
//!
//! ```text
//!  63                                                         12 11     0
//! +---------------------------------------------------------------+------+
//! | physical page number (up to 52-bit pfn shifted by 12)         | ctrl |
//! +---------------------------------------------------------------+------+
//! ```
//!
//! Control bits we use in Phase 55a:
//!
//! | Bit  | Name | Meaning |
//! |-----:|------|---------|
//! |  0   | R    | Readable by device |
//! |  1   | W    | Writable by device |
//! |  7   | SP   | Super-page (PDE-level 2 MiB, PDPE-level 1 GiB) |
//!
//! All other bits in [11:2] and [11:8] are "available for software" or
//! "ignored" at Phase 55a — we write them as zero. An all-zero entry is
//! "not present" (neither R nor W set).
//!
//! # Walker trait
//!
//! Resolving an IOVA through a constructed second-level page table requires
//! reading four 4 KiB pages (PML4 → PDPT → PD → PT). Kernel code reaches
//! them through the phys-offset window; host tests construct a fake memory
//! map out of `Vec<[u64; 512]>`. Both paths go through [`PhysMemAccess`].

use alloc::vec::Vec;

/// 4 KiB page size in bytes.
pub const PAGE_SIZE: u64 = 4096;

/// Bit mask selecting the control-bit field (low 12 bits of a SL-PTE).
pub const PTE_CTRL_MASK: u64 = 0xFFF;

/// Bit mask selecting the physical-page-number field (bits [51:12]).
///
/// The VT-d spec caps the "host address width" at 52 bits (4 PB); the
/// top 12 bits are reserved as zero. Mask accordingly.
pub const PTE_PPN_MASK: u64 = 0x000F_FFFF_FFFF_F000;

/// Shift amount from pfn (page-frame number) to byte address.
pub const PTE_PPN_SHIFT: u32 = 12;

// ---------------------------------------------------------------------------
// VtdPteFlags
// ---------------------------------------------------------------------------

/// Control-bit subset of a VT-d SL-PTE. See module docs for the full
/// layout table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VtdPteFlags(pub u8);

impl VtdPteFlags {
    /// Entry is readable by the device (bit 0).
    pub const READ: Self = Self(1 << 0);
    /// Entry is writable by the device (bit 1).
    pub const WRITE: Self = Self(1 << 1);
    /// Super-page indicator (bit 7): at a non-leaf level, treat this as a
    /// terminal 2 MiB or 1 GiB mapping rather than a pointer to the next
    /// level.
    pub const SUPER_PAGE: Self = Self(1 << 7);

    /// Empty mask — "not present".
    pub const NONE: Self = Self(0);

    /// Raw bit access for tests and MMIO.
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// `true` if every bit in `other` is set in `self`.
    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    /// Compose two flag masks via bitwise OR.
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

impl core::ops::BitOr for VtdPteFlags {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        self.union(rhs)
    }
}

// ---------------------------------------------------------------------------
// VtdPageTableEntry
// ---------------------------------------------------------------------------

/// A single 64-bit SL-PTE.
///
/// Internally stored as the raw wire word; `encode` / `decode` round-trip
/// through `(phys, flags)`. An all-zero entry means "not present".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VtdPageTableEntry(pub u64);

impl VtdPageTableEntry {
    /// Construct a present entry pointing at `phys` with the named `flags`.
    ///
    /// `phys` is the physical byte address of the target page; only the
    /// page-aligned bits survive (low 12 bits are masked to zero).
    pub const fn new(phys: u64, flags: VtdPteFlags) -> Self {
        let ppn = phys & PTE_PPN_MASK;
        Self(ppn | (flags.0 as u64))
    }

    /// Raw 64-bit wire word.
    pub const fn encode(self) -> u64 {
        self.0
    }

    /// Reconstruct a `VtdPageTableEntry` from its wire word.
    ///
    /// Pure bit-twiddle; always succeeds. A round-trip through `encode` /
    /// `decode` is the identity provided the caller supplied a wire word
    /// that only uses documented bits (upper reserved bits zero).
    pub const fn decode(raw: u64) -> Self {
        // Preserve only documented bits. Upper reserved bits (bits [63:52])
        // and internal "available" bits in [11:2] & [11:8] are stripped so
        // decode(encode(x)) is the identity on constructed entries.
        let ppn = raw & PTE_PPN_MASK;
        let flags = raw
            & (VtdPteFlags::READ.0 as u64
                | VtdPteFlags::WRITE.0 as u64
                | VtdPteFlags::SUPER_PAGE.0 as u64);
        Self(ppn | flags)
    }

    /// Extract the target physical page address (page-aligned).
    pub const fn phys(self) -> u64 {
        self.0 & PTE_PPN_MASK
    }

    /// Extract the flag bits.
    pub const fn flags(self) -> VtdPteFlags {
        VtdPteFlags((self.0 & 0xFF) as u8)
    }

    /// `true` if the entry is present — i.e. at least one of R, W is set.
    pub const fn is_present(self) -> bool {
        (self.0 & (VtdPteFlags::READ.0 as u64 | VtdPteFlags::WRITE.0 as u64)) != 0
    }

    /// `true` if the super-page bit is set (leaf at PDE / PDPE level).
    pub const fn is_super_page(self) -> bool {
        (self.0 & VtdPteFlags::SUPER_PAGE.0 as u64) != 0
    }
}

// ---------------------------------------------------------------------------
// PhysMemAccess — read-only physical-memory window
// ---------------------------------------------------------------------------

/// Read-only window onto physical memory. Kernel code implements this via
/// the phys-offset window; host tests implement it via a `Vec<[u64; 512]>`
/// mock. Kept narrow on purpose: the walker only needs `u64` page-table
/// entry reads.
pub trait PhysMemAccess {
    /// Read the 64-bit value at physical address `phys`.
    ///
    /// `phys` must be 8-byte aligned; a walker that passes a misaligned
    /// address is a bug and implementations may return 0 rather than
    /// panic.
    fn read_u64(&self, phys: u64) -> u64;
}

// ---------------------------------------------------------------------------
// Walker: IOVA → physical, for a constructed 4-level SL page table
// ---------------------------------------------------------------------------

/// VT-d second-level 4-level index extraction.
///
/// The VT-d SL page-table walks use the same nine-bit-per-level split as
/// x86_64 4-level paging: `[47:39]=PML4`, `[38:30]=PDPT`, `[29:21]=PD`,
/// `[20:12]=PT`, `[11:0]=offset`. At the PDPT and PD levels a SuperPage
/// bit (bit 7) in the entry turns the walk into a terminal 1 GiB / 2 MiB
/// translation.
pub const LEVEL_SHIFTS: [u32; 4] = [39, 30, 21, 12];

/// Extract the 9-bit index into the table at `level` (0 = PML4, 3 = PT).
pub const fn level_index(iova: u64, level: usize) -> usize {
    ((iova >> LEVEL_SHIFTS[level]) & 0x1FF) as usize
}

/// Walk a VT-d second-level page table rooted at `root_phys` resolving
/// `iova` to the target physical byte address.
///
/// Returns `None` if any entry along the walk is not present. Handles
/// SuperPage terminal entries at the PDPT (1 GiB) or PD (2 MiB) level as
/// long as `page_sizes_supported` includes the matching size bit
/// (`1 << 30` for 1 GiB, `1 << 21` for 2 MiB). A walker that sees an
/// unsupported super-page size returns `None` — the contract is "walker
/// answers what this unit would answer".
pub fn walk(
    access: &dyn PhysMemAccess,
    root_phys: u64,
    iova: u64,
    page_sizes_supported: u64,
) -> Option<u64> {
    let mut table_phys = root_phys;
    let mut level = 0usize;
    loop {
        let idx = level_index(iova, level);
        let entry_phys = table_phys + (idx as u64) * 8;
        let raw = access.read_u64(entry_phys);
        let pte = VtdPageTableEntry::decode(raw);
        if !pte.is_present() {
            return None;
        }

        // Leaf PT (level 3) is always a 4 KiB terminal.
        if level == 3 {
            if (page_sizes_supported & (1u64 << 12)) == 0 {
                return None;
            }
            let offset_in_page = iova & 0xFFF;
            return Some(pte.phys() | offset_in_page);
        }

        // Super-page terminal at PDPT (level 1 → 1 GiB) or PD (level 2 → 2 MiB).
        if pte.is_super_page() {
            let size_bit = match level {
                1 => 1u64 << 30,  // 1 GiB
                2 => 1u64 << 21,  // 2 MiB
                _ => return None, // SP at PML4 is reserved in the spec
            };
            if (page_sizes_supported & size_bit) == 0 {
                return None;
            }
            let page_mask = size_bit - 1;
            let offset_in_page = iova & page_mask;
            return Some((pte.phys() & !page_mask) | offset_in_page);
        }

        // Descend.
        table_phys = pte.phys();
        level += 1;
    }
}

// ---------------------------------------------------------------------------
// Vec-backed PhysMemAccess mock (host tests)
// ---------------------------------------------------------------------------

/// Simple mock used by kernel-core tests. Backing store is a `Vec<u8>`
/// covering `[0, size)`. `read_u64` does an 8-byte little-endian read at
/// the given physical offset, returning 0 if the access is out of bounds.
///
/// Exposed as `pub` so the Track C kernel crate could (in principle)
/// reuse it; in practice kernel code uses the phys-offset window. The
/// type stays here to keep the test suite self-contained.
pub struct VecPhysMem {
    mem: Vec<u8>,
}

impl VecPhysMem {
    /// New zero-initialised window of `size` bytes.
    pub fn new(size: usize) -> Self {
        Self {
            mem: alloc::vec![0u8; size],
        }
    }

    /// Write a 64-bit word at `phys`. Out-of-bounds writes are silently
    /// clamped (the test either gets back 0 or the in-bound bytes).
    pub fn write_u64(&mut self, phys: u64, value: u64) {
        let start = phys as usize;
        let bytes = value.to_le_bytes();
        for (i, byte) in bytes.iter().enumerate() {
            if start + i < self.mem.len() {
                self.mem[start + i] = *byte;
            }
        }
    }

    /// Physical size of the backing store in bytes.
    pub fn size(&self) -> u64 {
        self.mem.len() as u64
    }
}

impl PhysMemAccess for VecPhysMem {
    fn read_u64(&self, phys: u64) -> u64 {
        let start = phys as usize;
        if start + 8 > self.mem.len() {
            return 0;
        }
        let mut buf = [0u8; 8];
        buf.copy_from_slice(&self.mem[start..start + 8]);
        u64::from_le_bytes(buf)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn new_preserves_phys_and_flags() {
        let pte = VtdPageTableEntry::new(0xDEAD_B000, VtdPteFlags::READ | VtdPteFlags::WRITE);
        assert_eq!(pte.phys(), 0xDEAD_B000);
        assert!(pte.flags().contains(VtdPteFlags::READ));
        assert!(pte.flags().contains(VtdPteFlags::WRITE));
        assert!(!pte.flags().contains(VtdPteFlags::SUPER_PAGE));
        assert!(pte.is_present());
    }

    #[test]
    fn not_present_when_no_rw_bit() {
        let pte = VtdPageTableEntry::new(0xCAFE_0000, VtdPteFlags::NONE);
        assert!(!pte.is_present());
    }

    #[test]
    fn new_strips_misaligned_low_bits() {
        // Caller hands in a misaligned address. The low 12 bits belong to
        // the flag field; the encoder must zero them out of the phys
        // portion so no bleed into the flags.
        let pte = VtdPageTableEntry::new(0x1234_5678, VtdPteFlags::READ);
        assert_eq!(pte.phys(), 0x1234_5000);
        assert_eq!(pte.flags(), VtdPteFlags::READ);
    }

    #[test]
    fn encode_decode_round_trip_basic() {
        let original = VtdPageTableEntry::new(
            0x8000_1000,
            VtdPteFlags::READ | VtdPteFlags::WRITE | VtdPteFlags::SUPER_PAGE,
        );
        let raw = original.encode();
        let roundtrip = VtdPageTableEntry::decode(raw);
        assert_eq!(original, roundtrip);
    }

    #[test]
    fn decode_strips_reserved_upper_bits() {
        // Hardware's reserved bits [63:52] must not bleed into the phys
        // slot. Synthesize a word with a top-bit set and confirm decode
        // masks it off.
        let raw: u64 = 0xFFF0_0000_0000_0000 | 0xABCD_E000 | (VtdPteFlags::READ.0 as u64);
        let pte = VtdPageTableEntry::decode(raw);
        assert_eq!(pte.phys(), 0xABCD_E000);
        assert_eq!(pte.flags(), VtdPteFlags::READ);
    }

    #[test]
    fn level_index_extracts_correct_nine_bits() {
        // IOVA: PML4=0x1A3, PDPT=0x055, PD=0x0F2, PT=0x011, offset=0x123
        let iova =
            (0x1A3u64 << 39) | (0x055u64 << 30) | (0x0F2u64 << 21) | (0x011u64 << 12) | 0x123u64;
        assert_eq!(level_index(iova, 0), 0x1A3);
        assert_eq!(level_index(iova, 1), 0x055);
        assert_eq!(level_index(iova, 2), 0x0F2);
        assert_eq!(level_index(iova, 3), 0x011);
    }

    /// Build a 4-level SL page table in `mem` with a single 4 KiB leaf
    /// mapping `iova -> phys_target`. Returns the root phys.
    fn build_single_leaf_table(
        mem: &mut VecPhysMem,
        iova: u64,
        phys_target: u64,
        flags: VtdPteFlags,
    ) -> u64 {
        // Layout: root at 0x1000, PDPT at 0x2000, PD at 0x3000, PT at 0x4000,
        // target payload at phys_target. Caller must ensure mem is big enough.
        let root_phys = 0x1000u64;
        let pdpt_phys = 0x2000u64;
        let pd_phys = 0x3000u64;
        let pt_phys = 0x4000u64;

        let pml4_idx = level_index(iova, 0);
        let pdpt_idx = level_index(iova, 1);
        let pd_idx = level_index(iova, 2);
        let pt_idx = level_index(iova, 3);

        mem.write_u64(
            root_phys + (pml4_idx as u64) * 8,
            VtdPageTableEntry::new(pdpt_phys, VtdPteFlags::READ | VtdPteFlags::WRITE).encode(),
        );
        mem.write_u64(
            pdpt_phys + (pdpt_idx as u64) * 8,
            VtdPageTableEntry::new(pd_phys, VtdPteFlags::READ | VtdPteFlags::WRITE).encode(),
        );
        mem.write_u64(
            pd_phys + (pd_idx as u64) * 8,
            VtdPageTableEntry::new(pt_phys, VtdPteFlags::READ | VtdPteFlags::WRITE).encode(),
        );
        mem.write_u64(
            pt_phys + (pt_idx as u64) * 8,
            VtdPageTableEntry::new(phys_target, flags).encode(),
        );

        root_phys
    }

    #[test]
    fn walker_resolves_4k_leaf() {
        let mut mem = VecPhysMem::new(0x10000);
        let iova: u64 =
            (0x100u64 << 39) | (0x50u64 << 30) | (0x80u64 << 21) | (0x11u64 << 12) | 0x456;
        let phys_target: u64 = 0x5000;
        let root = build_single_leaf_table(
            &mut mem,
            iova,
            phys_target,
            VtdPteFlags::READ | VtdPteFlags::WRITE,
        );

        let phys = walk(&mem, root, iova, 1u64 << 12).expect("leaf should resolve");
        assert_eq!(phys, phys_target + 0x456);
    }

    #[test]
    fn walker_returns_none_on_missing_entry() {
        let mem = VecPhysMem::new(0x10000); // All zeros = not-present.
        let iova: u64 = 0x0001_0000;
        let r = walk(&mem, 0x1000, iova, 1u64 << 12);
        assert_eq!(r, None);
    }

    #[test]
    fn walker_resolves_2mib_super_page() {
        let mut mem = VecPhysMem::new(0x10000);
        let iova: u64 = (0x100u64 << 39) | (0x50u64 << 30) | (0x80u64 << 21) | 0x1_2345;
        // Note: 2 MiB page — PD entry carries SP bit and the phys_target is
        // 2 MiB-aligned.
        let phys_target: u64 = 0x20_0000;

        let root_phys = 0x1000u64;
        let pdpt_phys = 0x2000u64;
        let pd_phys = 0x3000u64;
        mem.write_u64(
            root_phys + (level_index(iova, 0) as u64) * 8,
            VtdPageTableEntry::new(pdpt_phys, VtdPteFlags::READ | VtdPteFlags::WRITE).encode(),
        );
        mem.write_u64(
            pdpt_phys + (level_index(iova, 1) as u64) * 8,
            VtdPageTableEntry::new(pd_phys, VtdPteFlags::READ | VtdPteFlags::WRITE).encode(),
        );
        mem.write_u64(
            pd_phys + (level_index(iova, 2) as u64) * 8,
            VtdPageTableEntry::new(
                phys_target,
                VtdPteFlags::READ | VtdPteFlags::WRITE | VtdPteFlags::SUPER_PAGE,
            )
            .encode(),
        );

        let phys = walk(&mem, root_phys, iova, (1u64 << 12) | (1u64 << 21))
            .expect("2 MiB super page should resolve");
        assert_eq!(phys, phys_target + 0x1_2345);
    }

    #[test]
    fn walker_rejects_super_page_when_size_unsupported() {
        // Same fixture as the 2 MiB test, but we refuse to advertise 2 MiB
        // support. Walker must return None.
        let mut mem = VecPhysMem::new(0x10000);
        let iova: u64 = (0x100u64 << 39) | (0x50u64 << 30) | (0x80u64 << 21);
        let phys_target: u64 = 0x20_0000;

        let root_phys = 0x1000u64;
        let pdpt_phys = 0x2000u64;
        let pd_phys = 0x3000u64;
        mem.write_u64(
            root_phys + (level_index(iova, 0) as u64) * 8,
            VtdPageTableEntry::new(pdpt_phys, VtdPteFlags::READ | VtdPteFlags::WRITE).encode(),
        );
        mem.write_u64(
            pdpt_phys + (level_index(iova, 1) as u64) * 8,
            VtdPageTableEntry::new(pd_phys, VtdPteFlags::READ | VtdPteFlags::WRITE).encode(),
        );
        mem.write_u64(
            pd_phys + (level_index(iova, 2) as u64) * 8,
            VtdPageTableEntry::new(
                phys_target,
                VtdPteFlags::READ | VtdPteFlags::WRITE | VtdPteFlags::SUPER_PAGE,
            )
            .encode(),
        );

        // page_sizes_supported = 4 KiB only
        let r = walk(&mem, root_phys, iova, 1u64 << 12);
        assert_eq!(r, None);
    }

    proptest! {
        #[test]
        fn prop_encode_decode_round_trip(
            pfn in 0u64..(1u64 << 40), // up to 40-bit pfn (covers 52-bit phys)
            read in any::<bool>(),
            write in any::<bool>(),
            super_page in any::<bool>(),
        ) {
            let phys = pfn << 12;
            let mut flags = VtdPteFlags::NONE;
            if read { flags = flags | VtdPteFlags::READ; }
            if write { flags = flags | VtdPteFlags::WRITE; }
            if super_page { flags = flags | VtdPteFlags::SUPER_PAGE; }
            let entry = VtdPageTableEntry::new(phys, flags);
            let round = VtdPageTableEntry::decode(entry.encode());
            prop_assert_eq!(entry, round);
            prop_assert_eq!(round.phys(), phys);
            prop_assert_eq!(round.flags(), flags);
        }
    }
}
