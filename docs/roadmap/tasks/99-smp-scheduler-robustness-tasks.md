# Phase 99 — SMP & Scheduler Robustness Hardening: Task List

**Status:** Planned
**Source Ref:** phase-99
**Depends on:** Phase 57a–e (v2 scheduler block/wake protocol + preemption) ✅, Phase 35 / Phase 25 (SMP boot + IPI / TLB shootdown) ✅, Phase 98 (Roadmap Audit & Re-Charter) ✅
**Goal:** Retire the recurring cross-core lost-wakeup bug class by auditing every blocking call site against the **already-landed** single-state-word v2 model (`block_current_until`/`wake_task_v2`) and validating it at `-smp 8`, then close the companion fault-handling cluster: the deferred 2026-06-14 kstack-origin + locks-across-faults audit, the 2026-06-05 4 GiB panic-path AP-quiesce, the 2026-06-25 step-25 demand-fault NULL-deref CI flake, and two correctness bugs (`fs.copyFile`→EFAULT and a 55c `net::remote` test that encodes its RX fixture with the wrong-direction header). CI-able under QEMU SMP — this is the multi-core foundation Phase 100's bare-metal GUI session requires (the laptop is 8-core and cannot pin `-smp 1`).

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Blocking-primitive consolidation: call-site audit, futex conformance, scheduler-state diagnostic, `smp-smoke` raised to `-smp 8` | 57a–e | In Progress — audit+fix+diagnostic+`-smp 8` landed; SMP-8 validation pending |
| B | Fault-handling robustness: locks-across-faults audit, kstack-overflow origin, recovery-stack review | 2026-06-14 tracks A–D | In Progress — audit+`debug_assert`+origin doc landed; gate validation pending |
| C | 4 GiB SMP panic-path AP-quiesce (diagnosability) + residual OOM/race pass | A (diagnostic reuse) | In Progress — C.1 quiesce landed; capture/C.2 pass pending |
| D | Step-25 `dynlink-hello-versioned-mismatch-smoke` demand-fault NULL-deref flake → root-cause + fix + soak | B (fault-handler audit) | In Progress — CI-correlated; local repro + root-cause-by-inspection |
| E | Two correctness bugs: `copy_file_range`/`sendfile`→EFAULT, 55c `net::remote` RX-test encoder | — | In Progress — E.2 already fixed (f39ca133, Phase 57b); E.1 in progress |

---

## Track A — Blocking-Primitive Consolidation, Audit & SMP-8 Validation

> The single-state-word v2 model is **already present** (Phase 57a): `block_current_until` follows the four-step Linux recipe, `wake_task_v2` is the CAS wake, and `switching_out`/`wake_after_switch`/`PENDING_SWITCH_OUT` are deleted. Track A *completes and validates* it — it does not re-introduce it.

### A.1 — Blocking call-site conformance audit

**File:** `kernel/src/task/scheduler.rs` (plus an audit note under `docs/handoffs/` or `docs/appendix/`)
**Symbol:** `block_current_until` and every caller — the `BlockedOnReply` / `BlockedOnRecv` / `BlockedOnNotif` / `BlockedOnSend` wrappers (~`scheduler.rs:3853`–`4268`) and the futex waiter
**Why it matters:** The lost-wake recurrences (89/90b/06-14/95) were per-site patches; the model is only as correct as its least-conformant wait site, so a uniform-conformance audit is what actually retires the bug class.

**Acceptance:**
- [ ] An audit note enumerates every `block_current_until` caller with: the `woken` flag it registers, where it rechecks its condition after the state write, and confirmation that no latched per-site flag survives a block call.
- [ ] Each caller is confirmed to pass a **fresh** stack-local `AtomicBool` (no carried-over flag), matching the v1-flag-deletion invariant documented at `scheduler.rs:114`–`118`.
- [ ] Any non-conformant site found is fixed; if all sites conform, the audit records that explicitly (negative result is a valid deliverable).

### A.2 — Consolidate per-site wake wrappers into one documented pattern

**File:** `kernel/src/task/scheduler.rs`
**Symbol:** the `block_on_*` wrapper helpers around `block_current_until`; the "Bug #6 brackets" / stack-local-`AtomicBool` comments (~`scheduler.rs:2356`–`2374`, `4112`–`4161`)
**Why it matters:** The bracketing comments and stack-local wrappers are N independent special cases that read as folklore; collapsing them to one referenced pattern prevents the next wait site from reinventing a subtly-wrong variant.

