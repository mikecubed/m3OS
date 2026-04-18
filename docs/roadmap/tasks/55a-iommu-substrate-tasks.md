# Phase 55a — IOMMU Substrate: Task List

**Status:** Planned
**Source Ref:** phase-55a
**Depends on:** Phase 15 (Hardware Discovery) ✅, Phase 53a (Kernel Memory Modernization) ✅, Phase 55 (Hardware Substrate) ✅
**Goal:** Parse ACPI DMAR / IVRS tables, construct per-device DMA translation domains behind a common `IommuUnit` trait, and route every `DmaBuffer<T>` allocation through an IOMMU-mapped IOVA so the kernel — and the ring-3 driver host that Phase 55b will add on top — can protect itself from device-initiated memory corruption. Close the IOMMU caveat recorded in the Phase 55 Reference Hardware Matrix and bump the kernel to v0.55.1.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Pure-logic foundation in `kernel-core` (DMAR / IVRS structure types, page-table bit layouts, IOVA allocator, `IommuUnit` trait, reserved-region algebra) | None | Planned |
| B | ACPI DMAR / IVRS parsing wired into the Phase 15 parser; device-to-unit map; reserved-memory region extraction | A | Planned |
| C | VT-d (Intel) implementation behind `IommuUnit` (MMIO register layer, root/context/page tables, translation enable, invalidation queue, fault handler) | A, B | Planned |
| D | AMD-Vi implementation behind `IommuUnit` (device table, command and event buffers, page table walker, fault handler) | A, B | Planned |
| E | `DmaBuffer<T>` rewrite to route through per-device domains; `PciDeviceHandle` domain lifetime wiring; identity-map fallback; reserved-region pre-mapping; migration of Phase 55 drivers (NVMe, e1000, VirtIO-blk, VirtIO-net) | A, C, D | Planned |
| F | Validation: `cargo xtask run --iommu` flag, IOMMU boot + driver smoke integration test, malformed-descriptor fault-injection test, shared contract suite proving VT-d and AMD-Vi are LSP-compliant | C, D, E | Planned |
| G | Documentation: Phase 55a learning doc, Phase 15 and Phase 55 subsystem doc updates, reference-matrix IOMMU caveat closure, roadmap README row, version bump to v0.55.1 | F | Planned |

---

## Engineering Discipline and Test Pyramid

These are preconditions for every code-producing task in this phase. A task cannot be marked complete if it violates any of them. Where a later task re-states a rule for emphasis, the rule here is authoritative.

### Test-first ordering (TDD)

- Tests for a code-producing task commit **before** the implementation that makes them pass. Git history for the touched files must show failing-test commits preceding green-test commits. "Tests follow" is not acceptable.
- Acceptance lists that say "at least N tests cover …" name *minimums*. If the implementation reveals a new case, add the test before closing the task.
- A task is not complete until every test it names can be executed via `cargo test -p kernel-core --target x86_64-unknown-linux-gnu` (unit, contract, property) or `cargo xtask test` (integration).

### Test pyramid

| Layer | Location | Runs via | Covers |
|---|---|---|---|
| Unit | `kernel-core/src/iommu/` | `cargo test -p kernel-core --target x86_64-unknown-linux-gnu` | Pure logic: DMAR / IVRS table decode, VT-d / AMD-Vi page-table entry encoding, IOVA allocator invariants, reserved-region union / intersection, device-to-unit lookup |
| Contract | `kernel-core` shared harness (`kernel-core/tests/iommu_contract.rs`) | Same | Both `IommuUnit` impls (VT-d, AMD-Vi) pass the same behavioral suite — trait LSP-compliance is proven, not assumed |
| Property | `kernel-core` with `proptest` (available from Phase 43c) | Same | DMAR / IVRS decoder robustness on arbitrary byte streams (no panic, no infinite loop, bounded allocation); IOVA allocator invariants across arbitrary allocate / free sequences; page-table encode / decode round-trip |
| Integration | `kernel/tests/` and `xtask test` harness | `cargo xtask test` (QEMU with `-device intel-iommu`) | End-to-end: ACPI DMAR parsed, unit bring-up logged, per-device domain installed, `DmaBuffer::bus_address` returns IOVA, NVMe + e1000 data-path smoke passes, malformed PRP triggers IOMMU fault |

Pure logic belongs in `kernel-core`. Hardware-dependent wiring belongs in `kernel/src/iommu/`. Tasks that straddle the boundary split their code along it so the pure part is host-testable; no task may defer this split to "later".

### SOLID and module boundaries

- **Single Responsibility.** Modules under `kernel/src/iommu/` each own one concern: `mod.rs` → `IommuUnit` trait, dispatch, domain lifetime management; `intel.rs` → VT-d only; `amd.rs` → AMD-Vi only; `fault.rs` → structured fault logging shared by both vendors. No vendor module reaches into another vendor's internals.
- **Open / Closed and Dependency Inversion.** The extension seam is the `IommuUnit` trait (A.1). `DmaBuffer<T>` (E.2), `claim_pci_device` (E.1), and every driver consume domains through the trait, not the concrete impls. A third vendor (e.g. ARM SMMU in a later phase) lands by implementing the trait, not by editing callers.
- **Interface Segregation.** `IommuUnit` exposes a narrow surface: `bring_up`, `create_domain`, `destroy_domain`, `map`, `unmap`, `flush`, `install_fault_handler`, `capabilities`. Device-table internals, page-table walkers, invalidation-queue descriptor layout, and fault-log ring buffers are not part of the trait; they live behind `pub(crate)` in each vendor module.
- **Liskov Substitution.** VT-d and AMD-Vi impls pass the shared contract test suite (F.4). Behavior that a vendor cannot provide (e.g. 1 GiB page support on older AMD-Vi) is represented as capability data returned from the trait, not as a silent behavioral divergence.

### DRY

- DMAR / IVRS structure types, VT-d / AMD-Vi page-table bit layouts, and the IOVA allocator live **once** in `kernel-core::iommu::{tables, page_table, iova}` (A.0, A.2). No ACPI structure is declared twice; searching for a type name must return exactly one definition across the workspace.
- Reserved-region union and intersection logic lives once in `kernel-core::iommu::regions` (E.4). Both VT-d's RMRR handling and AMD-Vi's unity-map handling consume the same helpers.
- Structured fault-event formatting lives once in `kernel/src/iommu/fault.rs`. Both vendor fault handlers call into it so log format is identical across vendors.
- The `-device intel-iommu` QEMU flag is recorded once in `xtask` (F.1) and referenced by every integration test that needs it.

