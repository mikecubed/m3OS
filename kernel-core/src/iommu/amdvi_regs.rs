//! AMD-Vi MMIO register offsets + device-table-entry bit layouts.
//!
//! Pure logic, host-testable. No MMIO access here — only the field
//! definitions and encode / decode helpers that let the kernel-side
//! [`kernel::iommu::amd::AmdViUnit`] (Phase 55a Track D) read and write
//! the structures the AMD I/O Virtualization spec defines.
//!
//! # Source reference
//!
//! Offsets and bit positions in this module match the AMD "I/O
//! Virtualization Technology (IOMMU) Specification", revision 3.00 (2016),
//! §3 "Data Structures" and §3.1 "MMIO Registers". Where the spec names a
//! field, the constant name below echoes it exactly so a spec reader can
//! grep either side.
//!
//! # What is here
//!
//! - Register offsets (`DEV_TAB_BAR`, `CMD_BUF_BAR`, `EVENT_LOG_BAR`,
//!   `CONTROL`, `STATUS`, `CMD_BUF_HEAD`, `CMD_BUF_TAIL`, etc.).
//! - [`ControlBits`] — the bit fields inside the CONTROL register that
//!   the bring-up sequence toggles in a specific order.
//! - [`DeviceTableEntry`] — the 256-bit per-BDF entry shape with
//!   [`DeviceTableEntry::encode`] / [`decode`] round-trip.
//! - Command-ring descriptor helpers ([`CommandEntry`]) for the two
//!   invalidation commands Phase 55a issues (INVALIDATE_IOMMU_PAGES,
//!   INVALIDATE_DEVTAB_ENTRY) plus the trailing COMPLETION_WAIT.
//! - Event-log descriptor decode ([`EventEntry::decode`]) for the
//!   IO_PAGE_FAULT and ILLEGAL_DEV_TABLE_ENTRY events the fault handler
//!   will surface through the shared `log_fault_event` path.
//!
//! # What is not here
//!
//! Hardware bring-up, MMIO volatile access, or IRQ registration. Those
//! live in `kernel/src/iommu/amd.rs`. This module is pure data — every
//! function takes and returns plain integers so it can be exercised on
//! the host via `cargo test -p kernel-core`.

// ---------------------------------------------------------------------------
// MMIO register offsets (AMD IOMMU spec §3.1)
// ---------------------------------------------------------------------------

/// Device-table base register (DEV_TAB_BAR). 8 bytes. Bits 11:0 encode
/// `Size = 2^(bits+1) * 4 KiB entries` minus one; we always use the
/// maximum (65536 entries = 2 MiB table → `Size = 0x1FF`). Bits 51:12
/// carry the table base address; bits 63:52 are reserved.
pub const REG_DEV_TAB_BAR: usize = 0x0000;

/// Command-buffer base register (CMD_BUF_BAR). 8 bytes. Bits 11:0
/// reserved. Bits 51:12 carry the buffer base. Bits 59:56 encode
/// `BufferLen = 2^(BufferLen) * 128 bytes` — minimum 8 (4 KiB),
/// maximum 15 (128 KiB).
pub const REG_CMD_BUF_BAR: usize = 0x0008;

/// Event-log base register (EVENT_LOG_BAR). Same shape as CMD_BUF_BAR.
pub const REG_EVENT_LOG_BAR: usize = 0x0010;

/// Control register (CONTROL). 8 bytes. See [`ControlBits`].
pub const REG_CONTROL: usize = 0x0018;

/// Status register (STATUS). 8 bytes. Bit 0 = event-log overflow, bit 1 =
/// event-log interrupt, bit 2 = completion-wait interrupt, bit 3 =
/// event-log running, bit 4 = command-buffer running.
pub const REG_STATUS: usize = 0x2020;

/// Command-buffer head pointer (CMD_BUF_HEAD). 8 bytes. Hardware updates
/// this as it consumes command entries.
pub const REG_CMD_BUF_HEAD: usize = 0x2000;

/// Command-buffer tail pointer (CMD_BUF_TAIL). 8 bytes. Software advances
/// this after writing a new command entry.
pub const REG_CMD_BUF_TAIL: usize = 0x2008;

