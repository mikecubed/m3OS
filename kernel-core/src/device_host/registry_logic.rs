// Pure-logic `DeviceHostRegistryCore` — Phase 55b Track B.1.
//
// The kernel-side syscall wrapper (`kernel/src/syscall/device_host.rs`) wraps
// this registry with the syscall entry point, capability-table plumbing, and
// the PCI-claim handoff. Keeping the core state machine here in
// `kernel-core` makes every invariant the host-test suite pins — double
// claim, release-on-exit, concurrent-claim — run without a booted kernel
// and forces the kernel-side to stay thin.
//
// TDD red commit: the type exists with stub bodies that compile but fail
// the invariants. The green commit wires it up.
//
// ## Phase 57b G.9 — preempt-discipline classification
//
// Per the Track A.1 spinlock callsite audit
// (`docs/handoffs/57b-spinlock-callsite-audit.md`, row for
// `kernel-core/src/device_host/registry_logic.rs`), this module declares
// **no lock of its own** — `DeviceHostRegistryCore` is a plain `Vec`-backed
// state machine. The audit classifies the row as `host-test-only` because
// the kernel-side wrapper that holds it (`DEVICE_HOST_REGISTRY` in
// `kernel/src/syscall/device_host.rs`) IS the lock surface, and Phase 57b
// Track G.6.b already migrated that wrapper to
// `IrqSafeMutex<DeviceHostRegistry>`. Track F's preempt-discipline therefore
// covers every kernel-build acquisition of this type by construction — no
// `kernel-core` change is required for G.9.
//
// Host tests in this file (`#[cfg(test)] mod tests` below) drive the
// registry single-threaded with no lock at all, exactly as the
// `host-test-only` classification expects.

extern crate alloc;

use alloc::vec::Vec;

use super::types::{DeviceCapKey, DeviceHostError};

/// Process identifier the kernel uses when it records a claim.
///
/// Declared as a dedicated alias so the kernel-side wrapper can feed it
/// `crate::process::Pid` without `kernel-core` pulling in any of the
/// kernel's process-table machinery. Both ends are `u32`.
pub type RegistryPid = u32;

/// Errors returned by the pure-logic registry.
///
/// These mirror (a subset of) [`DeviceHostError`] for the paths the registry
/// knows about; the full error surface is reported at the syscall boundary.
/// `#[non_exhaustive]` so later tracks may add variants (e.g. capacity
/// caps) without forcing a match update in every test.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum RegistryError {
    /// Another PID has already claimed this BDF.
    AlreadyClaimed,
    /// The BDF is not currently claimed — used by `release` when the caller
    /// has no record.
    NotClaimed,
    /// The caller PID does not match the recorded owner. Release must be
    /// routed through the owning process.
    WrongOwner,
}

impl From<RegistryError> for DeviceHostError {
    fn from(e: RegistryError) -> Self {
        match e {
            RegistryError::AlreadyClaimed => DeviceHostError::AlreadyClaimed,
            RegistryError::NotClaimed => DeviceHostError::NotClaimed,
            // A wrong-owner release is a capability bug at the caller — in
            // the syscall boundary it surfaces as `NotClaimed` because the
            // caller's `Capability::Device` should never name a device it
            // doesn't own. The conversion is kept for diagnostic completeness.
            RegistryError::WrongOwner => DeviceHostError::NotClaimed,
        }
    }
}

/// Pure-logic backing store for the kernel-side `DeviceHostRegistry`.
///
/// Holds one entry per claimed `(PID, DeviceCapKey)` pair. The kernel-side
/// wrapper combines this with `spin::Mutex` and the PCI registry handoff;
/// the pure-logic version is lock-free by construction (host tests drive it
/// single-threaded with explicit race modeling).
#[derive(Default)]
pub struct DeviceHostRegistryCore {
    entries: Vec<ClaimEntry>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ClaimEntry {
    pid: RegistryPid,
    key: DeviceCapKey,
}

impl DeviceHostRegistryCore {
    /// Construct an empty registry.
    pub const fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Try to record a new claim on `key` for `pid`.
    ///
    /// Returns `Ok(())` on success, `Err(RegistryError::AlreadyClaimed)` if
    /// `key` is already claimed by *any* process (including `pid` itself —
    /// double-claim is rejected so a buggy driver cannot burn through the
    /// claim count).
    ///
    /// The kernel-side wrapper holds a `spin::Mutex` across this call plus
    /// the downstream PCI `claim_specific` handoff so the (registry, PCI)
    /// pair updates atomically from the scheduler's view.
    pub fn try_claim(&mut self, pid: RegistryPid, key: DeviceCapKey) -> Result<(), RegistryError> {
        if self.entries.iter().any(|e| e.key == key) {
            return Err(RegistryError::AlreadyClaimed);
        }
        self.entries.push(ClaimEntry { pid, key });
        Ok(())
    }

