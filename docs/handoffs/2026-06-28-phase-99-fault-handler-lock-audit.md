---
status: COMPLETE — Phase 99 Track B deliverable (the origin audit the 2026-06-14
  handoff explicitly deferred). NO lock-across-fault violations found; the
  MAP_LAZY_FILE demand-fault discipline is confirmed and now enforced by a debug
  assertion; the per-core recovery-stack + recursion-latch contract is re-verified;
  the kstack-overflow origin is characterised (64 KiB sufficient given the
  fault_kill_trampoline recovery). One lock-ordering observation recorded for
  separate follow-up.
date: 2026-06-28
phase: phase-99
component: kernel/arch/x86_64/interrupts.rs (fault handlers + recovery), kernel/process
  (shared_vma_demand_file), kernel/task/kstack
related:
  - docs/handoffs/2026-06-14-claude-smp-tlb-shootdown-kstack-panic.md   # landed the recovery; deferred this origin audit
  - docs/post-mortems/2026-04-21-scheduler-lock-isr-deadlock.md         # the lock-held-across-fault bug class
  - docs/roadmap/tasks/99-smp-scheduler-robustness-tasks.md             # Track B.1 / B.2 / B.3
---

# Phase 99 — Fault-Handler Lock & Recovery Audit (Track B)

The 2026-06-14 SMP/TLB-shootdown/kstack handoff landed the *recovery* (NMI-on-IST,
mark-core-offline-on-halt, degrade-shootdown-timeout, `fault_kill_trampoline`) and
explicitly **deferred the origin audit**. This is that audit.

## B.1 — Locks-held-across-faults sweep + asserted invariant

**Audited:** `page_fault_handler` (`interrupts.rs:1539`), `general_protection_fault_handler`
(`:1970`), `double_fault_handler` (`:2202`), `fault_kill_trampoline` (`:303`),
`try_recover_kstack_overflow` (`:214`), plus the supporting chain
`demand_map_vma_page` → `shared_vma_demand_file` (`process/mod.rs:1308`) →
`vfs_service_handle_for_fd` / `claim_vfs_read_window` / `kernel_read_fd_at`.

**Result: NO LOCK-ACROSS-FAULT VIOLATIONS.** Every `SCHEDULER`/`PROCESS_TABLE`
lock-acquire site in the audited functions releases its guard before any blocking IPC or
fault-prone operation.

| # | Function (site) | Lock | Released before blocking/fault? |
|---|---|---|---|
| 1 | `page_fault_handler` ring-3 kill | `SCHEDULER` via `try_lock_scheduler` (`:1784`) | Yes — drops before `dump_crash_context` + trampoline |
| 2 | PKU read-recovery `shared_pkey_table` (`process/mod.rs:1363`) | `PROCESS_TABLE` | Yes — returns Copy `pkey_table`, guard drops; no IPC after |
| 3 | MAP_LAZY_FILE `shared_vma_demand_file` (`process/mod.rs:1309`) | `PROCESS_TABLE` | **Yes** — returns value copies `(prot,pkey,fd,off,end)`; `vfs_read_into_window` (`:982`) + `demand_read_file_page` (`:1040`) run with no `PROCESS_TABLE` held |
| 4 | MAP_LAZY_FILE `vfs_service_handle_for_fd` (`syscall/mod.rs:9477`) | `PROCESS_TABLE` | Yes — returns `Option<u64>` before blocking IPC |
| 5 | MAP_LAZY_FILE `claim_vfs_read_window` reclaim (`syscall/mod.rs:9393`) | `PROCESS_TABLE` | Yes — temporary `.is_none()` borrow, dropped at `;` |
| 6 | `kernel_read_fd_at` (`syscall/mod.rs:12087`) | `PROCESS_TABLE` | Yes — FdEntry moved out, guard drops at `:12092`; `vfs_service_read_kernel` IPC at `:12163` after |
| 7 | anon zero-fill `shared_vma_prot_and_pkey` (`process/mod.rs:1293`) | `PROCESS_TABLE` (nested in page-table guard) | Yes — returns `(prot,pkey)`, no IPC after — **but see lock-ordering note** |
| 8 | `general_protection_fault_handler` kill | `SCHEDULER` via `try_lock_scheduler` (`:2023`) | Yes — drops before dumps + trampoline |
| 9,10 | `kstack_overflow_minimal_kill` (`:376`, `:386`) | `PROCESS_TABLE` ×2 | Yes — each a narrow `{}` block; no blocking IPC after |