/// Event-log head pointer (EVENT_LOG_HEAD). 8 bytes. Software advances
/// this as it drains event entries.
pub const REG_EVENT_LOG_HEAD: usize = 0x2010;

/// Event-log tail pointer (EVENT_LOG_TAIL). 8 bytes. Hardware advances
/// this as it posts new event entries.
pub const REG_EVENT_LOG_TAIL: usize = 0x2018;

/// Extended-feature register (EXT_FEATURE). 8 bytes. Bit 0 = prefetch,
/// bit 1 = PPR (peripheral page request), bit 6 = NX enable, bit 2 =
/// X2APIC supported, bits 15:14 = host-page-table depth, bits 12:8 = HATS
/// (host-address translation size). This register is the source Track D
/// consults to populate [`crate::iommu::contract::IommuCapabilities`].
pub const REG_EXT_FEATURE: usize = 0x0030;

/// MSI capability fields inside the register block (AMD-Vi MSI — not the
/// PCI MSI capability, which is a different pathway). Offset 0x0158 and
/// onwards: bit 0 of MSI_CTRL enables the IOMMU interrupt, followed by
/// the MSI address and data at 0x015C and 0x0160 respectively.
pub const REG_MSI_CTRL: usize = 0x0158;
/// Low 32 bits of the MSI address (system-wide APIC message address).
pub const REG_MSI_ADDR_LO: usize = 0x015C;
/// High 32 bits of the MSI address.
pub const REG_MSI_ADDR_HI: usize = 0x0160;
/// 32-bit MSI data (carries the vector byte in bits 7:0).
pub const REG_MSI_DATA: usize = 0x0164;

// ---------------------------------------------------------------------------
// CONTROL register bit positions (AMD IOMMU spec §3.1.3)
// ---------------------------------------------------------------------------

/// Bit layout of the CONTROL register. The bring-up sequence sets these
/// in a specific order (event-log → cmd-buf → IOMMU enable) per the
/// spec; the constants are named for the field the spec uses.
pub struct ControlBits;

impl ControlBits {
    /// IOMMU-enable bit. Set last.
    pub const IOMMU_EN: u64 = 1 << 0;
    /// HT tunnel translation enable.
    pub const HT_TUN_EN: u64 = 1 << 1;
    /// Event-log enable.
    pub const EVENT_LOG_EN: u64 = 1 << 2;
    /// Event-log interrupt enable (MSI fires on new events).
    pub const EVENT_INT_EN: u64 = 1 << 3;
    /// Completion-wait interrupt enable.
    pub const COMP_WAIT_INT_EN: u64 = 1 << 4;
    /// Command-buffer enable.
    pub const CMD_BUF_EN: u64 = 1 << 12;
    /// PPR log enable (Phase 55a leaves this off — no PASID support).
    pub const PPR_LOG_EN: u64 = 1 << 13;
}

// ---------------------------------------------------------------------------
// Device-table entry (AMD IOMMU spec §3.2.2.1)
// ---------------------------------------------------------------------------

/// Per-BDF device-table entry, 256 bits wide (32 bytes = 4 u64s).
///
/// Phase 55a populates only the subset needed for translation on a
/// claimed device: the Valid bit, the page-table root pfn, the paging
/// mode (levels), and the IR / IW permission bits. Remaining fields
/// (interrupt-map base, IOTLB invalidation, PASID context) remain zero.
///
/// The `words` field is `[u64; 4]` so the struct is naturally `Copy`
/// and the encode / decode round-trip works over the raw memory
/// representation callers will install in the device table.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DeviceTableEntry {
    /// Four little-endian 64-bit words laid out as in the spec.
    pub words: [u64; 4],
}

