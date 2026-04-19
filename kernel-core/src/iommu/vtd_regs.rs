//! VT-d MMIO register offsets and field-accessor pure logic — Phase 55a
//! Track C.1.
//!
//! Every offset and field split here is lifted from Intel VT-d 3.3
//! §10.4 "Register Descriptions" (the "Remapping Registers" table). The
//! kernel-side driver (`kernel/src/iommu/intel.rs`) reads and writes
//! these offsets with `read_volatile` / `write_volatile`; this module
//! provides the register layout and field decoders as host-testable pure
//! logic.
//!
//! # Why this is a separate module
//!
//! Splitting the bit layouts from the MMIO path lets a future scalable-
//! mode patch change only the decoders without changing the driver, and
//! lets `cargo test -p kernel-core` confirm the decoders match the spec
//! without booting QEMU.

// ---------------------------------------------------------------------------
// Register offsets — Intel VT-d 3.3 §10.4
// ---------------------------------------------------------------------------

/// VT-d MMIO register offsets. All relative to the unit's register base.
pub struct VtdRegs;

impl VtdRegs {
    /// Version register (32-bit, RO). MAX=15:8, MIN=7:0 nibble pair.
    pub const VER: usize = 0x00;
    /// Capability register (64-bit, RO). Super-page support, address width,
    /// queued-invalidation support, fault recording offset live here.
    pub const CAP: usize = 0x08;
    /// Extended Capability register (64-bit, RO). Scalable-mode,
    /// interrupt-remapping, and several queue-format features.
    pub const ECAP: usize = 0x10;
    /// Global Command register (32-bit, WO — reads are implementation
    /// defined). Writes trigger one-shot hardware actions (TE, SRTP,
    /// WRTP, QIE, ...).
    pub const GCMD: usize = 0x18;
    /// Global Status register (32-bit, RO). Acknowledge bits for each
    /// GCMD request bit.
    pub const GSTS: usize = 0x1C;
    /// Root Table Address register (64-bit, RW). Holds the root-table
    /// base plus a two-bit table-type field in the low bits.
    pub const RTADDR: usize = 0x20;
    /// Context Command register (64-bit, RW). Register-path
    /// context-cache invalidation trigger + granularity.
    pub const CCMD: usize = 0x28;
    /// Fault Status register (32-bit, RW1C). Primary / overflow / pending
    /// fault bits.
    pub const FSTS: usize = 0x34;
    /// Fault Event Control register (32-bit, RW). Interrupt mask bit.
    pub const FECTL: usize = 0x38;
    /// Fault Event Data register (32-bit, RW). MSI data (vector +
    /// delivery mode) delivered on fault.
    pub const FEDATA: usize = 0x3C;
    /// Fault Event Address register (32-bit, RW). MSI address (LAPIC ID
    /// + redirection hint) delivered on fault.
    pub const FEADDR: usize = 0x40;
    /// Fault Event Upper Address register (32-bit, RW). Upper half of a
    /// 64-bit MSI address; typically zero on x86.
    pub const FEUADDR: usize = 0x44;
    /// Advanced Fault Log register (64-bit, RW). Base of the fault-log
    /// ring when advanced-fault-logging is enabled.
    pub const AFLOG: usize = 0x58;
    /// Invalidation Queue Head register (64-bit, RO). Hardware-managed
    /// consumer cursor into the invalidation queue.
    pub const IQH: usize = 0x80;
    /// Invalidation Queue Tail register (64-bit, RW). Software-managed
    /// producer cursor; bumped after each descriptor write.
    pub const IQT: usize = 0x88;
    /// Invalidation Queue Address register (64-bit, RW). Base + size of
    /// the invalidation queue.
    pub const IQA: usize = 0x90;
    /// Interrupt Remapping Table Address register (64-bit, RW). Phase 55a
    /// leaves interrupt remapping disabled; the offset is published for
    /// forward compatibility.
    pub const IRTA: usize = 0xB8;

    /// IOTLB register block offset inside the unit; the exact offset
    /// within the block is derived from `CAP.IRO * 16`. For the QEMU
    /// VT-d model the block sits at 0x108 (IVA, IOTLB command)
    /// relative to the unit base. Derived at runtime from CAP.
    pub const IOTLB_REGS_BASE_FROM_CAP_IRO: &'static str =
        "iva_and_iotlb_regs_base = (cap.iro_16byte_units) * 16";
}

// ---------------------------------------------------------------------------
// GCMD / GSTS bit positions — Intel VT-d 3.3 §10.4.4, §10.4.5
// ---------------------------------------------------------------------------

