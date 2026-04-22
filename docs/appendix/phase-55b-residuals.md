# Phase 55b Residuals — Scheduling Record

**Status:** Assigned — two real follow-ups now owned by Phase 55c + documented annotations
**Source:** Phase 55b (Ring-3 Driver Host) closure pass
**Discovered:** During the Phase 55b closure work (waves 15–17); Phase 55b itself is landed as of v0.55.2
**Audience:** Roadmap planning and closure tracking — the two *real* items below are assigned to Phase 55c

## Why this document exists

Phase 55b's architectural goal (ring-3 NVMe + e1000 drivers, IOMMU-isolated, supervised, restartable) is delivered and live at runtime. During the closure pass two real follow-ups surfaced that do **not** belong to Phase 55b's scope but cannot be ignored. This doc records them precisely so their later owner — Phase 55c — stays explicit rather than getting lost in commit history.

It also inventories the `#[ignore]` test stubs that are *correctly* ignored (not gaps) so future readers don't mistake them for debt.

## Section 1 — Real follow-ups assigned to Phase 55c

### R1 — `sys_net_send` syscall for userspace-observable EAGAIN

**Severity:** Medium — no correctness regression, but Phase 55b's `NetDriverError::DriverRestarting` surface is architecturally complete yet invisible to userspace `sendto()` callers.

**Current state:**
- `RemoteNic::send_frame` in `kernel/src/net/remote.rs` correctly flips into the restart-suspected state on IPC transport failure and returns `NetDriverError::DriverRestarting` to kernel-side callers. Verified by `net_error_to_neg_errno_driver_restarting_is_eagain` in `kernel-core/tests/driver_restart.rs`.
- The existing net datapath for userspace (`sys_sendto`, UDP socket writes, TCP stack) routes through `net_server` and the virtio-net fallback, not through `RemoteNic::send_frame` with the error byte propagated.
- Consequence: when the e1000 driver is mid-restart, a userspace UDP `sendto()` still succeeds (virtio-net handles it) or fails generically. The ring-3-driver-specific `EAGAIN` is never visible to the application.

**What's needed:**
1. Either a new `sys_net_send` syscall that routes through `RemoteNic::send_frame` and maps `NetDriverError` through `net_error_to_neg_errno` (parallel to `sys_block_{read,write}`), **or** teach the existing UDP/TCP send path to prefer `RemoteNic` when it is registered and propagate the error byte.
2. Update `userspace/e1000-crash-smoke/` to assert `EAGAIN` on mid-crash send (today it only asserts the infrastructure steps).
3. Remove `#[ignore]` from the now-observable stub in `kernel-core/tests/driver_restart.rs`.

**Recommended owner:** **Phase 55c (Ring-3 Driver Correctness Closure)** — the ring-3-driver correctness follow-up that groups the SSH-over-e1000 wake fix, IOMMU BAR identity coverage, and userspace-visible restart handling into one pre-1.0 closure pass.

**Acceptance for closure:**
- `e1000-crash-smoke` binary observes `EAGAIN` (or equivalent) from its `sendto()` call during the mid-crash window.
- `cargo xtask regression --test e1000-restart-crash` passes end-to-end with the EAGAIN assertion enabled.
- One `#[ignore]` stub removed: `qemu_e1000_kill_mid_send_returns_driver_restarting_then_icmp_echo_succeeds`.

### R2 — IOMMU VT-d MMIO translation breaks ring-3 driver `CTRL.RST` under `--iommu`

**Severity:** Medium-High — blocks the Phase 55b "run under IOMMU-active configuration" F.4 acceptance bullet end-to-end; the device-smoke assertions correctly surface the regression.

