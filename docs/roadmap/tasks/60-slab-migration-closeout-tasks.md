# Phase 60 — Phase 33 Slab Migration Closeout: Task List

**Status:** Planned
**Source Ref:** phase-60
**Depends on:** Phase 33 (Kernel Memory Improvements) ✅, Phase 53a (Kernel Memory Modernization) ✅, Phase 57e (Full Kernel Preemption — Deferred 2026-05-07) ✅
**Goal:** Migrate the five hottest kernel object families (`Task`, `Endpoint`, `Notification`, `FdEntry`, `VmRegion`) from the global linked-list heap onto the Phase 33/53a slab caches; measure global heap relief; flip Phase 33 task-doc C.4 from deferred to complete with a measurement citation.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Audit allocation sites, rank candidate families | — | Planned |
| B | Migrate five object families to slab caches | A | Planned |
| C | Measure global heap relief under 60-second IPC workload | B | Planned |
| D | Regression suite — full QEMU + host tests after each migration | B | Planned |
| E | Phase 33 design doc + task doc updated to mark C.4 closed | B C D | Planned |

---

## Track A — Audit and Family Selection

### A.1 — Walk kernel allocation sites and rank by frequency

**Files:**
- `kernel/src/task/scheduler.rs`
- `kernel/src/ipc/endpoint.rs`
- `kernel/src/ipc/notification.rs`
- `kernel/src/fs/fd_table.rs` (or equivalent `FdEntry` site)
- `kernel/src/mm/vm_region.rs` (or equivalent)

**Symbol:** all `Box::new(...)` call sites in `kernel/src/`
**Why it matters:** Confirms that the five target families are genuinely the hottest before investing migration effort; may reveal a higher-priority family not in the original audit list.

**Acceptance:**
- [ ] `grep -r 'Box::new' kernel/src/` output reviewed and each site classified by type.
- [ ] Ranked table of candidate families produced (file: `docs/handoffs/60a-allocation-audit.md`).
- [ ] Five target families confirmed in the top tier; any higher-priority outlier noted with an explanation of why it is not targeted in this phase.

---

## Track B — Per-Family Slab Migration

### B.1 — Migrate `FdEntry` to slab cache

**File:** `kernel/src/fs/fd_table.rs` (or equivalent)
**Symbol:** `FdEntry` alloc site; new `FD_ENTRY_CACHE: SlabCache<FdEntry>` lazy static
**Why it matters:** `FdEntry` is allocated on every `open` syscall. Server processes (sshd, display_server) hold hundreds. The smallest and most self-contained migration, making it the right starting point.

**Acceptance:**
- [ ] `FD_ENTRY_CACHE` slab cache declared with `SlabCache::<FdEntry>::new(...)`.
- [ ] All `Box::new(FdEntry { ... })` replaced with `slab_alloc!(FD_ENTRY_CACHE)`.
- [ ] Drop path uses `slab_free!(FD_ENTRY_CACHE, ptr)` after child fields are dropped.
- [ ] `cargo xtask test` passes with no regression.
- [ ] At least one new `kernel-core` host-side test exercises `FdEntry` alloc/free/reuse through the slab path.

### B.2 — Migrate `Notification` to slab cache

**File:** `kernel/src/ipc/notification.rs`
**Symbol:** `Notification` alloc site; new `NOTIFICATION_CACHE: SlabCache<Notification>` lazy static
**Why it matters:** `Notification` objects are the lowest-latency IPC primitive; they are allocated at `sys_notification_create` and freed when all capability references drop. Small fixed-size struct — ideal slab candidate.

**Acceptance:**
- [ ] `NOTIFICATION_CACHE` slab cache declared.
- [ ] All `Notification` allocation sites migrated.
- [ ] Drop path uses slab free.
- [ ] `cargo xtask test` passes.
- [ ] One new host-side test for `Notification` slab alloc/free.

### B.3 — Migrate `Endpoint` to slab cache

**File:** `kernel/src/ipc/endpoint.rs`
**Symbol:** `Endpoint` alloc site; new `ENDPOINT_CACHE: SlabCache<Endpoint>` lazy static
**Why it matters:** `Endpoint` objects are allocated at service-registration time and survive for the lifetime of the service. Moderate-frequency allocation; high value because `Endpoint` is held across IPC rendezvous transitions.

**Acceptance:**
- [ ] `ENDPOINT_CACHE` slab cache declared.
- [ ] All `Endpoint` allocation sites migrated.
- [ ] Drop path uses slab free.
- [ ] `cargo xtask test` passes.
- [ ] One new host-side test for `Endpoint` slab alloc/free.

### B.4 — Migrate `VmRegion` to slab cache

**File:** `kernel/src/mm/vm_region.rs` (or equivalent)
**Symbol:** `VmRegion` alloc site; new `VM_REGION_CACHE: SlabCache<VmRegion>` lazy static
**Why it matters:** Every `mmap` call allocates a `VmRegion`. Under the Phase 54 serverized network stack, connection churn generates sustained `VmRegion` pressure on the global heap.