**Acceptance:**
- [ ] The recv/reply/notif/send wrappers share one documented helper shape (or a single doc-comment block they all reference) describing the register-flag → recheck → block sequence.
- [ ] No behavioral change to the wake path: `smoke-test` + `regression` stay green after the refactor.

### A.3 — Periodic / on-demand scheduler-state diagnostic

**File:** `kernel/src/task/scheduler.rs`
**Symbol:** `dump_scheduler_state` (new function), wired into the existing stale-ready / stuck-no-waker watchdog (~`scheduler.rs:2519`)
**Why it matters:** The 2026-04-25 handoff's recommendation #3 — the missing tool that turns "task stuck in Blocked forever" into direct evidence at the moment of hang, which would have shortened prior lost-wake debugging by hours.

**Acceptance:**
- [ ] `dump_scheduler_state` prints one `[sched] task pid=X state=Y wake_deadline=Z on_cpu=W` line per task.
- [ ] It fires on the stuck-no-waker watchdog verdict (and optionally every N seconds behind a debug flag), without acquiring `pi_lock` in an order that violates the `pi_lock`-outer / `SCHEDULER.lock`-inner invariant.
- [ ] The diagnostic is ISR-safe / lock-aware (no panic if a lock is already held — mirrors the existing `try_lock_scheduler` usage).

### A.4 — Raise `smp-smoke` to `-smp 8`

**File:** `xtask/src/main.rs`
**Symbol:** `cmd_smp_smoke` (the `cores` default, ~`main.rs:18535`); `qemu_smp_count` (~`main.rs:5191`)
**Why it matters:** The laptop is 8-core; the futex WAIT/WAKE handshake must be proven at that core count, not `-smp 4`, before the bare-metal GUI arc relies on multi-core.

**Acceptance:**
- [ ] `cmd_smp_smoke`'s default core count is raised from 4 to **8** (still `M3OS_SMP=<N≥2>`-overridable for CI's 2-vCPU runners).
- [ ] `smp-smoke` PASSES at `-smp 8`: `SMP_STRESS_OK 256` prints, with no `KERNEL PANIC`, `RECURSIVE KERNEL PAGE FAULT`, `process killed`, or `BlockedOnFutex … no waker registered` watchdog verdict.
- [ ] The `AGENTS.md` `M3OS_SMP_REGRESSION` row notes the new `-smp 8` default.

### A.5 — Futex model conformance (`REQUEUE` / `CMP_REQUEUE` + per-AS keys)

**File:** `kernel/src/arch/x86_64/syscall/mod.rs`
**Symbol:** the futex op handler — `FUTEX_REQUEUE` / `FUTEX_CMP_REQUEUE` (~`mod.rs:19747`–`20026`) and the per-address-space futex-key path
**Why it matters:** Phase 89/90b patched these per-incident; confirming a requeued cond-waiter is woken by a CAS on its state (never by a flag the requeue must coordinate) folds them into the audited model.

**Acceptance:**
- [ ] The audit (A.1) covers the futex waiter and `REQUEUE`/`CMP_REQUEUE` requeue path, confirming requeued waiters wake via `wake_task_v2`'s state CAS.
- [ ] `node-smoke`'s always-on `NODE_EGRESS_OK` libuv-threadpool-condvar arm and `smp-smoke` (which both exercise `pthread_cond` requeue) stay green at `-smp 8`.

---

## Track B — Fault-Handling Robustness Audit

> The 2026-06-14 handoff landed the *recovery* (NMI-on-IST, mark-core-offline, degrade-shootdown, `fault_kill_trampoline`) and explicitly deferred the *origin audit*. Track B finishes that deferred work.

### B.1 — Locks-held-across-faults audit + assertion

