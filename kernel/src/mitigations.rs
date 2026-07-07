//! Phase 84 Track D.2 — boot-time mitigation policy + single global off-switch.
//!
//! A single [`MitigationState`] snapshot is populated **once** at BSP boot from
//! the build-time `mitigations=` default + the host-tested
//! [`kernel_core::spectre`] CPUID/MSR decode, and drives KPTI (Track A), IBRS /
//! eIBRS (C.2), IBPB (C.3), and the `m3ctl mitigations status` reporter (D.3).
//! Every consumer checks the ONE global off-switch ([`mitigations_off`]) — the
//! Linux `cpu_mitigations_off()` discipline — so `mitigations=off` cannot leave
//! a track half-applied.
//!
//! **Boot-cmdline surface (net-new, honest).** m3OS has no kernel
//! `/proc/cmdline` and `bootloader_api::BootInfo` carries no command line, so
//! the level is a **build-time default**: the `M3OS_MITIGATIONS` env var
//! (default `auto`) baked in at compile time via [`option_env!`]. The xtask
//! spectre / perf gates rebuild the kernel with `M3OS_MITIGATIONS=full|off` to
//! exercise the other levels; `kernel/build.rs` re-runs the build when the env
//! changes. The reporter reads this boot-populated snapshot, never a re-`rdmsr`
//! of the write-mostly SPEC_CTRL MSR.

use alloc::collections::BTreeSet;

use spin::{Mutex, Once};

pub use kernel_core::spectre::{
    IbrsMode, MitigationLevel, MitigationReport, SpecCtrlFeatures, Status, VULNS, Vuln,
    build_vuln_map, mitigations_recognized, parse_mitigations, report_map,
};

use crate::arch::x86_64::cpuid;

/// Compile-time `mitigations=` default. See module docs for why this is a
/// build-time value rather than a runtime cmdline.
const MITIGATIONS_DEFAULT: &str = match option_env!("M3OS_MITIGATIONS") {
    Some(s) => s,
    None => "auto",
};

/// `true` since Phase 110 A.4: Track A (KPTI) is fully wired into the
/// page-table / trampoline path — per-process user-half PML4s
/// (`AddressSpace::build_kpti_user_half`, A.3b), the LSTAR-selected
/// `syscall_entry_kpti` stub + KPTI-aware sysret tail (A.2), naked entry/exit
/// CR3 switches on every ring-3-reachable vector incl. the NMI/`#DF` paranoid
/// path (A.3b), all four ring0→ring3 exit trampolines (A.3b part 3), and the
/// per-core CR3-pair publish at every dispatch locus (A.4). `kpti_active =
/// kpti_policy && KPTI_WIRED` therefore reflects real enforcement: `auto`
/// activates on Meltdown-susceptible silicon — QEMU TCG reports
/// `rdcl_no=false`, so **every default QEMU boot runs the CR3 trampoline** —
/// and deactivates under `off` or `auto` + `RDCL_NO`. The activation itself is
/// the A.4 trio: this flag, the BSP LSTAR re-install after [`init_bsp`]
/// (`lib.rs`; APs and the S3-resume path self-select via `lstar_target`), and
/// `smp::publish_kpti_cr3_pair` going live (it no-ops while inactive).
const KPTI_WIRED: bool = true;

