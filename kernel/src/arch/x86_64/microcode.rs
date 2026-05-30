//! Phase 77 Track E — microcode loading.
//!
//! Applies a vendor microcode patch on every CPU at boot. The embedded blob is
//! an AMD `amd-ucode` container (the dev machine is `AuthenticAMD`); parsing is
//! done by the host-tested `kernel_core::microcode` pure-logic module, and the
//! application is the AMD `MSR_AMD64_PATCH_LOADER` (`0xC0010020`) write (of a
//! 16-byte-aligned copy of the patch payload) with the patch level read back
//! from `0x8B` and **verified** against the expected revision.
//!
//! **Safety / QEMU behaviour:** the patch-loader MSR is written ONLY when the
//! running CPU's signature matches an entry in the blob's equivalence table AND
//! the candidate revision is strictly newer than the current level. QEMU's
//! virtual CPU is not in the fam19h equivalence table, so the match fails and
//! the load is a clean skip (no MSR write) — the boot is unchanged. On a
//! non-AMD CPU the whole path is skipped before any MSR access. This keeps the
//! feature safe to run unconditionally at boot.

use x86_64::registers::model_specific::Msr;

/// AMD patch level (read) — also `IA32_BIOS_SIGN_ID` on Intel.
const MSR_AMD64_PATCH_LEVEL: u32 = 0x0000_008B;
/// AMD patch loader (write the patch virtual address to apply).
const MSR_AMD64_PATCH_LOADER: u32 = 0xC001_0020;

/// The embedded AMD microcode container (linux-firmware `microcode_amd_fam19h`).
/// Embedded at compile time so it is available in the BSP/AP bring-up window,
/// long before any filesystem is mounted.
static AMD_UCODE: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/initrd/lib/firmware/amd-ucode.bin"
));

/// Returns true when `CPUID.0:EBX/EDX/ECX` spell "AuthenticAMD".
fn cpu_vendor_is_amd() -> bool {
    // `__cpuid` is a safe intrinsic on x86_64 (leaf 0 is always valid).
    let r = core::arch::x86_64::__cpuid(0);
    r.ebx == 0x6874_7541 && r.edx == 0x6974_6e65 && r.ecx == 0x444d_4163
}

/// `CPUID.1:EAX` — the family/model/stepping signature (`installed_cpu`).
fn cpuid_signature() -> u32 {
    core::arch::x86_64::__cpuid(1).eax
}

/// Read the current microcode patch level (MSR `0x8B`).
fn read_patch_level() -> u32 {
    // SAFETY: 0x8B (IA32_BIOS_SIGN_ID / AMD patch level) is architectural and
    // readable on every x86_64 CPU; QEMU implements it.
    unsafe { Msr::new(MSR_AMD64_PATCH_LEVEL).read() as u32 }
}

/// Parse the embedded blob, and — if a newer patch matches this CPU — apply it
/// via the AMD patch-loader MSR. Logs the outcome on every CPU. Never writes an
/// MSR unless a strictly-newer matching patch is found (so QEMU and non-AMD
/// CPUs are a clean skip).
pub fn apply_microcode_on_cpu(cpu_id: u8) {
    if !cpu_vendor_is_amd() {
        log::info!("[ucode] CPU{cpu_id}: vendor not AuthenticAMD — microcode load skipped");
        return;
    }
    let sig = cpuid_signature();
    let current = read_patch_level();
    match kernel_core::microcode::find_applicable_amd_patch(AMD_UCODE, sig, current) {
        Some(patch) => {
            // The AMD patch loader takes the virtual address of the patch
            // payload, and the AMD hardware loader requires that address to be
            // **16-byte aligned**. The embedded blob is an `include_bytes!`
            // `&[u8]` (alignment 1), and the patch payload sits at a non-16-
            // aligned intra-blob offset (`20 + equiv_table_len`, i.e. ≡ 4 mod
            // 16), so `AMD_UCODE.as_ptr() + data_offset` is never suitably
            // aligned — passing it directly would make the load silently no-op
            // or `#GP` on real AMD silicon. Linux copies the patch out of the
            // container for the same reason; mirror that here with a 16-byte-
            // aligned scratch buffer (a `Vec<u128>` is 16-aligned on x86_64).
            let end = patch.data_offset + patch.data_len;
            let src = &AMD_UCODE[patch.data_offset..end];
            let words = patch.data_len.div_ceil(16).max(1);
            let mut aligned: alloc::vec::Vec<u128> = alloc::vec![0u128; words];
            // SAFETY: `aligned` owns `words * 16` bytes; we view them as `u8` to
            // copy the patch payload in. The buffer is 16-byte aligned because
            // `u128`'s alignment is 16.
            let aligned_bytes = unsafe {
                core::slice::from_raw_parts_mut(aligned.as_mut_ptr() as *mut u8, words * 16)
            };
            aligned_bytes[..patch.data_len].copy_from_slice(src);
            let patch_va = aligned.as_ptr() as u64;
            debug_assert_eq!(
                patch_va % 16,
                0,
                "AMD patch loader address must be 16-aligned"
            );
            // SAFETY: 0xC0010020 is the AMD patch-loader MSR; `patch_va` points
            // at a 16-byte-aligned copy of a validated, in-bounds patch payload,
            // and stays live until after this synchronous WRMSR consumes it.
            // Reached only on an exact equivalence + strictly-newer-revision
            // match, so it never fires on QEMU's virtual CPU.
            unsafe {
                Msr::new(MSR_AMD64_PATCH_LOADER).write(patch_va);
            }
            let new_level = read_patch_level();
            // Verify the apply actually took effect: on success MSR 0x8B now
            // reflects the patch revision. An unchanged level means the load was
            // rejected (e.g. address/format) — surface it instead of logging a
            // misleading "applied". `aligned` is dropped after this point.
            if new_level == patch.patch_id {
                log::info!(
                    "[ucode] CPU{cpu_id} sig={sig:#x}: applied patch {:#x} (level {current:#x} -> {new_level:#x})",
                    patch.patch_id
                );
            } else {
                log::warn!(
                    "[ucode] CPU{cpu_id} sig={sig:#x}: patch {:#x} apply did NOT take effect (level still {new_level:#x}, expected {:#x})",
                    patch.patch_id,
                    patch.patch_id
                );
            }
        }
        None => {
            log::info!(
                "[ucode] CPU{cpu_id} sig={sig:#x}: no newer microcode in blob (current level {current:#x}); skipped"
            );
        }
    }
}
