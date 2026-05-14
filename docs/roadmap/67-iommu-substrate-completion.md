# Phase 67 - IOMMU Substrate Completion

**Status:** Complete
**Source Ref:** phase-67
**Depends on:** Phase 55a (IOMMU Substrate) ✅, Phase 55b (Ring-3 Driver Host) ✅, Phase 55c (Ring-3 Driver Correctness Closure) ✅
**Co-existing tip:** Phase 66 (Security and Hygiene Closeout) — Phase 67 lands on top of the 56/57/63/63a/64/66 tip; no surface contested by those phases is touched here
**Builds on:** Closes the AMD-Vi and VT-d items that Phase 55a deferred or left as TODOs; replaces the four `todo!()` isolation-test scaffolds from Phase 55c with a real supervised-spawn + CapHandle injection harness
**Primary Components:** kernel/src/iommu/amd.rs, kernel/src/iommu/intel.rs, userspace/drivers/nvme/tests/isolation.rs, kernel-core/iommu

## Milestone Goal

The IOMMU substrate is complete: AMD-Vi installs a fault ISR and decodes fault events; VT-d uses queued invalidation instead of the register-based path; VT-d scalable mode is brought up on hardware that reports it; AMD-Vi groups multiple BDFs into a shared domain per IVRS grouping; the four isolation-test `todo!()` scaffolds are replaced with passing tests that use a supervised-spawn + CapHandle injection harness. The Phase 55a design doc's "Known Open Bug" section is updated to reflect 55c R2 closure.

## Why This Phase Exists

Phase 55a declared its AMD-Vi fault ISR installation as Track E work that would be done, but `kernel/src/iommu/amd.rs:938` retains a `// Track E TODO` comment. VT-d scalable mode is hardcoded `false` at `intel.rs:178`, and VT-d has no per-domain queued-invalidation API at all — `VtdUnit::bring_up` (around `intel.rs:715`) fires a single global context-cache + IOTLB invalidation through register writes and that is the only flush path. These are not cosmetic deferrals: without fault dispatch the IOMMU cannot report DMA violations; without queued invalidation the VT-d implementation has no per-domain flush primitive and is incompatible with scalable mode on modern hardware.

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

Today `VtdUnit::bring_up` (around `intel.rs:715`) issues a one-shot global context-cache + IOTLB invalidation through `invalidate_context_cache_global` / `invalidate_iotlb_global` and no per-domain flush API exists. Add new `flush_domain`, `flush_iotlb`, and `flush_context` methods that submit typed descriptors to a queued-invalidation ring allocated at bring-up and poll the tail counter. The existing global-invalidation calls at bring-up stay — they fire before the queue is up, and queued invalidation is not yet available at that point. The `IommuUnit` trait surface is unchanged.

### VT-d scalable mode bring-up

When the DMAR capability register reports scalable mode support and CR4.LA57 is set, the VT-d implementation constructs 5-level page tables instead of 4-level. The `intel.rs:178` `false` constant is replaced by a runtime capability check. Systems without scalable mode continue using the existing 4-level path.

### AMD-Vi multi-BDF domain grouping

The IVRS parser in `kernel-core::iommu::tables` already exposes `IvhdDeviceEntry::AliasSelect` and `IvhdDeviceEntry::AliasStartRange`. This phase adds a BDF-grouping helper (union-find over those alias entries) and threads it through `AmdVi::group_bdf_domains` so claimed BDFs that share an IVRS alias share a single `DomainId`. Before this phase each claimed BDF received an independent domain regardless of IVRS grouping.

### IOMMU isolation tests

Replace the four `todo!()` bodies in `userspace/drivers/nvme/tests/isolation.rs` (`cross_device_mmio_denied_end_to_end`, `cross_device_dma_denied_end_to_end`, `capability_forge_denied_end_to_end`, `post_crash_handles_invalid_end_to_end`) with real tests using a supervised-spawn harness, and add one new test `driver_restart_resets_domain` covering the cross-restart domain-recycle path. Each test spawns one or more NVMe userspace driver instances, injects a `CapHandle` for a DMA buffer that belongs to a different domain (or a forged / stale handle), and verifies the kernel rejects the operation at the IOMMU or capability check point rather than allowing it through.

## Important Components and How They Work

### `kernel/src/iommu/amd.rs` — fault ISR and event log drain

