# Phase 60 — Phase 33 Slab Migration Closeout: Task List

**Status:** In Progress
**Source Ref:** phase-60
**Depends on:** Phase 33 (Kernel Memory Improvements) ✅, Phase 53a (Kernel Memory Modernization) ✅, Phase 57e (Full Kernel Preemption — Deferred 2026-05-07) ✅
**Goal:** Audit every kernel `Box::new` / `Arc::new` site, document why most "candidate" object families (`FdEntry`, `Endpoint`, `Notification`, `Pipe`, `UnixSocket`, `MemoryMapping`) are stored inline in slot arrays or BTreeMap nodes and therefore not slab-migration candidates, then migrate the two genuinely heap-allocated hot kernel object families (`Task` and `XSaveArea`) onto the existing `KernelSlabCaches` infrastructure. Measure global-heap relief using `heap_stats()` + `all_slab_stats()`. Flip Phase 33 task-doc C.4 (`docs/roadmap/tasks/33-kernel-memory-tasks.md`) from `[ ] Deferred` to `[x]` with a measurement citation.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Audit `Box::new`/`Arc::new` sites; rank and document candidates and non-candidates | — | Done |
| B | Migrate `Task` and `XSaveArea` to slab caches | A | In Progress |
| C | Measure global-heap relief under 60-second IPC workload | B | Planned |
| D | Regression suite — full QEMU + host tests after each migration | B | Planned |
| E | Phase 33 design doc + task doc updated to mark C.4 closed | B C D | Planned |
| F | Documentation and Release | B C D E | Planned |

---

## Track A — Audit and Family Selection

### A.1 — Walk kernel allocation sites and classify each

**Files:**
- `kernel/src/task/scheduler.rs` (Task and XSaveArea allocation; lines 1092, 1097, 1098, 3651)
- `kernel/src/process/mod.rs` (kernel-stack allocation; line 987)
- `kernel/src/ipc/endpoint.rs` (inline-stored Endpoint; lines 100, 111)
- `kernel/src/ipc/notification.rs` (fixed 64-slot ISR-safe pool; documented in module header)
- `kernel/src/pipe.rs` (inline-stored Pipe; lines 53, 61)
- `kernel/src/net/unix.rs` (inline-stored UnixSocket)
- `kernel/src/process/mod.rs` (inline-stored FdEntry in `[Option<FdEntry>; MAX_FDS]`; line 218)
- `kernel-core/src/mm.rs` (`MemoryMapping` inside `BTreeMap<u64, MemoryMapping>`; lines 13, 27)
- `kernel/src/smp/mod.rs` (once-per-CPU PerCoreData/TSS/GDT; lines 636, 728, 738, 750)

**Symbol:** every `Box::new(` and `Arc::new(` call site under `kernel/src/`
**Why it matters:** The original Phase 60 plan named five "hottest object families" without verifying that each was actually heap-allocated. Track A replaces that assumption with a written audit. The audit doc is the durable record that prevents future phases from repeating the same mistake.

**Acceptance:**
- [x] `grep -rn 'Box::new\|Arc::new' kernel/src/` output captured and each site classified by: type allocated, frequency tier (per-syscall / per-process / per-CPU / once-at-boot), fixed-size-ness, and current allocator path.
- [x] `docs/handoffs/60a-allocation-audit.md` created with a ranked table of all sites.
- [x] The audit explicitly lists `FdEntry`, `Endpoint`, `Notification`, `Pipe`, `UnixSocket` as inline-slot-array non-candidates with the file:line of each slot-array storage site, and `MemoryMapping` as a BTreeMap-node non-candidate.
- [x] The audit explicitly lists `Task` and `XSaveArea` as the two confirmed migration candidates, with `kernel/src/task/scheduler.rs:1092, 1097, 1098, 3651` cited.
- [x] The audit notes that kernel stacks (`kernel/src/process/mod.rs:987`, `KERNEL_STACK_SIZE = 32 KiB`) are buddy-sized rather than slab-sized and explicitly defer them to a post-1.0 stack pool.

---

## Track B — Per-Family Slab Migration

### B.1 — Migrate `Task` to `task_cache`

