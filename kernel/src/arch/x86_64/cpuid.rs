//! Phase 57e Track J — CPUID-based detection of XSAVE/AVX state preservation.
//!
//! The kernel queries CPUID at BSP init to discover the XSAVE feature surface
//! (whether XSAVE is supported by the CPU, whether the OS is allowed to enable
//! it via CR4.OSXSAVE, the supported state-component bitmap, the maximum and
//! current XSAVE area sizes for those components, and whether `xsaveopt` is
//! available).  The result is stored in a `spin::Once` and consulted by:
//!
//! * `enable_xsave_state()` (BSP and AP boot) — to set CR4.OSXSAVE and write
//!   the per-core XCR0 mask.
//! * `XSaveArea::new()` (task allocation) — to validate the static
//!   `XSAVE_AREA_SIZE` against the runtime requirement.
//! * `save_fpu_state` / `restore_fpu_state` — to choose the asm instruction
//!   variant (`xsaveopt64` if available, else `xsave64`).
//!
//! The 1.0 supported mask is x87 + SSE + AVX = 0x7.  AVX-512 is deferred (one
//! bit in XCR0; trivial to add).
//!
//! Hardware floor: Intel Sandy Bridge (2011) / AMD Bulldozer (2011) or later.
//! Earlier CPUs lack the architectural XSAVE instruction (CPUID.1.ECX bit 26)
//! and the AVX state component, so they are explicitly unsupported.  If the
//! boot-time probe finds either missing, the kernel panics with a clear
//! message.  The probe does **not** require CR4.OSXSAVE — that bit reflects
//! runtime state and is 0 until [`enable_xsave_state`] sets it.

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use spin::Once;
use x86_64::registers::model_specific::Msr;

/// XSAVE state-component mask for the 1.0 release: x87 (bit 0) + SSE (bit 1) +
/// AVX (bit 2).  AVX-512 (bit 5) is intentionally deferred.
pub const XSAVE_FEATURE_MASK: u64 = 0x7;

/// Static XSAVE area size.  Validated at boot in `kernel_main` against the
/// post-`enable_xsave_state` size returned by [`enabled_area_size`] (CPUID
/// 0Dh.0.EBX re-read after `xsetbv` so it reflects the actually-enabled XCR0
/// mask) — if a future CPUID change ever makes this too small, the kernel
/// panics.
///
/// Intel SDM Vol 1 §13.4: with x87 + SSE + AVX enabled in XCR0, the standard
/// (non-compacted) area is 832 bytes (legacy region 512, header 64, AVX YMM_HI
/// region 256).
///
/// **Phase 90a B.1:** when the CPU supports PKU and the kernel enables XSAVE
/// component 9 (PKRU) in XCR0, the standard area grows to include the PKRU
/// component, whose architectural offset (2688 on parts that reserve the
/// AVX-512 component region before it) + size (8 bytes) = 2696.  The static
/// buffer is therefore sized to 2752 (the next 64-byte multiple ≥ 2696) so the
/// per-task `XSaveArea`, the slab slot, the signal frame, and the syscall
/// snapshot buffers all fit the PKU-grown layout.  On a no-PKU CPU component 9
/// is never enabled, the enabled area stays 832, and the only cost is the
/// (small, fixed) slack in the static buffer — behaviour is otherwise
/// bit-for-bit unchanged.  `XSAVE_AREA_SIZE >= enabled_area_size()` is asserted
/// at boot on every configuration.
pub const XSAVE_AREA_SIZE: usize = 2752;

/// CPUID-discovered XSAVE feature surface.
///
/// Populated once during BSP init via [`probe`].
#[derive(Clone, Copy, Debug)]
pub struct XSaveFeatures {
    /// CPUID.1.ECX[26]: XSAVE instruction set supported by the CPU.
    pub supported: bool,
    /// CPUID.1.ECX[27]: OSXSAVE — the OS has enabled XSAVE via CR4.OSXSAVE.
    /// At probe time this is 0 (we haven't set CR4.OSXSAVE yet); the field is
    /// retained for diagnostic inspection of whether someone else has already
    /// enabled XSAVE on this CPU.
    #[allow(dead_code)]
    pub osxsave_capable: bool,
    /// CPUID.0Dh.0.EDX:EAX — supported state-component bitmap.
    pub supported_components: u64,
    /// CPUID.0Dh.0.ECX — maximum XSAVE area size for *all* supported
    /// components.  Always `>=` [`XSAVE_AREA_SIZE`].
    pub max_area_size: usize,
    /// CPUID.0Dh.0.EBX — current XSAVE area size for the components currently
    /// enabled in XCR0.  Captured at probe time (before [`enable_xsave_state`]
    /// runs), so this reflects the reset XCR0 (typically x87-only, ~512 B) on
    /// the BSP and is **not** the post-enable size for the 1.0 mask.  Use
    /// [`XSaveFeatures::max_area_size`] for the worst-case allocation budget;
    /// `area_size_at_mask` is retained for diagnostic output.
    pub area_size_at_mask: usize,
    /// CPUID.0Dh.1.EAX[0]: XSAVEOPT supported.  When true, the save path uses
    /// `xsaveopt64` to skip components in init form.
    pub xsaveopt: bool,
}

impl XSaveFeatures {
    /// Construct features from raw CPUID register triples.
    ///
    /// Pure helper — used by the runtime probe and by host-side unit tests in
    /// `kernel-core` that feed synthetic CPUID stubs.
    pub fn from_raw(
        leaf1_ecx: u32,
        leaf_d_0_eax: u32,
        leaf_d_0_ebx: u32,
        leaf_d_0_ecx: u32,
        leaf_d_0_edx: u32,
        leaf_d_1_eax: u32,
    ) -> Self {
        let supported = (leaf1_ecx & (1 << 26)) != 0;
        let osxsave_capable = (leaf1_ecx & (1 << 27)) != 0;
        let supported_components = (u64::from(leaf_d_0_edx) << 32) | u64::from(leaf_d_0_eax);
        let max_area_size = leaf_d_0_ecx as usize;
        let area_size_at_mask = leaf_d_0_ebx as usize;
        let xsaveopt = (leaf_d_1_eax & 1) != 0;
        Self {
            supported,
            osxsave_capable,
            supported_components,
            max_area_size,
            area_size_at_mask,
            xsaveopt,
        }
    }
}

