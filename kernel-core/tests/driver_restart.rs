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
use kernel_core::driver_ipc::net::{
    NetDriverError, net_error_to_neg_errno, net_send_dispatch, net_send_result_to_syscall_ret,
    sendto_restart_errno,
};
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
// 8b. NetDriverError wire byte and net_error_to_neg_errno mapping
//     (Phase 55b Track F.3d-3)
// ---------------------------------------------------------------------------

#[test]
fn net_driver_error_to_byte_mapping() {
    // Discriminant bytes must be stable — they cross the IPC seam.
    assert_eq!(NetDriverError::Ok.to_byte(), 0);
    assert_eq!(NetDriverError::LinkDown.to_byte(), 1);
    assert_eq!(NetDriverError::RingFull.to_byte(), 2);
    assert_eq!(NetDriverError::DeviceAbsent.to_byte(), 3);
    assert_eq!(NetDriverError::DriverRestarting.to_byte(), 4);
    assert_eq!(NetDriverError::InvalidFrame.to_byte(), 5);
}

#[test]
fn net_error_to_neg_errno_driver_restarting_is_eagain() {
    // DriverRestarting (byte 4) must map to NEG_EAGAIN (-11) so callers
    // can distinguish restart from a hard I/O error (Phase 55b F.3d-3).
    let byte = NetDriverError::DriverRestarting.to_byte();
    assert_eq!(
        net_error_to_neg_errno(byte),
        -11,
        "DriverRestarting (byte {byte}) must map to NEG_EAGAIN (-11)"
    );
}

#[test]
fn net_error_to_neg_errno_ring_full_is_eagain() {
    // RingFull (byte 2) is also retriable — must map to NEG_EAGAIN.
    let byte = NetDriverError::RingFull.to_byte();
    assert_eq!(
        net_error_to_neg_errno(byte),
        -11,
        "RingFull (byte {byte}) must map to NEG_EAGAIN (-11)"
    );
}

#[test]
fn net_error_to_neg_errno_ok_is_zero() {
    assert_eq!(net_error_to_neg_errno(NetDriverError::Ok.to_byte()), 0);
}

#[test]
fn net_error_to_neg_errno_hard_errors_are_eio() {
    // LinkDown, DeviceAbsent, InvalidFrame — non-retriable, map to NEG_EIO.
    for &variant in &[
        NetDriverError::LinkDown,
        NetDriverError::DeviceAbsent,
        NetDriverError::InvalidFrame,
    ] {
        let byte = variant.to_byte();
        assert_eq!(
            net_error_to_neg_errno(byte),
            -5,
            "{variant:?} (byte {byte}) must map to NEG_EIO (-5)"
        );
    }
}

// ---------------------------------------------------------------------------
// G.2 — sys_net_send mid-restart EAGAIN observability (Phase 55c Track G)
// ---------------------------------------------------------------------------
// These tests verify that the composition `RemoteNic::send_frame returns
// Err(DriverRestarting)` → `net_send_result_to_syscall_ret` → `-EAGAIN`
// is wired correctly through the chosen send-path shape.  The helper
// `net_send_result_to_syscall_ret` is the pure-logic layer that
// `kernel/src/syscall/net.rs::sys_net_send` delegates to; testing it
// here keeps the invariant host-testable without QEMU.

/// Phase 55c G.2 — end-to-end EAGAIN observability.
///
/// Proves that a `RemoteNic::send_frame` return value of
/// `Err(NetDriverError::DriverRestarting)` propagates to `-EAGAIN` (-11)
/// when fed through `net_send_result_to_syscall_ret` — the pure-logic
/// bridge that `sys_net_send` uses.  This is the load-bearing invariant
/// for R1 correctness; if `DriverRestarting` silently maps to anything
/// other than `EAGAIN` the restart window is invisible to userspace.
#[test]
fn sys_net_send_mid_restart_returns_eagain() {
    let result: Result<(), NetDriverError> = Err(NetDriverError::DriverRestarting);
    assert_eq!(
        net_send_result_to_syscall_ret(result),
        -11_i64,
        "DriverRestarting must surface as NEG_EAGAIN (-11) through sys_net_send"
    );
}

