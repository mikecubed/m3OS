# Phase 99 - SMP & Scheduler Robustness Hardening

**Status:** Complete (2026-06-28)
**Source Ref:** phase-99
**Depends on:** Phase 57a–e (v2 scheduler block/wake protocol + preemption) ✅, Phase 35 / Phase 25 (SMP boot + IPI / TLB shootdown) ✅, Phase 98 (Roadmap Audit & Re-Charter — this phase is chartered there) ✅
**Builds on:** **Hardens** the Phase 57a single-state-word block/wake path (`block_current_until` / `wake_task_v2`) — which already exists — rather than introducing it. It consolidates the per-site ad-hoc lost-wakeup patches that accreted in Phase 89 (`FUTEX_REQUEUE`/`FUTEX_CMP_REQUEUE`), Phase 90b (per-address-space futex keys + the cross-thread PKU read-recovery), the 2026-06-14 SMP/TLB/kstack hardening (Tracks A–D), and the Phase 95 rustc futex work under one audited model, finishes the **deferred** Track-D origin audit from the 2026-06-14 handoff, lands the panic-path AP-quiesce asked for in the 2026-06-05 4 GiB handoff, and root-causes the open 2026-06-25 demand-fault CI flake.
**Primary Components:** `kernel/src/task/scheduler.rs` (`block_current_until`, `wake_task_v2`, `pi_lock` / `Task::on_cpu` protocol, `scan_expired_wake_deadlines`, a new scheduler-state diagnostic), `kernel/src/arch/x86_64/interrupts.rs` (`page_fault_handler`, `double_fault_handler`, `general_protection_fault_handler`, `fault_kill_trampoline`, `FAULT_RECOVERY_STACKS`, `try_recover_kstack_overflow`, the `MAP_LAZY_FILE` demand-fault branch), `kernel/src/process/mod.rs` (`shared_vma_demand_file`), `kernel/src/lib.rs` (`handle_panic` AP quiesce), `kernel/src/arch/x86_64/syscall/{mod,fs}.rs` (`copy_file_range`/`sendfile` ENOSYS path), `kernel/src/net/remote.rs` + `kernel-core/src/driver_ipc/net.rs` (RX-path test encoder), `xtask/src/main.rs` (`cmd_smp_smoke` raised to `-smp 8`)

## Milestone Goal

m3OS becomes **provably reliable multi-core**. The recurring cross-core lost-wakeup bug class is retired — not patched a fifth time — by auditing every blocking call site against the single-state-word model and validating it at `-smp 8` (the real Dell Tiger Lake laptop is 8-core/16-thread and **cannot** pin `-smp 1` the way the Node/Go/Claude/rustc toolchain gates do today). The companion fault-handling cluster is closed: the kstack-overflow origin and the "lock held across a fault-recovery path" audit deferred from 2026-06-14, a readable 4 GiB SMP panic banner, the ~11–15 % step-25 demand-fault NULL-deref CI flake, and two long-tail correctness bugs (`fs.copyFile`→EFAULT and a 55c `net::remote` test that encodes its fixture with the wrong-direction header). This is the kernel foundation the entire bare-metal GUI arc (Phases 100–110) rests on.

## Why This Phase Exists

Lost-wakeups have recurred **four-plus times** despite the Phase 57a rewrite to a single state word: Phase 89's `FUTEX_REQUEUE`/`FUTEX_CMP_REQUEUE` (a silent no-op deadlocked libuv's threadpool condvar), Phase 90b's per-address-space futex keys + cross-thread PKU read-recovery, the 2026-06-14 cross-core lost-wake + TLB-shootdown panic, and the Phase 95 rustc parallel-linker futex work. Each was fixed with a **targeted, per-site patch** — a stack-local `AtomicBool` wrapper here, a requeue special case there, a bracketing comment around one `deliver_message + wake_task_v2` pair — so the *model* is correct (the scheduler-design-comparison handoff recommended exactly it, and 57a delivered it) but its *application* across every wait site is uneven and **unvalidated above `-smp 4`**. The always-on toolchain gates dodge the whole problem by pinning `-smp 1`; that escape hatch evaporates on an 8-core laptop where multi-core is mandatory.

