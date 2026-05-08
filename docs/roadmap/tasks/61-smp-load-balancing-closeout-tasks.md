# Phase 61 — Phase 35 SMP Load Balancing Closeout: Task List

**Status:** Planned
**Source Ref:** phase-61
**Depends on:** Phase 25 (SMP) ✅, Phase 35 (True SMP Multitasking) ✅, Phase 52d (Kernel Completion and Roadmap Alignment) ✅, Phase 57e (Full Kernel Preemption — Deferred 2026-05-07) ✅
**Goal:** Close every Phase 35 and Phase 25 deferred item that has a tractable fix, plus one stale Phase 35 acceptance line that is currently false. The substantive SMP code (`maybe_load_balance()`, `tlb_shootdown_range`-from-`sys_linux_munmap`, the object-attached `PIPE_WAITQUEUES` and per-`Endpoint` queues) already shipped between Phase 35 close and the 2026-05-08 audit; the gaps are missing SMP regression tests, four genuinely stale or unimplemented deferred items, and the Phase 25 / Phase 35 doc reconciliation. Phase 61 does **not** attempt a per-core lock-free dispatch refactor, NUMA awareness, scheduler domains, or `mmu_gather`-style coalesced shootdown — see the Deferred Until Later list in the design doc for the rationale on each.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Run-queue-length API + load-balance hook verification (doc-only) | — | Planned |
| B | SMP load-balance correctness test | A | Planned |
| C | `sys_linux_munmap` TLB-shootdown verification + cross-core stale-TLB test | — | Planned |
| D | Pipe + IPC wait-queue cross-core wakeup tests | — | Planned |
| E | Time accounting closeout (children, user/system split, `wait4` + `getrusage`, page-fault + ctxsw counters) | — | Planned |
| F | Pipe blocking sleep/wake via `PIPE_WAITQUEUES` (close Phase 35 G.2 lines 251–252) | D | Planned |
| G | Regression + 10-minute SMP soak | A B C D E F | Planned |
| H | Phase 35 + Phase 25 doc closeout | G | Planned |
| I | Documentation and Release | G H | Planned |

---

## Track A — Run-Queue-Length API + Load-Balance Hook Verification

### A.1 — Verify `maybe_load_balance()` is uncommented and reads queue lengths via `with_run_queue`

**Files:**
- `kernel/src/task/scheduler.rs`
- `kernel/src/smp/mod.rs`

**Symbols:** `maybe_load_balance` (`task/scheduler.rs:4536`); BSP dispatch hook (`task/scheduler.rs:3837`); `CoreData::with_run_queue` (`smp/mod.rs:392`); `CoreData::run_queue` (`smp/mod.rs:214`)

**Why it matters:** The 2026-05-08 audit Red Flag #3 ("`maybe_load_balance()` commented out") was based on a state that no longer holds: the hook is called from the BSP dispatch loop every 50 ticks, the function reads each core's queue length via `with_run_queue(|q| q.len())` against `spin::Mutex<VecDeque<usize>>`, applies a hard-coded `> shortest_len + 2` threshold, respects affinity masks, and updates `last_migrated_tick` for cooldown. There is nothing to uncomment and nothing to add — this track records that the audit finding is stale, captures the design decision to derive lengths from the `VecDeque` directly instead of a separate `AtomicU32` counter (the form Phase 35 E.1 originally planned), and writes a doc comment in `kernel/src/task/scheduler.rs` immediately above `maybe_load_balance` that future readers will see when they look for the missing counter.

**Acceptance:**
- [ ] A doc comment immediately above `maybe_load_balance` (or above `BALANCE_COUNTER`) records: hook callsite (BSP dispatch loop, every 50 ticks), threshold (`> shortest_len + 2`), and the deliberate choice to read length via `run_queue.lock().len()` rather than a parallel `AtomicU32` counter.
- [ ] No code change to `maybe_load_balance` or `with_run_queue` is required.
- [ ] `cargo xtask check` passes.

---

## Track B — SMP Load-Balance Correctness Test

### B.1 — QEMU SMP test: spawn imbalanced workload, observe migration

**File:** `kernel/tests/load_balance_smp.rs` (new)

**Symbol:** `test_load_balance_distributes_tasks` (new)

**Why it matters:** No automated test currently exercises `maybe_load_balance`. Phase 35 I.1 claims load-distribution under `-smp 4`, but it does so by visual inspection only. A targeted test that pins all spawned tasks to one core, waits for the load balancer to run, and asserts redistribution converts the implicit guarantee into a regression-protected one.

**Acceptance:**
- [ ] Test boots with `-smp 2`, spawns 8 CPU-bound kernel tasks all assigned to core 0 at spawn time, then yields for at least 60 ticks (3× the `BALANCE_COUNTER` interval of 50 plus margin).
- [ ] After yielding, the test reads `crate::smp::get_core_data(id).with_run_queue(|q| q.len())` for both cores and asserts `|len(core0) - len(core1)| <= BALANCE_THRESHOLD + 1` where `BALANCE_THRESHOLD = 2`. Extract `2` as `pub(crate) const BALANCE_THRESHOLD: usize = 2;` in `task::scheduler` (it is currently a magic number on line 4565) and reference it from both the function and the test.
- [ ] Test exits via the standard QEMU ISA-debug-exit success code (`0x10`).
- [ ] `cargo xtask test --test load_balance_smp` passes.
- [ ] If the load balancer's cooldown (`MIGRATE_COOLDOWN`) prevents migration within the test window, the test prints the cooldown value in its setup log so a future failure is diagnosable.