    /// Release the claim on `key` held by `pid`.
    ///
    /// Returns `Ok(())` if the entry was present and removed, or
    /// `Err(RegistryError::NotClaimed)` if no entry matches the BDF,
    /// or `Err(RegistryError::WrongOwner)` if an entry exists but under
    /// a different PID.
    pub fn release(&mut self, pid: RegistryPid, key: DeviceCapKey) -> Result<(), RegistryError> {
        let pos = self.entries.iter().position(|e| e.key == key);
        match pos {
            None => Err(RegistryError::NotClaimed),
            Some(i) if self.entries[i].pid != pid => Err(RegistryError::WrongOwner),
            Some(i) => {
                self.entries.swap_remove(i);
                Ok(())
            }
        }
    }

    /// Release every claim owned by `pid`.
    ///
    /// Called from the process-exit path so a driver crash or kill
    /// automatically frees the devices for the supervisor restart to re-claim.
    /// Returns the list of freed keys so the caller can log the release.
    pub fn release_for_pid(&mut self, pid: RegistryPid) -> Vec<DeviceCapKey> {
        let mut freed = Vec::new();
        self.entries.retain(|e| {
            if e.pid == pid {
                freed.push(e.key);
                false
            } else {
                true
            }
        });
        freed
    }

    /// Return the current owner of `key`, if any.
    pub fn owner_of(&self, key: DeviceCapKey) -> Option<RegistryPid> {
        self.entries.iter().find(|e| e.key == key).map(|e| e.pid)
    }

    /// Number of active claim entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the registry currently has no claims.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BDF_A: DeviceCapKey = DeviceCapKey::new(0, 0x00, 0x03, 0);
    const BDF_B: DeviceCapKey = DeviceCapKey::new(0, 0x00, 0x04, 0);

    // ---- First claim succeeds; duplicate claim fails ------------------

    #[test]
    fn first_claim_succeeds() {
        let mut reg = DeviceHostRegistryCore::new();
        assert_eq!(reg.try_claim(100, BDF_A), Ok(()));
        assert_eq!(reg.owner_of(BDF_A), Some(100));
        assert_eq!(reg.len(), 1);
    }

    #[test]
    fn second_claim_on_same_bdf_by_same_pid_returns_already_claimed() {
        // The registry is concerned with ownership, not with idempotency —
        // a driver that re-enters its own init path indicates a bug in the
        // driver, not a fresh claim. Reject it the same way as a
        // cross-process race.
        let mut reg = DeviceHostRegistryCore::new();
        reg.try_claim(100, BDF_A).expect("first claim succeeds");
        assert_eq!(
            reg.try_claim(100, BDF_A),
            Err(RegistryError::AlreadyClaimed),
        );
    }

    #[test]
    fn second_claim_on_same_bdf_by_different_pid_returns_already_claimed() {
        let mut reg = DeviceHostRegistryCore::new();
        reg.try_claim(100, BDF_A).expect("first claim succeeds");
        assert_eq!(
            reg.try_claim(200, BDF_A),
            Err(RegistryError::AlreadyClaimed),
        );
        // Original owner is unchanged.
        assert_eq!(reg.owner_of(BDF_A), Some(100));
    }

    #[test]
    fn distinct_bdfs_are_independently_claimable() {
        let mut reg = DeviceHostRegistryCore::new();
        reg.try_claim(100, BDF_A).expect("A claims BDF_A");
        reg.try_claim(200, BDF_B).expect("B claims BDF_B");
        assert_eq!(reg.owner_of(BDF_A), Some(100));
        assert_eq!(reg.owner_of(BDF_B), Some(200));
        assert_eq!(reg.len(), 2);
    }

    // ---- Release frees the slot for re-claim --------------------------

    #[test]
    fn release_frees_the_bdf_for_reclaim() {
        let mut reg = DeviceHostRegistryCore::new();
        reg.try_claim(100, BDF_A).expect("first claim succeeds");
        reg.release(100, BDF_A).expect("owner releases");
        assert_eq!(reg.owner_of(BDF_A), None);
        // A different process can now claim it.
        reg.try_claim(200, BDF_A)
            .expect("second PID claims after release");
        assert_eq!(reg.owner_of(BDF_A), Some(200));
    }

    #[test]
    fn release_of_unclaimed_bdf_returns_not_claimed() {
        let mut reg = DeviceHostRegistryCore::new();
        assert_eq!(reg.release(100, BDF_A), Err(RegistryError::NotClaimed),);
    }

    #[test]
    fn release_by_non_owner_returns_not_claimed_or_wrong_owner() {
        let mut reg = DeviceHostRegistryCore::new();
        reg.try_claim(100, BDF_A).expect("claim");
        // A non-owner cannot release someone else's claim; the owner's
        // claim must remain intact.
        let err = reg
            .release(200, BDF_A)
            .expect_err("non-owner release fails");
        assert!(matches!(
            err,
            RegistryError::WrongOwner | RegistryError::NotClaimed
        ));
        assert_eq!(
            reg.owner_of(BDF_A),
            Some(100),
            "owner's claim survives the bogus release",
        );
    }