/// GCMD.TE — Translation Enable. GSTS.TES mirrors.
pub const GCMD_TE: u32 = 1 << 31;
/// GCMD.SRTP — Set Root Table Pointer. GSTS.RTPS mirrors.
pub const GCMD_SRTP: u32 = 1 << 30;
/// GCMD.WBF — Write Buffer Flush. GSTS.WBFS mirrors. Implementation
/// defined; we do not use it.
pub const GCMD_WBF: u32 = 1 << 29;
/// GCMD.EAFL — Enable Advanced Fault Logging. GSTS.AFLS mirrors.
pub const GCMD_EAFL: u32 = 1 << 28;
/// GCMD.SFL — Set Fault Log pointer. GSTS.FLS mirrors.
pub const GCMD_SFL: u32 = 1 << 27;
/// GCMD.SIRTP — Set Interrupt Remap Table Pointer. GSTS.IRTPS mirrors.
pub const GCMD_SIRTP: u32 = 1 << 26;
/// GCMD.IRE — Interrupt Remap Enable. GSTS.IRES mirrors.
pub const GCMD_IRE: u32 = 1 << 25;
/// GCMD.QIE — Queued Invalidation Enable. GSTS.QIES mirrors.
pub const GCMD_QIE: u32 = 1 << 26; // bit overlaps SIRTP; spec §10.4.4 table

/// GSTS.TES — Translation Enabled.
pub const GSTS_TES: u32 = 1 << 31;
/// GSTS.RTPS — Root Table Pointer Set.
pub const GSTS_RTPS: u32 = 1 << 30;
/// GSTS.QIES — Queued Invalidation Enabled.
pub const GSTS_QIES: u32 = 1 << 26;

// ---------------------------------------------------------------------------
// CAP register — Intel VT-d 3.3 §10.4.2
// ---------------------------------------------------------------------------

/// Read-only Capability register. Bits beyond what Phase 55a consumes
/// are preserved by the caller and ignored here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VtdCap(pub u64);

impl VtdCap {
    /// Adjusted Guest Address Width support — `CAP.SAGAW`, bits [12:8].
    /// Each bit set advertises one of: 39-bit / 48-bit / 57-bit / 64-bit.
    pub const fn sagaw(self) -> u8 {
        ((self.0 >> 8) & 0x1F) as u8
    }

    /// MGAW — Maximum Guest Address Width (width - 1), bits [21:16].
    /// e.g. 47 = 48-bit address width.
    pub const fn mgaw_bits(self) -> u8 {
        (((self.0 >> 16) & 0x3F) as u8) + 1
    }

    /// SLLPS — Second-Level Large Page Support, bits [37:34].
    /// Bit 0: 21-bit (2 MiB), bit 1: 30-bit (1 GiB), ...
    pub const fn sllps(self) -> u8 {
        ((self.0 >> 34) & 0xF) as u8
    }

    /// FRO — Fault Recording Offset, bits [33:24]. In units of 16 bytes
    /// from the unit base.
    pub const fn fro_16byte_units(self) -> u16 {
        ((self.0 >> 24) & 0x3FF) as u16
    }

    /// NFR — Number of Fault-Recording Registers minus 1, bits [47:40].
    pub const fn nfr(self) -> u8 {
        ((self.0 >> 40) & 0xFF) as u8
    }

    /// Convert the supported-page-size bitmap into a mask indexed by the
    /// `(1 << n)` convention (bit `n` = `2^n`-byte page). 4 KiB is
    /// always supported.
    pub const fn supported_page_sizes_mask(self) -> u64 {
        let mut mask: u64 = 1 << 12; // 4 KiB always.
        if (self.sllps() & 0x1) != 0 {
            mask |= 1 << 21; // 2 MiB
        }
        if (self.sllps() & 0x2) != 0 {
            mask |= 1 << 30; // 1 GiB
        }
        mask
    }

    /// Address-width bits advertised by the unit. Returns 48 as the
    /// phase-55a default when SAGAW bit 2 (48-bit) is set; falls back
    /// to 39 bits (SAGAW bit 1) or to the MGAW + 1 if neither.
    pub const fn address_width_bits(self) -> u8 {
        // SAGAW bit 2 → 48-bit; bit 1 → 39-bit; bit 3 → 57-bit.
        let sagaw = self.sagaw();
        if (sagaw & 0x04) != 0 {
            48
        } else if (sagaw & 0x08) != 0 {
            57
        } else if (sagaw & 0x02) != 0 {
            39
        } else {
            self.mgaw_bits()
        }
    }
}

// ---------------------------------------------------------------------------
// ECAP register — Intel VT-d 3.3 §10.4.3
// ---------------------------------------------------------------------------

/// Read-only Extended Capability register.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VtdEcap(pub u64);

