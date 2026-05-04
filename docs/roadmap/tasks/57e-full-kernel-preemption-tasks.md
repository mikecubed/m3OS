# Phase 57e — Full Kernel Preemption (PREEMPT_FULL): Task List

**Status:** Planned
**Source Ref:** phase-57e
**Depends on:** Phase 57b ✅, Phase 57c ✅, Phase 57d (functional ✅; gates I.2 and I.3 must close before 57e starts — see Track 0)
**Goal:** Drop the `from_user` check from 57d's IRQ-return preemption point.  Kernel-mode code becomes preemptible at any point where `preempt_count == 0`.  Per-trigger latency floors improve over the 57d baseline; cross-core IPI wakeup is the only path expected to drop into the microsecond range.  This is the **stretch goal** of the 57b/c/d/e programme — the realistic 1.0 release target is `PREEMPT_VOLUNTARY` parity at end of 57d.

In addition, this phase migrates kernel FPU state save/restore from `fxsave64`/`fxrstor64` to `xsave64`/`xrstor64` (Track J) so AVX YMM state survives context switches.  57e increases switch frequency under load; without xsave, hosted binaries that emit AVX (modern Rust/LLVM, musl ports, ion shell, audio_server pipeline) accumulate silent FP corruption.  The work is mechanical and folded into 57e because the same 24-hour soak validates both changes.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| 0 | Prelude — confirm 57d I.2 (post-flip 24-hour soak) and I.3 (flag removal) are closed | 57d I.2, I.3 | Planned |
| A | Audit (kernel preempt invariants — second pass over 57c catalogue) | 0, 57c ✅ | Planned |
| B | `preempt_disable` wrapping (verify done sites + wrap remaining sites + per-CPU audit) | A | Planned |
| C | `preempt_resume_to_kernel` + `dispatch_preempted_and_resume_kernel` (asm + Rust shims) | 0 | Planned |
| D | Dispatch reentrancy audit + held-lock watchdog | A, C | Planned |
| E | Latency benchmarks (per trigger path) | 0 | Planned |
| F | Activate kernel-mode preemption (kernel-handler body, immediate zero-crossing) | A–E, J | Planned |
| G | 24-hour soak | F | Planned |
| H | Default-on flip and feature-flag removal | G | Planned |
| J | XSAVE migration (FXSAVE → XSAVE, AVX YMM coverage; AVX-512 deferred) | 0 | Planned |

## Engineering Practice Gates (apply to every track)

- **TDD.**  Every implementation commit references a test commit landed earlier.  Latency benchmarks land before the headline change so the "before" baseline is captured.
- **SOLID.**  `preempt_resume_to_kernel` only restores kernel-mode tasks; `preempt_resume_to_user` only restores user-mode.  No code branches on ring inside a single routine.
- **DRY.**  `_user` and `_kernel` resume variants share **only the GPR-restore portion** via the existing `restore_gprs_all` macro at `kernel/src/arch/x86_64/interrupts.rs:605–621`.  The iretq frame layout (5-field privilege-changing for `_user`, 3-field same-CPL for `_kernel`) and the RSP handling (CPU-pushed `rsp` for `_user`, explicit `mov rsp, preempt_frame.rsp` for `_kernel`) are variant-specific.
- **Documented invariants.**  The `from_user` check is the *only* difference between 57d and 57e in the preemption decision; documented at the IRQ handler.  Every kernel busy-spin in 57c's catalogue is annotated with whether `preempt_disable` is required under `PREEMPT_FULL`.
- **Lock ordering.**  Unchanged from 57d.
- **Migration safety.**  Headline change gated on `cfg(feature = "preempt-full")`.  Default off until G validates; flip in H.  The Cargo feature declares `preempt-full = ["preempt-voluntary"]` so the `_kernel`-handler body change always layers on top of the user-mode preemption foundation.
- **Observability.**  57d `[TRACE] [preempt]` line gains `kernel_mode=true|false` field.  A new `[WARN] [preempt]` line is emitted by the held-lock watchdog (Track D.3) if a kernel-mode preemption is observed at the kernel-handler body but the chosen task is found to be holding a known scheduler-context lock at dispatch time.
- **FPU/XSAVE coverage.**  Track J replaces `fxsave64`/`fxrstor64` with `xsave64`/`xrstor64` so AVX YMM state survives a context switch.  The save/restore call sites are unchanged (already at the dispatch boundary in `kernel/src/task/scheduler.rs:1428–1447, 3644, 3727, 1979`); only the type, alignment, and asm instruction change.

---

## Track 0 — Prelude (57d gate inheritance)

### 0.1 — Confirm 57d post-flip soak (I.2) is green

**File:** procedural; reference `docs/handoffs/57d-validation-gate.md`
**Symbol:** —
**Why it matters:** 57e drops the `from_user` check that protects kernel-mode code under `PREEMPT_VOLUNTARY`.  If 57d's voluntary baseline has any latent preempt-discipline bug, stacking `PREEMPT_FULL` on top hides whether a soak failure is a 57d regression or a 57e bug.

**Acceptance:**
- [ ] 57d I.2 24-hour soak result documented and clean.
- [ ] 57d I.3 (`preempt-voluntary` feature flag removed; `git grep preempt-voluntary` returns zero results) merged.
- [ ] `cargo xtask check` and `cargo xtask test` pass on a fresh checkout of `main` post-57d-cleanup.

### 0.2 — Re-baseline 57d latency numbers

**File:** procedural; results in `docs/handoffs/57e-baseline-latency.md` (new)
**Symbol:** —
**Why it matters:** Track E's per-trigger benchmarks compare 57e numbers against 57d; the 57d numbers must come from the same QEMU/hardware configuration that 57e will run on, not from the 57d phase doc's historical figures.

**Acceptance:**
- [ ] Run E.1, E.2, E.3, E.4 benchmark stubs (Track E) against `main` post-57d-cleanup with `preempt-full` *not yet declared*.
- [ ] Record median, P95, P99 for each trigger.
- [ ] Document the QEMU CPU model and host CPU.  `kernel/tests/preempt_latency.rs` should exist and be runnable; only the `preempt-full`-on path is missing.

---

## Track A — Audit

### A.1 — Second pass over 57c catalogue

**File:** `docs/handoffs/57e-kernel-preempt-audit.md` (new)
**Symbol:** —
**Why it matters:** Every kernel codepath must be classified for `PREEMPT_FULL` safety.  A missed callsite is a deadlock waiting to happen.

**Acceptance:**
- [ ] Markdown table with rows = every entry from `docs/handoffs/57c-busy-wait-audit.md` plus every spinlock callsite from `docs/handoffs/57b-spinlock-callsite-audit.md`.
- [ ] Columns: file:line, symbol, spin pattern, current `preempt_disable` discipline (none, IrqSafeMutex-inherited, explicit), required under PREEMPT_FULL, rationale.
- [ ] Every "annotate" entry from 57c maps to a Track B task that adds the `preempt_disable` wrapper.
- [ ] Every "convert" entry from 57c is verified preempt-safe (block+wake calls already preempt-safe by construction; verify).

