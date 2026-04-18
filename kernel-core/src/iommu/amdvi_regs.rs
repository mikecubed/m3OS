//! AMD-Vi MMIO register offsets + device-table-entry bit layouts.
//!
//! Pure logic, host-testable. Phase 55a Track D.1 — test-first commit.
//! This commit introduces the public test surface and stub types; the
//! encode / decode implementations that make the tests pass land in the
//! following commit.

// Register offsets (values will land alongside the implementation commit).
pub const REG_DEV_TAB_BAR: usize = 0x0000;
pub const REG_CMD_BUF_BAR: usize = 0x0008;
pub const REG_EVENT_LOG_BAR: usize = 0x0010;
pub const REG_CONTROL: usize = 0x0018;
pub const REG_EXT_FEATURE: usize = 0x0030;
pub const REG_MSI_CTRL: usize = 0x0158;
pub const REG_MSI_ADDR_LO: usize = 0x015C;
pub const REG_MSI_ADDR_HI: usize = 0x0160;
pub const REG_MSI_DATA: usize = 0x0164;
pub const REG_CMD_BUF_HEAD: usize = 0x2000;
pub const REG_CMD_BUF_TAIL: usize = 0x2008;
pub const REG_EVENT_LOG_HEAD: usize = 0x2010;
pub const REG_EVENT_LOG_TAIL: usize = 0x2018;
pub const REG_STATUS: usize = 0x2020;

pub const PFN_MASK_40: u64 = (1 << 40) - 1;
pub const STORE_ADDR_MASK_W0: u64 = ((1u64 << 52) - 1) & !0x7u64;

