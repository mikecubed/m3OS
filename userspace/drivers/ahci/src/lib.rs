//! `ahci_driver` — Phase 82 ring-3 AHCI/SATA storage hardware driver.
//!
//! Out-of-process driver serving the `driver_ipc::block` protocol on the
//! `"ahci.block"` service, exactly like `userspace/drivers/nvme` serves
//! `"nvme.block"`. It owns the AHCI Host Bus Adapter (ABAR = BAR5 MMIO), brings
//! up each implemented port through the spec-mandated stop/start engine
//! ordering, allocates the per-port command list + received-FIS area + command
//! table as IOMMU-routed `DmaBuffer<T>` (programming the **IOVA**, never
//! host-physical, into every HBA register), issues `IDENTIFY` / `READ DMA EXT`
//! / `WRITE DMA EXT` / `FLUSH CACHE EXT`, and presents upward as a
//! `RemoteBlockDevice`.
//!
//! The host-testable pure logic (register/struct/FIS/PRDT/slot/classifier)
//! lives in [`kernel_core::storage`]; this crate's modules are the production
//! register-poking layer. The few host-testable driver-side helpers (request
//! sizing, the issue/await completion decision built over the
//! `kernel_core::storage::ahci` predicates) live here and are exercised by the
//! test module at the bottom.

#![cfg_attr(not(test), no_std)]

extern crate alloc;
#[cfg(test)]
extern crate std;

// Driver-internal production modules (register-poking). Gated off the host-test
// build because they depend on `driver_runtime`/`syscall_lib` syscall surfaces.
#[cfg(not(test))]
pub mod cmd;
#[cfg(not(test))]
pub mod init;
#[cfg(not(test))]
pub mod io;
#[cfg(not(test))]
pub mod port;

use kernel_core::driver_ipc::block::MAX_SECTORS_PER_REQUEST;
use kernel_core::storage::ahci::{cmd_complete, find_free_slot, is_fatal};

/// Service name the driver registers and the kernel `blk::remote` cold-path
/// lookup resolves (Phase 82 D.2). Pinning it here keeps the registration side
/// and the kernel lookup referring to the same string.
pub const SERVICE_NAME: &str = "ahci.block";

/// Boot-log marker written when the driver starts.
pub const BOOT_LOG_MARKER: &str = "ahci_driver: spawned\n";

/// Sentinel emitted once the HBA + port are up and the block server loop is
/// about to start. The `ahci-smoke` gate waits for this line.
pub const SERVER_READY_SENTINEL: &str = "AHCI_SMOKE:server:READY\n";

/// ABAR (BAR5) MMIO window length. The AHCI register file is the generic host
/// control block (0x100) plus 32 port register blocks of 0x80 each
/// (`0x100 + 32 * 0x80 = 0x1100`); 0x2000 covers it with headroom.
pub const AHCI_ABAR_LEN: usize = 0x2000;

/// Default logical sector size assumed before IDENTIFY refines it. QEMU
/// `ide-hd` and every target we ship against default to 512 B.
pub const DEFAULT_SECTOR_BYTES: u32 = 512;

/// Bytes the per-port data bounce `DmaBuffer` holds: `MAX_SECTORS_PER_REQUEST`
/// (256) × 512 = 128 KiB. A single PRDT entry's 4 MiB `DBC` ceiling covers it,
/// so each command needs exactly one PRDT entry.
pub const DATA_BOUNCE_BYTES: usize = MAX_SECTORS_PER_REQUEST as usize * 512;

/// Hard upper bound on every MMIO completion / status polling spin. The ring-3
/// driver has no timer subsystem of its own, so bounds are in iterations — 8 M
/// spins is empirically multiple seconds and keeps a wedged controller from
/// stalling the driver past the service-manager restart window. Mirrors the
/// NVMe driver's `MMIO_SPIN_BUDGET`.
pub const MMIO_SPIN_BUDGET: u64 = 8_000_000;