### A.2 — Identify dispatch-path reentrancy windows

**File:** `docs/handoffs/57e-dispatch-reentrancy.md` (new)
**Symbol:** —
**Why it matters:** Under `PREEMPT_FULL`, the dispatch path itself can be preempted.  Each window where this is unsafe must be `preempt_disable`-wrapped.

**Acceptance:**
- [ ] Identifies every window in `pick_next` and `dispatch` where preemption would corrupt state:
  - `SCHEDULER.lock` held: `preempt_count > 0` → safe.
  - Post-`pick_next`, pre-`switch_context` window: brief; benign-preemption case (chosen task goes back on queue).
  - `switch_context` body: IF=0 between `cli` and `popf` → safe.
  - `preempt_resume_to_kernel` body: IF=0 until `iretq` → safe.
  - `dispatch_preempted_and_resume_kernel` body (Track C.4): IF=0 until `iretq` → safe.
- [ ] Each window has a regression test that exercises preemption at that point.
- [ ] FPU/XSAVE coverage: confirm that `restore_fpu_state` is called *before* the dispatch path branches on `resume_mode == Preempted` (currently `kernel/src/task/scheduler.rs:3643–3645`), so both kernel-mode and user-mode preempt-resume paths inherit the same FPU restore.  The kernel build flags (`-mmx,-sse`) mean kernel code does not dirty XMM/YMM, but a kernel-mode preemption can interrupt a thread mid-syscall whose user FPU state is live in the registers; that state is captured at switch-out via the same `save_fpu_state` call.

---

## Track B — `preempt_disable` Wrapping

The 57c "annotate" sites split into two states post-57d.  B.1 verifies the already-wrapped sites are correctly placed and remain in force; B.2 wraps the remaining sites (annotated in source with the placeholder comment but no actual call).  B.3 audits per-CPU access patterns where preemption could cause a silent core-migration race.

### B.1 — Verify already-wrapped sites

**Files (have actual `preempt_disable` / `preempt_enable` calls today):**
- `kernel/src/smp/tlb.rs:93/128` — `tlb_shootdown` (full-range)
- `kernel/src/smp/tlb.rs:143/231` — `tlb_shootdown_range`
- `kernel/src/mm/frame_allocator.rs:894/901/957` — `drain_per_cpu_page_caches`
- `kernel/src/mm/slab.rs:463/470/510` — `collect_remote_frees`
- `kernel/src/arch/x86_64/ps2.rs:147/152` — `with_mouse_decoder` (covers both `wait_input_clear` and `wait_output_full` callers)
- `kernel/src/iommu/registry.rs:179/196` — VT-d migration discipline

**Symbol:** each spin's enclosing function
**Why it matters:** Reviewers must not assume "annotate" comments imply no work — these sites have real wrappers, and a refactor that drops the wrapper would re-introduce a deadlock under `PREEMPT_FULL`.

**Acceptance:**
- [ ] Each site has a regression test that asserts no preemption fires inside the spin (tracepoint count via `--features sched-trace`).
- [ ] Audit doc `docs/handoffs/57e-kernel-preempt-audit.md` records the wrapper bracket pair (line numbers) for each site.
- [ ] If a site needs the wrapper repositioned (e.g., wider scope), the change is justified in the audit doc.

### B.2 — Wrap remaining annotated-but-not-wrapped sites

**Files (have only the `// preempt_disable() wrapper added in Phase 57e Track B (load-bearing for PREEMPT_FULL only)` comment, no actual call):**
- `kernel/src/smp/ipi.rs:46` — `wait_icr_idle` (LAPIC ICR poll)
- `kernel/src/smp/boot.rs:277` — `delay_us` (LAPIC timer countdown, AP boot only)
- `kernel/src/arch/x86_64/apic.rs:436` — `calibrate_lapic_timer` PIT 10 ms gate
- `kernel/src/iommu/amd.rs:339` — `submit_and_wait` (AMD-Vi command-queue completion)
- `kernel/src/iommu/intel.rs:247` — `wait_gsts_bit`
- `kernel/src/iommu/intel.rs:368` — `invalidate_context_cache_global`
- `kernel/src/iommu/intel.rs:390` — `invalidate_iotlb_global`
- `kernel/src/rtc.rs:90` — `read_rtc` (UIP wait)

**Symbol:** each spin's enclosing function
**Why it matters:** SMP / IOMMU / hardware-handshake busy-spins must not be preempted — preemption mid-spin can block the holder's IPI delivery (SMP), leave hardware in an indeterminate state mid-handshake (IOMMU), or extend init-time deadlines past architectural bounds (RTC, PIT).

**Acceptance:**
- [ ] Each site has a `preempt_disable()` immediately before the spin and a `preempt_enable()` immediately after, replacing the placeholder comment.
- [ ] No-op behaviour verified for sites that run only at init time (boot.rs, apic.rs PIT calibration) — the wrapper is harmless when the scheduler is not yet live, because `preempt_disable` short-circuits when `current_preempt_count_ptr` is null.
- [ ] Regression test for each runtime-hot site (ipi.rs, amd.rs, intel.rs three sites, rtc.rs) asserts no preemption fires inside the spin.

### B.3 — Per-CPU data access audit

**Files:** every callsite that uses `try_per_core()` / `per_core()` whose **read value escapes the local statement** (stored in a struct, returned, or used after another lock acquire / `await` / preempt point).  Read-once-and-discard within a single statement (e.g., `let core_id = per_core().core_id;` immediately followed by use within the same atomic statement that does not yield) is **not** required to be wrapped.

**Symbol:** the per-CPU access pattern
**Why it matters:** Under `PREEMPT_FULL`, a task that reads a per-CPU value, gets preempted, migrates to another core, and resumes will see the **new** core's per-CPU value — silent data race.  `preempt_disable` around the *use lifetime* of the value prevents migration.  The "escapes the local statement" heuristic excludes the ~50 trivial `core_id` reads currently in `kernel/src/` and focuses the audit on the dozen-or-so genuinely stateful uses.

**Acceptance:**
- [ ] Audit doc enumerates every `per_core()` / `try_per_core()` callsite in `kernel/src/` (today: ~73 sites) and classifies each as `safe-read-once`, `wrapped-already`, or `needs-wrap`.
- [ ] Every `needs-wrap` site gets a `preempt_disable` / `preempt_enable` pair around the value's use lifetime.
- [ ] Regression test: a synthetic preemption inside a wrapped region does not migrate the task; a synthetic preemption inside an unwrapped read-once site is observed (proving the heuristic does not over-protect).
- [ ] When a new `per_core()` callsite is added in a later PR, the reviewer is expected to apply the same heuristic; documented in `docs/04-tasking.md`.

