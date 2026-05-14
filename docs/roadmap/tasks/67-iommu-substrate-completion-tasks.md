# Phase 67 — IOMMU Substrate Completion: Task List

**Status:** Complete
**Source Ref:** phase-67
**Depends on:** Phase 55a (IOMMU Substrate) ✅, Phase 55b (Ring-3 Driver Host) ✅, Phase 55c (Ring-3 Driver Correctness Closure) ✅
**Goal:** Close the deferred IOMMU items: install AMD-Vi fault ISR and decoder; replace VT-d register-based invalidation with queued invalidation; bring up VT-d scalable mode; implement AMD-Vi multi-BDF domain grouping; replace the four `todo!()` isolation-test scaffolds with real supervised-spawn tests; update Phase 55a + 55c design docs.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | AMD-Vi fault ISR installation and event log drain | None | ✅ Done |
| B | AMD-Vi fault decoder `AmdViFaultEvent` in `kernel-core` | A | ✅ Done |
| C | VT-d queued invalidation engine: descriptor ring, flush methods | None | ✅ Done |
| D | VT-d scalable mode: runtime cap check, 5-level page tables | C | ✅ Done |
| E | AMD-Vi multi-BDF domain grouping via IVRS alias entries | A | ✅ Done |
| F | Isolation tests: `SupervisedSpawn` + `CapHandle::inject_foreign_dma` harness | None | ✅ Done |
| G | Phase 55a + 55c design docs and task docs updated | A, B, C, D, E, F | ✅ Done |
| H | Documentation and Release | G | ✅ Done |

---

## Track A — AMD-Vi Fault ISR

### A.1 — Install AMD-Vi fault interrupt handler

**File:** `kernel/src/iommu/amd.rs`
**Symbol:** `AmdVi::install_fault_handler`
**Why it matters:** Without an ISR the kernel never knows when a device attempts a DMA access outside its domain.

**Acceptance:**
- [ ] `install_fault_handler` registers a handler for the AMD-Vi event log completion interrupt.
- [ ] Handler entry point calls `drain_event_log` and returns without blocking or allocating.
- [ ] A boot-time log event confirms handler registration at `iommu.amd.fault_isr = installed`.

### A.2 — Implement `drain_event_log`

**File:** `kernel/src/iommu/amd.rs`
**Symbol:** `AmdVi::drain_event_log`
**Why it matters:** The ISR must drain all pending event records before returning to avoid event log overflow.

**Acceptance:**
- [ ] `drain_event_log` reads the EVTLOG_HEAD and EVTLOG_TAIL registers, iterates entries, and advances HEAD after processing.
- [ ] Loop is bounded to `EVENT_LOG_RING_DEPTH`; entries beyond the bound increment a named overflow counter.
- [ ] `cargo xtask test --test iommu_amd_fault` observes a structured log event after injecting a malformed DMA request.

---

## Track B — AMD-Vi Fault Decoder

### B.1 — Define `AmdViFaultEvent` and `AmdViFaultCode` in `kernel-core`

**File:** `kernel-core/src/iommu/amd.rs`
**Symbol:** `AmdViFaultEvent`, `AmdViFaultCode`, `decode_event_log_entry`
**Why it matters:** A typed decoder is host-testable and ensures the fault log output is structured and matchable by tools.

**Acceptance:**
- [ ] `AmdViFaultEvent` has fields: `requestor_bdf: u16`, `fault_code: AmdViFaultCode`, `iova: u64`, `flags: u8`.
- [ ] `AmdViFaultCode` has variants for the AMD-Vi spec Table 57 defined codes (at minimum: `IllegalDevTableEntry`, `IoPageFault`, `DevTableHwError`, `PageTableHwError`, `IllegalCommandError`, `CommandHwError`, `EventLogOverflow`).
- [ ] `decode_event_log_entry(raw: &[u8; 16]) -> Result<AmdViFaultEvent, DecodeError>` covers all defined codes.
- [ ] At least seven unit tests, one per `AmdViFaultCode` variant, verifying field extraction.

---

## Track C — VT-d Queued Invalidation Engine

### C.1 — Allocate invalidation queue ring at bring-up

**File:** `kernel/src/iommu/intel.rs`
**Symbol:** `VtdUnit::bring_up`
**Why it matters:** The queue ring must exist before any domain or IOTLB flush can use it.

