# Phase 67 - IOMMU Substrate Completion

**Status:** Planned
**Source Ref:** phase-67
**Depends on:** Phase 55a (IOMMU Substrate) ✅, Phase 55b (Ring-3 Driver Host) ✅, Phase 55c (Ring-3 Driver Correctness Closure) ✅
**Builds on:** Closes the AMD-Vi and VT-d items that Phase 55a deferred or left as TODOs; replaces the four `todo!()` isolation-test scaffolds from Phase 55c with a real supervised-spawn + CapHandle injection harness
**Primary Components:** kernel/src/iommu/amd.rs, kernel/src/iommu/intel.rs, userspace/drivers/nvme/tests/isolation.rs, kernel-core/iommu

## Milestone Goal

The IOMMU substrate is complete: AMD-Vi installs a fault ISR and decodes fault events; VT-d uses queued invalidation instead of the register-based path; VT-d scalable mode is brought up on hardware that reports it; AMD-Vi groups multiple BDFs into a shared domain per IVRS grouping; the four isolation-test `todo!()` scaffolds are replaced with passing tests that use a supervised-spawn + CapHandle injection harness. The Phase 55a design doc's "Known Open Bug" section is updated to reflect 55c R2 closure.

## Why This Phase Exists

Phase 55a declared its AMD-Vi fault ISR installation as Track E work that would be done, but `kernel/src/iommu/amd.rs:938` retains a `// Track E TODO` comment. VT-d scalable mode is hardcoded `false` at `intel.rs:178`, and VT-d queued invalidation is bypassed in favor of a slower register-based path at `intel.rs:722`. These are not cosmetic deferrals: without fault dispatch the IOMMU cannot report DMA violations; without queued invalidation the VT-d implementation is incompatible with scalable mode on modern hardware.

The isolation tests in `userspace/drivers/nvme/tests/isolation.rs` contain four `todo!()` bodies that were Phase 55c scaffolding, never replaced with real test logic.

## Learning Goals

- Understand how AMD-Vi event log entries encode fault information (requestor ID, fault code, IOVA).
- Learn how VT-d queued invalidation differs from register-based invalidation in throughput and ordering guarantees.
- See how VT-d scalable mode enables 5-level page tables aligned with CR4.LA57.
- Understand how AMD-Vi groups multiple functions from the same device into a shared IOMMU domain.

## Feature Scope

### AMD-Vi fault ISR installation

Install a fault interrupt handler that drains the AMD-Vi event log, decodes each event record (requestor BDF, fault code, faulting IOVA, flags), and emits a structured log event. The handler runs in IRQ context with no allocation and bounded loop depth (drain at most `EVENT_LOG_RING_DEPTH` entries per invocation).

### AMD-Vi fault decoder and log/recover path

A `decode_event_log_entry` function in `kernel-core::iommu::amd` parses the raw 128-bit event record into a typed `AmdViFaultEvent` struct. The ISR calls `decode` and then either logs (non-fatal) or halts the offending device domain (fatal, e.g., invalid-device-table-entry).

### VT-d queued invalidation engine

Replace the register-based invalidation path (`intel.rs:722`) with a queued invalidation descriptor submission. The invalidation queue descriptor ring is allocated at IOMMU bring-up; `flush_domain`, `flush_iotlb`, and `flush_context` submit typed descriptors and poll the tail counter. No behavioral change to callers; the `IommuUnit` trait surface is unchanged.

### VT-d scalable mode bring-up

When the DMAR capability register reports scalable mode support and CR4.LA57 is set, the VT-d implementation constructs 5-level page tables instead of 4-level. The `intel.rs:178` `false` constant is replaced by a runtime capability check. Systems without scalable mode continue using the existing 4-level path.

### AMD-Vi multi-BDF domain grouping

Parse IVRS Device Entry records for `IVHD_DEV_ALL` and alias entries; group BDFs that the IVRS lists as sharing a domain into a single IOMMU domain at bring-up time. Before this phase each claimed BDF received an independent domain regardless of IVRS grouping.

### IOMMU isolation tests

Replace the four `todo!()` bodies in `userspace/drivers/nvme/tests/isolation.rs` with real tests using a supervised-spawn harness: spawn the NVMe userspace driver, inject a `CapHandle` for a DMA buffer that belongs to a different domain, and verify that the kernel rejects the I/O at the IOMMU check point rather than allowing the cross-domain access.

## Important Components and How They Work