---

## Track C — `preempt_resume_to_kernel` + `dispatch_preempted_and_resume_kernel`

> **Note on file paths.**  57d kept all preempt asm in `global_asm!` blocks inside `kernel/src/arch/x86_64/interrupts.rs:583–795` — it did **not** move the asm into a separate `.S` file (despite 57d C.3 being marked complete).  57e follows that convention: new asm symbols extend the existing `global_asm!` block in `interrupts.rs`.  The shared GPR macros are already present at `interrupts.rs:587–621` (`save_gprs_all` / `restore_gprs_all`); 57e reuses them rather than introducing a `_preempt_resume_common` macro.

### C.0 — Implement `preempt_to_scheduler_kernel` (Rust shim)

**File:** `kernel/src/task/scheduler.rs`
**Symbol:** `preempt_to_scheduler_kernel`
**Why it matters:** 57d's kernel-path asm stub already captures the interrupted kernel RSP and passes it as the second argument to `timer_handler_kernel(&mut PreemptTrapFrameKernel, captured_kernel_rsp: u64)` (`interrupts.rs:1324`).  In 57d the kernel handler returns early without using the value.  Track F.1 will call `preempt_to_scheduler_kernel(frame, captured_kernel_rsp)` instead.

The plumbing is largely in place.  `PreemptTrapFrameKernel::to_preempt_frame(captured_kernel_rsp: u64) -> PreemptFrame` already exists at `kernel/src/arch/x86_64/preempt_trap_frame.rs:175` and writes the captured RSP into the `rsp` slot with `ss = 0`.  C.0 is therefore a thin wrapper that builds a `PreemptFrame` via the existing helper and delegates to `preempt_frame_to_scheduler` (`scheduler.rs:1901`) — the same routine the user path uses.

**Acceptance:**
- [ ] `pub fn preempt_to_scheduler_kernel(frame: &PreemptTrapFrameKernel, captured_kernel_rsp: u64) -> !` calls `frame.to_preempt_frame(captured_kernel_rsp)` and forwards the result to `preempt_frame_to_scheduler`.
- [ ] `preempt_frame_to_scheduler` already marks `state = Ready`, `on_cpu = false`, `resume_mode = Preempted` and stores the frame in `Task::preempt_frame` — no duplication of that bookkeeping in the `_kernel` shim.
- [ ] `-> !` — does not return; relies on `preempt_frame_to_scheduler`'s `switch_context` divergence.
- [ ] In-QEMU test: a synthetic ring-0 preemption produces a `Task::preempt_frame` whose `rsp` equals the kernel-stack pointer at the moment of CPU entry and whose `cs & 3 == 0`.
- [ ] Gated on `cfg(feature = "preempt-full")`; under `preempt-voluntary` only the symbol does not exist.

### C.1 — Implement `preempt_resume_to_kernel` (assembly, same-CPL `iretq`)

**File:** `kernel/src/arch/x86_64/interrupts.rs` (extend the existing `global_asm!` block at lines 706–795)
**Symbol:** `preempt_resume_to_kernel`
**Why it matters:** Same-CPL `iretq` is structurally different from privilege-changing `iretq`.  The CPU pops only `rip, cs, rflags` (no `rsp`, no `ss`).  Pushing 5 fields and `iretq`ing would corrupt the stack.  This routine mirrors `preempt_resume_to_user` (lines 706–738) but builds a 3-field frame and switches the stack pointer to `preempt_frame.rsp` before pushing.

**Acceptance:**
- [ ] Routine reads `rdi = *const PreemptFrame` (SysV AMD64 arg1) — same calling convention as `preempt_resume_to_user`.
- [ ] Routine restores GPRs from `frame.gprs` using the existing `restore_gprs_all` macro shape (rename to `restore_gprs_all_no_iretq` if a variant is needed; otherwise reuse).
- [ ] Routine sets `RSP = preempt_frame.rsp` (placing the stack pointer where the interrupted code was running).
- [ ] Routine pushes only 3 fields onto that stack: `rip, cs, rflags` (in iretq pop order: rflags, cs, rip).
- [ ] Routine `iretq`s.  CPU pops the 3 fields and resumes at `rip` in ring 0.
- [ ] In-QEMU test (round-trip): a kernel task is preempted, dispatched, and resumed; the resumed task's RIP, RSP, RFLAGS, and all 15 GPRs match what was saved.
- [ ] Negative test: pushing 5 fields and `iretq`ing from this routine produces a fault (validates the test catches the wrong frame shape).
- [ ] Gated on `cfg(feature = "preempt-full")`.

### C.2 — Dispatch path inspects `cs & 3` and routes correctly

**File:** `kernel/src/task/scheduler.rs` (`dispatch` function, around line 3661 where the `Preempted` branch currently calls `dispatch_preempted_and_resume`)
**Symbol:** `dispatch`
**Why it matters:** The dispatch path must choose between `_user` and `_kernel` resume routines based on the saved `cs.rpl()`.  A wrong branch produces a privilege-changing iretq from a same-CPL frame (or vice versa), which faults.

**Acceptance:**
- [ ] Dispatch reads `Task::preempt_frame.cs & 3` and routes:
  - rpl == 3 → `dispatch_preempted_and_resume` (existing user path; preserves 57d behaviour).
  - rpl == 0 → `dispatch_preempted_and_resume_kernel` (new in C.4).
- [ ] Regression test: a user-mode preemption resumes via the user trampoline; a kernel-mode preemption resumes via the kernel trampoline.
- [ ] Negative test: a deliberately misrouted task (e.g., user-mode `cs` with `_kernel` resume) faults — confirming the branch is the only thing standing between the two paths.
- [ ] Gated on `cfg(feature = "preempt-full")`; the user path is unchanged when the feature is off.

### C.3 — Reuse the `restore_gprs_all` macro for the kernel resume routine

**File:** `kernel/src/arch/x86_64/interrupts.rs`
**Symbol:** `restore_gprs_all` (existing, lines 605–621)
**Why it matters:** DRY — GPR restore is identical between the user and kernel resume routines.  The iretq frame layout (5-field privilege-changing for `_user`, 3-field same-CPL for `_kernel`) and the RSP handling (CPU-pushed `rsp` for `_user`, explicit `mov rsp, preempt_frame.rsp` for `_kernel`) are variant-specific and *not* shared.

