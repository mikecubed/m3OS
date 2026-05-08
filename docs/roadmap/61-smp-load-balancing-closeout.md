# Phase 61 — Phase 35 SMP Load Balancing Closeout

**Status:** Planned
**Source Ref:** phase-61
**Depends on:** Phase 25 (SMP) ✅, Phase 35 (True SMP Multitasking) ✅, Phase 52d (Kernel Completion and Roadmap Alignment) ✅, Phase 57e (Full Kernel Preemption — Deferred 2026-05-07) ✅
**Builds on:** Closes every Phase 25 and Phase 35 deferred item that has a tractable fix. The substantive SMP code (`maybe_load_balance()`, the per-CPU run queues, the object-attached pipe and IPC wait queues, and the Phase 25 `tlb_shootdown_range` wiring into `sys_linux_munmap`) already shipped between Phase 35 / Phase 25 close and the 2026-05-08 audit; the audit's "Red Flag #3" framing read those phases' task-doc deferred lines as live and reported the items as missing. The real gaps are: missing SMP regression tests; a polling `yield_now()` loop in the syscall-layer pipe blocking path that should be a `WaitQueue.sleep()` blocking call (Phase 35 G.2 lines 251–252); a stale Phase 35 H.2 acceptance line that claims `system_ticks` increases during syscalls when in fact `accumulate_ticks` attributes all elapsed time to `user_ticks`; child user/system tick accumulation in `sys_times` (`tms_cutime` / `tms_cstime` are still hard-coded to zero); the absence of `sys_wait4` and `sys_getrusage` syscalls (round out the CPU-time-accounting story Phase 35 H.3 began); and the doc reconciliation for Phase 25 / Phase 35.
**Primary Components:** `kernel/src/task/scheduler.rs` (`maybe_load_balance` at 4536, BSP dispatch hook at 3837, `accumulate_ticks` at 1291), `kernel/src/smp/mod.rs` (`CoreData::with_run_queue` at 392, `CoreData::run_queue` at 214), `kernel/src/smp/tlb.rs` (`tlb_shootdown_range`), `kernel/src/arch/x86_64/syscall/mod.rs` (`sys_linux_munmap` at 8831 calling `tlb_shootdown_range` at 8981; `sys_times` at 3703; `sys_waitpid` at 4485; `FdBackend::PipeRead` arm at 15131; new `sys_wait4` and `sys_getrusage`), `kernel/src/arch/x86_64/interrupts.rs` (timer IRQ handler — adds per-tick CS-based user/system sampling), `kernel/src/pipe.rs` (`PIPE_WAITQUEUES`, `wake_pipe`), `kernel/src/ipc/endpoint.rs` (`Endpoint::senders`, `Endpoint::receivers`), `kernel/src/task/mod.rs` (`Task::user_ticks`, `system_ticks` at 468 / 470; new `child_user_ticks`, `child_system_ticks`).

## Milestone Goal

Phase 25 P25-T033 closed (TLB shootdown wired into `munmap`, with a cross-core stale-TLB regression test). Phase 35 E.1, E.2, all three G.2 lines (251–253), and H.3's children-stub line all flipped to `[x]` with citations to the actual code lines that satisfy each one. All three Phase 35 G.3 lines flipped as `[x]` "won't-do" with the design-decision rationale recorded (bespoke payload-carrying per-`Endpoint` queues retained as final form). Phase 35 H.2's stale `system_ticks` claim made genuinely true via per-tick CS-based ring sampling in the timer IRQ handler. Nine new QEMU SMP tests live under `kernel/tests/`: `load_balance_smp.rs`, `munmap_tlb_smp.rs`, `pipe_wakeup_smp.rs`, `ipc_wakeup_smp.rs`, `pipe_blocking_no_busy_wait.rs`, `system_ticks.rs`, `wait4_rusage.rs`, `sys_times_children.rs`, `rusage_counters.rs`. Direct `read(pipe_fd)` / `write(pipe_fd)` block on `PIPE_WAITQUEUES` instead of polling `yield_now()`. `sys_times`, `sys_wait4`, and `sys_getrusage` together expose the full per-task and per-children CPU-time accounting POSIX expects. A 10-minute `-smp 2` soak with the documented workload runs without panic. Kernel version bumped to `0.61.0`.