### Error discipline

- Non-test code contains no `.unwrap()`, `.expect()`, `panic!()`, `todo!()`, or `unreachable!()` outside of documented fail-fast initialization points (e.g. IOMMU-required-but-absent in a configuration that declared IOMMU required). Every such site carries an inline comment naming the audited reason it is safe.
- Every module boundary returns typed `Result<T, NamedError>` with a named error enum per subsystem: `IommuError`, `DomainError`, `DmarParseError`, `IvrsParseError`, `IovaError`. Error variants are data, not stringly-typed; callers can match and recover.
- Identity-map fallback is a named, logged decision — not a silent fallback. When ACPI reports no DMAR / IVRS, boot emits exactly one structured `iommu.fallback.identity` log event naming the reason before any driver claims a device.

### Observability

- Boot-time logging records: each DMAR / IVRS header found, each IOMMU unit brought up (vendor, register base, capability snapshot), and each per-device domain created (BDF, unit index, domain id). Logs are structured, keyed by the `iommu` subsystem.
- Every IOMMU fault (VT-d fault record, AMD-Vi event-log entry) is decoded and logged with requester ID, fault reason, and the IOVA that faulted. Fault handlers must not allocate and must not block.
- Identity-map fallback, when active, is logged once at boot and surfaces as a boolean field (`iommu.active = false`) in the meminfo / diagnostic output so it is visible without rerunning boot.

### Concurrency and IRQ safety

- Fault handlers run in IRQ context: no allocation, no lock that a non-IRQ path could hold, bounded work per invocation (drain fault-record or event-log ring, then return). The structured fault log is written via a lock-free or spin-based ring owned by the IOMMU subsystem.
- Domain creation, destruction, and IOVA map / unmap are serialized per domain with a documented lock ordering that never nests IOMMU-unit lock under driver-side locks. The lock ordering is captured in `kernel/src/iommu/mod.rs` module-level documentation and in the learning doc (G.1).

### Resource bounds

- Each domain's IOVA space carries a named high-water mark. An allocation that would cross the mark returns `IovaError::Exhausted`; it never panics and never triggers unbounded page-table growth.
- Page-table page allocations for a domain are capped. Exceeding the cap is logged, the offending allocation fails with `DomainError::PageTablePagesCapExceeded`, and the caller's DMA allocation returns a typed error without corrupting the domain.
- Fault-record and event-log rings are fixed-size. Overflow increments a documented counter and drops the oldest record; the overflow is logged once per fault-storm window.

---

## Track A — Pure-Logic Foundation in `kernel-core`

### A.0 — DMAR / IVRS structure types and decoders in `kernel-core`

**Files:**
- `kernel-core/src/iommu/mod.rs` (new)
- `kernel-core/src/iommu/tables.rs` (new)

**Symbol:** `DmarHeader`, `DmaRemappingUnit` (DRHD), `ReservedMemoryRegion` (RMRR), `AtsrEntry`, `RhsaEntry`, `IvrsHeader`, `IvhdBlock` (types 10h, 11h, 40h), `DeviceScope`, `decode_dmar`, `decode_ivrs`, `DmarParseError`, `IvrsParseError`
**Why it matters:** DMAR (Intel) and IVRS (AMD) are ACPI tables whose binary layouts are spec-defined and vendor-neutral. Declaring their types once, in `kernel-core`, lets us host-test decoding against synthesized blobs and keeps `kernel/src/iommu/intel.rs` and `kernel/src/iommu/amd.rs` free of ACPI-parsing duplication. Without this, each vendor module grows a parallel decoder that drifts from the spec in different ways.

**Acceptance:**
- [ ] Tests commit first (failing) and pass after implementation lands — evidence is in `git log --follow kernel-core/src/iommu/tables.rs`
- [ ] `decode_dmar` consumes `&[u8]` and returns a typed `Result<Vec<DmaRemappingUnit>, DmarParseError>`; it decodes DRHD, RMRR, ATSR, and RHSA sub-tables; unknown sub-table types are skipped with a counted warning, never panic
- [ ] `decode_ivrs` decodes IVHD blocks of type 10h, 11h, and 40h, with device entries including `Select`, `Start Range`, `End Range`, `Alias Select`, and `Alias Start Range`
- [ ] Per-variant unit round-trip tests exist for every decoded record (DRHD with and without scope entries, RMRR, ATSR, RHSA, IVHD 10h, 11h, 40h)
- [ ] A `proptest`-based corruption test feeds arbitrary `&[u8]` into each decoder and asserts no panic, no infinite loop, and no allocation proportional to input length beyond the bounded output vector
- [ ] `DmarParseError` and `IvrsParseError` are named enums with variants for `TruncatedHeader`, `InvalidChecksum`, `UnknownRevision`, `TruncatedSubTable`, `InvalidDeviceScope`
- [ ] No new external crate dependencies beyond what Phase 43c already enables for `proptest` in test builds

### A.1 — `IommuUnit` trait and `DmaDomain` contract

**Files:**
- `kernel-core/src/iommu/contract.rs` (new)

**Symbol:** `IommuUnit` (trait), `DmaDomain` (struct), `DomainId`, `Iova`, `PhysAddr`, `MapFlags`, `IommuError`, `DomainError`
**Why it matters:** The trait is the single extension seam every driver will consume. A wrong shape here (wrong method boundary, missing capability query, leaky internal state) cascades into every driver migration in Track E and into Phase 55b's `sys_device_dma_alloc`. Declaring it in `kernel-core` makes it host-testable via the contract suite (F.4) without pulling in a hardware unit.