---

## Track C — `sys_linux_munmap` TLB-Shootdown Verification + Cross-Core Stale-TLB Test

### C.1 — Verify `tlb_shootdown_range` is wired into `sys_linux_munmap`

**File:** `kernel/src/arch/x86_64/syscall/mod.rs`

**Symbol:** `sys_linux_munmap` (line 8831); shootdown call at line 8981 (`crate::smp::tlb::tlb_shootdown_range(addr_space, range_start, range_end)`)

**Why it matters:** Phase 25's P25-T033 (and the Track-E table-row label "**Done** (handler+API; munmap hook deferred)") were both stale at the 2026-05-08 audit. `sys_linux_munmap` already calls `tlb_shootdown_range` after the per-page unmap loop with the full `[range_start, range_end)` span. This task records that the wiring is present and identifies the exact line for citation in Track H.

**Acceptance:**
- [ ] No code change. A one-line cross-reference comment is added at the `tlb_shootdown_range` call site naming Phase 25 P25-T033 closure (so future readers can grep).
- [ ] `cargo xtask check` passes.

### C.2 — QEMU SMP test: stale-TLB after `munmap` on remote core

**File:** `kernel/tests/munmap_tlb_smp.rs` (new)

**Symbol:** `test_munmap_no_stale_tlb_on_other_core` (new)

**Why it matters:** Phase 25 P25-T045 ("a TLB shootdown triggered by `munmap` does not leave stale mappings on another core") was an acceptance line never implemented as an automated test. Phase 61 closes that gap.

**Acceptance:**
- [ ] Test boots with `-smp 2`, spawns task A on core 0 and task B on core 1 sharing one address space. A maps a page, writes a sentinel byte, ensures B reads the byte (forcing a TLB load on core 1), then `munmap`s the page. After `munmap` returns on A, B reads the address; the test asserts a page-fault (or POSIX SIGSEGV equivalent) is raised, not the stale sentinel byte.
- [ ] Test exits via QEMU ISA-debug-exit success.
- [ ] `cargo xtask test --test munmap_tlb_smp` passes.

---

## Track D — Pipe + IPC Wait-Queue Cross-Core Wakeup Tests

### D.1 — Cross-core pipe wakeup test

**File:** `kernel/tests/pipe_wakeup_smp.rs` (new)

**Symbol:** `test_pipe_wakeup_across_cores` (new)

**Citations of code under test:** `kernel/src/pipe.rs` — `PIPE_WAITQUEUES: IrqSafeMutex<Vec<Option<WaitQueue>>>` (line 32), `wake_pipe(pipe_id)` (line 35); the syscall-layer pipe blocking path is wired in Track F.

**Why it matters:** Phase 35 G.2's third deferred line ("cross-core pipe wake behavior will be validated after that replacement lands") is the SMP-correctness aspect of the pipe wait-queue. The wait-queue is already object-attached: indexed by `pipe_id`, so a producer on any core wakes a consumer on any core via `wake_pipe → wq.wake_all()`. Track F replaces the polling `yield_now()` loop in the syscall-layer pipe path with a proper `WaitQueue.sleep()` blocking call; this test validates that the resulting cross-core wake works.

**Acceptance:**
- [ ] Test boots with `-smp 2`, spawns reader task pinned to core 0 and writer task pinned to core 1, has the reader block on an empty pipe, then has the writer write 1 byte. After Track F lands, the reader must wake within **10 ticks** (≈100 ms at 100 Hz) — a tighter latency than the current polling implementation could achieve. Initial implementation may relax to `≤ 100 ticks` until Track F lands; tighten to `≤ 10 ticks` as part of Track F's acceptance.
- [ ] Test exits via QEMU ISA-debug-exit success.
- [ ] `cargo xtask test --test pipe_wakeup_smp` passes.

### D.2 — Cross-core IPC wakeup test

**File:** `kernel/tests/ipc_wakeup_smp.rs` (new)

**Symbol:** `test_ipc_wakeup_across_cores` (new)

**Citations of code under test:** `kernel/src/ipc/endpoint.rs` — `Endpoint::senders: VecDeque<PendingSend>` and `Endpoint::receivers: VecDeque<TaskId>` (lines 231–233). These per-`Endpoint` queues are already object-attached, not per-CPU; any sender on any core wakes the appropriate receiver via the `ENDPOINTS` registry and `scheduler::deliver_message_and_wake` / `scheduler::wake_task_v2`.

**Why it matters:** Symmetrical to D.1 for the IPC path. The Phase 35 G.3 deferred lines are reframed in Track H as won't-do (see H.1 for rationale) — the bespoke per-endpoint queues carry payload (`PendingSend { task, msg, wants_reply }`) and atomically integrate with `deliver_message_and_wake`; replacing them with a generic `WaitQueue<TaskId>` would split message storage from blocking for no functional gain. This test proves the bespoke implementation is cross-core correct.