## Why This Phase Exists

The 2026-05-08 audit raised three Red Flags against Phase 35 ("`maybe_load_balance()` commented out"; pipe and IPC wait queues "may be per-CPU"; child times "stubbed at zero") and one against Phase 25 (P25-T033, TLB shootdown not wired into `munmap`). Re-reading the code against those flags shows that three of the four are stale at the source-of-truth level:

- `maybe_load_balance()` is called from the BSP dispatch loop at `kernel/src/task/scheduler.rs:3837`, fires every 50 ticks, reads queue lengths via `with_run_queue(|q| q.len())`, applies a `> shortest_len + 2` threshold, and migrates one task per cycle while honouring affinity and a per-task migration cooldown.
- `tlb_shootdown_range` is called from `sys_linux_munmap` at `syscall/mod.rs:8981`, batched over the entire unmapped range after the per-page unmap loop completes.
- The pipe wait-queue lives in `PIPE_WAITQUEUES` (a `Vec<Option<WaitQueue>>` indexed by `pipe_id`, `kernel/src/pipe.rs:32`); the IPC wait-queue lives on each `Endpoint` as `senders` / `receivers` `VecDeque`s (`kernel/src/ipc/endpoint.rs:231-233`). Both are object-attached, not per-CPU, so cross-core wakeup works.

What is genuinely missing or broken:

- **Test coverage** of the three SMP guarantees above (load balancing actually distributes; munmap shootdown actually invalidates remote TLBs; cross-core wakeup actually wakes).
- **Pipe blocking discipline** (Phase 35 G.2 lines 251–252): the syscall-layer pipe path uses `loop { pipe_read(...); yield_now(); }` instead of `WaitQueue.sleep()`. The wake mechanism exists (`wake_pipe`) but direct `read`/`write` consumers don't register on it — only `poll`/`select`/`epoll` do. Result: blocked pipe readers wake only on next scheduler dispatch (~10 ms at 100 Hz), not when the writer writes; and they burn dispatch slots while waiting.
- **`system_ticks` always-zero** (Phase 35 H.2 stale `[x]`): `accumulate_ticks` at `scheduler.rs:1291–1295` adds all elapsed time to `user_ticks`. The acceptance line `[x] system_ticks increases during syscall handling` is currently false. The simplest fix: at every timer IRQ, inspect the saved frame's CS register and increment `user_ticks` (ring 3) or `system_ticks` (ring 0) by 1 — Linux's `CONFIG_TICK_CPU_ACCOUNTING` model.
- **Children CPU times** (Phase 35 H.3 line 306): `sys_times` writes `0_i64` for `tms_cutime` / `tms_cstime`. Real `times(2)` users (shell timing, build systems) need accurate children data.
- **`sys_wait4` and `sys_getrusage` absent**: once child user/system ticks accumulate, `sys_wait4` is `sys_waitpid` plus an extra `rusage` write and `sys_getrusage` is a one-syscall reader. Both round out the POSIX CPU-time-accounting story Phase 35 H.3 began.

This phase reframes Phase 35 G.3 (IPC `WaitQueue` swap) as won't-do rather than deferred. The bespoke per-`Endpoint` `senders` / `receivers` `VecDeque`s carry payload (`PendingSend { task: TaskId, msg: Message, wants_reply: bool }`) and atomically integrate with `scheduler::deliver_message_and_wake` and `scheduler::wake_task_v2`. Generic `WaitQueue<TaskId>` carries no payload; replacing the bespoke design would require either making `WaitQueue` payload-generic or splitting message storage from blocking — added complexity for no SMP correctness or performance gain over a correct, idiomatic design.

## Learning Goals