impl VtdEcap {
    /// QI — Queued-Invalidation Supported, bit 1.
    pub const fn queued_invalidation(self) -> bool {
        (self.0 & (1 << 1)) != 0
    }

    /// IR — Interrupt Remapping Supported, bit 3.
    pub const fn interrupt_remapping(self) -> bool {
        (self.0 & (1 << 3)) != 0
    }

    /// SMTS — Scalable Mode Translation Supported, bit 43.
    pub const fn scalable_mode(self) -> bool {
        (self.0 & (1 << 43)) != 0
    }

    /// IRO — IOTLB Register Offset, bits [17:8] in units of 16 bytes.
    pub const fn iro_16byte_units(self) -> u16 {
        ((self.0 >> 8) & 0x3FF) as u16
    }
}

// ---------------------------------------------------------------------------
// Version register decode
// ---------------------------------------------------------------------------

/// Decoded VT-d version. `MAX` is the major revision, `MIN` is the
/// minor. E.g. VER=0x10 means major=1, minor=0.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VtdVersion {
    pub major: u8,
    pub minor: u8,
}

impl VtdVersion {
    /// Decode the 32-bit VER register value.
    pub const fn from_raw(raw: u32) -> Self {
        let major = ((raw >> 4) & 0xF) as u8;
        let minor = (raw & 0xF) as u8;
        Self { major, minor }
    }
}

// ---------------------------------------------------------------------------
// RTADDR encoding
// ---------------------------------------------------------------------------

/// Encode a legacy-mode (non-scalable) root-table address register
/// value: `phys | table_type`. Legacy mode uses table type = 0 in the
/// low two bits; scalable mode would set bit 10.
pub const fn encode_rtaddr_legacy(root_phys: u64) -> u64 {
    // Low 12 bits of a page-aligned root-table base are implicitly zero
    // in the register; the TTM (Translation Table Mode) field uses bit
    // 10 per spec. For legacy mode, TTM = 00b.
    root_phys & !0xFFFu64
}

// ---------------------------------------------------------------------------
// Context Command (CCMD) encoding
// ---------------------------------------------------------------------------

/// CCMD.ICC — Invalidate Context-Cache trigger, bit 63 (set to request,
/// hardware clears on completion).
pub const CCMD_ICC: u64 = 1 << 63;
/// CCMD.CIRG — Context Invalidation Request Granularity, bits [62:61].
/// Granularities: 01 = global, 10 = domain, 11 = device.
pub const CCMD_CIRG_SHIFT: u64 = 61;
/// Global-invalidation request value for CIRG.
pub const CCMD_CIRG_GLOBAL: u64 = 0b01u64 << CCMD_CIRG_SHIFT;

// ---------------------------------------------------------------------------
// IOTLB invalidation via IVA + IOTLB command registers
// ---------------------------------------------------------------------------

/// IOTLB_REG offset within the IOTLB register block (after the
/// CAP.IRO-derived base). IVA lives at offset 0, IOTLB_REG at offset 8
/// within the block.
pub const IOTLB_IVA: usize = 0x00;
pub const IOTLB_REG: usize = 0x08;