**Acceptance:**
- [ ] Test boots with `-smp 2`, spawns server task pinned to core 1 blocked in `sys_ipc_recv`, then spawns client task pinned to core 0 that calls `sys_ipc_send`. The server must wake within 10 ticks and receive the message label and body unchanged.
- [ ] Test exits via QEMU ISA-debug-exit success.
- [ ] `cargo xtask test --test ipc_wakeup_smp` passes.

---

## Track E — Time Accounting Closeout

### E.1 — Accumulate user/system ticks from exited children into the parent

**Files:**
- `kernel/src/task/mod.rs` — `Task` struct (already has `user_ticks: u64` at line 468 and `system_ticks: u64` at line 470; add two new fields)
- `kernel/src/arch/x86_64/syscall/mod.rs` — `sys_waitpid` (line 4485), `sys_times` (line 3703)

**Symbols:** new `Task::child_user_ticks: u64`, `Task::child_system_ticks: u64`; reaping path in `sys_waitpid` that today drops the zombie's accounting; `sys_times` lines 3709–3710 that hard-code `tms_cutime` / `tms_cstime` to zero.

**Why it matters:** Phase 35 H.3's third acceptance line at `docs/roadmap/tasks/35-true-smp-multitasking-tasks.md:306` is `[ ] Deferred — child tms_cutime / tms_cstime accumulation is still stubbed as zero in the current implementation`. Real `times(2)` users (shell timing, build systems) require accurate children data.

**Acceptance:**
- [ ] `Task` struct gains `child_user_ticks: u64` and `child_system_ticks: u64` (initialized to zero at `kernel/src/task/mod.rs:659`).
- [ ] At the zombie-reap point in `sys_waitpid` (the `state == Zombie` branch around `kernel/src/arch/x86_64/syscall/mod.rs:4554`), the parent's `child_user_ticks` and `child_system_ticks` are increased by the zombie's `user_ticks + child_user_ticks` and `system_ticks + child_system_ticks` respectively (POSIX requires recursive child-of-child accumulation).
- [ ] `sys_times` reads the current task's `child_user_ticks` / `child_system_ticks` and writes them as `tms_cutime` / `tms_cstime` instead of zero (replaces the two `0_i64` writes at lines 3709–3710).
- [ ] A new kernel-core host test exercises the accumulation logic on a synthetic task tree (parent → child → grandchild), verifying the recursive accumulation rule.
- [ ] A new QEMU test (`kernel/tests/sys_times_children.rs`) forks a child that runs a CPU-bound loop for ~50 ticks, then calls `sys_waitpid`, then calls `sys_times`, and asserts `tms_cutime > 0`.
- [ ] `cargo xtask test` passes.

### E.2 — Per-tick CS-based user/system tick split

**Files:**
- `kernel/src/arch/x86_64/interrupts.rs` — timer IRQ handler
- `kernel/src/task/scheduler.rs` — `accumulate_ticks` (line 1291) and the comment at lines 1288–1290

**Symbols:** new `crate::task::scheduler::tick_account_current_task(saved_cs: u16)` (or equivalent); modification of `accumulate_ticks` to stop attributing all elapsed time to `user_ticks`; addition of the per-tick sample call in the timer IRQ handler.

