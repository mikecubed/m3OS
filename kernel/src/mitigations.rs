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

/// `true` once Track A (KPTI) is wired into the page-table / trampoline path.
/// Until then KPTI cannot enforce regardless of policy, so [`MitigationState`]
/// reports `kpti_active = false` and the reporter honestly shows Meltdown as
/// `Vulnerable` (or `Not affected` on `RDCL_NO` silicon) — a half-built KPTI
/// can never read as `Mitigated`. Flipped to `true` by the KPTI landing PR,
/// which also adds the actual enable on the `kpti_policy` path.
const KPTI_WIRED: bool = false;

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
    /// IBPB issued on cross-process switch this boot.
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
        // KPTI can only enforce once Track A is wired. (When it is, this same
        // path will perform the enable and set `kpti_active` to the result.)
        let kpti_active = kpti_policy && KPTI_WIRED;

        let ibpb_active = !off && features.ibrs_ibpb;

        // Track A.4 — KPTI GLOBAL-bit guard. m3OS marks no kernel PTE GLOBAL, so
        // this must be 0; a nonzero count means a future CR4.PGE optimization
        // introduced global kernel PTEs that would survive a KPTI CR3 switch.
        let global_kernel_ptes = crate::mm::count_global_kernel_leaf_ptes();
        debug_assert_eq!(
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
            ibpb_active,
            leaf7_edx,
            arch_caps,
        };

        log::info!(
            "[sec] mitigations={:?}{} ibrs={:?} ibpb={} stibp_avail={} rdcl_no={} kpti(policy={} active={}) global_kernel_ptes={}",
            state.level,
            if state.level_recognized { "" } else { " (unrecognized→auto)" },
            state.ibrs_mode,
            state.ibpb_active,
            state.features.stibp,
            state.features.rdcl_no,
            state.kpti_policy,
            state.kpti_active,
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
pub fn report() -> MitigationReport {
    match STATE.get() {
        Some(s) => MitigationReport {
            level: s.level,
            level_recognized: s.level_recognized,
            kpti_active: s.kpti_active,
            ibpb_active: s.ibpb_active,
            ibrs_mode: s.ibrs_mode,
            leaf7_edx: s.leaf7_edx,
            arch_caps: s.arch_caps,
        },
        None => MitigationReport {
            level: MitigationLevel::Auto,
            level_recognized: true,
            kpti_active: false,
            ibpb_active: false,
            ibrs_mode: IbrsMode::None,
            leaf7_edx: 0,
            arch_caps: 0,
        },
    }
}