**File:** `kernel/src/arch/x86_64/interrupts.rs`
**Symbols:** `page_fault_handler` (~`interrupts.rs:1539`), `general_protection_fault_handler` (~`:1970`), `double_fault_handler` (~`:2202`), `fault_kill_trampoline` (~`:303`), the `PROCESS_TABLE.lock()` / `try_lock_scheduler` sites (~`:376`, `:1642`, `:1784`)
**Why it matters:** Holding `SCHEDULER`/`PROCESS_TABLE` across a fault-prone or blocking operation is the `2026-04-21-scheduler-lock-isr-deadlock` class; the `MAP_LAZY_FILE` branch already documents the correct discipline (take `PROCESS_TABLE` before `shared_vma_demand_file` re-takes it; never strand it across the blocking IPC) and the audit codifies it.

**Acceptance:**
- [ ] A documented sweep confirms no fault handler holds `SCHEDULER` or `PROCESS_TABLE` across the blocking `MAP_LAZY_FILE` `vfs_server` read or across `fault_kill_trampoline`.
- [ ] A debug assertion (or a documented invariant comment with a `debug_assert!`) enforces "no scheduler/process-table lock held on entry to the blocking demand-fault IPC."
- [ ] `cargo xtask check` passes with the assertion compiled in (debug builds).

### B.2 — Pin the kstack-overflow origin

**File:** `kernel/src/arch/x86_64/interrupts.rs`, `kernel/src/task/kstack.rs`
**Symbol:** `try_recover_kstack_overflow` (`interrupts.rs:214`), the kstack guard-page classifier, the 64 KiB usable + 4 KiB guard sizing (`kstack.rs`)
**Why it matters:** 2026-06-14 Open Question #1 — the exact call chain that exhausts 64 KiB under V8/PKU churn was unrecoverable from the corrupted pre-IST frame; with NMI-on-IST it is now capturable, and pinning it tells us whether to bound a path or accept 64 KiB.

**Acceptance:**
- [ ] An IST-captured clean frame from a multi-core repro (or the existing `kstack-overflow-smoke` probe) is used to name the deepest contributing kernel call chain.
- [ ] Either the offending path's depth is bounded (documented), or a note records that 64 KiB is sufficient given the `fault_kill_trampoline` recovery, with the measured worst-case depth.

### B.3 — Recovery-stack & recursion-latch correctness review

**File:** `kernel/src/arch/x86_64/interrupts.rs`
**Symbol:** `FAULT_RECOVERY_STACKS` (`:191`), `IN_KERNEL_PAGE_FAULT` (`:271`), `recovery_stack_top_for_core` (~`:200`)
**Why it matters:** The recovery path must use a **per-core** stack and clear the recursion latch only when genuinely recovering (not cascading), or a recovered fault can corrupt a sibling core's recovery frame or mask a real recursion.

**Acceptance:**
- [ ] Confirmed: each core uses its own `FAULT_RECOVERY_STACKS[core]` slot; `try_recover_kstack_overflow` only redirects when `current_pid() != 0` (task-attributable).
- [ ] Confirmed: `IN_KERNEL_PAGE_FAULT[core]` is cleared on the recovery path (we are recovering, not cascading) and not on a genuine recursive cascade.
- [ ] `kstack-overflow-smoke` (`M3OS_KSTACK_OVERFLOW_REGRESSION`) and `dynamic-hello-smoke` `THREAD_FAULT` (`leader-ok` / `worker-ok`) arms stay green.

---

## Track C — 4 GiB SMP Panic-Path AP-Quiesce

### C.1 — Quiesce sibling cores before printing the panic banner

**File:** `kernel/src/lib.rs` (and `kernel/src/main.rs` `#[panic_handler]`)
**Symbol:** `handle_panic` (`lib.rs:1272`); reuse `smp::try_per_core` / the `is_online` marking already added to `hlt_loop`
**Why it matters:** The 2026-06-05 handoff's blocking ask — at 4 GiB + `--kvm` + SMP the panic banner is unreadable because other cores keep writing COM1 during the print/dump; a legible banner is the prerequisite for root-causing the residual instability.

**Acceptance:**
- [ ] `handle_panic` broadcasts a halt IPI/NMI to sibling cores and spins a bounded grace window for them to stop before printing the banner + trace-ring dump.
- [ ] A captured 4 GiB panic shows an **uninterleaved**, readable banner + `=== CRASH DIAGNOSTICS ===` block (no SMP byte-interleave garbage).
- [ ] The quiesce is bounded (does not itself hang if a core is already wedged) and does not regress the single-core panic path used by the test harness ISA-debug-exit.