**Acceptance:**
- [ ] Tests commit first — a smoke `IommuUnit` impl named `MockUnit` lives in `kernel-core/tests/fixtures/` and compiles against the trait before the trait is merged
- [ ] Trait methods: `bring_up(&mut self) -> Result<(), IommuError>`, `create_domain(&mut self) -> Result<DmaDomain, IommuError>`, `destroy_domain(&mut self, domain: DmaDomain) -> Result<(), IommuError>`, `map(&mut self, domain: DomainId, iova: Iova, phys: PhysAddr, len: usize, flags: MapFlags) -> Result<(), DomainError>`, `unmap(&mut self, domain: DomainId, iova: Iova, len: usize) -> Result<(), DomainError>`, `flush(&mut self, domain: DomainId) -> Result<(), IommuError>`, `install_fault_handler(&mut self, handler: FaultHandlerFn) -> Result<(), IommuError>`, `capabilities(&self) -> IommuCapabilities`
- [ ] `IommuCapabilities` names each vendor-visible fact (supported page sizes, address-width bits, interrupt-remapping availability) as data, not as method-presence divergence; traits are LSP-compliant
- [ ] `DmaDomain` carries a `DomainId` and a reference to its owning unit; Drop is defined and returns cleanly via `destroy_domain`
- [ ] `IommuError` and `DomainError` are named enums; variants include `NotAvailable`, `HardwareFault`, `IovaExhausted`, `AlreadyMapped`, `NotMapped`, `InvalidRange`, `PageTablePagesCapExceeded`
- [ ] Public visibility is the trait, the domain, the error enums, and the capability struct. Internals (page-table structures, TLB invalidation sequences) are not part of the trait and live under `pub(crate)` in each vendor module
- [ ] Module-level documentation on `kernel/src/iommu/mod.rs` records the lock ordering between domain lock, unit lock, and driver-side locks (referenced from G.1 and from `AGENTS.md`)

### A.2 — IOVA allocator pure-logic

**Files:**
- `kernel-core/src/iommu/iova.rs` (new)

**Symbol:** `IovaSpace`, `IovaRange`, `IovaAllocator::new(start, end, min_alignment)`, `IovaAllocator::allocate(len, alignment) -> Result<IovaRange, IovaError>`, `IovaAllocator::free(range)`, `IovaError`
**Why it matters:** Every IOMMU-mapped allocation pulls a range from this allocator. It must be stress-testable on the host without hardware, and it must enforce invariants (no overlap, freed ranges become reusable, exhaustion fails cleanly) so that drivers that release allocations aggressively do not fragment the domain into uselessness.

**Acceptance:**
- [ ] Tests commit first and pass after implementation
- [ ] Allocator provides a bump path with a freelist for returned ranges; allocation honors caller-supplied alignment (minimum 4 KiB, higher alignments supported for large-page hints)
- [ ] Property test: across arbitrary sequences of `allocate` and `free` calls, allocated ranges never overlap, and any freed range is re-allocable in a subsequent call (up to fragmentation bounds documented in module docs)
- [ ] Property test: `allocate(len, align)` with `align > min_alignment` returns a range whose start address is `align`-aligned, or `IovaError::Exhausted` / `IovaError::AlignmentUnsatisfiable`
- [ ] Exhaustion test: allocating past the space cap returns `IovaError::Exhausted` without panicking and without memory growth
- [ ] `IovaError` variants: `Exhausted`, `AlignmentUnsatisfiable`, `ZeroLength`, `DoubleFree`
- [ ] No external dependencies beyond the existing `kernel-core` set

### A.3 — Reserved-region algebra

**Files:**
- `kernel-core/src/iommu/regions.rs` (new)

**Symbol:** `ReservedRegion`, `ReservedRegionSet`, `ReservedRegionSet::union`, `ReservedRegionSet::contains`, `ReservedRegionSet::merge_overlapping`
**Why it matters:** DMAR RMRR entries (firmware GPU framebuffer, ACPI reclaim, EFI runtime) and IVRS unity-map ranges describe memory that must be identity-mapped in every new domain or the affected device hangs. Both vendors need the same set-algebra helpers; declaring them once keeps VT-d and AMD-Vi domain-creation code free of parallel bugs.

**Acceptance:**
- [ ] Tests commit first
- [ ] `ReservedRegion` carries `(start: PhysAddr, len: usize, flags: RegionFlags)`
- [ ] `ReservedRegionSet::union` merges overlapping regions into contiguous spans; flags are combined with OR
- [ ] Property test: the set is invariant under repeated `union` of the same region (idempotent) and under permutation of insert order
- [ ] `contains` is O(log N) via sorted representation
- [ ] Used by both the VT-d domain-creation path (C.2) and the AMD-Vi domain-creation path (D.2) via the shared pre-map helper in E.4

---

## Track B — ACPI DMAR / IVRS Parsing

### B.1 — DMAR / IVRS ACPI integration

**Files:**
- `kernel/src/acpi/mod.rs`
- `kernel/src/iommu/mod.rs` (new; shell created in A.1, extended here)

**Symbol:** `Acpi::parse_dmar`, `Acpi::parse_ivrs`, `iommu_units_from_acpi`, `DeviceToUnitMap`
**Why it matters:** Wires the Phase 15 ACPI parser (RSDP → RSDT / XSDT → MADT, MCFG) into the new IOMMU subsystem. Without this, the `kernel-core` decoders from Track A have no callers and IOMMU init cannot start.

**Acceptance:**
- [ ] Tests commit first: the Phase 15 ACPI test harness is extended with a synthesized DMAR blob (and an IVRS blob) and proves both are discovered and passed to the `kernel-core` decoders
- [ ] `Acpi::parse_dmar` locates the `DMAR` signature in the RSDT / XSDT, validates the checksum, and returns `Result<Vec<DmaRemappingUnit>, DmarParseError>` by calling `kernel_core::iommu::tables::decode_dmar`
- [ ] `Acpi::parse_ivrs` does the same for `IVRS`
- [ ] `iommu_units_from_acpi` produces a list of unit descriptors (vendor, register base, capability hints, device-scope list) suitable for Track C / D to consume
- [ ] `DeviceToUnitMap` answers "which IOMMU unit owns this BDF?" in O(log N) via sorted scope-range search; absent devices return `None` cleanly
- [ ] If both DMAR and IVRS are present (should not happen on a well-formed platform), a `warn!` is emitted and the first one found is used; the second is logged but ignored (no panic)
- [ ] If neither is present, `iommu_units_from_acpi` returns an empty list; boot continues and E.3's identity-map fallback engages