**File:** `kernel/src/task/scheduler.rs` (allocation sites at lines 1097 and 3651), `kernel/src/mm/slab.rs` (existing `task_cache` member of `KernelSlabCaches`)
**Symbol:** `sched.tasks.push(Box::new(task))` → routed through `caches().task_cache.lock().allocate(&mut slab_page_alloc)`
**Why it matters:** `task_cache` has been declared at 512 bytes since Phase 33 C.3 but has never carried real load — only one in-kernel `#[test_case]` (`fd_cache`-based, at `kernel/src/main.rs:1330-1370`) exercises a named cache. B.1 wires `task_cache` into the actual `Task` allocation path.

**Acceptance:**
- [ ] `core::mem::size_of::<Task>()` measured and recorded as a `const_assert!` (or equivalent compile-time check) at the allocation site.
- [ ] If `size_of::<Task>() > 512`, `task_cache` slot size in `kernel/src/mm/slab.rs:333` raised to the next power of two that fits, with a comment citing the assertion.
- [ ] `slab_page_alloc` at `kernel/src/mm/slab.rs:292` made `pub(crate)` so the scheduler can pass it as the page-allocator callback to `.allocate(...)`.
- [ ] A `SlabBox<T>` newtype is introduced (in a new module, e.g. `kernel/src/mm/slab_box.rs`) holding `(NonNull<T>, &'static IrqSafeMutex<SlabCache>)`. Its `Drop` impl calls `core::ptr::drop_in_place(self.ptr.as_ptr())` then `cache.lock().free(self.ptr.as_ptr() as usize)`. It exposes `Deref<Target = T>` and `DerefMut`. (`Box::from_raw` cannot be reused because the global allocator's `dealloc` does not know about the slab cache.)
- [ ] Both `Box::new(task)` sites at `kernel/src/task/scheduler.rs:1097` and `:3651` replaced with `SlabBox::<Task>::new_in(&caches().task_cache, task)` (or equivalent). The scheduler's `tasks: Vec<Box<Task>>` field is changed to `tasks: Vec<SlabBox<Task>>`.
- [ ] All scheduler code paths that read `&self.tasks[i]` or `&mut self.tasks[i]` continue to compile unchanged because `SlabBox<T>` exposes `Deref`/`DerefMut` to `T`.
- [ ] `cargo xtask test` passes with no regression.
- [ ] `cargo test -p kernel-core` passes with at least one new test exercising slab alloc/free/reuse for a `size_of::<Task>()`-sized object (the test uses a raw byte buffer, not a real `Task`, because `Task` carries kernel-only globals).

### B.2 — Migrate `XSaveArea` to a new `xsave_cache`

**File:** `kernel/src/task/scheduler.rs` (allocation sites at lines 1092 and 1098), `kernel/src/mm/slab.rs` (extend `KernelSlabCaches` with a new `xsave_cache` member)
**Symbol:** `sched.fpu_states.push(Box::new(XSaveArea::new()))` → routed through `caches().xsave_cache.lock().allocate(&mut slab_page_alloc)`
**Why it matters:** `XSaveArea` is allocated 1:1 with `Task` (once per task spawn). It is the second-largest hot heap-allocated kernel object after `Task`. Without a dedicated cache it stays on the global heap even after B.1 lands.

**Acceptance:**
- [ ] `xsave_cache: IrqSafeMutex<SlabCache>` added to `KernelSlabCaches` in `kernel/src/mm/slab.rs` (struct definition currently at lines 274-286).
- [ ] `xsave_cache` initialised in `pub fn init()` (currently at `kernel/src/mm/slab.rs:319-340`) sized to `crate::arch::x86_64::cpuid::XSAVE_AREA_SIZE` (832 bytes — verify the const at `kernel/src/arch/x86_64/cpuid.rs:42`).
- [ ] Both `Box::new(XSaveArea::new())` sites at `kernel/src/task/scheduler.rs:1092` and `:1098` replaced with `SlabBox::<XSaveArea>::new_in(&caches().xsave_cache, XSaveArea::new())`. The scheduler's `fpu_states: Vec<Box<XSaveArea>>` field is changed to `fpu_states: Vec<SlabBox<XSaveArea>>`.
- [ ] `all_slab_stats()` (`kernel/src/mm/slab.rs:849`) extended to include `xsave_cache` stats; the smoke-test path in `kernel/src/main.rs` extended to exercise it.
- [ ] `cargo xtask test` passes with no regression.
- [ ] `cargo test -p kernel-core` passes with at least one new test exercising slab alloc/free/reuse for an `XSAVE_AREA_SIZE`-sized object.

---

## Track C — Heap Relief Measurement

### C.1 — Capture before/after global heap state under IPC workload

**File:** `docs/handoffs/60c-slab-heap-measurement.md` (new)
**Symbol:** `kernel::mm::heap::heap_stats()` + `kernel::mm::slab::all_slab_stats()` output captured from kernel serial dump
**Why it matters:** Without a recorded measurement, the migration is a code change with no observable outcome record. The infrastructure for capture already exists (`heap_stats()` at `kernel/src/mm/heap.rs:941`, `all_slab_stats()` at `kernel/src/mm/slab.rs:849`); C.1 only needs to invoke them under the workload and record the output.

**Acceptance:**
- [ ] "Before" measurement captured prior to landing B.1: `cargo xtask run`, fork 50 tasks, run a 60-second IPC workload, dump `heap_stats()` and `all_slab_stats()` to serial. Output recorded verbatim in the handoff doc.
- [ ] "After" measurement captured after B.1 and B.2 land, same workload. Output recorded verbatim in the same doc.
- [ ] `task_cache` and `xsave_cache` hit rates reported from the "after" `all_slab_stats()` output.
- [ ] `docs/handoffs/60c-slab-heap-measurement.md` contains both raw dumps and a comparison narrative noting where global-heap usage decreased and where the new slab caches absorbed the load.
- [ ] Global heap allocated-bytes count measurably lower in the "after" measurement; both new slab caches show non-zero hit counts.

---

## Track D — Regression Suite

### D.1 — Full regression pass after each migration

**Files:**
- `xtask/src/main.rs` (test harness; existing)
- `kernel-core/src/slab.rs` (host-side slab tests; existing — Phase 60 adds two new test cases)

**Symbol:** `cargo xtask test`, `cargo test -p kernel-core`, `cargo xtask check`
**Why it matters:** Slab migration changes the allocator path for two of the most lifecycle-critical kernel objects. A miscounted Drop or size mismatch causes a use-after-free or heap corruption.

**Acceptance:**
- [ ] `cargo xtask test` passes with zero regressions after B.1.
- [ ] `cargo xtask test` passes with zero regressions after B.2.
- [ ] `cargo test -p kernel-core` passes with the two new per-family slab unit tests added in B.1 and B.2.
- [ ] `cargo xtask check` (clippy `-D warnings` + rustfmt) passes after each migration.
- [ ] Any new `unsafe` block introduced by the slab-allocation helper has an adjacent `// SAFETY:` comment explaining the invariant.

---

## Track E — Phase 33 Doc Closure

### E.1 — Flip Phase 33 task-doc C.4 and update design doc audit note

**Files:**
- `docs/roadmap/tasks/33-kernel-memory-tasks.md` (note: actual filename is `33-kernel-memory-tasks.md`, not `33-kernel-memory-improvements-tasks.md`)
- `docs/roadmap/33-kernel-memory-improvements.md`

**Symbol:** C.4 checkbox; "Shipped state (audited in Phase 53a)" audit note in the design doc
**Why it matters:** Phase 33 C.4 is the audit's Red Flag #4 / Blocker C2. C.4's acceptance bar is "At least two frequently allocated kernel object types use slab-backed allocation paths" — Phase 60 delivers exactly two (`Task` and `XSaveArea`), satisfying the bar honestly.

**Acceptance:**
- [ ] `docs/roadmap/tasks/33-kernel-memory-tasks.md` C.4 changed from `[ ] Deferred` to `[x] Migrated in Phase 60 — see docs/handoffs/60c-slab-heap-measurement.md` for `Task` and `XSaveArea`.
- [ ] `docs/roadmap/tasks/33-kernel-memory-tasks.md` track C status row in the Track Layout table updated from "Done (C.4 migration deferred)" to "Done".
- [ ] `docs/roadmap/tasks/33-kernel-memory-tasks.md` "Deferred Follow-ups" header line updated to remove `C.4 broad slab-backed object migration`.
- [ ] `docs/roadmap/33-kernel-memory-improvements.md` "Shipped state" audit note (currently lines 68-69) updated to replace "the main hot kernel object families were not broadly migrated" with a factual record citing Phase 60's audit conclusion (most candidate families use inline slot arrays; `Task` + `XSaveArea` migrated).
- [ ] `docs/roadmap/33-kernel-memory-improvements.md` `Status:` field remains `Complete` (no demotion needed — the infrastructure was complete; the two-family migration bar is now satisfied).
- [ ] Phase 60 design doc and task doc cross-reference back to Phase 33 C.4.

---

## Track F — Documentation and Release

### F.1 — Create the aligned legacy learning doc

**File:** `docs/60-slab-migration-closeout.md`
**Symbol:** new file
**Why it matters:** The doc-template "aligned legacy learning doc" form gives a learner-friendly companion to the design + task docs. Every shipped phase has one (or has a deliberate exception). This file is created from the template in `docs/appendix/doc-templates.md` § "Template: aligned legacy learning doc".

**Acceptance:**
- [ ] `docs/60-slab-migration-closeout.md` exists, follows the template (Aligned Roadmap Phase, Status, Source Ref, Supersedes Legacy Doc / new — all present).
- [ ] Overview paragraph is learner-friendly and explains the phase outcome in plain language, including the audit finding that most "candidate" families turned out to be inline-slot-array non-candidates.
- [ ] "What This Doc Covers" lists 3+ concrete topics (the audit, `Task`/`XSaveArea` migrations, measurement).
- [ ] "Core Implementation" is written for a learner who has not read the design or task doc.
- [ ] "Key Files" table cites the actual files this phase touches (`kernel/src/mm/slab.rs`, `kernel/src/task/scheduler.rs`, the audit + measurement handoff docs).
- [ ] "How This Phase Differs From Later Memory Work" explains why most kernel object families are inline-stored and why that is a different (equally valid) form of allocator avoidance.
- [ ] "Related Roadmap Docs" links the design and task docs.

### F.2 — Bump kernel version to 0.60.0

**Files:**
- `kernel/Cargo.toml`
- `Cargo.lock`
- `AGENTS.md`
- `docs/roadmap/README.md` (any version annotations)

**Symbol:** `version` field in `kernel/Cargo.toml` `[package]` section
**Why it matters:** Phase closure is signalled by a kernel version bump per project convention. Each new phase moves the project from `0.<previous>.x` to `0.<NN>.0`. The current `kernel/Cargo.toml` version is `0.58.0` (Phases 58 and 59 were documentation-only); Phase 60 is the first code-touching phase since the bump and moves the kernel to `0.60.0`.

**Acceptance:**
- [ ] `kernel/Cargo.toml` `version = "0.60.0"`.
- [ ] `Cargo.lock` regenerated (`cargo generate-lockfile` or `cargo build` updates it).
- [ ] `AGENTS.md` "Kernel v0.58.0" reference updated to "Kernel v0.60.0".
- [ ] `cargo xtask check` passes after the bump.
- [ ] Git tag suggestion: `v0.60.0` (tag at phase merge, not at task-checkbox tick).

---

## Documentation Notes

- Track B order is dependency-light: B.1 (`Task`) and B.2 (`XSaveArea`) can land in either order, but Track C measurement requires both to land before the "after" capture is meaningful. The implementation outline keeps B.1 first because `task_cache` already exists and only needs wiring; B.2 requires extending `KernelSlabCaches` with a new member.
- The slab API in `kernel/src/mm/slab.rs` exposes `caches().NAME_cache.lock().allocate(page_alloc_callback)` returning `Option<usize>` (raw pointer) and `.free(addr: usize)`. The page-allocator callback is `fn slab_page_alloc()` at `kernel/src/mm/slab.rs:292` (made `pub(crate)` by B.1). There is no `slab_alloc!` / `slab_free!` macro — the `SlabBox<T>` helper for `Box`-style usage is constructed in B.1 and reused by B.2.
- When writing the measurement doc (C.1), use the exact serial-output lines from `heap_stats()` and `all_slab_stats()` — not paraphrases. Copy the raw dump and annotate it.
- Host-side slab tests in `kernel-core` use `#[cfg(test)]` and the Cargo `dev-dependencies` `std` access pattern. The tests exercise raw byte buffers sized to `core::mem::size_of::<Task>()` and `XSAVE_AREA_SIZE` rather than constructing real `Task` / `XSaveArea` values, because both types carry kernel-only globals.
- The audit deliverable (Track A) is the most important durable artefact of this phase — it documents the architectural finding that most "candidate" kernel object families are stored inline in slot arrays and therefore not slab-migration candidates. Future phases that propose slab migration of any of these families should be challenged against this audit.