**Acceptance:**
- [ ] `VM_REGION_CACHE` slab cache declared.
- [ ] All `VmRegion` allocation sites migrated.
- [ ] Drop path uses slab free (after child guard pages / backing pages are released).
- [ ] `cargo xtask test` passes.
- [ ] One new host-side test for `VmRegion` slab alloc/free.

### B.5 — Migrate `Task` to slab cache

**File:** `kernel/src/task/scheduler.rs`
**Symbol:** `Task` alloc site (fork/exec path); new `TASK_CACHE: SlabCache<Task>` lazy static
**Why it matters:** `Task` is the largest and most complex migration — it holds file descriptor table handles, signal state, and scheduler fields. Correct Drop ordering (child references first, then slab free) is the primary risk.

**Acceptance:**
- [ ] `TASK_CACHE` slab cache declared with the correct `Task` size (verified with `core::mem::size_of::<Task>()` assertion).
- [ ] `fork` and `exec` paths use `slab_alloc!(TASK_CACHE)`.
- [ ] Drop path: file descriptor table, signal state, and all `Arc`-held references drop before `slab_free!(TASK_CACHE, ptr)`.
- [ ] `cargo xtask test` passes (full suite — including SMP tests).
- [ ] `cargo test -p kernel-core` passes.
- [ ] One new host-side test for `Task` slab alloc/free (using a mock `Task` without kernel globals).

---

## Track C — Heap Relief Measurement

### C.1 — Capture before/after global heap state under IPC workload

**File:** `docs/handoffs/60c-slab-heap-measurement.md` (new)
**Symbol:** kernel serial debug heap dump
**Why it matters:** Proves that the migrated families are consuming slab cache rather than global heap. Without a measurement, the migration is a code change with no observable outcome record.

**Acceptance:**
- [ ] "Before" measurement: `cargo xtask run`, start 50 tasks, run 60-second IPC workload, capture global heap free-list depth from serial dump — recorded in handoff doc.
- [ ] "After" measurement: same workload after all five B-track migrations — recorded in same doc.
- [ ] Slab cache hit rates for all five caches reported from serial dump.
- [ ] `docs/handoffs/60c-slab-heap-measurement.md` contains both measurements in a readable comparison table.
- [ ] Global heap usage measurably lower in the "after" measurement for at least three of the five families.

---

## Track D — Regression Suite

### D.1 — Full regression pass after all five migrations

**Files:**
- `xtask/src/main.rs` (test harness)
- `kernel-core/src/` (host-side slab tests)

**Symbol:** `cargo xtask test`, `cargo test -p kernel-core`
**Why it matters:** Slab migration changes the allocator path for the five most critical kernel objects. A single miscounted Drop or size mismatch causes a use-after-free or heap corruption.

**Acceptance:**
- [ ] `cargo xtask test` passes with zero regressions after all five B-track migrations.
- [ ] `cargo test -p kernel-core` passes with all five new per-family unit tests added in Track B.
- [ ] `cargo xtask check` (clippy -D warnings + rustfmt) passes.
- [ ] No new `unsafe` block introduced without an adjacent `// SAFETY:` comment.

---

## Track E — Phase 33 Doc Closure

### E.1 — Flip Phase 33 task-doc C.4 and update design doc audit note

**Files:**
- `docs/roadmap/tasks/33-kernel-memory-improvements-tasks.md`
- `docs/roadmap/33-kernel-memory-improvements.md`

**Symbol:** C.4 checkbox item; "Shipped state (audited...)" audit note in design doc
**Why it matters:** Phase 33 C.4 is the audit's Red Flag #4 and Blocker C2. Flipping it closes the longest-standing open deferral in the Phase 33–53a memory arc.

**Acceptance:**
- [ ] Phase 33 task-doc C.4 changed from `[ ] Deferred` to `[x] Migrated in Phase 60 — see docs/handoffs/60c-slab-heap-measurement.md`.
- [ ] Phase 33 design doc "Shipped state" audit note updated to replace the "not broadly migrated" sentence with a factual record of the Phase 60 migration.
- [ ] Phase 33 design doc `Status:` field remains `Complete` (no demotion needed — the infrastructure was complete; the migration gap is now closed).
- [ ] Phase 60 design doc and task doc cross-reference back to Phase 33 C.4.

---

## Documentation Notes

- Migration order in Track B is dependency-ordered: `FdEntry` → `Notification` → `Endpoint` → `VmRegion` → `Task`. Do not migrate `Task` before the others; `Task` holds references to `FdEntry` and depends on clean Drop semantics in all child types.
- When writing the measurement doc (C.1), use exact kernel serial output lines — not paraphrases. Copy the raw dump and annotate it.
- The `slab_alloc!` / `slab_free!` macro names are assumed from the Phase 33/53a infrastructure; verify the actual macro names in `kernel/src/mm/slab.rs` before use.
- Host-side slab tests in `kernel-core` must use `#[cfg(test)]` and `std` — they are not `no_std`. Use the `kernel-core` feature-flag pattern (`#[cfg(feature = "std")]`) for any test-only imports.
