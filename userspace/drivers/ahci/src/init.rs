//! HBA generic-host-control bring-up — Phase 82 Track B.1 / B.2 / B.3.
//!
//! Enables AHCI mode (`GHC.AE`), performs the global `GHC.HR` HBA reset
//! (re-reading `CAP`/`PI`/`VS` after the reset self-clears and `AE` is
//! re-asserted, matching Linux `ahci_reset_controller` → `ahci_save_initial_config`
//! ordering), and runs the `CAP2.BOH` BIOS/OS handoff handshake on firmware
//! that still owns the HBA (a no-op on QEMU's `ich9-ahci`, which leaves
//! `CAP2.BOH = 0`).

use driver_runtime::Mmio;
use syscall_lib::STDOUT_FILENO;
use syscall_lib::write_str;

use kernel_core::storage::ahci::{
    BOHC_BB, BOHC_BOS, BOHC_OOS, CAP_S64A, CAP_SCLO, CAP_SSS, GHC_AE, GHC_HR, HBA_BOHC, HBA_CAP,
    HBA_CAP2, HBA_GHC, HBA_PI, HBA_VS, handoff_needed, ncs_from_cap,
};

use crate::MMIO_SPIN_BUDGET;

/// Phantom typestate marker so `Mmio<AhciAbar>` cannot be confused with another
/// driver's BAR window at compile time.
pub struct AhciAbar;

/// The HBA capabilities the driver depends on, read from `CAP`/`PI`/`VS`.
#[derive(Debug, Clone, Copy)]
pub struct HbaCaps {
    /// Number of command slots (`CAP.NCS + 1`). Bounds the slot allocator.
    pub ncs: u8,
    /// `CAP.S64A` — 64-bit addressing supported (the `*U` high-dword registers
    /// may carry the IOVA upper 32 bits).
    pub s64a: bool,
    /// `CAP.SSS` — staggered spin-up supported (`0` on QEMU).
    pub sss: bool,
    /// `CAP.SCLO` — command list override supported (clears a stuck BSY).
    pub sclo: bool,
    /// `PI` — Ports Implemented bitmap.
    pub pi: u32,
    /// `VS` — AHCI version (QEMU `ich9-ahci` reports `0x0001_0000`).
    pub version: u32,
}

/// Busy-spin a fixed number of iterations as a coarse delay. The ring-3 driver
/// has no timer; `iters` is tuned so the named call sites land in the
/// milliseconds range on every target.
#[inline]
fn spin_delay(iters: u64) {
    let mut i = 0u64;
    while i < iters {
        core::hint::spin_loop();
        i += 1;
    }
}

/// A "short delay" (~order of a millisecond on every target) used between
/// register-readback retries.
const SHORT_DELAY_ITERS: u64 = 50_000;

/// Set `GHC.AE` and confirm it reads back, retrying up to 5× with a short delay
/// (mirrors Linux `ahci_enable_ahci`). On QEMU `AE` is read-only-1 so the call
/// is idempotent.
pub fn enable_ahci(mmio: &Mmio<AhciAbar>) -> bool {
    for _ in 0..5 {
        let ghc = mmio.read_reg::<u32>(HBA_GHC);
        if ghc & GHC_AE != 0 {
            write_str(STDOUT_FILENO, "AHCI: GHC_AE confirmed\n");
            return true;
        }
        mmio.write_reg::<u32>(HBA_GHC, ghc | GHC_AE);
        spin_delay(SHORT_DELAY_ITERS);
    }
    // Final read-back check.
    if mmio.read_reg::<u32>(HBA_GHC) & GHC_AE != 0 {
        write_str(STDOUT_FILENO, "AHCI: GHC_AE confirmed\n");
        true
    } else {
        write_str(STDOUT_FILENO, "AHCI: GHC_AE could not be set\n");
        false
    }
}