- How to read a task-doc "deferred" line against the actual code state and recognise when the doc lags reality.
- How `maybe_load_balance` reads queue lengths atomically without a separate counter, by exploiting the fact that `VecDeque::len()` on a per-core mutex-guarded run queue is O(1).
- How the existing TLB-shootdown IPI path (`tlb_shootdown_range` + per-core `handle_tlb_shootdown_ipi`) is already integrated with `munmap`'s batched unmap loop.
- Why the pipe sleep/wake path needs `WaitQueue.sleep()` rather than `yield_now()` polling, and how the existing `STDIN_WAITQUEUE` direct-read pattern at `syscall/mod.rs:5243` provides the reference implementation.
- Why the bespoke per-`Endpoint` queues are kept as the final form and what generic `WaitQueue<TaskId>` would have lost.
- How per-tick CS-based user/system tick sampling works and why it is preferred over syscall-entry/exit cycle-accurate instrumentation at this stage.
- How POSIX `tms_cutime` / `tms_cstime` accumulation works recursively: a reaped grandchild's time is already part of its parent's `child_*` fields by the time the parent itself becomes a zombie.
- How `sys_wait4` and `sys_getrusage` map onto the underlying `user_ticks` / `system_ticks` / `child_*` fields once those fields are accurate.

## Feature Scope

### Track A — Run-Queue-Length API + Load-Balance Hook Verification (doc-only)

Verify that `maybe_load_balance` is uncommented, runs from the BSP every 50 ticks, and reads queue lengths via `with_run_queue(|q| q.len())`. Add a doc comment at `kernel/src/task/scheduler.rs` near `maybe_load_balance` recording the design decision to read length directly from the `VecDeque` rather than maintain a parallel `AtomicU32` counter (the form Phase 35 E.1 originally planned). No code change.

### Track B — SMP Load-Balance Correctness Test

Write `kernel/tests/load_balance_smp.rs`. Boot `-smp 2`, spawn 8 CPU-bound tasks all initially assigned to core 0 so the run queues start fully imbalanced, yield for at least 60 ticks, then read `with_run_queue(|q| q.len())` for both cores and assert `|len(core0) - len(core1)| <= BALANCE_THRESHOLD + 1`. Extract the magic `2` at `scheduler.rs:4565` as `pub(crate) const BALANCE_THRESHOLD: usize = 2;`.

### Track C — `sys_linux_munmap` TLB-Shootdown Verification + Cross-Core Stale-TLB Test

Verify that `sys_linux_munmap` already calls `tlb_shootdown_range` and add a one-line cross-reference comment at the call site naming Phase 25 P25-T033. Then write `kernel/tests/munmap_tlb_smp.rs`: map a page on core 0, write a sentinel, force a TLB load on core 1, `munmap` on core 0, and assert that the next access on core 1 page-faults rather than reading the stale sentinel.

### Track D — Pipe + IPC Wait-Queue Cross-Core Wakeup Tests

Write `kernel/tests/pipe_wakeup_smp.rs` and `kernel/tests/ipc_wakeup_smp.rs`. The pipe test pins a reader to core 0, a writer to core 1, blocks the reader on an empty pipe, and asserts the reader wakes within 10 ticks of the writer's write (tightened from an initial `≤ 100 ticks` once Track F lands). The IPC test does the symmetrical thing for `sys_ipc_recv` / `sys_ipc_send` against an `Endpoint`.

### Track E — Time Accounting Closeout

**E.1 (children):** Add `child_user_ticks` / `child_system_ticks` to `Task`. At the zombie-reap site in `sys_waitpid`, accumulate the zombie's `user_ticks + child_user_ticks` and `system_ticks + child_system_ticks` into the parent's `child_*` fields (recursive accumulation rule). Replace the two `0_i64` writes in `sys_times` with the parent's `child_*` reads.

**E.2 (user/system split):** At every timer IRQ, inspect the saved frame's CS register and increment the running task's `user_ticks` (ring 3) or `system_ticks` (ring 0) by 1. Rewrite `accumulate_ticks` to stop attributing all elapsed time to `user_ticks` — per-tick sampling now owns the split. This makes Phase 35 H.2's previously-stale `[x] system_ticks increases during syscall handling` genuinely true.