/// Fields the kernel cares about for Phase 55a. Kept as a separate view
/// type so `encode` / `decode` round-trip without caring about the
/// bits we never touch.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DeviceTableFields {
    /// Valid bit (word0 bit 0). When clear the entry does not translate.
    pub valid: bool,
    /// Translation-valid bit (word0 bit 1). Set alongside `valid` when
    /// page-table translation is active.
    pub translation_valid: bool,
    /// Page-table root page-frame number. Physical address >> 12.
    /// Occupies word0 bits 51:12 when `translation_valid` is set.
    pub page_table_root_pfn: u64,
    /// Paging mode: number of page-table levels. Legal values 0..=6;
    /// Phase 55a always uses 3 (4 KiB pages over 4-level hierarchy,
    /// matching the VT-d second-level shape and covering 48-bit IOVAs).
    /// Occupies word0 bits 11:9.
    pub mode: u8,
    /// IR bit (IO-read permission). Word1 bit 61.
    pub io_read: bool,
    /// IW bit (IO-write permission). Word1 bit 62.
    pub io_write: bool,
    /// Suppress-I/O-fault bit (word2 bit 4). Unused; exposed so round-trip
    /// tests cover the field.
    pub suppress_io_fault: bool,
}

impl DeviceTableEntry {
    /// Construct an empty entry (all zero = invalid, not translating).
    pub const fn empty() -> Self {
        Self { words: [0; 4] }
    }

    /// Encode [`DeviceTableFields`] into a new 256-bit entry.
    ///
    /// The `page_table_root_pfn` field is masked to 40 bits — AMD-Vi
    /// addresses at most bit 51, and PFN = addr >> 12 leaves 40 bits.
    /// Any high-order bits on the caller's pfn are silently dropped.
    pub fn encode(fields: DeviceTableFields) -> Self {
        let mut word0: u64 = 0;
        if fields.valid {
            word0 |= 1 << 0;
        }
        if fields.translation_valid {
            word0 |= 1 << 1;
        }
        word0 |= ((fields.mode as u64) & 0x7) << 9;
        word0 |= (fields.page_table_root_pfn & PFN_MASK_40) << 12;

        let mut word1: u64 = 0;
        if fields.io_read {
            word1 |= 1 << 61;
        }
        if fields.io_write {
            word1 |= 1 << 62;
        }

        let mut word2: u64 = 0;
        if fields.suppress_io_fault {
            word2 |= 1 << 4;
        }

        Self {
            words: [word0, word1, word2, 0],
        }
    }

    /// Decode a 256-bit entry back into its structured fields. The
    /// inverse of [`encode`]: `decode(encode(fields)) == fields` for
    /// every legal input.
    pub fn decode(&self) -> DeviceTableFields {
        let word0 = self.words[0];
        let word1 = self.words[1];
        let word2 = self.words[2];
        DeviceTableFields {
            valid: (word0 & (1 << 0)) != 0,
            translation_valid: (word0 & (1 << 1)) != 0,
            mode: ((word0 >> 9) & 0x7) as u8,
            page_table_root_pfn: (word0 >> 12) & PFN_MASK_40,
            io_read: (word1 & (1 << 61)) != 0,
            io_write: (word1 & (1 << 62)) != 0,
            suppress_io_fault: (word2 & (1 << 4)) != 0,
        }
    }
}

/// 40-bit mask for page-frame numbers. A PFN = physical address >> 12;
/// AMD-Vi tops out at bit 51, leaving 40 bits of PFN.
pub const PFN_MASK_40: u64 = (1 << 40) - 1;

// ---------------------------------------------------------------------------
// Command ring descriptors (AMD IOMMU spec §3.3)
// ---------------------------------------------------------------------------

/// 16-byte command-ring entry. Every AMD-Vi command is 128 bits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CommandEntry {
    pub words: [u64; 2],
}

/// Command opcodes Phase 55a issues. Values come from §3.3.1 "Commands".
pub struct CommandOpcode;

impl CommandOpcode {
    /// COMPLETION_WAIT — used as a barrier after any batch of invalidations.
    pub const COMPLETION_WAIT: u8 = 0x01;
    /// INVALIDATE_DEVTAB_ENTRY — forces the IOMMU to re-read a device-table
    /// entry after software has modified it.
    pub const INVALIDATE_DEVTAB_ENTRY: u8 = 0x02;
    /// INVALIDATE_IOMMU_PAGES — invalidates a range of IOVAs in a domain.
    pub const INVALIDATE_IOMMU_PAGES: u8 = 0x03;
}

