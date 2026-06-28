# Phase 99 — SMP & Scheduler Robustness Hardening: Task List

**Status:** Planned
**Source Ref:** phase-99
**Depends on:** Phase 57a–e (v2 scheduler block/wake protocol + preemption) ✅, Phase 35 / Phase 25 (SMP boot + IPI / TLB shootdown) ✅, Phase 98 (Roadmap Audit & Re-Charter) ✅
**Goal:** Retire the recurring cross-core lost-wakeup bug class by auditing every blocking call site against the **already-landed** single-state-word v2 model (`block_current_until`/`wake_task_v2`) and validating it at `-smp 8`, then close the companion fault-handling cluster: the deferred 2026-06-14 kstack-origin + locks-across-faults audit, the 2026-06-05 4 GiB panic-path AP-quiesce, the 2026-06-25 step-25 demand-fault NULL-deref CI flake, and two correctness bugs (`fs.copyFile`→EFAULT and a 55c `net::remote` test that encodes its RX fixture with the wrong-direction header). CI-able under QEMU SMP — this is the multi-core foundation Phase 100's bare-metal GUI session requires (the laptop is 8-core and cannot pin `-smp 1`).

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Blocking-primitive consolidation: call-site audit, futex conformance, scheduler-state diagnostic, `smp-smoke` raised to `-smp 8` | 57a–e | **Complete** — audit (29 sites, 1 lost-wake fixed) + `dump_scheduler_state` + `-smp 8`; smp-smoke @ -smp 8 PASS |
| B | Fault-handling robustness: locks-across-faults audit, kstack-overflow origin, recovery-stack review | 2026-06-14 tracks A–D | **Complete** — audit + `debug_assert` + origin doc; kstack-overflow-smoke + dynamic-hello PASS |
| C | 4 GiB SMP panic-path AP-quiesce (diagnosability) + residual OOM/race pass | A (diagnostic reuse) | **Mostly complete** — C.1 quiesce landed + no-regression; C.2 4 GiB run captured (clean boot, no panic, RAM-scaling slowness); readable-banner demo deferred (no panic to force) |
| D | Step-25 `dynlink-hello-versioned-mismatch-smoke` demand-fault NULL-deref flake → root-cause + fix + soak | B (fault-handler audit) | **Blocked** — CI-host-correlated flake, no red artifact captured; inspection + C.1 diagnosability advance landed; definitive fix needs a red CI ELF |
| E | Two correctness bugs: `copy_file_range`/`sendfile`→EFAULT, 55c `net::remote` RX-test encoder | — | **Complete** — E.1 clean-ENOSYS + `fs.copyFile` probe folded into node-smoke (PASS); E.2 already fixed (f39ca133, Phase 57b) |

---

## Track A — Blocking-Primitive Consolidation, Audit & SMP-8 Validation

> The single-state-word v2 model is **already present** (Phase 57a): `block_current_until` follows the four-step Linux recipe, `wake_task_v2` is the CAS wake, and `switching_out`/`wake_after_switch`/`PENDING_SWITCH_OUT` are deleted. Track A *completes and validates* it — it does not re-introduce it.

### A.1 — Blocking call-site conformance audit

**File:** `kernel/src/task/scheduler.rs` (plus an audit note under `docs/handoffs/` or `docs/appendix/`)
**Symbol:** `block_current_until` and every caller — the `BlockedOnReply` / `BlockedOnRecv` / `BlockedOnNotif` / `BlockedOnSend` wrappers (~`scheduler.rs:3853`–`4268`) and the futex waiter
**Why it matters:** The lost-wake recurrences (89/90b/06-14/95) were per-site patches; the model is only as correct as its least-conformant wait site, so a uniform-conformance audit is what actually retires the bug class.

**Acceptance:**
- [x] An audit note enumerates every `block_current_until` caller with: the `woken` flag it registers, where it rechecks its condition after the state write, and confirmation that no latched per-site flag survives a block call. → `docs/handoffs/2026-06-28-phase-99-block-wake-callsite-audit.md` (29 sites tabulated).
- [x] Each caller is confirmed to pass a **fresh** stack-local `AtomicBool` (no carried-over flag), matching the v1-flag-deletion invariant. → 28/29 conform; statics are edge-reset, loops reset per-iteration.
- [x] Any non-conformant site found is fixed. → `ipc/notification.rs::wait()` task-context `signal()` lost-wake fixed (re-register `WAITERS` slot on `AlreadyAwake` so the existing `drain_pending_waiters` net rescues it).

