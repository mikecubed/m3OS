//! AMD-Vi host-page-table entry layout + walker — Phase 55a Track D.2.
//!
//! Pure logic, host-testable. The AMD I/O Virtualization Technology
//! specification (rev 3.00, §2.2.3 "I/O Page Tables") defines a 4-level
//! page-table hierarchy whose 8-byte entry encodes present / read /
//! write bits, the next-level page-frame number (or final physical
//! frame), and a 3-bit `NextLevel` field naming the kind of the target.
//!
//! # Entry layout (64 bits, spec §2.2.3)
//!
//! | Bits   | Field                                        |
//! |--------|----------------------------------------------|
//! |   0    | Present (P)                                  |
//! |   1    | Reserved                                     |
//! |   2    | Reserved                                     |
//! |   5    | Accessed (A)                                 |
//! |   6    | Dirty (D)                                    |
//! |  8:7   | Reserved                                     |
//! | 11:9   | NextLevel (0 = leaf; 1..6 = next-level page) |
//! | 51:12  | NextTablePfn / PageAddrPfn                   |
//! | 60     | Force Coherent (FC) — Phase 55a leaves 0     |
//! | 61     | IR (I/O Read permission)                     |
//! | 62     | IW (I/O Write permission)                    |
//! | 63     | Reserved                                     |
//!
//! # Walker contract
//!
//! [`walk`] takes a root page-table physical address, an IOVA, and a
//! [`PhysMemAccess`] trait object through which it reads successive
//! page-table pages. It returns `Some(phys)` on a complete walk, `None`
//! when any entry on the path has the Present bit clear. It is
//! unit-testable against an in-memory [`VecPhysMem`] fixture.

use alloc::vec::Vec;

/// AMD-Vi page-table entry — 64 bits of flags + PFN.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AmdViPageTableEntry(pub u64);

/// 40-bit PFN mask (physical address >> 12) — AMD-Vi spec caps IOMMU
/// physical addresses at bit 51.
pub const PFN_MASK_40: u64 = (1u64 << 40) - 1;

/// Permission / attribute flags callers hand to [`AmdViPageTableEntry::new`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AmdViPteFlags {
    /// Present bit — must be set for the translation to succeed.
    pub present: bool,
    /// I/O Read permission.
    pub io_read: bool,
    /// I/O Write permission.
    pub io_write: bool,
    /// Force Coherent — Phase 55a leaves clear.
    pub force_coherent: bool,
    /// NextLevel: `0` = leaf, `1..=6` = level index of the page this
    /// entry points at. The walker uses this to decide when to stop
    /// descending.
    pub next_level: u8,
}

impl AmdViPageTableEntry {
    /// Build a new entry pointing at `phys` (a physical byte address,
    /// which will be shifted to a PFN) with `flags`.
    pub fn new(phys: u64, flags: AmdViPteFlags) -> Self {
        let pfn = (phys >> 12) & PFN_MASK_40;
        let mut raw: u64 = 0;
        if flags.present {
            raw |= 1 << 0;
        }
        raw |= ((flags.next_level as u64) & 0x7) << 9;
        raw |= pfn << 12;
        if flags.force_coherent {
            raw |= 1 << 60;
        }
        if flags.io_read {
            raw |= 1 << 61;
        }
        if flags.io_write {
            raw |= 1 << 62;
        }
        Self(raw)
    }

    /// Raw encoded bits — what the kernel writes into page-table memory.
    pub fn encode(self) -> u64 {
        self.0
    }

    /// Decode from raw bits.
    pub fn decode(raw: u64) -> Self {
        Self(raw)
    }

    /// `true` if the Present bit is set.
    pub fn is_present(self) -> bool {
        (self.0 & 0x1) != 0
    }

    /// NextLevel value in bits 11:9. `0` = leaf.
    pub fn next_level(self) -> u8 {
        ((self.0 >> 9) & 0x7) as u8
    }

    /// Physical frame number recorded in bits 51:12.
    pub fn pfn(self) -> u64 {
        (self.0 >> 12) & PFN_MASK_40
    }

    /// Byte address of the target (PFN << 12).
    pub fn phys_addr(self) -> u64 {
        self.pfn() << 12
    }

    /// Flags view of the entry.
    pub fn flags(self) -> AmdViPteFlags {
        AmdViPteFlags {
            present: self.is_present(),
            io_read: (self.0 & (1 << 61)) != 0,
            io_write: (self.0 & (1 << 62)) != 0,
            force_coherent: (self.0 & (1 << 60)) != 0,
            next_level: self.next_level(),
        }
    }
}

// ---------------------------------------------------------------------------
// Walker
// ---------------------------------------------------------------------------

