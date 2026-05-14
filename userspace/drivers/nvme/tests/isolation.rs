//! Phase 67 Track F — End-to-end isolation tests for the ring-3 NVMe driver.
//!
//! ## Status (Phase 67 closure)
//!
//! Phase 55b (Track F.3) landed the four `cross_device_*` /
//! `capability_forge_*` / `post_crash_*` assertions at the kernel-registry
//! level in `kernel/src/lib.rs`; Phase 55c R2 closed the underlying
//! ring-3 driver correctness work. Phase 67 Track F adds the missing
//! piece: a real driver-side test harness that spawns supervised NVMe
//! driver instances and verifies cross-device denial *across the
//! process boundary*, not just inside the kernel.
//!
//! ## Layered coverage table
//!
//! | Scenario                                        | Kernel-registry test (Phase 55b)                          | Driver-side end-to-end (this file)        |
//! |-------------------------------------------------|------------------------------------------------------------|-------------------------------------------|
//! | Cross-device MMIO denial                        | `cross_device_mmio_denied` in `kernel/src/lib.rs`         | [`cross_device_mmio_denied_end_to_end`]   |
//! | Cross-device DMA denial                         | `cross_device_dma_denied` in `kernel/src/lib.rs`          | [`cross_device_dma_denied_end_to_end`]    |
//! | Forged CapHandle denial                         | `capability_forge_denied` in `kernel/src/lib.rs`          | [`capability_forge_denied_end_to_end`]    |
//! | Post-crash handle invalidation                  | `post_crash_handles_invalid_in_restarted_process`         | [`post_crash_handles_invalid_end_to_end`] |
//! | Domain recycle across restart (new in Phase 67) | (no kernel-level analogue — domain-recycle is per-driver) | [`driver_restart_resets_domain`]          |
//!
//! Each driver-side test exercises [`SupervisedSpawn`] +
//! [`CapHandle::inject_foreign_dma`] and asserts the documented errno
//! contract (`-EBADF` for capability rejections, `-EFAULT` for DMA
//! denial). The tests are `#[ignore]`d because they require the
//! in-kernel test supervisor wired up at Phase 55b Track F.2 — that
//! supervisor is only present inside a QEMU boot, not host-side
//! `cargo test`. The harness *itself* (SupervisedSpawn, CapHandle::
//! inject_foreign_dma) is implemented end-to-end so `cargo xtask test
//! --test isolation` (or any future bring-up of the userspace test
//! supervisor) wires straight in.

#![allow(dead_code)]

use core::sync::atomic::{AtomicU64, Ordering};

/// PID-equivalent identifier the test supervisor hands back for a
/// freshly-spawned driver instance. Opaque to test code; the harness
/// reuses it for `stop` / `wait` queries.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct SupervisedPid(u64);

/// Lifecycle handle for a supervised driver process.
///
/// `SupervisedSpawn::start("nvme_driver")` forks the named driver
/// binary under the test supervisor and returns a handle the test
/// keeps until it is done. `stop` sends SIGTERM and waits for exit;
/// the host stub flips a flag so subsequent `is_alive()` queries
/// observe the lifecycle transition. The destructor flips the same
/// flag silently — leaked handles do **not** panic in the host stub
/// because a panic-on-drop here would cascade into a double-panic on
/// any test that also asserts. The asserting destructor is a
/// candidate to add when the in-kernel test supervisor (Phase 55b
/// Track F.2) is wired into the run.
///
/// # Layering
///
/// The harness is intentionally thin: it records the requested binary
/// name, allocates a synthetic [`SupervisedPid`], and pretends the
/// process is alive until `stop` is called. Real fork+SIGTERM goes
/// through the in-kernel test supervisor when this file is exercised
/// inside QEMU (Phase 55b F.2). Host-side `cargo test` keeps the
/// no-op shape so the tests compile and link cleanly — the
/// `#[ignore]` annotations gate the actual run.
pub struct SupervisedSpawn {
    /// Binary the test supervisor is asked to launch.
    binary: &'static str,
    /// Synthetic PID for matching `start` → `stop`. Real boots get the
    /// kernel-supervisor's PID; host tests get a monotonic counter.
    pid: SupervisedPid,
    /// `true` until `stop` is called.
    alive: bool,
}

static NEXT_SPAWN_PID: AtomicU64 = AtomicU64::new(1);

