//! F.2 — Crash-and-restart regression test suite.
//!
//! Pure-logic coverage for the restart-state-machine. Every acceptance
//! bullet that can be exercised without a running QEMU kernel is pinned
//! here. The SIGKILL-driver-mid-I/O scenario that requires a live NVMe
//! driver process and the `BlockDriverError::DriverRestarting` observation
//! path are scaffolded as `#[ignore]` stubs at the bottom of this file
//! with explicit TODOs that name the missing API surface.
//!
//! # What this file proves
//!
//! 1. `BlockDispatchState` transitions from `Ready` → `Restarting` →
//!    `Ready` on a simulated crash/re-register cycle, and that the
//!    `DRIVER_RESTART_TIMEOUT_MS` constant is the agreed deadline.
//! 2. `DeviceHostRegistryCore::release_for_pid` frees the driver's device
//!    claim on crash, and a fresh process PID can re-claim it.
//! 3. `ServiceState` (the init service-manager lifecycle) transitions
//!    through `Running → Stopped → Starting → Running` correctly for the
//!    crash-restart path.
//! 4. `max_restart` enforcement: after `MAX_RESTARTS` crashes the service
//!    transitions to `PermanentlyStopped`, modelling the `service status`
//!    returning `failed` acceptance bullet.
//! 5. `BlockDriverError::DriverRestarting` and `NetDriverError::DriverRestarting`
//!    encode / decode cleanly — the wire type exists and can traverse the
//!    IPC seam.
//! 6. A write request arriving while the driver is `Restarting` (but not
//!    yet timed out) returns `Ok` from `check_dispatch` — the caller is
//!    expected to wait up to `DRIVER_RESTART_TIMEOUT_MS`.
//! 7. After the deadline passes (`timed_out = true`) `check_dispatch`
//!    returns `RestartTimeout`, matching the acceptance bullet
//!    "outstanding write returns `BlockDriverError::DriverRestarting`
//!    within `DRIVER_RESTART_TIMEOUT_MS`".
//! 8. e1000 analogue: `NetDriverError::DriverRestarting` exists and is
//!    distinct from `NetDriverError::Ok` and `NetDriverError::LinkDown`.
//!
//! # What is NOT covered here (deferred to Track F.3)
//!
//! - Spawning an actual NVMe userspace driver process, issuing a real write,
//!   and delivering SIGKILL mid-write (F.3). Phase 55b F.2b resolved the two
//!   original blockers:
//!     (a) `service kill <name>` is now in `userspace/coreutils-rs/src/service.rs`.
//!     (b) `kernel/src/blk/remote.rs` now returns `BlockDriverError::DriverRestarting`
//!         (byte 5) on IPC endpoint closure, not generic 0xFF.
//!   What remains for F.3: a guest-accessible I/O-client binary that can trigger
//!   a write mid-restart and inspect the returned error code. The QEMU regression
//!   `driver-restart-guest` covers the boot/kill/restart cycle (enabled via
//!   `M3OS_ENABLE_DRIVER_RESTART_REGRESSION`).
//! - Service-manager log events `driver.restart` and `driver.restarted` require a
//!   structured log subscriber attached to the running init process. The log-pipeline
//!   regression (`log-pipeline` xtask) exercises the log path but does not drill into
//!   driver-restart events.
//! - The "subsequent write to same LBA succeeds" path is exercised by the Phase 55
//!   storage round-trip regression (`storage-roundtrip` xtask) once the ring-3 NVMe
//!   driver is wired into that regression.

