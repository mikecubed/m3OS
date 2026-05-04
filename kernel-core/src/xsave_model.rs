//! Phase 57e Track J — host-testable model of XSAVE feature parsing.
//!
//! Mirrors the runtime CPUID parsing in `kernel/src/arch/x86_64/cpuid.rs`
//! without depending on the `cpuid` instruction.  Lets host-side unit tests
//! pin the parsing logic against synthetic CPUID register triples.

/// Bit positions in CPUID Leaf 1 ECX.
pub const LEAF1_ECX_XSAVE: u32 = 1 << 26;
pub const LEAF1_ECX_OSXSAVE: u32 = 1 << 27;

/// 1.0 release supported state-component mask (x87 + SSE + AVX).
pub const XSAVE_FEATURE_MASK: u64 = 0x7;

/// Parsed XSAVE feature surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct XSaveFeaturesModel {
    pub supported: bool,
    pub osxsave_capable: bool,
    pub supported_components: u64,
    pub max_area_size: usize,
    pub area_size_at_mask: usize,
    pub xsaveopt: bool,
}

impl XSaveFeaturesModel {
    /// Parse from raw CPUID register triples.  Mirrors the runtime
    /// `XSaveFeatures::from_raw` in the kernel.
    pub fn from_raw(
        leaf1_ecx: u32,
        leaf_d_0_eax: u32,
        leaf_d_0_ebx: u32,
        leaf_d_0_ecx: u32,
        leaf_d_0_edx: u32,
        leaf_d_1_eax: u32,
    ) -> Self {
        Self {
            supported: (leaf1_ecx & LEAF1_ECX_XSAVE) != 0,
            osxsave_capable: (leaf1_ecx & LEAF1_ECX_OSXSAVE) != 0,
            supported_components: (u64::from(leaf_d_0_edx) << 32) | u64::from(leaf_d_0_eax),
            max_area_size: leaf_d_0_ecx as usize,
            area_size_at_mask: leaf_d_0_ebx as usize,
            xsaveopt: (leaf_d_1_eax & 1) != 0,
        }
    }

    /// True when the CPU advertises the 1.0 required mask (x87 + SSE + AVX).
    ///
    /// The boot-time probe only requires the architectural XSAVE bit (bit 26)
    /// — `osxsave_capable` (bit 27) reflects the runtime state of
    /// `CR4.OSXSAVE`, which is 0 before the kernel sets it.  The model
    /// mirrors that contract: meets_minimum is independent of whether the
    /// OS has already enabled XSAVE.
    pub fn meets_minimum(&self) -> bool {
        self.supported && (self.supported_components & XSAVE_FEATURE_MASK) == XSAVE_FEATURE_MASK
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sandy Bridge / Bulldozer baseline: XSAVE + OSXSAVE + AVX, 832-byte area,
    /// no XSAVEOPT.
    #[test]
    fn baseline_sandy_bridge() {
        let f = XSaveFeaturesModel::from_raw(
            LEAF1_ECX_XSAVE | LEAF1_ECX_OSXSAVE,
            0x0000_0007, // EAX low: x87+SSE+AVX
            832,         // EBX: area at current XCR0
            832,         // ECX: max area for all components
            0x0000_0000, // EDX high: none
            0x0000_0000, // Leaf D.1 EAX: no XSAVEOPT
        );
        assert!(f.meets_minimum());
        assert_eq!(f.area_size_at_mask, 832);
        assert!(!f.xsaveopt);
    }

    /// Ivy Bridge / Haswell: XSAVEOPT bit advertised.
    #[test]
    fn xsaveopt_capable() {
        let f = XSaveFeaturesModel::from_raw(
            LEAF1_ECX_XSAVE | LEAF1_ECX_OSXSAVE,
            0x0000_0007,
            832,
            832,
            0,
            0x0000_0001,
        );
        assert!(f.meets_minimum());
        assert!(f.xsaveopt);
    }

    /// Pre-Sandy Bridge: XSAVE bit absent.
    #[test]
    fn pre_sandy_bridge_rejected() {
        let f = XSaveFeaturesModel::from_raw(0, 0, 0, 0, 0, 0);
        assert!(!f.meets_minimum());
    }

    /// CPU advertises XSAVE+AVX but OSXSAVE not yet enabled by the OS — this
    /// is the state the kernel observes at boot before
    /// `enable_xsave_state` runs.  Minimum is met (we only require the
    /// architectural XSAVE bit at probe time).
    #[test]
    fn xsave_supported_but_osxsave_not_yet_enabled() {
        let f = XSaveFeaturesModel::from_raw(
            LEAF1_ECX_XSAVE, // OSXSAVE bit 27 NOT set
            0x0000_0007,
            832,
            832,
            0,
            0,
        );
        assert!(f.meets_minimum());
        assert!(!f.osxsave_capable);
        assert!(f.supported);
    }

    /// XSAVE+OSXSAVE present but AVX not advertised — m3OS 1.0 requires AVX.
    #[test]
    fn xsave_without_avx_rejected() {
        let f = XSaveFeaturesModel::from_raw(
            LEAF1_ECX_XSAVE | LEAF1_ECX_OSXSAVE,
            0x0000_0003, // x87+SSE only
            512,
            512,
            0,
            0,
        );
        assert!(!f.meets_minimum());
    }

    /// AVX-512 capable: state mask sets bits 5-7.  Still meets minimum (we
    /// only require the low 3 bits); area size grows but our static
    /// `XSAVE_AREA_SIZE` (832) is too small — the boot-time assertion in
    /// `kernel/src/main.rs` would catch this and panic.
    #[test]
    fn avx512_advertised_but_not_enabled() {
        let f = XSaveFeaturesModel::from_raw(
            LEAF1_ECX_XSAVE | LEAF1_ECX_OSXSAVE,
            0x0000_00E7, // bits 0-2 (x87+SSE+AVX) + bits 5-7 (AVX-512)
            832,         // current XCR0 still at 0x7 — area_size_at_mask matches
            2688,        // max area covering all advertised components
            0,
            1,
        );
        assert!(f.meets_minimum());
        assert_eq!(f.area_size_at_mask, 832);
        assert_eq!(f.max_area_size, 2688);
    }
}