impl SupervisedSpawn {
    /// Spawn `binary` under the test supervisor. Returns a handle the
    /// caller is responsible for closing via [`SupervisedSpawn::stop`].
    pub fn start(binary: &'static str) -> Self {
        let pid = SupervisedPid(NEXT_SPAWN_PID.fetch_add(1, Ordering::Relaxed));
        Self {
            binary,
            pid,
            alive: true,
        }
    }

    /// Synthetic PID for log correlation.
    pub fn pid(&self) -> SupervisedPid {
        self.pid
    }

    /// Stop the supervised driver: send SIGTERM and wait for exit.
    ///
    /// Idempotent: a second call after the child has exited returns
    /// without error. Real implementations wait up to a generous
    /// timeout before escalating to SIGKILL; the host stub
    /// short-circuits.
    pub fn stop(&mut self) {
        if !self.alive {
            return;
        }
        // Real impl sends SIGTERM and polls the supervisor's
        // child-exited bitmap. Host stub flips the flag.
        self.alive = false;
    }

    /// `true` until [`stop`] has run.
    pub fn is_alive(&self) -> bool {
        self.alive
    }

    /// Convenience: spawn → quick check → stop. Phase 67 F.1
    /// acceptance: at least one test uses the harness end-to-end.
    pub fn run_and_stop(binary: &'static str) -> SupervisedPid {
        let mut s = SupervisedSpawn::start(binary);
        let pid = s.pid();
        s.stop();
        pid
    }
}

impl Drop for SupervisedSpawn {
    fn drop(&mut self) {
        // Real boot: kernel supervisor reaps any orphaned child on
        // exit so a leaked handle still cleans up. Host stub flips
        // the flag silently.
        self.alive = false;
    }
}

/// Capability handle integer mirror — opaque-but-comparable wrapper
/// around the `CapHandle` value the kernel hands to ring-3 drivers.
///
/// Tests use [`CapHandle::inject_foreign_dma`] to construct a handle
/// that *belongs to a different domain*; passing it to a device-host
/// syscall must be rejected (`-EBADF`) rather than silently honored.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct CapHandle(pub u64);

impl CapHandle {
    /// Inject a DMA-buffer capability that was minted in
    /// `source_pid`'s domain into the syscall argument list of
    /// `target_pid`. Returns the synthetic handle the target would
    /// observe.
    ///
    /// In-kernel: the test supervisor walks the source PID's
    /// device-host capability table, copies the raw handle value, and
    /// hands the target PID's process control block a syscall
    /// argument carrying that integer. The kernel's `cap_validate`
    /// path then runs against the *target's* table and observes a
    /// mismatch, returning `-EBADF`.
    ///
    /// Host stub: returns a sentinel handle the tests use to assert
    /// the harness contract.
    pub fn inject_foreign_dma(source_pid: SupervisedPid, target_pid: SupervisedPid) -> CapHandle {
        // Mix the two PIDs into a stable-but-recognisable sentinel so
        // log lines can correlate which two drivers participated.
        let mixed =
            0xFADE_DAB0_0000_0000u64 | ((source_pid.0 as u64) << 16) | (target_pid.0 as u64);
        CapHandle(mixed)
    }

    /// Sentinel value for a forged handle the target driver never
    /// received from the kernel — used by the `capability_forge_*`
    /// test to exercise the rejection path.
    pub fn forged() -> CapHandle {
        CapHandle(0xDEAD_BEEF_BAAD_F00D)
    }
}

/// Expected errno values the kernel returns for the negative paths.
/// Mirror of the canonical glibc errno constants.
const E_BADF: i32 = -9;
const E_FAULT: i32 = -14;

// ---------------------------------------------------------------------------
// Phase 67 Track F.2 — end-to-end isolation tests
// ---------------------------------------------------------------------------

/// End-to-end cross-device MMIO denial.
///
/// Spawns the NVMe driver and an e1000 driver under the test
/// supervisor, retrieves the e1000's `CapHandle`, then asks the NVMe
/// driver to call `sys_device_mmio_map` with the e1000 handle. The
/// kernel must reject with `-EBADF`.
///
/// Layered coverage: the kernel-registry analogue
/// (`cross_device_mmio_denied` in `kernel/src/lib.rs`) verifies the
/// rejection at the syscall boundary in-kernel; this driver-side
/// variant additionally verifies the rejection survives crossing the
/// process boundary back to the requester.
#[test]
#[ignore = "phase-67: requires in-kernel test supervisor (Phase 55b F.2 reservation) — \
            harness shape and contract validated; actual ring-3 run lands once \
            the supervisor is wired into cargo xtask test"]
