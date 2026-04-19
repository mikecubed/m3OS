//! Pure-logic block-dispatch state machine — Phase 55b Track D.4.
//!
//! This module hosts the host-testable dispatch-priority table and restart-
//! timeout budget tracker used by `kernel/src/blk/remote.rs`. Putting the
//! pure state here keeps `kernel-core` the single home for logic that crosses
//! the kernel / userspace boundary, and lets all invariants be exercised via
//! `cargo test -p kernel-core` without a running kernel.
//!
//! The **dispatch priority** rule is:
//!   1. `RemoteBlockDevice` (ring-3 NVMe driver via IPC) — if registered.
//!   2. VirtIO-blk (in-kernel driver) — otherwise.
//!
//! This matches the Phase 55 in-kernel NVMe priority with the NVMe driver
//! removed from ring 0 and replaced by the ring-3 facade.
//!
//! The **restart-timeout** rule is:
//!   - A request arriving while the driver is mid-restart must stall for at
//!     most `DRIVER_RESTART_TIMEOUT_MS` before returning an error.
//!   - The kernel-side facade reads this bound at construction time via
//!     [`DRIVER_RESTART_TIMEOUT_MS`] and may override it per-driver via the
//!     service `.conf`.
//!
//! The **grant single-use** rule is:
//!   - Each IPC capability grant carrying write bulk-data is consumed
//!     exactly once. Replaying the same grant handle across requests violates
//!     the Phase 50 contract; [`GrantIdTracker`] enforces this in the pure-
//!     logic domain so the facade can reject replays before forwarding them.

use crate::device_host::DRIVER_RESTART_TIMEOUT_MS;

// ---------------------------------------------------------------------------
// WaitOutcome — result of polling the restart-wait loop
// ---------------------------------------------------------------------------

/// Outcome returned by [`BlockDispatchState::check_restart_wait`].
///
/// The kernel facade drives a yield loop and calls `check_restart_wait` on
/// each iteration. This type is the single-iteration decision:
///
/// - `Ready` — break out and retry IPC once.
/// - `TimedOut` — break out and return EIO.
/// - `Waiting` — driver not yet ready and budget remains; yield and retry.
///
/// Keeping the three-way split in pure-logic lets tests drive all branches
/// without a real clock or scheduler.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WaitOutcome {
    /// Driver became ready within the timeout window; caller should retry IPC.
    Ready,
    /// Timeout elapsed before the driver re-registered; caller should return EIO.
    TimedOut,
    /// Driver still mid-restart but budget not yet exhausted; caller should
    /// yield and call `check_restart_wait` again.
    Waiting,
}

// ---------------------------------------------------------------------------
// RemoteDeviceError
// ---------------------------------------------------------------------------

/// Errors returned by the `RemoteBlockDevice` facade.
///
/// `#[non_exhaustive]` allows later phases to add variants (e.g., for
/// `RemoteNic`) without breaking downstream `match` exhaustiveness.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum RemoteDeviceError {
    /// No remote driver is registered; fall through to the in-kernel device.
    NotRegistered,
    /// The driver process is mid-restart and did not come back within
    /// [`DRIVER_RESTART_TIMEOUT_MS`].
    RestartTimeout,
    /// The IPC call to the driver process failed (endpoint closed, etc.).
    IpcError,
    /// The caller attempted to replay a write-data grant that was already
    /// consumed by a prior request (Phase 50 single-use contract).
    GrantReplayed,
    /// The driver reported an I/O error (mapped from [`BlockDriverError::IoError`]).
    IoError,
    /// The sector range exceeds the device's addressable range.
    InvalidLba,
    /// The request is too large (exceeds [`MAX_SECTORS_PER_REQUEST`]).
    ///
    /// [`MAX_SECTORS_PER_REQUEST`]: crate::driver_ipc::block::MAX_SECTORS_PER_REQUEST
    RequestTooLarge,
    /// The driver process was absent or did not reply with a recognised status.
    DriverAbsent,
}

// ---------------------------------------------------------------------------
// BlockDispatchState
// ---------------------------------------------------------------------------

