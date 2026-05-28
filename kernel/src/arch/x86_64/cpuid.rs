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

use core::sync::atomic::{AtomicBool, Ordering};

use spin::Once;

/// XSAVE state-component mask for the 1.0 release: x87 (bit 0) + SSE (bit 1) +
/// AVX (bit 2).  AVX-512 (bit 5) is intentionally deferred.
pub const XSAVE_FEATURE_MASK: u64 = 0x7;

/// Static XSAVE area size for the 1.0 mask.  Validated at boot in
/// `kernel_main` against the post-`enable_xsave_state` size returned by
/// [`enabled_area_size`] (CPUID 0Dh.0.EBX re-read after `xsetbv` so it
/// reflects the actually-enabled XCR0 mask) — if a future CPUID change
/// ever makes this too small, the kernel panics.
///
/// Intel SDM Vol 1 §13.4: with x87 + SSE + AVX enabled in XCR0, the standard
/// area is 832 bytes (legacy region 512, header 64, AVX YMM_HI region 256).
pub const XSAVE_AREA_SIZE: usize = 832;

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
/// Sets `CR4.OSXSAVE` (bit 18) and writes `XCR0 = XSAVE_FEATURE_MASK` via
/// `xsetbv` with `ECX=0`.  Must be called once on the BSP **before**
/// `smp::boot::boot_aps()` (so APs inherit `CR4` via the trampoline copy)
/// and once on each AP after its CR4 is loaded.
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
    unsafe {
        Cr4::write(cr4 | Cr4Flags::OSXSAVE);
    }

    // Write XCR0 via `xsetbv` with ECX=0 — only XCR0 exists today.
    let mask = XSAVE_FEATURE_MASK;
    unsafe {
        core::arch::asm!(
            "xsetbv",
            in("ecx") 0u32,
            in("eax") mask as u32,
            in("edx") (mask >> 32) as u32,
            options(nomem, nostack, preserves_flags),
        );
    }

    OSXSAVE_ENABLED.store(true, Ordering::Release);
}

/// XSAVE area size (CPUID 0Dh.0.EBX) for the components currently enabled in
/// XCR0.  Re-runs CPUID every call — must be invoked after
/// [`enable_xsave_state`] for the value to reflect the 1.0 mask (x87+SSE+AVX
/// = 832 B).  Used by the boot-time validation assertion to confirm
/// [`XSAVE_AREA_SIZE`] fits the actually-enabled mask.
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