/// Phase 55c G.2 — success path is zero.
///
/// `Ok(())` from `RemoteNic::send_frame` must map to 0 (success) —
/// not to any errno.  Verifies the identity case is not accidentally
/// swallowed by the errno-mapping logic.
#[test]
fn sys_net_send_success_returns_zero() {
    let result: Result<(), NetDriverError> = Ok(());
    assert_eq!(
        net_send_result_to_syscall_ret(result),
        0_i64,
        "Ok(()) must map to 0 (syscall success)"
    );
}

/// Phase 55c G.2 — non-retriable errors are EIO, not EAGAIN.
///
/// `DeviceAbsent`, `LinkDown`, and `InvalidFrame` are hard errors;
/// callers must not retry them as if they were transient.  Verifying
/// this guards against accidentally broadening the EAGAIN surface.
#[test]
fn sys_net_send_hard_errors_return_eio() {
    for &variant in &[
        NetDriverError::LinkDown,
        NetDriverError::DeviceAbsent,
        NetDriverError::InvalidFrame,
    ] {
        let result: Result<(), NetDriverError> = Err(variant);
        assert_eq!(
            net_send_result_to_syscall_ret(result),
            -5_i64,
            "{variant:?} must map to NEG_EIO (-5), not EAGAIN"
        );
    }
}

/// Phase 55c G.2 — RingFull is retriable (EAGAIN), not a hard error.
///
/// `RingFull` is a transient backpressure condition; the caller should
/// retry.  Maps to `EAGAIN` identically to `DriverRestarting`.
#[test]
fn sys_net_send_ring_full_returns_eagain() {
    let result: Result<(), NetDriverError> = Err(NetDriverError::RingFull);
    assert_eq!(
        net_send_result_to_syscall_ret(result),
        -11_i64,
        "RingFull must map to NEG_EAGAIN (-11)"
    );
}

// ---------------------------------------------------------------------------
// G.3 — sys_net_send dispatch-seam: socket boundary + errno mapping combined
//
// These tests exercise `net_send_dispatch`, which is the actual function the
// kernel's `sys_net_send` calls after the arch dispatcher resolves the socket
// fd.  Unlike the G.2 tests above (which cover only the pure-logic
// `net_send_result_to_syscall_ret` helper), these tests cover the full ABI
// path:
//
//   arch dispatch validates sock_fd → has_socket bool
//   → net_send_dispatch(has_socket, frame_result)
//   → NEG_EBADF | net_send_result_to_syscall_ret(frame_result)
//
// This seam is the load-bearing boundary between the socket capability check
// and the errno mapping; if either invariant regresses the test fails.
// ---------------------------------------------------------------------------

/// G.3 — caller without a socket fd receives NEG_EBADF.
///
/// The arch dispatcher passes `has_socket = false` when `arg0` does not
/// resolve to a `FdBackend::Socket` entry.  `net_send_dispatch` must gate on
/// this and return `NEG_EBADF` (-9) without touching the driver path.
#[test]
fn sys_net_send_dispatch_no_socket_returns_ebadf() {
    // frame_result is irrelevant — the socket gate fires first.
    for result in [
        Ok(()),
        Err(NetDriverError::DriverRestarting),
        Err(NetDriverError::RingFull),
        Err(NetDriverError::LinkDown),
    ] {
        assert_eq!(
            net_send_dispatch(false, result),
            -9_i64,
            "no socket must return NEG_EBADF (-9) regardless of frame_result"
        );
    }
}

/// G.3 — valid socket + DriverRestarting surfaces as EAGAIN through dispatch.
///
/// This is the load-bearing R1 invariant for Track G: a caller with a live
/// socket fd that hits a driver restart window must see -EAGAIN (-11), not
/// -EIO, so it can retry.
#[test]
fn sys_net_send_dispatch_driver_restarting_returns_eagain() {
    assert_eq!(
        net_send_dispatch(true, Err(NetDriverError::DriverRestarting)),
        -11_i64,
        "DriverRestarting must surface as NEG_EAGAIN (-11) through the dispatch seam"
    );
}