### A.2 — Consolidate per-site wake wrappers into one documented pattern

**File:** `kernel/src/task/scheduler.rs`
**Symbol:** the `block_on_*` wrapper helpers around `block_current_until`; the "Bug #6 brackets" / stack-local-`AtomicBool` comments (~`scheduler.rs:2356`–`2374`, `4112`–`4161`)
**Why it matters:** The bracketing comments and stack-local wrappers are N independent special cases that read as folklore; collapsing them to one referenced pattern prevents the next wait site from reinventing a subtly-wrong variant.

**Acceptance:**
- [x] The recv/reply/notif/send wrappers share one documented helper shape. → the canonical "fresh registered `Arc` waker → recheck-after-register → `block_current_until` → clear waker" pattern is documented once on `block_current_on_reply_v2` (the wrappers already reference it); the stale "local AtomicBool / wake side still v1" doc was replaced.
- [x] No behavioral change to the wake path: `smoke-test` stays green (PASSED, 26 steps); `regression` pending in the full validation pass. (Pure-doc change.)

### A.3 — Periodic / on-demand scheduler-state diagnostic

**File:** `kernel/src/task/scheduler.rs`
**Symbol:** `dump_scheduler_state` (new function), wired into the existing stale-ready / stuck-no-waker watchdog (~`scheduler.rs:2519`)
**Why it matters:** The 2026-04-25 handoff's recommendation #3 — the missing tool that turns "task stuck in Blocked forever" into direct evidence at the moment of hang, which would have shortened prior lost-wake debugging by hours.

**Acceptance:**
- [x] `dump_scheduler_state` prints one `[sched] task pid=X state=Y wake_deadline=Z on_cpu=W` line per live task (the four fields contiguous + greppable, plus `name`/`idx`/derived `blocked~` age).
- [x] It fires on the stuck-no-waker watchdog verdict (one-shot per boot), after `SCHEDULER.lock` is released; it acquires only `SCHEDULER.lock` (never `pi_lock`), so no lock-order inversion.
- [x] The diagnostic is ISR-safe / lock-aware: uses the non-blocking `try_scheduler_lock` and logs a "busy — skipped" note rather than deadlocking/panicking on contention.

### A.4 — Raise `smp-smoke` to `-smp 8`

**File:** `xtask/src/main.rs`
**Symbol:** `cmd_smp_smoke` (the `cores` default, ~`main.rs:18535`); `qemu_smp_count` (~`main.rs:5191`)
**Why it matters:** The laptop is 8-core; the futex WAIT/WAKE handshake must be proven at that core count, not `-smp 4`, before the bare-metal GUI arc relies on multi-core.

**Acceptance:**
- [x] `cmd_smp_smoke`'s default core count is raised from 4 to **8** (still `M3OS_SMP=<N≥2>`-overridable for CI's 2-vCPU runners).
- [x] `smp-smoke` PASSES at `-smp 8`: `SMP_STRESS_OK 256` (step 18 "multithreaded stress completes" passed), no `KERNEL PANIC` / `RECURSIVE KERNEL PAGE FAULT` / `process killed` / `no waker registered`. **Validated 2026-06-28, KVM, 18 steps in 68s.**
- [x] The `AGENTS.md` `M3OS_SMP_REGRESSION` row notes the new `-smp 8` default.

### A.5 — Futex model conformance (`REQUEUE` / `CMP_REQUEUE` + per-AS keys)

**File:** `kernel/src/arch/x86_64/syscall/mod.rs`
**Symbol:** the futex op handler — `FUTEX_REQUEUE` / `FUTEX_CMP_REQUEUE` (~`mod.rs:19747`–`20026`) and the per-address-space futex-key path
**Why it matters:** Phase 89/90b patched these per-incident; confirming a requeued cond-waiter is woken by a CAS on its state (never by a flag the requeue must coordinate) folds them into the audited model.