/// The boot-populated mitigation snapshot. Immutable after [`init_bsp`].
#[derive(Clone, Copy, Debug)]
pub struct MitigationState {
    /// Selected level (build-time default, parsed).
    pub level: MitigationLevel,
    /// `false` if the configured string was unrecognized (defaulted to `Auto`).
    pub level_recognized: bool,
    /// Decoded CPU speculation-control feature surface.
    pub features: SpecCtrlFeatures,
    /// IBRS mode **applied** this boot (`None` on `mitigations=off` or on
    /// silicon without SPEC_CTRL — e.g. the QEMU test lanes).
    pub ibrs_mode: IbrsMode,
    /// KPTI policy the level implies (on, unless `off` or `auto` + `RDCL_NO`).
    pub kpti_policy: bool,
    /// KPTI **actually enforcing** this boot — the reporter's source of truth.
    pub kpti_active: bool,
    /// Phase 110 A.5 — the KPTI PCID scheme is **active** this boot: KPTI is
    /// enforcing AND the CPU advertises both PCID and INVPCID
    /// ([`cpuid::probe_pcid`]), so `CR4.PCIDE` is enabled and the CR3-write loci
    /// tag the kernel/user halves with distinct PCIDs + the no-flush bit. On
    /// every QEMU lane (TCG advertises neither bit) this is `false` and the
    /// kernel runs the A.4 full-flush fallback. The single gate consulted by
    /// `smp::publish_kpti_cr3_pair`, `mm::write_kernel_cr3`, and the SMP
    /// shootdown to decide whether to emit PCID-tagged CR3 loads.
    pub pcid_active: bool,
    /// IBPB **enabled** this boot: when `true`, an IBPB is issued on every
    /// cross-process switch. Set once from policy + CPU features at boot — this
    /// is a configuration flag, not a counter of barriers actually issued.
    pub ibpb_active: bool,
    /// Guarded raw `CPUID.07H.0:EDX` (for the D.3 report wire).
    pub leaf7_edx: u32,
    /// Guarded raw `IA32_ARCH_CAPABILITIES` (for the D.3 report wire).
    pub arch_caps: u64,
}

static STATE: Once<MitigationState> = Once::new();

/// Decide the policy and apply the boot-time-applicable mitigations (eIBRS
/// set-once) on the **BSP**. Idempotent. Call once at BSP init, after the
/// CPUID/XSAVE probe. Requires [`cpuid::probe_spec_ctrl`] to be callable.
pub fn init_bsp() -> &'static MitigationState {
    STATE.call_once(|| {
        let level = parse_mitigations(MITIGATIONS_DEFAULT);
        let level_recognized = mitigations_recognized(MITIGATIONS_DEFAULT);
        let features = *cpuid::probe_spec_ctrl();
        let (leaf7_edx, arch_caps) = cpuid::spec_ctrl_raw_regs();
        let off = matches!(level, MitigationLevel::Off);

        // IBRS: apply eIBRS set-once when on and supported; otherwise leave the
        // MSR untouched (legacy toggle lives in the KPTI trampolines; `None`
        // and the off-switch perform no write → no `#GP`).
        let ibrs_mode = if off || !features.ibrs_ibpb {
            IbrsMode::None
        } else {
            // SAFETY: gated on `ibrs_ibpb`; ring 0 boot context, IRQs off.
            unsafe { cpuid::enable_ibrs() }
        };

        // KPTI policy: on unless off, or auto on Meltdown-immune (`RDCL_NO`).
        let kpti_policy = match level {
            MitigationLevel::Off => false,
            MitigationLevel::Full => true,
            MitigationLevel::Auto => !features.rdcl_no,
        };
        // Phase 110 A.4 — KPTI enforcement. The wired substrate (A.1–A.3b)
        // activates through three consumers of this flag, all downstream of
        // this snapshot: `lstar_target()` selects `syscall_entry_kpti` (the
        // BSP re-installs LSTAR right after this returns; APs and the S3
        // resume path run their `syscall::init*` after the policy decision),
        // `AddressSpace::build_kpti_user_half` builds the per-process user
        // half at every address-space birth, and `smp::publish_kpti_cr3_pair`
        // publishes the per-core CR3 pair at every dispatch locus (it no-ops
        // while inactive, keeping every entry/exit switch never-taken).
        let kpti_active = kpti_policy && KPTI_WIRED;

        // Phase 110 A.5 — PCID TLB-cost recovery is active iff KPTI enforces AND
        // the CPU has both PCID + INVPCID. `enable_pcid_if_kpti_active` (called
        // right after this returns on the BSP, per-AP via the CR4 trampoline
        // copy, and on S3 resume) sets `CR4.PCIDE` under the same condition, so
        // this flag and the live register agree. False on every QEMU lane.
        let pcid_active = kpti_active && cpuid::probe_pcid();

        let ibpb_active = !off && features.ibrs_ibpb;

        // Track A.4 — KPTI GLOBAL-bit guard. m3OS marks no kernel PTE GLOBAL, so
        // this must be 0; a nonzero count means a future CR4.PGE optimization
        // introduced global kernel PTEs that would survive a KPTI CR3 switch and
        // silently defeat the isolation. This is a security invariant, so it is a
        // hard `assert_eq!` that fires in release builds too (a `debug_assert!`
        // is compiled out of the `--release` kernel and would never catch the
        // regression on a shipping image). The count is 0 today, so the assert
        // is inert until such a regression is introduced.
        let global_kernel_ptes = crate::mm::count_global_kernel_leaf_ptes();
        assert_eq!(
            global_kernel_ptes, 0,
            "Track A.4: {global_kernel_ptes} GLOBAL kernel leaf PTE(s) would survive a KPTI CR3 switch"
        );

        let state = MitigationState {
            level,
            level_recognized,
            features,
            ibrs_mode,
            kpti_policy,
            kpti_active,
            pcid_active,
            ibpb_active,
            leaf7_edx,
            arch_caps,
        };

        log::info!(
            "[sec] mitigations={:?}{} ibrs={:?} ibpb={} stibp_avail={} rdcl_no={} kpti(policy={} active={}) pcid(active={} supported={}) global_kernel_ptes={}",
            state.level,
            if state.level_recognized { "" } else { " (unrecognized→auto)" },
            state.ibrs_mode,
            state.ibpb_active,
            state.features.stibp,
            state.features.rdcl_no,
            state.kpti_policy,
            state.kpti_active,
            state.pcid_active,
            cpuid::probe_pcid(),
            global_kernel_ptes,
        );
        state
    })
}