/// Command FIS Length in dwords for the H2D Register FIS (20 bytes = 5 dwords).
pub const CFL_DWORDS: u8 = 5;

/// Busy-spin a fixed number of iterations as a coarse delay. The ring-3 driver
/// has no timer; call sites pass an iteration count tuned to land in the
/// milliseconds range on every target. Lives in `lib.rs` so it is available to
/// both the production modules and (trivially) the host-test build.
#[inline]
pub fn init_spin(iters: u64) {
    let mut i = 0u64;
    while i < iters {
        core::hint::spin_loop();
        i += 1;
    }
}

// ---------------------------------------------------------------------------
// Host-testable driver-side decision logic (reuses kernel_core predicates).
// ---------------------------------------------------------------------------

/// `true` when a block request's sector count exceeds the single-command cap.
/// The kernel-side facade is the first line of defence, but a compliant driver
/// rejects an oversized request rather than issuing one giant command.
#[inline]
pub fn request_is_oversized(sector_count: u32) -> bool {
    sector_count > MAX_SECTORS_PER_REQUEST
}

/// The outcome of polling a single in-flight command slot — the C.1 issue/reap
/// decision, expressed over the host-tested A.5 predicates so it is provable
/// without QEMU.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmdOutcome {
    /// The slot's `PxCI` bit auto-cleared with no error — success.
    Complete,
    /// The slot is still in flight (`PxCI` bit set, no error).
    Pending,
    /// A fatal `PxIS` error latched — route to recovery (C.4).
    Failed,
}

/// Decide whether a slot may be issued: only when [`find_free_slot`] returns it
/// (the slot is clear in `PxSACT | PxCI` and within `ncs`).
#[inline]
pub fn pick_slot(sact: u32, ci: u32, ncs: u8) -> Option<u8> {
    find_free_slot(sact, ci, ncs)
}

/// Classify a single in-flight slot's completion state from `(PxCI, PxIS)`.
/// A fatal error wins over a clear `PxCI` bit so a failed command is never
/// reported as success.
#[inline]
pub fn poll_outcome(ci: u32, slot: u8, is: u32) -> CmdOutcome {
    if is_fatal(is) {
        CmdOutcome::Failed
    } else if cmd_complete(ci, slot, is) {
        CmdOutcome::Complete
    } else {
        CmdOutcome::Pending
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kernel_core::storage::ahci::{IS_DHRS, PX_IS_TFES};

    #[test]
    fn request_oversize_gate() {
        assert!(!request_is_oversized(1));
        assert!(!request_is_oversized(MAX_SECTORS_PER_REQUEST));
        assert!(request_is_oversized(MAX_SECTORS_PER_REQUEST + 1));
        assert!(request_is_oversized(u32::MAX));
    }

    /// C.1: a slot is issued only when free, and `poll_outcome` returns
    /// `Complete` only when the slot's `PxCI` bit is clear with no error; a
    /// `PxIS.TFES` error makes it `Failed`.
    #[test]
    fn issue_then_complete() {
        // Idle controller: slot 0 is the pick.
        let slot = pick_slot(0, 0, 32).expect("a free slot must exist when idle");
        assert_eq!(slot, 0);

        // Just issued: PxCI bit set → Pending.
        let ci_issued = 1u32 << slot;
        assert_eq!(poll_outcome(ci_issued, slot, 0), CmdOutcome::Pending);

        // Completed cleanly: PxCI bit cleared, a benign completion bit set.
        assert_eq!(poll_outcome(0, slot, IS_DHRS), CmdOutcome::Complete);

        // Task-file error latched (even with PxCI clear) → Failed.
        assert_eq!(poll_outcome(0, slot, PX_IS_TFES), CmdOutcome::Failed);
        // Error while still in flight is also Failed.
        assert_eq!(
            poll_outcome(ci_issued, slot, PX_IS_TFES),
            CmdOutcome::Failed
        );
    }

    #[test]
    fn no_free_slot_when_all_busy() {
        assert_eq!(pick_slot(0xFFFF_FFFF, 0, 32), None);
    }
}