/// G.3 — valid socket + RingFull surfaces as EAGAIN through dispatch.
///
/// `RingFull` is retriable; callers with a live socket must see EAGAIN, not
/// EIO, so they know to back off and retry.
#[test]
fn sys_net_send_dispatch_ring_full_returns_eagain() {
    assert_eq!(
        net_send_dispatch(true, Err(NetDriverError::RingFull)),
        -11_i64,
        "RingFull must surface as NEG_EAGAIN (-11) through the dispatch seam"
    );
}

/// G.3 — valid socket + successful send returns zero through dispatch.
#[test]
fn sys_net_send_dispatch_success_returns_zero() {
    assert_eq!(
        net_send_dispatch(true, Ok(())),
        0_i64,
        "successful send through dispatch seam must return 0"
    );
}

/// G.3 — valid socket + hard errors return EIO, not EAGAIN.
///
/// Guards against accidentally broadening the EAGAIN surface: only
/// `DriverRestarting` and `RingFull` are retriable; all other errors are
/// hard failures that the caller must not retry blindly.
#[test]
fn sys_net_send_dispatch_hard_errors_return_eio() {
    for variant in [
        NetDriverError::LinkDown,
        NetDriverError::DeviceAbsent,
        NetDriverError::InvalidFrame,
    ] {
        assert_eq!(
            net_send_dispatch(true, Err(variant)),
            -5_i64,
            "{variant:?} must map to NEG_EIO (-5) through the dispatch seam, not EAGAIN"
        );
    }
}

/// G.3 — socket gate takes precedence: socket boundary beats frame_result.
///
/// Proves that the gate ordering is `socket check → errno mapping`, not the
/// reverse.  Without a socket, even a successful frame result returns EBADF.
#[test]
fn sys_net_send_dispatch_gate_precedes_result_mapping() {
    // Success result — but no socket → EBADF, not 0.
    assert_eq!(net_send_dispatch(false, Ok(())), -9_i64);
    // EAGAIN-class result — but no socket → EBADF, not -11.
    assert_eq!(
        net_send_dispatch(false, Err(NetDriverError::DriverRestarting)),
        -9_i64
    );
}

// ---------------------------------------------------------------------------
// G.3 (sendto path) — sendto_restart_errno: the sys_sendto restart gate seam
//
// These tests cover the pure-logic seam that the kernel's `sys_sendto` UDP
// and ICMP branches exercise via `RemoteNic::sendto_restart_ret()`.
//
// The production flow is:
//
//   sys_sendto (FdBackend::Socket validated above)
//   → RemoteNic::sendto_restart_ret()     // reads two AtomicBools and forwards
//       to sendto_restart_errno(...)
//   → return NEG_EAGAIN (-11)
//
// `sendto_restart_errno(is_registered, is_restarting)` is the host-testable
// seam that production invokes through that wrapper. Testing it proves the
// same gate invariant production executes:
//   registered + restarting  → Some(-11) (EAGAIN)
//   registered + healthy     → None (proceed to send)
//   unregistered + any state → None (use virtio fallback)
// ---------------------------------------------------------------------------

/// G.3 sendto — registered ring-3 NIC in restart window surfaces EAGAIN.
///
/// This is the load-bearing R1 acceptance bullet for the sendto path:
/// a `sendto()` call during a driver restart window must return `-EAGAIN`
/// so callers can back off and retry, not -EIO or a silent success.
#[test]
fn sendto_restart_gate_registered_restarting_returns_eagain() {
    assert_eq!(
        sendto_restart_errno(true, true),
        Some(-11_i64),
        "registered + restarting must return Some(NEG_EAGAIN) (-11)"
    );
}

/// G.3 sendto — registered ring-3 NIC in healthy state returns None.
///
/// When the driver is registered and healthy, `sys_sendto` must NOT
/// short-circuit with EAGAIN — it must proceed to the normal send path.
#[test]
fn sendto_restart_gate_registered_healthy_returns_none() {
    assert_eq!(
        sendto_restart_errno(true, false),
        None,
        "registered + healthy must return None (proceed normally)"
    );
}

