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

/// `mitigations=` policy level. In m3OS this is selected at **build time** via
/// the `M3OS_MITIGATIONS` environment variable (there is no kernel boot command
/// line); this pure-logic crate only parses/classifies the value.
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

/// Parse a `mitigations=` policy value (the build-time `M3OS_MITIGATIONS`
/// string in m3OS — see [`MitigationLevel`]).
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

/// Honest per-vulnerability map for the runtime reporter (D.3).
///
/// Starts from the policy model [`build_vuln_map`] (keyed on `level`), then
/// **overrides Meltdown with the actual KPTI state** (`kpti_active`): a `Full`
/// level on a boot where KPTI is not enforcing reports `Vulnerable`, not a false
/// `Mitigated("PTI")` — a half-built KPTI can never read as mitigated. Shared by
/// the kernel snapshot and the `m3ctl` formatter so the honesty rule lives in
/// exactly one host-tested place.
pub fn report_map(
    features: &SpecCtrlFeatures,
    level: MitigationLevel,
    kpti_active: bool,
) -> [(Vuln, Status); VULNS] {
    let mut map = build_vuln_map(features, level);
    for entry in map.iter_mut() {
        if entry.0 == Vuln::Meltdown {
            entry.1 = if features.rdcl_no {
                Status::NotAffected
            } else if kpti_active {
                Status::Mitigated("PTI")
            } else {
                Status::Vulnerable
            };
        }
    }
    map
}

// ── D.3 / C.4: syscall numbers + report wire format ─────────────────────────
//
// m3OS-native syscall numbers in the custom `0x1000–0x1FFF` range. The
// device-host family occupies `0x112x`; the mitigations family takes `0x114x`.
// Declared here (single source of truth, like `device_host::syscalls`) so the
// kernel dispatcher and the `m3ctl` userspace wrapper share the same constants.

/// `m3ctl mitigations status` — copy the boot [`MitigationReport`] wire bytes
/// into a user buffer. `sys_mitigations_status(buf_ptr, buf_len) -> isize`
/// (bytes written, or a negative errno).
pub const SYS_MITIGATIONS_STATUS: u64 = 0x1140;

/// Per-process STIBP opt-in (C.4) — m3OS-native (m3OS has no Linux `prctl`).
/// `sys_set_spec_ctrl(enable_stibp) -> isize` (0 on success, negative errno).
pub const SYS_SET_SPEC_CTRL: u64 = 0x1141;

/// Fixed wire length of an encoded [`MitigationReport`].
pub const MITIGATION_REPORT_WIRE_LEN: usize = 16;

/// The boot mitigation snapshot in a compact, fixed-layout, host-testable wire
/// form. The kernel encodes its boot snapshot; `m3ctl` decodes and formats. The
/// CPU feature surface travels as the raw `leaf7_edx` + `arch_caps` so the
/// reader reconstructs [`SpecCtrlFeatures`] via the same decode (DRY).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MitigationReport {
    pub level: MitigationLevel,
    pub level_recognized: bool,
    pub kpti_active: bool,
    pub ibpb_active: bool,
    pub ibrs_mode: IbrsMode,
    pub leaf7_edx: u32,
    pub arch_caps: u64,
    /// Phase 90a Track C.2 — W^X policy is **v2** this boot (the pkey-guarded
    /// W+X exception is available). Sourced from PKU being active: v2 iff
    /// `pku_active`, v1 otherwise. (No separate flag — `wx_v2 == pku_active`
    /// by construction; carried explicitly so the formatter need not re-derive
    /// the rule.)
    pub wx_v2: bool,
    /// Phase 90a Track C.2 — PKU is **present** in CPUID (the architectural PKU
    /// bit + the XSAVE PKRU component, i.e. `cpuid::pku_usable()`'s static
    /// half). A no-PKU CPU reports `false`.
    pub pku_present: bool,
    /// Phase 90a Track C.2 — PKU is **active** this boot (`CR4.PKE` was set, so
    /// `RDPKRU`/`WRPKRU` and the v2 W+X exception are live). On the default TCG
    /// lane (no PKU) this is `false`; under a PKU host it is `true`.
    pub pku_active: bool,
    /// Phase 110 Track A.5 — the KPTI **PCID** TLB-cost-recovery scheme is active
    /// this boot: KPTI enforces AND the CPU has PCID + INVPCID, so `CR4.PCIDE` is
    /// on and the trampoline CR3 loads carry distinct kernel/user PCIDs +
    /// no-flush. `false` on every QEMU lane (TCG models neither instruction), so
    /// KPTI there runs the full-flush fallback. Only meaningful when
    /// `kpti_active`; a Meltdown-immune (`RDCL_NO`) or `off` boot leaves it
    /// `false`.
    pub pcid_active: bool,
}