**Acceptance:**
- [x] The audit (A.1) covers the futex waiter and `REQUEUE`/`CMP_REQUEUE` requeue path, confirming requeued waiters wake via `wake_task_v2`'s state CAS (set `woken` before `wake_task_v2`; requeued waiters keep their fresh `Arc` flag, dormant until a `FUTEX_WAKE` on `uaddr2`). → audit §4.
- [x] `node-smoke` (PASSED) and `smp-smoke` at `-smp 8` (PASSED) both stay green. **Validated 2026-06-28.**

---

## Track B — Fault-Handling Robustness Audit

> The 2026-06-14 handoff landed the *recovery* (NMI-on-IST, mark-core-offline, degrade-shootdown, `fault_kill_trampoline`) and explicitly deferred the *origin audit*. Track B finishes that deferred work.

### B.1 — Locks-held-across-faults audit + assertion

**File:** `kernel/src/arch/x86_64/interrupts.rs`
**Symbols:** `page_fault_handler` (~`interrupts.rs:1539`), `general_protection_fault_handler` (~`:1970`), `double_fault_handler` (~`:2202`), `fault_kill_trampoline` (~`:303`), the `PROCESS_TABLE.lock()` / `try_lock_scheduler` sites (~`:376`, `:1642`, `:1784`)
**Why it matters:** Holding `SCHEDULER`/`PROCESS_TABLE` across a fault-prone or blocking operation is the `2026-04-21-scheduler-lock-isr-deadlock` class; the `MAP_LAZY_FILE` branch already documents the correct discipline (take `PROCESS_TABLE` before `shared_vma_demand_file` re-takes it; never strand it across the blocking IPC) and the audit codifies it.

**Acceptance:**
- [x] A documented sweep confirms no fault handler holds `SCHEDULER` or `PROCESS_TABLE` across the blocking `MAP_LAZY_FILE` `vfs_server` read or across `fault_kill_trampoline`. → `docs/handoffs/2026-06-28-phase-99-fault-handler-lock-audit.md` (10 lock sites, all SAFE; value-copy discipline traced).
- [x] A debug assertion enforces "no scheduler/process-table lock held on entry to the blocking demand-fault IPC." → `debug_assert_eq!(current_preempt_count(), 0, …)` at the demand-fault entry (covers both `IrqSafeMutex`es; non-flaky per-task counter).
- [x] `cargo xtask check` passes with the assertion compiled in. **Validated 2026-06-28.**

### B.2 — Pin the kstack-overflow origin

**File:** `kernel/src/arch/x86_64/interrupts.rs`, `kernel/src/task/kstack.rs`
**Symbol:** `try_recover_kstack_overflow` (`interrupts.rs:214`), the kstack guard-page classifier, the 64 KiB usable + 4 KiB guard sizing (`kstack.rs`)
**Why it matters:** 2026-06-14 Open Question #1 — the exact call chain that exhausts 64 KiB under V8/PKU churn was unrecoverable from the corrupted pre-IST frame; with NMI-on-IST it is now capturable, and pinning it tells us whether to bound a path or accept 64 KiB.

**Acceptance:**
- [x] The deepest contributing kernel call chain is named: the MAP_LAZY_FILE demand-fault → 64 KiB readahead-cluster → blocking `vfs_server` IPC path (every toolchain DSO demand-pages it). → audit B.2.
- [x] A note records that 64 KiB is sufficient given the `fault_kill_trampoline` recovery (a task-attributable overflow becomes a SIGSEGV; the reverted 64→96 KiB bump is explicitly **not** the fix). 64 KiB retained.

### B.3 — Recovery-stack & recursion-latch correctness review

**File:** `kernel/src/arch/x86_64/interrupts.rs`
**Symbol:** `FAULT_RECOVERY_STACKS` (`:191`), `IN_KERNEL_PAGE_FAULT` (`:271`), `recovery_stack_top_for_core` (~`:200`)
**Why it matters:** The recovery path must use a **per-core** stack and clear the recursion latch only when genuinely recovering (not cascading), or a recovered fault can corrupt a sibling core's recovery frame or mask a real recursion.

**Acceptance:**
- [x] Confirmed: each core uses its own `FAULT_RECOVERY_STACKS[core]` slot (indexed by `core_id.min(MAX_CORES-1)`); `try_recover_kstack_overflow` only redirects when `current_pid() != 0`. → audit B.3.
- [x] Confirmed: `IN_KERNEL_PAGE_FAULT[core]` is cleared (`:229`) only on the recovery path; on a true recursive cascade the CAS returns `Err` → halt with the flag left set. → audit B.3.
- [x] `kstack-overflow-smoke` PASS (child SIGSEGV-killed, parent survived — **2026-06-28**); `dynamic-hello-smoke` `THREAD_FAULT` arms green (**2026-06-28**).

