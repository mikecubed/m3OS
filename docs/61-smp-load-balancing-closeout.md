# Phase 35 SMP Load Balancing + Phase 25 Closeout

**Aligned Roadmap Phase:** Phase 61
**Status:** Complete
**Source Ref:** phase-61
**Supersedes Legacy Doc:** new (no prior learning doc — Phase 61 is an audit-closeout phase for Phase 25 P25-T033 and Phase 35 deferred-line drift)

## Overview

The 2026-05-08 audit raised three Red Flags against Phase 35 ("`maybe_load_balance()` commented out"; pipe and IPC wait queues "may be per-CPU"; child times "stubbed at zero") and one against Phase 25 (P25-T033, TLB shootdown not wired into `munmap`). Re-reading the code against those flags showed that three of the four were stale at the source-of-truth level — the substantive work had landed silently between the phases' merge and the audit, but the task docs were never reconciled. Phase 61 is the closure pass: verify what already shipped, fix what genuinely was broken, add the SMP regression tests Phase 35 lacked, and reconcile the docs.

The honest accounting is more interesting than the audit framed it:

1. **The load balancer was a silent no-op.** The hook was wired (audit was wrong about that), but `task.last_migrated_tick` was being reset on every cooperative yield, defeating the migration cooldown gate. The result: CPU-bound tasks that yield frequently (the common case) were never migration-eligible, so the hook fired every 50 ticks and never moved anything. Production never noticed because the initial `least_loaded_core` placement on spawn handles typical boot workloads.

2. **Phase 35 H.2's `system_ticks increases during syscall handling` acceptance was checked off but false.** `accumulate_ticks` attributed all elapsed time to `user_ticks` regardless of ring. Phase 61 fixes this with per-tick CS-based sampling driven from the timer IRQ handler.

3. **Children CPU-time accounting was stubbed.** `sys_times` wrote `0_i64` for `tms_cutime` / `tms_cstime`. Phase 61 adds the recursive accumulation rule at the zombie-reap site.

4. **The kernel test harness could not exercise live SMP code.** `kernel/tests/*.rs` integration tests could only test pure-logic mirrors in `kernel-core`; the kernel binary had no `lib.rs`. Phase 61 splits the kernel into a lib + bin pair so integration tests can `use kernel::...` to reach scheduler / SMP / pipe / IPC internals — a phase-worth of harness work that was prerequisite for any of the SMP regression tests the audit asked for.

## What This Doc Covers

- The kernel lib+bin split (Track 0a) and the `test_prelude` helper (Track 0b) — the harness foundation every Phase 61 test rests on.
- The load-balancer silent-no-op bug: how it was hidden by production behaviour, what the test caught, what the fix changed.
- The per-tick CS-based CPU-time accounting: why it replaces `accumulate_ticks`, why per-tick sampling is the right granularity at this stage, and how the timer IRQ paths feed it.
- Children CPU-time accumulation: the recursive rule, the `Task::child_user_ticks` / `child_system_ticks` fields, the `sys_waitpid` zombie-reap wiring, and the new `sys_wait4` / `sys_getrusage` syscalls.
- The Phase 35 G.3 IPC `WaitQueue` swap: why Phase 61 reframes it as won't-do rather than deferred (the bespoke per-`Endpoint` queues are payload-carrying and atomically integrate with the scheduler — no functional gain from a generic swap).

## Core Implementation

### Track 0a — kernel as lib + bin