use kernel_core::device_host::{
    DRIVER_RESTART_TIMEOUT_MS, DeviceCapKey, DeviceHostError, DeviceHostRegistryCore, RegistryError,
};
use kernel_core::driver_ipc::block::{BlkReplyHeader, BlockDriverError, encode_blk_reply};
use kernel_core::driver_ipc::net::NetDriverError;
use kernel_core::driver_ipc::{BlockDispatchState, RemoteDeviceError};
use kernel_core::service::{
    ExitClassification, RestartPolicy, ServiceState, classify_exit, should_restart,
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

const NVME_BDF: DeviceCapKey = DeviceCapKey::new(0, 0x00, 0x03, 0);

/// Simulate `N` crash-restart cycles using `BlockDispatchState`.
///
/// Returns the number of times the driver completed a full
/// `Restarting → Ready` cycle without hitting a timeout.
fn simulate_crash_cycles(state: &mut BlockDispatchState, cycles: u32) -> u32 {
    let mut completed = 0u32;
    for _ in 0..cycles {
        // Driver crashes — service manager calls mark_restarting.
        state.mark_restarting();
        assert!(state.is_restarting(), "must be restarting after crash");

        // Service manager restarts the driver and re-registers it.
        state.register("nvme_driver");
        assert!(
            !state.is_restarting(),
            "register must clear restarting flag"
        );
        completed += 1;
    }
    completed
}

// ---------------------------------------------------------------------------
// 1. Block dispatch state machine: Ready → Restarting → Ready
// ---------------------------------------------------------------------------

#[test]
fn block_dispatch_crash_restart_cycle() {
    let mut state = BlockDispatchState::new();
    state.register("nvme_driver");

    assert!(state.is_registered());
    assert!(!state.is_restarting());
    assert_eq!(state.check_dispatch(false), Ok(()));

    // Simulate driver crash.
    state.mark_restarting();
    assert!(state.is_restarting());

    // Request arrives while mid-restart but timeout not yet elapsed.
    // The facade should stall, not immediately error — check_dispatch
    // returns Ok so the caller can wait.
    assert_eq!(
        state.check_dispatch(false),
        Ok(()),
        "mid-restart without timeout should allow the caller to wait"
    );

    // Restart deadline exceeded — check_dispatch reports timeout.
    assert_eq!(
        state.check_dispatch(true),
        Err(RemoteDeviceError::RestartTimeout),
        "mid-restart with timed_out=true must return RestartTimeout"
    );

    // Service manager re-registers the driver after restart.
    state.register("nvme_driver");
    assert!(!state.is_restarting());
    assert_eq!(state.check_dispatch(false), Ok(()));
}

// ---------------------------------------------------------------------------
// 2. Registry releases device claim on driver crash (PID exit path)
// ---------------------------------------------------------------------------

#[test]
fn registry_releases_claim_on_driver_crash_then_new_pid_can_reclaim() {
    let mut registry = DeviceHostRegistryCore::new();

    // Driver PID 100 claims the NVMe device.
    registry
        .try_claim(100, NVME_BDF)
        .expect("initial claim by PID 100");
    assert_eq!(registry.owner_of(NVME_BDF), Some(100));

    // Driver crashes — service manager calls release_for_pid on exit.
    let freed = registry.release_for_pid(100);
    assert_eq!(freed.len(), 1);
    assert!(freed.contains(&NVME_BDF));
    assert_eq!(registry.owner_of(NVME_BDF), None);

    // Service manager restarts the driver as PID 200.
    registry
        .try_claim(200, NVME_BDF)
        .expect("re-claim by restarted PID 200");
    assert_eq!(registry.owner_of(NVME_BDF), Some(200));
}

// ---------------------------------------------------------------------------
// 3. Service-state lifecycle: Running → Stopped → Starting → Running
// ---------------------------------------------------------------------------

#[test]
fn service_state_crash_restart_lifecycle() {
    let state = ServiceState::Running;

    // Driver crashes — SIGKILL produces a SignalDeath exit.
    let crashed = state
        .try_transition(ServiceState::Stopped { exit_code: 0x89 })
        .expect("Running → Stopped is valid");
    assert!(matches!(crashed, ServiceState::Stopped { .. }));

    // The exit was a signal death — classify_exit maps it correctly.
    let exit = classify_exit(0x89); // bit 7 set → SignalDeath, signal = 9
    assert!(matches!(exit, ExitClassification::SignalDeath(9)));

    // restart policy `Always` means the service manager restarts.
    assert!(
        should_restart(RestartPolicy::Always, &exit),
        "Always policy must restart on signal death"
    );

    // Transition to Starting.
    let starting = crashed
        .try_transition(ServiceState::Starting)
        .expect("Stopped → Starting is valid");
    assert_eq!(starting, ServiceState::Starting);

    // Transition to Running once the driver re-registers.
    let running = starting
        .try_transition(ServiceState::Running)
        .expect("Starting → Running is valid");
    assert_eq!(running, ServiceState::Running);
}

// ---------------------------------------------------------------------------
// 4. max_restart enforcement: 6 crashes in a window → PermanentlyStopped
// ---------------------------------------------------------------------------

/// The `.conf` default is 5 restarts permitted; the 6th crash must tip
/// the service into `PermanentlyStopped` (i.e. `service status` returns
/// `failed`).
///
/// This test models the enforcement at the service-manager level using the
/// `ServiceState` machine. The restart count tracking is the caller's
/// responsibility (init keeps the count per-service); this test exercises
/// the state-machine branch only.
#[test]
fn max_restart_enforcement_sixth_crash_transitions_to_permanently_stopped() {
    const MAX_RESTARTS: u32 = 5;

    let mut state = ServiceState::NeverStarted;
    state = state
        .try_transition(ServiceState::Starting)
        .expect("NeverStarted → Starting");
    state = state
        .try_transition(ServiceState::Running)
        .expect("Starting → Running");

    let mut restart_count: u32 = 0;

    // Simulate MAX_RESTARTS crashes that are recoverable.
    for i in 0..MAX_RESTARTS {
        state = state
            .try_transition(ServiceState::Stopped { exit_code: 0x89 })
            .unwrap_or_else(|_| panic!("crash {i}: Running → Stopped must succeed"));

        restart_count += 1;

        if restart_count <= MAX_RESTARTS {
            state = state
                .try_transition(ServiceState::Starting)
                .unwrap_or_else(|_| panic!("restart {i}: Stopped → Starting must succeed"));
            state = state
                .try_transition(ServiceState::Running)
                .unwrap_or_else(|_| panic!("restart {i}: Starting → Running must succeed"));
        }
    }

    assert_eq!(restart_count, MAX_RESTARTS);
    assert_eq!(state, ServiceState::Running);

    // 6th crash: restart_count would be MAX_RESTARTS + 1 → PermanentlyStopped.
    state = state
        .try_transition(ServiceState::Stopped { exit_code: 0x89 })
        .expect("6th crash: Running → Stopped");

    // Supervisor decides not to restart — transitions to PermanentlyStopped.
    state = state
        .try_transition(ServiceState::PermanentlyStopped)
        .expect("Stopped → PermanentlyStopped after max restarts");

    assert_eq!(
        state,
        ServiceState::PermanentlyStopped,
        "service must be PermanentlyStopped after exceeding max_restart"
    );

    // Terminal state — no further transitions are valid.
    let err = state.try_transition(ServiceState::Starting);
    assert!(err.is_err(), "PermanentlyStopped must be a terminal state");
}

// ---------------------------------------------------------------------------
// 5. BlockDriverError::DriverRestarting encodes / decodes on the wire
// ---------------------------------------------------------------------------

#[test]
fn block_driver_error_driver_restarting_wire_round_trip() {
    let reply = BlkReplyHeader {
        cmd_id: 42,
        status: BlockDriverError::DriverRestarting,
        bytes: 0,
    };
    let wire = encode_blk_reply(reply, 0);

    // The status byte at offset 8 must be the `DriverRestarting` discriminant.
    assert_eq!(wire[8], BlockDriverError::DriverRestarting.to_byte());

    // Decode must produce the same header.
    let (decoded, grant) =
        kernel_core::driver_ipc::block::decode_blk_reply(&wire).expect("decode succeeds");
    assert_eq!(decoded.status, BlockDriverError::DriverRestarting);
    assert_eq!(decoded.cmd_id, 42);
    assert_eq!(decoded.bytes, 0);
    assert_eq!(grant, 0);
}

#[test]
fn block_driver_error_driver_restarting_is_distinct_from_ok() {
    assert_ne!(
        BlockDriverError::DriverRestarting,
        BlockDriverError::Ok,
        "DriverRestarting must not compare equal to Ok"
    );
    assert_ne!(
        BlockDriverError::DriverRestarting.to_byte(),
        BlockDriverError::Ok.to_byte(),
    );
}

// ---------------------------------------------------------------------------
// 6. Restart-timeout constant is the single source of truth
// ---------------------------------------------------------------------------

#[test]
fn driver_restart_timeout_constant_is_one_second() {
    // Pinned to 1000 ms per the Phase 55b A.1 spec.
    // Any change here must also update every usage site (D.4, E.4, F.2).
    assert_eq!(
        DRIVER_RESTART_TIMEOUT_MS, 1000,
        "DRIVER_RESTART_TIMEOUT_MS must equal 1000 ms (one second)"
    );
}

#[test]
fn block_dispatch_state_restart_deadline_defaults_to_constant() {
    let state = BlockDispatchState::new();
    assert_eq!(
        state.restart_deadline_ms, DRIVER_RESTART_TIMEOUT_MS,
        "BlockDispatchState::restart_deadline_ms must default to DRIVER_RESTART_TIMEOUT_MS"
    );
}

// ---------------------------------------------------------------------------
// 7. Multiple crash cycles complete without false timeouts
// ---------------------------------------------------------------------------

#[test]
fn multiple_crash_cycles_all_complete() {
    let mut state = BlockDispatchState::new();
    state.register("nvme_driver");

    let completed = simulate_crash_cycles(&mut state, 5);
    assert_eq!(
        completed, 5,
        "all 5 crash cycles must complete successfully"
    );
    assert!(state.is_registered());
    assert!(!state.is_restarting());
    assert_eq!(state.check_dispatch(false), Ok(()));
}

// ---------------------------------------------------------------------------
// 8. e1000 analogue: NetDriverError::DriverRestarting exists and is typed
// ---------------------------------------------------------------------------

#[test]
fn net_driver_error_driver_restarting_is_distinct() {
    // NetDriverError::DriverRestarting must be constructible and distinct
    // from Ok and LinkDown — matching the acceptance bullet "kill mid-send,
    // assert NetDriverError::DriverRestarting".
    assert_ne!(NetDriverError::DriverRestarting, NetDriverError::Ok);
    assert_ne!(NetDriverError::DriverRestarting, NetDriverError::LinkDown);
}

// ---------------------------------------------------------------------------
// 9. Registry pre-crash handle invalidation: stale PID sees NotClaimed
// ---------------------------------------------------------------------------

#[test]
fn stale_pid_handle_invalid_after_restart() {
    let mut registry = DeviceHostRegistryCore::new();

    // PID 100 claims.
    registry.try_claim(100, NVME_BDF).expect("initial claim");

    // Simulate crash: release_for_pid frees the claim.
    let freed = registry.release_for_pid(100);
    assert!(freed.contains(&NVME_BDF));

    // Stale PID 100 tries to release again — must return an error,
    // not succeed silently (proves post-crash handles are invalid).
    let err = registry
        .release(100, NVME_BDF)
        .expect_err("stale release must fail");
    assert!(
        matches!(err, RegistryError::NotClaimed | RegistryError::WrongOwner),
        "stale release must return NotClaimed or WrongOwner, got {:?}",
        err
    );
}

// ---------------------------------------------------------------------------
// 10. DeviceHostError::NotClaimed surfaces on stale release
// ---------------------------------------------------------------------------

#[test]
fn device_host_error_not_claimed_surfaces_from_registry_error() {
    // RegistryError::NotClaimed maps to DeviceHostError::NotClaimed —
    // this is the wire-level error that the IPC facade returns to the
    // caller when a restarted driver tries to re-use pre-crash caps.
    let err: DeviceHostError = RegistryError::NotClaimed.into();
    assert_eq!(err, DeviceHostError::NotClaimed);

    let err2: DeviceHostError = RegistryError::WrongOwner.into();
    assert_eq!(
        err2,
        DeviceHostError::NotClaimed,
        "WrongOwner also surfaces as NotClaimed at the IPC boundary"
    );
}

// ---------------------------------------------------------------------------
// QEMU-heavy stubs (require SIGKILL API + BlockDriverError client observation)
// ---------------------------------------------------------------------------

/// FIXME(F.3b → phase-55c-write-path): Spawn the NVMe driver process, issue a
/// write mid-restart, and assert the outstanding write returns
/// `BlockDriverError::DriverRestarting` within `DRIVER_RESTART_TIMEOUT_MS`.
///
/// **Phase 55b F.2b progress:** both F.2b blockers are now resolved:
///   - `service kill <name>` is wired in `userspace/coreutils-rs/src/service.rs`:
///     reads PID from `/run/services.status` and calls kill(2) with SIGKILL.
///   - `kernel/src/blk/remote.rs` now returns `BlockDriverError::DriverRestarting`
///     (wire byte 5) instead of generic 0xFF when the IPC endpoint closes mid-call.
///   - `cargo xtask regression --test driver-restart-guest` (Phase 55b F.2b, gated
///     behind M3OS_ENABLE_DRIVER_RESTART_REGRESSION) drives: boot → `service status
///     nvme_driver` → `service kill nvme_driver` → observe restart log line → re-query
///     status. Requires `--device nvme` (enforced via `RegressionTest::devices`).
///
/// **Phase 55b F.3b progress:** `userspace/nvme-crash-smoke/src/main.rs` is now
///   the guest-side I/O-client binary. It speaks the block IPC protocol directly
///   to the `nvme.block` endpoint, forks a child to kill the driver mid-call,
///   and confirms the IPC transport failure + successful post-restart retry.
///   The QEMU regression `driver-restart-crash` (gated behind
///   M3OS_ENABLE_CRASH_SMOKE) exercises the full end-to-end path.
///
/// Still blocked on (phase-55c write-path):
///   Observing `BlockDriverError::DriverRestarting` (byte 5) from the _kernel
///   facade_ requires an ordinary guest binary to have storage-server privileges
///   (euid = 200, exec_path ∈ {/bin/vfs_server, /bin/fat_server}). The
///   nvme-crash-smoke binary observes the raw IPC transport failure (u64::MAX)
///   instead. The byte-5 assertion from the kernel facade path requires
///   either a privilege relaxation or a new test-only syscall: deferred to
///   phase-55c.
#[test]
#[ignore = "phase-55c write-path deferred: F.3b landed nvme-crash-smoke (IPC transport \
            failure visible from guest); kernel facade DriverRestarting byte (5) \
            requires storage-server privilege or a new test syscall. \
            See driver-restart-crash QEMU regression (M3OS_ENABLE_CRASH_SMOKE)."]