### C.2 — Residual 4 GiB OOM/race investigation pass

**File:** `kernel/src/smp/tlb.rs`, `kernel/src/task/scheduler.rs` (stale-ready watchdog), `kernel/src/trace.rs` (`dump_trace_rings`, `trace.rs:55`)
**Symbol:** the 4 GiB-only crash class (cross-referenced from `2026-05-24-4gib-pci-hole-vga-mapping.md`)
**Why it matters:** With a readable banner (C.1), one diagnostic pass at the residual >2 GiB instability can either close it or record a precise next-step, rather than leaving it permanently masked by the 2 GiB workaround.

**Acceptance:**
- [ ] At least one 4 GiB + `--kvm` + `-smp 8` boot/stress run is captured with the C.1 readable banner; the panic site (if it fires) is symbolized and recorded.
- [ ] Either the residual race is fixed, or a handoff records the symbolized panic site + a concrete hypothesis (closing the diagnosability gap the 2026-06-05 handoff opened).

---

## Track D — Step-25 Demand-Fault NULL-Deref Flake

### D.1 — Capture + symbolize a red step-25 run

**File:** `.github/workflows/pr.yml` (artifact upload already in place), `kernel/src/arch/x86_64/interrupts.rs`
**Symbol:** `dynlink-hello-versioned-mismatch-smoke` step 25; the crash `rip` / PIE load base from `target/ci-crash/smoke-test.log`
**Why it matters:** The flake is host-correlated, low-rate (~11–15 %), and does not reproduce locally; the kernel ELF is not bit-reproducible, so the crash can only be symbolized against CI's own uploaded ELF.

**Acceptance:**
- [ ] A red step-25 run is captured via `gh run download <run-id> -n pr-regression-artifacts`.
- [ ] `addr2line -fiCe <CI-built kernel> <rip − load_base>` resolves the faulting function (and trace-ring addresses) against the **uploaded** ELF — not a locally-built kernel.
- [ ] The verdict (NULL deref vs. true stack-depth) is recorded; the 2026-06-25 hypothesis (single primary fault, varying secondary manifestation) is confirmed or refuted.

### D.2 — Root-cause + fix the `cr2=0` NULL deref in the `MAP_LAZY_FILE` chain

**File:** `kernel/src/arch/x86_64/interrupts.rs` (`page_fault_handler` `MAP_LAZY_FILE` branch, ~`:906`/`:937`/`:943`), `kernel/src/process/mod.rs` (`shared_vma_demand_file`, `mod.rs:1308`)
**Symbol:** `shared_vma_demand_file`, `demand_map_user_page_from_buf_locked`, the blocking `vfs_server` read issued from the fault handler
**Why it matters:** This path demand-pages every multi-hundred-MB toolchain DSO (rustc/node/clang) and is exercised far harder on the slow bare-metal VFS, so a 1-in-7 kernel fault is unacceptable for the GUI arc.

**Acceptance:**
- [ ] The NULL deref (kernel-mode read of address 0x0 in the demand-fault chain) is root-caused to a specific unchecked pointer / `Option` / lock-state and fixed.
- [ ] The fix is grounded in the symbolized faulting function from D.1 (not a speculative kstack bump — the reverted 64→96 KiB experiment is explicitly not the fix).

### D.3 — N-iteration soak proving flake = 0

**File:** `xtask/src/main.rs` (the `dynlink-hello-versioned-mismatch-smoke` step), CI run records
**Symbol:** the step-25 smoke step
**Why it matters:** A flake this rate only proves fixed by a soak, not a single green run.

**Acceptance:**
- [ ] Step 25 passes **N≥50** consecutive iterations (mixed CI runners where possible) with 0 kernel faults — recorded run IDs / log.
- [ ] `smoke-test` overall stays green; no new flake is introduced elsewhere.

---

## Track E — Two Correctness Bugs

### E.1 — `copy_file_range` / `sendfile` → clean result or `-ENOSYS` (no EFAULT)

**Files:**
- `kernel/src/arch/x86_64/syscall/mod.rs` (the default dispatch arm returning `NEG_ENOSYS`, ~`mod.rs:2502`)
- `kernel/src/arch/x86_64/syscall/fs.rs` (new handlers if implemented)