impl CommandEntry {
    /// Build a COMPLETION_WAIT command that writes the constant
    /// `completion_marker` to physical address `completion_store_addr`
    /// when the command drains. The poller watches `completion_store_addr`
    /// for the marker as its "commands before this one have completed"
    /// signal.
    pub fn completion_wait(completion_store_addr: u64, completion_marker: u64) -> Self {
        // Word0: opcode in bits 63:60, store bit (s) bit 0, completion
        // bit (i) bit 1, store address bits 51:3 in bits 51:3 of word0.
        let opcode = CommandOpcode::COMPLETION_WAIT as u64;
        let word0 = (opcode << 60)
            | (1 << 0) // Store (s)
            | (completion_store_addr & STORE_ADDR_MASK_W0);
        let word1 = completion_marker;
        Self {
            words: [word0, word1],
        }
    }

    /// Build an INVALIDATE_DEVTAB_ENTRY command targeting `device_id`.
    pub fn invalidate_devtab_entry(device_id: u16) -> Self {
        let opcode = CommandOpcode::INVALIDATE_DEVTAB_ENTRY as u64;
        let word0 = (opcode << 60) | (device_id as u64);
        Self { words: [word0, 0] }
    }

    /// Build an INVALIDATE_IOMMU_PAGES command covering the entire
    /// address space for `domain_id`. Phase 55a uses the "entire domain"
    /// form; partial-range invalidation is deferred.
    pub fn invalidate_iommu_pages_all(domain_id: u16) -> Self {
        let opcode = CommandOpcode::INVALIDATE_IOMMU_PAGES as u64;
        // word0 bits 31:16 = domain_id; word1 bit 0 = Size (S) = 1 for "all";
        // word1 bits 63:12 = address. For "all" with S=1 the address bits
        // are set to the spec's all-ones encoding 0x7FFF_FFFF_FFFF_FFFF >> 0.
        let word0 = (opcode << 60) | ((domain_id as u64) << 32);
        // Address bits all-1 in bits 63:12, plus the S (size) bit 0.
        let word1 = !0xFFFu64 | 0x1;
        Self {
            words: [word0, word1],
        }
    }

    /// Opcode byte extracted from the command header. Used by tests to
    /// verify descriptor construction.
    pub fn opcode(&self) -> u8 {
        ((self.words[0] >> 60) & 0xF) as u8
    }
}

/// Bit 51:3 mask inside word0 for COMPLETION_WAIT store address.
pub const STORE_ADDR_MASK_W0: u64 = ((1u64 << 52) - 1) & !0x7u64;

// ---------------------------------------------------------------------------
// Event-log entry decode (AMD IOMMU spec §3.4)
// ---------------------------------------------------------------------------

/// 16-byte event-log entry. Every AMD-Vi event is 128 bits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EventEntry {
    pub words: [u64; 2],
}

/// Event codes Phase 55a decodes. Values come from §3.4.
pub struct EventCode;

impl EventCode {
    /// ILLEGAL_DEV_TABLE_ENTRY — hardware found an invalid DT entry on a
    /// device request.
    pub const ILLEGAL_DEV_TABLE_ENTRY: u8 = 0x1;
    /// IO_PAGE_FAULT — a translation walk through the DT+page-table hit
    /// a missing or permission-failing entry.
    pub const IO_PAGE_FAULT: u8 = 0x2;
    /// DEV_TAB_HW_ERROR — uncorrectable error reading the device table.
    pub const DEV_TAB_HW_ERROR: u8 = 0x3;
    /// PAGE_TAB_HW_ERROR — uncorrectable error reading the page table.
    pub const PAGE_TAB_HW_ERROR: u8 = 0x4;
}

/// Structured view of an event-log entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DecodedEvent {
    /// 4-bit event-code field.
    pub code: u8,
    /// Requester BDF (word0 bits 15:0 in IO_PAGE_FAULT).
    pub device_id: u16,
    /// Domain id or PASID (word0 bits 47:32 where applicable).
    pub domain_id: u16,
    /// Faulting IOVA for IO_PAGE_FAULT (word1 bits 63:12 << 12) — zero for
    /// events that don't carry an address.
    pub address: u64,
}