**Asserted invariant (landed).** A `debug_assert_eq!(current_preempt_count(), 0, …)` is
added at the entry to the blocking lazy-file demand-fault path (`interrupts.rs`, just above
the existing budgeted deadlock-guard log). Both `SCHEDULER` and `PROCESS_TABLE` are
`IrqSafeMutex`es whose `lock()` raises `preempt_count` (Phase 57b F.1), so
`current_preempt_count() == 0` is the exact, non-flaky (per-task, not sibling-affected)
encoding of "no such lock held on this core on entry to the blocking IPC." `cargo xtask
check` passes with the assertion compiled in.

**Lock-ordering note (tracked separately, not a blocking violation):** in the anonymous
zero-fill path (#7), `PROCESS_TABLE` is taken *inside* the addr-space page-table guard
(`interrupts.rs:~1089` → `process/mod.rs:1293`). No blocking happens while either is held,
so it does not deadlock, but it is an inner/outer nesting relative to paths that take
`PROCESS_TABLE` outer. Recorded here for a future dedicated lock-ordering pass; out of
scope for Phase 99 (no blocking/fault hazard).

## B.2 — kstack-overflow origin

The 2026-06-14 Open Question #1 (the exact call chain that exhausts the 64 KiB per-task
kernel stack under V8/PKU churn) was unrecoverable from the corrupted pre-IST frame; with
NMI-on-IST + the per-core recovery stack it is now capturable. Characterisation:

- **The deepest contributing kernel chain is the MAP_LAZY_FILE demand-fault →
  readahead-cluster → blocking `vfs_server` IPC** path (`demand_map_vma_page`), which every
  toolchain DSO demand-pages. The B.1 sweep above confirms this path holds **no** kernel
  lock across its blocking IPC, so a deep instance switches the task out cleanly rather than
  stranding a lock — it is depth, not lock-holding, that bounds it.
- **64 KiB is sufficient given the recovery.** A task-attributable overflow is converted to
  a `SIGSEGV` of the offending process by `fault_kill_trampoline` (validated by
  `kstack-overflow-smoke`), so the exact worst-case depth is no longer a liveness hazard —
  it is survivable regardless of origin. The reverted 64→96 KiB experiment
  (2026-06-25 handoff) is explicitly **not** the fix; bumping the stack on a false
  "stack-overflow" premise was shown not to help.
- **Decision:** keep 64 KiB usable + 4 KiB guard. The recovery makes a rare deep
  demand-fault chain non-fatal; spending +17 MiB of committed kstack RAM to chase a depth
  that the recovery already handles is not warranted. If a *non-task-attributable*
  (pid==0) overflow is ever observed (which halts rather than recovers), revisit by
  capturing the IST-clean frame from `kstack-overflow-smoke` and bounding that specific
  path.

## B.3 — Recovery-stack & recursion-latch correctness (re-verified)

- **Per-core recovery stacks.** `FAULT_RECOVERY_STACKS[MAX_CORES]` (`interrupts.rs:191`) is
  indexed by `fault_recovery_stack_top(core_id.min(MAX_CORES-1))` (`:195`), and
  `try_recover_kstack_overflow` uses `page_fault_core_index()` (= `current_core_id()`).
  **Each core uses its own slot** — no cross-core sharing; the `.min` clamp guards
  pathological LAPIC IDs.
- **`IN_KERNEL_PAGE_FAULT` latch.** Per-core array (`:271`). SET via CAS `false→true` on
  first ring-0 #PF entry (`:1845`); on a *true recursive cascade* the re-entry CAS returns
  `Err` → compact message → `hlt_loop()` with the flag **left set** (correct — prevents
  deeper recursion). CLEARED (`:229`) **only** on the genuine recovery path inside
  `try_recover_kstack_overflow` (kstack guard-page fault, `pid != 0`), before IRETQ to the
  trampoline on the clean stack. So a later legitimate ring-0 fault on the recovered core
  gets its full diagnostic dump rather than being mistaken for a cascade.
- **`current_pid() != 0` gate.** `try_recover_kstack_overflow` returns `false` (→ caller
  `hlt_loop()`) when per-core data isn't ready or `pid == 0` (idle/kernel-thread). Only a
  task-attributable userspace overflow is redirected to `fault_kill_trampoline`.

**Validation:** `kstack-overflow-smoke` (`M3OS_KSTACK_OVERFLOW_REGRESSION`) and the
`dynamic-hello-smoke` `THREAD_FAULT` (`leader-ok`/`worker-ok`) arms stay green (Phase 99
validation run).

## Verdict

`NO LOCK-ACROSS-FAULT VIOLATIONS FOUND`; recovery contract `PER-CORE CORRECT`; kstack
origin characterised (64 KiB retained, recovery makes depth non-fatal). The deferred
2026-06-14 origin audit is closed.