impl MitigationReport {
    /// Reconstruct the decoded feature surface from the carried raw registers.
    pub fn features(&self) -> SpecCtrlFeatures {
        SpecCtrlFeatures::from_cpuid(self.leaf7_edx, self.arch_caps)
    }

    /// The honest per-vulnerability map (see [`report_map`]).
    pub fn vuln_map(&self) -> [(Vuln, Status); VULNS] {
        report_map(&self.features(), self.level, self.kpti_active)
    }

    /// Encode to the fixed `MITIGATION_REPORT_WIRE_LEN` byte layout (LE).
    pub fn encode(&self) -> [u8; MITIGATION_REPORT_WIRE_LEN] {
        let mut b = [0u8; MITIGATION_REPORT_WIRE_LEN];
        b[0] = match self.level {
            MitigationLevel::Off => 0,
            MitigationLevel::Auto => 1,
            MitigationLevel::Full => 2,
        };
        // Bits 0..=2 are the Phase 84 flags; bits 3..=5 carry the Phase 90a
        // C.2 W^X/PKU posture; bit 6 carries the Phase 110 A.5 PCID flag — all
        // in the same byte (no parallel channel, no length change). Bit 7 stays
        // free for a future flag.
        b[1] = (self.level_recognized as u8)
            | ((self.kpti_active as u8) << 1)
            | ((self.ibpb_active as u8) << 2)
            | ((self.wx_v2 as u8) << 3)
            | ((self.pku_present as u8) << 4)
            | ((self.pku_active as u8) << 5)
            | ((self.pcid_active as u8) << 6);
        b[2] = match self.ibrs_mode {
            IbrsMode::None => 0,
            IbrsMode::Legacy => 1,
            IbrsMode::Enhanced => 2,
        };
        // Wire version 3 (Phase 110 A.5 added the PCID flag bit to `b[1]`; v2 was
        // Phase 90a C.2's W^X/PKU bits). Bumped so a stale decoder refuses rather
        // than reading the new bit as zero. The kernel and `m3ctl` are built
        // together, so both sides move in lock-step.
        b[3] = 3;
        b[4..8].copy_from_slice(&self.leaf7_edx.to_le_bytes());
        b[8..16].copy_from_slice(&self.arch_caps.to_le_bytes());
        b
    }

    /// Decode from the wire layout. Returns `None` on short or malformed input,
    /// including an unrecognized wire-version byte (`b[3]`) — a future,
    /// incompatible layout is refused rather than silently mis-decoded.
    pub fn decode(buf: &[u8]) -> Option<Self> {
        if buf.len() < MITIGATION_REPORT_WIRE_LEN {
            return None;
        }
        // Wire version (written as `b[3] = 3` by `encode()`; was 2 for 90a, 1
        // pre-90a). Reject anything we do not know how to parse so a bumped
        // format fails cleanly here.
        if buf[3] != 3 {
            return None;
        }
        let level = match buf[0] {
            0 => MitigationLevel::Off,
            1 => MitigationLevel::Auto,
            2 => MitigationLevel::Full,
            _ => return None,
        };
        let flags = buf[1];
        let ibrs_mode = match buf[2] {
            0 => IbrsMode::None,
            1 => IbrsMode::Legacy,
            2 => IbrsMode::Enhanced,
            _ => return None,
        };
        let leaf7_edx = u32::from_le_bytes(buf[4..8].try_into().ok()?);
        let arch_caps = u64::from_le_bytes(buf[8..16].try_into().ok()?);
        Some(Self {
            level,
            level_recognized: flags & 0b0000_0001 != 0,
            kpti_active: flags & 0b0000_0010 != 0,
            ibpb_active: flags & 0b0000_0100 != 0,
            ibrs_mode,
            leaf7_edx,
            arch_caps,
            wx_v2: flags & 0b0000_1000 != 0,
            pku_present: flags & 0b0001_0000 != 0,
            pku_active: flags & 0b0010_0000 != 0,
            pcid_active: flags & 0b0100_0000 != 0,
        })
    }
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