static FEATURES: Once<XSaveFeatures> = Once::new();
static OSXSAVE_ENABLED: AtomicBool = AtomicBool::new(false);
/// Phase 90a B.1 — set true once `enable_xsave_state` has set `CR4.PKE` on at
/// least one core (i.e. PKU is active this boot).  Mirrors `OSXSAVE_ENABLED`.
static OSPKE_ENABLED: AtomicBool = AtomicBool::new(false);

/// Probe CPUID for XSAVE features.  Idempotent — first call wins.
///
/// Panics if the CPU does not advertise XSAVE (CPUID 1.ECX bit 26) or does
/// not advertise the required state-component mask (x87 + SSE + AVX = 0x7
/// in CPUID 0Dh.0.EDX:EAX).  m3OS as of 57e requires Sandy Bridge (2011)
/// or later.
///
/// **Note on `OSXSAVE`:** CPUID 1.ECX bit 27 (OSXSAVE) reflects the runtime
/// state of `CR4.OSXSAVE`, which is 0 at probe time (no kernel sets it
/// before this call) and 1 after [`enable_xsave_state`] runs.  We therefore
/// **don't** require it at probe time — only the architectural XSAVE bit.
/// The `osxsave_capable` field on [`XSaveFeatures`] is left for diagnostic
/// inspection of whether someone else has already enabled XSAVE.
pub fn probe() -> &'static XSaveFeatures {
    FEATURES.call_once(|| {
        let leaf1 = cpuid_raw(1, 0);
        let leaf_d_0 = cpuid_raw(0x0D, 0);
        let leaf_d_1 = cpuid_raw(0x0D, 1);
        let f = XSaveFeatures::from_raw(
            leaf1.ecx,
            leaf_d_0.eax,
            leaf_d_0.ebx,
            leaf_d_0.ecx,
            leaf_d_0.edx,
            leaf_d_1.eax,
        );
        assert!(
            f.supported,
            "57e requires XSAVE (CPUID 1.ECX bit 26); running on a pre-2011 CPU is not supported"
        );
        assert!(
            f.supported_components & XSAVE_FEATURE_MASK == XSAVE_FEATURE_MASK,
            "57e requires CPUID 0Dh state mask {:#x} (x87+SSE+AVX); CPU advertises {:#x}",
            XSAVE_FEATURE_MASK,
            f.supported_components
        );
        f
    })
}

/// Return the cached XSAVE features.  Must be called after [`probe`].
pub fn features() -> &'static XSaveFeatures {
    FEATURES
        .get()
        .expect("xsave_features() called before cpuid::probe() ran")
}

/// Enable XSAVE on the current core.
///
/// Sets `CR4.OSXSAVE` (bit 18) and writes `XCR0` via `xsetbv` with `ECX=0`.
/// The XCR0 mask is `XSAVE_FEATURE_MASK` (x87+SSE+AVX) plus, **when the CPU
/// supports PKU (Phase 90a B.1)**, the PKRU component bit (9) — and on a
/// PKU-capable CPU this also sets `CR4.PKE` (bit 22) so protection keys are
/// active and PKRU rides the per-task XSAVE save/restore.  Must be called once
/// on the BSP **before** `smp::boot::boot_aps()` (so APs inherit `CR4.OSXSAVE`
/// via the trampoline copy) and once on each AP after its CR4 is loaded.
///
/// **Per-core coverage is the point:** `CR4.PKE` and XCR0 are per-core
/// registers and are **not** carried by the SMP trampoline's `DATA_CR4` copy
/// for the PKE bit on its own (the trampoline snapshots BSP CR4, but XCR0 is
/// never inherited and this is the single per-core entry point that programs
/// both), so an AP that skipped this call would silently ignore protection-key
/// bits — a per-core security hole.  Routing PKE+XCR0.PKRU through the function
/// every core already runs closes that gap by construction.
///
/// # Safety
/// CR4 / XCR0 are privileged registers; this call must run in ring 0 with
/// IRQs disabled or equivalently single-threaded.
pub unsafe fn enable_xsave_state() {
    use x86_64::registers::control::{Cr4, Cr4Flags};
    let f = features();
    debug_assert!(f.supported);

    // CR4.OSFXSR (bit 9) is already set by the bootloader / startup — required
    // for fxsave64 to have worked under 57d.  We assert rather than set so an
    // unexpected unset surfaces loudly rather than silently re-enabling.
    // `assert!` (not `debug_assert!`) so a release-build boot-path regression
    // fails fast here instead of producing hard-to-debug #UD/#GP faults later
    // on the first SSE/AVX instruction.  This runs once per core at boot;
    // the runtime cost is irrelevant.
    let cr4 = Cr4::read();
    assert!(
        cr4.contains(Cr4Flags::OSFXSR),
        "CR4.OSFXSR must be set before enable_xsave_state"
    );

    // Phase 90a B.1 — when PKU is usable, set CR4.PKE (bit 22) on *this* core
    // so the protection-key MMU check is active and `RDPKRU`/`WRPKRU` do not
    // `#UD`.  `Cr4Flags::PROTECTION_KEY_USER` is bit 22.  On a no-PKU CPU
    // `pku_usable()` is false and CR4 is written exactly as before (PKE clear),
    // keeping behaviour bit-for-bit identical.
    let pku = pku_usable();
    let mut new_cr4 = cr4 | Cr4Flags::OSXSAVE;
    if pku {
        new_cr4 |= Cr4Flags::PROTECTION_KEY_USER;
    }
    unsafe {
        Cr4::write(new_cr4);
    }

    // Write XCR0 via `xsetbv` with ECX=0 — only XCR0 exists today.  When PKU is
    // usable, fold in component 9 (PKRU) so the architecture *knows* PKRU is
    // an active user-mode register and sizes the XSAVE area accordingly.
    // Phase 90a B.4 (gap closed): the per-task save/restore RFBM is now the
    // runtime [`xsave_rfbm`] (`0x207` here, when PKU is usable; `0x7` otherwise)
    // consumed by scheduler.rs `save_fpu_state`/`restore_fpu_state`/
    // `sanitize_xsave_header` and the signal-frame snapshot — so once XCR0[9] is
    // set here, PKRU is saved/restored across context switches and signal
    // delivery, and fresh tasks seed the Linux-default PKRU rather than the
    // all-permissive hardware init value.
    let mask = pku_features().xcr0_mask(XSAVE_FEATURE_MASK);
    unsafe {
        core::arch::asm!(
            "xsetbv",
            in("ecx") 0u32,
            in("eax") mask as u32,
            in("edx") (mask >> 32) as u32,
            options(nomem, nostack, preserves_flags),
        );
    }

    if pku {
        OSPKE_ENABLED.store(true, Ordering::Release);
    }
    OSXSAVE_ENABLED.store(true, Ordering::Release);
}

