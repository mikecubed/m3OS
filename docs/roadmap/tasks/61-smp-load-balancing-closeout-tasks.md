# Phase 61 — Phase 35 SMP Load Balancing Closeout: Task List

**Status:** Planned
**Source Ref:** phase-61
**Depends on:** Phase 25 (SMP) ✅, Phase 35 (True SMP Multitasking) ✅, Phase 52d (Kernel Completion and Roadmap Alignment) ✅, Phase 57e (Full Kernel Preemption — Deferred 2026-05-07) ✅
**Goal:** Uncomment `maybe_load_balance()` in the scheduler dispatch loop; add the per-run-queue length `AtomicUsize` counter it requires; wire `tlb_shootdown` into `munmap`; verify and correct pipe and IPC wait-queue object attachment; implement child CPU times in `sys_wait4`/`sys_getrusage`; run a 10-minute SMP soak; flip Phase 35 E.1/G.2/G.3/H.3 and Phase 25 P25-T033 to `[x]`.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Per-run-queue `AtomicUsize` length counter | — | Planned |
| B | Uncomment + harden `maybe_load_balance` | A | Planned |
| C | `munmap` TLB shootdown IPI | — | Planned |
| D | Pipe + IPC wait-queue object attachment verification | — | Planned |
| E | Child CPU times — `sys_wait4` / `sys_getrusage` | — | Planned |
| F | Regression + 10-minute SMP soak | A B C D E | Planned |
| G | Phase 35 + Phase 25 doc updates | F | Planned |
| H | Documentation and Release | F G | Planned |

---

## Track A — Per-Run-Queue Length Counter

### A.1 — Add `AtomicUsize` queue length to `PerCpuScheduler`

**File:** `kernel/src/task/scheduler.rs`
**Symbol:** `PerCpuScheduler` struct; `enqueue_to_core`; `dequeue_from_core`
**Why it matters:** `maybe_load_balance()` needs to compare run-queue lengths across CPUs without holding the global lock. An `AtomicUsize` per CPU satisfies this with relaxed atomics on the reader side.

**Acceptance:**
- [ ] `PerCpuScheduler` has a `queue_len: AtomicUsize` field.
- [ ] `enqueue_to_core` calls `queue_len.fetch_add(1, Relaxed)` before returning.
- [ ] `dequeue_from_core` (or equivalent pop path) calls `queue_len.fetch_sub(1, Relaxed)` before returning.
- [ ] `cargo xtask check` passes after this change alone (no other changes).

---

## Track B — Uncomment and Harden `maybe_load_balance`

### B.1 — Uncomment `maybe_load_balance()` call in dispatch loop

**File:** `kernel/src/task/scheduler.rs`
**Symbol:** `pick_next` (or equivalent dispatch entry); `maybe_load_balance`
**Why it matters:** The single commented-out call is the reason Phase 35's load balancing is inoperative. The function exists; it just never runs.

**Acceptance:**
- [ ] `maybe_load_balance()` call is uncommented in the scheduler dispatch loop.
- [ ] An imbalance threshold constant `BALANCE_THRESHOLD: usize = 2` is added (configurable at compile time via a Cargo feature or const).
- [ ] `maybe_load_balance` reads `queue_len` atomics (Track A) to identify the most and least loaded CPUs.
- [ ] Balancing only proceeds when the global `SCHEDULER` lock can be acquired without spinning (use `try_lock` if available, or acquire + check again).
- [ ] `cargo xtask test` passes after this change.

### B.2 — Write a load-balancing correctness test

**File:** `kernel/src/task/tests/load_balance.rs` (new) or `kernel-core/src/` host test
**Symbol:** `test_load_balance_distributes_tasks` (new)
**Why it matters:** Proves that tasks distribute under load balancing without a manual observation session.

**Acceptance:**
- [ ] Test spawns 8 CPU-bound tasks on a 2-core QEMU instance; after 5 seconds, reads per-CPU queue lengths; asserts neither queue exceeds the other by more than `BALANCE_THRESHOLD + 1`.
- [ ] `cargo xtask test --test load_balance` passes.

---

## Track C — `munmap` TLB Shootdown

### C.1 — Wire `tlb_shootdown` IPI into `munmap`

**File:** `kernel/src/mm/user_space.rs`
**Symbol:** `munmap` (or `unmap_range`); existing `tlb_shootdown` IPI sender
**Why it matters:** Without shootdown IPIs, cores other than the unmapping core retain stale TLB entries for freed pages. This is a silent correctness hazard under SMP — a use-after-free may read stale memory rather than triggering a fault.

**Acceptance:**
- [ ] After `invlpg` on the local core, `munmap` sends a TLB-shootdown IPI to all other active cores sharing the address space.
- [ ] Each receiving core's shootdown IPI handler calls `invlpg` for the relevant range.
- [ ] Initiating core waits for acknowledgement from all other cores before returning (bounded spin, max 10 µs per core).
- [ ] `cargo xtask test` passes; no new kernel panic under SMP.

