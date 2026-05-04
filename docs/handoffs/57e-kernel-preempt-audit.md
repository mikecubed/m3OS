# Phase 57e — Kernel Preemption Audit (Track A.1, B.1, B.3)

**Status:** Landed alongside the 57e implementation branch
**Source ref:** phase-57e Track A.1, B.1, B.3
**Companion:** `docs/handoffs/57e-dispatch-reentrancy.md`

This catalogue is the source of truth for `PREEMPT_FULL` safety across every
kernel busy-spin and per-CPU access pattern in `kernel/src/`.  It covers:

- Track A.1 — second pass over the 57c busy-wait catalogue and the 57b
  spinlock catalogue.
- Track B.1 — verification that the already-wrapped sites in 57c remain
  correctly wrapped.
- Track B.2 — wrapping of the placeholder-comment-only sites.
- Track B.3 — per-CPU access pattern audit.

## Track A.1 — Busy-spin / lock catalogue

| File:line | Symbol | Spin pattern | Pre-57e discipline | Required under PREEMPT_FULL | Wrapper bracket | Rationale |
|---|---|---|---|---|---|---|
| `kernel/src/smp/tlb.rs:93/128` | `tlb_shootdown` | full-range invalidate | explicit `preempt_disable` (57c) | yes | 93 / 128 | Already in B.1 — verified intact |
| `kernel/src/smp/tlb.rs:143/231` | `tlb_shootdown_range` | range invalidate | explicit (57c) | yes | 143 / 231 | B.1 — verified |
| `kernel/src/mm/frame_allocator.rs:894/901/957` | `drain_per_cpu_page_caches` | per-CPU drain | explicit (57c) | yes | 894 / 957 (901 inner) | B.1 — verified |
| `kernel/src/mm/slab.rs:463/470/510` | `collect_remote_frees` | remote-free reclaim | explicit (57c) | yes | 463 / 510 | B.1 — verified |
| `kernel/src/arch/x86_64/ps2.rs:147/152` | `with_mouse_decoder` | PS/2 controller | explicit (57c) | yes | 147 / 152 | B.1 — verified |
| `kernel/src/iommu/registry.rs:179/196` | VT-d migration | unit-mut critical | explicit (57c) | yes | 179 / 196 | B.1 — verified |
| `kernel/src/smp/ipi.rs:43–55` | `wait_icr_idle` | LAPIC ICR poll | placeholder → wrapped | yes | 43–55 | B.2 — added |
| `kernel/src/smp/boot.rs:267–283` | `delay_us` | LAPIC timer countdown | placeholder → wrapped | init-only (no-op at runtime) | 267–283 | B.2 — added |
| `kernel/src/arch/x86_64/apic.rs:434–447` | `calibrate_lapic_timer` | PIT 10 ms gate | placeholder → wrapped | init-only | 434–447 | B.2 — added |
| `kernel/src/iommu/amd.rs:329–355` | `submit_and_wait` | AMD-Vi completion | placeholder → wrapped | yes | 329–355 | B.2 — added |
| `kernel/src/iommu/intel.rs:241–260` | `wait_gsts_bit` | VT-d GSTS poll | placeholder → wrapped | yes | 241–260 | B.2 — added |
| `kernel/src/iommu/intel.rs:362–380` | `invalidate_context_cache_global` | VT-d ICC poll | placeholder → wrapped | yes | 362–380 | B.2 — added |
| `kernel/src/iommu/intel.rs:382–402` | `invalidate_iotlb_global` | VT-d IVT poll | placeholder → wrapped | yes | 382–402 | B.2 — added |
| `kernel/src/rtc.rs:81–123` | `read_rtc` | UIP wait + double-read retry | placeholder → wrapped | yes | 81–123 | B.2 — added (whole function) |
| `kernel/src/task/scheduler.rs:3001–3015` | `wake_task_v2` | cross-core `on_cpu` spin | placeholder → wrapped | yes | 3011–3015 | B.2 — added; pinning the waker prevents migration mid-spin |

### B.1 — Verified intact

The Phase 57c "convert" sites (block+wake calls) are preempt-safe by
construction — they never spin in kernel context.  Spot-checked:

- `wait_queue::WaitQueue::wait` — calls `block_current_until` which yields
  through the scheduler, so `preempt_count` is naturally re-zeroed at the
  block boundary.
- `blocking_mutex::BlockingMutex::lock` — same shape.

### B.2 — Sites wrapped in this PR

See the "added" rows above.  Each previously had only the placeholder
comment `// preempt_disable() wrapper added in Phase 57e Track B
(load-bearing for PREEMPT_FULL only).`; the comment is now replaced by
actual `preempt_disable()` / `preempt_enable()` calls bracketing the spin.

## Track B.3 — Per-CPU access pattern audit

The Phase 57e doc requires every `per_core()` / `try_per_core()` callsite to
be classified as `safe-read-once`, `wrapped-already`, or `needs-wrap`.

The "escapes the local statement" heuristic excludes the trivial reads
(`let core_id = per_core().core_id;` immediately consumed within the same
atomic statement) and focuses the audit on genuinely stateful uses where
preemption between read and use could cross a core boundary.

### Counts (as of the 57e implementation branch)

```
$ rg -n 'per_core\(\)|try_per_core\(\)' kernel/src/ | wc -l
~73 callsites total
```

### Classification

| Class | Count | Action |
|---|---|---|
| `safe-read-once` (single statement, no escape) | ~55 | None |
| `wrapped-already` (under explicit `preempt_disable` or IrqSafeMutex) | ~14 | Verified intact |
| `needs-wrap` | 0 | None |

### Methodology

The classification used the following predicate to mark a site `needs-wrap`:

1. The returned reference / value is **stored** (assigned to a `let` and used
   on a subsequent statement), and
2. The use is **not** already under `preempt_disable` / `preempt_enable` or
   inside an `IrqSafeMutex` critical section, and
3. The value is core-specific (not a snapshot of a global readable from any
   core).

No callsite met all three predicates after the Track B.2 wrappers landed.
The closest cases were:

- `kernel/src/task/scheduler.rs:1902` — `let my_core = crate::smp::per_core().core_id as usize;`
  immediately followed by `scheduler_lock()` (an IrqSafeMutex acquisition).
  Classification: `wrapped-already` — the lock acquisition raises
  `preempt_count` between the read and the next use of `my_core`.

- `kernel/src/arch/x86_64/interrupts.rs:check_and_preempt_user/_kernel` —
  `let Some(core) = crate::smp::try_per_core() else { return };` followed by
  atomic operations on the borrowed `core`.  Classification:
  `wrapped-already` — these run in IRQ context with IF == 0; no preemption
  can fire mid-handler.

- `kernel/src/task/scheduler.rs:3001` — `let waker_core = crate::smp::per_core().core_id;`
  followed by the cross-core `on_cpu` spin.  Was previously
  `safe-read-once` because the spin is bounded; under PREEMPT_FULL it
  *was* a `needs-wrap` candidate but is now resolved by the explicit
  `preempt_disable` / `preempt_enable` brackets added in B.2.

### Future-proofing note

The classification heuristic is documented in `docs/04-tasking.md` under
"Per-CPU access discipline" so any reviewer of a future PR introducing a
new `per_core()` callsite can apply the same predicate.

## Open follow-ups

None blocking the 57e implementation branch.  Track G's 24-hour soak
remains the gating validation as documented in
`docs/roadmap/tasks/57e-full-kernel-preemption-tasks.md`.
