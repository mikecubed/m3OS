//! Phase 84 Tracks C.1 & D.1 — host-testable Spectre/KPTI mitigation decode.
//!
//! Mirrors CPUID leaf 7 EDX and `IA32_ARCH_CAPABILITIES` MSR parsing without
//! executing any privileged instructions.  Lets host-side unit tests pin the
//! parsing and policy logic against synthetic register values.
//!
//! This module is `no_std`-clean: it uses only `core`, fixed-size arrays, and
//! `&'static str`.  No `alloc` dependency.

// ── C.1: SpecCtrlFeatures ────────────────────────────────────────────────────

/// Parsed speculative-execution control surface from CPUID 07H.0 EDX and
/// `IA32_ARCH_CAPABILITIES` MSR.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpecCtrlFeatures {
    /// CPUID.07H.0:EDX[26] — enumerates both IBRS and IBPB support.
    pub ibrs_ibpb: bool,
    /// CPUID.07H.0:EDX[27] — Single Thread Indirect Branch Predictors.
    pub stibp: bool,
    /// CPUID.07H.0:EDX[31] — Speculative Store Bypass Disable.
    pub ssbd: bool,
    /// CPUID.07H.0:EDX[29] — `IA32_ARCH_CAPABILITIES` MSR is present.
    pub arch_caps_present: bool,
    /// `IA32_ARCH_CAPABILITIES`[0] — CPU is not susceptible to Meltdown/RDCL.
    /// Only valid when `arch_caps_present` is true.
    pub rdcl_no: bool,
    /// `IA32_ARCH_CAPABILITIES`[1] (IBRS_ALL) — Enhanced IBRS: set-once-at-boot
    /// mode, no per-entry toggle needed.  Only valid when `arch_caps_present`.
    pub eibrs: bool,
}

impl SpecCtrlFeatures {
    /// Decode from raw CPUID leaf 7, sub-leaf 0, EDX and the optional
    /// `IA32_ARCH_CAPABILITIES` MSR value.
    ///
    /// `arch_caps` is consulted **only** when `EDX[29]` (arch_caps_present) is
    /// set.  If the bit is clear, `rdcl_no` and `eibrs` are forced to `false`
    /// regardless of the `arch_caps` argument — executing the MSR read on CPUs
    /// that do not advertise it is undefined behaviour, so the caller must not
    /// pass meaningful data in that case.
    pub fn from_cpuid(leaf7_edx: u32, arch_caps: u64) -> SpecCtrlFeatures {
        let arch_caps_present = (leaf7_edx & (1 << 29)) != 0;
        SpecCtrlFeatures {
            ibrs_ibpb: (leaf7_edx & (1 << 26)) != 0,
            stibp: (leaf7_edx & (1 << 27)) != 0,
            ssbd: (leaf7_edx & (1 << 31)) != 0,
            arch_caps_present,
            // Only decode arch_caps bits when the MSR is advertised.
            rdcl_no: arch_caps_present && (arch_caps & (1 << 0)) != 0,
            eibrs: arch_caps_present && (arch_caps & (1 << 1)) != 0,
        }
    }

    /// Max-basic-leaf-guarded entry point.
    ///
    /// Executing `CPUID` with a leaf number that exceeds `max_basic_leaf`
    /// returns the data for `max_basic_leaf` itself, not zeroes.  On a CPU
    /// whose highest supported basic leaf is 6 (e.g. some early Core 2
    /// processors), bits 26, 27, 29, 31 of the returned EDX are leaf-6 data
    /// and have no relationship to IBRS/STIBP/ARCH_CAPS/SSBD.  Treating leaf-7
    /// as absent (zeroing EDX) when `max_basic_leaf < 7` prevents spurious
    /// "mitigation present" conclusions on such CPUs.
    pub fn from_cpuid_guarded(
        max_basic_leaf: u32,
        leaf7_edx: u32,
        arch_caps: u64,
    ) -> SpecCtrlFeatures {
        let effective_edx = if max_basic_leaf >= 7 { leaf7_edx } else { 0 };
        Self::from_cpuid(effective_edx, arch_caps)
    }
}

/// IBRS operating mode classification.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IbrsMode {
    /// CPU does not enumerate IBRS.
    None,
    /// Legacy IBRS: must be toggled on every kernel-entry/exit path.
    Legacy,
    /// Enhanced IBRS (IBRS_ALL in `IA32_ARCH_CAPABILITIES`): set once at boot,
    /// protects unconditionally without per-entry overhead.
    Enhanced,
}