/// Perform the global `GHC.HR` HBA reset. Sets `HR`, polls until it self-clears
/// to 0 within the bounded budget (the spec bounds the self-clear at 1 s), then
/// re-asserts `GHC.AE`. Returns `false` (controller dead) if the reset never
/// self-clears.
///
/// `CAP`/`PI`/`VS` are reloaded by the reset and must be read **after** this
/// returns, never before — see [`read_caps`].
pub fn reset_hba(mmio: &Mmio<AhciAbar>) -> bool {
    let ghc = mmio.read_reg::<u32>(HBA_GHC);
    mmio.write_reg::<u32>(HBA_GHC, ghc | GHC_HR);

    let mut i = 0u64;
    loop {
        if mmio.read_reg::<u32>(HBA_GHC) & GHC_HR == 0 {
            break;
        }
        if i >= MMIO_SPIN_BUDGET {
            write_str(
                STDOUT_FILENO,
                "AHCI: HBA reset did not complete within budget — controller dead\n",
            );
            return false;
        }
        core::hint::spin_loop();
        i += 1;
    }

    // The reset cleared AE; re-assert it before any port-register access.
    enable_ahci(mmio)
}

/// Read `CAP`/`PI`/`VS` and decode the fields the driver depends on. Must be
/// called **after** [`reset_hba`] re-asserts `AE`, because the reset reloads
/// these registers.
pub fn read_caps(mmio: &Mmio<AhciAbar>) -> HbaCaps {
    let cap = mmio.read_reg::<u32>(HBA_CAP);
    let pi = mmio.read_reg::<u32>(HBA_PI);
    let version = mmio.read_reg::<u32>(HBA_VS);

    let caps = HbaCaps {
        ncs: ncs_from_cap(cap),
        s64a: cap & CAP_S64A != 0,
        sss: cap & CAP_SSS != 0,
        sclo: cap & CAP_SCLO != 0,
        pi,
        version,
    };

    write_str(
        STDOUT_FILENO,
        &alloc::format!("AHCI: VS={:#010x} PI={:#010x}\n", version, pi),
    );
    write_str(
        STDOUT_FILENO,
        &alloc::format!(
            "AHCI: CAP.NCS={} S64A={}\n",
            caps.ncs,
            if caps.s64a { 1 } else { 0 }
        ),
    );
    caps
}

/// BIOS/OS handoff (Track B.3). Gated on `CAP2.BOH`: on firmware that still owns
/// the HBA, request ownership via the BOHC handshake (set `OOS`, poll `BOS` → 0,
/// extend the wait while `BB` is set). On QEMU `CAP2.BOH = 0`, so this logs a
/// skip and does nothing.
pub fn bios_os_handoff(mmio: &Mmio<AhciAbar>) {
    let cap2 = mmio.read_reg::<u32>(HBA_CAP2);
    if !handoff_needed(cap2) {
        write_str(STDOUT_FILENO, "bios/os handoff: skipped (CAP2.BOH=0)\n");
        return;
    }

    // --- bare-metal/VFIO-only path: QEMU never reaches here. ---
    let bohc = mmio.read_reg::<u32>(HBA_BOHC);
    mmio.write_reg::<u32>(HBA_BOHC, bohc | BOHC_OOS);

    // Allow ~25 ms for BIOS to release ownership; extend up to ~2 s if BB sets.
    let mut i = 0u64;
    let short_budget = MMIO_SPIN_BUDGET / 40; // ~25 ms relative to the 1 s budget
    loop {
        let b = mmio.read_reg::<u32>(HBA_BOHC);
        if b & BOHC_BOS == 0 && b & BOHC_BB == 0 {
            write_str(STDOUT_FILENO, "bios/os handoff: OS now owns the HBA\n");
            return;
        }
        let budget = if b & BOHC_BB != 0 {
            MMIO_SPIN_BUDGET * 2
        } else {
            short_budget
        };
        if i >= budget {
            write_str(
                STDOUT_FILENO,
                "bios/os handoff: timed out waiting for BIOS release\n",
            );
            return;
        }
        core::hint::spin_loop();
        i += 1;
    }
}