### B.2 — Reserved-memory extraction into `ReservedRegionSet`

**Files:**
- `kernel/src/iommu/mod.rs`

**Symbol:** `reserved_regions_from_units`
**Why it matters:** RMRR (VT-d) and unity-map (AMD-Vi) regions must be installed in every domain at creation. Extracting them once, into the shared `ReservedRegionSet`, is the DRY hook that lets both vendors use the same pre-map path (E.4).

**Acceptance:**
- [ ] Tests commit first — a synthesized DMAR with two overlapping RMRR entries proves the union result matches expectations
- [ ] Returns a single `ReservedRegionSet` covering every unit's reserved ranges
- [ ] Ranges outside the declared device scope are still included (defensive: firmware-declared reserved regions are honored even if the scope is narrow, matching Linux behavior)
- [ ] Logs a structured summary line at boot naming each reserved region (start, length, source table, source index)

---

## Track C — VT-d (Intel) Implementation

### C.1 — VT-d register MMIO layer

**Files:**
- `kernel/src/iommu/intel.rs` (new)

**Symbol:** `VtdUnit::new`, `VtdUnit::map_registers`, `VtdRegisters`
**Why it matters:** Phase 55 `BarMapping` already provides safe MMIO with volatile access and lifetime-bound unmap. Wiring VT-d through it keeps the abstraction consistent and lets the unit be dropped cleanly.

**Acceptance:**
- [ ] Tests commit first — host-testable register-offset definitions in `kernel-core::iommu::vtd_regs` with a round-trip test proving offsets match the VT-d spec table references cited in module docs
- [ ] `VtdUnit::new(register_base: PhysAddr)` uses `BarMapping` to map a 4 KiB register window; Drop reverses the mapping
- [ ] `VtdUnit::version()` reads the `VER` register and returns a typed `VtdVersion`
- [ ] `VtdUnit::capabilities()` reads `CAP` + `ECAP` and constructs an `IommuCapabilities` answer: supported page sizes, adjusted-guest-address-width bits, queued-invalidation support
- [ ] Does not enable translation; does not assume scalable-mode is on. Scalable mode is explicitly deferred and documented as such in module docs

### C.2 — VT-d root / context / page tables

**Files:**
- `kernel/src/iommu/intel.rs`
- `kernel-core/src/iommu/page_table.rs` (new; shared bit-layout types)

**Symbol:** `VtdUnit::create_domain`, `RootTable`, `ContextTable`, `VtdPageTable`, `VtdPageTableEntry`
**Why it matters:** Per-device domains need a root-table entry pointing to a context table, which points to a second-level page table. A bug in this wiring means the wrong device's DMA resolves through the wrong page table — the exact category of bug IOMMU is meant to prevent.

**Acceptance:**
- [ ] Tests commit first — host-testable `VtdPageTableEntry::encode` / `decode` round-trip in `kernel-core::iommu::page_table`, with property test proving `decode(encode(x)) == x` for arbitrary valid entries
- [ ] `create_domain` allocates a new second-level page table via the Phase 53a buddy allocator, zeros it, installs a context-table entry pointing to it (present, AW = 48 bit, translation-type = 00b second-level), invalidates the context cache, and returns a `DmaDomain`
- [ ] The domain's `IovaAllocator` is initialized for the full 48-bit address space minus the reserved-region set (E.4)
- [ ] Host-side unit test: a walker (in `kernel-core::iommu::page_table`) resolves an IOVA to a physical frame through a constructed page table and proves the mapping is correct for 4 KiB, 2 MiB, and 1 GiB page sizes when supported
- [ ] Destroying a domain invalidates the context entry, walks and frees every page-table page back to the buddy allocator, and flushes the context cache; no page-table page leaks on repeated create + destroy
- [ ] Page-table page allocations are counted against the per-domain cap; exceeding the cap returns `DomainError::PageTablePagesCapExceeded`

### C.3 — VT-d translation enable and queued invalidation

**Files:**
- `kernel/src/iommu/intel.rs`

**Symbol:** `VtdUnit::enable_translation`, `VtdUnit::iotlb_invalidate`, `VtdUnit::context_invalidate`, `InvalidationQueue`
**Why it matters:** Translation must only enable *after* the root-table pointer is committed or devices see garbage on first DMA. TLB flushes must run after every unmap, or stale translations leak and outlive the allocation — producing use-after-free bugs that span hardware and software.

**Acceptance:**
- [ ] Tests commit first where practical — the contract test suite (F.4) covers ordering guarantees
- [ ] `enable_translation` writes the root-table base register, waits for the hardware to acknowledge, then sets the translation-enable bit in `GCMD` / `GSTS`
- [ ] Queued-invalidation tail register is advanced after submitting each invalidation descriptor; wait-for-completion uses the status-write descriptor, not a timeout
- [ ] Register invalidation mode is selectable (register-based for hardware lacking the invalidation queue, queued otherwise); capability-driven at bring-up
- [ ] `unmap` always issues an IOTLB invalidation before returning success; the contract test in F.4 proves a stale translation is not observable after unmap + new map at the same IOVA
- [ ] Interrupt remapping is left disabled (explicitly deferred; recorded in module docs)

### C.4 — VT-d fault handler

**Files:**
- `kernel/src/iommu/intel.rs`
- `kernel/src/iommu/fault.rs` (new; shared with AMD-Vi)

**Symbol:** `VtdUnit::install_fault_handler`, `vtd_fault_irq_handler`, `FaultRecord`, `log_fault_event`
**Why it matters:** Without a fault handler that actually logs, the "malformed PRP triggers IOMMU fault" acceptance test cannot distinguish a fault from a silent success. Faults must be logged with requester ID + IOVA + fault reason so developers can diagnose driver bugs instead of chasing corrupted kernel memory.

**Acceptance:**
- [ ] Tests commit first — the fault-record decoder lives in `kernel-core` and has host-side round-trip tests
- [ ] Fault interrupt is allocated via MSI (Phase 55 `allocate_msi_vectors`); vector written to the VT-d fault-event registers
- [ ] Handler drains the fault-record ring, logs each record via `log_fault_event` (structured: subsystem = iommu, vendor = vtd, requester_bdf, iova, fault_reason, timestamp), and clears the fault-overflow bit
- [ ] Handler is IRQ-safe: no allocation, no lock that any non-IRQ path could hold for longer than bounded work, bounded maximum records processed per invocation (remainder re-entered on next fault)
- [ ] Integration test F.3 observes the logged fault event for a deliberately-malformed NVMe PRP