---

## Track C — 4 GiB SMP Panic-Path AP-Quiesce

### C.1 — Quiesce sibling cores before printing the panic banner

**File:** `kernel/src/lib.rs` (and `kernel/src/main.rs` `#[panic_handler]`)
**Symbol:** `handle_panic` (`lib.rs:1272`); reuse `smp::try_per_core` / the `is_online` marking already added to `hlt_loop`
**Why it matters:** The 2026-06-05 handoff's blocking ask — at 4 GiB + `--kvm` + SMP the panic banner is unreadable because other cores keep writing COM1 during the print/dump; a legible banner is the prerequisite for root-causing the residual instability.

**Acceptance:**
- [x] `handle_panic` broadcasts a halt NMI to sibling cores (`panic_quiesce_aps`) and spins a bounded grace window for them to ack-and-park before printing; the `nmi_handler` parks non-owner cores. → `kernel/src/smp/mod.rs`, `kernel/src/lib.rs`, `kernel/src/arch/x86_64/interrupts.rs`.
- [~] A captured 4 GiB panic shows an uninterleaved banner — **mechanism landed; positive demonstration deferred**: no panic fired in the 4 GiB + smp-8 runs (no panic-trigger exists to force one). The diagnosability mechanism is in place for the next real panic. See `docs/handoffs/2026-06-28-phase-99-panic-quiesce-and-stepd25-flake.md`.
- [x] The quiesce is bounded (`SPIN_BUDGET` cap — never hangs on a wedged core) and does not regress the single-core/test panic path: `cfg(test)` `handle_panic` short-circuits to the ISA-debug-exit handler before the quiesce. No-regression validated (smoke-test + smp-smoke + 4 GiB boot all clean).

### C.2 — Residual 4 GiB OOM/race investigation pass

**File:** `kernel/src/smp/tlb.rs`, `kernel/src/task/scheduler.rs` (stale-ready watchdog), `kernel/src/trace.rs` (`dump_trace_rings`, `trace.rs:55`)
**Symbol:** the 4 GiB-only crash class (cross-referenced from `2026-05-24-4gib-pci-hole-vga-mapping.md`)
**Why it matters:** With a readable banner (C.1), one diagnostic pass at the residual >2 GiB instability can either close it or record a precise next-step, rather than leaving it permanently masked by the 2 GiB workaround.

**Acceptance:**
- [x] A 4 GiB + `--kvm` + `-smp 8` boot/stress run is captured (2026-06-28): boots clean (8 cores, ≈4 GiB), no panic, no wedge, scheduler stays live. No panic fired → nothing to symbolize this pass.
- [x] A handoff records the result + hypothesis: the residual >2 GiB effect manifests as cold-`node`-load **slowness** (RAM-scaling demand-fault/shootdown traffic), not a lost-wake or crash; the 30 s stuck-no-waker watchdog did not fire. Full root-cause of the RAM-scaling slowdown is **Deferred** per the design doc. → `docs/handoffs/2026-06-28-phase-99-panic-quiesce-and-stepd25-flake.md`.

---

## Track D — Step-25 Demand-Fault NULL-Deref Flake

### D.1 — Capture + symbolize a red step-25 run

**File:** `.github/workflows/pr.yml` (artifact upload already in place), `kernel/src/arch/x86_64/interrupts.rs`
**Symbol:** `dynlink-hello-versioned-mismatch-smoke` step 25; the crash `rip` / PIE load base from `target/ci-crash/smoke-test.log`
**Why it matters:** The flake is host-correlated, low-rate (~11–15 %), and does not reproduce locally; the kernel ELF is not bit-reproducible, so the crash can only be symbolized against CI's own uploaded ELF.