/// XSAVE area size (CPUID 0Dh.0.EBX) for the components currently enabled in
/// XCR0.  Re-runs CPUID every call — must be invoked after
/// [`enable_xsave_state`] for the value to reflect the current XCR0 mask
/// (legacy x87+SSE+AVX = 832 B; with PKRU component 9 enabled = 2752 B per
/// [`XSAVE_AREA_SIZE`]).  Used by the boot-time validation assertion to
/// confirm [`XSAVE_AREA_SIZE`] fits the actually-enabled mask.
pub fn enabled_area_size() -> usize {
    cpuid_raw(0x0D, 0).ebx as usize
}

/// True once `enable_xsave_state` has run at least once on the BSP.  Used by
/// the FPU save/restore path to gate xsave64 vs the legacy fxsave64 fallback.
#[inline]
pub fn osxsave_enabled() -> bool {
    OSXSAVE_ENABLED.load(Ordering::Acquire)
}

// ---------------------------------------------------------------------------
// Phase 90a Track B.1 — Memory Protection Keys (PKU) detection
// ---------------------------------------------------------------------------
//
// PKU is per-core state: `CR4.PKE` (bit 22) must be set on the BSP **and every
// AP** (an AP without it silently ignores key bits — a per-core security hole),
// and PKRU only rides XSAVE when component 9 is enabled in XCR0 and fits the
// sized XSAVE area the 57e probe validates.  This module exposes the detection;
// `enable_xsave_state` does the per-core CR4.PKE + XCR0 programming above.  All
// raw bit decode + the XSAVE-area accounting lives in host-tested
// `kernel_core::xsave_model::PkuFeaturesModel`.

pub use kernel_core::xsave_model::{
    PKRU_INIT_DEFAULT, PkuFeaturesModel, XCR0_PKRU, XSAVE_COMPONENT_PKRU,
};

/// The per-task / signal-frame XSAVE requested-feature bitmap (RFBM) for **this
/// boot**: `XSAVE_FEATURE_MASK` (0x7) on a no-PKU CPU, `0x207` (folding in PKRU
/// component 9) when PKU is usable.
///
/// **Phase 90a B.4:** this is the single runtime source of truth that every
/// `xsave64`/`xsaveopt64`/`xrstor64` site (`save_fpu_state` / `restore_fpu_state`
/// in the scheduler, the signal-frame snapshot, the syscall-path snapshot) and
/// the `sanitize_xsave_header` XSTATE_BV mask must consult — replacing the
/// hard-coded `XSAVE_FEATURE_MASK` so PKRU rides the task boundary.  Computed by
/// the host-tested [`kernel_core::xsave_model::xsave_rfbm`]; on a no-PKU CPU it
/// is bit-for-bit the legacy 0x7.
#[inline]
pub fn xsave_rfbm() -> u64 {
    kernel_core::xsave_model::xsave_rfbm(pku_usable())
}

/// The CPUID.0Dh.9:EBX byte offset of the PKRU state component within the
/// standard (non-compacted) XSAVE area, or `0` when PKU is not usable (no PKRU
/// component to seed).  Used by the new-task/fork PKRU seeding in `XSaveArea`.
#[inline]
pub fn pkru_component_offset() -> usize {
    if pku_usable() {
        pku_features().pkru_component_offset
    } else {
        0
    }
}

static PKU_FEATURES: Once<PkuFeaturesModel> = Once::new();

/// Probe the PKU/PKRU CPUID surface.  Idempotent — first call wins.
///
/// Reads `CPUID.07H.0:ECX` (PKU bit 3 / OSPKE bit 4) and the `CPUID.0Dh`
/// component-9 (PKRU) size/offset, both guarded by the max-basic-leaf check
/// (see [`probe_smep_smap`]) so an older CPU that lacks leaf 7 / sub-leaf 9
/// reports no PKU rather than mis-decoding a lower leaf.  Decode is the
/// host-tested [`PkuFeaturesModel::from_raw`].
///
/// **Note on OSPKE:** like `OSXSAVE`, `CPUID.07H.0:ECX[4]` reflects the runtime
/// state of `CR4.PKE` — 0 at probe time (the kernel hasn't set it yet), 1 after
/// [`enable_xsave_state`] runs — so the usability decision ([`pku_usable`])
/// **never** depends on it, only the architectural PKU bit (3) plus the XSAVE
/// component-9 advertisement.
pub fn probe_pku() -> &'static PkuFeaturesModel {
    PKU_FEATURES.call_once(|| {
        let max_leaf = cpuid_raw(0, 0).eax;
        if max_leaf < 0x0D {
            // CPUID leaf 0x0Dh (XSAVE enumeration) is unavailable, so the PKRU
            // state component (sub-leaf 9) cannot be advertised — PKU is not
            // usable regardless of whether leaf 0x07 (which may still exist when
            // 0x07 ≤ max_leaf < 0x0Dh) reports the architectural PKU bit. Report
            // no PKU surface.
            return PkuFeaturesModel::from_raw(0, 0, 0, 0, 0);
        }
        let leaf7_0 = cpuid_raw(0x07, 0);
        let leaf_d_0 = cpuid_raw(0x0D, 0);
        let leaf_d_9 = cpuid_raw(0x0D, XSAVE_COMPONENT_PKRU);
        PkuFeaturesModel::from_raw(
            leaf7_0.ecx,
            leaf_d_0.eax,
            leaf_d_0.edx,
            leaf_d_9.eax,
            leaf_d_9.ebx,
        )
    })
}

/// The cached PKU features.  Must be called after [`probe_pku`] (which
/// `enable_xsave_state` triggers via [`pku_usable`] on the first core).
pub fn pku_features() -> &'static PkuFeaturesModel {
    probe_pku()
}

/// True when the kernel may enable PKU on this CPU: the architectural PKU bit
/// is set **and** the XSAVE PKRU component (9) is advertised (so PKRU can ride
/// the per-task XSAVE save/restore).  Idempotently probes on first use.
///
/// This is the single `pku_supported()`-style predicate all downstream code
/// (the `pkey_*` syscalls, the W^X v2 rule, the `pku-smoke` gate, the `m3ctl`
/// reporter) must consult — they must never assume PKU from a bare CPUID read.
pub fn pku_usable() -> bool {
    probe_pku().pku_usable()
}