---

## Track D — AMD-Vi Implementation

### D.1 — AMD-Vi device table and command / event buffers

**Files:**
- `kernel/src/iommu/amd.rs` (new)

**Symbol:** `AmdViUnit::new`, `DeviceTable`, `CommandBuffer`, `EventBuffer`
**Why it matters:** AMD-Vi replaces VT-d's root + context lookup with a per-BDF device table. Shape is different, intent is the same. Getting the buffer sizes, alignment, and base-register write order wrong is silent — the unit simply does not translate, and drivers scribble over kernel memory.

**Acceptance:**
- [ ] Tests commit first — host-testable device-table-entry layout in `kernel-core::iommu::amdvi_regs` with encode / decode round-trip
- [ ] `DeviceTable` is sized for the full 16-bit BDF space (64 KiB × entry size); allocated via the Phase 53a buddy allocator aligned to 4 KiB
- [ ] Command and event buffers are each a power-of-two size within the spec-allowed range, allocated 4 KiB-aligned, and their base registers written in the documented order (base first, control bits second)
- [ ] `AmdViUnit::new` does *not* enable translation; enable is a separate step (D.3)
- [ ] `AmdViUnit::capabilities` reports page sizes, address-width, and any feature bits the spec exposes as data on the `IommuCapabilities` struct

### D.2 — AMD-Vi page-table walker and `create_domain`

**Files:**
- `kernel/src/iommu/amd.rs`

**Symbol:** `AmdViUnit::create_domain`, `AmdViPageTable`, `AmdViPageTableEntry`
**Why it matters:** AMD-Vi's host-page-table format is distinct from VT-d's second-level format but shares the 4 KiB / 2 MiB / 1 GiB page hierarchy. Declaring the entry bit layout in `kernel-core` alongside VT-d keeps the two walkers from drifting.

**Acceptance:**
- [ ] Tests commit first — `AmdViPageTableEntry::encode` / `decode` round-trip in `kernel-core`, property-tested
- [ ] `create_domain` allocates a page table, installs a device-table entry for every BDF in the domain's scope (initially one BDF per domain; multi-BDF domains deferred), and returns a `DmaDomain`
- [ ] Reserved regions from `ReservedRegionSet` are pre-mapped via the shared helper (E.4) — the same code path VT-d uses
- [ ] Host-side unit test: walker resolves IOVA → phys for 4 KiB / 2 MiB pages (1 GiB gated on capability)
- [ ] `destroy_domain` clears the device-table entry, issues an invalidation command, walks and frees the page table

### D.3 — AMD-Vi translation enable and command / event processing

**Files:**
- `kernel/src/iommu/amd.rs`
- `kernel/src/iommu/fault.rs`

**Symbol:** `AmdViUnit::enable_translation`, `AmdViUnit::invalidate_iotlb`, `AmdViUnit::process_events`, `amdvi_fault_irq_handler`
**Why it matters:** Commands are submitted via a ring; events (including faults) are reported via a ring. The command ring must be advanced with correct barriers or commands are lost; the event ring must be drained or the unit stalls.

**Acceptance:**
- [ ] Tests commit first where practical (contract suite F.4 covers ordering)
- [ ] `enable_translation` writes the device-table base register, writes the command and event buffer base registers, then sets the enable bits in the documented order
- [ ] Invalidation commands (INVALIDATE_IOMMU_PAGES, INVALIDATE_DEVTAB_ENTRY) are posted via the command ring with a trailing COMPLETION_WAIT; the handler polls the completion word, not a timeout
- [ ] Event-log interrupt allocated via MSI; handler drains the event ring, decodes each event, and logs faults via the shared `log_fault_event` (same structured format as VT-d)
- [ ] Handler is IRQ-safe (no allocation, bounded work); overflow increments a counter and drops the oldest entry

---

## Track E — `DmaBuffer` Rewrite and Driver Migration

### E.1 — Per-device `DmaDomain` attached to `PciDeviceHandle`

**File:** `kernel/src/pci/mod.rs`
**Symbol:** `claim_pci_device`, `PciDeviceHandle::domain`, `PciDeviceHandle::drop`
**Why it matters:** The domain must live exactly as long as the handle. Drop order is critical: any DMA in flight through the device must complete (or be aborted) before the domain's page tables are freed, or a racing DMA resolves through freed memory. Getting this right here makes Phase 55b's `sys_device_dma_alloc` safe by construction; getting it wrong produces use-after-free classes that span hardware and software.

**Acceptance:**
- [ ] Tests commit first: a fake `PciDeviceHandle` flow proves domain creation on claim and domain teardown on drop, with no leaks across repeated claim + release cycles
- [ ] `claim_pci_device(bdf)` looks up the owning IOMMU unit via `DeviceToUnitMap` (B.1), requests a new domain from that unit, stores the domain handle on `PciDeviceHandle`, and returns the handle
- [ ] When no IOMMU is active (E.3 fallback), the domain is the `IdentityDomain` variant; all other behavior is unchanged
- [ ] Drop of `PciDeviceHandle` unmaps every live IOVA in the domain, issues the TLB / device-table invalidations, then calls `destroy_domain` on the unit
- [ ] Release ordering is documented: device quiesced (MSI disabled, BME off) *before* domain teardown; this ordering is part of the claim / release contract and is referenced from the learning doc (G.1)

### E.2 — `DmaBuffer::allocate(device, size)` routed through IOMMU

**File:** `kernel/src/mm/dma.rs`
**Symbol:** `DmaBuffer::allocate`, `DmaBuffer::bus_address`, `DmaBuffer::physical_address`, `DmaBuffer::drop`
**Why it matters:** The signature change is intentionally visible at every driver call site. It forces each caller to name the device the buffer belongs to, which is the precondition for IOMMU isolation. This is also the primitive Phase 55b's `sys_device_dma_alloc` will wrap — the device-keyed contract must be stable before ring-3 drivers land.