fn qemu_nvme_kill_mid_write_returns_driver_restarting() {
    // Test body intentionally empty — the ignore attribute is the artifact.
    // When this guard is lifted the body will:
    //   1. boot guest with --device nvme
    //   2. launch guest I/O-client via storage-server privilege or test syscall
    //   3. race: `service kill nvme_driver` while write is in flight
    //   4. assert client receives DriverRestarting (byte 5) within DRIVER_RESTART_TIMEOUT_MS
    //   5. assert service logs driver.restart + driver.restarted
    //   6. assert subsequent write to LBA 0 succeeds
    //   7. assert no partial-write corruption (compare LBA 0 before and after)
}

/// FIXME(phase-55c e1000): Analogous e1000 regression test.
///
/// **Phase 55b F.2b progress:** `service kill e1000_driver` is now available
/// from the guest shell (same `service kill` subcommand). The `RemoteNic`
/// error-surfacing path (`NetDriverError::DriverRestarting`) needs the same
/// treatment as the block path.
///
/// **Phase 55b F.3b progress:** F.3b delivered the NVMe crash-smoke binary
///   (`userspace/nvme-crash-smoke/`) and the `driver-restart-crash` QEMU
///   regression. The analogous e1000 path requires a guest net I/O-client
///   binary speaking the `e1000.net` IPC protocol and the `RemoteNic` facade
///   surfacing `NetDriverError::DriverRestarting` on endpoint closure — neither
///   exists yet.
///
/// Still blocked on (phase-55c e1000):
///   - `RemoteNic` kernel facade net-side DriverRestarting surfacing.
///   - A guest net I/O-client binary analogous to nvme-crash-smoke.
///   - ICMP echo or TCP connect path to verify post-restart connectivity.
#[test]
#[ignore = "phase-55c e1000 deferred: F.3b covered NVMe; e1000 still needs \
            RemoteNic DriverRestarting surfacing + guest net I/O-client binary."]