**Symptom:**
- `cargo xtask device-smoke --device nvme --iommu` and `--device e1000 --iommu` both **time out at step 3** (the driver's bring-up/self-test step) after ~2 s.
- Root cause (per F.4b investigation): VT-d remapping covers the device MMIO window; the driver's `CTRL.RST` write (NVMe `CC.EN` / e1000 `CTRL.RST`) is silently dropped or mis-routed because the IOMMU domain does not have an identity MMIO entry for the BAR.

**Why it was invisible before F.4b:**
- The original F.4 smoke script only asserted the `init: driver.registered name=...` boot-log line, which fires during init service-config load — *before* the driver process executes.
- F.4b's tighter assertions (`NVME_SMOKE:rw:PASS`, `E1000_SMOKE:link:PASS`) reach into actual driver behaviour and correctly surface the IOMMU regression.

**What's needed:**
1. In the Phase 55a IOMMU substrate (`kernel/src/iommu/`, `kernel-core/src/iommu/`), extend the per-device domain setup to include identity-mapped MMIO windows for each claimed device's BAR regions.
2. Re-run `cargo xtask device-smoke --device nvme --iommu` and `--device e1000 --iommu`; both should pass like their non-IOMMU counterparts (~6 s each).
3. Audit any other MMIO window the ring-3 driver might touch (MSI-X tables, PCIe config writes, etc.) for the same gap.

**Recommended owner:** **Phase 55c (Ring-3 Driver Correctness Closure)** — the pre-1.0 ring-3-driver follow-up that groups the SSH-over-e1000 wake fix, IOMMU BAR identity coverage, and userspace-visible restart handling into one closure pass. If 1.0 shipping claims "IOMMU-isolated ring-3 drivers", **this must close before Phase 58**.

**Acceptance for closure:**
- `cargo xtask device-smoke --device nvme --iommu` passes end-to-end.
- `cargo xtask device-smoke --device e1000 --iommu` passes end-to-end.
- Kernel-side invariant test in `kernel-core/src/iommu/` verifying BAR-MMIO identity coverage for any claimed device.

## Section 2 — Correct `#[ignore]` annotations (not gaps, just documentation)

The following remain `#[ignore]`-d by design. They are **not** work items.

| File | Stub | Why it's correctly ignored |
|---|---|---|
| `kernel-core/tests/driver_restart.rs` | `qemu_nvme_kill_mid_write_returns_driver_restarting` | QEMU-only test. Cannot run as host unit test. Authoritative check is `cargo xtask regression --test driver-restart-crash`. |
| `kernel-core/tests/driver_restart.rs` | `qemu_max_restart_exceeded_service_status_returns_failed` | QEMU-only. Authoritative check is `cargo xtask regression --test max-restart-exceeded`. |
| `kernel-core/tests/driver_restart.rs` | `qemu_e1000_kill_mid_send_returns_driver_restarting_then_icmp_echo_succeeds` | Currently QEMU-only; will fully close when R1 (`sys_net_send`) lands. Authoritative check is `cargo xtask regression --test e1000-restart-crash` (without the EAGAIN assertion until R1). |
| `userspace/drivers/nvme/tests/isolation.rs` | `cross_device_mmio_denied_end_to_end` | Covered at kernel registry level by `cross_device_mmio_denied` in `kernel/src/main.rs` (passes in QEMU). The userspace end-to-end variant would duplicate the kernel-level proof. |
| `userspace/drivers/nvme/tests/isolation.rs` | `cross_device_dma_denied_end_to_end` | Covered at kernel level by `cross_device_dma_denied` (passes in QEMU). |
| `userspace/drivers/nvme/tests/isolation.rs` | `capability_forge_denied_end_to_end` | Covered at kernel level by `capability_forge_denied` (passes in QEMU). |
| `userspace/drivers/nvme/tests/isolation.rs` | `post_crash_handles_invalid_end_to_end` | Covered at kernel level by `post_crash_handles_invalid_in_restarted_process` (passes in QEMU). |

If a future phase reshapes the test harness so these become runnable, great — but shipping them as `#[ignore]` with precise pointers to the authoritative coverage is the correct state, not a deferral.

## Section 3 — LOC-metric observations (documented, not fixable)

The Phase 55b task doc set two measurement targets that the delivered work missed:

| Metric | Target | Actual | Cause |
|---|---|---|---|
| Net kernel LOC | ≤ −1800 | **+1917** | New `kernel/src/syscall/device_host.rs` is 2204 LOC of ring-0 syscall infrastructure the task doc didn't budget for |
| Driver-isolation LOC delta | ≤ −1800 | **−1597** | Facades grew beyond the ≤300 combined target |
| Facade size (`blk/remote.rs` + `net/remote.rs`) | ≤ 300 combined | **518 combined** | `net/remote.rs` (310 LOC) grew past ~150 to implement RX-routing + link-state reset over IPC |

These are **architectural accounting misses**, not engineering bugs. The phase's actual outcome (drivers in ring 3, IOMMU-isolated, supervised, restartable) is achieved. The learning doc at `docs/55b-ring-3-driver-host.md` records these numbers honestly in its "Outcome Metrics" section so Phase 58 1.0-gate accounting is accurate.

These do **not** belong in a future phase's scope.

## Section 4 — Scheduling summary

| Item | Severity | Recommended owner | Must-fix-before-1.0? |
|---|---|---|---|
| **R1** `sys_net_send` | Medium | Phase 55c (Ring-3 Driver Correctness Closure) | **Yes** — the pre-1.0 ring-3 driver story now depends on userspace-visible restart handling |
| **R2** IOMMU VT-d MMIO | Medium-High | Phase 55c (Ring-3 Driver Correctness Closure) | **Yes** — 1.0 claims "IOMMU-isolated ring-3 drivers"; R2 makes the claim partially false under `--iommu` |
| §2 annotations | None | No phase | No — correct as-is |
| §3 metrics | None | No phase | No — documented in learning doc |

## Related docs

- `docs/55b-ring-3-driver-host.md` — Phase 55b learning doc (contains the Outcome Metrics section)
- `docs/roadmap/55b-ring-3-driver-host.md` — Phase 55b design doc
- `docs/roadmap/tasks/55b-ring-3-driver-host-tasks.md` — Phase 55b task list
- `docs/roadmap/55c-ring-3-driver-correctness-closure.md` — owner of R1 and R2
- `docs/roadmap/tasks/55c-ring-3-driver-correctness-closure-tasks.md` — execution plan for R1 and R2
- `docs/roadmap/58-release-1-0-gate.md` — gate that R2 must clear
