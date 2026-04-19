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
//! **That harness does not exist yet.** The supervised-restart scaffolding from
//! Track F.2 (`driver_restart_regression`) brings the process-lifecycle piece;
//! injecting a *wrong* capability from userspace requires a dedicated
//! "negative-path driver" binary that can be composed with the F.2 harness.
//! That work is deferred to a future phase, tracked as:
//!
//! > TODO(phase-55c): end-to-end cross-device negative path — spawn both
//! > drivers, use wrong cap handle, assert `-EBADF` at the syscall return.
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
#[ignore = "requires end-to-end driver spawn harness (TODO phase-55c)"]
fn cross_device_mmio_denied_end_to_end() {
    // Covered at the kernel registry level by `cross_device_mmio_denied`
    // in kernel/src/main.rs. See module-level doc for gap analysis.
    todo!("end-to-end cross-device MMIO denial: needs supervised spawn harness")
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
#[ignore = "requires end-to-end driver spawn harness (TODO phase-55c)"]
fn cross_device_dma_denied_end_to_end() {
    // Covered at the kernel registry level by `cross_device_dma_denied`
    // in kernel/src/main.rs. See module-level doc for gap analysis.
    todo!("end-to-end cross-device DMA denial: needs supervised spawn harness")
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
#[ignore = "requires negative-path driver binary (TODO phase-55c)"]
fn capability_forge_denied_end_to_end() {
    // Covered at the kernel registry level by `capability_forge_denied`
    // in kernel/src/main.rs. See module-level doc for gap analysis.
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
#[ignore = "requires F.2 supervised restart harness (TODO phase-55c)"]
fn post_crash_handles_invalid_end_to_end() {
    // Covered at the kernel registry level by
    // `post_crash_handles_invalid_in_restarted_process` in kernel/src/main.rs.
    // See module-level doc for gap analysis.
    todo!("end-to-end post-crash handle invalidation: needs F.2 restart harness")
}
