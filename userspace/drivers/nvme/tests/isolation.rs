//! Phase 55b Track F.3 — Cross-device capability isolation: driver-side stubs.
//!
//! ## Gap analysis and honest scope statement
//!
//! The four F.3 acceptance items are fully validated at the *kernel registry
//! level* by the `#[test_case]` functions in `kernel/src/main.rs`:
//!
//! | Acceptance item | Kernel test |
//! |---|---|
//! | Test 1 — cross-device MMIO denied | `cross_device_mmio_denied` |
//! | Test 2 — cross-device DMA denied  | `cross_device_dma_denied` |
//! | Test 3 — forged CapHandle denied  | `capability_forge_denied` |
//! | Test 4 — post-crash handles invalid | `post_crash_handles_invalid_in_restarted_process` |
//!
//! A true *end-to-end* driver-side test would:
//!
//! 1. Spawn two ring-3 driver processes (`nvme_driver` and `e1000_driver`).
//! 2. Hand each a `Capability::Device` for its own BDF via `sys_device_claim`.
//! 3. Have the NVMe process call `sys_device_mmio_map` / `sys_device_dma_alloc`
//!    passing the *e1000's* capability handle — receive `-EBADF` back.
//! 4. Kill the NVMe process, restart it, and verify the old CapHandle values
//!    are rejected by the kernel on the new PID.
//!
//! This requires a test-harness capable of:
//! - Forking supervised driver processes with their own address spaces.
//! - Injecting an arbitrary `CapHandle` value into a process's syscall arguments
//!   (or synthesising a driver that deliberately calls with a stolen handle).
//! - Observing the kernel's errno return across the process boundary.
//!
//! ## Phase 55b F.3c status
//!
//! **F.3b progress:** Track F.3b delivered `userspace/nvme-crash-smoke/` which
//! exercises the crash-and-restart lifecycle from the guest side (kill →
//! IPC transport failure → restart → retry success). The cross-process capability
//! injection scenario is a different and harder problem — it requires a "negative-
//! path driver" binary that deliberately passes a wrong `CapHandle` in a syscall
//! and a harness that can correlate the kernel's errno return to the injected value.
//! That harness still does not exist.
//!
//! **F.3c progress:** Track F.3c resolved the privilege gate for `nvme-crash-smoke`
//! (euid=200, BLOCK_READ_ALLOWED whitelist, EAGAIN propagation from `sys_block_read`).
//! This is orthogonal to the capability-isolation stubs below — the cap-injection
//! harness is the specific blocker for all four stubs, and F.3c did not build it.
//!
//! The stubs below remain deferred to phase-55c:
//!
//! > TODO(phase-55c): end-to-end cross-device negative path — spawn both
//! > drivers, use wrong cap handle, assert `-EBADF` at the syscall return.
//! > Specific blocker: "negative-path driver" binary + test supervisor that can
//! > inject a stolen `CapHandle` integer into a live driver process's syscall and
//! > read back the kernel's errno return across the process boundary.
//!
//! The stubs below mark where those tests will live so the file structure
//! survives intact when the harness is built.

// Suppress the "file is not in module tree" lint when built as a standalone
// test binary (the nvme_driver crate is no_std; tests here would need std
// once the harness lands).
#![allow(dead_code)]

/// Placeholder — end-to-end cross-device MMIO denial test.
///
/// When implemented this test should:
/// - Spawn `nvme_driver` and `e1000_driver` under the test supervisor.
/// - Extract the e1000 `CapHandle` value from the driver-host registry.
/// - Call `sys_device_mmio_map` from the NVMe driver process using that handle.
/// - Assert the syscall returns `-EBADF`.
/// - Assert the process VA layout is unchanged (no mapping installed).
///
/// # Deferred
///
/// Requires end-to-end supervised spawn + cross-process handle injection
/// harness. Tracked as TODO(phase-55c).
#[test]
#[ignore = "phase-55c deferred: F.3c resolved nvme-crash-smoke privilege gate (EAGAIN path); \
            cross-device MMIO denial end-to-end is blocked on supervised spawn + CapHandle \
            injection harness — orthogonal to F.3c privilege work. \
            Covered at kernel level by cross_device_mmio_denied in kernel/src/main.rs."]
fn cross_device_mmio_denied_end_to_end() {
    // Covered at the kernel registry level by `cross_device_mmio_denied`
    // in kernel/src/main.rs. See module-level doc for gap analysis.
    // F.3c blocker: "negative-path driver" binary that deliberately passes the
    // e1000 CapHandle to sys_device_mmio_map + a test supervisor that can observe
    // the kernel's -EBADF return across the process boundary.
    todo!("end-to-end cross-device MMIO denial: needs supervised spawn + cap-injection harness")
}

