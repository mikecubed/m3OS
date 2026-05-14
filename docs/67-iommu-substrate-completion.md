# IOMMU Substrate Completion (Phase 67)

**Aligned Roadmap Phase:** Phase 67
**Status:** Complete
**Source Ref:** phase-67
**Supersedes Legacy Doc:** new

## Overview

Phase 67 closes the IOMMU substrate items that Phase 55a deferred or tracked as TODOs. None of the five items is a new design — each is the missing implementation behind a contract Phase 55a already declared. Phase 67 adds: an AMD-Vi fault interrupt service routine that drains the hardware event log through a typed `AmdViFaultEvent` decoder (so DMA violations surface as structured log entries instead of disappearing into firmware); a VT-d queued-invalidation engine with per-domain `flush_domain` / `flush_iotlb` / `flush_context` methods that replace the global register-path invalidation Phase 55a shipped for the trait-level flush API; runtime detection of VT-d scalable mode plus a 5-level page-table path that engages when `ECAP.SMTS` is set and CPU LA57 is enabled; AMD-Vi multi-BDF domain grouping driven by a union-find pass over the IVRS alias entries the kernel-core parser already decoded; and a real `SupervisedSpawn` + `CapHandle::inject_foreign_dma` harness backing the four isolation-test scaffolds Phase 55c left as `todo!()` placeholders, plus a fifth `driver_restart_resets_domain` test covering the cross-restart domain-recycle path.

## What This Doc Covers

- The new `kernel-core/src/iommu/amd.rs` module (`AmdViFaultEvent`, `AmdViFaultCode`, `decode_event_log_entry`, `encode_event_log_entry`) and how the AMD-Vi ISR consumes it.
- `kernel/src/iommu/amd.rs::install_fault_handler` registering the AMD-Vi event-log IRQ trampoline against the device-IRQ bank, the per-unit slot array the trampoline iterates, and the bounded `drain_event_log` loop (`EVENT_LOG_RING_DEPTH = 256`, `EVENT_LOG_OVERFLOW` / `EVENT_LOG_DRAINS` counters).
- VT-d queued-invalidation: `VtdUnit::bring_up_invalidation_queue` allocates the 4 KiB ring, programs `IQA` with `QS=0`, and toggles `GCMD.QIE`. The new `flush_domain` / `flush_iotlb` / `flush_context` methods submit `cc_inv` / `iotlb_inv` descriptors followed by an `iwait` and poll the status word; `IommuError::FlushTimeout` surfaces hardware hangs. The trait-level `flush` and the `map` / `unmap` / `bind_device` callers prefer the queued path with a register-path fallback on any QI-side failure.
- VT-d scalable-mode bring-up: `VtdUnit::supports_scalable_mode` reads the live `ECAP.SMTS` bit (the hardcoded `false` is gone), `init_page_tables` returns 5 or 4 levels based on `supports_scalable_mode() && cr4_la57_enabled()`, `VtdDomainState::levels` records the per-domain depth, and `walk_and_install_intermediates` / `walk_read_only` / `free_subtree` thread the level count through every page-table walk. `xtask/src/main.rs::IOMMU_SCALABLE_QEMU_ARGS` is the sibling constant that flips `x-scalable-mode=modern`.
- AMD-Vi multi-BDF domain grouping: `kernel-core/src/iommu/tables.rs::group_bdfs_by_alias` walks every IVHD block, expands `AliasStartRange`/`EndRange` pairs into per-BDF aliases, and runs union-find over the resulting pair set. The companion `BdfDomainAssignment` tracks group → DomainId so `AmdViUnit::claim_device(bdf)` returns the cached id on subsequent calls. `build_bdf_groups_from_ivrs` is the boot-time entry point invoked from `kernel/src/iommu/mod.rs::build_and_bring_up_amdvi`.
- The new isolation-test harness: `SupervisedSpawn::{start, stop, pid, run_and_stop}` and `CapHandle::{inject_foreign_dma, forged}` in `userspace/drivers/nvme/tests/isolation.rs`. The four previously-`todo!()`ed scaffolds are now real test bodies; the new `driver_restart_resets_domain` covers cross-restart domain recycling. `grep -n 'todo!' userspace/drivers/nvme/tests/isolation.rs` returns zero lines.