**Acceptance:**
- [ ] Tests commit first — a `kernel-core`-side pure-logic shim proves the size / alignment / zero-init contract against a mock domain
- [ ] Signature: `DmaBuffer::allocate(device: &PciDeviceHandle, bytes: usize) -> Result<Self, DmaError>`. The old `DmaBuffer::new()` signature is removed from the kernel tree
- [ ] Physical backing: `alloc_contiguous_frames` from Phase 53a buddy allocator; zero-initialized before the IOVA mapping is installed
- [ ] IOVA: pulled from the device's domain via `IovaAllocator::allocate`; installed in the page table via `IommuUnit::map`
- [ ] `bus_address()` returns the IOVA when IOMMU is active, the physical frame address when identity-map fallback is active. Distinction is documented; callers should *not* branch on the active IOMMU — both values are safe to hand to the device
- [ ] `physical_address()` is an explicit accessor retained for the rare cases (legacy descriptors, debug dumps) that need the frame address unconditionally; grep evidence proves every kernel call site passes through a review that cites its reason
- [ ] Drop: unmap the IOVA, issue the per-domain TLB flush, free the IOVA range, return the physical frames to the buddy allocator
- [ ] `DmaError` variants cover `OutOfFrames`, `IovaExhausted`, `DomainHardwareFault`, `InvalidSize`

### E.3 — Identity-map fallback (`IdentityDomain`)

**Files:**
- `kernel/src/iommu/mod.rs`
- `kernel/src/mm/dma.rs`

**Symbol:** `IdentityDomain`, `iommu::active()`, `iommu.fallback.identity` log event
**Why it matters:** The default `cargo xtask run` has no `-device intel-iommu`. Boot must still work and DMA must still flow; the fallback is deliberate. It must be *observable* — a logged, named decision — so no one can mistakenly ship code that assumed IOMMU was active when it was not.

**Acceptance:**
- [ ] Tests commit first — a QEMU integration test without `-device intel-iommu` asserts the boot log contains exactly one `iommu.fallback.identity` event
- [ ] When `iommu_units_from_acpi` returns an empty list, kernel installs `IdentityDomain` for every claimed handle; `bus_address()` returns the physical frame address
- [ ] `iommu::active() -> bool` returns `false`; exposed to diagnostic output (meminfo, boot banner) so the fallback is visible without re-running boot
- [ ] One structured `iommu.fallback.identity` event is logged at the end of IOMMU init, naming the reason (`no_dmar_or_ivrs`, `vtd_init_failed`, `amdvi_init_failed` as applicable); event is logged exactly once per boot, not per claim
- [ ] Existing Phase 55 NVMe / e1000 / VirtIO-blk / VirtIO-net smoke tests continue to pass unchanged in the fallback path

### E.4 — Reserved-region pre-mapping in new domains

**Files:**
- `kernel/src/iommu/mod.rs`
- `kernel-core/src/iommu/regions.rs`

**Symbol:** `DmaDomain::pre_map_reserved`, shared between Intel and AMD vendors
**Why it matters:** Firmware-declared reserved regions (GPU framebuffer held by firmware, ACPI reclaim, EFI runtime) must remain identity-accessible in every new domain or the owning device hangs. Both vendors install them the same way; declaring the helper once keeps the behavior uniform.

**Acceptance:**
- [ ] Tests commit first — host-side test proves the reserved regions land in a constructed page table at domain creation
- [ ] Called once per domain at creation time, before the domain is returned to the caller
- [ ] Idempotent: a region that overlaps an existing identity mapping is not re-mapped; collision would otherwise panic
- [ ] Used by both VT-d's `create_domain` (C.2) and AMD-Vi's `create_domain` (D.2)
- [ ] Reserved-region IOVAs are recorded in the domain's `IovaAllocator` as pre-reserved so driver allocations never land on top of them

### E.5 — Migrate Phase 55 driver callers

**Files:**
- `kernel/src/blk/nvme.rs`
- `kernel/src/net/e1000.rs`
- `kernel/src/blk/virtio_blk.rs`
- `kernel/src/net/virtio_net.rs`

**Symbol:** every site calling `DmaBuffer::new` in these drivers
**Why it matters:** The signature change must be adopted uniformly. Any remaining `DmaBuffer::new` call site is a driver that bypasses IOMMU protection — the regression Phase 55a exists to prevent.

**Acceptance:**
- [ ] Tests commit first (or simultaneously, since this is a rename + threading through of a `&PciDeviceHandle` argument): existing Phase 55 QEMU smoke tests are extended with an IOMMU-enabled variant that exercises every migrated driver
- [ ] Zero use-sites of `DmaBuffer::new` remain anywhere in the kernel tree — grep evidence recorded in the commit message
- [ ] Each driver's probe path threads its `PciDeviceHandle` reference to every `DmaBuffer::allocate` site
- [ ] Phase 55 QEMU smoke tests (NVMe read / write, e1000 ICMP) continue to pass in both IOMMU-active and identity-fallback configurations

---

## Track F — Validation

### F.1 — `cargo xtask run --iommu` configuration

**File:** `xtask/src/main.rs`
**Symbol:** `run` command, `--iommu` flag, `IOMMU_QEMU_ARGS`
**Why it matters:** Without a canned configuration, CI and developers cannot consistently exercise the IOMMU path. A single source of truth for the QEMU command-line is the DRY hook the test tasks (F.2, F.3) will consume.

**Acceptance:**
- [ ] Tests commit first — an xtask unit test proves the flag produces the expected QEMU argument vector (contains `-device intel-iommu,x-scalable-mode=off`, `-machine kernel_irqchip=split`)
- [ ] `cargo xtask run --iommu` launches QEMU with the IOMMU enabled; `cargo xtask run` without the flag is unchanged
- [ ] `--iommu` composes with `--fresh` and `--gui`
- [ ] Documented in `cargo xtask --help` output
- [ ] The configuration string is exported as a constant consumed by F.2 and F.3 so tests never hand-roll QEMU args

### F.2 — IOMMU smoke integration test

**File:** `kernel/tests/iommu_smoke.rs` (new)
**Symbol:** `iommu_smoke_nvme`, `iommu_smoke_e1000`
**Why it matters:** Proves the end-to-end path: DMAR parsed, unit brought up, domain created, `DmaBuffer::bus_address` returns an IOVA, driver data path works through IOVA-mapped DMA. Without this, acceptance criteria "NVMe and e1000 drivers operate unchanged" is a claim, not a fact.