/// Placeholder — end-to-end cross-device DMA denial test.
///
/// When implemented this test should:
/// - Spawn both drivers and extract the e1000 BDF claim handle.
/// - Have the NVMe driver call `sys_device_dma_alloc` targeting the e1000 IOMMU
///   domain (passing e1000's `CapHandle`).
/// - Assert the syscall returns `-EBADF`.
/// - Assert the DMA registry has no new entry for the NVMe driver's PID.
///
/// # Deferred
///
/// Requires end-to-end supervised spawn + cross-process handle injection
/// harness. Tracked as TODO(phase-55c).
#[test]
#[ignore = "phase-55c deferred: F.3c resolved nvme-crash-smoke privilege gate (EAGAIN path); \
            cross-device DMA denial end-to-end is blocked on supervised spawn + CapHandle \
            injection harness — orthogonal to F.3c privilege work. \
            Covered at kernel level by cross_device_dma_denied in kernel/src/main.rs."]
fn cross_device_dma_denied_end_to_end() {
    // Covered at the kernel registry level by `cross_device_dma_denied`
    // in kernel/src/main.rs. See module-level doc for gap analysis.
    // F.3c blocker: "negative-path driver" binary that deliberately passes the
    // e1000 IOMMU domain CapHandle to sys_device_dma_alloc + a test supervisor
    // that can observe -EBADF across the process boundary.
    todo!("end-to-end cross-device DMA denial: needs supervised spawn + cap-injection harness")
}

/// Placeholder — end-to-end forged CapHandle denial test.
///
/// When implemented this test should:
/// - Spawn `nvme_driver` and synthesise a plausible `CapHandle` integer it
///   never received.
/// - Call any device-host syscall using that handle.
/// - Assert the syscall returns `-EBADF` and no side-effect is observable.
///
/// # Deferred
///
/// Requires a "negative-path driver" binary capable of deliberate wrong-handle
/// syscalls. Tracked as TODO(phase-55c).
#[test]
#[ignore = "phase-55c deferred: F.3c resolved nvme-crash-smoke privilege gate (EAGAIN path); \
            forged CapHandle denial end-to-end requires a negative-path driver binary that \
            deliberately passes a synthesised CapHandle integer in a device-host syscall. \
            Covered at kernel level by capability_forge_denied in kernel/src/main.rs."]
fn capability_forge_denied_end_to_end() {
    // Covered at the kernel registry level by `capability_forge_denied`
    // in kernel/src/main.rs. See module-level doc for gap analysis.
    // F.3c blocker: a binary that constructs a plausible-but-unissued CapHandle
    // integer and passes it to any device-host syscall; a test supervisor that
    // can read the kernel's -EBADF return across the process boundary without
    // the process being able to observe it through normal error channels.
    todo!("end-to-end forged cap denial: needs negative-path driver binary")
}

/// Placeholder — end-to-end post-crash CapHandle invalidation test.
///
/// When implemented this test should:
/// - Record a live CapHandle value from the NVMe driver before killing it.
/// - Kill the driver via `SIGKILL` through the supervisor.
/// - Wait for the driver to be restarted by the supervisor (F.2 restart path).
/// - Attempt to use the pre-crash CapHandle value in the restarted process.
/// - Assert the kernel returns `-EBADF` for all pre-crash handle values.
///
/// # Deferred
///
/// Requires the F.2 supervised-restart harness and a way to pass a raw handle
/// integer across the kill/restart boundary for the negative assertion.
/// Tracked as TODO(phase-55c).
#[test]
#[ignore = "phase-55c deferred: F.3c updated nvme-crash-smoke with EAGAIN observation \
            (kill → EAGAIN from sys_block_read → restart → retry success); asserting \
            pre-crash CapHandle values are rejected by the restarted process requires \
            cap-handle injection across the kill/restart boundary — no harness for that yet. \
            Covered at kernel level by post_crash_handles_invalid_in_restarted_process."]
fn post_crash_handles_invalid_end_to_end() {
    // Covered at the kernel registry level by
    // `post_crash_handles_invalid_in_restarted_process` in kernel/src/main.rs.
    // See module-level doc for gap analysis.
    // F.3c blocker: recording a live CapHandle value from nvme_driver, killing it,
    // waiting for restart (already works via nvme-crash-smoke), and then passing the
    // pre-crash handle to any device-host syscall from the restarted process and
    // reading back -EBADF requires a way to pass raw handle integers across the
    // kill/restart boundary, which no test harness currently supports.
    todo!("end-to-end post-crash handle invalidation: needs cap-handle injection harness")
}