**E.3 (`sys_wait4` + `sys_getrusage`):** Add both syscalls. `sys_wait4` is `sys_waitpid` plus a 144-byte `struct rusage` write with `ru_utime` / `ru_stime` in microseconds and the four E.4 counters; `sys_getrusage` reads the calling task's own counters for `RUSAGE_SELF` and the `child_*` accumulators for `RUSAGE_CHILDREN`.

**E.4 (page-fault and context-switch counters):** Add four counter fields and four `child_*` accumulators to `Task`: `minor_faults`, `major_faults`, `voluntary_ctxsw`, `involuntary_ctxsw`. Increment minor on CoW resolution at `interrupts.rs:987`; increment major on disk-backed page-in (if/when that path resolves; otherwise zero in practice). Increment `voluntary_ctxsw` in `yield_now`; increment `involuntary_ctxsw` in the timer-IRQ-driven preempt path before the switch. Recursively accumulate at the zombie-reap site, identical to E.1's pattern. These four are the most-used `rusage` fields after the time fields and are each a single-line increment in an existing kernel path; the remaining 10 `rusage` fields stay deferred (see Deferred Until Later).

### Track F — Pipe Blocking Sleep/Wake via `PIPE_WAITQUEUES`

Replace the polling `yield_now()` loop in the `FdBackend::PipeRead` arm of `sys_read` and the `FdBackend::PipeWrite` arm of `sys_write` with the `WaitQueue.register / sleep / deregister` pattern that the stdin path at `syscall/mod.rs:5243` already uses. Direct `read(pipe_fd)` blocked on an empty pipe will then wake when the writer calls `wake_pipe` rather than on next scheduler dispatch. Closes Phase 35 G.2 lines 251–252.

### Track G — Regression and Soak