`install_fault_handler` registers an interrupt handler that points to `drain_event_log`. `drain_event_log` reads the tail pointer, iterates unprocessed entries, calls `decode_event_log_entry` for each, and advances the head pointer. The loop is bounded to the ring size; excess events are counted and logged.

### `kernel-core/src/iommu/amd.rs` — `AmdViFaultEvent` and decoder

Pure-logic decoder, host-testable. Input: 128-bit raw entry bytes. Output: `AmdViFaultEvent { requestor_bdf, fault_code: AmdViFaultCode, iova, flags }`. Property tests cover all defined `AmdViFaultCode` values.

### `kernel/src/iommu/intel.rs` — queued invalidation and scalable mode

`bring_up` allocates the invalidation queue ring page and writes the IQA register. `flush_domain(domain_id)` submits a Context Cache invalidation descriptor and an IOTLB invalidation descriptor, then polls the tail register. `init_page_tables` checks `cap_reg.srs()` and `cr4.la57()` and constructs 5-level tables when both are true.

### `userspace/drivers/nvme/tests/isolation.rs` — real isolation test harness

A `SupervisedSpawn` helper starts the NVMe driver binary under the test supervisor. `CapHandle::inject_foreign_dma` creates a `DmaBuffer` in domain A and hands its bus address to domain B's driver instance. The four existing scaffolds (`cross_device_mmio_denied_end_to_end`, `cross_device_dma_denied_end_to_end`, `capability_forge_denied_end_to_end`, `post_crash_handles_invalid_end_to_end`) exercise cross-device denial, capability-forge denial, and post-crash handle invalidation; the new `driver_restart_resets_domain` covers domain recycling across a supervised restart. Each test asserts the failing syscall returns the expected negative errno (`-EFAULT` / `-EBADF`) rather than succeeding.

## How This Builds on Earlier Phases

- Extends Phase 55a's AMD-Vi and VT-d implementations in-place without changing the `IommuUnit` trait surface.
- Reuses the existing IVHD parser in `kernel-core::iommu::tables` (alias variants already decoded and round-trip tested); this phase adds a grouping helper rather than a parallel parser.
- Uses the Phase 55b supervised driver spawn infrastructure for the isolation test harness.
- Uses Phase 55c's `CapHandle` and domain lifetime mechanics to construct the cross-domain injection scenario.
- Updates Phase 55a and 55c design docs to mark the closed items.
- Lands on top of the 56/57/63/63a/64/66 tip without touching any surface those phases introduced (no compositor, audio, session-manager, or DOOM-stack churn).

## Implementation Outline

The AMD-Vi and VT-d branches share structural symmetry: both expose a fault-reporting path, both require a flush-invalidation pipeline, and both need a domain-grouping table. Abstract where natural — `kernel-core::iommu` should contain the pure-logic decoder and IVRS parser shared by both branches, while `kernel/src/iommu/amd.rs` and `kernel/src/iommu/intel.rs` retain the hardware-specific ISR and ring management. Avoid duplicating the event-log drain loop shape across the two files.

Follow TDD for the pure-logic components: write host-side tests for `decode_event_log_entry` (covering all seven `AmdViFaultCode` variants) and the new BDF-grouping helper (union-find over the existing `IvhdDeviceEntry::AliasSelect` / `AliasStartRange` records) before integrating them into the kernel ISR. The isolation tests in Track F are the QEMU top of this test pyramid — they cannot replace the host-side decoder tests.

1. Write host-side tests for `AmdViFaultEvent` decoder in new `kernel-core/src/iommu/amd.rs`; then implement `install_fault_handler` and `drain_event_log` in `kernel/src/iommu/amd.rs`.
2. Implement VT-d queued invalidation ring at bring-up; add new `flush_domain`/`flush_iotlb`/`flush_context` methods that submit descriptors and poll IQH; leave the global bring-up invalidation untouched.
3. Add scalable-mode page-table construction behind runtime capability check.
4. Add a BDF-grouping helper that consumes the existing `IvhdDeviceEntry` alias variants in `kernel-core/src/iommu/tables.rs`; implement `AmdVi::group_bdf_domains`.
5. Implement `SupervisedSpawn` and `CapHandle::inject_foreign_dma` in the NVMe test harness.
6. Replace four `todo!()` bodies and add a fifth `driver_restart_resets_domain` test.
7. Update Phase 55a design doc (retitle "Known Open Bug" section to record 55c R2 closure; add Phase 67 completion note); back-fill an Isolation Tests track in the Phase 55c task doc and annotate it as implemented in Phase 67.

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
