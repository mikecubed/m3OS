//! Phase 57e Track J — CPUID-based detection of XSAVE/AVX state preservation.
//!
//! The kernel queries CPUID at BSP init to discover the XSAVE feature surface
//! (whether XSAVE is supported by the CPU, whether the OS is allowed to enable
//! it via CR4.OSXSAVE, the supported state-component bitmap, the maximum and
//! current XSAVE area sizes for those components, and whether `xsaveopt` is
//! available).  The result is stored in a `OnceCell` and consulted by:
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

/// Static XSAVE area size for the 1.0 mask.  Validated at boot against the
/// runtime CPUID-reported size in `XSaveFeatures::area_size` — if a future
/// CPUID change ever makes this too small, the kernel panics.
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
    let cr4 = Cr4::read();
    debug_assert!(
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

#[derive(Clone, Copy)]
struct CpuidRaw {
    eax: u32,
    ebx: u32,
    ecx: u32,
    edx: u32,
}

/// Execute `cpuid` with the given leaf and sub-leaf.
fn cpuid_raw(leaf: u32, sub_leaf: u32) -> CpuidRaw {
    let eax: u32;
    let ebx: u32;
    let ecx: u32;
    let edx: u32;
    unsafe {
        // Preserve rbx (LLVM reserves it under PIC).  Spill via a free
        // register, then restore.
        core::arch::asm!(
            "mov {rbx_save:r}, rbx",
            "cpuid",
            "mov {ebx_out:r}, rbx",
            "mov rbx, {rbx_save:r}",
            rbx_save = out(reg) _,
            ebx_out = out(reg) ebx,
            inout("eax") leaf => eax,
            inout("ecx") sub_leaf => ecx,
            out("edx") edx,
            options(nomem, nostack, preserves_flags),
        );
    }
    CpuidRaw { eax, ebx, ecx, edx }
}