/// Classify the IBRS mode from a decoded `SpecCtrlFeatures`.
pub fn classify_ibrs(features: &SpecCtrlFeatures) -> IbrsMode {
    if features.eibrs {
        IbrsMode::Enhanced
    } else if features.ibrs_ibpb {
        IbrsMode::Legacy
    } else {
        IbrsMode::None
    }
}

// ── D.1: mitigations= parser, vuln map, status vocabulary ───────────────────

/// Kernel command-line `mitigations=` level.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MitigationLevel {
    /// `mitigations=off` — all mitigations disabled.
    Off,
    /// `mitigations=auto` (default) — enable only what is needed for the
    /// hardware profile.
    Auto,
    /// `mitigations=full` — enable every mitigation regardless of CPU flags.
    Full,
}

/// Parse the `mitigations=` command-line value.
///
/// Accepts exactly `"off"`, `"auto"`, or `"full"` (lowercase, no trim).
/// Any other value returns `Auto` — see `mitigations_recognized` for a
/// separate predicate that distinguishes "known" from "unknown".
pub fn parse_mitigations(s: &str) -> MitigationLevel {
    match s {
        "off" => MitigationLevel::Off,
        "auto" => MitigationLevel::Auto,
        "full" => MitigationLevel::Full,
        _ => MitigationLevel::Auto,
    }
}

/// Returns `true` only for the three recognised values (`"off"`, `"auto"`,
/// `"full"`).  Use alongside `parse_mitigations` to detect and flag unknown
/// values in the boot log.
pub fn mitigations_recognized(s: &str) -> bool {
    matches!(s, "off" | "auto" | "full")
}

/// Enumeration of tracked CPU vulnerabilities.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Vuln {
    Meltdown,
    SpectreV1,
    SpectreV2,
    Mds,
    L1tf,
    Ssb,
    Retbleed,
    Downfall,
}

impl Vuln {
    /// Human-readable label for boot-log and `/proc/cpuinfo`-style output.
    pub fn name(&self) -> &'static str {
        match self {
            Vuln::Meltdown => "Meltdown",
            Vuln::SpectreV1 => "Spectre-v1",
            Vuln::SpectreV2 => "Spectre-v2",
            Vuln::Mds => "MDS",
            Vuln::L1tf => "L1TF",
            Vuln::Ssb => "SSB (Spectre-v4)",
            Vuln::Retbleed => "Retbleed",
            Vuln::Downfall => "Downfall/GDS",
        }
    }
}

/// Per-vulnerability mitigation status.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// CPU hardware is not susceptible.
    NotAffected,
    /// Mitigation available but disabled (e.g. `mitigations=off`).
    Vulnerable,
    /// Mitigation active; the static string names the mechanism(s).
    Mitigated(&'static str),
    /// Vulnerability is known but m3OS has not yet implemented a mitigation.
    Unaddressed,
}

/// Number of entries in the vulnerability map.
pub const VULNS: usize = 8;