### C.2 — Write SMP memory-unmap correctness test

**File:** `kernel/src/mm/tests/smp_munmap.rs` (new)
**Symbol:** `test_smp_munmap_no_stale_tlb` (new)
**Why it matters:** Automated proof that the TLB shootdown reaches all cores.

**Acceptance:**
- [ ] Test maps a page on core 0, writes a sentinel value, then unmaps the page; a task on core 1 attempts to access the address and receives a page fault (not a stale read).
- [ ] `cargo xtask test --test smp_munmap` passes.
- [ ] Phase 25 task-doc P25-T033 cross-references this test.

---

## Track D — Wait-Queue Attachments

### D.1 — Verify pipe wait-queue is attached to `PipeBuf` not per-CPU

**File:** `kernel/src/fs/pipe.rs`
**Symbol:** `PipeBuf` (or `Pipe`) wait-queue head field
**Why it matters:** If the wait-queue is per-CPU, a producer on core 0 cannot wake a consumer on core 1. Phase 35 G.2 requires the pipe wait-queue to be object-attached.

**Acceptance:**
- [ ] `PipeBuf` (or equivalent) contains a `WaitQueue` (or equivalent) field — not a reference to a per-CPU structure.
- [ ] A producer calling `write` wakes all waiters on the `PipeBuf` wait queue regardless of which core the producer runs on.
- [ ] A multi-core pipe test (`echo foo | cat` across two cores) passes without deadlock.
- [ ] Phase 35 G.2 checkbox flipped to `[x]` with this file+symbol citation.

### D.2 — Verify IPC endpoint wait-queue is attached to `Endpoint` not per-CPU

**File:** `kernel/src/ipc/endpoint.rs`
**Symbol:** `Endpoint` sender/receiver wait-queue fields
**Why it matters:** Phase 35 G.3 requires IPC wait-queue wakeup to be core-agnostic. A client blocked on `sys_ipc_recv` must wake when the server calls `sys_ipc_send` from any core.

**Acceptance:**
- [ ] `Endpoint` struct contains the wait-queue head for blocked senders and receivers.
- [ ] `sys_ipc_send` wakes the receiver's wait-queue entry regardless of which core the sender runs on.
- [ ] A cross-core IPC test (client on core 0, server on core 1) passes without deadlock.
- [ ] Phase 35 G.3 checkbox flipped to `[x]` with this file+symbol citation.

---

## Track E — Child CPU Times

### E.1 — Implement child CPU time accumulation in `sys_wait4` and `sys_getrusage`

**File:** `kernel/src/arch/x86_64/syscall/mod.rs`
**Symbol:** `sys_wait4`; `sys_getrusage`; `Task` CPU-time accumulation fields
**Why it matters:** Phase 35 H.3 requires that `sys_wait4` returns the child's accumulated user and kernel CPU ticks in the `rusage` output parameter. Currently the `rusage` fields are zeroed.

**Acceptance:**
- [ ] `Task` struct has `user_ticks: AtomicU64` and `kernel_ticks: AtomicU64` accumulation fields (or equivalent).
- [ ] The scheduler's context-switch accounting increments the appropriate field on every tick.
- [ ] `sys_wait4` populates `rusage.ru_utime` and `rusage.ru_stime` from the exiting child's accumulated fields.
- [ ] `sys_getrusage(RUSAGE_CHILDREN, ...)` sums accumulated CPU times from all waited children.
- [ ] A `wait4` test that runs a known-duration CPU-bound child verifies that `ru_utime` is non-zero and plausible.
- [ ] Phase 35 H.3 checkbox flipped to `[x]` with these file+symbol citations.

---

## Track F — Regression and Soak

### F.1 — Full regression pass

**Files:** `xtask/src/main.rs` (test harness)
**Symbol:** `cargo xtask test`
**Why it matters:** Tracks A–E all touch the scheduler hot path or memory management. A regression in any existing test indicates a correctness violation.

**Acceptance:**
- [ ] `cargo xtask test` passes with zero regressions.
- [ ] `cargo xtask check` (clippy -D warnings + rustfmt) passes.
- [ ] No new `unsafe` block introduced without an adjacent `// SAFETY:` comment.

### F.2 — 10-minute SMP soak

**File:** `docs/handoffs/61f-smp-soak.md` (new log artifact)
**Symbol:** QEMU 2-core instance under sustained IPC load
**Why it matters:** Load balancing and TLB shootdown both touch SMP-sensitive paths. A soak test exercises the race surfaces that unit tests cannot reach.

**Acceptance:**
- [ ] `cargo xtask run` with 2 QEMU cores, 8 CPU-bound tasks + sshd + display_server running for 10 minutes.
- [ ] Zero kernel panics or WARNINGs in serial log.
- [ ] `docs/handoffs/61f-smp-soak.md` populated with QEMU command, duration, observed events, and pass/fail verdict.

---