    // ---- Double-release is safe ---------------------------------------

    #[test]
    fn double_release_is_safe_and_returns_not_claimed() {
        let mut reg = DeviceHostRegistryCore::new();
        reg.try_claim(100, BDF_A).expect("claim");
        reg.release(100, BDF_A).expect("first release");
        // Second release of the same (pid, key) must be a typed error —
        // not a panic, not UB. B.1 acceptance: "releasing a
        // Capability::Device twice returns -EBADF, not panic".
        assert_eq!(reg.release(100, BDF_A), Err(RegistryError::NotClaimed),);
    }

    // ---- release_for_pid (process exit path) --------------------------

    #[test]
    fn release_for_pid_frees_every_claim_owned_by_that_pid() {
        let mut reg = DeviceHostRegistryCore::new();
        reg.try_claim(100, BDF_A).unwrap();
        reg.try_claim(100, BDF_B).unwrap();
        reg.try_claim(200, DeviceCapKey::new(0, 0, 5, 0)).unwrap();

        let freed = reg.release_for_pid(100);
        // Both A and B are freed; the other PID's claim is untouched.
        assert_eq!(freed.len(), 2);
        assert!(freed.contains(&BDF_A));
        assert!(freed.contains(&BDF_B));
        assert_eq!(reg.owner_of(BDF_A), None);
        assert_eq!(reg.owner_of(BDF_B), None);
        assert_eq!(
            reg.owner_of(DeviceCapKey::new(0, 0, 5, 0)),
            Some(200),
            "other PID's claim survives",
        );
    }

    #[test]
    fn release_for_pid_with_no_claims_returns_empty_list() {
        let mut reg = DeviceHostRegistryCore::new();
        reg.try_claim(100, BDF_A).unwrap();
        let freed = reg.release_for_pid(999);
        assert!(freed.is_empty());
        // The other PID's claim is untouched.
        assert_eq!(reg.owner_of(BDF_A), Some(100));
    }

    // ---- Claim-release-reclaim cycle (supervisor restart) -------------

    #[test]
    fn supervisor_restart_cycle_pid_a_claims_exits_pid_b_reclaims() {
        let mut reg = DeviceHostRegistryCore::new();
        reg.try_claim(100, BDF_A).expect("initial claim");

        // PID 100 exits — simulate via release_for_pid.
        let freed = reg.release_for_pid(100);
        assert_eq!(freed, alloc::vec![BDF_A]);

        // Supervisor restarts the driver as PID 200; claim succeeds.
        reg.try_claim(200, BDF_A).expect("reclaim after exit");
        assert_eq!(reg.owner_of(BDF_A), Some(200));
    }

    // ---- Concurrent-claim race (modeled sequentially) -----------------

    #[test]
    fn concurrent_claim_race_exactly_one_succeeds() {
        // Pure-logic approximation: the kernel wrapper holds a `spin::Mutex`
        // across `try_claim` so the two orderings below are the only
        // observable interleavings. Asserting that both orderings yield
        // exactly one success proves the invariant the wrapper preserves.
        for (first, second) in [(100, 200), (200, 100)] {
            let mut reg = DeviceHostRegistryCore::new();
            let r1 = reg.try_claim(first, BDF_A);
            let r2 = reg.try_claim(second, BDF_A);
            let successes = [r1, r2].iter().filter(|r| r.is_ok()).count();
            assert_eq!(
                successes, 1,
                "exactly one of the two racers must succeed (first={first}, second={second})",
            );
            assert_eq!(reg.owner_of(BDF_A), Some(first));
        }
    }

    // ---- Accounting ---------------------------------------------------

    #[test]
    fn is_empty_and_len_track_entries() {
        let mut reg = DeviceHostRegistryCore::new();
        assert!(reg.is_empty());
        assert_eq!(reg.len(), 0);
        reg.try_claim(1, BDF_A).unwrap();
        reg.try_claim(1, BDF_B).unwrap();
        assert!(!reg.is_empty());
        assert_eq!(reg.len(), 2);
        reg.release(1, BDF_A).unwrap();
        assert_eq!(reg.len(), 1);
    }

    // ---- Error → DeviceHostError mapping ------------------------------

    #[test]
    fn registry_error_maps_to_device_host_error() {
        assert_eq!(
            DeviceHostError::from(RegistryError::AlreadyClaimed),
            DeviceHostError::AlreadyClaimed,
        );
        assert_eq!(
            DeviceHostError::from(RegistryError::NotClaimed),
            DeviceHostError::NotClaimed,
        );
        // WrongOwner deliberately surfaces at the boundary as NotClaimed
        // because the capability table validated the handle before the
        // registry saw the request.
        assert_eq!(
            DeviceHostError::from(RegistryError::WrongOwner),
            DeviceHostError::NotClaimed,
        );
    }
}