/// Re-apply per-core boot mitigations (eIBRS set-once) on an **AP**. Call from
/// the AP boot path after its CPUID is usable. No-op unless the BSP decided to
/// apply IBRS and the silicon supports it.
pub fn init_ap() {
    let Some(state) = STATE.get() else {
        return;
    };
    if matches!(state.ibrs_mode, IbrsMode::Enhanced) {
        // SAFETY: BSP already proved `ibrs_ibpb`; ring 0 AP boot context.
        unsafe {
            cpuid::enable_ibrs();
        }
    }
}

/// The boot snapshot, or `None` before [`init_bsp`].
pub fn state() -> Option<&'static MitigationState> {
    STATE.get()
}

/// The single global off-switch (the Linux `cpu_mitigations_off()` discipline).
/// `true` only when the booted level is `off`. Before [`init_bsp`] (no
/// snapshot yet) returns `false` — nothing consults it that early.
#[inline]
pub fn mitigations_off() -> bool {
    STATE
        .get()
        .map(|s| matches!(s.level, MitigationLevel::Off))
        .unwrap_or(false)
}

/// Whether the KPTI PCID scheme is active this boot (Phase 110 A.5): the single
/// gate `smp::publish_kpti_cr3_pair`, `mm::write_kernel_cr3`, and the SMP
/// shootdown consult before emitting PCID-tagged / no-flush CR3 loads. `false`
/// before [`init_bsp`] and on every QEMU lane (no PCID/INVPCID). Cheap (one
/// `Once` read) — safe on the dispatch hot path.
#[inline]
pub fn pcid_active() -> bool {
    STATE.get().map(|s| s.pcid_active).unwrap_or(false)
}

/// Whether IBPB should be issued on cross-process switches (C.3): the feature
/// is present and the off-switch is not engaged. Cheap (one `Once` read).
#[inline]
pub fn ibpb_enabled() -> bool {
    STATE.get().map(|s| s.ibpb_active).unwrap_or(false)
}

/// Whether STIBP is available to opt into (C.4): the CPU advertises it and the
/// off-switch is not engaged.
#[inline]
pub fn stibp_available() -> bool {
    STATE
        .get()
        .map(|s| s.features.stibp && !matches!(s.level, MitigationLevel::Off))
        .unwrap_or(false)
}