**Acceptance:** (BLOCKED on a red CI artifact — probabilistic/external; cannot be forced from the dev box. See `docs/handoffs/2026-06-28-phase-99-panic-quiesce-and-stepd25-flake.md`.)
- [ ] A red step-25 run is captured via `gh run download …` — **not yet captured** (the flake is ~11–15% CI-host-correlated and did not flake red during this work).
- [ ] `addr2line` against the **uploaded** ELF — pending a red artifact (local ELF gives a misleading symbol; the kernel is not bit-reproducible).
- [~] Verdict: the faulting `rip` is in kernel `.text` (a kernel NULL deref), **not** a kstack guard-page fault; inspection of the chain found no unchecked NULL deref in the current tree (`current_addr_space` cannot return `Some(null)`; the window-slice path is `?`-guarded). Definitive verdict still needs the symbolized CI frame.

### D.2 — Root-cause + fix the `cr2=0` NULL deref in the `MAP_LAZY_FILE` chain

**File:** `kernel/src/arch/x86_64/interrupts.rs` (`page_fault_handler` `MAP_LAZY_FILE` branch, ~`:906`/`:937`/`:943`), `kernel/src/process/mod.rs` (`shared_vma_demand_file`, `mod.rs:1308`)
**Symbol:** `shared_vma_demand_file`, `demand_map_user_page_from_buf_locked`, the blocking `vfs_server` read issued from the fault handler
**Why it matters:** This path demand-pages every multi-hundred-MB toolchain DSO (rustc/node/clang) and is exercised far harder on the slow bare-metal VFS, so a 1-in-7 kernel fault is unacceptable for the GUI arc.

**Acceptance:** (BLOCKED on D.1.)
- [ ] Root-cause + fix — **pending D.1's symbolization**. Inspection narrowed candidates (no obvious null deref in the current tree) but did not pin the exact NULL read; a fix without the symbolized frame would be speculative.
- [x] No speculative fix applied — honoring "grounded in the symbolized faulting function from D.1, not a speculative kstack bump." (The C.1 panic AP-quiesce is the concrete advance: it makes the next red CI banner readable so D.1 becomes actionable.)

### D.3 — N-iteration soak proving flake = 0

**File:** `xtask/src/main.rs` (the `dynlink-hello-versioned-mismatch-smoke` step), CI run records
**Symbol:** the step-25 smoke step
**Why it matters:** A flake this rate only proves fixed by a soak, not a single green run.

**Acceptance:**
- [ ] N≥50 consecutive step-25 iterations — **not achievable locally** (the flake is CI-host-correlated and does not reproduce on the dev box: the 2026-06-25 handoff already recorded 5/5 + an 8-run hunt with no red). A local soak only confirms known local non-repro; the real proof needs CI runs after a D.2 fix.
- [x] `smoke-test` overall stays green (PASSED, includes step 25) and no new flake is introduced; `dynamic-hello-smoke` + `smp-smoke @ -smp 8` (same `MAP_LAZY_FILE` chain, heavy SMP churn) also green. **Validated 2026-06-28.**

---

## Track E — Two Correctness Bugs

### E.1 — `copy_file_range` / `sendfile` → clean result or `-ENOSYS` (no EFAULT)

**Files:**
- `kernel/src/arch/x86_64/syscall/mod.rs` (the default dispatch arm returning `NEG_ENOSYS`, ~`mod.rs:2502`)
- `kernel/src/arch/x86_64/syscall/fs.rs` (new handlers if implemented)

**Symbol:** `copy_file_range` (syscall 326), `sendfile` (syscall 40) — neither has a handler today; `NEG_ENOSYS`
**Why it matters:** Node's `fs.copyFile` surfaces `EFAULT` instead of a working copy or a clean `ENOSYS`-fallback; the bad errno originates downstream of the (currently unhandled) `copy_file_range`/`sendfile` probe and must be tracked to its source.

**Acceptance:**
- [x] Origin identified: there is **no collision and no partial stub** — syscalls 326/40 hit the default dispatch arm (`mod.rs:2483`) returning a clean `NEG_ENOSYS`. The reported `EFAULT` does not reproduce on current HEAD; libuv's userspace read/write fallback succeeds after the ENOSYS probe.
- [x] `copy_file_range`/`sendfile` return a clean `-ENOSYS` so node's userspace fallback succeeds; `fs.copyFile` does **not** return `EFAULT`. (Kept ENOSYS — the acceptance's lower-risk option — rather than adding an untested syscall impl.)
- [x] A node `fs.copyFile` probe (5 KiB file copied + byte-verified, `COPYFILE_OK`) succeeds, **folded into `node-smoke`** (always-on). **Validated 2026-06-28 (node-smoke PASSED).**