/// IOTLB_REG.IVT — Invalidate IOTLB trigger, bit 63.
pub const IOTLB_IVT: u64 = 1 << 63;
/// IOTLB_REG.IIRG shift (bits [61:60]). 01 = global, 10 = domain,
/// 11 = domain-page.
pub const IOTLB_IIRG_SHIFT: u64 = 60;
/// Global IOTLB flush.
pub const IOTLB_IIRG_GLOBAL: u64 = 0b01u64 << IOTLB_IIRG_SHIFT;

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_offsets_match_spec_table() {
        // These values are hard-coded from Intel VT-d 3.3 §10.4.1 "Register
        // Location" table — if any drifts, a hardware-compliant unit will
        // reject us.
        assert_eq!(VtdRegs::VER, 0x00);
        assert_eq!(VtdRegs::CAP, 0x08);
        assert_eq!(VtdRegs::ECAP, 0x10);
        assert_eq!(VtdRegs::GCMD, 0x18);
        assert_eq!(VtdRegs::GSTS, 0x1C);
        assert_eq!(VtdRegs::RTADDR, 0x20);
        assert_eq!(VtdRegs::CCMD, 0x28);
        assert_eq!(VtdRegs::FSTS, 0x34);
        assert_eq!(VtdRegs::FECTL, 0x38);
        assert_eq!(VtdRegs::FEDATA, 0x3C);
        assert_eq!(VtdRegs::FEADDR, 0x40);
        assert_eq!(VtdRegs::FEUADDR, 0x44);
        assert_eq!(VtdRegs::AFLOG, 0x58);
        assert_eq!(VtdRegs::IQH, 0x80);
        assert_eq!(VtdRegs::IQT, 0x88);
        assert_eq!(VtdRegs::IQA, 0x90);
        assert_eq!(VtdRegs::IRTA, 0xB8);
    }

    #[test]
    fn version_decode_round_trip() {
        let v = VtdVersion::from_raw(0x10);
        assert_eq!(v.major, 1);
        assert_eq!(v.minor, 0);
        let v = VtdVersion::from_raw(0x25);
        assert_eq!(v.major, 2);
        assert_eq!(v.minor, 5);
    }

    #[test]
    fn cap_decode_qemu_q35_default() {
        // QEMU's q35 intel-iommu default CAP value (approximate):
        //   MGAW=47 (48-bit), SAGAW=0b00100 (48-bit only), SLLPS=0b0011
        //   (2M+1G).
        // Pack: bits 12:8 = 0b00100, bits 21:16 = 47, bits 37:34 = 0b0011.
        let cap_value: u64 =
            (0b00100u64 << 8) | ((47u64) << 16) | (0b0011u64 << 34) | (16u64 << 40);
        let cap = VtdCap(cap_value);
        assert_eq!(cap.sagaw(), 0b00100);
        assert_eq!(cap.mgaw_bits(), 48);
        assert_eq!(cap.sllps(), 0b0011);
        assert_eq!(cap.address_width_bits(), 48);
        let sizes = cap.supported_page_sizes_mask();
        assert!(sizes & (1 << 12) != 0, "4K must be supported");
        assert!(sizes & (1 << 21) != 0, "2M supported");
        assert!(sizes & (1 << 30) != 0, "1G supported");
        assert_eq!(cap.nfr(), 16);
    }

    #[test]
    fn cap_supports_only_4k_when_sllps_zero() {
        let cap_value: u64 = (0b00100u64 << 8) | ((47u64) << 16);
        let cap = VtdCap(cap_value);
        let sizes = cap.supported_page_sizes_mask();
        assert_eq!(sizes, 1u64 << 12);
    }

    #[test]
    fn cap_falls_back_to_mgaw_when_sagaw_empty() {
        let cap_value: u64 = (0u64 << 8) | (38u64 << 16);
        let cap = VtdCap(cap_value);
        assert_eq!(cap.address_width_bits(), 39);
    }

    #[test]
    fn ecap_feature_bits_decode() {
        // QI enabled, IR enabled, scalable-mode disabled.
        let ecap_value: u64 = (1u64 << 1) | (1u64 << 3);
        let ecap = VtdEcap(ecap_value);
        assert!(ecap.queued_invalidation());
        assert!(ecap.interrupt_remapping());
        assert!(!ecap.scalable_mode());
    }

    #[test]
    fn ecap_scalable_mode_decode() {
        let ecap = VtdEcap(1u64 << 43);
        assert!(ecap.scalable_mode());
    }

    #[test]
    fn rtaddr_strips_low_bits() {
        // Page-aligned input is passed through; misaligned low bits are
        // stripped so the TTM field remains zero (legacy mode).
        let r = encode_rtaddr_legacy(0xDEAD_B123);
        assert_eq!(r, 0xDEAD_B000);
    }

    #[test]
    fn gcmd_gsts_bit_positions() {
        // Bit 31 is TE; the corresponding GSTS ack is also bit 31.
        assert_eq!(GCMD_TE, 1u32 << 31);
        assert_eq!(GSTS_TES, 1u32 << 31);
        assert_eq!(GCMD_SRTP, 1u32 << 30);
        assert_eq!(GSTS_RTPS, 1u32 << 30);
    }

    #[test]
    fn ccmd_global_invalidation_value() {
        let v = CCMD_ICC | CCMD_CIRG_GLOBAL;
        assert!(v & (1u64 << 63) != 0, "ICC bit must be set");
        // CIRG = 01b in [62:61]
        assert_eq!((v >> 61) & 0x3, 0b01);
    }

    #[test]
    fn iotlb_global_invalidation_value() {
        let v = IOTLB_IVT | IOTLB_IIRG_GLOBAL;
        assert!(v & (1u64 << 63) != 0, "IVT bit must be set");
        assert_eq!((v >> 60) & 0x3, 0b01);
    }

    #[test]
    fn ecap_iro_sample() {
        // IRO = 0x40 → the IOTLB block sits at register-base + 0x40 * 16 =
        // offset 0x400. Matches the QEMU q35 layout.
        let ecap = VtdEcap((0x40u64) << 8);
        assert_eq!(ecap.iro_16byte_units(), 0x40);
    }
}