/// True if `CR4.PKE` (bit 22) is currently set on **this** core.  Reads the
/// live register, so a per-core audit (e.g. the AP boot log) can confirm the
/// bit actually landed rather than trusting the global `OSPKE_ENABLED` flag.
pub fn cr4_pke_enabled() -> bool {
    let cr4: u64;
    unsafe {
        core::arch::asm!("mov {}, cr4", out(reg) cr4, options(nomem, nostack));
    }
    cr4 & (1 << 22) != 0
}

/// True once `enable_xsave_state` has set `CR4.PKE` on at least one core this
/// boot (PKU is active).  False on a no-PKU CPU.  Mirrors [`osxsave_enabled`].
#[inline]
pub fn ospke_enabled() -> bool {
    OSPKE_ENABLED.load(Ordering::Acquire)
}

// ---------------------------------------------------------------------------
// Phase 77 Track B — SMEP / SMAP (cheap CR4 security mitigations)
// ---------------------------------------------------------------------------

/// `CPUID.07h:0.EBX[7]` — Supervisor-Mode Execution Prevention.
const CPUID_07_EBX_SMEP: u32 = 1 << 7;
/// `CPUID.07h:0.EBX[20]` — Supervisor-Mode Access Prevention.
const CPUID_07_EBX_SMAP: u32 = 1 << 20;

static SMEP_SMAP: Once<(bool, bool)> = Once::new();

/// Probe `CPUID.07h:0.EBX` for SMEP (bit 7) and SMAP (bit 20).  Returns
/// `(smep_supported, smap_supported)`.  Idempotent — first call wins.
///
/// Leaf `0x07` is not probed elsewhere in this module (only leaves 1 and
/// `0x0D`), so this is the only `CPUID.07h` consumer.
pub fn probe_smep_smap() -> (bool, bool) {
    *SMEP_SMAP.call_once(|| {
        // CPUID leaf `0x07` only exists when the maximum basic leaf
        // (`CPUID.0:EAX`) is at least 7. On an older CPU or a VM CPU model
        // that exposes only lower leaves, executing `cpuid` with an
        // unsupported basic leaf returns the data of the *highest* supported
        // leaf instead — whose EBX bits 7/20 could be mistaken for SMEP/SMAP
        // support, after which `enable_smep_smap` would set unsupported CR4
        // bits and `#GP` during boot. Gate the read on the max basic leaf.
        if cpuid_raw(0, 0).eax < 0x07 {
            return (false, false);
        }
        let leaf7 = cpuid_raw(0x07, 0);
        (
            leaf7.ebx & CPUID_07_EBX_SMEP != 0,
            leaf7.ebx & CPUID_07_EBX_SMAP != 0,
        )
    })
}

/// Enable `CR4.SMEP` (bit 20) and `CR4.SMAP` (bit 21) on the **current** core
/// when the CPU reports support.  Returns `(smep_enabled, smap_enabled)`.
///
/// * **SMEP** faults (`#PF`) if ring 0 ever *fetches an instruction* from a
///   user-accessible page — closing the "jump into userspace shellcode"
///   exploit class.  It has no effect on legitimate kernel execution.
/// * **SMAP** faults if ring 0 *reads or writes* a user-accessible page
///   outside an explicit `STAC`/`CLAC` window.  m3OS is SMAP-clean by
///   construction: every deliberate user-memory access funnels through
///   `mm::user_mem` (`copy_from_user`/`copy_to_user` and the `UserSlice*`
///   wrappers) and the ELF loader / ABI-stack / signal-frame writers, all of
///   which reach the bytes through the **physical-memory direct map**
///   (`mm::phys_offset() + phys_addr`) — a supervisor mapping that SMAP does
///   not police — never through the user virtual address.  No `STAC`/`CLAC`
///   window is therefore required; enabling the bit is free.
///
/// Must run on the BSP **before** `smp::boot::boot_aps()` so the trampoline's
/// captured `DATA_CR4` carries the bits and every AP inherits them when it
/// reloads CR4 in `ap_entry`.
///
/// # Safety
/// CR4 is a privileged register; this must run in ring 0 with IRQs disabled or
/// single-threaded.
pub unsafe fn enable_smep_smap() -> (bool, bool) {
    let (smep, smap) = probe_smep_smap();
    if !smep && !smap {
        return (false, false);
    }
    let mut cr4: u64;
    unsafe {
        core::arch::asm!("mov {}, cr4", out(reg) cr4, options(nomem, nostack));
    }
    if smep {
        cr4 |= 1 << 20;
    }
    if smap {
        cr4 |= 1 << 21;
    }
    unsafe {
        core::arch::asm!("mov cr4, {}", in(reg) cr4, options(nostack));
    }
    // NOTE: clearing EFLAGS.AC (so SMAP actually enforces — it only blocks
    // ring-0 user access while AC == 0) is done by the *callers*, not here.
    // A `clac` inside this function would be undone by the `without_interrupts`
    // popf that the BSP caller wraps this in.  See `clear_ac_for_smap` and the
    // SFMASK (ALIGNMENT_CHECK) syscall-entry mask.
    (smep, smap)
}

/// Clear `EFLAGS.AC` on the current core so `CR4.SMAP` actually enforces.
///
/// SMAP only blocks ring-0 access to user pages while `AC == 0`; firmware may
/// leave `AC == 1`.  Call this on each core's boot path **outside** any
/// `without_interrupts` / `pushf`/`popf` bracket (which would restore the old
/// AC).  Syscall entry separately clears AC via `SFMASK`.
///
/// # Safety
/// `clac` is only valid when `CR4.SMAP` is set; call after [`enable_smep_smap`].
#[inline]
pub unsafe fn clear_ac_for_smap() {
    if cr4_smap_enabled() {
        unsafe {
            core::arch::asm!("clac", options(nomem, nostack));
        }
    }
}

/// True if `CR4.SMEP` (bit 20) is currently set on this core.
pub fn cr4_smep_enabled() -> bool {
    let cr4: u64;
    unsafe {
        core::arch::asm!("mov {}, cr4", out(reg) cr4, options(nomem, nostack));
    }
    cr4 & (1 << 20) != 0
}

/// True if `CR4.SMAP` (bit 21) is currently set on this core.
pub fn cr4_smap_enabled() -> bool {
    let cr4: u64;
    unsafe {
        core::arch::asm!("mov {}, cr4", out(reg) cr4, options(nomem, nostack));
    }
    cr4 & (1 << 21) != 0
}

