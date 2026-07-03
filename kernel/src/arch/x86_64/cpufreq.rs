//! Phase 103 Track E — the cpufreq MSR **mechanism** (ring 0).
//!
//! Division of labor (charter correction, mirrors the Phase 103 slice-1
//! split): the governor *policy* — the conservative state machine in
//! `kernel_core::power::governor` — ticks in ring-3 `powerd` per the
//! userspace-first rule. This module owns only what genuinely requires
//! ring 0: probing HWP via CPUID, opting in via `IA32_PM_ENABLE`, and
//! translating an abstract 1–255 target onto `IA32_HWP_REQUEST`.
//!
//! Every QEMU lane (TCG and KVM default CPU models) exposes **no HWP**,
//! so CI proves the probe + graceful-degradation posture only; the MSR
//! write path is bare-metal/VFIO-validated like the mt792x radio path.
//! Legacy `IA32_PERF_CTL` P-state stepping needs the `_PSS` table
//! (acpid evaluation) and stays a documented residual.

use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use kernel_core::power::syscalls::{CPUFREQ_MECH_HWP, CPUFREQ_MECH_NONE, CpufreqStatusWire};
use x86_64::registers::model_specific::Msr;

/// HWP opt-in; write-once 1 (sticky until reset), package scope.
const IA32_PM_ENABLE: u32 = 0x770;
/// Read-only performance range: bits 7:0 highest, 31:24 lowest.
const IA32_HWP_CAPABILITIES: u32 = 0x771;
/// Package-wide request (exists iff CPUID.06h:EAX[11]).
const IA32_HWP_REQUEST_PKG: u32 = 0x772;
/// Per-logical-processor request.
const IA32_HWP_REQUEST: u32 = 0x774;

/// `IA32_HWP_REQUEST` energy/performance preference, bits 31:24
/// (0 = max performance, 255 = max energy saving); 0x80 = balanced.
const EPP_BALANCED: u64 = 0x80;

static MECHANISM: AtomicU8 = AtomicU8::new(CPUFREQ_MECH_NONE);
static LAST_TARGET: AtomicU8 = AtomicU8::new(0);
static HWP_HIGHEST: AtomicU8 = AtomicU8::new(0);
static HWP_LOWEST: AtomicU8 = AtomicU8::new(0);
static HWP_PKG: AtomicBool = AtomicBool::new(false);

/// BSP boot probe: detect HWP, opt in, and cache the capability range.
/// Safe to call exactly once, before APs boot (`IA32_PM_ENABLE` and the
/// capability MSR are package-scope, so the BSP write covers siblings).
pub fn init_bsp() {
    let (hwp, hwp_pkg) = super::cpuid::probe_hwp();
    if !hwp {
        log::info!(
            "cpufreq: no HWP (CPUID.06h:EAX[7] clear) — governor targets are computed but not applied"
        );
        return;
    }
    // SAFETY: CPUID reported HWP, so IA32_PM_ENABLE/IA32_HWP_CAPABILITIES
    // exist; PM_ENABLE=1 is the architectural opt-in (sticky until reset).
    let caps = unsafe {
        Msr::new(IA32_PM_ENABLE).write(1);
        Msr::new(IA32_HWP_CAPABILITIES).read()
    };
    let highest = (caps & 0xFF) as u8;
    let lowest = ((caps >> 24) & 0xFF) as u8;
    HWP_HIGHEST.store(highest, Ordering::Relaxed);
    HWP_LOWEST.store(lowest, Ordering::Relaxed);
    HWP_PKG.store(hwp_pkg, Ordering::Relaxed);
    MECHANISM.store(CPUFREQ_MECH_HWP, Ordering::Release);
    log::info!(
        "cpufreq: HWP enabled, perf range {lowest}..{highest}, pkg-request={}",
        hwp_pkg
    );
}

/// Apply a governor target from the abstract 1–255 scale. Returns the
/// hardware perf level written, or 0 when no mechanism is present (the
/// QEMU posture — the call is a successful no-op so `powerd` behaves
/// identically on every platform).
pub fn apply_target(target: u8) -> u8 {
    let target = target.max(1);
    LAST_TARGET.store(target, Ordering::Relaxed);
    if MECHANISM.load(Ordering::Acquire) != CPUFREQ_MECH_HWP {
        return 0;
    }
    let highest = HWP_HIGHEST.load(Ordering::Relaxed) as u64;
    let lowest = HWP_LOWEST.load(Ordering::Relaxed) as u64;
    // Map 1..=255 linearly onto [lowest, highest].
    let span = highest.saturating_sub(lowest);
    let level = lowest + (span * (target as u64 - 1)) / 254;
    // Cap maximum-allowed at the target (the governor's actual control),
    // keep minimum at the hardware floor, desired = 0 (hardware autonomy
    // inside the [min, max] window), EPP balanced.
    let request = lowest | (level << 8) | (EPP_BALANCED << 24);
    // SAFETY: mechanism == HWP means init_bsp probed the MSRs; PKG vs
    // per-CPU selection follows CPUID.06h:EAX[11]. A per-CPU write lands
    // on whichever core runs the syscall — acceptable single-package
    // behavior for this slice (multi-core broadcast is a documented
    // residual alongside IA32_PERF_CTL).
    unsafe {
        if HWP_PKG.load(Ordering::Relaxed) {
            Msr::new(IA32_HWP_REQUEST_PKG).write(request);
        } else {
            Msr::new(IA32_HWP_REQUEST).write(request);
        }
    }
    level as u8
}

/// Snapshot for `SYS_POWER_CPUFREQ_STATUS`: probe results + cumulative
/// scheduler CPU times (ring-3 `powerd` diffs successive snapshots into
/// the governor's load sample).
pub fn status() -> CpufreqStatusWire {
    let (user_ticks, system_ticks, idle_ticks) = crate::task::scheduler::global_cpu_times();
    CpufreqStatusWire {
        mechanism: MECHANISM.load(Ordering::Acquire),
        last_target: LAST_TARGET.load(Ordering::Relaxed),
        hwp_highest: HWP_HIGHEST.load(Ordering::Relaxed),
        hwp_lowest: HWP_LOWEST.load(Ordering::Relaxed),
        user_ticks,
        system_ticks,
        idle_ticks,
    }
}