/// Build a complete vulnerability-to-status map for the running CPU and the
/// requested mitigation level.
///
/// **Addressed vulns** (m3OS implements mitigations):
/// - `Meltdown`: KPTI (Page Table Isolation).
/// - `SpectreV2`: Retpoline + IBPB (compiled-in retpoline is always present).
///
/// **Unaddressed vulns** (no implementation yet): `SpectreV1`, `Mds`, `L1tf`,
/// `Ssb`, `Retbleed`, `Downfall`.  These are always reported as
/// `Status::Unaddressed` regardless of `level` — silent omission of a
/// deferred class would misrepresent the security posture.
pub fn build_vuln_map(
    features: &SpecCtrlFeatures,
    level: MitigationLevel,
) -> [(Vuln, Status); VULNS] {
    let meltdown_status = match level {
        MitigationLevel::Off => Status::Vulnerable,
        MitigationLevel::Full => Status::Mitigated("PTI"),
        MitigationLevel::Auto => {
            if features.rdcl_no {
                // Hardware is not susceptible; suppress KPTI overhead.
                Status::NotAffected
            } else {
                Status::Mitigated("PTI")
            }
        }
    };

    let spectre_v2_status = match level {
        MitigationLevel::Off => Status::Vulnerable,
        MitigationLevel::Full | MitigationLevel::Auto => Status::Mitigated("Retpoline, IBPB"),
    };

    [
        (Vuln::Meltdown, meltdown_status),
        (Vuln::SpectreV1, Status::Unaddressed),
        (Vuln::SpectreV2, spectre_v2_status),
        (Vuln::Mds, Status::Unaddressed),
        (Vuln::L1tf, Status::Unaddressed),
        (Vuln::Ssb, Status::Unaddressed),
        (Vuln::Retbleed, Status::Unaddressed),
        (Vuln::Downfall, Status::Unaddressed),
    ]
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── C.1 tests ────────────────────────────────────────────────────────────

    /// Verify individual EDX bit positions and the arch_caps guard:
    /// - EDX[26] → ibrs_ibpb
    /// - EDX[27] → stibp only
    /// - EDX[31] → ssbd
    /// - EDX[29] → arch_caps_present; with that bit clear, a non-zero
    ///   arch_caps must NOT set rdcl_no or eibrs.
    #[test]
    fn edx_bits() {
        // EDX[26] → ibrs_ibpb, nothing else.
        let f = SpecCtrlFeatures::from_cpuid(1 << 26, 0);
        assert!(f.ibrs_ibpb);
        assert!(!f.stibp);
        assert!(!f.ssbd);
        assert!(!f.arch_caps_present);
        assert!(!f.rdcl_no);
        assert!(!f.eibrs);

        // EDX[27] → stibp only.
        let f = SpecCtrlFeatures::from_cpuid(1 << 27, 0);
        assert!(!f.ibrs_ibpb);
        assert!(f.stibp);
        assert!(!f.ssbd);

        // EDX[31] → ssbd only.
        let f = SpecCtrlFeatures::from_cpuid(1 << 31, 0);
        assert!(!f.ibrs_ibpb);
        assert!(!f.stibp);
        assert!(f.ssbd);

        // EDX[29] set, arch_caps has both rdcl_no and eibrs → both decoded.
        let f = SpecCtrlFeatures::from_cpuid(1 << 29, 0b11);
        assert!(f.arch_caps_present);
        assert!(f.rdcl_no);
        assert!(f.eibrs);

        // EDX[29] CLEAR, but non-zero arch_caps → rdcl_no and eibrs stay false.
        let f = SpecCtrlFeatures::from_cpuid(0, 0xFFFF_FFFF_FFFF_FFFF);
        assert!(!f.arch_caps_present);
        assert!(!f.rdcl_no);
        assert!(!f.eibrs);
    }

    /// Max-basic-leaf guard: when max_basic_leaf < 7 the leaf-7 EDX argument
    /// (even 0xFFFF_FFFF) must yield all-false feature bits.  When
    /// max_basic_leaf >= 7 the bits are read normally.
    #[test]
    fn leaf7_absent_reads_zero() {
        // max_basic_leaf = 6 → EDX treated as 0 → all feature bits false.
        let f = SpecCtrlFeatures::from_cpuid_guarded(6, 0xFFFF_FFFF, 0);
        assert!(!f.ibrs_ibpb);
        assert!(!f.stibp);
        assert!(!f.ssbd);
        assert!(!f.arch_caps_present);
        assert!(!f.rdcl_no);
        assert!(!f.eibrs);

        // max_basic_leaf = 7 → EDX bits are read; all-ones sets everything.
        let f = SpecCtrlFeatures::from_cpuid_guarded(7, 0xFFFF_FFFF, 0b11);
        assert!(f.ibrs_ibpb);
        assert!(f.stibp);
        assert!(f.ssbd);
        assert!(f.arch_caps_present);
        assert!(f.rdcl_no);
        assert!(f.eibrs);
    }

    /// IBRS mode classification covers all three cases.
    #[test]
    fn ibrs_mode() {
        // eibrs (arch_caps[1]) + arch_caps_present (EDX[29]) + ibrs_ibpb
        // (EDX[26]) → Enhanced.
        let f = SpecCtrlFeatures::from_cpuid((1 << 29) | (1 << 26), 0b10);
        assert!(f.arch_caps_present);
        assert!(f.eibrs);
        assert_eq!(classify_ibrs(&f), IbrsMode::Enhanced);

        // ibrs_ibpb set, IBRS_ALL (arch_caps[1]) clear → Legacy.
        let f = SpecCtrlFeatures::from_cpuid(1 << 26, 0);
        assert!(f.ibrs_ibpb);
        assert!(!f.eibrs);
        assert_eq!(classify_ibrs(&f), IbrsMode::Legacy);

        // Neither ibrs_ibpb nor eibrs → None.
        let f = SpecCtrlFeatures::from_cpuid(0, 0);
        assert_eq!(classify_ibrs(&f), IbrsMode::None);
    }

    /// rdcl_no bit gated by arch_caps_present.
    #[test]
    fn rdcl_no() {
        // arch_caps[0] set and EDX[29] set → rdcl_no true.
        let f = SpecCtrlFeatures::from_cpuid(1 << 29, 0b01);
        assert!(f.rdcl_no);

        // arch_caps[0] set but EDX[29] CLEAR → rdcl_no must remain false.
        let f = SpecCtrlFeatures::from_cpuid(0, 0b01);
        assert!(!f.rdcl_no);
    }

    // ── D.1 tests ────────────────────────────────────────────────────────────

    /// `mitigations=` parsing: known values round-trip; unknown defaults to
    /// Auto and is flagged by `mitigations_recognized`.
    #[test]
    fn parse_mitigations() {
        assert_eq!(super::parse_mitigations("off"), MitigationLevel::Off);
        assert_eq!(super::parse_mitigations("auto"), MitigationLevel::Auto);
        assert_eq!(super::parse_mitigations("full"), MitigationLevel::Full);

        // Unknown value → Auto but NOT recognized.
        assert_eq!(super::parse_mitigations("garbage"), MitigationLevel::Auto);
        assert!(!mitigations_recognized("garbage"));

        // Known values are recognized.
        assert!(mitigations_recognized("off"));
        assert!(mitigations_recognized("auto"));
        assert!(mitigations_recognized("full"));
    }

    /// Vulnerability map honours the mitigation level for addressed vulns.
    #[test]
    fn vuln_map_tracks_level() {
        // Auto + rdcl_no (CPU not susceptible to Meltdown) → NotAffected.
        let features_rdcl_no = SpecCtrlFeatures::from_cpuid(1 << 29, 0b01);
        assert!(features_rdcl_no.rdcl_no);
        let map = build_vuln_map(&features_rdcl_no, MitigationLevel::Auto);
        let meltdown = map.iter().find(|(v, _)| *v == Vuln::Meltdown).unwrap();
        assert_eq!(meltdown.1, Status::NotAffected);

        // Off → Meltdown and SpectreV2 both Vulnerable.
        let features_plain = SpecCtrlFeatures::from_cpuid(0, 0);
        let map = build_vuln_map(&features_plain, MitigationLevel::Off);
        let meltdown = map.iter().find(|(v, _)| *v == Vuln::Meltdown).unwrap();
        let sv2 = map.iter().find(|(v, _)| *v == Vuln::SpectreV2).unwrap();
        assert_eq!(meltdown.1, Status::Vulnerable);
        assert_eq!(sv2.1, Status::Vulnerable);

        // Full → Meltdown Mitigated("PTI"), SpectreV2 Mitigated("Retpoline, IBPB").
        let map = build_vuln_map(&features_plain, MitigationLevel::Full);
        let meltdown = map.iter().find(|(v, _)| *v == Vuln::Meltdown).unwrap();
        let sv2 = map.iter().find(|(v, _)| *v == Vuln::SpectreV2).unwrap();
        assert_eq!(meltdown.1, Status::Mitigated("PTI"));
        assert_eq!(sv2.1, Status::Mitigated("Retpoline, IBPB"));
    }

    /// Unaddressed vulnerabilities appear as Unaddressed for every level.
    #[test]
    fn unaddressed_always_listed() {
        let features = SpecCtrlFeatures::from_cpuid(0, 0);
        let unaddressed_vulns = [
            Vuln::Mds,
            Vuln::L1tf,
            Vuln::Ssb,
            Vuln::Retbleed,
            Vuln::Downfall,
        ];
        for level in [
            MitigationLevel::Off,
            MitigationLevel::Auto,
            MitigationLevel::Full,
        ] {
            let map = build_vuln_map(&features, level);
            for vuln in &unaddressed_vulns {
                let entry = map.iter().find(|(v, _)| v == vuln).unwrap_or_else(|| {
                    panic!("{:?} missing from vuln map at level {:?}", vuln, level)
                });
                assert_eq!(
                    entry.1,
                    Status::Unaddressed,
                    "{:?} should be Unaddressed at level {:?}",
                    vuln,
                    level
                );
            }
        }
    }
}
