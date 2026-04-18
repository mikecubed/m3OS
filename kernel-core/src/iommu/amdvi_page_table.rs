//! AMD-Vi host-page-table entry layout + walker — test-first stub.
//!
//! Phase 55a Track D.2. Implementation lands in the next commit.

use alloc::vec::Vec;

pub const PFN_MASK_40: u64 = (1u64 << 40) - 1;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AmdViPageTableEntry(pub u64);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AmdViPteFlags {
    pub present: bool,
    pub io_read: bool,
    pub io_write: bool,
    pub force_coherent: bool,
    pub next_level: u8,
}

impl AmdViPageTableEntry {
    pub fn new(_phys: u64, _flags: AmdViPteFlags) -> Self {
        Self(0)
    }
    pub fn encode(self) -> u64 {
        self.0
    }
    pub fn decode(raw: u64) -> Self {
        Self(raw)
    }
    pub fn is_present(self) -> bool {
        false
    }
    pub fn next_level(self) -> u8 {
        0
    }
    pub fn pfn(self) -> u64 {
        0
    }
    pub fn phys_addr(self) -> u64 {
        0
    }
    pub fn flags(self) -> AmdViPteFlags {
        AmdViPteFlags::default()
    }
}

pub trait PhysMemAccess {
    fn read_u64(&self, phys: u64) -> Option<u64>;
}

pub fn walk(_root_phys: u64, _iova: u64, _mem: &dyn PhysMemAccess) -> Option<u64> {
    None
}

pub struct VecPhysMem {
    pub pages: Vec<(u64, Vec<u64>)>,
}
impl VecPhysMem {
    pub fn new() -> Self {
        Self { pages: Vec::new() }
    }
    pub fn add_page(&mut self, phys: u64, entries: Vec<u64>) {
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
        let iova = 0x1234_5678_9000u64;
        let leaf_phys = 0x5000u64;
        let root_phys = 0x1_0000u64;
        let l2_phys = 0x2_0000u64;
        let l1_phys = 0x3_0000u64;
        let l0_phys = 0x4_0000u64;

        let mut mem = VecPhysMem::new();
        let mut root = vec![0u64; 512];
        root[level_index(iova, 3)] = make_intermediate_entry(l2_phys, 2);
        mem.add_page(root_phys, root);
        let mut l2 = vec![0u64; 512];
        l2[level_index(iova, 2)] = make_intermediate_entry(l1_phys, 1);
        mem.add_page(l2_phys, l2);
        let mut l1 = vec![0u64; 512];
        l1[level_index(iova, 1)] = make_intermediate_entry(l0_phys, 1);
        mem.add_page(l1_phys, l1);
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
        let root = vec![0u64; 512];
        mem.add_page(root_phys, root);
        assert!(walk(root_phys, iova, &mem).is_none());
    }

    #[test]
    fn walker_resolves_2mib_leaf() {
        let iova = 0x0000_0040_0000_1234u64;
        let leaf_phys = 0x8000_0000_0000u64 & ((1u64 << 52) - 1);
        let leaf_phys = leaf_phys & !((1u64 << 21) - 1);
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
        let mut l1 = vec![0u64; 512];
        l1[level_index(iova, 1)] = make_leaf_entry(leaf_phys);
        mem.add_page(l1_phys, l1);

        let result = walk(root_phys, iova, &mem).expect("2MiB walk resolves");
        let expected = leaf_phys | (iova & ((1u64 << 21) - 1));
        assert_eq!(result, expected);
    }
}