/// Dispatch-priority table for the block layer — pure-logic, no IPC, no
/// `no_std` kernel dependencies.
///
/// The kernel-side `RemoteBlockDevice` holds one of these (behind a `Mutex`).
/// The state machine has three states:
///
/// - **Unregistered** — no ring-3 driver present; every dispatch call
///   returns `Err(RemoteDeviceError::NotRegistered)` so the block layer falls
///   through to VirtIO-blk.
/// - **Ready** — driver registered and presumed healthy; dispatch calls
///   succeed and the timeout budget is reset.
/// - **Restarting** — driver has crashed; dispatch calls stall up to
///   `restart_deadline_ms` before returning `Err(RestartTimeout)`.
///
/// The `restart_deadline_ms` field is set at construction time from
/// [`DRIVER_RESTART_TIMEOUT_MS`] and can be overridden for testing.
#[derive(Debug)]
pub struct BlockDispatchState {
    /// Registered driver endpoint name (up to 32 bytes, ASCII).
    ///
    /// `None` means the remote driver has not been installed yet.
    device_name: Option<alloc::string::String>,
    /// Whether the driver is currently mid-restart.
    restarting: bool,
    /// How many milliseconds to wait for a restarting driver before returning
    /// `RestartTimeout`. Defaults to [`DRIVER_RESTART_TIMEOUT_MS`].
    pub restart_deadline_ms: u32,
}

impl BlockDispatchState {
    /// Create a new, **unregistered** dispatch state.
    ///
    /// `restart_deadline_ms` defaults to [`DRIVER_RESTART_TIMEOUT_MS`] but
    /// may be overridden by per-driver service config or test fixtures.
    pub fn new() -> Self {
        Self {
            device_name: None,
            restarting: false,
            restart_deadline_ms: DRIVER_RESTART_TIMEOUT_MS,
        }
    }

    /// Register a remote driver endpoint.
    ///
    /// After registration [`is_registered`] returns `true` and the state
    /// transitions to **Ready**. Registering a second driver overwrites the
    /// previous name and clears the restart flag (service manager re-registers
    /// after a clean restart).
    ///
    /// [`is_registered`]: Self::is_registered
    pub fn register(&mut self, device_name: &str) {
        self.device_name = Some(alloc::string::String::from(device_name));
        self.restarting = false;
    }

    /// Returns `true` when a remote driver endpoint is installed.
    pub fn is_registered(&self) -> bool {
        self.device_name.is_some()
    }

    /// Returns the registered device name, if any.
    pub fn device_name(&self) -> Option<&str> {
        self.device_name.as_deref()
    }

    /// Mark the driver as mid-restart.
    ///
    /// The block layer calls this when an IPC call to the driver endpoint
    /// fails with a closed-endpoint or driver-restarting indication. The
    /// facade will then stall new requests up to `restart_deadline_ms`.
    pub fn mark_restarting(&mut self) {
        self.restarting = true;
    }

    /// Mark the driver as recovered (restart complete).
    ///
    /// The block layer (or the service-manager notification path) calls this
    /// when the driver endpoint is re-registered after a restart.
    pub fn mark_ready(&mut self) {
        self.restarting = false;
    }

    /// Returns `true` when the driver is mid-restart.
    pub fn is_restarting(&self) -> bool {
        self.restarting
    }

    /// Single-shot poll of the restart-wait state machine with an injected clock.
    ///
    /// This is a **pure-logic, single-call decision function** — it does not
    /// sleep or loop internally. The kernel facade calls this in a yield loop:
    ///
    /// ```text
    /// let deadline = tick_count() + state.restart_deadline_ms as u64;
    /// loop {
    ///     match BlockDispatchState::check_restart_wait(
    ///             tick_count(), deadline, !state.is_restarting()) {
    ///         WaitOutcome::Ready   => { /* retry IPC once */ break; }
    ///         WaitOutcome::TimedOut => return Err(0xFF),
    ///         WaitOutcome::Waiting  => { yield_now(); }
    ///     }
    /// }
    /// ```
    ///
    /// `now_ms` — current monotonic milliseconds (kernel: `tick_count()`).
    /// `deadline_ms` — absolute deadline computed once before the loop.
    /// `is_ready` — `true` when the driver has re-registered (i.e.
    ///   `!state.is_restarting()`).
    pub fn check_restart_wait(now_ms: u64, deadline_ms: u64, is_ready: bool) -> WaitOutcome {
        if is_ready {
            WaitOutcome::Ready
        } else if now_ms >= deadline_ms {
            WaitOutcome::TimedOut
        } else {
            WaitOutcome::Waiting
        }
    }

    /// Check whether this state should dispatch to the remote driver.
    ///
    /// Returns:
    /// - `Ok(())` — driver is registered and **Ready**; caller should forward
    ///   the request via IPC.
    /// - `Err(NotRegistered)` — no driver registered; fall through to
    ///   VirtIO-blk.
    /// - `Err(RestartTimeout)` — driver is mid-restart and the caller has
    ///   already exhausted the restart budget (signalled by passing
    ///   `timed_out = true`); return `EIO` to the caller.
    ///
    /// When `timed_out` is `false` and `is_restarting()` is `true` the
    /// caller should sleep up to `restart_deadline_ms` and retry. This
    /// function does not perform the sleep itself because it is pure-logic;
    /// the kernel-side facade owns the actual wait.
    pub fn check_dispatch(&self, timed_out: bool) -> Result<(), RemoteDeviceError> {
        match &self.device_name {
            None => Err(RemoteDeviceError::NotRegistered),
            Some(_) if self.restarting && timed_out => Err(RemoteDeviceError::RestartTimeout),
            Some(_) => Ok(()),
        }
    }
}