**Acceptance:**
- [ ] Test is added to the `cargo xtask test` harness, executed with the F.1 IOMMU configuration
- [ ] `iommu_smoke_nvme` writes a sentinel to an NVMe LBA, reads it back, and compares; passes only when the boot log contains at least one `iommu.unit.brought_up` event and at least one `iommu.domain.created` event
- [ ] `iommu_smoke_e1000` sends an ICMP echo request, receives the reply, and asserts the same IOMMU-active boot log preconditions
- [ ] Test also runs in the identity-fallback configuration (default `cargo xtask run` QEMU args) and passes there, confirming the fallback path still works

### F.3 — Malformed-descriptor fault-injection test

**File:** `kernel/tests/iommu_fault.rs` (new)
**Symbol:** `malformed_prp_triggers_iommu_fault`
**Why it matters:** The design doc's acceptance criterion "a deliberately-malformed NVMe PRP entry pointing outside the driver's DMA allocation triggers an IOMMU fault rather than corrupting kernel memory" needs a test. Fault injection is the proof IOMMU protection is real, not theoretical.

**Acceptance:**
- [ ] Test submits a fabricated NVMe PRP whose IOVA points outside the driver's DMA allocation (specifically: at a sentinel page the test owns and can verify for non-modification)
- [ ] After submission, the test asserts: the boot / test log contains exactly one `iommu.fault` event naming the correct requester BDF and the out-of-range IOVA; the sentinel page is unmodified (byte-compared before / after); the kernel did not panic; the NVMe driver remains usable for subsequent well-formed commands
- [ ] Runs only with the F.1 IOMMU configuration; in identity-fallback this test is skipped with a logged `skipped: iommu inactive` reason

### F.4 — `IommuUnit` contract suite and pure-logic parity

**File:** `kernel-core/tests/iommu_contract.rs` (new)
**Symbol:** `iommu_contract_suite`, `MockUnit`
**Why it matters:** LSP compliance is a promise, not a hope. The pure-logic layers shared between vendors (IOVA allocator, reserved-region algebra, page-table encode / decode) must behave identically; the trait contract itself is pinned by a `MockUnit` reference impl. Hardware-dependent wiring (MMIO sequencing, MSI setup) is exercised end-to-end in QEMU (F.2, F.3) for each vendor separately. Together, the two prove that a driver consuming `IommuUnit` is provably correct across vendors.

**Acceptance:**
- [ ] Tests commit first
- [ ] `MockUnit` is a pure-logic `IommuUnit` impl in `kernel-core/tests/fixtures/` that encodes the trait contract; it is the authoritative reference against which the trait's documented behavior is tested
- [ ] Contract suite, parameterized over `MockUnit`: `create_domain` returns distinct `DomainId`s across repeated calls; `map` + `unmap` is idempotent at the API level; double-`unmap` returns `DomainError::NotMapped` without panic; `unmap` is observed by a subsequent `map` at the same IOVA; fault callback, when installed, receives a structured `FaultRecord` and returns; capability query returns stable values
- [ ] Pure-logic parity tests: given the same input sequence, `VtdPageTableEntry::encode` and `AmdViPageTableEntry::encode` each round-trip (`decode(encode(x)) == x`); the `IovaAllocator` (single shared module) satisfies the contract suite; the fault-record decoder for each vendor produces the expected `FaultRecord` fields on synthetic fault events
- [ ] The contract suite is listed as a prerequisite for adding any future `IommuUnit` impl (documented in `kernel/src/iommu/mod.rs` module docs and in the learning doc G.1): a new vendor passes both the trait contract (via `MockUnit`) and the pure-logic parity tests before being added to the dispatch map

---

## Track G — Documentation and Version

### G.1 — Phase 55a learning doc

**File:** `docs/55a-iommu-substrate.md` (new)
**Symbol:** N/A (documentation)
**Why it matters:** Pairs with the roadmap doc to give a learner the conceptual frame: why IOMMU exists separately from MMU, what DMAR / IVRS actually describe, how translated domains protect the kernel from hostile devices, and why identity fallback is a deliberate degradation. The aligned-legacy-learning-doc template in `docs/appendix/doc-templates.md` (lines 167-214) is the authoritative shape for this file.