### `kernel/src/iommu/amd.rs` — fault ISR and event log drain

`install_fault_handler` registers an interrupt handler that points to `drain_event_log`. `drain_event_log` reads the tail pointer, iterates unprocessed entries, calls `decode_event_log_entry` for each, and advances the head pointer. The loop is bounded to the ring size; excess events are counted and logged.

### `kernel-core/src/iommu/amd.rs` — `AmdViFaultEvent` and decoder

Pure-logic decoder, host-testable. Input: 128-bit raw entry bytes. Output: `AmdViFaultEvent { requestor_bdf, fault_code: AmdViFaultCode, iova, flags }`. Property tests cover all defined `AmdViFaultCode` values.

### `kernel/src/iommu/intel.rs` — queued invalidation and scalable mode

`bring_up` allocates the invalidation queue ring page and writes the IQA register. `flush_domain(domain_id)` submits a Context Cache invalidation descriptor and an IOTLB invalidation descriptor, then polls the tail register. `init_page_tables` checks `cap_reg.srs()` and `cr4.la57()` and constructs 5-level tables when both are true.

### `userspace/drivers/nvme/tests/isolation.rs` — real isolation test harness

A `SupervisedSpawn` helper starts the NVMe driver binary under the test supervisor. `CapHandle::inject_foreign_dma` creates a `DmaBuffer` in domain A and hands its bus address to domain B's driver instance. The test asserts that the subsequent I/O syscall returns `-EFAULT` or similar rather than succeeding.

## How This Builds on Earlier Phases

- Extends Phase 55a's AMD-Vi and VT-d implementations in-place without changing the `IommuUnit` trait surface.
- Uses the Phase 55b supervised driver spawn infrastructure for the isolation test harness.
- Uses Phase 55c's `CapHandle` and domain lifetime mechanics to construct the cross-domain injection scenario.
- Updates Phase 55a and 55c design docs to mark the closed items.

## Implementation Outline

1. Implement AMD-Vi event log drain in `amd.rs`; add `install_fault_handler`; add `AmdViFaultEvent` decoder in `kernel-core`.
2. Implement VT-d queued invalidation ring; wire `flush_domain`/`flush_iotlb`/`flush_context` to submit descriptors.
3. Add scalable-mode page-table construction behind runtime capability check.
4. Parse IVRS alias and grouping entries; implement `group_bdf_domains`.
5. Implement `SupervisedSpawn` and `CapHandle::inject_foreign_dma` in the NVMe test harness.
6. Replace four `todo!()` bodies with real isolation test logic.
7. Update Phase 55a design doc (remove "Known Open Bug" section per 55c R2 closure; add Phase 67 completion note); update Phase 55c doc.

## Acceptance Criteria

- `cargo xtask test --test iommu_amd_fault` passes: inject a malformed DMA request from the NVMe driver, observe a structured `AmdViFaultEvent` log entry.
- `cargo xtask test --test iommu_vtd_qi` passes: domain flush uses queued invalidation descriptors; confirmed by tracing the IQT register advance.
- On a simulated scalable-mode QEMU target, `cargo xtask test --test iommu_vtd_scalable` passes.
- `cargo xtask test --test isolation` passes for all four previously-`todo!()`ed test bodies.
- `grep -n 'todo!' userspace/drivers/nvme/tests/isolation.rs` returns zero lines.
- Phase 55a design doc does not contain the "Known Open Bug" section; a completion note references Phase 67.

## Companion Task List

- [Phase 67 Task List](./tasks/67-iommu-substrate-completion-tasks.md)

## How Real OS Implementations Differ

- Linux's IOMMU subsystem (`drivers/iommu/`) abstracts VT-d, AMD-Vi, ARM SMMU, and RISC-V IOMMU behind a common `iommu_ops` structure; m3OS uses a trait with two impls.
- Linux uses DMAR-based interrupt remapping in addition to DMA remapping; m3OS defers interrupt remapping.
- Production VT-d drivers use Interrupt Remapping Tables and Posted Interrupt Descriptors for MSI delivery through the IOMMU; m3OS routes MSI through the APIC without IOMMU interception.

## Deferred Until Later

- ARM SMMU or RISC-V IOMMU third impl
- Interrupt remapping through the IOMMU
- Posted Interrupt Descriptor support for MSI-X
- SR-IOV virtual function IOMMU domain allocation
- IOMMU-backed memory encryption (AMD SME/SEV)