    // ── D.3 / C.4 tests ──────────────────────────────────────────────────────

    /// `report_map` overrides Meltdown with the ACTUAL kpti state so a
    /// half-built KPTI cannot read as `Mitigated`.
    #[test]
    fn report_map_meltdown_tracks_actual_kpti() {
        let plain = SpecCtrlFeatures::from_cpuid(0, 0);
        let meltdown =
            |m: &[(Vuln, Status); VULNS]| m.iter().find(|(v, _)| *v == Vuln::Meltdown).unwrap().1;

        // Full level but KPTI NOT enforcing → Vulnerable (not a false PTI claim).
        let m = report_map(&plain, MitigationLevel::Full, false);
        assert_eq!(meltdown(&m), Status::Vulnerable);

        // Full level and KPTI enforcing → Mitigated("PTI").
        let m = report_map(&plain, MitigationLevel::Full, true);
        assert_eq!(meltdown(&m), Status::Mitigated("PTI"));

        // RDCL_NO silicon → NotAffected regardless of kpti_active / level.
        let immune = SpecCtrlFeatures::from_cpuid(1 << 29, 0b01);
        assert!(immune.rdcl_no);
        assert_eq!(
            meltdown(&report_map(&immune, MitigationLevel::Full, false)),
            Status::NotAffected
        );
        assert_eq!(
            meltdown(&report_map(&immune, MitigationLevel::Auto, false)),
            Status::NotAffected
        );

        // Off → Vulnerable (no rdcl_no).
        assert_eq!(
            meltdown(&report_map(&plain, MitigationLevel::Off, false)),
            Status::Vulnerable
        );

        // Unaddressed classes are still always Unaddressed through report_map.
        let m = report_map(&plain, MitigationLevel::Full, true);
        for v in [
            Vuln::Mds,
            Vuln::L1tf,
            Vuln::Ssb,
            Vuln::Retbleed,
            Vuln::Downfall,
        ] {
            assert_eq!(
                m.iter().find(|(x, _)| *x == v).unwrap().1,
                Status::Unaddressed
            );
        }
    }

    /// `MitigationReport` survives an encode→decode round-trip across all
    /// levels / IBRS modes, and reconstructs the feature surface.
    #[test]
    fn mitigation_report_wire_round_trip() {
        for level in [
            MitigationLevel::Off,
            MitigationLevel::Auto,
            MitigationLevel::Full,
        ] {
            for ibrs_mode in [IbrsMode::None, IbrsMode::Legacy, IbrsMode::Enhanced] {
                // Vary the C.2 W^X/PKU bits across the matrix so the flag-byte
                // packing is exercised in every combination. `wx_v2` mirrors
                // `pku_active` by construction, so they move together here.
                let pku_active = matches!(ibrs_mode, IbrsMode::Legacy);
                let r = MitigationReport {
                    level,
                    level_recognized: matches!(level, MitigationLevel::Full),
                    kpti_active: matches!(ibrs_mode, IbrsMode::Enhanced),
                    ibpb_active: !matches!(level, MitigationLevel::Off),
                    ibrs_mode,
                    leaf7_edx: (1 << 26) | (1 << 29),
                    arch_caps: 0b11,
                    wx_v2: pku_active,
                    pku_present: !matches!(ibrs_mode, IbrsMode::None),
                    pku_active,
                    // Phase 110 A.5 — vary the PCID bit across the matrix so the
                    // flag-byte packing is exercised set and clear (PCID implies
                    // KPTI active, so key it on the same condition).
                    pcid_active: matches!(ibrs_mode, IbrsMode::Enhanced),
                };
                let bytes = r.encode();
                assert_eq!(bytes.len(), MITIGATION_REPORT_WIRE_LEN);
                let back = MitigationReport::decode(&bytes).expect("decode");
                assert_eq!(back, r);
                // features() reconstructs from the carried raw registers.
                let f = back.features();
                assert!(f.ibrs_ibpb && f.arch_caps_present && f.rdcl_no && f.eibrs);
            }
        }
        // Short buffer → None.
        assert!(MitigationReport::decode(&[0u8; 4]).is_none());
        // Bad level tag → None.
        let mut bad = [0u8; MITIGATION_REPORT_WIRE_LEN];
        bad[0] = 9;
        assert!(MitigationReport::decode(&bad).is_none());
        // Unrecognized wire version → None (a valid encode otherwise).
        let mut wrong_ver = MitigationReport {
            level: MitigationLevel::Auto,
            level_recognized: true,
            kpti_active: false,
            ibpb_active: true,
            ibrs_mode: IbrsMode::None,
            leaf7_edx: 0,
            arch_caps: 0,
            wx_v2: false,
            pku_present: false,
            pku_active: false,
            pcid_active: false,
        }
        .encode();
        assert!(MitigationReport::decode(&wrong_ver).is_some());
        wrong_ver[3] = 4; // bump the version byte past the current v3
        assert!(MitigationReport::decode(&wrong_ver).is_none());
    }