impl EventEntry {
    /// Construct an entry directly from its two u64 words (for tests).
    pub fn new(word0: u64, word1: u64) -> Self {
        Self {
            words: [word0, word1],
        }
    }

    /// Decode the entry into a [`DecodedEvent`]. Unknown event codes
    /// return a record with `code` preserved so the caller can log the
    /// unrecognized event without losing information.
    pub fn decode(&self) -> DecodedEvent {
        let word0 = self.words[0];
        let word1 = self.words[1];
        // Event code is in word0 bits 63:60 of the header word. The spec
        // uses word0 bit layout: bits 15:0 = device_id, bits 31:16 = PASID,
        // bits 47:32 = domain_id, bits 59:56 reserved, bits 63:60 = event code.
        let code = ((word0 >> 60) & 0xF) as u8;
        let device_id = (word0 & 0xFFFF) as u16;
        let domain_id = ((word0 >> 32) & 0xFFFF) as u16;
        // Address word: IO_PAGE_FAULT uses all 64 bits; some other events
        // use word1 bits 63:3. Phase 55a callers treat `address` as the
        // byte IOVA for IO_PAGE_FAULT only.
        let address = word1;
        DecodedEvent {
            code,
            device_id,
            domain_id,
            address,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    // ------------------- Device-table entry -------------------

    #[test]
    fn device_table_entry_empty_is_all_zero() {
        let e = DeviceTableEntry::empty();
        assert_eq!(e.words, [0; 4]);
        let f = e.decode();
        assert!(!f.valid);
        assert!(!f.translation_valid);
        assert_eq!(f.page_table_root_pfn, 0);
        assert_eq!(f.mode, 0);
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
            // High bits above bit 40 must be silently dropped.
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
        // IR bit should be set, IW bit clear.
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

    // ------------------- Command entries -------------------

    #[test]
    fn completion_wait_command_carries_opcode_and_marker() {
        let cmd = CommandEntry::completion_wait(0x1234_0000, 0xFEED_FACE_DEAD_BEEF);
        assert_eq!(cmd.opcode(), CommandOpcode::COMPLETION_WAIT);
        // Store bit (s) must be set.
        assert!((cmd.words[0] & 0x1) != 0);
        // Marker word must match.
        assert_eq!(cmd.words[1], 0xFEED_FACE_DEAD_BEEF);
        // Store address must be embedded, lower 3 bits ignored.
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
        // Domain id in word0 bits 47:32.
        assert_eq!((cmd.words[0] >> 32) & 0xFFFF, 0x00AB);
        // S (size) bit 0 of word1 must be set.
        assert!((cmd.words[1] & 0x1) != 0);
    }

    // ------------------- Event decode -------------------

    #[test]
    fn io_page_fault_decode_extracts_device_and_address() {
        // code=2 in bits 63:60, device_id=0x0100 in bits 15:0,
        // domain_id=0x0005 in bits 47:32.
        let word0 = ((EventCode::IO_PAGE_FAULT as u64) << 60) | (0x0005u64 << 32) | 0x0100u64;
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
        // code = 0xF, which Phase 55a does not recognize.
        let word0 = (0xFu64 << 60) | 0x0042u64;
        let event = EventEntry::new(word0, 0);
        let decoded = event.decode();
        assert_eq!(decoded.code, 0xF);
        assert_eq!(decoded.device_id, 0x0042);
    }

    // ------------------- ControlBits -------------------

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
        // Each bit must set exactly one bit.
        for b in bits {
            prop_assert_eq_helper(b.count_ones(), 1);
        }
        // And they must be pairwise distinct.
        for i in 0..bits.len() {
            for j in (i + 1)..bits.len() {
                prop_assert_eq_helper(bits[i] & bits[j], 0);
            }
        }
    }

    fn prop_assert_eq_helper<T: PartialEq + core::fmt::Debug>(a: T, b: T) {
        assert_eq!(a, b);
    }
}