## Track G — Phase 35 and Phase 25 Doc Updates

### G.1 — Flip Phase 35 task-doc checkboxes and update design doc

**Files:**
- `docs/roadmap/tasks/35-true-smp-multitasking-tasks.md`
- `docs/roadmap/35-true-smp-multitasking.md`

**Symbol:** E.1, G.2, G.3, H.3 checkbox items
**Why it matters:** Closes the Phase 35 audit gaps that were the primary driver for creating Phase 61.

**Acceptance:**
- [ ] E.1 (`maybe_load_balance` uncommented + queue-length counter) flipped to `[x]` citing `scheduler.rs::maybe_load_balance`.
- [ ] G.2 (pipe wait-queue attachment) flipped to `[x]` citing `pipe.rs::PipeBuf`.
- [ ] G.3 (IPC wait-queue attachment) flipped to `[x]` citing `endpoint.rs::Endpoint`.
- [ ] H.3 (child CPU times) flipped to `[x]` citing `syscall/mod.rs::sys_wait4`.
- [ ] Phase 35 design doc audit note updated: `maybe_load_balance` uncommented in Phase 61; load balancing is now operational.

### G.2 — Flip Phase 25 P25-T033 and update design doc

**Files:**
- `docs/roadmap/tasks/25-smp-tasks.md`
- `docs/roadmap/25-smp.md`

**Symbol:** P25-T033 checkbox item
**Why it matters:** The TLB-shootdown deferral has been open since Phase 25 shipped. Phase 61 Track C closes it.

**Acceptance:**
- [ ] P25-T033 flipped to `[x]` citing `user_space.rs::munmap` + Phase 61 Track C.2 test.
- [ ] Phase 25 design doc receives a one-line note in the relevant section: "TLB shootdown wired into munmap in Phase 61."

---

---

## Track H — Documentation and Release

### H.1 — Create the aligned legacy learning doc

**File:** `docs/61-smp-load-balancing-closeout.md`
**Symbol:** new file
**Why it matters:** The doc-template "aligned legacy learning doc" form gives a learner-friendly companion to the design + task docs. Every shipped phase has one (or has a deliberate exception). This file is created from the template in `docs/appendix/doc-templates.md` § "Template: aligned legacy learning doc".

**Acceptance:**
- [ ] `docs/61-smp-load-balancing-closeout.md` exists, follows the template (Aligned Roadmap Phase, Status, Source Ref, Supersedes Legacy Doc / new — all present)
- [ ] Overview paragraph is learner-friendly and explains the phase outcome in plain language
- [ ] "What This Doc Covers" lists 3+ concrete topics
- [ ] "Core Implementation" is written for a learner who has not read the design or task doc
- [ ] "Key Files" table cites the actual files this phase touches
- [ ] "How This Phase Differs From Later SMP Work" (or analogous heading specific to this phase) is filled in
- [ ] "Related Roadmap Docs" links the design and task docs

### H.2 — Bump kernel version to 0.61.0

**Files:**
- `kernel/Cargo.toml`
- `Cargo.lock`
- `AGENTS.md`
- `docs/roadmap/README.md` (any version annotations)

**Symbol:** `version` field in `kernel/Cargo.toml` `[package]` section
**Why it matters:** Phase closure is signalled by a kernel version bump per project convention. Each new phase moves the project from `0.<previous>.x` to `0.<NN>.0`. The `AGENTS.md` "Kernel v0.X.Y" reference must move with it (per audit Red Flag — `AGENTS.md` was found stale at `v0.51.0` during the 2026-05-08 audit).

**Acceptance:**
- [ ] `kernel/Cargo.toml` `version = "0.61.0"`
- [ ] `Cargo.lock` regenerated (`cargo generate-lockfile` or similar)
- [ ] `AGENTS.md` "Kernel v0.61.0" reference updated
- [ ] `cargo xtask check` passes after the bump
- [ ] Git tag suggestion: `v0.61.0` (tag at phase merge, not at task-checkbox tick)

---

## Documentation Notes

- The global `SCHEDULER` lock is intentionally retained. Do not attempt to remove it as part of Track B — that is the per-core lock-free dispatch deferral explicitly owned by a future phase. Comments in the code should make this explicit: `// NOTE: global SCHEDULER lock retained — per-core lock-free dispatch deferred per Phase 52d`.
- Track C (munmap TLB shootdown) is the highest-risk change in this phase. Prefer a conservative implementation (always shoot all cores, ignore the page-table walk) over an optimized one (compute the exact core set from PTE tracking). The conservative approach is correct; the optimized one can be a post-1.0 improvement.
- Track D items may turn out to be already correctly implemented. In that case, verify through code review and a multi-core test, then flip the checkboxes with a "verified — no change required" note. Do not skip the verification.
- The `BALANCE_THRESHOLD` constant in Track B should be added to `kernel/src/task/scheduler.rs` as a documented constant with a comment explaining the tradeoff, not buried in the function body.