fn qemu_e1000_kill_mid_send_returns_driver_restarting_then_icmp_echo_succeeds() {
    // intentionally empty — see FIXME above
}

/// FIXME(phase-55c max-restart): max_restart=5 enforcement in the running guest.
///
/// **Phase 55b F.2b progress:** `service kill <name>` is now available in the
/// guest shell.
///
/// **Phase 55b F.3b progress:** F.3b's `nvme-crash-smoke` binary kills the
///   driver once and confirms the restart. Scripting 6 kills in sequence within
///   the QEMU regression timeout budget (each kill → restart cycle takes ~1 s)
///   is feasible but requires the binary to loop and count restarts, which is a
///   phase-55c item. The pure-logic analogue
///   (`max_restart_enforcement_sixth_crash_transitions_to_permanently_stopped`)
///   already passes.
///
/// Still blocked on (phase-55c max-restart):
///   A scripted kill-loop in the guest binary that drives restart_count past
///   max_restart within the QEMU regression timeout budget (6 × ~1 s = ~6 s,
///   well within 180 s) and asserts `service status nvme_driver` returns
///   `permanently-stopped` on the 6th crash.
#[test]
#[ignore = "phase-55c max-restart deferred: F.3b kills once; 6-kill loop + \
            permanently-stopped assertion needs a looping guest binary variant."]
fn qemu_max_restart_exceeded_service_status_returns_failed() {
    // intentionally empty — see FIXME above
}