**Why it matters:** Phase 35 H.2's acceptance line `[x] system_ticks increases during syscall handling` at `docs/roadmap/tasks/35-true-smp-multitasking-tasks.md` is **false today**. The scheduler comment at `kernel/src/task/scheduler.rs:1288–1290` documents this: `Currently all ticks are attributed to user_ticks. Splitting ticks into user vs system (ring 3 vs ring 0) requires tracking the syscall-entry boundary and is deferred to a future phase.` The simplest fix uses per-tick sampling rather than syscall-entry instrumentation: at every timer IRQ, inspect the saved frame's `CS` register — if ring 3 the task was in user mode, if ring 0 it was in kernel mode — and increment the appropriate counter by 1 (matching Linux's `CONFIG_TICK_CPU_ACCOUNTING` model).

**Acceptance:**
- [ ] Timer IRQ handler calls `tick_account_current_task(saved_cs)` once per tick (after the EOI is sent and before any rescheduling decision), passing the saved frame's CS register.
- [ ] `tick_account_current_task` skips the idle task and any task in the `Blocked*` family; for the running task on the current core, it inspects `saved_cs`: if the low 2 bits indicate ring 3, increment `task.user_ticks` by 1; otherwise increment `task.system_ticks` by 1.
- [ ] `accumulate_ticks` (`kernel/src/task/scheduler.rs:1291`) is rewritten so that it no longer adds all elapsed time to `user_ticks` — per-tick sampling now owns the user / system split. The function may be removed entirely if no remaining caller depends on the prior elapsed-on-switch semantics; otherwise it becomes a no-op or is retained only for `start_tick` bookkeeping.
- [ ] The doc comment at lines 1288–1290 is updated to describe the new per-tick sampling mechanism and the expected granularity (one tick = 10 ms at 100 Hz).
- [ ] A QEMU test (`kernel/tests/system_ticks.rs`) spawns a task that spends most of its time in syscalls (e.g. a `getpid` loop) for ~100 ticks, then calls `sys_times`, and asserts `tms_stime > 0`. A second task that spends most of its time in a tight user-mode loop asserts `tms_utime > 0` and `tms_stime` near zero.
- [ ] `cargo xtask test` passes.

### E.3 — Add `sys_wait4` and `sys_getrusage` syscalls

**Files:**
- `kernel/src/arch/x86_64/syscall/mod.rs` — new `sys_wait4` (Linux syscall 61) and `sys_getrusage` (Linux syscall 98) entries; syscall dispatch table.

**Symbols:** new `pub(super) fn sys_wait4(pid: u64, status_ptr: u64, options: u64, rusage_ptr: u64) -> u64`; new `pub(super) fn sys_getrusage(who: i32, usage_ptr: u64) -> u64`; existing `sys_waitpid` for shared logic.

**Why it matters:** Tracks E.1, E.2, and E.4 land all the per-task and per-children CPU-time and event-count data required to populate POSIX `struct rusage`. Once that data exists, `sys_wait4` is `sys_waitpid` plus an extra `rusage` write, and `sys_getrusage(RUSAGE_SELF / RUSAGE_CHILDREN)` is a one-syscall reader. Adding both rounds out the CPU-time-accounting story Phase 35 H.3 began without leaving the half-step gap of "we have the data but no syscall surface to expose it." Userspace may not call these immediately; that is acceptable — the ABI surface is small, the test is kernel-side, and shipping POSIX-named syscalls now avoids re-litigation in a later phase.

**Acceptance:**
- [ ] `sys_wait4` reuses the `sys_waitpid` core logic for the wait/zombie scan and exit-code transfer. After a successful reap, if `rusage_ptr != 0`, it writes a 144-byte (Linux x86_64 layout) `struct rusage` with `ru_utime` / `ru_stime` populated from the reaped child's `user_ticks + child_user_ticks` / `system_ticks + child_system_ticks` (in microseconds, computed as `ticks * 10_000`); `ru_minflt` / `ru_majflt` / `ru_nvcsw` / `ru_nivcsw` populated from Track E.4's counters (child's own + child's children-accumulated counters); all other 10 `rusage` fields are zeroed.
- [ ] `sys_getrusage`:
  - `who == RUSAGE_SELF` (0): writes `ru_utime` / `ru_stime` / `ru_minflt` / `ru_majflt` / `ru_nvcsw` / `ru_nivcsw` from the calling task's own counters; other fields zeroed.
  - `who == RUSAGE_CHILDREN` (-1): writes `ru_utime` / `ru_stime` / `ru_minflt` / `ru_majflt` / `ru_nvcsw` / `ru_nivcsw` from the calling task's `child_*` counters; other fields zeroed.
  - `who == RUSAGE_THREAD` (1): same as `RUSAGE_SELF` for m3OS (one task per process at the moment).
  - Any other `who` returns `-EINVAL`.
- [ ] Syscall numbers added to the Linux syscall dispatch table at the appropriate location in `kernel/src/arch/x86_64/syscall/mod.rs` (61 = `wait4`, 98 = `getrusage`).
- [ ] A QEMU test (`kernel/tests/wait4_rusage.rs`) forks a CPU-bound child, calls `sys_wait4`, and asserts the reaped `rusage.ru_utime` is in microseconds and matches `tms_cutime * 10_000` (within tolerance), and that `ru_minflt + ru_majflt > 0` (the child took at least one page fault) and `ru_nvcsw + ru_nivcsw > 0` (the child was scheduled out at least once).
- [ ] `cargo xtask test` passes.

### E.4 — Per-task page-fault and context-switch counters for `rusage`

**Files:**
- `kernel/src/task/mod.rs` — `Task` struct; add four counter fields and four matching `child_*` accumulator fields.
- `kernel/src/arch/x86_64/interrupts.rs` — `page_fault_handler` at line 966; the CoW-resolution branch at line 987.
- `kernel/src/task/scheduler.rs` — `yield_now` at line 2257; `pick_next` (or the equivalent dispatch entry); the timer-IRQ-driven preempt path.
- `kernel/src/arch/x86_64/syscall/mod.rs` — `sys_waitpid` zombie-reap site (line 4554) for the recursive accumulation rule.

**Symbols:** new `Task::minor_faults: u64`, `Task::major_faults: u64`, `Task::voluntary_ctxsw: u64`, `Task::involuntary_ctxsw: u64`; new `Task::child_minor_faults`, `Task::child_major_faults`, `Task::child_voluntary_ctxsw`, `Task::child_involuntary_ctxsw`; new helper `crate::task::scheduler::current_task_record_page_fault(major: bool)`; new helper `crate::task::scheduler::current_task_record_ctxsw(voluntary: bool)`.

**Why it matters:** Track E.3 adds `sys_wait4` and `sys_getrusage` syscalls. POSIX `struct rusage` carries 16 fields; without E.4, only `ru_utime` and `ru_stime` are nonzero. The four counters in this task (page faults — minor and major; context switches — voluntary and involuntary) are the most-used `rusage` fields after the time fields and are each a single-line increment in an existing kernel path. Adding them now closes the half-step gap where userspace-facing `getrusage` returns mostly zeros. The remaining 10 `rusage` fields (memory residency, deprecated SysV/swap counters, block I/O, signal counts) require more invasive instrumentation and are explicitly post-1.0 work — see the design doc's "Deferred Until Later" section.

**Acceptance:**
- [ ] `Task` struct gains the eight new fields (four own counters, four child accumulators), each `u64`, all initialized to zero in the `Task::new` / equivalent constructor at `kernel/src/task/mod.rs:659`.
- [ ] `page_fault_handler` calls `current_task_record_page_fault(false)` after a successful CoW resolution at `interrupts.rs:987` (minor — no disk I/O); for non-CoW page faults that are nonetheless successfully handled (e.g., demand-page-from-mmap-file backing if/when that path resolves), use `current_task_record_page_fault(true)`. If a page fault path that triggers disk I/O does not currently exist, only the minor counter is wired in this phase and the major counter remains zero in practice — document this in the helper's doc comment.
- [ ] `yield_now` (voluntary path) calls `current_task_record_ctxsw(true)` immediately before invoking the dispatch.
- [ ] The timer-IRQ-driven preempt path that calls into the scheduler (the call site that ultimately leads to `pick_next` from a timer interrupt — typically in the timer handler in `kernel/src/arch/x86_64/interrupts.rs`) calls `current_task_record_ctxsw(false)` for the outgoing task before the switch (involuntary).
- [ ] At the zombie-reap site in `sys_waitpid` (line 4554, alongside the Track E.1 child-time accumulation), the parent's four `child_*` counter fields are increased by the zombie's own counter + zombie's `child_*` counter (recursive accumulation rule, identical to E.1's pattern).
- [ ] The kernel-core host test from E.1 is extended to cover the four counters' recursive accumulation on a synthetic task tree.
- [ ] A QEMU test (`kernel/tests/rusage_counters.rs`) spawns a task that triggers ≥10 page faults (e.g., by writing into a CoW-shared page after fork) and yields ≥10 times, then calls `sys_getrusage(RUSAGE_SELF)`, and asserts `ru_minflt >= 10` and `ru_nvcsw >= 10`.
- [ ] `cargo xtask test` passes.

