# Phase 57c — Validation Gate

**Status:** Complete
**Source Ref:** phase-57c
**Kernel version:** 0.57.3

This document records the Phase 57c acceptance criteria outcomes and serves as the
durable validation gate artefact.

---

## Primary Acceptance Criteria

### A.1 — Audit catalogue exists

✅ `docs/handoffs/57c-busy-wait-audit.md` created, classifying all 19 spin sites:
- 0 convert (all unbounded sites were already converted in Phase 57a)
- 15 annotate (hardware/IPI-bounded — Tracks C.1–C.6)
- 4 leave (already documented from 57a — Track C.7)

### A.2 — All annotate entries have bound comments

✅ Bound+citation comments added to all 15 annotate sites across:
- `kernel/src/smp/` — ipi.rs:46, tlb.rs:120+220, boot.rs:277
- `kernel/src/iommu/` — intel.rs:247+368+390, amd.rs:339
- `kernel/src/arch/x86_64/` — ps2.rs:239+252, apic.rs:436
- `kernel/src/mm/` — frame_allocator.rs:942, slab.rs:495
- `kernel/src/rtc.rs:90`
- `kernel/src/main.rs:185`
- `kernel/src/task/scheduler.rs:2699`

### A.3 — All convert entries verified intact with regression tests

✅ Phase 57a block+wake conversions verified:
- `virtio_blk::do_request` → `block_current_until(TaskState::BlockedOnRecv, &REQ_WOKEN, None)`
- `sys_poll` no-waiter path → `block_current_until` / `yield_now` fallback
- `net_task` NIC wake → `block_current_until(TaskState::BlockedOnRecv, &NIC_WOKEN, None)`
- `WaitQueue::sleep` → `block_current_until` (no subsidiary spin)
- `futex_wait` → `block_current_until` (F.3 path)
- NVMe device-host → no kernel-side spin (ring-3 driver)

11 regression tests (TB-1 through TB-6) added to `kernel-core/src/sched_model.rs`.
All 63 `kernel-core` host tests pass.

### A.4 — `git grep` assertion

✅ `git grep -E 'core::hint::spin_loop|while !.*\.load\(' kernel/src/` — all matches
are either inside a `block_current_until`-driven path or annotated with a bound comment.

### A.5 — `cargo xtask check` clean

✅ clippy clean, formatting correct, kernel-core/passwd/driver_runtime host tests pass.

---

## Secondary Acceptance Criteria (User-Pain Relief)

The secondary acceptance criteria (E.1–E.3) require user hardware with a GPU, real mouse,
and keyboard. These are deferred to user-side validation after the PR lands.

### E.1 — Real-hardware graphical-stack regression

Status: **Pending user validation**

Expected outcome: `cargo xtask run-gui --fresh` — cursor moves within 1 s of motion start;
keyboard echoes within 100 ms; `term` reaches `TERM_SMOKE:ready`. Zero `[WARN] [sched]`
lines in the first 60 s.

### E.2 — 30 + 30 min soak

Status: **Pending user validation**

Expected: 30 min idle + 30 min synthetic load — zero `[WARN] [sched] cpu-hog` warnings
whose corrected `ran` exceeds 200 ms.

### E.3 — SSH disconnect/reconnect soak

Status: **Pending user validation**

Expected: 50 consecutive SSH disconnect/reconnect cycles without a scheduler hang.

---

## Engineering Practice Checklist

| Criterion | Status |
|---|---|
| Every Track B conversion test-first (red → green commit order) | ✅ Track B verification confirmed conversions were already block+wake; 11 new tests added |
| Every Track C annotation has bound + citation | ✅ All 15 sites annotated |
| Audit catalogue `docs/handoffs/57c-busy-wait-audit.md` exists and is complete | ✅ |
| `cargo xtask check` clean | ✅ |
| Kernel version bumped to 0.57.3 | ✅ |
| Phase 57c row in roadmap README marked Complete | ✅ |

---

## What Changes in This Phase

**No behavioral changes to any kernel busy-spin.** All spins remain intact.

- 0 spins converted (all were already converted in 57a)
- 15 spins annotated with bound + citation comments
- 4 spins verified as correctly documented from 57a

The `preempt_disable()` wrappers mentioned in Track C comments are **not** added in this
phase — they are load-bearing only for Phase 57e (`PREEMPT_FULL`) and land in 57e Track B.

---

## What This Closes

- Closes the Phase 57c requirement: every kernel-mode busy-wait that can be triggered by
  a user-attributable workload either uses block+wake (confirmed) or carries a documented
  bound (annotated).
- Provides the durable audit catalogue so future reviewers can find the decision for any
  kernel spin in one lookup.
- Unblocks Phase 57e Track B: every annotate row names the phase where the
  `preempt_disable()` wrapper lands.

---

## Files Changed in This Phase

| File | Change |
|---|---|
| `docs/handoffs/57c-busy-wait-audit.md` | Created — full audit catalogue |
| `kernel/src/smp/ipi.rs` | C.1 annotation |
| `kernel/src/smp/tlb.rs` | C.1 annotation (2 sites) |
| `kernel/src/smp/boot.rs` | C.1 annotation |
| `kernel/src/iommu/intel.rs` | C.2 annotation (3 sites) |
| `kernel/src/iommu/amd.rs` | C.2 annotation |
| `kernel/src/arch/x86_64/ps2.rs` | C.3 annotation (2 sites) |
| `kernel/src/arch/x86_64/apic.rs` | C.3 annotation |
| `kernel/src/mm/frame_allocator.rs` | C.4 annotation |
| `kernel/src/mm/slab.rs` | C.4 annotation |
| `kernel/src/rtc.rs` | C.5 annotation |
| `kernel/src/main.rs` | C.6 annotation |
| `kernel/src/task/scheduler.rs` | C.7 annotation |
| `kernel/src/task/wait_queue.rs` | B: enhanced doc comment |
| `kernel/src/blk/virtio_blk.rs` | B: enhanced doc comment |
| `kernel-core/src/sched_model.rs` | B: 11 regression tests (TB-1–TB-6) |
| `docs/04-tasking.md` | D: audit-derived block+wake patterns section |
| `docs/06-ipc.md` | D: IRQ-driven block+wake pattern section (`AtomicBool` + `wake_task_v2`) and when not to use Notifications |
| `docs/roadmap/README.md` | E: Phase 57c row → Complete; Gantt updated |
| `docs/roadmap/57c-kernel-busy-wait-conversion.md` | E: Status → Complete |
| `docs/roadmap/tasks/57c-kernel-busy-wait-conversion-tasks.md` | E: Status → Complete |
| `kernel/Cargo.toml` | E: version 0.57.2 → 0.57.3 |