// ---------------------------------------------------------------------------
// Phase 110 Track A.5 — PCID / INVPCID (KPTI TLB-cost recovery)
// ---------------------------------------------------------------------------
//
// KPTI switches CR3 twice per syscall/IRQ. With PCID (`CR4.PCIDE`) the
// entry/exit trampolines can load the kernel/user half **no-flush** (distinct
// PCIDs tag the two halves), recovering the bulk of the ~30 % naive-KPTI cost.
// The pure bit-layout/gate logic is host-tested in `kernel_core::kpti_pcid`;
// this file performs the privileged probe + `CR4.PCIDE` write, gated on both
// the PCID and INVPCID CPUID bits so a CPU lacking either never `#GP`s and runs
// the A.4 full-flush fallback. QEMU TCG advertises neither bit, so on every CI
// lane `probe_pcid()` is `false` and `CR4.PCIDE` stays 0 — the scheme is live
// only on bare metal (validated on the Dell).

static PCID_SUPPORTED: Once<bool> = Once::new();

/// Probe whether the KPTI PCID scheme is usable: **both** `CPUID.01H:ECX[17]`
/// (PCID) and `CPUID.07H.0:EBX[10]` (INVPCID) present. Idempotent — first call
/// wins. The gate decision is the host-tested
/// [`kernel_core::kpti_pcid::pcid_supported`]. Both bits are `0` under QEMU TCG.
pub fn probe_pcid() -> bool {
    *PCID_SUPPORTED.call_once(|| {
        // Bench-bisection knob (Phase 110 Dell validation): `M3OS_MASK_PCID=1` at
        // build time forces "PCID unsupported" (identical to QEMU TCG) so the
        // CR4.PCIDE enable + tagged-CR3 + INVPCID paths stay off while KPTI+CET
        // remain active — to isolate a PCID-vs-CET bring-up fault on real
        // silicon. Default off; no effect on production builds.
        if option_env!("M3OS_MASK_PCID").is_some() {
            return false;
        }
        let leaf1_ecx = cpuid_raw(1, 0).ecx;
        // Leaf 7 is guarded by the max-basic-leaf check (same trap as
        // `probe_smep_smap`): reading leaf 7 on a CPU whose max basic leaf is
        // < 7 returns the highest leaf's data, which could spuriously set the
        // INVPCID bit and enable an unsupported feature.
        let leaf7_ebx = if cpuid_raw(0, 0).eax >= 0x07 {
            cpuid_raw(0x07, 0).ebx
        } else {
            0
        };
        kernel_core::kpti_pcid::pcid_supported(leaf1_ecx, leaf7_ebx)
    })
}

/// Enable `CR4.PCIDE` (bit 17) on the **current** core when the CPU supports the
/// PCID scheme AND KPTI is active this boot. Returns whether PCIDE is now set.
///
/// `CR4.PCIDE` may be set only while `CR3[11:0] == 0` (Intel SDM Vol. 3A
/// §4.10.1) — true at every call site (the kernel runs on a page-aligned boot /
/// process PML4 with no PCID yet), so no explicit CR3 scrub is needed. Enabling
/// PCIDE changes how `mov cr3` interprets bits 11:0 and 63, so the kernel only
/// enables it once every CR3-write locus is PCID-aware (Phase 110 A.5). No-op
/// unless [`probe_pcid`] holds — so on QEMU/CI it never runs and the CR3 writes
/// keep loading `PCID = 0` with a plain flush (the A.4 fallback).
///
/// Must run on the BSP **before** `smp::boot::boot_aps()` so the trampoline's
/// captured `DATA_CR4` carries the bit and every AP inherits it, and on the S3
/// resume path (the machine reset clears CR4).
///
/// # Safety
/// CR4 is a privileged register; ring 0, IRQs disabled or single-threaded.
pub unsafe fn enable_pcid_if_kpti_active(kpti_active: bool) -> bool {
    if !kpti_active || !probe_pcid() {
        return false;
    }
    let mut cr4: u64;
    unsafe {
        core::arch::asm!("mov {}, cr4", out(reg) cr4, options(nomem, nostack));
    }
    cr4 |= 1 << 17; // CR4.PCIDE
    unsafe {
        core::arch::asm!("mov cr4, {}", in(reg) cr4, options(nostack));
    }
    true
}

/// True if `CR4.PCIDE` (bit 17) is currently set on this core.
pub fn cr4_pcide_enabled() -> bool {
    let cr4: u64;
    unsafe {
        core::arch::asm!("mov {}, cr4", out(reg) cr4, options(nomem, nostack));
    }
    cr4 & (1 << 17) != 0
}

// ---------------------------------------------------------------------------
// Phase 110 Track B.3 — CET user shadow stacks
// ---------------------------------------------------------------------------
//
// CET shadow stacks are a hardware CFI layer: CALL pushes the return address to
// a protected shadow stack, RET checks it, and a mismatch (a return-address
// overwrite the canary missed) faults #CP before control transfers. The pure
// bit-layout/decode logic is host-tested in `kernel_core::cet`; this file does
// the privileged probe + `CR4.CET`/`IA32_U_CET` writes, gated on the CET_SS
// CPUID bit so a CPU without it never `#GP`s. QEMU TCG does not model CET, so
// on every CI lane `probe_cet()` is `false`, `CR4.CET` stays 0, and the whole
// shadow-stack path is inert (validated active on the Dell Tiger Lake).

static CET_FEATURES: Once<kernel_core::cet::CetFeatures> = Once::new();
static CET_ENABLED: AtomicBool = AtomicBool::new(false);

/// Probe the CET feature surface: `CPUID.07H.0:ECX[7]` (CET_SS) + `:EDX[20]`
/// (CET_IBT), guarded by the max-basic-leaf check (the `probe_smep_smap` trap —
/// reading leaf 7 on a CPU whose max basic leaf is `< 7` returns a lower leaf's
/// data, which could spuriously set the CET bit and enable an unsupported
/// feature). Idempotent — first call wins. Decode is the host-tested
/// [`kernel_core::cet::CetFeatures::from_leaf7`]. Both bits are `0` on QEMU TCG.
pub fn probe_cet() -> kernel_core::cet::CetFeatures {
    *CET_FEATURES.call_once(|| {
        // Bench-bisection knob (Phase 110 Dell validation): `M3OS_MASK_CET=1` at
        // build time forces "no CET" (identical to QEMU TCG) so the CR4.CET +
        // IA32_U_CET + shadow-stack paths stay off while KPTI+PCID remain active
        // — to isolate a CET-vs-PCID bring-up fault on real silicon. Default off;
        // no effect on production builds.
        if option_env!("M3OS_MASK_CET").is_some() {
            return kernel_core::cet::CetFeatures::from_leaf7(0, 0);
        }
        if cpuid_raw(0, 0).eax < 0x07 {
            return kernel_core::cet::CetFeatures::from_leaf7(0, 0);
        }
        let leaf7 = cpuid_raw(0x07, 0);
        kernel_core::cet::CetFeatures::from_leaf7(leaf7.ecx, leaf7.edx)
    })
}