    /// Phase 90a C.2 — the W^X v2 / PKU posture bits survive the wire
    /// round-trip independently of the Phase 84 flags, and are packed into the
    /// spare bits of `b[1]` (no length change, version byte = 3 since A.5).
    #[test]
    fn wx_pku_posture_wire_round_trip() {
        let base = MitigationReport {
            level: MitigationLevel::Auto,
            level_recognized: true,
            kpti_active: false,
            ibpb_active: false,
            ibrs_mode: IbrsMode::None,
            leaf7_edx: 0,
            arch_caps: 0,
            wx_v2: false,
            pku_present: false,
            pku_active: false,
            pcid_active: false,
        };

        // No-PKU boot (default TCG lane): all three posture bits clear.
        let no_pku = base.encode();
        assert_eq!(no_pku.len(), MITIGATION_REPORT_WIRE_LEN);
        assert_eq!(no_pku[3], 3, "wire version must be 3 after A.5");
        let back = MitigationReport::decode(&no_pku).expect("decode no-pku");
        assert!(!back.wx_v2 && !back.pku_present && !back.pku_active);

        // PKU-active boot: v2 + present + active all set.
        let pku = MitigationReport {
            wx_v2: true,
            pku_present: true,
            pku_active: true,
            ..base
        }
        .encode();
        let back = MitigationReport::decode(&pku).expect("decode pku");
        assert!(back.wx_v2 && back.pku_present && back.pku_active);

        // The C.2 bits are independent of the Phase 84 flags: a present-but-
        // inactive CPU (PKU silicon the kernel did not enable) decodes cleanly.
        let present_only = MitigationReport {
            pku_present: true,
            ..base
        }
        .encode();
        let back = MitigationReport::decode(&present_only).expect("decode present-only");
        assert!(back.pku_present && !back.pku_active && !back.wx_v2);
        // The Phase 84 flag bits (0..=2) are untouched by the C.2 bits.
        assert!(back.level_recognized && !back.kpti_active && !back.ibpb_active);
    }

    /// Phase 110 A.5 — the PCID flag (bit 6 of `b[1]`) survives the wire
    /// round-trip and is independent of every other flag.
    #[test]
    fn pcid_active_wire_round_trip() {
        let base = MitigationReport {
            level: MitigationLevel::Full,
            level_recognized: true,
            kpti_active: true,
            ibpb_active: false,
            ibrs_mode: IbrsMode::None,
            leaf7_edx: 0,
            arch_caps: 0,
            wx_v2: false,
            pku_present: false,
            pku_active: false,
            pcid_active: false,
        };

        // Fallback lane: KPTI active, PCID off.
        let back = MitigationReport::decode(&base.encode()).expect("decode fallback");
        assert!(back.kpti_active && !back.pcid_active);

        // PCID-active lane: the bit round-trips set, without disturbing the
        // adjacent PKU bits.
        let pcid = MitigationReport {
            pcid_active: true,
            ..base
        };
        let back = MitigationReport::decode(&pcid.encode()).expect("decode pcid");
        assert!(back.pcid_active);
        assert!(!back.pku_present && !back.pku_active && !back.wx_v2);
    }
}
