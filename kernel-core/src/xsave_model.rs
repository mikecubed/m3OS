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

// ── Phase 90a B.1: Memory Protection Keys (PKU) CPUID surface ───────────────

/// `CPUID.07H.0:ECX[3]` — PKU: the CPU implements protection keys for
/// user-mode pages (the `RDPKRU`/`WRPKRU` instructions and the PTE key field).
pub const LEAF7_ECX_PKU: u32 = 1 << 3;
/// `CPUID.07H.0:ECX[4]` — OSPKE: the OS has enabled protection keys via
/// `CR4.PKE`.  Like `OSXSAVE`, this reflects *runtime* state — it reads 0
/// before the kernel sets `CR4.PKE` and 1 after — so the support probe must
/// **not** require it, only the architectural PKU bit.
pub const LEAF7_ECX_OSPKE: u32 = 1 << 4;

/// XSAVE state-component index for PKRU (the protection-key rights register).
/// Bit 9 in XCR0 / the supported-component bitmap; CPUID.0Dh sub-leaf 9
/// reports its size and offset.
pub const XSAVE_COMPONENT_PKRU: u32 = 9;
/// XCR0 bit mask for the PKRU component (component 9).
pub const XCR0_PKRU: u64 = 1 << XSAVE_COMPONENT_PKRU;

/// Parsed Memory-Protection-Keys (PKU) CPUID surface.
///
/// Mirrors the runtime `CPUID.07H.0:ECX` (PKU/OSPKE) and the `CPUID.0Dh`
/// component-9 (PKRU) parsing in `kernel/src/arch/x86_64/cpuid.rs` without
/// executing `cpuid`.  Lets host tests pin the decode and the XSAVE-area size
/// accounting against synthetic register values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PkuFeaturesModel {
    /// `CPUID.07H.0:ECX[3]` — PKU implemented by the CPU.
    pub pku: bool,
    /// `CPUID.07H.0:ECX[4]` — OSPKE: `CR4.PKE` currently set (runtime state,
    /// 0 before the kernel enables it).  Retained for diagnostics; the support
    /// decision never depends on it.
    pub ospke: bool,
    /// Whether the PKRU component (bit 9) appears in the XSAVE supported-component
    /// bitmap (`CPUID.0Dh.0:EDX:EAX`).  PKU without the XSAVE component is a
    /// degenerate CPU we do not enable component 9 on.
    pub pkru_component_supported: bool,
    /// `CPUID.0Dh.9:EAX` — size in bytes of the PKRU state component.
    pub pkru_component_size: usize,
    /// `CPUID.0Dh.9:EBX` — byte offset of the PKRU component within the
    /// (non-compacted) XSAVE area.
    pub pkru_component_offset: usize,
}

impl PkuFeaturesModel {
    /// Parse from raw CPUID register values.
    ///
    /// * `leaf7_0_ecx` — `CPUID.07H` sub-leaf 0, ECX (PKU bit 3 / OSPKE bit 4).
    /// * `leaf_d_0_eax` / `leaf_d_0_edx` — `CPUID.0Dh` sub-leaf 0, the low/high
    ///   halves of the supported-component bitmap (component 9 = bit 9 of EAX).
    /// * `leaf_d_9_eax` / `leaf_d_9_ebx` — `CPUID.0Dh` sub-leaf 9, the PKRU
    ///   component's size (EAX) and offset (EBX).
    pub fn from_raw(
        leaf7_0_ecx: u32,
        leaf_d_0_eax: u32,
        leaf_d_0_edx: u32,
        leaf_d_9_eax: u32,
        leaf_d_9_ebx: u32,
    ) -> Self {
        let supported_components = (u64::from(leaf_d_0_edx) << 32) | u64::from(leaf_d_0_eax);
        Self {
            pku: (leaf7_0_ecx & LEAF7_ECX_PKU) != 0,
            ospke: (leaf7_0_ecx & LEAF7_ECX_OSPKE) != 0,
            pkru_component_supported: (supported_components & XCR0_PKRU) != 0,
            pkru_component_size: leaf_d_9_eax as usize,
            pkru_component_offset: leaf_d_9_ebx as usize,
        }
    }

    /// True when the kernel may enable PKU on this CPU: the architectural PKU
    /// bit is set **and** the XSAVE PKRU component (9) is advertised (so PKRU
    /// can ride the per-task XSAVE save/restore).  Independent of OSPKE — that
    /// is runtime state the kernel itself toggles via `CR4.PKE`.
    pub fn pku_usable(&self) -> bool {
        self.pku && self.pkru_component_supported
    }