/// True when the kernel may enable user shadow stacks: the architectural
/// `CET_SS` bit is set ([`kernel_core::cet::CetFeatures::shstk_usable`]).
/// The single predicate every downstream CET consumer (per-task shadow-stack
/// alloc, the `#CP` handler's relevance, the reporter) must consult. `false`
/// on QEMU TCG.
pub fn cet_shstk_usable() -> bool {
    probe_cet().shstk_usable()
}

/// Enable CET user shadow stacks on the **current** core when the CPU supports
/// `CET_SS` AND the CET policy is on this boot. Sets `CR4.CET` (bit 23) and
/// `IA32_U_CET.SH_STK_EN` (+ `WR_SHSTK_EN`, so the signal path may seed a
/// restore token onto the user shadow stack via `WRUSS`). Leaves `IA32_S_CET`
/// untouched — **no** supervisor (kernel) shadow stack. Returns whether CET is
/// now enabled on this core.
///
/// `IA32_PL3_SSP` is left 0 here; each task's shadow stack is armed at first
/// entry to ring 3 (Track B.3 3/n) — with `SH_STK_EN` set but `PL3_SSP = 0` a
/// task that never gets a shadow stack simply performs no shadow-stack ops
/// until one is installed.
///
/// Must run on the BSP **before** `smp::boot::boot_aps()` so the trampoline's
/// captured `DATA_CR4` carries `CR4.CET` and every AP inherits it, and again on
/// the S3 resume path (the machine reset clears CR4 + the CET MSRs). No-op
/// unless [`cet_shstk_usable`] holds — so on QEMU/CI it never runs.
///
/// # Safety
/// CR4 + the CET MSRs are privileged; ring 0, IRQs disabled or single-threaded.
pub unsafe fn enable_user_cet_if_supported(policy_on: bool) -> bool {
    use kernel_core::cet::{MSR_IA32_U_CET, compose_u_cet};
    use x86_64::registers::control::{Cr0, Cr0Flags};
    if !policy_on || !cet_shstk_usable() {
        return false;
    }
    // Bare-metal bring-up diagnostic (Phase 110 CET boot hang, Dell/Tiger Lake):
    // serial-free POST squares that localize *which* CET-enable instruction hangs
    // real silicon. Slots 32–35 (grid row 2) — the last square painted is the
    // last step that completed, so the hang is in the instruction after it. No-op
    // unless built with `M3OS_BRINGUP_DIAG=1` (see `crate::BRINGUP_DIAG`). The BSP
    // reaches here first (before `boot_aps`), so on the hanging path only it
    // paints these. See docs/handoffs/2026-07-09-cet-boot-hang-on-tiger-lake.md.
    crate::post_marker(32); // entered CET enable (policy on + CET_SS usable)
    // Intel SDM Vol 3A: setting `CR4.CET` while `CR0.WP = 0` raises `#GP(0)` —
    // CET is architecturally tied to write-protect. QEMU TCG models no CET and
    // never enforces this, so a `WP = 0` boot works there but `#GP`-hangs on real
    // CET silicon (the Dell Precision 5560 black-screened at exactly this write).
    // Nothing else in BSP boot guarantees `WP`, so ensure it on this core before
    // touching `CR4.CET`. `WP` is per-core; every core enabling CET runs this.
    unsafe {
        if !Cr0::read().contains(Cr0Flags::WRITE_PROTECT) {
            Cr0::update(|f| f.insert(Cr0Flags::WRITE_PROTECT));
        }
    }
    crate::post_marker(33); // CR0.WP = 1 confirmed; next: mov cr4 (CR4.CET)
    let mut cr4: u64;
    unsafe {
        core::arch::asm!("mov {}, cr4", out(reg) cr4, options(nomem, nostack));
    }
    cr4 |= kernel_core::cet::CR4_CET;
    unsafe {
        core::arch::asm!("mov cr4, {}", in(reg) cr4, options(nostack));
    }
    crate::post_marker(34); // CR4.CET set OK; next: wrmsr IA32_U_CET
    unsafe {
        // User shadow stacks on; WRUSS allowed (signal restore-token seeding).
        Msr::new(MSR_IA32_U_CET).write(compose_u_cet(true, true));
    }
    crate::post_marker(35); // IA32_U_CET written — CET enable fully succeeded
    CET_ENABLED.store(true, Ordering::Release);
    true
}

/// True if `CR4.CET` (bit 23) is currently set on this core (a live-register
/// audit for the per-core boot log, like [`cr4_pcide_enabled`]).
pub fn cr4_cet_enabled() -> bool {
    let cr4: u64;
    unsafe {
        core::arch::asm!("mov {}, cr4", out(reg) cr4, options(nomem, nostack));
    }
    cr4 & kernel_core::cet::CR4_CET != 0
}

/// True once [`enable_user_cet_if_supported`] set `CR4.CET` on at least one core
/// this boot (CET is active). `false` on a no-CET CPU. Mirrors [`ospke_enabled`].
#[inline]
pub fn cet_enabled() -> bool {
    CET_ENABLED.load(Ordering::Acquire)
}

/// True if `EFLAGS.AC` (bit 18) is currently set on this core.
pub fn eflags_ac_set() -> bool {
    use x86_64::registers::rflags::{self, RFlags};
    rflags::read().contains(RFlags::ALIGNMENT_CHECK)
}

/// Debug-only assertion that SMAP is currently *enforcing*: when `CR4.SMAP` is
/// set, `EFLAGS.AC` must be 0 (SMAP only blocks ring-0 access to user pages
/// while `AC == 0`). Stripped in release builds; in a debug kernel it catches a
/// ring-0 path that left AC set — e.g. a `popf`/`iret` that restored a
/// firmware-set AC, or an interrupt/exception entry that forgot to `clac`
/// (PR #201 audit) — turning a silent SMAP-disabled window into a loud panic.
#[inline(always)]
pub fn debug_assert_smap_enforcing() {
    debug_assert!(
        !cr4_smap_enabled() || !eflags_ac_set(),
        "SMAP enabled but EFLAGS.AC=1 — SMAP is non-enforcing on this ring-0 path"
    );
}

