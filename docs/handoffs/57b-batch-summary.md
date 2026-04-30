# Phase 57b — Batch Summary (parallel-impl)

**Status:** Complete pending soak
**PR:** [#132](https://github.com/mikecubed/m3OS/pull/132)
**Integration branch:** `feat/57b-preemption-foundation` (65 commits + 7 wave merges)
**Kernel version:** 0.57.1 → **0.57.2**
**Source ref:** phase-57b
**Date landed:** 2026-04-30

## Wave plan executed

| Wave | Tracks | Worktree concurrency | Outcome |
|------|--------|---------------------|---------|
| 1 | A (audit + counter model + PreemptFrame) ‖ B.1 (`Vec<Box<Task>>`) | 2 | Merged |
| 2 | D.1 + E.1 + E.2 + B.2 (Task struct fields + tests) | 1 | Merged |
| 3 | C.1 + C.2 + C.3 + C.5 (per-CPU pointer + dispatch retargeting) | 1 | Merged |
| 4 | D.2 + D.3 (lock-free helpers + user-mode-return assertion) | 1 | Merged |
| 5 | F.1 + F.2 + C.4 (`IrqSafeMutex` wiring + lifecycle tests) | 1 | Merged |
| 6a | G.1 (blk) ‖ G.2 (net) | 2 | Merged |
| 6b | G.3 (fs) ‖ G.4 (mm) | 2 | Merged |
| 6c | G.5 (iommu) ‖ G.6 (process/ipc/syscall) | 2 | Merged |
| 6d | G.7 (misc) ‖ G.8 (smp/arch) | 2 | Merged |
| 6e | G.9 (kernel-core, doc-only) | 1 | Merged |
| 7 | H (closeout: docs + version + soak-gate doc) | 1 | Merged |

Concurrency cap: 2 (matched `.flow/defaults.json` baked-in default since the
project does not pin one). Never exceeded; G.4 and G.5 deliberately landed in
adjacent waves rather than parallel because they both touch sensitive
allocator + IOMMU paths.

## Track outcomes

| Track | Locks / files touched | Tests added | Status |
|-------|----------------------|-------------|--------|
| A | 64 locks classified; 4 new files in `kernel-core` | proptest fuzz (10k cases), compile-time `PreemptFrame` layout assertions | Merged |
| B | `Scheduler::tasks: Vec<Box<Task>>` | `task_preempt_count_address_stable_across_vec_growth` | Merged |
| C | `PerCoreData::current_preempt_count_ptr` + `SCHED_PREEMPT_COUNT_DUMMY` + dispatch retargeting | sched-trace tracepoint; lifecycle test in Wave 5 | Merged |
| D | `Task::preempt_count` + `preempt_disable`/`preempt_enable` + assertion at every IRQ-return-to-ring-3 + sysretq + enter_userspace | lock-freedom synthetic test; max-depth 32 nesting test | Merged |
| E | `Task::preempt_frame` + offset re-exports | compile-time offset 448 pinned (build fails on layout drift) | Merged |
| F | `IrqSafeMutex::lock` raises + `IrqSafeGuard::Drop` releases (field declaration order encodes drop sequence) | drop-order test, recursion-safety test, scheduler_lock cycles-once test | Merged |
| G.1 (blk) | 3 locks (REQUEST_LOCK, REMOTE_BLOCK convert; DRIVER explicit) | virtio-blk submit + IRQ-wake `preempt_count` regression test | Merged |
| G.2 (net) | 8 locks (7 convert; virtio-net DRIVER explicit) | rely on D.3 + existing test suite | Merged |
| G.3 (fs) | 5 locks (all convert) | rely on D.3 + existing FS coreutils test path | Merged |
| G.4 (mm) | 13 locks (all convert; `AddressSpace::lock_page_tables` return type changed) | existing `heap_grows_on_oom` + `zero_exposure_*` + `frame_stats_consistent` | Merged |
| G.5 (iommu) | 5 locks (4 convert incl. 2 RwLock→IrqSafeMutex; vt-d UNIT_SLOTS explicit) | rely on D.3 + boot-path IOMMU init | Merged |
| G.6 (process/ipc/syscall) | 16 locks (all convert; Arc<IrqSafeMutex<>> for shared fd table + signal actions) | rely on D.3 + existing process test suite. Added `Debug` impl on `IrqSafeMutex<T: Debug>` for `derive(Debug)` on `ThreadGroup`. | Merged |
| G.7 (misc) | 13 locks (12 convert; RAW_INPUT_ROUTER explicit) | rely on D.3 | Merged |
| G.8 (smp/arch) | 5 locks (all explicit-preempt-and-cli; PerCoreData `with_run_queue` helper rewrites 9 scheduler.rs callsites) | rely on D.3 + IPI delivery via existing test suite | Merged |
| G.9 (kernel-core) | 0 code (doc-only — host-test-only locks inherit via kernel-side wrappers) | n/a | Merged |
| H | scheduler.rs preempt_count doc block, docs/04-tasking.md section, kernel/Cargo.toml v0.57.2, boot banner via CARGO_PKG_VERSION, soak-gate procedure | n/a | Merged |

Total locks migrated: **65** (64 from the audit + `WaitQueue::waiters` discovered during G.7).

## Validations run

- `cargo xtask check` — passed after every wave merge (clippy `-D warnings` clean, rustfmt clean, kernel-core + passwd + driver_runtime host tests pass).
- `cargo xtask test` — passed after every wave merge (all 3 QEMU test binaries: `bound_recv`, `sched_fuzz`, `kernel`).
- `cargo test -p kernel-core --target x86_64-unknown-linux-gnu` — 1376 tests pass after Wave 1.
- D.3 user-mode-return debug assertion: never tripped throughout the test suite.

## Pre-existing issues fixed in scope

The kernel test binary halts at the first failing test, so a small pre-existing
test bug from PR #118 (Phase 55c) blocked validation of B.2 end-to-end. Fixed
in scope:

- `fix(net::remote)` (`docs/roadmap/follow-ups/55c-net-remote-rx-test-bug.md`):
  - Three RX-path tests imported `encode_net_send` (stamps `NET_SEND_FRAME`)
    but expected `decode_net_rx_notify` to accept the payload — switched to
    `encode_net_rx_notify`.
  - `drain_rx_queue_removes_malformed_frames_after_deferred_queueing` fed a
    14-byte all-zero frame which is structurally valid; shrunk to 8 bytes so
    it actually fails the Ethernet header check.
  - `link_event_recovers_restart_suspected_slot_with_live_endpoint` asserted
    `RESTART_SUSPECTED` is cleared by `ensure_link_event_entry`, but the
    function's own doc comment says the latch is intentionally NOT cleared
    because of a userspace `sendto` retry race. Removed the stale assertion.

## Retained or abandoned tracks

None. All in-scope tracks (A, B, C, D, E, F, G.1–G.9, H) merged.

## Unresolved follow-ups

1. **30-minute soak gate (H.4).** Procedural; documented at
   `docs/handoffs/57b-soak-gate.md`. Run after PR merge to mark Phase 57b
   fully closed.
2. **Phase 57b row in `docs/roadmap/README.md`.** Currently
   `Complete pending soak`. Update to `Complete` after the soak passes.
3. **kernel-core G.9 long-term migration.** `MagazineDepot` locks remain
   plain `spin::Mutex` per the audit's host-test-only classification; the
   eventual migration is owned by Phase 57e (full kernel preemption) at
   the kernel-side consumer site (`kernel/src/mm/slab.rs`).

## Integration branch + PR status

- Integration feature branch `feat/57b-preemption-foundation`: committed and
  pushed to origin.
- PR #132: created as draft on Wave 0, kept up to date as each wave merged,
  flipped to ready-for-review at Wave 7 close.

## Workflow outcome measures

- `discovery-reuse`: scout step skipped (task spec was already a fully-scoped
  task list at `docs/roadmap/tasks/57b-preemption-foundation-tasks.md`); the
  task list itself acted as the discovery brief, used unmodified by every
  implementer agent.
- `rescue-attempts`: 0. The Wave 7 (H) implementer did hit a mid-run auth
  failure during H.3-H.5; the coordinator picked up the partially-staged
  work (H.3 uncommitted, H.4/H.5 not started) and finished it directly
  rather than spawning a rescue agent — consistent with Core Rule 5 (never
  rescue while the original is still running) since the original had
  already terminated with a clear failure boundary.
- `abandonment-events`: 0.
- `re-review-loops`: 0. No track required a revision round; every track's
  diff was approved on first integration validation.

## Files of record

- Audit: `docs/handoffs/57b-spinlock-callsite-audit.md` (65 lock declarations classified, mapped to G.1–G.9 owners).
- Soak gate: `docs/handoffs/57b-soak-gate.md` (procedure, pass criteria, result-tracking table).
- Design: `docs/roadmap/57b-preemption-foundation.md` (status: Complete pending soak).
- Task list: `docs/roadmap/tasks/57b-preemption-foundation-tasks.md` (status: Complete pending soak).
- Roadmap README: `docs/roadmap/README.md` (Phase 57b row + Mermaid graph + Gantt chart updated).