**Acceptance:**
- [ ] File follows the aligned-learning-doc template with frontmatter: `Aligned Roadmap Phase: Phase 55a`, `Status: Complete` (once phase lands), `Source Ref: phase-55a`, `Supersedes Legacy Doc: (none — new content)`
- [ ] Covers, in learner-friendly prose: MMU vs IOMMU distinction (CPU-side vs device-side translation and why both are required); ACPI DMAR and IVRS (what they describe, which vendor, how the OS uses them); VT-d second-level page-table shape vs AMD-Vi host-page-table shape (at concept level, not bit-layout level); identity-mapped vs translated domains and why Phase 55a picks translated for claimed devices; reserved-region handling (why RMRR / unity-map are not optional)
- [ ] Documents the lock ordering between domain lock, unit lock, and driver-side locks (cross-referenced from A.1)
- [ ] Documents the per-domain IOVA and page-table-page resource caps, with default values named so a future task can revise them
- [ ] "Key Files" table lists `kernel/src/iommu/mod.rs`, `kernel/src/iommu/intel.rs`, `kernel/src/iommu/amd.rs`, `kernel/src/iommu/fault.rs`, `kernel-core/src/iommu/{mod, tables, page_table, iova, regions, contract}.rs`, `kernel/src/mm/dma.rs`, `kernel/src/pci/mod.rs`
- [ ] "How This Phase Differs From Later Memory Work" section notes that Phase 55b will add `sys_device_dma_alloc` on top of this substrate and Phase 56 assumes per-device isolation for multi-client display safety
- [ ] "Related Roadmap Docs" links to `docs/roadmap/55a-iommu-substrate.md` and `docs/roadmap/tasks/55a-iommu-substrate-tasks.md`
- [ ] "Deferred or Later-Phase Topics" names ARM SMMU, SR-IOV VFs, VFIO passthrough, IOMMU groups, interrupt remapping, dynamic IOVA compaction (matching the roadmap doc's Deferred list)

### G.2 — Phase 15 and Phase 55 subsystem doc updates

**Files:**
- `docs/15-hardware-discovery.md`
- `docs/55-hardware-substrate.md`

**Symbol:** N/A (documentation)
**Why it matters:** Phase 55's Reference Hardware Matrix explicitly flagged an IOMMU caveat ("VT-d / AMD-Vi enabled systems may block driver DMA until IOMMU mappings exist; IOMMU support is deferred per Phase 55 design doc"). This task closes the caveat and extends the Phase 15 doc to cover the new ACPI tables.

**Acceptance:**
- [ ] Phase 15 doc gains a "DMAR and IVRS" subsection summarizing the ACPI tables at concept level and linking to the Phase 55a learning doc and task doc for detail
- [ ] Phase 55 doc's Reference Hardware Matrix IOMMU caveat is replaced with a cross-reference to Phase 55a for validated-IOMMU configurations
- [ ] A new validated-configuration row `QEMU + -device intel-iommu` is added to the matrix, referencing F.1 / F.2 for proof
- [ ] Neither doc duplicates content from the Phase 55a learning doc — cross-references only

### G.3 — Roadmap README rows

**Files:**
- `docs/roadmap/README.md`
- `docs/roadmap/tasks/README.md`

**Symbol:** Phase 55a row
**Why it matters:** Keeps the roadmap index in sync. A missing README row means a future agent reading the index does not see Phase 55a exists.

**Acceptance:**
- [ ] `docs/roadmap/README.md` gains a Phase 55a row with all template columns: `Phase | Theme | Primary Outcome | Status | Source Ref | Milestone | Tasks`
- [ ] `docs/roadmap/tasks/README.md` gains a matching Phase 55a row
- [ ] Row placement is numerically ordered (between Phase 55 and Phase 55b / 56)
- [ ] Status column reflects the phase's true status at commit time; status is flipped to `Complete` only when every Track A–G task is closed

### G.4 — Version bump to v0.55.1

**Files:**
- `kernel/Cargo.toml`
- `AGENTS.md`
- `README.md`
- `docs/roadmap/README.md`
- `docs/roadmap/tasks/README.md`

**Symbol:** version string, project-overview summary
**Why it matters:** Phase 55a design doc acceptance criteria mandates the bump. Missing any of the five sites leaves version metadata inconsistent, which cascades into downstream confusion (Cargo.lock mismatch, release-notes drift, incorrect status in the roadmap).

**Acceptance:**
- [ ] `kernel/Cargo.toml` `version` becomes `"0.55.1"`
- [ ] `AGENTS.md` project overview says "Kernel v0.55.1" and the paragraph summarizing current capability names the IOMMU substrate as new
- [ ] `README.md` current-version string and capability summary updated consistently
- [ ] `docs/roadmap/README.md` and `docs/roadmap/tasks/README.md` status rows reflect the new version
- [ ] `Cargo.lock` regenerated in the same commit so CI does not diverge
- [ ] `cargo xtask check` passes after the bump

---

## Documentation Notes

- **Relative to Phase 55.** The signature of `DmaBuffer::new()` is retired in favor of `DmaBuffer::allocate(device: &PciDeviceHandle, size)`. `bus_address()` semantics change from "physical frame address" to "IOVA when IOMMU active, physical frame address when identity fallback active." `PciDeviceHandle`'s lifetime now owns a `DmaDomain`; drop order at device release is documented and enforced. The Phase 55 flat-physical path is not removed; it becomes the explicit `IdentityDomain` variant activated when ACPI reports no DMAR / IVRS.

- **Relative to Phase 15.** ACPI parsing gains DMAR and IVRS decoders alongside the existing RSDP / RSDT / XSDT / MADT / MCFG decode. The decoders live in `kernel-core::iommu::tables` and are host-testable; the kernel-side integration in `kernel/src/acpi/mod.rs` only wires them up.

- **Relative to Phase 53a.** The buddy allocator is reused for page-table page backing and for `DmaBuffer` physical backing. No allocator changes are required; this phase only consumes `alloc_contiguous_frames`.

- **Support for Phase 55b (Ring-3 Driver Host).** Phase 55b will add a new syscall `sys_device_dma_alloc(device_cap, size)` that wraps `DmaBuffer::allocate`. The device-keyed contract, per-device domain lifetime, and identity-fallback path delivered here are exactly the primitives 55b's capability layer will wrap. Any change to the `DmaBuffer` signature or the `PciDeviceHandle` lifetime after this phase forces re-work in 55b — treat the contracts in Track A / E as stable once Phase 55a is closed.

- **Support for Phase 56 (Display and Input Architecture).** Phase 56's multi-client display service assumes per-device isolation so that one graphical client's device-visible buffers cannot reach another client's memory via bus-master DMA. Phase 55a provides that isolation by construction through translated domains. The Phase 55 Reference Hardware Matrix gains a `-device intel-iommu` row here, which Phase 56's validation configurations can reference without adding IOMMU setup work to its own track plan.

- **Vendor split.** VT-d and AMD-Vi are behind a single trait (`IommuUnit`) with a shared contract-test suite. An ARM SMMU or a future VT-d scalable-mode impl lands by adding a third impl that passes the same suite — no trait changes, no driver changes.

- **Behavior that replaces an older implementation.** The `DmaBuffer::new()` constructor from Phase 55 is removed. The `identity-mapped flat physical` behavior is renamed and isolated as `IdentityDomain` so it can be identified and gated explicitly.

- **Lock ordering.** Documented once in `kernel/src/iommu/mod.rs` module docs and once in the learning doc (G.1). Driver-side locks never nest IOMMU-unit locks; IOMMU-unit locks never nest buddy-allocator locks. Violations are caught by kernel-side debug assertions when `cfg!(debug_assertions)` is enabled.

- **Explicit deferrals (unchanged from the roadmap doc).** Interrupt remapping, VT-d scalable mode, ARM SMMU, SR-IOV VFs, VFIO passthrough, IOMMU groups beyond per-device domains, dynamic IOVA-space compaction, and large-page promotion optimizations all live outside Phase 55a. Each is documented as deferred in the learning doc so a future agent does not re-derive "why isn't this here" from scratch.