// ---------------------------------------------------------------------------
// Phase 84 Track C — IA32_SPEC_CTRL family (IBRS / eIBRS / IBPB / STIBP)
// ---------------------------------------------------------------------------
//
// Mirrors the Phase 77 SMEP/SMAP detect→enable→status shape. Every bit of raw
// CPUID/MSR decode lives in host-tested `kernel_core::spectre`; this file only
// performs the privileged probe + MSR writes, each gated on the decoded feature
// bits so a CPU lacking SPEC_CTRL never `#GP`s. On the QEMU test lanes
// (`qemu64`, and `-cpu host` on AMD which advertises IBRS via its own
// `Fn8000_0008_EBX` leaf rather than `CPUID.07H.0:EDX[26]`) the Intel bit is
// absent, so `IbrsMode::None` is reported and **no** SPEC_CTRL/PRED_CMD write
// is performed — the path is exercised by the `kernel_core::spectre` host tests
// and by the dedicated `mitigations=full` spectre gate with a spec-ctrl CPU.

pub use kernel_core::spectre::{IbrsMode, SpecCtrlFeatures, classify_ibrs};

/// `IA32_SPEC_CTRL` — IBRS (bit 0), STIBP (bit 1), SSBD (bit 2).
const MSR_IA32_SPEC_CTRL: u32 = 0x48;
/// `IA32_PRED_CMD` — **write-only**; bit 0 = IBPB.
const MSR_IA32_PRED_CMD: u32 = 0x49;
/// `IA32_ARCH_CAPABILITIES` — RDCL_NO (bit 0), IBRS_ALL (bit 1).
const MSR_IA32_ARCH_CAPABILITIES: u32 = 0x10A;

/// `IA32_SPEC_CTRL.IBRS` (bit 0).
const SPEC_CTRL_IBRS: u64 = 1 << 0;
/// `IA32_SPEC_CTRL.STIBP` (bit 1).
const SPEC_CTRL_STIBP: u64 = 1 << 1;
/// `IA32_PRED_CMD.IBPB` (bit 0).
const PRED_CMD_IBPB: u64 = 1 << 0;

static SPEC_CTRL_FEATURES: Once<SpecCtrlFeatures> = Once::new();
/// The (guarded) raw `(CPUID.07H.0:EDX, IA32_ARCH_CAPABILITIES)` behind
/// [`probe_spec_ctrl`]. Cached so the D.3 report wire can carry them verbatim.
static SPEC_CTRL_RAW: Once<(u32, u64)> = Once::new();

/// Cached `IA32_SPEC_CTRL` value (mirrors Linux `x86_spec_ctrl_base`). The MSR
/// is write-mostly, so we never re-`rdmsr` it as an "is it active?" signal:
/// `spec_ctrl_active()` reads this snapshot, and every write ORs the desired
/// bits into the base so a blind write cannot clobber IBRS/STIBP/SSBD set by
/// another path. Holds the **always-on** bits (eIBRS); per-task bits (STIBP
/// opt-in) are OR'd on top per-core at the call site, not stored here.
static SPEC_CTRL_BASE: AtomicU64 = AtomicU64::new(0);

/// Probe the `IA32_SPEC_CTRL` feature surface. Idempotent — first call wins.
///
/// Reads `CPUID.07H.0:EDX` (guarded by the max-basic-leaf check — see
/// [`probe_smep_smap`]) and, **only** when `EDX[29]` (ARCH_CAPABILITIES) is set,
/// `IA32_ARCH_CAPABILITIES` (MSR `0x10A`); an unguarded `rdmsr` of `0x10A`
/// `#GP`s on a CPU lacking it. Decode is the host-tested
/// [`kernel_core::spectre::SpecCtrlFeatures::from_cpuid_guarded`].
pub fn probe_spec_ctrl() -> &'static SpecCtrlFeatures {
    SPEC_CTRL_FEATURES.call_once(|| {
        let (leaf7_edx, arch_caps) = spec_ctrl_raw_regs();
        // `spec_ctrl_raw_regs` already applied the max-basic-leaf guard
        // (leaf7_edx == 0 when CPUID.0:EAX < 7), so `from_cpuid` here is
        // equivalent to `from_cpuid_guarded` (host-tested in kernel_core).
        SpecCtrlFeatures::from_cpuid(leaf7_edx, arch_caps)
    })
}

/// The cached **guarded** raw `(CPUID.07H.0:EDX, IA32_ARCH_CAPABILITIES)` pair.
///
/// `leaf7_edx` is `0` when the max basic leaf (`CPUID.0:EAX`) is `< 7` (the
/// trap [`probe_smep_smap`] defends); `arch_caps` is `0` unless `EDX[29]` is set
/// — an unguarded `rdmsr` of `0x10A` `#GP`s on a CPU lacking it. Idempotent;
/// the D.3 reporter ships these verbatim so a reader reconstructs the identical
/// [`SpecCtrlFeatures`] via the same host-tested decode.
pub fn spec_ctrl_raw_regs() -> (u32, u64) {
    *SPEC_CTRL_RAW.call_once(|| {
        let max_leaf = cpuid_raw(0, 0).eax;
        let leaf7_edx = if max_leaf >= 0x07 {
            cpuid_raw(0x07, 0).edx
        } else {
            0
        };
        let arch_caps = if (leaf7_edx & (1 << 29)) != 0 {
            // SAFETY: 0x10A is read only when CPUID advertised it (EDX[29]).
            unsafe { Msr::new(MSR_IA32_ARCH_CAPABILITIES).read() }
        } else {
            0
        };
        (leaf7_edx, arch_caps)
    })
}

/// The cached SPEC_CTRL features. Must be called after [`probe_spec_ctrl`].
pub fn spec_ctrl_features() -> &'static SpecCtrlFeatures {
    SPEC_CTRL_FEATURES
        .get()
        .expect("spec_ctrl_features() called before probe_spec_ctrl()")
}

/// Write the cached always-on base value (eIBRS) on the **current** core. Used
/// to enable eIBRS once per core at boot.
///
/// # Safety
/// `IA32_SPEC_CTRL` (0x48) must be present (`ibrs_ibpb`); ring 0 only.
unsafe fn write_spec_ctrl_base() {
    let base = SPEC_CTRL_BASE.load(Ordering::Acquire);
    unsafe {
        Msr::new(MSR_IA32_SPEC_CTRL).write(base);
    }
}