### E.2 — 55c `net::remote` RX-test encoder fix

**Files:**
- `kernel/src/net/remote.rs` (the `#[test_case]` RX-path tests, ~`remote.rs:920`–`990`; `InvalidFrame` reject at `remote.rs:365`)
- `kernel-core/src/driver_ipc/net.rs` (`encode_header_with_kind` `:235`, `encode_net_rx_notify` `:294`, `encode_net_send_frame` / `NET_SEND_FRAME`)

**Symbol:** `encode_net_rx_notify` (ingress, `NET_RX_FRAME`) vs `encode_header_with_kind(NET_SEND_FRAME, …)` (egress)
**Why it matters:** A Phase-55c-era RX-path test that encodes its fixture with the egress `NET_SEND_FRAME` kind decodes as `InvalidFrame`, so the test asserts the wrong outcome and gives no real coverage of the RX decode path.

**Acceptance:** (NOTE: already fixed in commit `f39ca133`, Phase 57b — this task doc was stale. Verify-only.)
- [x] The RX-path test(s) encode their fixture with the ingress `NET_RX_FRAME` header via `encode_net_rx_notify` (the only `NET_SEND_FRAME`/`encode_net_send` use in `remote.rs` is the real TX send path). Confirmed at `remote.rs:920–990`.
- [x] The tests exercise the real RX inject/drain decode path without tripping `InvalidFrame` for a well-formed RX frame.
- [x] `cargo test -p kernel-core` host tests pass (`cargo xtask check` green); the `net::remote` `#[test_case]` suite is covered by `cargo xtask test`.

---

## Documentation Notes

- The single-state-word block/wake model (`block_current_until`/`wake_task_v2`, with `switching_out`/`wake_after_switch`/`PENDING_SWITCH_OUT` deleted) **already landed in Phase 57a** — record that Phase 99 *completes, consolidates, and validates-at-`-smp 8`* that model rather than introducing it, so future readers don't re-walk the rewrite.
- Note that the four prior lost-wake fixes (Phase 89 `FUTEX_REQUEUE`, Phase 90b per-AS futex keys + cross-thread PKU read-recovery, the 2026-06-14 cross-core lost-wake, Phase 95 rustc futex) are subsumed by the A.1 audit + the uniform A.2 pattern.
- Cross-reference the three open handoffs this phase closes/advances: `docs/handoffs/2026-06-14-claude-smp-tlb-shootdown-kstack-panic.md` (Track D origin audit → Track B here), `docs/handoffs/2026-06-05-4gib-smp-panic-corrupted-output.md` (panic AP-quiesce → Track C here), `docs/handoffs/2026-06-25-flaky-dynlink-mismatch-demand-fault-kernel-fault.md` (step-25 flake → Track D here), and the design source `docs/handoffs/2026-04-25-scheduler-design-comparison.md`.
- This phase is the **gating prerequisite for Phase 100** (Bare-Metal GUI Session, Dell) per the Phase 98 charter dependency graph — the laptop is 8-core and cannot pin `-smp 1`; keep that edge consistent in `docs/roadmap/README.md`'s next-arc section and mermaid (`P99 --> P100`).
- Prefer exact files/symbols over directories as these tasks land; update the checkboxes and the Track Layout status column as tracks complete.
- **Phase 99 deliverable docs (committed this phase):** `docs/handoffs/2026-06-28-phase-99-block-wake-callsite-audit.md` (A.1/A.5), `docs/handoffs/2026-06-28-phase-99-fault-handler-lock-audit.md` (B.1/B.2/B.3), `docs/handoffs/2026-06-28-phase-99-panic-quiesce-and-stepd25-flake.md` (C + D).
- Mark the design doc + this task doc Status `Complete` only when `smp-smoke` PASSES at `-smp 8`, the step-25 soak shows flake=0, and the two Track-E bugs are fixed with their gates green. **Status as of 2026-06-28:** `smp-smoke @ -smp 8` ✅, Track-E bugs ✅ — but the step-25 **flake=0 proof is BLOCKED on a red CI artifact** (CI-host-correlated, does not reproduce locally), so the phase is **not** yet markable `Complete`. Tracks A/B/E + C.1 are done and validated; Track D is advanced (inspection + the C.1 diagnosability that makes the next red CI run actionable) but its definitive fix + soak await CI.