impl Default for BlockDispatchState {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// GrantIdTracker — enforces Phase 50 single-use grant contract
// ---------------------------------------------------------------------------

/// Tracks IPC grant handles that have been consumed so the facade can reject
/// any attempt to replay the same handle across requests.
///
/// The tracker stores the last `N` consumed handles in a fixed-size ring
/// (default capacity 64, matching the IPC client queue depth from the
/// engineering-discipline doc). A grant handle is a `u32`; `0` is always
/// valid as "no grant" (read requests) and is never recorded.
///
/// This type is pure-logic and host-testable. The facade wraps it in a
/// `Mutex` so concurrent requests see a consistent view.
pub struct GrantIdTracker {
    seen: alloc::vec::Vec<u32>,
    capacity: usize,
}

impl GrantIdTracker {
    /// Create a tracker with the default capacity (64).
    pub fn new() -> Self {
        Self {
            seen: alloc::vec::Vec::with_capacity(64),
            capacity: 64,
        }
    }

    /// Create a tracker with a custom capacity (for testing).
    pub fn with_capacity(cap: usize) -> Self {
        Self {
            seen: alloc::vec::Vec::with_capacity(cap),
            capacity: cap,
        }
    }

    /// Record `grant_id` as consumed and return `Ok(())`, or return
    /// `Err(RemoteDeviceError::GrantReplayed)` if `grant_id` was already seen.
    ///
    /// `grant_id == 0` is always accepted without recording (read requests
    /// carry no write payload).
    pub fn consume(&mut self, grant_id: u32) -> Result<(), RemoteDeviceError> {
        if grant_id == 0 {
            return Ok(());
        }
        if self.seen.contains(&grant_id) {
            return Err(RemoteDeviceError::GrantReplayed);
        }
        if self.seen.len() >= self.capacity {
            // Evict the oldest entry (FIFO ring behaviour).
            self.seen.remove(0);
        }
        self.seen.push(grant_id);
        Ok(())
    }

    /// Number of distinct grant IDs currently tracked.
    pub fn len(&self) -> usize {
        self.seen.len()
    }

    /// `true` when no grant IDs are tracked.
    pub fn is_empty(&self) -> bool {
        self.seen.is_empty()
    }
}

impl Default for GrantIdTracker {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ---- BlockDispatchState -------------------------------------------------

    #[test]
    fn new_state_is_unregistered() {
        let s = BlockDispatchState::new();
        assert!(!s.is_registered());
        assert!(!s.is_restarting());
        assert_eq!(s.device_name(), None);
    }

    #[test]
    fn register_makes_state_registered_and_ready() {
        let mut s = BlockDispatchState::new();
        s.register("nvme0");
        assert!(s.is_registered());
        assert!(!s.is_restarting());
        assert_eq!(s.device_name(), Some("nvme0"));
    }

    #[test]
    fn check_dispatch_unregistered_returns_not_registered() {
        let s = BlockDispatchState::new();
        assert_eq!(
            s.check_dispatch(false),
            Err(RemoteDeviceError::NotRegistered)
        );
    }

    #[test]
    fn check_dispatch_registered_and_ready_returns_ok() {
        let mut s = BlockDispatchState::new();
        s.register("nvme0");
        assert_eq!(s.check_dispatch(false), Ok(()));
    }

    #[test]
    fn dispatch_priority_remote_before_virtio() {
        // Simulates the dispatch-priority rule: when registered, RemoteBlockDevice
        // wins over VirtIO-blk.
        let mut s = BlockDispatchState::new();
        // Before registration: check_dispatch says not registered → use VirtIO.
        assert_eq!(
            s.check_dispatch(false),
            Err(RemoteDeviceError::NotRegistered),
            "unregistered state should signal VirtIO fallback"
        );
        // After registration: check_dispatch returns Ok → use remote driver.
        s.register("nvme0");
        assert_eq!(
            s.check_dispatch(false),
            Ok(()),
            "registered state should signal remote dispatch"
        );
    }