/// Detect and apply IBRS per the silicon's capability, returning the mode.
///
/// * `IbrsMode::Enhanced` (`IBRS_ALL`) → set `SPEC_CTRL.IBRS` **once on this
///   core** (folded into the cached base) — protects unconditionally with no
///   per-entry toggle. Call on the BSP and each AP at boot.
/// * `IbrsMode::Legacy` → IBRS is the per-kernel-entry toggle that lives in the
///   KPTI A.2/A.3 trampolines; **not** set here (retpoline, which is
///   compile-time-unconditional, already covers Spectre-v2 BTI in the interim).
/// * `IbrsMode::None` → nothing (no SPEC_CTRL write; no `#GP`).
///
/// Requires [`probe_spec_ctrl`] to have run.
///
/// # Safety
/// Writes `IA32_SPEC_CTRL` on Enhanced parts; ring 0, boot context.
pub unsafe fn enable_ibrs() -> IbrsMode {
    let f = spec_ctrl_features();
    let mode = classify_ibrs(f);
    if mode == IbrsMode::Enhanced {
        // SAFETY: Enhanced implies ibrs_ibpb (classify_ibrs requires the bit
        // path); SPEC_CTRL is present.
        unsafe {
            // Fold IBRS into the global base, then write this core's MSR.
            SPEC_CTRL_BASE.fetch_or(SPEC_CTRL_IBRS, Ordering::AcqRel);
            write_spec_ctrl_base();
        }
    }
    mode
}

/// Issue an IBPB (Indirect Branch Prediction Barrier) on the current core.
///
/// `IA32_PRED_CMD` (`0x49`) is **write-only** — an `rdmsr` of it `#GP`s, so we
/// never read it. Caller MUST gate on `features.ibrs_ibpb` and the global
/// mitigations off-switch. Used between **distinct** address spaces on the
/// context-switch path (C.3).
///
/// # Safety
/// `IA32_PRED_CMD` (0x49) must be present (`ibrs_ibpb`); ring 0 only.
pub unsafe fn issue_ibpb() {
    unsafe {
        Msr::new(MSR_IA32_PRED_CMD).write(PRED_CMD_IBPB);
    }
}

/// Set or clear `SPEC_CTRL.STIBP` (bit 1) on the **current** core for the task
/// about to run, composing it on top of the cached always-on base (so eIBRS is
/// preserved). Caller MUST gate on `features.stibp` and the global off-switch.
/// (C.4)
///
/// STIBP is a **per-core / per-task** control, so this composes the value into
/// *this core's* MSR only and never writes the per-task bit back into the shared
/// [`SPEC_CTRL_BASE`] (which holds always-on bits only). Storing STIBP in the
/// process-global base would let a dispatch on one core perturb the bit another
/// core composes for its own running task — the MSR write here is the single
/// source of truth for the current core's STIBP state.
///
/// # Safety
/// `IA32_SPEC_CTRL` (0x48) must be present; ring 0 only.
pub unsafe fn set_stibp(on: bool) {
    // Read the always-on base (eIBRS); never mutate it for a per-task bit.
    let base = SPEC_CTRL_BASE.load(Ordering::Acquire);
    let value = if on {
        base | SPEC_CTRL_STIBP
    } else {
        base & !SPEC_CTRL_STIBP
    };
    unsafe {
        Msr::new(MSR_IA32_SPEC_CTRL).write(value);
    }
}

/// True if the kernel has IBRS active (per the cached base snapshot — never a
/// re-`rdmsr` of the write-mostly MSR). Reflects eIBRS set-once; legacy-IBRS
/// per-entry state is not represented here (it is transient in the trampoline).
pub fn spec_ctrl_active() -> bool {
    SPEC_CTRL_BASE.load(Ordering::Acquire) & SPEC_CTRL_IBRS != 0
}

// ---------------------------------------------------------------------------
// Phase 103 Track E — HWP (Hardware-Controlled Performance States)
// ---------------------------------------------------------------------------

/// `CPUID.06h:EAX[7]` — HWP base (the `IA32_PM_ENABLE`/`IA32_HWP_REQUEST`
/// MSRs exist).
const CPUID_06_EAX_HWP: u32 = 1 << 7;
/// `CPUID.06h:EAX[11]` — `IA32_HWP_REQUEST_PKG` (one package-wide write
/// instead of a per-logical-processor MSR broadcast).
const CPUID_06_EAX_HWP_PKG: u32 = 1 << 11;

static HWP: Once<(bool, bool)> = Once::new();

/// Probe `CPUID.06h:EAX` for HWP (bit 7) and package-level HWP request
/// (bit 11).  Returns `(hwp_supported, hwp_pkg_supported)`.  Idempotent —
/// first call wins.  QEMU (TCG *and* KVM's default CPU models) exposes
/// neither, so both are `false` on every CI lane; the cpufreq module
/// degrades to a probe-only posture there.
///
/// Leaf `0x06` is not probed elsewhere in this module; same max-basic-leaf
/// guard as [`probe_smep_smap`] (an unsupported leaf echoes the highest
/// supported leaf's registers, which could fake the feature bits).
pub fn probe_hwp() -> (bool, bool) {
    *HWP.call_once(|| {
        if cpuid_raw(0, 0).eax < 0x06 {
            return (false, false);
        }
        let leaf6 = cpuid_raw(0x06, 0);
        (
            leaf6.eax & CPUID_06_EAX_HWP != 0,
            leaf6.eax & CPUID_06_EAX_HWP_PKG != 0,
        )
    })
}

#[derive(Clone, Copy)]
struct CpuidRaw {
    eax: u32,
    ebx: u32,
    ecx: u32,
    edx: u32,
}

/// Execute `cpuid` with the given leaf and sub-leaf.
///
/// Delegates to [`core::arch::x86_64::__cpuid_count`], the canonical
/// intrinsic that handles RBX preservation under PIC correctly.  An earlier
/// hand-rolled inline-asm version used `out(reg) _` for the RBX spill slot,
/// which the compiler could legitimately allocate to RBX itself — making the
/// save a `mov rbx, rbx` no-op and corrupting the caller's RBX after the
/// `cpuid` clobber.
fn cpuid_raw(leaf: u32, sub_leaf: u32) -> CpuidRaw {
    // `__cpuid_count` is safe on x86_64 — the `cpuid` instruction is
    // unconditionally available (it's the architectural feature-discovery
    // mechanism) and has no preconditions beyond setting eax/ecx.
    let result = core::arch::x86_64::__cpuid_count(leaf, sub_leaf);
    CpuidRaw {
        eax: result.eax,
        ebx: result.ebx,
        ecx: result.ecx,
        edx: result.edx,
    }
}