---

## Track F — Pipe Blocking Sleep/Wake via `PIPE_WAITQUEUES`

### F.1 — Replace polling `yield_now()` loop in pipe `sys_read` / `sys_write` with `WaitQueue` blocking

**File:** `kernel/src/arch/x86_64/syscall/mod.rs`

**Symbols:** the `FdBackend::PipeRead` arm of `sys_read` (line 15131) and the `FdBackend::PipeWrite` arm of `sys_write` (around line 5966 and the equivalent block in `sys_linux_write`).

**Why it matters:** Phase 35 G.2's first two deferred lines at `docs/roadmap/tasks/35-true-smp-multitasking-tasks.md:251–252` are accurate: the syscall-layer pipe blocking path is currently a `loop { pipe_read(...); yield_now(); }` polling loop, NOT a `WaitQueue.sleep()` blocking call. The wait queue (`PIPE_WAITQUEUES[pipe_id]`) exists and is woken on every read/write via `wake_pipe`, but only `poll(2)`/`select(2)`/`epoll(2)` consumers register on it via `fd_register_waiter` (`syscall/mod.rs:15681`). A direct `read(pipe_fd)` blocked on an empty pipe wakes only when the scheduler dispatches it next, which can be milliseconds later, and burns scheduler dispatch slots while it waits. The fix follows the existing pattern at `syscall/mod.rs:5243` (which `read(stdin_fd)` already uses with `STDIN_WAITQUEUE.register / sleep / deregister`).

**Acceptance:**
- [ ] In the `FdBackend::PipeRead` arm of `sys_read`, replace the `yield_now()` polling loop (line 15151–15171) with: `register on PIPE_WAITQUEUES[pipe_id] → call pipe_read → if would-block: WaitQueue.sleep() → on wake, recheck → deregister on success / signal / EOF`. Use the same `Arc<AtomicBool>` `woken` flag pattern that the stdin path at line 5243 uses.
- [ ] In the `FdBackend::PipeWrite` arm of `sys_write` (and `sys_linux_write` if separate), apply the same pattern: register, attempt write, if `Err(true)` (would block) then sleep, on wake recheck. EPIPE (`Err(false)`) returns immediately without registering.
- [ ] Signal handling (`has_pending_signal()` returns `EINTR`) is preserved — checked before sleeping and on every wake.
- [ ] After this lands, tighten Track D.1 pipe wakeup latency assertion from `≤100 ticks` to `≤10 ticks` and re-run.
- [ ] A new QEMU test (`kernel/tests/pipe_blocking_no_busy_wait.rs`) starts a reader on an empty pipe and observes that the reader is **not** dispatched by the scheduler (its `user_ticks` does not advance) until the writer writes. Use the per-task tick counter from Track E.2 to assert this.
- [ ] `cargo xtask test` passes.

---

## Track G — Regression and Soak

### G.1 — Full regression pass

**Files:** `xtask/src/main.rs` (test harness)

**Symbol:** `cargo xtask test`

**Why it matters:** Tracks B, C, D, E, F all add new tests; Tracks E and F change scheduler/syscall hot paths. A regression in any existing test indicates a correctness violation.