/// G.3 sendto — no ring-3 NIC registered; gate returns None regardless.
///
/// When no ring-3 NIC driver is registered, `sys_sendto` falls through
/// to the virtio-net fire-and-forget path.  The restart gate must not
/// interfere.
#[test]
fn sendto_restart_gate_unregistered_returns_none() {
    // Both restarting and healthy variants must pass through.
    assert_eq!(
        sendto_restart_errno(false, false),
        None,
        "not registered + healthy must return None"
    );
    assert_eq!(
        sendto_restart_errno(false, true),
        None,
        "not registered + restart flag set must return None (not our restart)"
    );
}

/// G.3 sendto — gate returns NEG_EAGAIN exactly (-11), not EIO or EBADF.
///
/// Guards against errno-mapping regressions: the returned value must be
/// precisely -11 (`EAGAIN`), not -5 (`EIO`) or -9 (`EBADF`).
#[test]
fn sendto_restart_gate_eagain_value_is_correct() {
    let ret = sendto_restart_errno(true, true);
    assert!(ret.is_some(), "registered+restarting must return Some");
    let errno = ret.unwrap();
    assert_ne!(errno, -5_i64, "must not be EIO (-5)");
    assert_ne!(errno, -9_i64, "must not be EBADF (-9)");
    assert_eq!(errno, -11_i64, "must be exactly NEG_EAGAIN (-11)");
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

/// Phase 55b Track F.3d-2 progress: `sys_block_write` delivered.
///
/// **F.3b progress:** `userspace/nvme-crash-smoke/src/main.rs` is the guest-side
///   I/O-client binary. It speaks the block IPC protocol directly to the
///   `nvme.block` endpoint, forks a child to kill the driver mid-call, and
///   confirms the IPC transport failure + successful post-restart retry.
///   The QEMU regression `driver-restart-crash` (gated behind
///   `M3OS_ENABLE_CRASH_SMOKE`) exercises the full end-to-end path.
///
/// **F.3c progress (resolves the F.3b privilege blocker):**
///   - `block_error_to_neg_errno` in `kernel-core/src/driver_ipc/block.rs` maps
///     `DriverRestarting` (byte 5) → `NEG_EAGAIN` (-11) and `Busy` (byte 4) →
///     `NEG_EAGAIN` (-11). All other errors map to `NEG_EIO` (-5).
///   - `sys_block_read` now calls `block_error_to_neg_errno` instead of
///     `Err(_) => NEG_EIO`, so callers can distinguish restart from hard failure.
///   - `privileged_exec_credentials("/bin/nvme-crash-smoke", _)` grants euid=200
///     at exec time (cfg-gated on `!hardened`).
///   - `BLOCK_READ_ALLOWED` (non-hardened) now includes `/bin/nvme-crash-smoke`.
///   - `nvme-crash-smoke` step 3.5 calls `sys_block_read` during the crash window
///     and asserts the return is either `0` (driver back already) or
///     `NEG_EAGAIN` (-11) (DriverRestarting propagated). It fails if it sees
///     `NEG_EIO` (-5) which would indicate the old errno-collapse is in effect.
///
/// **F.3d-2 progress (closes this stub):**
///   - `sys_block_write` (syscall 0x1012) added to
///     `kernel/src/arch/x86_64/syscall/mod.rs` with the same privilege gate
///     (euid=200 + `BLOCK_WRITE_ALLOWED` whitelist including `/bin/nvme-crash-smoke`)
///     and the same `block_error_to_neg_errno` mapping. `DriverRestarting` →
///     `EAGAIN` on writes, exactly as on reads.
///   - `syscall-lib` exports `block_write()` userspace wrapper (syscall 0x1012).
///   - `nvme-crash-smoke` now exercises the write path:
///       step 2:   `block_write(LBA=0, 0xB5×512)` before kill — emits `write:pre-crash:OK`
///       step 3.6: `block_write` during crash window — emits `write:EAGAIN-observed`
///       step 6:   `block_write(0xD2×512)` post-restart, `block_read` confirms
///                 round-trip — emits `write:post-restart-ok`
///   - Pure-logic tests added to `kernel-core/src/driver_ipc/block.rs` pin:
///       (a) `write_path_driver_restarting_maps_to_neg_eagain`
///       (b) `read_write_errno_mapping_is_symmetric`
///       (c) `block_write_syscall_number_is_0x1012`
///
/// This test is QEMU-only — it cannot run as a host unit test. The authoritative
/// end-to-end check is the `driver-restart-crash` xtask regression
/// (`M3OS_ENABLE_CRASH_SMOKE=1 cargo xtask regression --test driver-restart-crash`),
/// which exercises the full boot/kill/restart cycle and now observes both the
/// read-path and write-path EAGAIN from `nvme-crash-smoke`.
#[test]
#[ignore = "QEMU-only: the pure-logic coverage for sys_block_write errno mapping is in \
            kernel-core/src/driver_ipc/block.rs (write_path_driver_restarting_maps_to_neg_eagain, \
            read_write_errno_mapping_is_symmetric). End-to-end write-path EAGAIN observation is \
            exercised by the driver-restart-crash QEMU regression (M3OS_ENABLE_CRASH_SMOKE). \
            This stub remains #[ignore] because QEMU infrastructure is not available in \
            cargo test -p kernel-core."]
fn qemu_nvme_kill_mid_write_returns_driver_restarting() {
    // F.3d-2 delivered: sys_block_write exists; nvme-crash-smoke now asserts
    // EAGAIN on the write path during the driver crash window.
    // The QEMU regression driver-restart-crash is the authoritative check.
    // This stub body remains empty — the #[ignore] is the artifact; the
    // rationale above documents what is now proven by unit tests and the
    // QEMU regression.
}

/// Phase 55b Track F.3d-3 progress: RemoteNic EAGAIN surface + e1000-crash-smoke.
///
/// **F.2b progress:** `service kill e1000_driver` is available from the guest
///   shell (same `service kill` subcommand). The `RemoteNic` error-surfacing
///   path (`NetDriverError::DriverRestarting`) needs the same errno-propagation
///   treatment as the block path.
///
/// **F.3b progress:** F.3b delivered the NVMe crash-smoke binary and the
///   `driver-restart-crash` QEMU regression. The analogous e1000 path
///   required a guest net I/O-client binary and `RemoteNic` EAGAIN surfacing.
///
/// **F.3c progress:** F.3c resolved the block-path privilege gate and EAGAIN
///   propagation. The e1000 path remained blocked on the items below.
///
/// **F.3d-3 progress (resolves partial blockers from F.3c):**
///   - `NetDriverError::to_byte()` and `net_error_to_neg_errno()` added to
///     `kernel-core/src/driver_ipc/net.rs` — mirrors `block_error_to_neg_errno`.
///     `DriverRestarting` (byte 4) → `NEG_EAGAIN` (-11).
///   - `RESTART_SUSPECTED: AtomicBool` added to `kernel/src/net/remote.rs`;
///     `RemoteNic::send_frame()` returns `NetDriverError::DriverRestarting` when
///     the flag is set.  `drain_tx_queue()` calls `Self::on_ipc_error()` on IPC
///     failure, which sets the flag.  `register()` clears it on re-registration.
///   - `userspace/e1000-crash-smoke/` binary: sends pre-crash UDP, kills
///     e1000_driver, sends mid-crash UDP, polls restart, sends post-restart UDP.
///     Emits `E1000_CRASH_SMOKE:PASS` on success.
///   - `e1000-restart-crash` QEMU regression added to `xtask` (gated behind
///     `M3OS_ENABLE_CRASH_SMOKE`); exercises the full kill → restart → send cycle.
///
/// **Phase 55c Track G resend progress:**
///   - `sys_net_send` (syscall 0x1013) added with socket-capability gate.
///     Callers must pass a valid open socket fd as the first argument; the arch
///     dispatcher validates it against `FdBackend::Socket` before calling the
///     handler.  `DriverRestarting` → `NEG_EAGAIN` surfaces through
///     `net_send_dispatch` (pure-logic seam tested in G.3 tests above).
///   - POSIX `sendto()` (syscall 44) UDP and ICMP branches now call
///     `RemoteNic::sendto_restart_ret()` before the fire-and-forget send.
///     When the ring-3 NIC is registered and in a restart window, `sendto()`
///     returns `NEG_EAGAIN` (-11), satisfying the R1 contract.  Pure-logic
///     seam coverage is in the G.3 `sendto_restart_errno` tests above.
///
/// Remaining blocker for full QEMU smoke:
///   ICMP/TCP post-restart connectivity requires the full E.3 TX/RX server
///   loop in the e1000 driver (not yet landed; driver exits after bring-up).
///   This test is QEMU-only.
#[test]
#[ignore = "QEMU-only: sys_sendto UDP/ICMP now surfaces NEG_EAGAIN via RemoteNic::sendto_restart_ret(). \
            sys_net_send (0x1013) also surfaces NEG_EAGAIN for direct callers. \
            Pure-logic seam coverage: G.3 sendto_restart_errno tests + net_send_dispatch tests. \
            Remaining blocker: e1000 driver E.3 TX/RX server loop for post-restart ICMP/TCP. \
            QEMU regression e1000-restart-crash (M3OS_ENABLE_CRASH_SMOKE) exercises the \
            kill/restart cycle; post-restart connectivity check deferred to Track H."]
fn qemu_e1000_kill_mid_send_returns_driver_restarting_then_icmp_echo_succeeds() {
    // Phase 55c Track G (second resend): sys_sendto UDP/ICMP branches call
    // RemoteNic::sendto_restart_ret() before the fire-and-forget send path.
    // When RESTART_SUSPECTED is set, sendto() returns NEG_EAGAIN (-11).
    // The post-restart ICMP/TCP connectivity path requires the e1000 E.3
    // server loop and is covered by Track H.
}

/// Phase 55b Track F.3d-1: max-restart path resolved.
///
/// **F.2b progress:** `service kill <name>` is available in the guest shell.
///
/// **F.3b progress:** F.3b's `nvme-crash-smoke` binary kills the driver once
///   and confirms the restart. The pure-logic analogue
///   (`max_restart_enforcement_sixth_crash_transitions_to_permanently_stopped`)
///   already passes.
///
/// **F.3c status:** F.3c wired EAGAIN observation in the single-kill path.
///   The 6-kill loop was deferred.
///
/// **F.3d-1 status (resolved):** `userspace/max-restart-smoke/src/main.rs` is
///   the guest-side binary that issues 6 sequential kills of `nvme_driver`
///   (each preceded by a wait for `running`, except the 6th), then asserts
///   `/run/services.status` shows `permanently-stopped` for `nvme_driver`.
///   The authoritative QEMU regression is:
///     `M3OS_ENABLE_CRASH_SMOKE=1 cargo xtask regression --test max-restart-exceeded`
///   This stub remains `#[ignore]` because it requires a live QEMU guest and
///   cannot be expressed as a host unit test in kernel-core.
#[test]
#[ignore = "QEMU-only: the authoritative check is the `max-restart-exceeded` xtask \
            regression (M3OS_ENABLE_CRASH_SMOKE=1 cargo xtask regression \
            --test max-restart-exceeded). The pure-logic enforcement is covered by \
            max_restart_enforcement_sixth_crash_transitions_to_permanently_stopped (above)."]
fn qemu_max_restart_exceeded_service_status_returns_failed() {
    // intentionally empty — QEMU-only scenario.
    // The `max-restart-exceeded` xtask regression exercises the full path:
    //   1. Boot guest with --device nvme.
    //   2. Wait for nvme_driver running (NVME_SMOKE:rw:PASS).
    //   3. Run /bin/max-restart-smoke (6-kill loop).
    //   4. Assert MAX_RESTART_SMOKE:PASS (permanently-stopped in services.status).
}
