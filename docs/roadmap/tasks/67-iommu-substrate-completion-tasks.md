# Phase 67 — IOMMU Substrate Completion: Task List

**Status:** Planned
**Source Ref:** phase-67
**Depends on:** Phase 55a (IOMMU Substrate) ✅, Phase 55b (Ring-3 Driver Host) ✅, Phase 55c (Ring-3 Driver Correctness Closure) ✅
**Goal:** Close the deferred IOMMU items: install AMD-Vi fault ISR and decoder; replace VT-d register-based invalidation with queued invalidation; bring up VT-d scalable mode; implement AMD-Vi multi-BDF domain grouping; replace the four `todo!()` isolation-test scaffolds with real supervised-spawn tests; update Phase 55a + 55c design docs.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | AMD-Vi fault ISR installation and event log drain | None | Planned |
| B | AMD-Vi fault decoder `AmdViFaultEvent` in `kernel-core` | A | Planned |
| C | VT-d queued invalidation engine: descriptor ring, flush methods | None | Planned |
| D | VT-d scalable mode: runtime cap check, 5-level page tables | C | Planned |
| E | AMD-Vi multi-BDF domain grouping via IVRS alias entries | A | Planned |
| F | Isolation tests: `SupervisedSpawn` + `CapHandle::inject_foreign_dma` harness | None | Planned |
| G | Phase 55a + 55c design docs and task docs updated | A, B, C, D, E, F | Planned |
| H | Documentation and Release | G | Planned |

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

### C.2 — Implement descriptor submission for `flush_domain`, `flush_iotlb`, `flush_context`

**File:** `kernel/src/iommu/intel.rs`
**Symbol:** `VtdUnit::flush_domain`, `VtdUnit::flush_iotlb`, `VtdUnit::flush_context`
**Why it matters:** Replacing the register-based path removes the incompatibility with scalable mode.

**Acceptance:**
- [ ] Each flush method writes the appropriate invalidation descriptor to the ring and advances IQT.
- [ ] Each method polls IQH until it equals IQT (synchronous wait).
- [ ] Register-based path at `intel.rs:722` is removed; no call site still uses it.
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
- [ ] `cargo xtask test --test iommu_vtd_scalable` passes on a QEMU target with `-device intel-iommu,eim=on,device-iotlb=on`.

---

## Track E — AMD-Vi Multi-BDF Domain Grouping

### E.1 — Parse IVRS alias and device-all entries

**File:** `kernel-core/src/iommu/ivrs.rs`
**Symbol:** `parse_ivhd_entries`, `IvhdEntry`
**Why it matters:** Without alias parsing, devices that share a DMA domain get independent domains, breaking IOMMU correctness for multi-function devices.

**Acceptance:**
- [ ] `IvhdEntry` has a `DevAlias { source_bdf, target_bdf }` variant.
- [ ] `parse_ivhd_entries` returns a `Vec<IvhdEntry>` including alias entries.
- [ ] At least two unit tests cover alias record parsing.

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

### F.2 — Implement `CapHandle::inject_foreign_dma` and replace all four `todo!()` bodies

**File:** `userspace/drivers/nvme/tests/isolation.rs`
**Symbol:** `CapHandle::inject_foreign_dma`, `test_cross_domain_dma_rejected`, `test_driver_restart_resets_domain`, `test_overmapped_bar_rejected`, `test_stale_cap_rejected`
**Why it matters:** These were the Phase 55c acceptance tests that were scaffolded but never implemented.

**Acceptance:**
- [ ] `inject_foreign_dma` creates a `DmaBuffer` in one supervised driver's domain and passes its bus address to a second driver instance.
- [ ] `test_cross_domain_dma_rejected`: cross-domain DMA I/O returns `-EFAULT`; no kernel panic.
- [ ] `test_driver_restart_resets_domain`: after driver restart, the old domain is destroyed and a new one is created.
- [ ] `test_overmapped_bar_rejected`: a BAR MMIO map beyond the registered coverage range is rejected.
- [ ] `test_stale_cap_rejected`: using a capability handle after the driver that created it has exited returns `-EBADF`.
- [ ] `grep -n 'todo!' userspace/drivers/nvme/tests/isolation.rs` returns zero lines.

---

## Track G — Phase 55a + 55c Documentation

### G.1 — Update Phase 55a design doc

**File:** `docs/roadmap/55a-iommu-substrate.md`
**Symbol:** (document section `## Known Open Bug`)
**Why it matters:** The "Known Open Bug — must close before Phase 58" section was already closed by 55c R2; that fact must be recorded and the Phase 67 completion added.

**Acceptance:**
- [ ] The `## Known Open Bug` section is retitled `## Bug Closure Record` and notes that the VT-d MMIO CTRL.RST issue was closed in Phase 55c R2.
- [ ] A `> **Phase 67 completion note:**` block is appended listing the items closed by this phase.

### G.2 — Update Phase 55c task doc to reference isolation test closure

**File:** `docs/roadmap/tasks/55c-ring-3-driver-correctness-closure-tasks.md`
**Symbol:** (isolation test track)
**Why it matters:** The four `todo!()` scaffolds were Phase 55c acceptance items; their closure reference must be recorded.

**Acceptance:**
- [ ] Isolation test track acceptance items note "(implemented in Phase 67)".

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
- [ ] Key Files table cites `kernel/src/iommu/amd.rs`, `kernel/src/iommu/intel.rs`, `kernel-core/src/iommu/amd.rs`, `kernel-core/src/iommu/ivrs.rs`, and `userspace/drivers/nvme/tests/isolation.rs`.
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

- `kernel-core/src/iommu/amd.rs` for the fault decoder is a new file alongside the existing IVRS parser; it must be `no_std` compatible.
- The queued invalidation ring in Track C must be pinned (physically contiguous, non-pageable); use the existing `DmaBuffer<u8>` allocation path.
- VT-d scalable-mode 5-level page tables change the IOMMU page-table depth only — the kernel's own paging level (`cr4.la57`) is not changed by this phase.