    #[test]
    fn mark_restarting_then_check_dispatch_without_timeout_returns_ok() {
        // Mid-restart but not yet timed out → caller should still try to wait.
        let mut s = BlockDispatchState::new();
        s.register("nvme0");
        s.mark_restarting();
        assert!(s.is_restarting());
        // timed_out = false: caller has not exhausted the budget yet.
        assert_eq!(s.check_dispatch(false), Ok(()));
    }

    #[test]
    fn mark_restarting_then_check_dispatch_after_timeout_returns_restart_timeout() {
        let mut s = BlockDispatchState::new();
        s.register("nvme0");
        s.mark_restarting();
        // timed_out = true: restart budget exhausted.
        assert_eq!(
            s.check_dispatch(true),
            Err(RemoteDeviceError::RestartTimeout)
        );
    }

    #[test]
    fn mark_ready_clears_restarting_flag() {
        let mut s = BlockDispatchState::new();
        s.register("nvme0");
        s.mark_restarting();
        s.mark_ready();
        assert!(!s.is_restarting());
        assert_eq!(s.check_dispatch(false), Ok(()));
    }

    #[test]
    fn re_register_clears_restarting_flag() {
        let mut s = BlockDispatchState::new();
        s.register("nvme0");
        s.mark_restarting();
        // Service manager re-registers after restart.
        s.register("nvme0");
        assert!(!s.is_restarting());
    }

    #[test]
    fn restart_deadline_defaults_to_driver_restart_timeout_ms() {
        let s = BlockDispatchState::new();
        assert_eq!(s.restart_deadline_ms, DRIVER_RESTART_TIMEOUT_MS);
    }

    // ---- GrantIdTracker ---------------------------------------------------

    #[test]
    fn zero_grant_is_always_accepted() {
        let mut t = GrantIdTracker::new();
        assert_eq!(t.consume(0), Ok(()));
        // Even twice — zero is never recorded.
        assert_eq!(t.consume(0), Ok(()));
        assert_eq!(t.len(), 0);
    }

    #[test]
    fn nonzero_grant_accepted_first_time() {
        let mut t = GrantIdTracker::new();
        assert_eq!(t.consume(42), Ok(()));
        assert_eq!(t.len(), 1);
    }

    #[test]
    fn grant_replay_is_rejected() {
        let mut t = GrantIdTracker::new();
        t.consume(42).expect("first consume");
        assert_eq!(
            t.consume(42),
            Err(RemoteDeviceError::GrantReplayed),
            "replaying a grant handle must be rejected"
        );
    }

    #[test]
    fn different_grant_ids_are_independent() {
        let mut t = GrantIdTracker::new();
        assert_eq!(t.consume(1), Ok(()));
        assert_eq!(t.consume(2), Ok(()));
        assert_eq!(t.len(), 2);
        // Both replay — both fail.
        assert_eq!(t.consume(1), Err(RemoteDeviceError::GrantReplayed));
        assert_eq!(t.consume(2), Err(RemoteDeviceError::GrantReplayed));
    }

    #[test]
    fn tracker_evicts_oldest_when_capacity_reached() {
        let mut t = GrantIdTracker::with_capacity(3);
        t.consume(1).expect("1");
        t.consume(2).expect("2");
        t.consume(3).expect("3");
        // Capacity reached — adding 4 evicts 1.
        t.consume(4).expect("4");
        assert_eq!(t.len(), 3);
        // 1 was evicted, so replaying it is accepted again.
        assert_eq!(
            t.consume(1),
            Ok(()),
            "evicted grant should be accepted after eviction"
        );
        // After consuming 1 again, the ring is {3,4,1} (2 got evicted).
        // Replaying 3 must be rejected since it is still in the ring.
        assert_eq!(t.consume(3), Err(RemoteDeviceError::GrantReplayed));
    }

    #[test]
    fn grant_single_use_across_requests_semantics() {
        // Simulates a write followed by a replay attempt — the second attempt
        // must be rejected, proving the Phase 50 single-use contract holds.
        let mut t = GrantIdTracker::new();
        let write_grant: u32 = 0x0000_0007;
        // First request: consume the grant.
        assert_eq!(t.consume(write_grant), Ok(()));
        // Second request: same grant handle is replayed → rejected.
        assert_eq!(
            t.consume(write_grant),
            Err(RemoteDeviceError::GrantReplayed),
            "grant replay across requests must be rejected (Phase 50 single-use contract)"
        );
    }

    // ---- check_restart_wait (D.4 timed-block pure-logic) --------------------