// ── C.4: per-process STIBP opt-in registry ──────────────────────────────────
//
// STIBP is default-off and opt-in (a real perf cost), so we track the set of
// PIDs that opted in via the `sys_set_spec_ctrl` syscall and have the scheduler
// apply/clear `SPEC_CTRL.STIBP` at dispatch — but only when [`stibp_available`]
// (a no-op on silicon without STIBP, e.g. every QEMU test lane). The set is
// consulted from the dispatch path **only** under that gate, so the lock is
// never taken on the common (no-STIBP) hardware. A reused PID could inherit a
// stale opt-in (harmless STIBP over-protection, STIBP-hardware only) — acceptable
// for this niche default-off control; explicit teardown is deferred.
static STIBP_OPT_IN: Mutex<BTreeSet<u32>> = Mutex::new(BTreeSet::new());

/// Record (or clear) a process's STIBP opt-in (C.4). Called from
/// `sys_set_spec_ctrl`.
pub fn set_stibp_opt_in(pid: u32, on: bool) {
    let mut set = STIBP_OPT_IN.lock();
    if on {
        set.insert(pid);
    } else {
        set.remove(&pid);
    }
}

/// Whether `pid` opted into STIBP. Consulted by the scheduler dispatch path
/// under the [`stibp_available`] gate.
pub fn stibp_opt_in(pid: u32) -> bool {
    STIBP_OPT_IN.lock().contains(&pid)
}

/// Build the honest per-vulnerability status map for the reporter (D.3).
///
/// Starts from the host-tested policy model [`build_vuln_map`] (keyed on level),
/// then **overrides Meltdown with the actual KPTI state** so a level of `full`
/// on a boot where KPTI is not yet enforcing reports `Vulnerable`, not a false
/// `Mitigated("PTI")`. Retpoline (Spectre-v2) is compile-time-unconditional and
/// is reported separately by the reporter, so the SpectreV2 entry here reflects
/// only the runtime-gated IBRS/IBPB layer (per the D.1 contract).
pub fn vuln_map() -> [(Vuln, Status); VULNS] {
    match STATE.get() {
        Some(s) => report_map(&s.features, s.level, s.kpti_active),
        // Pre-init: conservative default-on policy with no features.
        None => report_map(
            &SpecCtrlFeatures::from_cpuid(0, 0),
            MitigationLevel::Auto,
            false,
        ),
    }
}

/// The wire-serializable boot report for the `m3ctl mitigations status` syscall
/// (D.3). Reflects the actual applied state — never a re-`rdmsr`.
///
/// Phase 90a C.2 adds the W^X / PKU posture. The values are sourced live from
/// the B.1 PKU probes (single source of truth — never a re-derived CPUID read):
/// - `pku_present` = the architectural PKU bit + the XSAVE PKRU component, i.e.
///   the static half of [`cpuid::pku_usable`].
/// - `pku_active` = PKU was actually enabled this boot (`CR4.PKE` set on at
///   least one core), i.e. [`cpuid::ospke_enabled`].
/// - `wx_v2` = the W^X policy is **v2** (the pkey-guarded W+X exception is
///   available) iff PKU is active — the same `pku_usable()` gate the C.1
///   `wx_decision` guard consults. On the no-PKU TCG lane this is `false` (v1).
pub fn report() -> MitigationReport {
    // `pku_present` is the static (CPUID/XSAVE) half; `pku_active` requires the
    // kernel to have set `CR4.PKE` this boot. `wx_v2` follows `pku_active`: the
    // v2 pkey-guarded exception only exists when PKU is live.
    let pku_present = cpuid::pku_usable();
    let pku_active = pku_present && cpuid::ospke_enabled();
    let wx_v2 = pku_active;
    match STATE.get() {
        Some(s) => MitigationReport {
            level: s.level,
            level_recognized: s.level_recognized,
            kpti_active: s.kpti_active,
            ibpb_active: s.ibpb_active,
            ibrs_mode: s.ibrs_mode,
            leaf7_edx: s.leaf7_edx,
            arch_caps: s.arch_caps,
            wx_v2,
            pku_present,
            pku_active,
            pcid_active: s.pcid_active,
        },
        None => MitigationReport {
            level: MitigationLevel::Auto,
            level_recognized: true,
            kpti_active: false,
            ibpb_active: false,
            ibrs_mode: IbrsMode::None,
            leaf7_edx: 0,
            arch_caps: 0,
            wx_v2,
            pku_present,
            pku_active,
            pcid_active: false,
        },
    }
}