pub struct ControlBits;
impl ControlBits {
    pub const IOMMU_EN: u64 = 0;
    pub const HT_TUN_EN: u64 = 0;
    pub const EVENT_LOG_EN: u64 = 0;
    pub const EVENT_INT_EN: u64 = 0;
    pub const COMP_WAIT_INT_EN: u64 = 0;
    pub const CMD_BUF_EN: u64 = 0;
    pub const PPR_LOG_EN: u64 = 0;
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DeviceTableEntry {
    pub words: [u64; 4],
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DeviceTableFields {
    pub valid: bool,
    pub translation_valid: bool,
    pub page_table_root_pfn: u64,
    pub mode: u8,
    pub io_read: bool,
    pub io_write: bool,
    pub suppress_io_fault: bool,
}

impl DeviceTableEntry {
    pub const fn empty() -> Self {
        Self { words: [0; 4] }
    }
    // Failing stubs — the implementation commit fills these in.
    pub fn encode(_: DeviceTableFields) -> Self {
        Self::empty()
    }
    pub fn decode(&self) -> DeviceTableFields {
        DeviceTableFields::default()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CommandEntry {
    pub words: [u64; 2],
}

pub struct CommandOpcode;
impl CommandOpcode {
    pub const COMPLETION_WAIT: u8 = 0x01;
    pub const INVALIDATE_DEVTAB_ENTRY: u8 = 0x02;
    pub const INVALIDATE_IOMMU_PAGES: u8 = 0x03;
}

impl CommandEntry {
    pub fn completion_wait(_addr: u64, _marker: u64) -> Self {
        Self::default()
    }
    pub fn invalidate_devtab_entry(_id: u16) -> Self {
        Self::default()
    }
    pub fn invalidate_iommu_pages_all(_id: u16) -> Self {
        Self::default()
    }
    pub fn opcode(&self) -> u8 {
        0
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EventEntry {
    pub words: [u64; 2],
}

pub struct EventCode;
impl EventCode {
    pub const ILLEGAL_DEV_TABLE_ENTRY: u8 = 0x1;
    pub const IO_PAGE_FAULT: u8 = 0x2;
    pub const DEV_TAB_HW_ERROR: u8 = 0x3;
    pub const PAGE_TAB_HW_ERROR: u8 = 0x4;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DecodedEvent {
    pub code: u8,
    pub device_id: u16,
    pub domain_id: u16,
    pub address: u64,
}

impl EventEntry {
    pub fn new(word0: u64, word1: u64) -> Self {
        Self {
            words: [word0, word1],
        }
    }
    pub fn decode(&self) -> DecodedEvent {
        DecodedEvent {
            code: 0,
            device_id: 0,
            domain_id: 0,
            address: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn device_table_entry_empty_is_all_zero() {
        let e = DeviceTableEntry::empty();
        assert_eq!(e.words, [0; 4]);
        let f = e.decode();
        assert!(!f.valid);
    }

    #[test]
    fn device_table_entry_round_trip_basic() {
        let fields = DeviceTableFields {
            valid: true,
            translation_valid: true,
            page_table_root_pfn: 0x1234_5,
            mode: 3,
            io_read: true,
            io_write: true,
            suppress_io_fault: false,
        };
        let entry = DeviceTableEntry::encode(fields);
        let decoded = entry.decode();
        assert_eq!(decoded, fields);
    }

    #[test]
    fn device_table_entry_pfn_mask_truncates_to_40_bits() {
        let fields = DeviceTableFields {
            valid: true,
            translation_valid: true,
            page_table_root_pfn: u64::MAX,
            mode: 3,
            io_read: true,
            io_write: true,
            suppress_io_fault: false,
        };
        let entry = DeviceTableEntry::encode(fields);
        let decoded = entry.decode();
        assert_eq!(decoded.page_table_root_pfn, PFN_MASK_40);
    }

    #[test]
    fn device_table_entry_permission_bits_isolated() {
        let f = DeviceTableFields {
            valid: true,
            translation_valid: true,
            page_table_root_pfn: 1,
            mode: 3,
            io_read: true,
            io_write: false,
            suppress_io_fault: false,
        };
        let e = DeviceTableEntry::encode(f);
        assert!((e.words[1] & (1 << 61)) != 0);
        assert!((e.words[1] & (1 << 62)) == 0);
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(128))]

        #[test]
        fn device_table_entry_encode_decode_roundtrip(
            valid in any::<bool>(),
            tv in any::<bool>(),
            pfn in any::<u64>(),
            mode in 0u8..=7,
            ior in any::<bool>(),
            iow in any::<bool>(),
            sif in any::<bool>(),
        ) {
            let fields = DeviceTableFields {
                valid,
                translation_valid: tv,
                page_table_root_pfn: pfn & PFN_MASK_40,
                mode,
                io_read: ior,
                io_write: iow,
                suppress_io_fault: sif,
            };
            let entry = DeviceTableEntry::encode(fields);
            let decoded = entry.decode();
            prop_assert_eq!(decoded, fields);
        }
    }

    #[test]
    fn completion_wait_command_carries_opcode_and_marker() {
        let cmd = CommandEntry::completion_wait(0x1234_0000, 0xFEED_FACE_DEAD_BEEF);
        assert_eq!(cmd.opcode(), CommandOpcode::COMPLETION_WAIT);
        assert!((cmd.words[0] & 0x1) != 0);
        assert_eq!(cmd.words[1], 0xFEED_FACE_DEAD_BEEF);
        assert_eq!(cmd.words[0] & STORE_ADDR_MASK_W0, 0x1234_0000);
    }

    #[test]
    fn invalidate_devtab_entry_carries_device_id() {
        let cmd = CommandEntry::invalidate_devtab_entry(0x0123);
        assert_eq!(cmd.opcode(), CommandOpcode::INVALIDATE_DEVTAB_ENTRY);
        assert_eq!(cmd.words[0] & 0xFFFF, 0x0123);
    }

    #[test]
    fn invalidate_iommu_pages_all_carries_domain_id_and_s_bit() {
        let cmd = CommandEntry::invalidate_iommu_pages_all(0x00AB);
        assert_eq!(cmd.opcode(), CommandOpcode::INVALIDATE_IOMMU_PAGES);
        assert_eq!((cmd.words[0] >> 32) & 0xFFFF, 0x00AB);
        assert!((cmd.words[1] & 0x1) != 0);
    }

    #[test]
    fn io_page_fault_decode_extracts_device_and_address() {
        let word0 = ((EventCode::IO_PAGE_FAULT as u64) << 60)
            | (0x0005u64 << 32)
            | 0x0100u64;
        let word1 = 0xDEAD_BEEF_0000u64;
        let event = EventEntry::new(word0, word1);
        let decoded = event.decode();
        assert_eq!(decoded.code, EventCode::IO_PAGE_FAULT);
        assert_eq!(decoded.device_id, 0x0100);
        assert_eq!(decoded.domain_id, 0x0005);
        assert_eq!(decoded.address, 0xDEAD_BEEF_0000);
    }

    #[test]
    fn unknown_event_code_still_decodes_safely() {
        let word0 = (0xFu64 << 60) | 0x0042u64;
        let event = EventEntry::new(word0, 0);
        let decoded = event.decode();
        assert_eq!(decoded.code, 0xF);
        assert_eq!(decoded.device_id, 0x0042);
    }

    #[test]
    fn control_bits_are_distinct() {
        let bits = [
            ControlBits::IOMMU_EN,
            ControlBits::HT_TUN_EN,
            ControlBits::EVENT_LOG_EN,
            ControlBits::EVENT_INT_EN,
            ControlBits::COMP_WAIT_INT_EN,
            ControlBits::CMD_BUF_EN,
            ControlBits::PPR_LOG_EN,
        ];
        for b in bits {
            assert_eq!(b.count_ones(), 1);
        }
        for i in 0..bits.len() {
            for j in (i + 1)..bits.len() {
                assert_eq!(bits[i] & bits[j], 0);
            }
        }
    }
}