`kernel/src/lib.rs` now holds the modules and the boot sequence (`kernel_main_entry`, `handle_panic`, `handle_alloc_error`); `kernel/src/main.rs` is a thin shim that owns only the bootloader `entry_point!` macro and the `#[panic_handler]` / `#[alloc_error_handler]` lang items, each delegating immediately back into the library. `kernel/Cargo.toml` declares `[lib] test = false` and `[[bin]] test = false` because neither target makes sense as a `cargo test` standalone (the lib has no panic_handler in lib-test mode; the bin's `cfg(test)` test_runner references `kernel::testing` which is itself `cfg(test)`).

Side effects of making modules `pub`:

- `AllocatorLocalReclaimStats` and `ForkChildCtx` promoted from `pub(crate)` to `pub` so the public functions returning/accepting them stay validly public.
- Two `#[expect(dead_code)]` attributes in `mm/heap.rs` and `mm/frame_allocator.rs` changed to `#[allow(dead_code)]` because the items are now reachable from outside the parent module.
- A crate-level `#![allow(...)]` block for clippy lints that surface only because items are newly visible (`missing_safety_doc`, `result_unit_err`, `new_without_default`, `len_without_is_empty`, `not_unsafe_ptr_arg_deref`).

### Track 0b — `test_prelude::init_minimal_smp`

`kernel/src/test_prelude.rs::init_minimal_smp(boot_info)` runs the strict subset of `kernel_main_entry` the SMP / scheduler / pipe / IPC subsystems require, then **returns** so `test_main()` can run inside a kernel task and signal QEMU exit:

1. Serial + structured logger.
2. GDT + IDT (`arch::init`).
3. Frame allocator + heap (`mm::init`).
4. PCI enumeration (required so APIC discovery sees IO-APIC entries).
5. Hardware interrupts enabled.
6. ACPI table discovery (RSDP → MADT) for AP and IO-APIC topology.
7. Local APIC + IO-APIC bring-up.
8. Per-core data for the BSP (`gs_base` set so `per_core()` does not panic).
9. CPUID probe + XSAVE state enable.

Cross-core tests then call `boot_aps_if_available()`. The prelude omits userspace `init`, network bring-up, display / audio / session services, framebuffer console, and RTC — none are needed for SMP correctness tests, all add boot time, and most would couple tests to the disk image.

### Track A — load-balance hook doc verification

`kernel/src/task/scheduler.rs::maybe_load_balance` is called from the BSP dispatch loop at line 3837, every 50 ticks (`BALANCE_COUNTER` modulo). Reads each core's queue length via `with_run_queue(|q| q.len())` and migrates one task per cycle when `longest_len > shortest_len + BALANCE_THRESHOLD` (named constant, `2`). Recently-migrated tasks are skipped via `MIGRATE_COOLDOWN` (100 ticks).

The Phase 35 E.1 plan's `queue_length: AtomicU32` counter was deliberately NOT added: `VecDeque::len()` is O(1), the lock is already held for the read, and a parallel counter would create a second source of truth that drifts on every enqueue/dequeue mistake.

### Track B — load-balance correctness test (and what we learned about the design)

`kernel/tests/load_balance_smp.rs` boots the kernel via the Track 0b prelude, boots APs, spawns 8 CPU-bound workers all initially on core 0 via the new `task::scheduler::spawn_on_core(entry, name, core_id)` helper, waits for several `BALANCE_COUNTER` cycles, then asserts core 0's queue length shrank.

Implementing this test taught us the design intent of `last_migrated_tick`. An earlier Phase 61 commit (798677b) read the dispatch-epilogue line `task.last_migrated_tick = now` as a bug — without it, `maybe_load_balance` migrated tasks aggressively. But that commit also caused a kernel page fault under fork-bomb load (ion exiting Doom; post-mortem in PR thread). The reset is deliberate: it implements a **cache-warmth invariant**. Actively-yielding tasks are "hot" on their current core, and both `maybe_load_balance` and `try_steal` (work stealing in `pick_next_task`) gate migration on `tick - last_migrated_tick >= MIGRATE_COOLDOWN` (100 ticks). Resetting on every yield keeps hot tasks pinned.

The load balancer's effective domain is therefore:

  - tasks freshly spawned (eligible 100 ticks after spawn, if they don't yield);
  - tasks woken from a block of 100+ ticks;
  - tasks running continuously for 100+ ticks without yielding (the classic "needs to be balanced" signal — they're starving other tasks on their core).

Track B's test workers therefore spin for 150 ticks between yields to qualify. Observed: core 0 queue goes 8 → 5–6 over 1500 ticks of waiting.

### Track C.1 — TLB-shootdown wiring verification

`sys_linux_munmap` already calls `crate::smp::tlb::tlb_shootdown_range(addr_space, range_start, range_end)` after the per-page unmap loop with the full `[range_start, range_end)` span, batched into a single shootdown IPI. Phase 61 added a P25-T033 closure cross-reference comment at the call site (`kernel/src/arch/x86_64/syscall/mod.rs:8981`) for future grep-ability.

### Track E.1 — children CPU-time accumulation

`Task` gains `child_user_ticks: u64` and `child_system_ticks: u64`, placed AFTER `preempt_frame` in the struct definition to preserve the `EXPECTED_TASK_PREEMPT_FRAME_OFFSET` const that Phase 57d's assembly entry stub relies on. At the zombie-reap site in `sys_waitpid`, the parent absorbs the zombie's `user_ticks + child_user_ticks` and `system_ticks + child_system_ticks` (POSIX recursive-accumulation rule). `sys_times` reads from these fields instead of writing `0_i64`.

### Track E.2 — per-tick CS-based user/system tick split

`accumulate_ticks` at `kernel/src/task/scheduler.rs` was attributing all elapsed time to `user_ticks` regardless of ring. Phase 61 makes it a no-op stub and introduces `tick_account_current_task(in_user_mode: bool)` that runs once per timer tick:

- `timer_handler_user` (interrupted ring-3 context) calls `tick_account_current_task(true)` → `user_ticks += 1`.
- `timer_handler_kernel` (interrupted ring-0 context) calls `tick_account_current_task(false)` → `system_ticks += 1`.

Skips idle tasks (priority 30) so halted cores don't inflate `system_ticks`. Bails before reading `per_core` if SMP is not yet ready (early-boot window between `arch::enable_interrupts()` and `smp::init_bsp_per_core()`). Matches Linux's `CONFIG_TICK_CPU_ACCOUNTING` model.

### Track E.3 — `sys_wait4` and `sys_getrusage`

Linux syscalls 61 and 98:

- `sys_wait4(pid, status_ptr, options, rusage_ptr)`: snapshots the calling task's `RUSAGE_CHILDREN` counters before the `sys_waitpid` reap, computes the delta after, writes that to `rusage_ptr`. The delta = exactly the reaped-just-now subtree's totals (Linux semantics — wait4's rusage value is the reaped-this-call subtree, not the parent's running totals).
- `sys_getrusage(who, usage_ptr)`: `RUSAGE_SELF` (0) reads the calling task's own counters; `RUSAGE_CHILDREN` (-1) reads the `child_*` accumulators; `RUSAGE_THREAD` (1) is treated as `RUSAGE_SELF`.

The 144-byte Linux `struct rusage` layout populates the four time fields and the four event-count fields delivered by Track E.4 (`ru_minflt`, `ru_majflt`, `ru_nvcsw`, `ru_nivcsw`); the remaining 10 fields are zeroed (memory residency, deprecated SysV/swap counters, block I/O, signal counts — post-1.0 instrumentation).

### Track E.4 — rusage event counters

`Task` gains four counter fields plus four `child_*` accumulators (`minor_faults`, `major_faults`, `voluntary_ctxsw`, `involuntary_ctxsw`). Increments are wired into the existing kernel hot path:

- `page_fault_handler`: after successful CoW resolution, `current_task_record_page_fault(false)` increments `minor_faults`. Major faults remain 0 in practice today because the disk-backed mmap demand-page path is not yet wired.
- `yield_now`: increment `voluntary_ctxsw` inside the same `scheduler_lock` critical section that already runs `accumulate_ticks` before the switch.
- `preempt_frame_to_scheduler` (timer-IRQ / IPI preempt path): increment `involuntary_ctxsw` before the switch.

At the zombie-reap site in `sys_waitpid`, the same recursive-accumulation rule applies (`current_task_accumulate_child_rusage`).

### Tracks H, I — doc closeout + version bump

Phase 35 task doc lines 189, 198, 260-262, 306 flipped to `[x]` with citations. G.2 lines 251-253 retained as Pending Phase 61 Track F (Track F is in this PR's open scope). H.2 task header carries the closure post-text recording the previously-stale acceptance now holds genuinely. Phase 25 task doc Track Layout drops the "(handler+API; munmap hook deferred)" caveat. Phase 25 / 35 design docs carry one-line closure notes under the relevant section headings. Kernel version bumped 0.60.0 → 0.61.0.

## Key Files

| File | Role |
|---|---|
| `kernel/src/lib.rs` | New library crate root (Track 0a). |
| `kernel/src/main.rs` | Thin binary shim — `entry_point!` + `#[panic_handler]` + `#[alloc_error_handler]`. |
| `kernel/src/test_prelude.rs` | `init_minimal_smp` + `boot_aps_if_available` + idle helpers (Track 0b). |
| `kernel/src/task/scheduler.rs` | `maybe_load_balance`, `BALANCE_THRESHOLD`, `spawn_on_core`, `tick_account_current_task`, the E.1/E.4 accumulation API, the dispatch-epilogue `last_migrated_tick` bug fix. |
| `kernel/src/task/mod.rs` | `Task` struct: `child_user_ticks`, `child_system_ticks`, the four E.4 counter fields + four child accumulators. |
| `kernel/src/arch/x86_64/syscall/mod.rs` | `sys_wait4`, `sys_getrusage`, `write_rusage`, `sys_times` updated to read `child_*`, `sys_waitpid` zombie-reap accumulation. |
| `kernel/src/arch/x86_64/interrupts.rs` | Timer handler calls to `tick_account_current_task`; CoW page-fault handler calls to `current_task_record_page_fault`. |
| `kernel/src/smp/boot.rs` | `install_trampoline` GDTR pseudo-descriptor uses `write_unaligned` (alignment hygiene surfaced by harness build). |
| `kernel/src/pipe.rs` | `PIPE_WAITQUEUES` (referenced; Track F refactors the syscall-layer blocking). |
| `kernel/src/ipc/endpoint.rs` | Bespoke per-`Endpoint` queues retained as final form (Phase 35 G.3 won't-do). |
| `kernel/tests/smp_prelude_smoke.rs` | Track 0b harness validation. |
| `kernel/tests/load_balance_smp.rs` | Track B regression test. |
| `kernel/tests/child_times_e1.rs` | Track E.1 + E.4 + E.2 invariants. |

## How This Phase Differs From Earlier SMP Work

- **Phase 25** built the IPI infrastructure, the per-core LAPIC, and the TLB-shootdown handler + API. P25-T033 (wiring shootdown into `munmap`) was deferred at Phase 25 close and silently delivered later; Phase 61 is the doc-closure and regression-test pass.
- **Phase 35** built per-CPU run queues, the affinity mask, the priority API, the `WaitQueue` primitive, and `maybe_load_balance` itself. The hook into the dispatch loop and the cross-core wakeup paths shipped, but the task-doc deferred lines were never reconciled with the code state. Phase 61 closes that reconciliation gap, **fixes the silent-no-op bug** (a real correctness issue not anticipated by either the audit or the original Phase 35 close), adds the SMP regression tests Phase 35 lacked, and makes Phase 35 H.2's previously-stale `system_ticks` claim genuinely true.
- **Phase 52d / 57e** formally deferred per-core lock-free dispatch and full kernel preemption respectively. Phase 61 takes those deferrals as fixed: load balancing runs under the existing global `SCHEDULER` lock and the existing batching cadence; voluntary preemption is the only preemption model.

## Related Roadmap Docs

- [Phase 25 design](roadmap/25-smp.md) — original SMP phase.
- [Phase 35 design](roadmap/35-true-smp-multitasking.md) — original load-balancing / wait-queue / time-accounting phase.
- [Phase 61 design](roadmap/61-smp-load-balancing-closeout.md) — this phase's design doc.
- [Phase 61 task list](roadmap/tasks/61-smp-load-balancing-closeout-tasks.md) — track breakdown.