On real silicon with >2 GiB RAM these races are **more** likely, not less: more cores means more concurrent `mprotect`/`pkey_mprotect`/CoW/demand-fault TLB shootdowns, a wider lost-wake window, and a larger panic blast radius. Three open handoffs (`2026-06-14`, `2026-06-05`, `2026-06-25`) all describe SMP-correlated faults whose residual work was explicitly deferred. Phase 98 chartered Phase 99 to fold them into one CI-able (QEMU SMP) hardening pass so the bare-metal GUI arc starts from a trustworthy multi-core kernel instead of a `-smp 1` crutch.

## Learning Goals

- Why a single state word + condition-recheck-after-state-write + CAS wake (Linux's `task->__state` / `try_to_wake_up` model, adopted in Phase 57a) eliminates the lost-wakeup class — and why *uniform* application across every wait site, not the primitive alone, is what actually retires the bug.
- How a lock-ordering invariant (`pi_lock` outer, `SCHEDULER.lock` inner) is kept correct across IPC, notifications, futexes, and deadline scans, and how to *audit* it at scale rather than patch it per incident.
- Why interrupt/exception handlers must never hold the scheduler or process-table lock across a fault-prone or blocking operation, and how the `fault_kill_trampoline` + per-core recovery-stack mechanism recovers a task-attributable kernel-stack overflow without wedging the core.
- How a multi-core panic path must **quiesce sibling cores** before printing, or the banner is interleaved into unreadable garbage — and why diagnosability is a first-class robustness feature.
- How a lazy file-backed demand-fault (`MAP_LAZY_FILE`) that issues a *blocking* `vfs_server` read from inside the page-fault handler is a uniquely fragile path, and why it gets exercised far harder on the slow bare-metal VFS.

## Feature Scope

### Track A — Blocking-Primitive Consolidation, Audit & SMP-8 Validation

The single-state-word v2 model already exists: `block_current_until` (`kernel/src/task/scheduler.rs`) follows the four-step Linux recipe (state write under `pi_lock` → release → condition recheck → yield via `SCHEDULER.lock`), `wake_task_v2` is the CAS-style wake, and the v1 flags (`switching_out`, `wake_after_switch`, `PENDING_SWITCH_OUT`) are **deleted**. Track A does **not** re-introduce it — it *completes and hardens* it:

- **Call-site conformance audit.** Enumerate every caller that blocks — the `BlockedOnReply` / `BlockedOnRecv` / `BlockedOnNotif` / `BlockedOnSend` wrappers, the futex waiter, and the deadline-scan path — and confirm each registers a fresh `woken` flag, rechecks its condition after the state write, and relies on no latched per-site flag that survives a block call. The ad-hoc bracketing comments ("Bug #6 brackets around `deliver_message + wake_task_v2`", the stack-local `AtomicBool` wrappers) become one documented, uniformly-applied pattern instead of N independent special cases.
- **Futex model conformance.** Confirm `FUTEX_REQUEUE`/`FUTEX_CMP_REQUEUE` and the per-address-space futex keys interact with the single-state-word model correctly (a requeued cond-waiter is woken by a CAS on its state, never by a flag the requeue must coordinate) — the exact shape Phase 89/90b patched per-incident.
- **Scheduler-state diagnostic.** Add the periodic/on-demand `[sched] tasks: pid=X state=Y wake_deadline=Z on_cpu=W` dump the 2026-04-25 handoff's recommendation #3 asked for — the missing tool that turns any future "task stuck in Blocked forever" symptom into direct evidence at the moment of hang.
- **SMP-8 validation.** Raise the always-on `smp-smoke` gate's default from `-smp 4` to `-smp 8` so the futex-heavy libuv-threadpool stress (`SMP_STRESS_OK 256`) runs at the laptop's core count, and any residual lost-wake trips the `BlockedOnFutex … no waker registered` watchdog `WaitPassOrFail` fail-fast.

### Track B — Fault-Handling Robustness Audit (kstack origin + locks-across-faults)

The 2026-06-14 handoff landed the *recovery* (Tracks A–D: NMI-on-IST, mark-core-offline-on-halt, degrade-shootdown-timeout, controlled-kill `fault_kill_trampoline`) but explicitly **deferred the origin audit**. Track B finishes it:

- **Locks-held-across-faults audit.** Confirm no kernel path holds `SCHEDULER`/`PROCESS_TABLE` across a fault-prone or blocking operation (the `2026-04-21-scheduler-lock-isr-deadlock` class). The `page_fault_handler`'s `MAP_LAZY_FILE` branch already documents taking `PROCESS_TABLE` *before* `shared_vma_demand_file` re-takes it and *not* stranding it across the blocking IPC — codify that as an asserted invariant, and sweep `general_protection_fault_handler` / `double_fault_handler` / `fault_kill_trampoline` for the same.
- **kstack-overflow origin.** Pin the exact kernel call chain that exhausts the 64 KiB per-task kernel stack under the V8/PKU multi-core churn (now capturable on the IST-backed clean frame), and either bound that path's depth or document why 64 KiB is sufficient with the recovery in place.
- **Recovery-stack correctness review.** Re-verify the per-core `FAULT_RECOVERY_STACKS` pool, the `IN_KERNEL_PAGE_FAULT` recursion-latch clear on the recovery path, and that `try_recover_kstack_overflow` only ever redirects a *userspace-task-attributable* overflow.

### Track C — 4 GiB SMP Panic-Path AP-Quiesce & Residual OOM/Race

The 2026-06-05 handoff's blocking ask is **diagnosability**: at 4 GiB + `--kvm` + SMP, an intermittent panic's banner is unreadable because `handle_panic` prints and dumps the trace rings while the other cores keep writing to COM1. Track C makes the panic path **quiesce sibling cores first** (broadcast a halt IPI/NMI, spin briefly for acknowledgement, then print on a quiet bus), so the banner is legible — the prerequisite for ever root-causing the residual 4 GiB-only OOM/race that lives behind it. With a readable banner, take one pass at the residual instability (its relationship to the TLB-shootdown saga in `2026-05-24-4gib-pci-hole-vga-mapping.md`).

### Track D — Step-25 Demand-Fault NULL-Deref Flake (`MAP_LAZY_FILE`)

The 2026-06-25 handoff documents an open ~11–15 % CI flake: the `dynlink-hello-versioned-mismatch-smoke` step (step 25) intermittently hits a kernel `#PF rip=… cr2=0x0 err=0x0` (a kernel-mode read of NULL) in the Phase 95b lazy file-backed demand-fault chain — `page_fault_handler` → `shared_vma_demand_file` → a blocking `vfs_server` read issued from inside the fault handler. It is host-correlated, low-rate, and does not reproduce locally; the symbolization infra (CI uploads the exact kernel ELF + serial log on smoke failure) is in place but a red run with that infra has not yet been captured. Track D captures it, symbolizes against the CI-built ELF, root-causes the NULL deref, and fixes it. **This path is load-bearing**: every toolchain install (rustc, node, clang) demand-pages multi-hundred-MB DSOs through it, and it is exercised far harder on the slower bare-metal VFS, so a 1-in-7 fault is unacceptable for the GUI arc.

### Track E — Two Correctness Bugs

- **`copy_file_range`/`sendfile` → EFAULT.** Node's `fs.copyFile` surfaces `EFAULT` instead of either copying or failing cleanly. Neither `copy_file_range` (326) nor `sendfile` (40) has a handler in `kernel/src/arch/x86_64/syscall/`, so both fall to the default dispatch arm returning `NEG_ENOSYS` — yet the user-visible symptom is EFAULT, so the bad errno originates downstream in libuv's copy path (the read/write fallback after the ENOSYS probe). Root-cause it and either implement a correct `copy_file_range`/`sendfile` or guarantee a clean `-ENOSYS` so node's userspace fallback succeeds — never a spurious EFAULT.
- **55c `net::remote` RX-test encoder.** A Phase-55c-era `net::remote` RX-path unit test encodes its fixture with the wrong-direction header (`encode_header_with_kind` / the egress `NET_SEND_FRAME` kind) rather than the ingress `encode_net_rx_notify` / `NET_RX_FRAME`, so the decode path rejects it as `InvalidFrame` and the test asserts against the wrong outcome. Re-point it at the ingress encoder so the test exercises the real RX decode path.

## Important Components and How They Work

### `kernel/src/task/scheduler.rs` — the existing v2 block/wake protocol (audited, not rewritten)

`block_current_until` writes `task.state ← Blocked*` and `task.wake_deadline` under `pi_lock`, releases it, rechecks the caller's condition (self-reverting `Blocked* → Running` if already satisfied), then yields via `SCHEDULER.lock`. `wake_task_v2` acquires `pi_lock`, CAS-transitions any `Blocked*` → `Ready` (a wake to a `Running`/`Ready` task is a silent `AlreadyAwake`), then enqueues under `SCHEDULER.lock`, spin-waiting on `Task::on_cpu` until the switch-out epilogue publishes `saved_rsp`. Track A audits that **every** wait wrapper drives this primitive identically and adds `dump_scheduler_state` (new) for hang forensics; it deletes no public symbol.

### `kernel/src/arch/x86_64/interrupts.rs` — fault handlers + recovery

`page_fault_handler`, `double_fault_handler`, and `general_protection_fault_handler` redirect a task-attributable kernel-stack overflow to `fault_kill_trampoline` on a per-core `FAULT_RECOVERY_STACKS` slot, gated by the `IN_KERNEL_PAGE_FAULT` recursion latch and `try_recover_kstack_overflow`. Track B audits the lock state on entry to these paths (no `SCHEDULER`/`PROCESS_TABLE` stranded across the blocking `MAP_LAZY_FILE` IPC or the trampoline) and pins the overflow origin; Track D root-causes the `cr2=0` NULL deref in the same handler's `MAP_LAZY_FILE` branch (`shared_vma_demand_file` in `kernel/src/process/mod.rs`).

### `kernel/src/lib.rs` — `handle_panic` AP quiesce

Today `handle_panic` prints the banner and dumps the trace rings while sibling cores keep running. Track C broadcasts a quiesce (halt IPI/NMI, leveraging `smp::try_per_core` / the `is_online` marking the 2026-06-14 work already added in `hlt_loop`) and waits a bounded grace window before printing, so the 4 GiB panic banner is readable rather than SMP-interleaved garbage.

### `xtask/src/main.rs` — `cmd_smp_smoke` at `-smp 8`

The `smp-smoke` gate boots multi-core (default `-smp 4` today via `M3OS_SMP`), `pkg install node`, and runs 256 async `pbkdf2` ops with 16 in flight, asserting `SMP_STRESS_OK 256`. Track A raises its default to `-smp 8` (still `M3OS_SMP`-overridable) so the futex WAIT/WAKE handshake is validated at the laptop's core count and the watchdog fail-fast guards the consolidated model.

## How This Builds on Earlier Phases

- **Extends Phase 57a–e** by *finishing* the single-state-word block/wake rewrite — auditing it for uniform conformance and validating it at `-smp 8` — rather than restarting it; `block_current_until`/`wake_task_v2`/`pi_lock`/`Task::on_cpu` are reused unchanged.
- **Consolidates the ad-hoc lost-wake patches** of Phase 89 (`FUTEX_REQUEUE`), Phase 90b (per-AS futex keys, cross-thread PKU read-recovery), and Phase 95 (rustc parallel-linker futex) under one audited pattern, replacing per-site special cases with a documented invariant.
- **Closes the deferred origin audit** of the 2026-06-14 SMP/TLB-shootdown/kstack handoff (Track D "still open") and the **diagnosability ask** of the 2026-06-05 4 GiB panic handoff, neither of which was a discrete phase before.
- **Hardens the Phase 95b `MAP_LAZY_FILE` demand-fault path** the entire toolchain arc depends on, by root-causing the open 2026-06-25 step-25 flake.
- **Is the gating prerequisite for Phase 100** (Bare-Metal GUI Session on the Dell): the laptop is 8-core and cannot fall back to `-smp 1`, so multi-core reliability must land first.

## Implementation Outline

1. **Track A** — audit every `block_current_until` caller for four-step conformance (write the audit into `docs/handoffs/` or `docs/appendix/`); collapse the per-site `AtomicBool` wrappers to one documented pattern; confirm futex `REQUEUE`/`CMP_REQUEUE` + per-AS-key conformance; add `dump_scheduler_state`; raise `cmd_smp_smoke` default to `-smp 8` and confirm `SMP_STRESS_OK 256`.
2. **Track B** — sweep `interrupts.rs` fault handlers for `SCHEDULER`/`PROCESS_TABLE` held across fault/blocking work; add a debug assertion enforcing it; pin the kstack-overflow origin from an IST-captured clean frame; review the `FAULT_RECOVERY_STACKS` + `IN_KERNEL_PAGE_FAULT` recovery contract.
3. **Track C** — add an AP-quiesce step to `handle_panic` (halt IPI/NMI broadcast + bounded ack spin) so the banner prints on a quiet COM1; capture one readable 4 GiB panic and take a pass at the residual OOM/race.
4. **Track D** — capture a red step-25 run with the in-place CI artifact infra, symbolize the `cr2=0` `#PF` against the CI ELF, root-cause + fix the NULL deref in the `MAP_LAZY_FILE` demand-fault chain, and prove the fix with an N≥50-iteration soak.
5. **Track E** — root-cause `fs.copyFile`→EFAULT and implement `copy_file_range`/`sendfile` or return a clean `-ENOSYS`; re-point the 55c `net::remote` RX test at `encode_net_rx_notify`.

## Acceptance Criteria

- `smp-smoke` PASSES at **`-smp 8`** (raised from the prior `-smp 4` default) with the futex-heavy libuv pbkdf2 stress completing — `SMP_STRESS_OK 256` printed, no `KERNEL PANIC`, no `BlockedOnFutex … no waker registered` watchdog verdict, no `process killed`.
- A blocking-call-site conformance audit exists (committed under `docs/`) enumerating every `block_current_until` caller and confirming each rechecks its condition after the state write with a fresh `woken` flag and no surviving latched per-site flag.
- A `dump_scheduler_state` diagnostic emits per-task `pid=… state=… wake_deadline=… on_cpu=…` lines (periodic and/or on the stuck-no-waker watchdog path).
- No `SCHEDULER`/`PROCESS_TABLE` lock is held across a fault-recovery or blocking path in `page_fault_handler` / `general_protection_fault_handler` / `double_fault_handler` / `fault_kill_trampoline` (audited + a debug assertion); the kstack-overflow originating call chain is pinned or its depth bounded.
- `kstack-overflow-smoke` (`M3OS_KSTACK_OVERFLOW_REGRESSION`) and `dynamic-hello-smoke` — including the `THREAD_FAULT` thread-group-kill arms (`leader-ok` / `worker-ok`) and `DYNAMIC_TLS:ok` — stay green.
- The step-25 `dynlink-hello-versioned-mismatch-smoke` flake drops to **0** over an N≥50-iteration soak (recorded run); the `cr2=0` kernel NULL deref in the `MAP_LAZY_FILE` demand-fault chain is identified (symbolized against the CI ELF) and fixed.
- The 4 GiB SMP panic banner is **readable** (sibling cores quiesced before printing) — demonstrated by a captured panic whose banner + crash diagnostics are uninterleaved.
- `node` `fs.copyFile` returns a working copy or the syscall(s) return a clean `-ENOSYS` (userspace fallback succeeds) — never a spurious EFAULT; verified by a small node `fs.copyFile` probe.
- The 55c `net::remote` RX-path test encodes its fixture with the ingress `NET_RX_FRAME` header (`encode_net_rx_notify`) and passes without tripping `InvalidFrame`; `cargo xtask check` host tests for `kernel-core` + `net::remote` pass.
- `cargo xtask check` + `smoke-test` + `regression` all green; no regression in the `claude-smoke` / `node-smoke` / `rustc-smoke` toolchain gates.

## Companion Task List

- [Phase 99 Task List](./tasks/99-smp-scheduler-robustness-tasks.md)

## How Real OS Implementations Differ

- **Linux** uses a single `task->__state` word with `set_current_state` (`smp_store_mb`) + `try_to_wake_up` (a state-match CAS under `p->pi_lock`), and never tracks "blockedness" in side flags — exactly the model Phase 57a adopted and Phase 99 audits to completion. It also runs **lockdep**, **KASAN**, and (in test kernels) **loom**-style model checking continuously; m3OS substitutes a focused call-site audit + the `smp-smoke` stress gate.
- Production kernels put `#PF`, `#DF`, `#NMI`, and `#MC` on dedicated IST stacks and broadcast a **panic stop** to all CPUs (`panic_smp_self_stop` / NMI shootdown) before printing; m3OS landed NMI-on-IST in 2026-06-14 and adds the panic AP-quiesce here, but keeps `#PF` off IST deliberately (it is the hot, returning demand-paging path and is incompatible with the `fault_kill_trampoline` RSP-capture).
- `copy_file_range`/`sendfile` in Linux are real VFS fast paths (server-side copy, reflink on btrfs/XFS, splice plumbing); m3OS treats them as optional accelerators behind a clean `ENOSYS` fallback rather than a correctness requirement.
- Mature schedulers (CFS/EEVDF) layer fairness, priority inheritance, and full kernel preemption on top of the wake primitive; m3OS keeps the simpler round-robin/work-stealing model and defers fair scheduling (see Phase 98's accepted-deferred backlog).

## Deferred Until Later

- **True per-core lock-free scheduling** (the dispatch hot path never acquiring a global `SCHEDULER.lock`) — a larger architectural change than this hardening pass; the per-task `pi_lock` already provides most of the contention relief.
- **CFS/EEVDF fair scheduling, priority-inheritance futexes, and full kernel preemption beyond Phase 57e** — Phase 98 accepted-deferred backlog (kernel concurrency maturity).
- **`lockdep` / `KASAN` / `loom`-style automated race detection** — substituted here by the `smp-smoke` stress gate + a manual call-site audit.
- **A genuine `copy_file_range`/`sendfile` fast path** (reflink / server-side copy / splice) — Phase 99 only guarantees a working copy *or* a clean `ENOSYS` fallback; the accelerated path is deferred.
- **The QEMU PS/2-mouse stick-at-top-left dev-path nuisance** (Phase 98 backlog) — likely real-HW-moot since the laptop pointer is I2C-HID (Phase 102), tracked separately.
- **Root-causing the underlying 4 GiB-only OOM/race itself** beyond what a now-readable panic banner reveals in one pass — Track C delivers diagnosability; a full fix may spill to a follow-up if the residual race proves deep.