/// Read-only access to host physical memory for the walker.
///
/// The walker runs in pure logic and never dereferences a raw address
/// directly. In the kernel, the implementation converts a physical
/// address into the kernel's physical-memory window and reads through a
/// `*const u64`; on the host the [`VecPhysMem`] fixture backs the calls
/// with a `BTreeMap`.
pub trait PhysMemAccess {
    /// Read the u64 at physical address `phys`. Must handle any address
    /// the walker asks for; implementations that cannot service the
    /// request return `None`.
    fn read_u64(&self, phys: u64) -> Option<u64>;
}

/// Walk the 4-level AMD-Vi page table rooted at `root_phys` for IOVA
/// `iova`, returning the final physical byte address when the walk
/// terminates on a present leaf. Returns `None` if any level's Present
/// bit is clear or if the backing memory access fails.
///
/// The walk descends from the highest level (root.next_level() initially
/// 3 for the 4-level tree Phase 55a uses) down to 0. At each level the
/// byte index within that level's page is `(iova >> shift) & 0x1FF` —
/// 9 bits per level, matching the 512-entry page structure shared by
/// VT-d second-level and AMD-Vi v2 host page tables.
///
/// The spec allows large-page leaves (2 MiB, 1 GiB) via setting
/// `next_level = 0` partway down the tree. Phase 55a accepts this shape
/// in the walker for completeness: a leaf encountered before level 0
/// terminates the walk with the leaf's PFN + the residual IOVA bits.
pub fn walk(root_phys: u64, iova: u64, mem: &dyn PhysMemAccess) -> Option<u64> {
    // Root page starts the walk; its entries are at level 3 (4-level tree).
    let mut table_phys = root_phys;
    // `level` here is the level of the *entries* we are about to read.
    let mut level: u8 = 3;
    loop {
        // 9 bits per level; level 3 contributes bits 47:39, level 2
        // 38:30, level 1 29:21, level 0 20:12. Final 12 bits are offset.
        let shift = 12 + 9 * (level as u32);
        let index = ((iova >> shift) & 0x1FF) as u64;
        let entry_phys = table_phys + index * 8;
        let raw = mem.read_u64(entry_phys)?;
        let entry = AmdViPageTableEntry::decode(raw);
        if !entry.is_present() {
            return None;
        }
        if entry.next_level() == 0 {
            // Leaf entry — assemble the final physical address. Residual
            // IOVA bits (below `shift`) become the byte offset inside
            // the mapped page.
            let mask = (1u64 << shift) - 1;
            return Some(entry.phys_addr() | (iova & mask));
        }
        // Intermediate: descend.
        table_phys = entry.phys_addr();
        if level == 0 {
            // level==0 with next_level != 0 is malformed. Stop instead
            // of walking forever.
            return None;
        }
        level -= 1;
    }
}

// ---------------------------------------------------------------------------
// Host-test fixture
// ---------------------------------------------------------------------------

/// In-memory [`PhysMemAccess`] backing for host tests. Callers push
/// pages onto `pages`, each with a fixed physical base, and `read_u64`
/// finds the enclosing page.
pub struct VecPhysMem {
    pub pages: Vec<(u64, Vec<u64>)>,
}

impl VecPhysMem {
    pub fn new() -> Self {
        Self { pages: Vec::new() }
    }

    /// Push a page of 512 u64 entries at physical base `phys`.
    pub fn add_page(&mut self, phys: u64, entries: Vec<u64>) {
        debug_assert_eq!(entries.len(), 512);
        self.pages.push((phys, entries));
    }
}

impl Default for VecPhysMem {
    fn default() -> Self {
        Self::new()
    }
}