**Acceptance:**
- [ ] `bring_up` allocates a 4 KiB invalidation queue ring page via the frame allocator.
- [ ] Writes the physical address to the IQA register with `QS=0` (256 descriptors).
- [ ] Asserts IQA write succeeded by reading back and comparing.

### C.2 — Add `flush_domain`, `flush_iotlb`, `flush_context` over queued invalidation

**File:** `kernel/src/iommu/intel.rs`
**Symbol:** `VtdUnit::flush_domain`, `VtdUnit::flush_iotlb`, `VtdUnit::flush_context`
**Why it matters:** Per-domain queued invalidation is required for scalable mode and gives the kernel a finer-grained flush API than the current global-only path.

**Acceptance:**
- [ ] Each new flush method writes the appropriate invalidation descriptor to the ring and advances IQT.
- [ ] Each method polls IQH until it equals IQT (synchronous wait); a bounded timeout produces `IommuError::FlushTimeout`.
- [ ] The pre-existing global invalidation calls in `bring_up` (`invalidate_context_cache_global`, `invalidate_iotlb_global`) stay — they fire before the queue is up. No callers currently use a per-domain register-based path, so nothing register-based is removed by this task.
- [ ] At least one callsite in `kernel/src/iommu/` invokes each new flush method (e.g., on domain mutation) so the path is not dead code.
- [ ] `cargo xtask test --test iommu_vtd_qi` passes.

---

## Track D — VT-d Scalable Mode

### D.1 — Add runtime scalable-mode capability check

**File:** `kernel/src/iommu/intel.rs`
**Symbol:** `VtdUnit::supports_scalable_mode`
**Why it matters:** The hardcoded `false` at `intel.rs:178` prevents scalable mode from activating even when hardware supports it.

**Acceptance:**
- [ ] `supports_scalable_mode()` reads the VT-d capability register `ECAP.SMTS` bit and returns the runtime value.
- [ ] The `false` constant at `intel.rs:178` is removed; all callers use `supports_scalable_mode()`.

### D.2 — Construct 5-level page tables when scalable mode is active

**File:** `kernel/src/iommu/intel.rs`
**Symbol:** `VtdUnit::init_page_tables`
**Why it matters:** 5-level tables are required for scalable mode; using 4-level tables on a scalable-mode unit produces a #GP on translation.