`cargo xtask test` (full suite, including all new tests from B / C / D / E / F) plus a 10-minute `-smp 2` soak with the documented workload (4 CPU-bound tasks, 1 pipe ping-pong pair exercising Track F's blocking path, `sshd`, `display_server`). Soak result captured in `docs/handoffs/61g-smp-soak.md`.

### Track H — Phase 35 + Phase 25 Doc Closeout

Flip the nine Phase 35 deferred lines that Tracks A–F close (E.1 line 189, E.2 line 198, G.2 lines 251–253, G.3 lines 260–262, H.3 line 306). Add Phase 35 H.2 task-header post-text recording the previously-stale `[x]` is now genuinely satisfied. Update Phase 25 task doc Track Layout row for Track E (drop the "(handler+API; munmap hook deferred)" caveat) and add a Phase 61 closure note under P25-T033. Add one-line Phase 61 closure notes to the Phase 25 and Phase 35 design doc section headings for Load Balancing, Wait Queues, Time Accounting, and TLB Shootdown.

### Track I — Documentation and Release

Aligned legacy learning doc at `docs/61-smp-load-balancing-closeout.md` per the doc-templates appendix. Kernel version bump from 0.60.0 → 0.61.0 (`kernel/Cargo.toml`, `Cargo.lock`, `AGENTS.md`). Phase 61 row in `docs/roadmap/README.md` reflects the rescoped phase.

## Important Components and How They Work

### `maybe_load_balance()` and the BSP dispatch hook

`kernel/src/task/scheduler.rs:4536`. Called every 50 ticks from `kernel/src/task/scheduler.rs:3837` (BSP only). Reads each core's queue length with `data.with_run_queue(|q| q.len())`, short-circuits on `longest_len <= shortest_len + 2`, otherwise acquires `SCHEDULER` (lock ordering: `SCHEDULER` before `run_queue`), scans for a migratable task (affinity-compatible, not pinned by `fork_ctx`, past `MIGRATE_COOLDOWN`), updates `assigned_core` and `last_migrated_tick`, and `enqueue_to_core(shortest_core, idx)`.

### `tlb_shootdown_range` and `sys_linux_munmap`

`sys_linux_munmap` at `syscall/mod.rs:8831` runs the per-page unmap loop with the local TLB flush deferred (`flush.ignore()` line 8947) so the entire unmapped range can be batched into a single shootdown call at 8981. Each receiving core's `handle_tlb_shootdown_ipi` invalidates per-page or via a full `cr3` reload depending on range size (`smp/tlb.rs::INVLPG_THRESHOLD`).

### Object-attached wait queues, and the pipe-blocking gap

`PIPE_WAITQUEUES: IrqSafeMutex<Vec<Option<WaitQueue>>>` at `pipe.rs:32` is indexed by `pipe_id` — one `WaitQueue` per pipe. `wake_pipe(pipe_id)` is called from `pipe_read`/`pipe_write` after every successful operation. **However**, the syscall-layer blocking path (`syscall/mod.rs:15151–15171` for `sys_read` of a `PipeRead` FD) is `loop { pipe_read(...); yield_now(); }` — it does not register on `PIPE_WAITQUEUES` and does not call `WaitQueue.sleep()`. So the wait-queue is correctly object-attached for `poll(2)`/`select(2)` consumers (which use `fd_register_waiter` at `syscall/mod.rs:15681`) but is bypassed by direct `read`/`write`. Track F closes this gap by applying the same `register / sleep / deregister` pattern that the stdin direct-read path at `syscall/mod.rs:5243` already uses.

`Endpoint::senders: VecDeque<PendingSend>` and `Endpoint::receivers: VecDeque<TaskId>` at `endpoint.rs:231–233` are payload-carrying — `PendingSend` includes the `Message` and `wants_reply` flag. The `recv_msg` path at `endpoint.rs:308–414` enqueues into `receivers`, and the matching `sys_ipc_send` atomically delivers + wakes via `scheduler::deliver_message_and_wake`. This is functionally a wait queue with payload; replacing it with generic `WaitQueue<TaskId>` would require splitting message storage from blocking. Phase 61 keeps the bespoke design and reframes Phase 35 G.3 as won't-do.

### Per-task tick fields and the per-tick CS sampling rule

`Task` at `task/mod.rs:468–470` already has `user_ticks` and `system_ticks`. Phase 61 adds `child_user_ticks` and `child_system_ticks`. The current `accumulate_ticks` at `scheduler.rs:1291–1295` adds all elapsed time to `user_ticks`. Track E.2 replaces this with per-tick sampling: at every timer IRQ, the handler inspects the saved frame's CS register — if low 2 bits indicate ring 3, increment `user_ticks` by 1; otherwise increment `system_ticks`. Skip idle and blocked tasks. POSIX `times(2)` recursive children rule is implemented at the zombie-reap site in `sys_waitpid`: parent absorbs child's own + child's `child_*` fields.

### `sys_wait4` and `sys_getrusage`

Linux syscalls 61 and 98. `sys_wait4(pid, status_ptr, options, rusage_ptr)` is `sys_waitpid` plus a 144-byte `struct rusage` write with `ru_utime` / `ru_stime` populated from the reaped child's accumulated ticks (in microseconds: `ticks * 10_000` at 100 Hz) and `ru_minflt` / `ru_majflt` / `ru_nvcsw` / `ru_nivcsw` populated from Track E.4's counters. `sys_getrusage(who, usage_ptr)` reads the same six fields for `RUSAGE_SELF` (0) from the calling task's own counters, and from the `child_*` accumulators for `RUSAGE_CHILDREN` (-1); `RUSAGE_THREAD` (1) is treated as `RUSAGE_SELF` (one task per process). The remaining 10 `rusage` fields are zeroed; populating them requires more invasive instrumentation (memory-residency tracking, block-I/O counters, signal-delivery counter) and is post-1.0.

## How This Builds on Earlier Phases

- Phase 25 built the IPI infrastructure, the per-core LAPIC, and the TLB-shootdown handler + API. P25-T033 (wiring shootdown into `munmap`) was deferred at Phase 25 close and silently delivered later; Phase 61 is the doc-closure and regression-test pass.
- Phase 35 built per-CPU run queues, the affinity mask, the priority API, the `WaitQueue` primitive, and `maybe_load_balance` itself. The hook into the dispatch loop and the cross-core wakeup paths shipped, but the task-doc deferred lines were never reconciled with the code state. Phase 61 closes that reconciliation gap, adds the SMP regression tests Phase 35 lacked, fixes the pipe direct-read polling bug, and makes Phase 35 H.2's previously-stale `system_ticks` claim genuinely true.
- Phase 52d formally deferred per-core lock-free dispatch (removing the global `SCHEDULER` lock from the dispatch hot path). Phase 57e revisited the same surface and confirmed the deferral. Phase 61 takes that deferral as fixed: load balancing runs under the existing global lock and the existing batching cadence.

## Implementation Outline

For Tracks B, C, D, E, and F follow a TDD discipline: write the failing test first, then either confirm the existing code already satisfies it (Tracks B, C, D) or add the minimum new code that makes it pass (Tracks E, F). Run `cargo xtask test --test <name>` after each new test as the QEMU smoke gate.

1. Track A: read `maybe_load_balance` and `with_run_queue` against the audit claim; add the doc comment that captures the run-queue length read decision. No code change.
2. Track B: write `kernel/tests/load_balance_smp.rs`; export `BALANCE_THRESHOLD`; run.
3. Track C: write `kernel/tests/munmap_tlb_smp.rs`; add the cross-reference comment at `syscall/mod.rs:8981` naming Phase 25 P25-T033.
4. Track D: write `kernel/tests/pipe_wakeup_smp.rs` and `kernel/tests/ipc_wakeup_smp.rs`. Initial pipe-test latency assertion is `≤ 100 ticks`; Track F tightens it.
5. Track E: in order — E.1 children, E.2 per-tick split, E.3 syscalls. Each step's tests gate the next.
6. Track F: refactor pipe `sys_read` / `sys_write` arms to use `register / sleep / deregister`; tighten Track D.1 latency assertion to `≤ 10 ticks`; add `kernel/tests/pipe_blocking_no_busy_wait.rs` using Track E.2's tick counters to assert no scheduler dispatch of the blocked reader.
7. Track G: full regression + 10-minute soak; capture in `docs/handoffs/61g-smp-soak.md`.
8. Track H: flip the nine Phase 35 deferred lines; add the Phase 35 H.2 post-text note; update Phase 25 task-doc Track Layout row and add the P25-T033 closure note; add the Phase 25 / 35 design-doc section-heading notes.
9. Track I: aligned legacy learning doc; version bump 0.60.0 → 0.61.0.

## Acceptance Criteria

- `kernel/src/task/scheduler.rs` has a doc comment at or above `maybe_load_balance` recording the BSP-dispatch-hook callsite, the threshold, and the deliberate run-queue-length read approach.
- `BALANCE_THRESHOLD: usize = 2` extracted as a named constant referenced from both `maybe_load_balance` and `kernel/tests/load_balance_smp.rs`.
- Nine new QEMU SMP tests under `kernel/tests/` pass: `load_balance_smp.rs`, `munmap_tlb_smp.rs`, `pipe_wakeup_smp.rs`, `ipc_wakeup_smp.rs`, `pipe_blocking_no_busy_wait.rs`, `system_ticks.rs`, `wait4_rusage.rs`, `sys_times_children.rs`, `rusage_counters.rs`.
- A new kernel-core host test for the recursive child-time accumulation rule passes.
- `sys_times` returns nonzero `tms_utime`, nonzero `tms_stime` (when the task spent time in syscalls), and nonzero `tms_cutime` (when its forked child ran a CPU-bound loop and was reaped).
- `sys_wait4` and `sys_getrusage` syscalls are present, dispatched from the Linux syscall table, and write the documented `rusage` fields.
- `read(pipe_fd)` and `write(pipe_fd)` block on `PIPE_WAITQUEUES` (no `yield_now()` polling) and wake within 10 ticks of the producer/consumer side acting.
- Phase 25 task doc Track Layout row for Track E reads `**Done**` (no caveat); a Phase 61 closure note appears under P25-T033; the stale "CPU affinity" bullet in Phase 25's "Deferred Until Later" list (task doc line 193, design doc line 142) is struck-through with a "shipped in Phase 35 F.2" annotation.
- Phase 35 task doc lines 189, 198, 251, 252, 253, 260, 261, 262, and 306 are flipped to `[x]` with the citation text from Track H.1; H.2 task header carries the documented Phase 61 closure post-text.
- Phase 25 design doc TLB Shootdown section, Phase 35 design doc Load Balancing / Wait Queues / Time Accounting sections each carry a one-line Phase 61 closure note. Phase 35 design doc "Kernel preemption" deferred bullet at line 205 is annotated with the 57b/57d shipped + 57e re-deferred history without flipping (full kernel preemption remains genuinely deferred).
- `docs/61-smp-load-balancing-closeout.md` exists and follows the aligned legacy learning doc template.
- `kernel/Cargo.toml`, `Cargo.lock`, and `AGENTS.md` reflect kernel `0.61.0`. `docs/roadmap/README.md`'s Phase 61 row Primary Outcome column matches the rescoped phase.
- `cargo xtask test` passes with no regression. `cargo xtask check` is clean.
- 10-minute SMP soak documented in `docs/handoffs/61g-smp-soak.md` shows zero panics, zero `WARN`/`ERROR` lines.

## Companion Task List

- [Phase 61 Task List](./tasks/61-smp-load-balancing-closeout-tasks.md)

## How Real OS Implementations Differ

- Linux's load balancer (`load_balance()` in `kernel/sched/fair.c`) runs in response to NOHZ idle ticks, uses scheduler domains for NUMA-aware balancing, tracks CPU capacity, and integrates with the `EAS` energy-aware path. m3OS's `maybe_load_balance` is a deliberate simplification.
- Linux's TLB shootdown coalesces range flushes across multiple `munmap` calls using `mmu_gather`. m3OS's `tlb_shootdown_range` already batches the per-`munmap` range but does not coalesce across calls.
- Linux's CPU-time accounting offers three modes: per-tick sampling (`CONFIG_TICK_CPU_ACCOUNTING`), virtual-time (`CONFIG_VIRT_CPU_ACCOUNTING_NATIVE`, syscall-boundary instrumentation), and gen-counter (`CONFIG_VIRT_CPU_ACCOUNTING_GEN`, ns-precise). Phase 61 ships the per-tick sampling mode.
- Linux's `rusage` carries 16 fields including I/O statistics, page-fault counts, signal counts, and voluntary/involuntary context switches. Phase 61 populates only `ru_utime` / `ru_stime`.
- Linux's pipe blocking uses per-pipe wait queues with `add_wait_queue` / `prepare_to_wait` / `schedule()`. Phase 61's pipe blocking uses the equivalent `WaitQueue.register / sleep / deregister` pattern that m3OS's stdin path already uses.

## Why These Are In Scope and Others Are Not

The user-facing rule for Phase 61: every Phase 25 / Phase 35 deferred item is in scope unless there is an articulable reason to keep it deferred. The following items were each held against that bar:

**In scope (work pulled in):**

- *Phase 35 G.2 lines 251–252 (pipe `WaitQueue` integration)*: the gap is real — `read(pipe_fd)` polls `yield_now()` instead of sleeping. Fix is bounded (one `register / sleep / deregister` pattern, copied from the stdin direct-read path). Track F.
- *Phase 35 H.2 stale `[x] system_ticks increases`*: false today (`accumulate_ticks` attributes everything to `user_ticks`). Per-tick CS sampling at the timer IRQ is a small, contained change. Track E.2.
- *Phase 35 H.3 line 306 (children)*: `tms_cutime`/`tms_cstime` hard-coded to zero. Track E.1.
- *POSIX `sys_wait4` + `sys_getrusage`*: not strictly a Phase 35 deferral, but trivial once Tracks E.1 and E.2 land; rounds out the CPU-time-accounting story. Track E.3.
- *Four small `rusage` counters — `ru_minflt` / `ru_majflt` / `ru_nvcsw` / `ru_nivcsw`*: each is a single-line increment in an existing kernel path (`page_fault_handler`, `yield_now`, the timer-preempt path). With `sys_getrusage` (E.3) shipping the syscall surface, populating only `ru_utime` / `ru_stime` and zeroing the rest leaves a half-step gap. Track E.4.
- *Phase 25 "Deferred Until Later" stale entry — CPU affinity (`sched_setaffinity`)*: shipped in Phase 35 F.2. Doc-only one-line edits in `docs/roadmap/25-smp.md` line 142 and `docs/roadmap/tasks/25-smp-tasks.md` line 193 close the stale deferral. Track H.3 / H.4.

**Reframed as won't-do (explicitly closed, not deferred):**

- *Phase 35 G.3 lines 260–262 (IPC `WaitQueue` swap)*: the bespoke per-`Endpoint` `senders` / `receivers` `VecDeque`s are payload-carrying (`PendingSend { task, msg, wants_reply }`) and atomically integrate with `scheduler::deliver_message_and_wake`. Replacing them with generic `WaitQueue<TaskId>` would require splitting message storage from blocking via a side table — added complexity for no SMP correctness or performance gain over a correct, idiomatic design. Flip the three deferred lines as `[x]` with this rationale rather than leaving them open.

**Held for a later phase (with rationale):**

- *True per-core lock-free dispatch (remove global `SCHEDULER` lock from dispatch hot path)*: large refactor of the most-tested kernel hot path. Cross-core coordination, migration paths, and lock-order discipline all need redesign. Phase 52d and Phase 57e both deferred it; appropriate as its own phase, not a closeout. Out of scope.
- *Cycle-precise CPU-time accounting via syscall-entry/exit instrumentation*: per-tick sampling delivers enough precision for `times(2)` and `getrusage(2)` users at this stage. Cycle-precise accounting is a separate scheduler-instrumentation work item.
- *NUMA-aware load balancing*: prerequisite infrastructure (ACPI SRAT parsing, per-NUMA-node frame allocator) does not exist. Out of scope until the NUMA topology phase lands.
- *Scheduler domain hierarchy (cache / SMT / NUMA grouping for balancing decisions)*: complexity not justified at the 2-core QEMU baseline. Post-1.0.
- *Coalesced TLB-shootdown across multiple `munmap` calls (`mmu_gather` equivalent)*: optimisation, not correctness. m3OS already batches per-`munmap`. Cross-`munmap` coalescing requires deferred-flush semantics that complicate the correctness reasoning. Post-1.0.
- *Populating the remaining 10 `rusage` fields (memory residency `ru_maxrss` / `ru_ixrss` / `ru_idrss` / `ru_isrss`; deprecated `ru_nswap` / `ru_msgsnd` / `ru_msgrcv`; block I/O `ru_inblock` / `ru_oublock`; signal count `ru_nsignals`)*: page-fault and ctxsw counters are pulled in via Track E.4; the rest require more invasive instrumentation (memory-residency tracking, block-I/O counters in storage syscalls, signal-delivery counter in the signal path) and are explicitly post-1.0 work — collectively a separate "POSIX accounting completion" track.

## Deferred Until Later

- True per-core lock-free dispatch (rationale: see "Why These Are In Scope and Others Are Not" above).
- Cycle-precise CPU-time accounting via syscall-entry/exit instrumentation.
- NUMA-aware load balancing.
- Scheduler domain hierarchy.
- Coalesced TLB-shootdown range flushing across multiple `munmap` calls (`mmu_gather` equivalent).
- Populating the remaining 10 `rusage` fields beyond the four counters delivered in Track E.4 (memory residency, deprecated SysV/swap counters, block I/O, signal count).