    /// Driver ready immediately: should return Ready regardless of deadline.
    #[test]
    fn check_restart_wait_ready_when_driver_is_ready() {
        let outcome = BlockDispatchState::check_restart_wait(
            100,   // now_ms
            1100,  // deadline_ms (budget not yet expired)
            true,  // is_ready
        );
        assert_eq!(
            outcome,
            WaitOutcome::Ready,
            "driver ready before timeout → WaitOutcome::Ready"
        );
    }

    /// Timeout elapsed before driver recovered: must return TimedOut.
    #[test]
    fn check_restart_wait_timed_out_when_deadline_passed() {
        let outcome = BlockDispatchState::check_restart_wait(
            1101,  // now_ms — past the deadline
            1100,  // deadline_ms
            false, // is_ready — driver still absent
        );
        assert_eq!(
            outcome,
            WaitOutcome::TimedOut,
            "past deadline with driver absent → WaitOutcome::TimedOut"
        );
    }

    /// Deadline not yet reached and driver still absent: must return Waiting.
    #[test]
    fn check_restart_wait_waiting_when_within_budget() {
        let outcome = BlockDispatchState::check_restart_wait(
            200,  // now_ms — well inside the budget
            1200, // deadline_ms
            false, // is_ready — driver still absent
        );
        assert_eq!(
            outcome,
            WaitOutcome::Waiting,
            "within budget, driver absent → WaitOutcome::Waiting so caller yields"
        );
    }

    /// Exact deadline boundary: now == deadline should be treated as timed out.
    #[test]
    fn check_restart_wait_timed_out_at_exact_deadline() {
        let outcome = BlockDispatchState::check_restart_wait(
            1000, // now_ms == deadline_ms
            1000, // deadline_ms
            false,
        );
        assert_eq!(
            outcome,
            WaitOutcome::TimedOut,
            "now == deadline with driver absent → WaitOutcome::TimedOut"
        );
    }

    /// Simulate the full wait-loop with a mock clock advancing each iteration.
    /// The driver becomes ready at iteration 3 (t=300 ms into a 1000 ms budget).
    #[test]
    fn check_restart_wait_loop_driver_recovers_mid_wait() {
        let budget_ms: u64 = 1000;
        let start_ms: u64 = 0;
        let deadline_ms = start_ms + budget_ms;
        let recovery_at_ms: u64 = 300; // driver re-registers at t=300

        let mut now = start_ms;
        let mut outcome = WaitOutcome::Waiting;
        let mut iterations = 0usize;
        loop {
            let is_ready = now >= recovery_at_ms;
            outcome = BlockDispatchState::check_restart_wait(now, deadline_ms, is_ready);
            match outcome {
                WaitOutcome::Ready | WaitOutcome::TimedOut => break,
                WaitOutcome::Waiting => {
                    // Mock yield: advance clock by 100 ms.
                    now += 100;
                    iterations += 1;
                    assert!(
                        iterations < 20,
                        "loop should terminate well before 20 iterations"
                    );
                }
            }
        }
        assert_eq!(
            outcome,
            WaitOutcome::Ready,
            "driver recovered at t=300 ms — loop must resolve to Ready"
        );
    }

    /// Simulate the full wait-loop where the driver never recovers.
    /// Must resolve to TimedOut after the budget expires.
    #[test]
    fn check_restart_wait_loop_driver_never_recovers() {
        let budget_ms: u64 = 1000;
        let start_ms: u64 = 0;
        let deadline_ms = start_ms + budget_ms;

        let mut now = start_ms;
        let mut outcome = WaitOutcome::Waiting;
        let mut iterations = 0usize;
        loop {
            outcome = BlockDispatchState::check_restart_wait(now, deadline_ms, false);
            match outcome {
                WaitOutcome::Ready | WaitOutcome::TimedOut => break,
                WaitOutcome::Waiting => {
                    now += 100; // advance 100 ms per mock yield
                    iterations += 1;
                    assert!(
                        iterations < 50,
                        "loop must terminate after budget expires"
                    );
                }
            }
        }
        assert_eq!(
            outcome,
            WaitOutcome::TimedOut,
            "driver never recovered — loop must resolve to TimedOut after 1000 ms budget"
        );
        assert!(
            now >= deadline_ms,
            "clock must have reached or passed the deadline"
        );
    }

    /// `check_restart_wait` with `is_ready = true` must return `Ready` even
    /// when called at or past the deadline — driver recovery wins over timeout.
    #[test]
    fn check_restart_wait_ready_beats_expired_deadline() {
        let outcome = BlockDispatchState::check_restart_wait(
            9999, // now_ms — far past deadline
            1000, // deadline_ms
            true, // is_ready — driver just re-registered
        );
        assert_eq!(
            outcome,
            WaitOutcome::Ready,
            "is_ready=true takes priority over expired deadline"
        );
    }
}