**Acceptance:**
- [ ] `cargo xtask test` passes with zero regressions, including all new tests from B / C / D / E / F.
- [ ] `cargo xtask check` (clippy `-D warnings` + rustfmt + kernel-core host tests) passes.
- [ ] No new `unsafe` block introduced without an adjacent `// SAFETY:` comment.

### G.2 — 10-minute SMP soak

**File:** `docs/handoffs/61g-smp-soak.md` (new log artifact)

**Symbol:** QEMU 2-core instance with a fixed CPU-bound + IPC + I/O workload

**Why it matters:** Tracks B / C / D / F each prove a specific SMP race surface in isolation. A soak combines them under sustained load to surface interactions the unit tests cannot reach.

**Workload (concrete):**
- 4 instances of a CPU-bound task spawned via the existing `kernel/tests/`-style harness or via the userspace `sh0` running a one-line loop (`while true; do :; done` per child).
- 2 instances of a pipe ping-pong loop (one writer, one reader; writer pins to core 0, reader pins to core 1) — exercises Track F's blocking sleep/wake path.
- `sshd` running on its default port (Phase 53 baseline).
- `display_server` started but with no clients connected (Phase 56 baseline).

**Acceptance:**
- [ ] `cargo xtask run` with `-smp 2`, the workload above, for 10 wall-clock minutes.
- [ ] Zero kernel panics, zero `WARN`/`ERROR` lines in the serial log.
- [ ] `docs/handoffs/61g-smp-soak.md` populated with: QEMU command line used, exact workload spawn commands, duration, the final `serial.log`, and a pass/fail verdict signed off by the running engineer.

---

## Track H — Phase 35 and Phase 25 Doc Closeout

### H.1 — Reconcile Phase 35 task doc

**File:** `docs/roadmap/tasks/35-true-smp-multitasking-tasks.md`

**Lines to flip (with required citations):**

1. **Line 189** — E.1 deferred line `[ ] Deferred — a standalone queue_length: AtomicU32 counter from the original plan has not been added`. Flip to `[x] Phase 61 closure: deliberate design — queue length is read via run_queue.lock().len() (kernel/src/smp/mod.rs:392 with_run_queue). A separate AtomicU32 is not added; the VecDeque length read is O(1) and avoids a second source of truth.`
2. **Line 198** — E.2 deferred line `[ ] Deferred — the scheduler loop currently leaves the maybe_load_balance() hook commented out pending the follow-up noted in code`. Flip to `[x] Phase 61 closure: hook is uncommented at kernel/src/task/scheduler.rs:3837 (BSP, every 50 ticks). SMP load-balance correctness test in kernel/tests/load_balance_smp.rs.`
3. **Line 251** — G.2 first deferred line `[ ] Deferred — pipe_read() and pipe_write() still use the older would-block return path rather than WaitQueue`. Flip to `[x] Phase 61 closure: sys_read / sys_write pipe paths now register on PIPE_WAITQUEUES and call WaitQueue.sleep() instead of yield-polling. See kernel/src/arch/x86_64/syscall/mod.rs FdBackend::PipeRead arm.`
4. **Line 252** — G.2 second deferred line `[ ] Deferred — pipe sleep/wake integration with WaitQueue remains future work`. Flip to `[x] Phase 61 closure: integration done in Track F; verified by kernel/tests/pipe_blocking_no_busy_wait.rs.`
5. **Line 253** — G.2 third deferred line `[ ] Deferred — cross-core pipe wake behavior will be validated after that replacement lands`. Flip to `[x] Phase 61 closure: cross-core pipe wakeup validated by kernel/tests/pipe_wakeup_smp.rs against the object-attached PIPE_WAITQUEUES (kernel/src/pipe.rs:32) and the new blocking-sleep path.`
6. **Line 260** — G.3 first deferred line `[ ] Deferred — IPC call()/reply_recv() still use endpoint-local sender/receiver queues plus scheduler block helpers`. Flip to `[x] Phase 61 closure (won't-do): bespoke per-Endpoint sender/receiver VecDeques are payload-carrying (PendingSend{task, msg, wants_reply}) and integrate atomically with deliver_message_and_wake (kernel/src/ipc/endpoint.rs recv_msg lines 308–414). Replacing them with generic WaitQueue<TaskId> would split message storage from blocking for no functional gain. Cross-core wakeup correctness verified by kernel/tests/ipc_wakeup_smp.rs.`
7. **Line 261** — G.3 second deferred line `[ ] Deferred — userspace IPC behavior is intentionally unchanged until the WaitQueue swap happens`. Flip to `[x] Phase 61 closure (won't-do): the WaitQueue swap is not done; the bespoke design is accepted as the final form. See line 260 closure note for rationale.`
8. **Line 262** — G.3 third deferred line `[ ] Deferred — endpoint-specific WaitQueue wiring remains future work`. Flip to `[x] Phase 61 closure (won't-do): endpoint-specific blocking is implemented via Endpoint::senders / receivers + scheduler block_state + wake_task_v2; this is the equivalent of WaitQueue wiring with payload, so no separate WaitQueue is needed.`
9. **Line 306** — H.3 third deferred line `[ ] Deferred — child tms_cutime / tms_cstime accumulation is still stubbed as zero in the current implementation`. Flip to `[x] Phase 61 closure: tms_cutime and tms_cstime populated via Task::child_user_ticks / child_system_ticks accumulated in sys_waitpid (kernel/src/arch/x86_64/syscall/mod.rs sys_waitpid + sys_times).`