**Acceptance:**
- [ ] `preempt_resume_to_kernel` uses the same GPR-restore code path as `preempt_resume_to_user` (either by reusing `restore_gprs_all` or by sharing inline `mov`/`pop` sequences — whichever fits the resume routine's register allocation).
- [ ] No new macro is introduced unless the existing `restore_gprs_all` cannot be adapted; the `_preempt_resume_common` shape originally proposed is **not** materialized.
- [ ] No regression from C.1 / C.2 tests.

### C.4 — Implement `dispatch_preempted_and_resume_kernel` (assembly trampoline)

**File:** `kernel/src/arch/x86_64/interrupts.rs` (extend the existing `global_asm!` block; mirror lines 769–794)
**Symbol:** `dispatch_preempted_and_resume_kernel`
**Why it matters:** **This is the structural piece the original 57e doc missed.**  57d ships `dispatch_preempted_and_resume` (`interrupts.rs:769`) — an asm trampoline that builds a `switch_context`-compatible frame on the *scheduler stack*, writes the new scheduler RSP to `*per_sched_rsp_ptr`, then jumps to `preempt_resume_to_user`.  Without the scheduler-RSP write, the dispatch loop's `saved_rsp` invariant breaks: when the resumed task next yields back, `switch_context` would load a stale frame on the prior scheduler RSP and the kernel UB-panics (this is the exact bug 57d D.3 fixed).  Kernel-mode preemption needs the same trampoline, ending in `jmp preempt_resume_to_kernel` instead of `jmp preempt_resume_to_user`.

**Acceptance:**
- [ ] `dispatch_preempted_and_resume_kernel(per_sched_rsp_ptr: *mut u64, frame: *const PreemptFrame) -> !` exposed as `extern "C"`.
- [ ] Body mirrors `dispatch_preempted_and_resume` (`interrupts.rs:769–794`) exactly: push the resume label, push callee-saves in `switch_context` order, `pushf`, `cli`, `mov [rdi], rsp`, then `mov rdi, rsi; jmp preempt_resume_to_kernel`.
- [ ] The `.Ldispatch_preempted_resume` landing label can be shared with the user variant (both trampolines `ret` to the same dispatch-loop epilogue).
- [ ] In-QEMU test (round-trip via dispatch): a kernel task is preempted; dispatch routes through `dispatch_preempted_and_resume_kernel`; the task runs; on next preemption `switch_context` loads the frame from `*per_sched_rsp_ptr` and the kernel does not UB-panic.
- [ ] Gated on `cfg(feature = "preempt-full")`.

---

## Track D — Dispatch Reentrancy Audit

### D.1 — Validate dispatch windows

**File:** `kernel/src/task/scheduler.rs`
**Symbol:** `pick_next`, `dispatch`
**Why it matters:** Each window identified in A.2 must have its preemption-safety property tested.

**Acceptance:**
- [ ] Each window has a regression test that fires preemption at that point and asserts no corruption (no panic, no deadlock, no stale state).
- [ ] If a window requires explicit `preempt_disable`, the wrapper is added and the test fails before / passes after.

### D.2 — Property test for kernel-mode preemption transitions

**File:** `kernel-core/src/preempt_model.rs` (extended)
**Symbol:** kernel-mode preempt event
**Why it matters:** Property tests cover random sequences of (kernel-mode preempt, lock acquire, lock release, syscall enter, syscall exit) and assert the invariants hold.

**Acceptance:**
- [ ] Property test runs ≥ 10 000 random sequences.
- [ ] Asserts: preemption only fires when `preempt_count == 0`.
- [ ] Asserts: preemption never fires while `SCHEDULER.lock` is held (because `SCHEDULER.lock` raises `preempt_count`).
- [ ] Asserts: a preempted kernel-mode task resumes to its kernel-mode `rip`.

### D.3 — Held-lock watchdog (`[WARN] [preempt] kernel-mode preemption with held lock`)

**File:** `kernel/src/task/scheduler.rs` (preempt path) and `kernel/src/sync/` (lock instrumentation)
**Symbol:** `kernel_preempt_watchdog`
**Why it matters:** A missed `preempt_disable` annotation around a kernel spinlock under `PREEMPT_FULL` would let an IRQ preempt the lock holder.  The watchdog fires when a kernel-mode preemption is observed *and* the chosen task is found to be holding a known scheduler-context lock at dispatch entry — flagging exactly the discipline bug 57e is most exposed to.  Without this, the same bug presents only as a deadlock during the 24-hour soak with no obvious source.

**Acceptance:**
- [ ] Each tracked lock (`SCHEDULER.lock`, `IRQ_REGISTRY.lock`, `IPC_PORTS.lock`, …) records its current holder pid in a per-lock `AtomicU32` field on acquire and clears it on release.  Lock list comes from a static set; not every kernel lock — just the ones whose ordering is known and whose acquisition raises `preempt_count`.
- [ ] At kernel-mode preempt entry (after the `preempt_count == 0` and `reschedule` checks pass, before transferring to the scheduler), the watchdog scans the tracked-lock holder fields for the current pid; if any match, emits `[WARN] [preempt] kernel-mode preemption with held lock pid=X lock=Y rip=Z` and panics in debug builds, logs-only in release.
- [ ] The watchdog never fires under `PREEMPT_VOLUNTARY` (the kernel handler doesn't call `preempt_to_scheduler_kernel` in that mode).
- [ ] Regression test: a synthetic kernel task that acquires `IRQ_REGISTRY.lock` and runs into a forced preemption produces the watchdog warning *and* the test fails — proving the watchdog catches the discipline bug.
- [ ] Gated on `cfg(feature = "preempt-full")`.

---

## Track E — Latency Benchmarks (per-trigger)

Each benchmark establishes a 57d baseline first (run with `preempt-full` *off*) and then measures under 57e (run with `preempt-full` *on*).  The four benchmarks measure structurally different trigger paths because dropping `from_user` affects them by very different amounts.

### E.1 — Cross-core reschedule-IPI wakeup benchmark

**File:** `kernel/tests/preempt_latency.rs` (new)
**Symbol:** `bench_cross_core_ipi_wakeup`
**Why it matters:** This is the path where `PREEMPT_FULL` is expected to deliver the biggest latency improvement.  Under 57d, an IPI delivered to a core in kernel mode is ignored by the preemption check (`from_user == false`); under 57e it preempts immediately.

**Acceptance:**
- [ ] Task A on core 0 wakes Task B blocked on core 1 via futex; measure wake-to-dispatch latency.
- [ ] Reports median, P95, P99 over 1000 iterations.
- [ ] 57d baseline captured with `preempt-full` off.  57e measurement with `preempt-full` on.
- [ ] Acceptance: 57e P95 < 57d P95 *by a measured factor reported in the PR description*.  Target ≥10× drop; merge-blocking only if the measured factor is ≤1×.

### E.2 — Same-core wakeup benchmark

**File:** `kernel/tests/preempt_latency.rs`
**Symbol:** `bench_same_core_wakeup`
**Why it matters:** `PREEMPT_FULL` does *not* add a self-IPI; same-core wakes still rely on the next timer tick or `preempt_enable` zero-crossing.  This benchmark establishes that 57e does not silently regress this path while improving the cross-core path.

**Acceptance:**
- [ ] Task A on core 0 wakes Task B *also on core 0* via futex.
- [ ] Reports median, P95, P99 over 1000 iterations.
- [ ] Acceptance: 57e P95 ≤ 57d P95 + 5 % (no regression).
- [ ] No order-of-magnitude improvement is claimed for this trigger.

### E.3 — Timer-only kernel-mode preemption benchmark

**File:** `kernel/tests/preempt_latency.rs`
**Symbol:** `bench_kernel_timer_preempt`
**Why it matters:** A kernel-mode CPU-bound loop (without `preempt_disable`) must be preempted at the next timer tick.  Under 57d this never happens; under 57e it must.

**Acceptance:**
- [ ] Spawn a kernel task running a tight loop with `preempt_count == 0`.
- [ ] Measure time from loop start to first preemption.
- [ ] Acceptance: 57e P95 < 1.5 × `1000 / TICKS_PER_SEC` ms (one timer tick plus a margin).

### E.4 — `preempt_enable` zero-crossing benchmark

**File:** `kernel/tests/preempt_latency.rs`
**Symbol:** `bench_preempt_enable_zero_crossing`
**Why it matters:** Under 57d, `preempt_enable` zero-crossings record `preempt_resched_pending` and consume it at the next user-mode return.  Under 57e, kernel-mode `preempt_enable` may fire the scheduler immediately if the calling context is preempt-safe.

**Acceptance:**
- [ ] An IRQ sets `reschedule` while the running task holds a lock; the lock is released; measure release-to-scheduler-entry latency.
- [ ] 57d baseline: latency = time-to-next-user-mode-return (potentially milliseconds depending on workload).
- [ ] 57e target: latency drops to microsecond range when the calling context is preempt-safe.
- [ ] Acceptance: 57e P95 < 57d P95 by a measured factor.

### E.5 — Audio-stack latency probe (qualitative)

**File:** `userspace/audio_server/tests/latency.rs` (new) or in-QEMU integration test
**Symbol:** —
**Why it matters:** End-to-end audio latency is a user-facing metric.  This is *not* a hard-gating benchmark; it confirms the synthetic improvements in E.1 / E.4 translate to a user-visible improvement.

**Acceptance:**
- [ ] Measure frame-to-output latency for the audio_server pipeline.
- [ ] Acceptance: no regression vs 57d baseline; no buffer underruns under 4-task synthetic load.
- [ ] An order-of-magnitude improvement is *not* required.

---

## Track F — Activate Kernel-Mode Preemption

### F.1 — Replace 57d's kernel-handler early-return with the user-handler body

**Files:** `kernel/src/arch/x86_64/interrupts.rs`
**Symbols:** `timer_handler_kernel`, `reschedule_ipi_handler_kernel` (Rust handlers introduced in 57d Track B)
**Why it matters:** Under 57d, the kernel handlers run only the tick / EOI / reschedule-flag work and return — kernel-mode preemption is structurally absent.  57e replaces the early-return body with the same preempt check the user handlers run, plus a call to `preempt_to_scheduler_kernel` (Track C.0) that consumes the captured kernel RSP that 57d's asm stub already passes as a second argument.  The decision-side change is structural (kernel handler body becomes the same shape as user handler), not a single-line drop; the rest of 57e (Tracks A–E and the kernel-mode `preempt_enable` immediacy in F.2) is what makes the change safe to ship.

**Acceptance:**
- [ ] In `timer_handler_kernel` and `reschedule_ipi_handler_kernel`, replace the early-return with: lapic_eoi; `let pc = unsafe { (*per_core().current_preempt_count_ptr.load(Acquire)).load(Relaxed) }; if pc != 0 { return; } if !per_core().reschedule.swap(false, AcqRel) { return; } unsafe { preempt_to_scheduler_kernel(frame, captured_kernel_rsp); }`.
- [ ] **Group-exit redirect is NOT applied** on the kernel handler path.  The user handler calls `maybe_redirect_group_exit_trampoline_user` (`interrupts.rs:1301`) before its preempt check, which can rewrite `frame.cs` to ring 0; the user-side `check_and_preempt_user` then guards `frame.cs & 3 != 3` to skip preemption when that happens (`interrupts.rs:1249`).  The kernel handler must not call the redirect helper because (a) the 3-field iretq frame has no `rsp`/`ss` to redirect through, and (b) kernel-mode tasks do not have `group_exit_pending` semantics.  Document this asymmetry inline.
- [ ] Gated on `cfg(feature = "preempt-full")`; default off.  Under `preempt-voluntary` only, the 57d body (early-return) remains.
- [ ] In-QEMU test: a kernel-mode CPU-bound task (one without `preempt_disable`) is preempted within 1 ms.
- [ ] In-QEMU test: a kernel-mode task holding an `IrqSafeMutex` (i.e., `preempt_count > 0`) is *not* preempted; the mutex unlock and subsequent `preempt_enable` zero-crossing path (F.2) handles the wake.

### F.2 — Kernel-mode `preempt_enable` immediate zero-crossing

**File:** `kernel/src/task/scheduler.rs`
**Symbol:** `preempt_enable`
**Why it matters:** Under 57d, `preempt_enable` zero-crossings record `preempt_resched_pending` and consume it at the next user-mode return — because kernel mode is non-preemptible.  Under 57e, kernel mode is preemptible, so `preempt_enable` may fire the scheduler immediately when the post-decrement count is 0 *and* `reschedule` is set *and* the calling context is preempt-safe.

The "preempt-safe" precondition is **not** just `preempt_count == 1 → 0`; it must also hold:

1. `interrupts::are_enabled() == true` at the call site.  Otherwise `preempt_enable` from inside a `without_interrupts` block (e.g., during PS/2 mouse byte handling, IRQ-disabled lock sections) would dispatch into the scheduler with IF=0 and the chosen task would resume with IF=0 until its next IRQ-disabled→enabled transition — silently breaking the scheduler's "tasks resume with their own RFLAGS" invariant.
2. The current core is **not** mid-`pick_next` / mid-`dispatch`.  In practice this is already guaranteed because both functions raise `preempt_count` via `scheduler_lock()`, but this is a documented invariant to be defensive against future refactors.

**Acceptance:**
- [ ] `preempt_enable` post-decrement: if `prev == 1` *and* `per_core().reschedule.load(Relaxed) == true` *and* `x86_64::instructions::interrupts::are_enabled() == true`, call into the scheduler immediately rather than only setting `preempt_resched_pending`.
- [ ] When any precondition fails, fall back to the 57d deferred-record path (set `preempt_resched_pending = true`); the next user-mode return or next IRQ-return-from-preempt-safe-context consumes it.
- [ ] Gated on `cfg(feature = "preempt-full")`; under `preempt-voluntary` only, the 57d behaviour is preserved.
- [ ] Latency benchmark E.4 (preempt_enable zero-crossing) under 57e measures the immediate-switch latency floor, not the deferred-to-user-return floor.
- [ ] Regression test: `preempt_enable` called from within `without_interrupts` does *not* dispatch immediately — verifies the IF gate.

### F.3 — Tracepoint update

**File:** `kernel/src/task/scheduler.rs`
**Symbol:** `preempt_to_scheduler` (Rust shim)
**Why it matters:** The trace entry must include `kernel_mode` so a future debugger can distinguish 57d-style and 57e-style preemptions.

**Acceptance:**
- [ ] Trace entry includes `kernel_mode: bool` field.
- [ ] Manual smoke: enable feature, reproduce a kernel-mode preemption, dump trace ring, see the entry.

---

## Track G — 24-Hour Soak

### G.1 — Standard graphical-stack workload

**File:** procedural; results in `docs/handoffs/57e-soak-result.md`
**Symbol:** —
**Why it matters:** A 24-hour soak with realistic load is the gate.  Any deadlock, panic, or `[WARN]` line during the soak fails the phase.

**Acceptance:**
- [ ] 24-hour run with `cargo xtask run-gui` plus a synthetic load: SSH disconnect/reconnect every 10 s, IPC ping/pong every 100 ms, futex wait/wake every 50 ms.
- [ ] Zero `[WARN] [sched]` lines.
- [ ] Zero `[WARN] [preempt]` lines.
- [ ] No deadlocks, panics, or scheduler hangs.
- [ ] No buffer underruns in audio_server.

### G.1.b — 57d open-item regression watch

**File:** procedural; documented alongside `docs/handoffs/57e-soak-result.md`
**Symbol:** —
**Why it matters:** The 57d-graphical-boot-debugging handoff and `fb-takeover-tiers.md` close out 57d with several known-flaky paths in IPC wake propagation, virtio-blk completion ordering, and userspace input/display state.  None blocks 57e correctness, but several are in code paths that PREEMPT_FULL stresses harder (higher switch frequency, kernel-mode preemption windows, cross-core IPI immediacy).  Making the soak explicitly grep for them turns "57e exposed a latent bug" into a hard merge gate rather than a post-flip surprise.

**Acceptance:**
The soak log is grep'd for each pattern; thresholds below are merge-blocking unless an explicit rationale is documented in `docs/handoffs/57e-soak-result.md`.
- [ ] `[virtio-blk] completion poll + queue notify after request timeout` — count must not exceed the 57d post-cleanup baseline (recorded in Track 0.2).  An increase under 57e indicates the kernel-mode preempt path interferes with virtio-blk completion delivery and must be root-caused before flag flip.
- [ ] `no waker registered .* stuck-since=` (the `BlockedOnReply` 30-second watchdog flagged in `docs/appendix/fb-takeover-tiers.md` "Second consecutive `fb-takeover doom` hangs") — must be **zero**.  This is the IPC wake-propagation path 57e changes most.
- [ ] `display_server: client protocol violation reason=` — count must not exceed the 57d post-cleanup baseline.  Display-protocol decoder is timing-sensitive; higher switch frequency under 57e could expose burst-time decoder bugs.
- [ ] `userspace page fault: pid=.* rip=` — must be **zero**.  Regression guard for the 57d Ion FS.base / `AT_PHENT` fix.
- [ ] Any `[exec-trace] fork-task-spawn` line without a matching `[exec-trace] fork-child .* trampoline-enter` line (use the canonical regression recipe from `docs/handoffs/57d-graphical-boot-debugging.md` § "Concrete regression recipe") — must be **zero**.  Regression guard for the 57d fork-dispatch stall fix.
- [ ] If any threshold is exceeded, root-cause the regression before Track H.1.  If the regression is judged independent of 57e (e.g., a 57d open item that happens to fire during the soak), document the rationale in `docs/handoffs/57e-soak-result.md` with reproduction steps.

### G.2 — Latency benchmark validation

**File:** procedural; results documented
**Symbol:** —
**Why it matters:** The latency targets from Track E must hold post-soak.

**Acceptance:**
- [ ] Re-run **E.1 / E.2 / E.3 / E.4** benchmarks at the end of the soak; results match the pre-soak measurements within ±10 %.

### G.3 — XSAVE / AVX soak coverage

**File:** procedural; soak workload extension
**Symbol:** —
**Why it matters:** Track J's XSAVE migration only matters under load that actually dirties YMM registers.  The 24-hour soak (G.1) must include an AVX-using workload component or the latent FP-corruption regressions in Track J go undetected.

**Acceptance:**
- [ ] G.1 synthetic load includes a userspace task that spins on AVX intrinsics (e.g., `_mm256_add_ps` over a working set) and validates a checksum every 100 ms; checksum must remain stable for the full 24 hours.
- [ ] At least one hosted-binary workload (audio_server pipeline or `ion` shell session) is part of the soak.

---

## Track H — Default-On Flip

### H.1 — Declare `preempt-full` feature and flip default

**File:** `kernel/Cargo.toml`
**Symbol:** `preempt-full` feature
**Why it matters:** The feature must layer correctly on top of `preempt-voluntary` so the kernel-handler body change always sits above a working user-mode preemption path.  The default flip happens once G validates.

**Acceptance:**
- [ ] `[features]` section declares `preempt-full = ["preempt-voluntary"]`.  Note: post-57d-I.3 the `preempt-voluntary` feature is *removed*; if I.3 has landed, `preempt-full = []` and the kernel-handler body change becomes unconditional under that feature alone.  Either form is acceptable; the choice depends on whether 0.1 (57d I.3) closes before H.1.
- [ ] `default` includes `preempt-full` once G validates.
- [ ] `cargo xtask check` clean; `cargo xtask test` passes.

### H.2 — Post-flip soak

**File:** procedural
**Symbol:** —
**Why it matters:** Final confidence gate after the default-on flip.

**Acceptance:**
- [ ] 24-hour soak with default build; results match G.1.

### H.3 — Remove the feature flag

**Files:**
- `kernel/Cargo.toml`
- All `cfg(feature = "preempt-full")` callsites

**Symbol:** —
**Why it matters:** Cleanup.

**Acceptance:**
- [ ] Feature flag removed; all `cfg` blocks unwrapped.
- [ ] `git grep preempt-full` returns zero results.
- [ ] **Scope guard:** This task removes only `cfg(feature = "preempt-full")` blocks.  The `cfg(feature = "exec-trace")` and `cfg(feature = "sched-trace")` diagnostic infrastructure (fork-task-spawn / trampoline-enter logs, `[TRACE] [sched]` ring, `[exec-trace] dup2/execve/close` lines) **stays intact** — `docs/handoffs/57d-graphical-boot-debugging.md` defers its removal until the burst-time `CommitSurface` and sector-2072 virtio-blk timeout leads are root-caused.  The virtio-blk timeout-recovery `[virtio-blk] completion poll + queue notify after request timeout` log line is unconditional and stays as well.  Reviewers should reject any H.3 PR that touches `exec-trace`, `sched-trace`, or virtio-blk timeout logging.

### H.4 — Documentation update

**Files:**
- `docs/03-interrupts.md`
- `docs/04-tasking.md`
- `docs/roadmap/README.md`
- `kernel/Cargo.toml` (version bump)

**Symbol:** —
**Why it matters:** Phase landing must be documented.

**Acceptance:**
- [ ] Documentation updated to describe `PREEMPT_FULL` semantics.
- [ ] `docs/04-tasking.md` describes the XSAVE/XRSTOR migration and the supported state-component mask (x87 + SSE + AVX for 1.0; AVX-512 deferred).
- [ ] Phase 57e row marked Complete in README.
- [ ] Kernel version bumped (e.g., `0.57.5` or `0.58.0` if this is the gate to release 1.0).

---

## Track J — XSAVE Migration (FXSAVE → XSAVE)

The kernel today uses `fxsave64`/`fxrstor64` (`kernel/src/task/scheduler.rs:1428–1447`) to save/restore FPU state at every dispatch boundary.  FXSAVE only covers x87 + SSE (first 16 × 128-bit XMM); it does **not** save AVX YMM upper halves, ZMM, opmask, or PKRU.  Hosted binaries (musl ports, Rust crates with default codegen) emit AVX freely; under 57e's higher switch frequency the resulting silent FP corruption becomes a likely soak failure.  Track J replaces FXSAVE with XSAVE and enables the x87 + SSE + AVX state-component mask.  AVX-512 is deferred (one bit in XCR0; trivial to add later).

**Hardware target floor:** Intel Sandy Bridge / AMD Bulldozer (2011) or later.  Earlier CPUs lack OSXSAVE and are explicitly unsupported by m3OS as of 57e.  If the boot-time CPUID probe finds OSXSAVE absent, the kernel panics with a clear message.

### J.1 — CPUID detection helper

**File:** `kernel/src/arch/x86_64/cpuid.rs` (new file or extension of an existing CPUID helper if one already lives in `arch/x86_64/`)
**Symbol:** `xsave_features() -> XSaveFeatures`
**Why it matters:** XSAVE area size and supported state components are CPU-specific; the kernel must query them at boot and store them in a global so the per-task storage allocator can size correctly.

**Acceptance:**
- [ ] `pub struct XSaveFeatures { supported: bool, supported_components: u64, area_size: usize, xsaveopt: bool }` populated from CPUID:
  - Leaf 1 ECX bit 26 = XSAVE supported, bit 27 = OSXSAVE.
  - Leaf 0Dh sub-leaf 0: EAX bits = supported state components (low 32), EDX = high 32; ECX = max area size for all components, EBX = current area size at current XCR0.
  - Leaf 0Dh sub-leaf 1: EAX bit 0 = XSAVEOPT supported.
- [ ] Stored in a `OnceCell<XSaveFeatures>` populated once during BSP init, before the first task is created.
- [ ] If `supported == false`, the kernel `panic!`s at boot with `"57e requires OSXSAVE; running on a pre-2011 CPU is not supported"`.
- [ ] If `supported_components & 0x7 != 0x7` (x87 + SSE + AVX required), same panic.
- [ ] Host-testable via `cargo test -p kernel-core`: a unit-test on a synthetic CPUID stub asserts the parsing logic.

### J.2 — BSP and AP boot wiring (CR4.OSXSAVE + XCR0)

**Files:**
- `kernel/src/main.rs` (BSP init, before any task is created)
- `kernel/src/smp/boot.rs` (AP init, before the AP enters the scheduler idle loop)

**Symbol:** `enable_xsave_state` (new helper)
**Why it matters:** XSAVE faults if `CR4.OSXSAVE` is not set or if `XCR0` does not enable the state components passed to xsave's mask.  Both must be configured on every core.

**Acceptance:**
- [ ] Helper sets `CR4.OSXSAVE` (bit 18); verifies `CR4.OSFXSR` is already set (it is — required for `fxsave64`).
- [ ] Helper writes `XCR0 = 0x7` (x87 | SSE | AVX) via `xsetbv` with ECX=0.
- [ ] BSP calls the helper before the first `spawn_kernel_task` / `spawn_user_task`.
- [ ] Each AP calls the helper before entering the scheduler idle loop in `smp::boot`.
- [ ] In-QEMU regression: a CPUID re-read after the helper confirms `CR4.OSXSAVE` and `XCR0` are set as expected on every core.
- [ ] QEMU `-cpu` argument verified to advertise AVX (m3OS already runs with the default `-cpu qemu64,...` model — confirm AVX is in that feature set, and add `-cpu max` or `+avx` to xtask args if not).

### J.3 — Replace `FxSaveArea` with `XSaveArea`

**Files:**
- `kernel/src/task/mod.rs` (type definition)
- `kernel/src/task/scheduler.rs` (storage and call sites)

**Symbol:** `XSaveArea`
**Why it matters:** XSAVE requires 64-byte alignment (vs FXSAVE's 16-byte); the area size depends on the runtime XCR0 mask.  For the AVX-only target the area is 832 bytes; that becomes the static const for 1.0, and J.4 reads the size from `xsave_features().area_size` to validate at boot.

**Acceptance:**
- [ ] `#[repr(C, align(64))] pub struct XSaveArea { bytes: [u8; XSAVE_AREA_SIZE] }` where `XSAVE_AREA_SIZE: usize = 832` (covers x87 + SSE + AVX header + extended region).
- [ ] `XSaveArea::new()` initialises:
  - First 24 bytes (legacy region): x87 control word `0x037F`, MXCSR `0x1F80`, MXCSR mask `0xFFFF`, matching `FxSaveArea::new()`.
  - Header region (offset 512–575): clear `XSTATE_BV` to 0 — the xsave init optimisation interprets this as "no state is in modified-from-init form", so xrstor will load architectural defaults for everything.
- [ ] Boot-time assertion: `XSAVE_AREA_SIZE >= xsave_features().area_size` (panic if a future CPUID change makes the static size too small).
- [ ] `Scheduler::fpu_states: Vec<Box<XSaveArea>>` — same shape as today, just the type changes.

### J.4 — Replace asm with `xsave64` / `xrstor64`

**File:** `kernel/src/task/scheduler.rs:1428–1447, 1979`
**Symbol:** `save_fpu_state`, `restore_fpu_state`, `reset_current_task_fpu_state`
**Why it matters:** The mechanical instruction swap.  XSAVE/XRSTOR take an EDX:EAX feature mask; for 1.0 we always pass `EAX=0x7, EDX=0` (x87 | SSE | AVX).  XSAVEOPT (if available) is preferred for the save path because it skips state components in init form.

**Acceptance:**
- [ ] `save_fpu_state` becomes `xsave64 [{area}]` with `eax=0x7, edx=0` set immediately before.  If `xsave_features().xsaveopt`, use `xsaveopt64` instead.
- [ ] `restore_fpu_state` becomes `xrstor64 [{area}]` with `eax=0x7, edx=0`.
- [ ] `reset_current_task_fpu_state` zeros the area's `XSTATE_BV` header (offset 512) and calls `xrstor64` — relies on the init optimisation to load architectural defaults.
- [ ] In-QEMU test: a kernel task writes to YMM upper halves via `vmovaps`, yields, runs another task that also writes YMM upper halves, yields back; the original YMM upper halves are restored.  This test fails under FXSAVE and passes under XSAVE — proves the migration actually fixes the bug.

### J.5 — AVX context-switch regression test

**File:** `kernel/tests/xsave_avx.rs` (new)
**Symbol:** `bench_xsave_avx_context_switch`
**Why it matters:** Without an explicit test, a future regression that drops back to FXSAVE (or sets the wrong XCR0 mask) would not be caught by `cargo xtask test` — the existing FPU tests only check x87/SSE.

**Acceptance:**
- [ ] Test spawns two userspace tasks; each writes a known pattern to YMM0–YMM7 upper halves (using `_mm256_set_*` intrinsics), yields, then verifies the pattern survives.
- [ ] Test runs for ≥ 1000 iterations of the yield-and-verify cycle.
- [ ] Test fails if any YMM upper half is corrupted between yield and resume.
- [ ] Test runs under both `--features preempt-full` (Track F) and the default build to validate the XSAVE migration in isolation from the kernel-mode preemption headline change.

### J.6 — Documentation and version bump

**Files:**
- `docs/04-tasking.md` (FPU section)
- `kernel/Cargo.toml` (no version bump from J alone; H.4 handles it)

**Symbol:** —
**Why it matters:** Future contributors must understand the supported state-component mask and how to extend it (e.g., to AVX-512).

**Acceptance:**
- [ ] `docs/04-tasking.md` adds a section "FPU/XSAVE state preservation" that documents:
  - The supported mask (x87 + SSE + AVX = 0x7) and the 832-byte area size.
  - The CPU floor (Sandy Bridge / Bulldozer 2011+).
  - How to extend to AVX-512 (set bit 5 of XCR0; bump `XSAVE_AREA_SIZE`; verify CPUID 0Dh sub-leaf 0 ECX size).
- [ ] No docs change in 57e for AVX-512 — it's listed in "Deferred Until Later" of the design doc.

---

## Documentation Notes

- This phase is the **stretch goal** of the 57b/57c/57d/57e programme.  Whether to land 57e depends on m3OS's release goals and the soak data from 57c/57d.  A credible release-1.0 plateau exists at 57d (PREEMPT_VOLUNTARY parity with Linux desktop default).
- **57d gate inheritance (Track 0).**  57e cannot start until 57d I.2 (post-flip 24-hour soak) and I.3 (`preempt-voluntary` flag removal) close.  Stacking `PREEMPT_FULL` on an unsoaked voluntary baseline conflates failure modes.
- **Asm convention.**  57d kept all preempt asm in `global_asm!` blocks inside `kernel/src/arch/x86_64/interrupts.rs:583–795` rather than separate `.S` files; 57e follows that convention.  The shared `save_gprs_all` / `restore_gprs_all` macros (lines 587–621) are reused; no new `_preempt_resume_common` macro is introduced.
- **Dispatch trampoline parity (C.4).**  The user-side `dispatch_preempted_and_resume` (`interrupts.rs:769`) is the structural bridge between the dispatch loop's `*per_sched_rsp_ptr` invariant and the iretq-based resume routine; without a `_kernel` analogue, kernel-mode preemption would re-introduce the saved-rsp staleness bug 57d D.3 fixed.
- The decision-side change is a single conditional removed (Track F.1).  The full 57e implementation surface is larger: same-CPL `iretq` resume routine + dispatch trampoline (Track C), per-CPU access audit (Track B.3), kernel-mode `preempt_enable` immediate zero-crossing with IF-enabled gate (Track F.2), held-lock watchdog (Track D.3), and the XSAVE migration (Track J).  Reviewers should treat the audit catalogue at `docs/handoffs/57e-kernel-preempt-audit.md` as the source of truth for completeness across all of these.
- **Track B partial state.**  Several 57c "annotate" sites already have real `preempt_disable` / `preempt_enable` wrappers in code today (B.1).  The remaining sites have only a placeholder comment and need the actual call inserted (B.2).  Reviewers should cross-check 57c's audit catalogue for any missed annotations before approving Track B.
- **XSAVE migration is folded into 57e (Track J)** rather than scheduled as a separate phase because the higher switch frequency under `PREEMPT_FULL` makes the latent FXSAVE-vs-AVX corruption risk more likely to manifest during the 24-hour soak.  A single soak validates both changes.  AVX-512 is deferred (one bit in XCR0; trivial to add).
- **57d open items NOT folded into 57e.**  The following are documented in their own handoffs and explicitly deferred: the burst-time `CommitSurface` protocol violation (`docs/handoffs/57d-graphical-boot-debugging.md` § "Highest-value lead"), the sector-2072 virtio-blk write timeout (same handoff § "Secondary lead"), the second-consecutive `fb-takeover doom` `BlockedOnReply` wedge (`docs/appendix/fb-takeover-tiers.md` § "Second consecutive `fb-takeover doom` hangs"), the post-reclaim mouse-pointer reset (same), and the partially-fixed bottom-row terminal rendering (57d handoff § S6).  None blocks 57e correctness.  The 24-hour soak (Track G.1.b) checks each as a regression watch — if 57e amplifies any of them, root-cause before flag flip; otherwise they remain separate post-57e work.
- **virtio-input migration is independent of 57e.**  `docs/handoffs/2026-05-04-virtio-input-migration.md` is a ~1.5-day plan addressing a QEMU-only PS/2 arbitration bug.  Recommended landing window: a small PR before 57e Track G so the 24-hour soak runs against the cleaner input stack.  Not gating on 57e.
- The 24-hour soak gate (G) is the most important checkpoint.  Until G passes, the feature flag stays off in production.  H.3 (flag removal) is the final cleanup after H.2 (post-flip soak) confirms stability — and is **scoped to `preempt-full` only**; `exec-trace` / `sched-trace` / virtio-blk timeout-recovery diagnostics are retained.