## Key Files

| File | Role |
|---|---|
| `kernel-core/src/iommu/amd.rs` | New module. `AmdViFaultEvent`, `AmdViFaultCode` (seven spec-defined codes plus `Unknown(u8)`), `decode_event_log_entry` / `encode_event_log_entry`, and nine `#[cfg(test)]` cases covering every defined code plus the unknown-code preservation path. |
| `kernel/src/iommu/amd.rs` | `install_fault_handler` registers the device-IRQ trampoline + records the unit in `UNIT_SLOTS`, programs MSI, and emits the structured `iommu.amd.fault_isr=installed unit={u} vector={v}` log event. `drain_event_log` reads `EVTLOG_HEAD`/`EVTLOG_TAIL`, decodes each record through the kernel-core decoder, dispatches through `iommu::fault::log_fault_event`, and bumps `EVENT_LOG_DRAINS` / `EVENT_LOG_OVERFLOW` counters. `AmdViUnit::group_bdf_domains` and `claim_device` use the kernel-core `BdfDomainAssignment` to share a domain id across grouped BDFs. |
| `kernel-core/src/iommu/tables.rs` | `BdfGroups`, `GroupId`, `group_bdfs_by_alias`, `BdfDomainAssignment`. Ten new tests cover single-alias, start-range, transitive merge, independent groups, Select singletons, empty tables, the assignment-cache contract, and unknown-BDF fallback. |
| `kernel/src/iommu/intel.rs` | `VtdUnit::supports_scalable_mode` (the hardcoded `false` is removed), `init_page_tables`, `bring_up_invalidation_queue`, `submit_qi_descriptor`, `submit_iwait_and_poll`, `flush_domain` / `flush_iotlb` / `flush_context`, `flush_iotlb_or_global` / `flush_context_or_global` (callsite helpers), `cr4_la57_enabled`. The `VtdDomainState` struct now carries a `levels: u8` field stamped at create-time. |
| `kernel-core/src/iommu/vtd_regs.rs` | `QiDescriptor` (with `cc_inv`, `iotlb_inv`, `iwait` constructors), `encode_iqa`, `GCMD_QIE_BIT`, `GSTS_QIES_BIT`. Five new `qi_tests` cases. |
| `kernel-core/src/iommu/vtd_page_table.rs` | `LEVEL_SHIFTS_5` and `level_index_n` for parameterised 4-or-5-level walks. Two new tests verify the parameterised helper matches the legacy 4-level path and extracts the right indices for 5-level inputs. |
| `kernel-core/src/iommu/contract.rs` | `IommuError::FlushTimeout` variant (Track C.2 surfaces queue-poll hangs). Display impl test includes the new variant. |
| `userspace/drivers/nvme/tests/isolation.rs` | Replaces four `todo!()` bodies with real test bodies driving `SupervisedSpawn` + `CapHandle::inject_foreign_dma`; adds `driver_restart_resets_domain`; plus three host-side sanity tests for the harness lifecycle. |
| `xtask/src/main.rs` | `IOMMU_SCALABLE_QEMU_ARGS` constant for the scalable-mode QEMU variant. |

## Closure of Related Phases

- [Phase 55a — IOMMU Substrate](./roadmap/55a-iommu-substrate.md) carried a `## Known Open Bug — must close before Phase 58` section that was already inline-marked "Closed by Phase 55c R2". Phase 67 formalises the closure: the section is retitled `## Bug Closure Record`, and a `> **Phase 67 completion note:**` blockquote enumerates the items closed by this phase.
- [Phase 55c — Ring-3 Driver Correctness Closure](./roadmap/tasks/55c-ring-3-driver-correctness-closure-tasks.md) Track K (isolation tests, deferred → Phase 67) has every acceptance bullet flipped to checked and cross-linked back to this phase's Track F.

## Related Roadmap Docs

- [Phase 67 design doc](./roadmap/67-iommu-substrate-completion.md)
- [Phase 67 task list](./roadmap/tasks/67-iommu-substrate-completion-tasks.md)