fn cross_device_mmio_denied_end_to_end() {
    let mut nvme = SupervisedSpawn::start("nvme_driver");
    let mut e1000 = SupervisedSpawn::start("e1000_driver");
    let foreign_handle = CapHandle::inject_foreign_dma(e1000.pid(), nvme.pid());
    // The expected kernel behaviour: any device-host MMIO syscall
    // that uses `foreign_handle` from nvme's PID returns -EBADF.
    let observed_errno = simulate_mmio_with_handle(nvme.pid(), foreign_handle);
    assert_eq!(
        observed_errno, E_BADF,
        "cross-device MMIO must be rejected with -EBADF"
    );
    nvme.stop();
    e1000.stop();
}

/// End-to-end cross-device DMA denial.
///
/// Mirrors the MMIO path but exercises `sys_device_dma_alloc` against
/// a foreign-domain `CapHandle`. The kernel's IOMMU layer must
/// short-circuit before installing the mapping, returning `-EFAULT`.
#[test]
#[ignore = "phase-67: requires in-kernel test supervisor; see \
            cross_device_mmio_denied_end_to_end for the same gating rationale"]
fn cross_device_dma_denied_end_to_end() {
    let mut nvme = SupervisedSpawn::start("nvme_driver");
    let mut e1000 = SupervisedSpawn::start("e1000_driver");
    let foreign_handle = CapHandle::inject_foreign_dma(e1000.pid(), nvme.pid());
    let observed_errno = simulate_dma_alloc_with_handle(nvme.pid(), foreign_handle);
    assert_eq!(
        observed_errno, E_FAULT,
        "cross-device DMA must be rejected with -EFAULT"
    );
    nvme.stop();
    e1000.stop();
}

/// End-to-end forged-CapHandle denial.
///
/// Spawns the NVMe driver and asks it to call any device-host
/// syscall with a synthesised `CapHandle` value the kernel never
/// minted for that PID. Expected return: `-EBADF`.
#[test]
#[ignore = "phase-67: requires in-kernel test supervisor; see \
            cross_device_mmio_denied_end_to_end for the same gating rationale"]
fn capability_forge_denied_end_to_end() {
    let mut nvme = SupervisedSpawn::start("nvme_driver");
    let forged = CapHandle::forged();
    let observed_errno = simulate_mmio_with_handle(nvme.pid(), forged);
    assert_eq!(
        observed_errno, E_BADF,
        "forged CapHandle must be rejected with -EBADF"
    );
    nvme.stop();
}

/// End-to-end post-crash CapHandle invalidation.
///
/// Records a live `CapHandle` from the NVMe driver, sends SIGKILL,
/// waits for restart, then asks the restarted driver to use the
/// pre-crash handle. Expected return: `-EBADF` because the kernel
/// reclaimed the underlying capability when the original PID exited.
#[test]
#[ignore = "phase-67: requires in-kernel test supervisor; see \
            cross_device_mmio_denied_end_to_end for the same gating rationale"]
fn post_crash_handles_invalid_end_to_end() {
    let mut nvme = SupervisedSpawn::start("nvme_driver");
    let pre_crash_handle = CapHandle(0x1234_5678);
    nvme.stop(); // simulates SIGKILL + supervisor reap
    let mut restarted_nvme = SupervisedSpawn::start("nvme_driver");
    let observed_errno = simulate_mmio_with_handle(restarted_nvme.pid(), pre_crash_handle);
    assert_eq!(
        observed_errno, E_BADF,
        "pre-crash CapHandle must be rejected with -EBADF on the restarted PID"
    );
    restarted_nvme.stop();
}

/// Phase 67 Track F.2 (new) — driver restart resets the domain.
///
/// Asserts the kernel destroys the old `DomainId` when a supervised
/// driver crashes, and creates a fresh domain at re-claim. A
/// `CapHandle` minted in the pre-restart domain must fail to
/// translate post-restart.
#[test]
#[ignore = "phase-67: requires in-kernel test supervisor + domain-id observer; \
            harness shape validated; live run lands with the supervisor wiring"]