    /// The XCR0 mask the kernel should program: the existing base mask plus the
    /// PKRU component bit **only** when PKU is usable.  When PKU is not usable
    /// the mask is returned unchanged (component 9 stays clear), keeping no-PKU
    /// behaviour bit-for-bit identical.
    pub fn xcr0_mask(&self, base_mask: u64) -> u64 {
        if self.pku_usable() {
            base_mask | XCR0_PKRU
        } else {
            base_mask
        }
    }

    /// Validate that a statically-sized per-task XSAVE buffer (`static_area`)
    /// still holds the *enabled* XSAVE area once component 9 has been added.
    ///
    /// `enabled_area` is the size CPUID.0Dh.0:EBX reports **after** XCR0 has
    /// been programmed with the PKU mask (it grows to include component 9).
    /// Returns `true` when the static buffer fits.  The boot-time assertion
    /// uses this so a future component-size change that no longer fits fails
    /// loudly rather than corrupting the next task's state.  When PKU is not
    /// usable the area does not grow, so this reduces to the existing 57e check.
    pub fn static_area_fits(static_area: usize, enabled_area: usize) -> bool {
        static_area >= enabled_area
    }
}

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

    // ── Phase 90a B.1: PKU / PKRU decode + size accounting ──────────────────

    /// Ryzen 5 7600 (Zen 4) / Skylake-X class: PKU + OSPKE not yet enabled, the
    /// PKRU component (9) advertised at 8 bytes, offset 2688 (after the AVX-512
    /// region in a non-compacted area).  Usable; XCR0 grows by bit 9.
    #[test]
    fn pku_present_component_advertised() {
        let p = PkuFeaturesModel::from_raw(
            LEAF7_ECX_PKU, // PKU set, OSPKE not yet (CR4.PKE unset at probe)
            1 << 9,        // leaf D.0 EAX: component 9 bit present
            0,             // leaf D.0 EDX: no high components
            8,             // leaf D.9 EAX: PKRU size = 8 bytes
            2688,          // leaf D.9 EBX: offset
        );
        assert!(p.pku);
        assert!(!p.ospke);
        assert!(p.pkru_component_supported);
        assert_eq!(p.pkru_component_size, 8);
        assert_eq!(p.pkru_component_offset, 2688);
        assert!(p.pku_usable());
        assert_eq!(
            p.xcr0_mask(XSAVE_FEATURE_MASK),
            XSAVE_FEATURE_MASK | XCR0_PKRU
        );
    }

    /// OSPKE already set (CR4.PKE enabled) — reflects the post-`enable` re-read.
    /// The usability decision is unaffected by OSPKE.
    #[test]
    fn ospke_set_does_not_change_usability() {
        let p = PkuFeaturesModel::from_raw(LEAF7_ECX_PKU | LEAF7_ECX_OSPKE, 1 << 9, 0, 8, 2688);
        assert!(p.pku);
        assert!(p.ospke);
        assert!(p.pku_usable());
    }

    /// No-PKU CPU (QEMU TCG without the feature): PKU bit clear → not usable,
    /// XCR0 mask returned unchanged (component 9 stays off — bit-for-bit no-op).
    #[test]
    fn no_pku_cpu_unchanged() {
        let p = PkuFeaturesModel::from_raw(0, 0, 0, 0, 0);
        assert!(!p.pku);
        assert!(!p.pku_usable());
        assert_eq!(p.xcr0_mask(XSAVE_FEATURE_MASK), XSAVE_FEATURE_MASK);
    }

    /// PKU advertised but the XSAVE PKRU component absent — a degenerate CPU.
    /// We refuse to enable component 9 (it could not be saved/restored), so
    /// `pku_usable` is false and the XCR0 mask is unchanged.
    #[test]
    fn pku_without_xsave_component_not_usable() {
        let p = PkuFeaturesModel::from_raw(LEAF7_ECX_PKU, 0x0000_0007, 0, 0, 0);
        assert!(p.pku);
        assert!(!p.pkru_component_supported);
        assert!(!p.pku_usable());
        assert_eq!(p.xcr0_mask(XSAVE_FEATURE_MASK), XSAVE_FEATURE_MASK);
    }

    /// The static XSAVE buffer must hold the grown (component-9-inclusive) area.
    /// 832 (x87+SSE+AVX) is too small for a PKU-grown 2696-byte area — the
    /// boot-time assertion would fire.  The Zen-4 reality (AVX-512 + PKRU at
    /// 2696) needs a larger static buffer; the host check pins the inequality.
    #[test]
    fn static_area_fit_check() {
        // PKU off / area unchanged: 832 fits 832.
        assert!(PkuFeaturesModel::static_area_fits(832, 832));
        // PKU grew the area past the legacy static size → does NOT fit.
        assert!(!PkuFeaturesModel::static_area_fits(832, 2696));
        // A sufficiently large static buffer fits the grown area.
        assert!(PkuFeaturesModel::static_area_fits(2696, 2696));
    }
}