**Symbol:** `copy_file_range` (syscall 326), `sendfile` (syscall 40) — neither has a handler today; `NEG_ENOSYS`
**Why it matters:** Node's `fs.copyFile` surfaces `EFAULT` instead of a working copy or a clean `ENOSYS`-fallback; the bad errno originates downstream of the (currently unhandled) `copy_file_range`/`sendfile` probe and must be tracked to its source.

**Acceptance:**
- [ ] The origin of the EFAULT in node's `fs.copyFile` path is identified (a buffer-validation step in libuv's read/write fallback, or a number collision, or a partial stub).
- [ ] Either `copy_file_range`/`sendfile` are implemented correctly **or** they return a clean `-ENOSYS` so node's userspace fallback succeeds; in no case does `fs.copyFile` return `EFAULT`.
- [ ] A node `fs.copyFile` probe (a few KiB file copied + byte-verified) succeeds on m3OS; recorded as a one-off check or folded into `node-smoke`.

### E.2 — 55c `net::remote` RX-test encoder fix

**Files:**
- `kernel/src/net/remote.rs` (the `#[test_case]` RX-path tests, ~`remote.rs:920`–`990`; `InvalidFrame` reject at `remote.rs:365`)
- `kernel-core/src/driver_ipc/net.rs` (`encode_header_with_kind` `:235`, `encode_net_rx_notify` `:294`, `encode_net_send_frame` / `NET_SEND_FRAME`)

**Symbol:** `encode_net_rx_notify` (ingress, `NET_RX_FRAME`) vs `encode_header_with_kind(NET_SEND_FRAME, …)` (egress)
**Why it matters:** A Phase-55c-era RX-path test that encodes its fixture with the egress `NET_SEND_FRAME` kind decodes as `InvalidFrame`, so the test asserts the wrong outcome and gives no real coverage of the RX decode path.

**Acceptance:**
- [ ] The RX-path test(s) encode their fixture with the ingress `NET_RX_FRAME` header via `encode_net_rx_notify` (the egress `encode_header_with_kind`/`NET_SEND_FRAME` use is removed from the RX path).
- [ ] The test exercises the real `process_rx_frames` decode path and passes without tripping `NetDriverError::InvalidFrame` for a well-formed RX frame.
- [ ] `cargo test -p kernel-core` host tests and the `net::remote` kernel test suite pass.

---

## Documentation Notes

- The single-state-word block/wake model (`block_current_until`/`wake_task_v2`, with `switching_out`/`wake_after_switch`/`PENDING_SWITCH_OUT` deleted) **already landed in Phase 57a** — record that Phase 99 *completes, consolidates, and validates-at-`-smp 8`* that model rather than introducing it, so future readers don't re-walk the rewrite.
- Note that the four prior lost-wake fixes (Phase 89 `FUTEX_REQUEUE`, Phase 90b per-AS futex keys + cross-thread PKU read-recovery, the 2026-06-14 cross-core lost-wake, Phase 95 rustc futex) are subsumed by the A.1 audit + the uniform A.2 pattern.
- Cross-reference the three open handoffs this phase closes/advances: `docs/handoffs/2026-06-14-claude-smp-tlb-shootdown-kstack-panic.md` (Track D origin audit → Track B here), `docs/handoffs/2026-06-05-4gib-smp-panic-corrupted-output.md` (panic AP-quiesce → Track C here), `docs/handoffs/2026-06-25-flaky-dynlink-mismatch-demand-fault-kernel-fault.md` (step-25 flake → Track D here), and the design source `docs/handoffs/2026-04-25-scheduler-design-comparison.md`.
- This phase is the **gating prerequisite for Phase 100** (Bare-Metal GUI Session, Dell) per the Phase 98 charter dependency graph — the laptop is 8-core and cannot pin `-smp 1`; keep that edge consistent in `docs/roadmap/README.md`'s next-arc section and mermaid (`P99 --> P100`).
- Prefer exact files/symbols over directories as these tasks land; update the checkboxes and the Track Layout status column as tracks complete.
- Mark the design doc + this task doc Status `Complete` only when `smp-smoke` PASSES at `-smp 8`, the step-25 soak shows flake=0, and the two Track-E bugs are fixed with their gates green.