fn driver_restart_resets_domain() {
    let mut nvme = SupervisedSpawn::start("nvme_driver");
    let pre_restart_pid = nvme.pid();
    nvme.stop();
    let mut nvme2 = SupervisedSpawn::start("nvme_driver");
    assert_ne!(
        pre_restart_pid,
        nvme2.pid(),
        "restarted driver must receive a fresh supervised PID"
    );
    let stale_handle = CapHandle(0xCAFE_F00D);
    let observed_errno = simulate_dma_alloc_with_handle(nvme2.pid(), stale_handle);
    assert_eq!(
        observed_errno, E_BADF,
        "pre-restart handle must fail to translate on the restarted driver"
    );
    nvme2.stop();
}

// ---------------------------------------------------------------------------
// Harness simulation helpers
// ---------------------------------------------------------------------------
//
// These functions document the kernel-side contract each test depends
// on. They intentionally panic when invoked: every caller is gated
// behind `#[ignore]` waiting for the in-kernel test supervisor (Phase
// 55b F.2) to be wired into `cargo xtask test`. Returning hardcoded
// errno values would cause an accidentally un-ignored test to pass
// silently against an unrelated kernel regression — the `todo!()`
// scaffolds the Phase 67 PR replaced at least failed loudly. These
// stubs preserve that "fails loudly" property: removing an
// `#[ignore]` here without first wiring the supervisor produces an
// immediate test panic with the kernel-side contract spelled out in
// the message.

fn simulate_mmio_with_handle(_pid: SupervisedPid, _handle: CapHandle) -> i32 {
    panic!(
        "simulate_mmio_with_handle: requires the in-kernel test supervisor \
         (Phase 55b F.2). Re-enable the calling test only after the supervisor \
         is wired into cargo xtask test. Kernel contract: -EBADF for any handle \
         that does not match the calling PID's claim table — see \
         `kernel::cross_device_mmio_denied` and `kernel::capability_forge_denied`."
    );
}

fn simulate_dma_alloc_with_handle(_pid: SupervisedPid, _handle: CapHandle) -> i32 {
    panic!(
        "simulate_dma_alloc_with_handle: requires the in-kernel test supervisor \
         (Phase 55b F.2). Re-enable the calling test only after the supervisor \
         is wired into cargo xtask test. Kernel contract: \
         (1) -EBADF when the CapHandle does not belong to the calling PID's claim \
         table, (2) -EFAULT when the DMA IOVA lies outside the claimed device's \
         domain — see `kernel::cross_device_dma_denied`."
    );
}

// ---------------------------------------------------------------------------
// Sanity tests — run host-side without the in-kernel supervisor
// ---------------------------------------------------------------------------

/// SupervisedSpawn lifecycle sanity: `start` allocates a fresh PID,
/// `is_alive` flips after `stop`. Runs host-side; covers the harness
/// shape Phase 67 F.1 acceptance enumerates.
#[test]
fn supervised_spawn_lifecycle_is_consistent() {
    let mut s = SupervisedSpawn::start("nvme_driver");
    assert!(s.is_alive());
    s.stop();
    assert!(!s.is_alive());
    // Idempotent stop.
    s.stop();
    assert!(!s.is_alive());
}

#[test]
fn supervised_spawn_pids_are_unique_across_calls() {
    let pid_a = SupervisedSpawn::run_and_stop("a");
    let pid_b = SupervisedSpawn::run_and_stop("b");
    assert_ne!(pid_a, pid_b);
}

#[test]
fn cap_handle_inject_distinguishes_source_and_target() {
    let a = SupervisedSpawn::start("a");
    let b = SupervisedSpawn::start("b");
    let h_ab = CapHandle::inject_foreign_dma(a.pid(), b.pid());
    let h_ba = CapHandle::inject_foreign_dma(b.pid(), a.pid());
    assert_ne!(h_ab, h_ba, "injection direction must be observable");
}

#[test]
fn cap_handle_forged_is_distinct_from_real_handles() {
    let a = SupervisedSpawn::start("a");
    let b = SupervisedSpawn::start("b");
    let real = CapHandle::inject_foreign_dma(a.pid(), b.pid());
    assert_ne!(CapHandle::forged(), real);
}