**Acceptance:**
- [ ] `init_page_tables` checks `supports_scalable_mode() && cr4_la57_enabled()` and sets the page-table level count accordingly (5 or 4).
- [ ] `cargo xtask test --test iommu_vtd_scalable` passes on a QEMU target with `-device intel-iommu,x-scalable-mode=modern,aw-bits=48` (matches the project's existing `IOMMU_QEMU_ARGS` pattern in `xtask/src/main.rs:63`, with `x-scalable-mode=off` flipped to `modern`).
- [ ] `xtask/src/main.rs` `IOMMU_QEMU_ARGS` gains a sibling constant (or a flag) for the scalable-mode variant so the test harness can opt in without forking the default IOMMU args.

---

## Track E — AMD-Vi Multi-BDF Domain Grouping

### E.1 — Add a BDF-grouping helper over existing IVHD alias entries

**File:** `kernel-core/src/iommu/tables.rs` (extend existing module)
**Symbol:** `group_bdfs_by_alias` (or similar), consuming the existing `IvhdDeviceEntry::AliasSelect` and `IvhdDeviceEntry::AliasStartRange` variants
**Why it matters:** The IVRS parser already decodes alias entries and has round-trip tests (`ivrs_decode_ivhd_40h_with_alias` at `tables.rs:1343`). The missing piece is a typed helper that walks an `IvrsTables` and groups BDFs into equivalence classes (union-find over alias pairs) for downstream consumption by `AmdVi::group_bdf_domains`.

**Acceptance:**
- [ ] No new parser is introduced; the helper consumes the existing `IvhdDeviceEntry` variants.
- [ ] The helper returns BDF equivalence classes (e.g., `Vec<Vec<u16>>` or a `BTreeMap<u16, GroupId>`) so a caller can look up a BDF's group in O(log n).
- [ ] `AliasStartRange` records expand into the implied per-device aliases over the (start, end) range before unioning.
- [ ] At least three unit tests: single `AliasSelect`, single `AliasStartRange`, and a graph that needs union-find (two alias pairs that transitively merge).

### E.2 — Group BDFs into shared domains at bring-up

**File:** `kernel/src/iommu/amd.rs`
**Symbol:** `AmdVi::group_bdf_domains`
**Why it matters:** The per-BDF domain map must respect IVRS grouping to avoid split-domain DMA coherency bugs.

**Acceptance:**
- [ ] `group_bdf_domains` builds a union-find over alias pairs so BDFs that are grouped share a single `DomainId`.
- [ ] When `claim_device(bdf)` is called for a grouped BDF, the existing domain for the group is returned rather than a new one allocated.
- [ ] At least one integration test verifies two grouped BDFs share a domain ID.

---

## Track F — Isolation Tests

### F.1 — Implement `SupervisedSpawn` test harness

**File:** `userspace/drivers/nvme/tests/isolation.rs`
**Symbol:** `SupervisedSpawn`
**Why it matters:** The four `todo!()` bodies need a shared spawn + teardown harness to avoid duplicating setup code.

**Acceptance:**
- [ ] `SupervisedSpawn::start(binary)` forks the named binary under the test supervisor and returns a handle.
- [ ] `SupervisedSpawn::stop()` sends SIGTERM and waits for exit.
- [ ] At least one test uses the harness end-to-end.

### F.2 — Implement `CapHandle::inject_foreign_dma`, fill the four scaffolds, and add a fifth restart test

**File:** `userspace/drivers/nvme/tests/isolation.rs`
**Symbol:** `CapHandle::inject_foreign_dma`, `cross_device_mmio_denied_end_to_end`, `cross_device_dma_denied_end_to_end`, `capability_forge_denied_end_to_end`, `post_crash_handles_invalid_end_to_end`, new `driver_restart_resets_domain`
**Why it matters:** Four existing `todo!()` scaffolds (at lines 85 / 112 / 139 / 171) were Phase 55c acceptance items that were never implemented. The fifth test covers the cross-restart domain-recycle path that none of the four scaffolds scopes.

**Acceptance:**
- [ ] `inject_foreign_dma` creates a `DmaBuffer` in one supervised driver's domain and passes its bus address to a second driver instance.
- [ ] `cross_device_mmio_denied_end_to_end`: an NVMe driver instance attempting an MMIO operation on a foreign device's BAR is rejected at the IOMMU check point with a typed negative errno.
- [ ] `cross_device_dma_denied_end_to_end`: cross-device DMA I/O returns `-EFAULT`; no kernel panic.
- [ ] `capability_forge_denied_end_to_end`: an attempt to call a device-host syscall with a forged `CapHandle` returns `-EBADF`.
- [ ] `post_crash_handles_invalid_end_to_end`: a `CapHandle` held by a supervisor after the issuing driver crashes is rejected on subsequent use with `-EBADF`.
- [ ] New `driver_restart_resets_domain`: after a supervised driver restart the old `DomainId` is destroyed, a new domain is created at re-claim, and a `CapHandle` minted in the pre-restart domain fails to translate post-restart.
- [ ] `grep -n 'todo!' userspace/drivers/nvme/tests/isolation.rs` returns zero lines.

---

## Track G — Phase 55a + 55c Documentation

### G.1 — Update Phase 55a design doc

**File:** `docs/roadmap/55a-iommu-substrate.md`
**Symbol:** (document section `## Known Open Bug`)
**Why it matters:** The section already carries an inline `Status (2026-05-08): Closed by Phase 55c R2` note; this task formalizes the closure header and appends the Phase 67 completion record.

**Acceptance:**
- [ ] The `## Known Open Bug — must close before Phase 58` heading is retitled `## Bug Closure Record` (the existing 55c R2 closure note stays as the record body — do not remove it).
- [ ] A `> **Phase 67 completion note:**` blockquote is appended listing the items closed by this phase (AMD-Vi fault ISR + decoder, VT-d queued invalidation, VT-d scalable mode, AMD-Vi multi-BDF grouping, isolation tests).

### G.2 — Annotate the new 55c Isolation Tests track as implemented in Phase 67

**File:** `docs/roadmap/tasks/55c-ring-3-driver-correctness-closure-tasks.md`
**Symbol:** Track K (added by this phase — see Documentation Notes below)
**Why it matters:** The 55c task doc currently has no track that records the four `todo!()` scaffolds as deferred acceptance items. Without that record, Phase 67's closure has nothing concrete to point back at. G.2 first back-fills the missing track in 55c, then marks it as closed.

**Acceptance:**
- [ ] A new `## Track K — Isolation Tests (deferred → Phase 67)` section exists in `55c-ring-3-driver-correctness-closure-tasks.md` listing the four scaffold tests and `CapHandle::inject_foreign_dma`.
- [ ] The Track Layout table at the top of 55c-tasks gains a row for Track K with Status `✅ Closed by Phase 67`.
- [ ] Each acceptance bullet in Track K is annotated `(implemented in Phase 67)` and cross-links to `docs/roadmap/tasks/67-iommu-substrate-completion-tasks.md` Track F.

---

---

## Track H — Documentation and Release

### H.1 — Create the aligned legacy learning doc

**File:** `docs/67-iommu-substrate-completion.md`
**Symbol:** (new document)
**Why it matters:** Learners need a self-contained reference for the IOMMU completion items — AMD-Vi fault dispatch, VT-d queued invalidation, scalable mode, multi-BDF grouping — without conflating them with Phase 55a's initial bring-up or future ARM SMMU work.

**Acceptance:**
- [ ] `docs/67-iommu-substrate-completion.md` exists with all template fields populated (`**Aligned Roadmap Phase:** Phase 67`, `**Status:** Planned`, `**Source Ref:** phase-67`, `**Supersedes Legacy Doc:** new`).
- [ ] Overview is one learner-friendly paragraph explaining what Phase 55a left incomplete and what this phase closes.
- [ ] Key Files table cites `kernel/src/iommu/amd.rs`, `kernel/src/iommu/intel.rs`, `kernel-core/src/iommu/amd.rs` (new — fault decoder), `kernel-core/src/iommu/tables.rs` (extended — BDF-grouping helper), and `userspace/drivers/nvme/tests/isolation.rs`.
- [ ] Related Roadmap Docs links `docs/roadmap/67-iommu-substrate-completion.md` and `docs/roadmap/tasks/67-iommu-substrate-completion-tasks.md`.

### H.2 — Bump kernel version to 0.67.0

**Files:** `kernel/Cargo.toml`, `Cargo.lock`, `AGENTS.md`, `docs/roadmap/README.md`
**Symbol:** `version` in `kernel/Cargo.toml` `[package]`
**Why it matters:** Project convention is one minor-bump per shipped phase; keeping the version cursor accurate ensures `AGENTS.md` and the README reflect the real state of the kernel at any given phase.

**Acceptance:**
- [ ] `kernel/Cargo.toml` `version = "0.67.0"`
- [ ] `Cargo.lock` regenerated (run `cargo check` or `cargo xtask check` to trigger)
- [ ] `AGENTS.md` "Kernel v0.X.0" reference updated to `v0.67.0`
- [ ] `cargo xtask check` passes after the bump
- [ ] Git tag `v0.67.0` recommended at phase merge

---

## Documentation Notes

- `kernel-core/src/iommu/amd.rs` for the fault decoder is a new file alongside the existing IVRS parser in `kernel-core/src/iommu/tables.rs`; it must be `no_std` compatible.
- Track E extends the existing IVHD parser in `kernel-core/src/iommu/tables.rs` rather than creating a parallel `ivrs.rs` module. The existing `IvhdDeviceEntry` enum (with `AliasSelect` and `AliasStartRange` variants) and its round-trip tests stay authoritative; this phase adds a grouping helper that consumes those types.
- The queued invalidation ring in Track C must be pinned (physically contiguous, non-pageable); use the existing `DmaBuffer<u8>` allocation path. The pre-existing global invalidation calls in `VtdUnit::bring_up` stay in place — they fire before the queue is up.
- VT-d scalable-mode 5-level page tables change the IOMMU page-table depth only — the kernel's own paging level (`cr4.la57`) is not changed by this phase.
- D.2's QEMU flag set (`x-scalable-mode=modern,aw-bits=48`) mirrors the project's existing `IOMMU_QEMU_ARGS` constant in `xtask/src/main.rs:63`, with only `x-scalable-mode` flipped. The current `eim` / `device-iotlb` flags are unrelated to scalable-mode bring-up and are not required here.
- Track F preserves the existing scaffold function names (`cross_device_mmio_denied_end_to_end`, `cross_device_dma_denied_end_to_end`, `capability_forge_denied_end_to_end`, `post_crash_handles_invalid_end_to_end`) and adds one new test (`driver_restart_resets_domain`). The acceptance bullets match what each scaffold scopes; do not rename the existing functions.