impl PhysMemAccess for VecPhysMem {
    fn read_u64(&self, phys: u64) -> Option<u64> {
        for (base, entries) in &self.pages {
            if phys >= *base && phys < base + 4096 {
                let idx = ((phys - base) / 8) as usize;
                return entries.get(idx).copied();
            }
        }
        None
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use proptest::prelude::*;

    #[test]
    fn entry_round_trip_empty() {
        let e = AmdViPageTableEntry::decode(0);
        assert!(!e.is_present());
        assert_eq!(e.pfn(), 0);
    }

    #[test]
    fn entry_new_sets_present_and_pfn() {
        let e = AmdViPageTableEntry::new(
            0x1234_5000,
            AmdViPteFlags {
                present: true,
                io_read: true,
                io_write: true,
                force_coherent: false,
                next_level: 0,
            },
        );
        assert!(e.is_present());
        assert_eq!(e.pfn(), 0x1234_5);
        assert_eq!(e.phys_addr(), 0x1234_5000);
        assert_eq!(e.next_level(), 0);
        assert!(e.flags().io_read);
        assert!(e.flags().io_write);
    }

    #[test]
    fn entry_intermediate_has_next_level() {
        let e = AmdViPageTableEntry::new(
            0x0010_0000,
            AmdViPteFlags {
                present: true,
                io_read: true,
                io_write: true,
                force_coherent: false,
                next_level: 3,
            },
        );
        assert_eq!(e.next_level(), 3);
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        #[test]
        fn entry_encode_decode_roundtrip(
            present in any::<bool>(),
            ior in any::<bool>(),
            iow in any::<bool>(),
            fc in any::<bool>(),
            nl in 0u8..=6,
            pfn in any::<u64>(),
        ) {
            let phys = (pfn & PFN_MASK_40) << 12;
            let flags = AmdViPteFlags {
                present,
                io_read: ior,
                io_write: iow,
                force_coherent: fc,
                next_level: nl,
            };
            let entry = AmdViPageTableEntry::new(phys, flags);
            let decoded_flags = entry.flags();
            prop_assert_eq!(decoded_flags, flags);
            prop_assert_eq!(entry.phys_addr(), phys);
        }
    }

    // ---------- Walker integration tests ----------

    fn level_index(iova: u64, level: u8) -> usize {
        let shift = 12 + 9 * (level as u32);
        ((iova >> shift) & 0x1FF) as usize
    }

    fn make_intermediate_entry(next_phys: u64, next_level: u8) -> u64 {
        AmdViPageTableEntry::new(
            next_phys,
            AmdViPteFlags {
                present: true,
                io_read: true,
                io_write: true,
                force_coherent: false,
                next_level,
            },
        )
        .encode()
    }

    fn make_leaf_entry(leaf_phys: u64) -> u64 {
        AmdViPageTableEntry::new(
            leaf_phys,
            AmdViPteFlags {
                present: true,
                io_read: true,
                io_write: true,
                force_coherent: false,
                next_level: 0,
            },
        )
        .encode()
    }

    #[test]
    fn walker_resolves_4k_leaf() {
        // Build a 4-level path to IOVA 0x1234_5678_9000:
        let iova = 0x1234_5678_9000u64;
        let leaf_phys = 0x5000u64;
        let root_phys = 0x1_0000u64;
        let l2_phys = 0x2_0000u64;
        let l1_phys = 0x3_0000u64;
        let l0_phys = 0x4_0000u64;

        let mut mem = VecPhysMem::new();
        // Level 3 (root): index with bits 47:39 of IOVA.
        let mut root = vec![0u64; 512];
        root[level_index(iova, 3)] = make_intermediate_entry(l2_phys, 2);
        mem.add_page(root_phys, root);
        // Level 2.
        let mut l2 = vec![0u64; 512];
        l2[level_index(iova, 2)] = make_intermediate_entry(l1_phys, 1);
        mem.add_page(l2_phys, l2);
        // Level 1.
        let mut l1 = vec![0u64; 512];
        l1[level_index(iova, 1)] = make_intermediate_entry(l0_phys, 0);
        // WAIT: intermediate entry must have NextLevel = 1 (pointing to level 0)
        // so the walker descends. Rewrite.
        l1[level_index(iova, 1)] = make_intermediate_entry(l0_phys, 1);
        mem.add_page(l1_phys, l1);
        // Level 0 (leaf page).
        let mut l0 = vec![0u64; 512];
        l0[level_index(iova, 0)] = make_leaf_entry(leaf_phys);
        mem.add_page(l0_phys, l0);

        let result = walk(root_phys, iova, &mem).expect("walk resolves");
        assert_eq!(result, leaf_phys);
    }

    #[test]
    fn walker_returns_none_when_entry_not_present() {
        let iova = 0x1_0000u64;
        let root_phys = 0x1_0000u64;
        let mut mem = VecPhysMem::new();
        let root = vec![0u64; 512]; // everything zero = not present
        mem.add_page(root_phys, root);
        assert!(walk(root_phys, iova, &mem).is_none());
    }

    #[test]
    fn walker_resolves_2mib_leaf() {
        // A leaf at level 1 represents a 2 MiB page. The walker should
        // return leaf_phys + (iova & 0x1F_FFFF).
        let iova = 0x0000_0040_0000_1234u64; // 2MiB + 0x1234 within it
        let leaf_phys = 0x8000_0000_0000u64 & ((1u64 << 52) - 1);
        let leaf_phys = leaf_phys & !((1u64 << 21) - 1); // 2MiB-aligned
        let root_phys = 0x1_0000u64;
        let l2_phys = 0x2_0000u64;
        let l1_phys = 0x3_0000u64;

        let mut mem = VecPhysMem::new();
        let mut root = vec![0u64; 512];
        root[level_index(iova, 3)] = make_intermediate_entry(l2_phys, 2);
        mem.add_page(root_phys, root);
        let mut l2 = vec![0u64; 512];
        l2[level_index(iova, 2)] = make_intermediate_entry(l1_phys, 1);
        mem.add_page(l2_phys, l2);
        // Level 1 entry is a leaf (next_level=0) → stops walker with 2MiB mapping.
        let mut l1 = vec![0u64; 512];
        l1[level_index(iova, 1)] = make_leaf_entry(leaf_phys);
        mem.add_page(l1_phys, l1);

        let result = walk(root_phys, iova, &mem).expect("2MiB walk resolves");
        let expected = leaf_phys | (iova & ((1u64 << 21) - 1));
        assert_eq!(result, expected);
    }
}