**H.2 acceptance line correction:** the existing `[x]` at H.2 for "system_ticks increases during syscall handling" was stale. Add a Phase 61 closure note immediately under the H.2 task header: `Phase 61 closure: H.2's system_ticks acceptance was previously stale — accumulate_ticks attributed all elapsed time to user_ticks. Per-tick CS-based ring detection in the timer IRQ handler now correctly increments user_ticks vs system_ticks per tick. See kernel/src/arch/x86_64/interrupts.rs timer handler and kernel/src/task/scheduler.rs::tick_account_current_task.` Do not unflip the `[x]` — the acceptance is now genuinely satisfied.

**Acceptance:**
- [ ] All nine flips above land with the cited citation text.
- [ ] The H.2 closure note lands as task-header post-text.

### H.2 — Reconcile Phase 35 design doc

**File:** `docs/roadmap/35-true-smp-multitasking.md`

**Acceptance:**
- [ ] A one-line note added under the "Load Balancing" section heading (line 75): `Phase 61 closure: maybe_load_balance() hook uncommented and validated by kernel/tests/load_balance_smp.rs.`
- [ ] A one-line note added under the "Wait Queues" section heading (line 119): `Phase 61 closure: pipe sys_read / sys_write now block on PIPE_WAITQUEUES; cross-core wakeup verified for pipes and IPC endpoints. Endpoint queues retain their bespoke payload-carrying design (won't-do for generic WaitQueue swap).`
- [ ] A one-line note added under the "Time Accounting" section heading (line 138): `Phase 61 closure: per-tick CS-based user/system tick split now correctly attributes ring-3 vs ring-0 time; child tms_cutime / tms_cstime populated; sys_wait4 and sys_getrusage syscalls added.`

### H.3 — Reconcile Phase 25 task doc

**File:** `docs/roadmap/tasks/25-smp-tasks.md`

**Acceptance:**
- [ ] In the Track Layout table at line 63, change `| E | TLB shootdown | C, D | **Done** (handler+API; munmap hook deferred) |` to `| E | TLB shootdown | C, D | **Done** |`.
- [ ] Add a Phase 61 closure note immediately after the Track E table at line 153 (under P25-T033): `**Phase 61 closure (P25-T033 + P25-T045):** tlb_shootdown_range() is wired into sys_linux_munmap at kernel/src/arch/x86_64/syscall/mod.rs:8981 (post-batch shootdown over the full unmapped range). Cross-core stale-TLB regression test: kernel/tests/munmap_tlb_smp.rs.`
- [ ] In the "Deferred Until Later" list at line 188, replace the bullet `- CPU affinity (\`sched_setaffinity\`)` (line 193) with `- ~~CPU affinity (\`sched_setaffinity\`)~~ — shipped in Phase 35 F.2 (\`sys_sched_setaffinity\` / \`sys_sched_getaffinity\`)`. The strikethrough plus shipped-in-phase note preserves the historical record while making the current state clear.

### H.4 — Reconcile Phase 25 design doc

**File:** `docs/roadmap/25-smp.md`

**Acceptance:**
- [ ] A one-line note added under the "TLB Shootdown" section heading at line 88: `Phase 61 closure: shootdown wired into sys_linux_munmap; SMP regression test in kernel/tests/munmap_tlb_smp.rs.`
- [ ] In the "Deferred Until Later" list at line 139, replace `- CPU affinity (\`sched_setaffinity\`)` (line 142) with `- ~~CPU affinity (\`sched_setaffinity\`)~~ — shipped in Phase 35 F.2`.

### H.5 — Clarify Phase 35 design doc preemption deferred-line

**File:** `docs/roadmap/35-true-smp-multitasking.md`

**Acceptance:**
- [ ] In the "Deferred Until Later" list at line 199, replace the bullet `- Kernel preemption` (line 205) with `- Kernel preemption — voluntary kernel preemption shipped in Phases 57b/57d; full timer-driven kernel-mode preemption attempted in Phase 57e and re-deferred 2026-05-07 (see post-mortem at \`docs/post-mortems/2026-05-07-57e-preempt-full-deferred.md\`).` This clarifies the partial-shipment state without flipping the line — full kernel preemption remains deferred.

---

## Track I — Documentation and Release

### I.1 — Create the aligned legacy learning doc

**File:** `docs/61-smp-load-balancing-closeout.md`

**Symbol:** new file

**Why it matters:** The doc-template "aligned legacy learning doc" form gives a learner-friendly companion to the design + task docs. Every shipped phase has one (or has a deliberate exception). This file is created from the template in `docs/appendix/doc-templates.md` § "Template: aligned legacy learning doc".

