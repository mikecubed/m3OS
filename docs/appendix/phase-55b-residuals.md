# Phase 55b Residuals — Scheduling Record

**Status:** Closed — R1 and R2 resolved in Phase 55c; annotations and metrics remain correctly documented
**Source:** Phase 55b (Ring-3 Driver Host) closure pass
**Discovered:** During the Phase 55b closure work (waves 15–17); Phase 55b itself is landed as of v0.55.2
**Audience:** Roadmap planning and closure tracking — the two *real* items below are assigned to Phase 55c

## Why this document exists

Phase 55b's architectural goal (ring-3 NVMe + e1000 drivers, IOMMU-isolated, supervised, restartable) is delivered and live at runtime. During the closure pass two real follow-ups surfaced that do **not** belong to Phase 55b's scope but cannot be ignored. This doc records them precisely so their later owner — Phase 55c — stays explicit rather than getting lost in commit history.

It also inventories the `#[ignore]` test stubs that are *correctly* ignored (not gaps) so future readers don't mistake them for debt.

## Section 1 — Real follow-ups assigned to Phase 55c

### R1 — `sys_net_send` syscall for userspace-observable EAGAIN

Closed in Phase 55c (Track G/H). See `docs/roadmap/55c-ring-3-driver-correctness-closure.md` and `docs/post-mortems/2026-04-22-e1000-bound-notif.md`.

### R2 — IOMMU VT-d MMIO translation breaks ring-3 driver `CTRL.RST` under `--iommu`

Closed in Phase 55c (Track C/D). See `docs/roadmap/55c-ring-3-driver-correctness-closure.md` and `docs/post-mortems/2026-04-22-e1000-bound-notif.md`.

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

| Item | Severity | Owner | Must-fix-before-1.0? | Status |
|---|---|---|---|---|
| **R1** `sys_net_send` | Medium | Phase 55c (Ring-3 Driver Correctness Closure) | **Yes** | ✅ **Closed in Phase 55c (Track G/H)** |
| **R2** IOMMU VT-d MMIO | Medium-High | Phase 55c (Ring-3 Driver Correctness Closure) | **Yes** | ✅ **Closed in Phase 55c (Track C/D)** |
| §2 annotations | None | No phase | No — correct as-is | N/A |
| §3 metrics | None | No phase | No — documented in learning doc | N/A |

## Related docs

- `docs/55b-ring-3-driver-host.md` — Phase 55b learning doc (contains the Outcome Metrics section)
- `docs/roadmap/55b-ring-3-driver-host.md` — Phase 55b design doc
- `docs/roadmap/tasks/55b-ring-3-driver-host-tasks.md` — Phase 55b task list
- `docs/roadmap/55c-ring-3-driver-correctness-closure.md` — owner of R1 and R2
- `docs/roadmap/tasks/55c-ring-3-driver-correctness-closure-tasks.md` — execution plan for R1 and R2
- `docs/roadmap/58-release-1-0-gate.md` — gate that R2 must clear