**Acceptance:**
- [ ] `docs/61-smp-load-balancing-closeout.md` exists, follows the template (Aligned Roadmap Phase, Status, Source Ref, Supersedes Legacy Doc / new — all present).
- [ ] Overview paragraph names the actual phase outcome in plain language (audit closeout + targeted code fixes, not "implement load balancing from scratch").
- [ ] "What This Doc Covers" lists the verification, the new SMP tests (load-balance, munmap-TLB, pipe-wakeup, IPC-wakeup, blocking-no-busy-wait), the time-accounting fixes (children, user/system split, `wait4` + `getrusage`), and the pipe sleep/wake refactor.
- [ ] "Core Implementation" walks a learner through: the run-queue length read, the `maybe_load_balance` migration step, the `tlb_shootdown_range` call from `munmap`, the per-tick CS-based time accounting, the children-accumulation flow, and the pipe-blocking `WaitQueue` pattern.
- [ ] "Key Files" table cites: `kernel/src/task/scheduler.rs`, `kernel/src/smp/mod.rs`, `kernel/src/smp/tlb.rs`, `kernel/src/arch/x86_64/syscall/mod.rs`, `kernel/src/arch/x86_64/interrupts.rs`, `kernel/src/pipe.rs`, `kernel/src/ipc/endpoint.rs`, `kernel/src/task/mod.rs`.
- [ ] "How This Phase Differs From Earlier SMP Work" explains: Phase 25 built the IPI + per-core data; Phase 35 built per-CPU run queues and the load-balance algorithm; Phase 61 closes the audit-era gaps and adds the time-accounting + pipe-sleep work the prior phases deferred.
- [ ] "Related Roadmap Docs" links the design and task docs.

### I.2 — Bump kernel version to 0.61.0

**Files:**
- `kernel/Cargo.toml`
- `Cargo.lock`
- `AGENTS.md`
- `docs/roadmap/README.md` (Phase 61 row's Primary Outcome column)

**Symbol:** `version` field in `kernel/Cargo.toml` `[package]` section

**Why it matters:** Phase closure is signalled by a kernel version bump per project convention. The current baseline is `kernel/Cargo.toml = 0.60.0` and `AGENTS.md = "Kernel v0.60.0"`. Both move to `0.61.0`.

**Acceptance:**
- [ ] `kernel/Cargo.toml` `version = "0.61.0"`.
- [ ] `Cargo.lock` regenerated.
- [ ] `AGENTS.md` "Kernel v0.60.0" → "Kernel v0.61.0".
- [ ] `docs/roadmap/README.md` Phase 61 row's Primary Outcome column matches the rescoped phase. The row was rewritten when the docs landed; re-confirm at version-bump time that no later edit reverted it. Expected text: `Audit closeout for Phase 35 SMP load balancing and Phase 25 P25-T033 TLB-shootdown deferral. Verifies maybe_load_balance() + tlb_shootdown_range from sys_linux_munmap + object-attached pipe / IPC wait queues; replaces pipe yield-polling with WaitQueue blocking; adds per-tick user/system tick split, child tms_cutime / tms_cstime, sys_wait4 + sys_getrusage. Closes audit Red Flag #3 + Phase 25 P25-T033`.
- [ ] `cargo xtask check` passes after the bump.
- [ ] Git tag suggestion: `v0.61.0` (tag at phase merge, not at task-checkbox tick).

---

## Documentation Notes

- The global `SCHEDULER` lock is intentionally retained. The per-core lock-free dispatch refactor remains deferred per Phase 52d and Phase 57e and is out of scope for Phase 61 — the rationale is recorded in the design doc's "Deferred Until Later" section (large refactor of the most-tested kernel hot path; cross-core coordination and migration paths require careful redesign; appropriate as its own phase, not a closeout).
- The audit Red Flag #3 framing ("`maybe_load_balance()` commented out") is itself stale — the hook was uncommented between Phase 35's merge and the 2026-05-08 audit, but the Phase 35 task-doc deferred line at 198 was not updated. Track A captures this so future audits do not re-raise the same flag.
- Track H.1's reframe of Phase 35 G.3 as won't-do is the substantive design call of this phase: the bespoke per-`Endpoint` queues are payload-carrying and atomically integrate with the scheduler's block/wake machinery; replacing them with generic `WaitQueue<TaskId>` would require either making `WaitQueue` payload-generic or splitting message storage from blocking via a side table — added complexity for no SMP correctness or performance gain. The G.3 deferred lines are flipped as "closed, won't-do" rather than left deferred.
- Track E.2 uses per-tick CS-based sampling rather than syscall-entry/exit instrumentation. This matches Linux's `CONFIG_TICK_CPU_ACCOUNTING` model: simpler, lower-overhead, and accurate enough for `times(2)` and `getrusage(2)` users. Cycle-precise accounting via syscall-boundary instrumentation is post-1.0.
- Track F's pipe sleep/wake refactor follows the existing pattern at `syscall/mod.rs:5243` (stdin direct-read blocking via `STDIN_WAITQUEUE.register / sleep / deregister`). The same pattern is already used by `sys_poll` / `sys_select` / `sys_epoll_wait` for pipe FDs — Track F extends it to direct `read` / `write`.
- Test files under `kernel/tests/*.rs` follow the existing QEMU-isa-debug-exit convention (`0x10` success, `0x11` failure). Use the same harness pattern as the existing tests in that directory; the xtask harness picks them up via `cargo xtask test --test <name>`.
