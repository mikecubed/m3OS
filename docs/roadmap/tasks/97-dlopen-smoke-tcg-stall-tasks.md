# Phase 97 — `dlopen-test-smoke` Intermittent TCG Stall: Task List

**Status:** Complete (landed in PR #268; kernel `v0.97.0`)
**Source Ref:** phase-97
**Depends on:** Phase 95b ✅ (`MAP_LAZY_FILE` demand-paged loader + blocking page-fault→`vfs_server` read), the SMP TLB-shootdown / lost-wakeup hardening ✅ (`docs/handoffs/2026-06-14-claude-smp-tlb-shootdown-kstack-panic.md`)
**Goal:** Get observability on the intermittent `smoke-test` step-26 (`dlopen-test-smoke`) failure, **confirm** the real cause (the handoff's blocking-`vfs` lost-wakeup hypothesis is falsified at the artifact level — all DSOs are ramdisk-embedded/synchronous; the leading surviving suspect is the cross-core TLB shootdown `dlclose`'s `munmap` runs twice on the `FINI`→`PASS` path under `-smp 4` TCG oversubscription), land only the confirmed fix plus a couple of always-safe hardenings, and replace the observability-blind PASS-or-SKIP gate with a falsifiable, CI-deterministic one.

## Outcome (2026-06-27)

**Confirmed root cause: the `ld-musl` loader had no `DT_RELR` support.**
`libhello_fini.so`'s only relocation (its `DT_FINI_ARRAY` destructor pointer) is
`DT_RELR`-encoded (`RELASZ: 0`, `RELR: 0x1070`), so the destructor pointer was
never relocated → `dlclose` → `run_destructors_for` jumped to the unrelocated
in-file vaddr `0x2a0` → near-NULL `INSTRUCTION_FETCH` → `process killed`. The
"intermittent TCG stall" framing in the design doc's hypotheses is **refuted**:
both the blocking-`vfs` and the cross-core TLB-shootdown hypotheses produced
**zero** corroborating evidence (no `[tlb]` lines), and the failure is a
*toolchain-dependent deterministic* fault (modern linkers emit `.relr.dyn`,
older emit `.rela.dyn` which the loader already handled) that *looked* like a
stall only because the gate had no FAIL pattern. See the design doc's
**Investigation Findings** for the full verdict.

**Fix:** `DT_RELR` decode (`reloc::apply_relr`, host-tested) wired into the `Dyn`
parser and all three relocation sites; the `dlopen-test-smoke` gate switched to
`WaitPassOrFail` (real FAIL pattern) and the kernel-fatal scan was hoisted into
every `run_smoke_script` wait arm.

### Per-task resolution

The tasks below were written under the (now refuted) shootdown/`vfs` hypotheses,
so several are **N/A**. The original task text is kept verbatim as the planning
record; this list is the authoritative status.

| Task | Status | Notes |
|---|---|---|
| **A.1** repro harness | ✅ Done | `cargo xtask dlopen-repro` (`-smp 4` default, `M3OS_DLOPEN_ITERS`); classifies PASS vs the destructor fault by fail-fast rather than a 3-way bucket (unnecessary once the fault was identified as deterministic). Counts recorded in the design doc. |
| **A.2** base-rate ≥200× | ⛔ N/A | The bug is *deterministic* (DT_RELR ignored → 100 % fault under load on any RELR-emitting linker), not probabilistic — there is no rate to measure. Reproduced 100 % on iteration 1 every run (and on 2 consecutive baseline-smoke attempts). |
| **A.3** backend Ramdisk-vs-VfsService log | ⛔ N/A | Confirmed at the code level (`kernel_read_fd_at` `FdBackend::Ramdisk` is a synchronous memcpy); a runtime log was moot once the cause was pinned to a userspace relocation bug, not the demand-fill backend. |
| **A.4** shootdown instrumentation | ⛔ N/A | Shootdown refuted: the reproduced dump had **zero** `[tlb]` lines (the always-on `wait_for_shootdown_acks` degrade/ack-timeout logging would have printed them). No debug-gated instrumentation needed. |
| **A.5** watchdog / deadlock-guard verdict | ✅ Done | Dump grepped: `no waker registered` = 0, `[deadlock-guard]` = 0 (budget not exhausted); the one `[stallcensus]` line was a benign timed-sleep daemon. Recorded — consistent with a userspace fault, not a wedge. |
| **A.6** `-smp 1` vs `-smp 4` | ⛔ N/A | The confirmed cause (DT_RELR, a userspace relocation bug) is core-count-independent, so the cross-core discriminator is moot. The repro harness supports `M3OS_SMP=1` if ever needed. |
| **B.1** verdict | ✅ Done | Written in the design doc's Investigation Findings: shootdown + `vfs` **excluded** (no evidence); DT_RELR **confirmed** with `readelf` + the reproduced dump. |
| **B.2** minimal fix | ✅ Done | The fix is DT_RELR loader support (not the speculated shootdown fix). `cargo xtask check` + `smoke-test` + `test` + `regression` all green. |
| **B.3** global fatal-pattern hoist | ✅ Done | `global_fatal_line` (`KERNEL PANIC` / `RECURSIVE KERNEL PAGE FAULT` / `no waker registered`) checked in every `run_smoke_script` wait arm; `process killed` stays per-step (negative tests fork-and-kill). |
| **B.4** lazy-file / `vfs` hardenings | ⛔ N/A | Targeted the refuted lazy-file/`vfs` hypotheses; the real bug is a userspace reloc miss, so adding speculative kernel hardenings would violate "minimal confirmed fix". Noted in the design doc's Deferred Until Later. |
| **B.5** record kernel demand-read deferral | ✅ Done | Recorded in the design doc's Deferred Until Later (and B.4 noted unnecessary). |
| **C.1** FAIL pattern + ordering | ✅ Done (lighter form) | Gate → `WaitPassOrFail` on the runner's existing `SMOKE:dlopen-test-smoke:FAIL` verdict; the `FINI_PENDING < RAN < PASS` ordering assert is preserved guest-side in `run_command_expect_dlopen_order` (unchanged), so no `smoke-runner` serial-direct rewrite was needed. The real (pre-fix) bug *did* make the runner emit FAIL, which the new gate catches fast. |
| **C.2** COM1-RX guard | ⛔ N/A | The chosen (lighter) gate does not host-inject the dlopen command — the `smoke-runner` forks/execs it internally — so there is no injected command to RX-guard. |
| **C.3** TCG posture | ✅ Done | Decided **always-on under TCG** (no KVM-gate): the cause is a deterministic DT_RELR miss, not an irreducible TCG-latency tail, so the fix is binary. Recorded in the design doc. |
| **C.4** controlled-load soak | ✅ Done (deterministic-bug form) | The "20/20" target was framed for a probabilistic flake; the cause is deterministic, so a single green full `smoke-test` on the **original** `fork`+`dup2(capture)`+`waitpid` path + the 232-iter `dlopen-repro` soak + the host-tested decoder are conclusive. Bug shown reproduced (pre-fix baseline smoke, original path) then fixed (post-fix smoke, original path). |
| **D.1** docs | ✅ Done | Design + task docs authored and kept current. |
| **D.2** README row | ✅ Done | Repointed to the phase docs; status updated. |
| **D.3** version bump | ✅ Done | `AGENTS.md` kernel `v0.96.0` → `v0.97.0`. |
| **D.4** handoff cross-link | ✅ Done | The 2026-06-26 handoff points forward to the phase docs and flags its hypothesis as falsified; Phase 98's audit folding-in is a forward reference (out of this phase's scope). |
| **D.5** learning doc | ✅ Done | `docs/97-dlopen-smoke-tcg-stall.md` authored to the learning-doc template; README row links it. (This task was missing from the original Track D and added here.) |

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Observability & reproduction (standalone repro, instrumentation, discriminators) | — | Done (repro built; `readelf` + serial dump confirmed the `DT_RELR` cause; shootdown/`vfs` instrumentation N/A) |
| B | Root-cause fix + honest-gate fail-fast | A | Done (`DT_RELR` loader support; gate FAIL pattern. Shootdown fix B.2 / lazy-file hardenings B.4 N/A — refuted) |
| C | Honest, CI-deterministic regression gate (FAIL pattern + soak) | A, B | Done (dlopen step → `WaitPassOrFail`; soaked via `dlopen-repro`) |
| D | Docs & version bump | — | Done (findings recorded; kernel `v0.96.0`→`v0.97.0`) |

---

## Track A — Observability & Reproduction

### A.1 — Looped standalone `dlopen_test` repro that buckets each run

**Files:**
- `xtask/src/main.rs` (`cmd_run` / a new `cmd_dlopen_repro` helper)
- `userspace/dlopen_test/dlopen_test.c`

**Symbol:** `cmd_run` (reused) / new repro loop; `dlopen_test::_start` sentinels (`puts1`, `dlopen_test.c:114`)
**Why it matters:** A live `cargo xtask run` at the smoke default `-smp 4` streams `dlopen_test`'s sentinels directly to serial (the gate hides them behind a tmpfs capture file), so a loop of N runs is the single cheapest way to split *slow-PASS* / *partial-then-silent* / *fault* — the data point the handoff never collected.

**Acceptance:**
- [ ] A documented procedure (script or `xtask` subcommand) boots `cargo xtask run` at `-smp 4` TCG, autoruns `/bin/dlopen_test` ≥50×, tees serial, and classifies each run as slow-PASS / partial-then-silent / faulted.
- [ ] The per-bucket counts from a real run are recorded in `docs/roadmap/97-dlopen-smoke-tcg-stall.md` (or a linked handoff).
- [ ] The repro keeps `-smp 4` by default (does **not** force `M3OS_SMP=1`), preserving the multi-core window.

### A.2 — Measure the unmodified gate's base failure rate under controlled load

**File:** `xtask/src/main.rs`
**Symbol:** `cmd_smoke_test` / `smoke_test_script` (run unmodified)
**Why it matters:** An intermittent bug needs a *measured* base rate to power the post-fix soak — "50× pass" against an unknown (possibly 1-in-200) rate is statistically meaningless (rule-of-three bounds the rate only at ~3/N).

**Acceptance:**
- [ ] The unmodified `smoke-test` is run ≥200× at `-smp 4` on a deliberately oversubscribed host (e.g. pinned `stress-ng` / parallel builds), and the observed step-26 failure rate is recorded.
- [ ] The measurement environment (host core count, concurrent load, KVM off) is documented so the soak in C.4 reproduces it.

### A.3 — Prove the demand-fill backend at runtime (Ramdisk vs VfsService)

**File:** `kernel/src/arch/x86_64/syscall/mod.rs`
**Symbol:** `kernel_read_fd_at` (the `FdBackend::Ramdisk` arm ~`12098` vs the `FdBackend::VfsService` arm ~`12162`); `open` fd-creation log (~`10424`)
**Why it matters:** Empirically closing (or reopening) the falsified blocking-`vfs` hypothesis — if every `dlopen_test` fd is `Ramdisk`, the synchronous-memcpy fill is confirmed and the investigation is correctly redirected to the `munmap` shootdown.

**Acceptance:**
- [ ] A rate-limited / one-shot log line distinguishes the Ramdisk arm from the VfsService arm for each demand fill, gated behind a debug build/feature so it does not spam production boots.
- [ ] A captured `dlopen_test` run shows **100%** Ramdisk for its dependency graph (or names the surprise `VfsService` fd if not).

### A.4 — Instrument the `dlclose` `munmap` cross-core TLB shootdown

**Files:**
- `kernel/src/smp/tlb.rs`
- `kernel/src/arch/x86_64/syscall/mod.rs` (`sys_linux_munmap` ~`12820`, the `tlb_shootdown_range` call ~`13006`)

**Symbol:** `tlb_shootdown_range`, `wait_for_shootdown_acks` (`tlb.rs:130`), `AddressSpace::active_cores`
**Why it matters:** The leading surviving hypothesis is that `dlclose`'s two munmaps (`unmap_dso` → `sys_munmap`, `dl.rs:670`) broadcast NMIs to remote cores and the sender spins/degrades in `wait_for_shootdown_acks` under TCG oversubscription — this instrumentation confirms or excludes it with data (target mask, spin time, any offline-mark).

**Acceptance:**
- [ ] `tlb_shootdown_range` (debug-gated) logs, for the `dlopen_test` child, the remote target mask and the `wait_for_shootdown_acks` spin duration (and any re-NMI / mark-offline event).
- [ ] A reproduced stall's capture is annotated with whether the munmap shootdown targeted zero remote cores or spun/degraded, and the result is recorded against the hypothesis.

### A.5 — Capture the watchdog & deadlock-guard verdict during a stall

**Files:**
- `kernel/src/task/scheduler.rs` (`watchdog_scan` / the `(no waker registered)` line ~`6602`)
- `kernel/src/arch/x86_64/interrupts.rs` (`[deadlock-guard] demand-fault under lock` ~`925`)

**Symbol:** `watchdog_scan`; the deadlock-guard branch in `demand_map_vma_page`
**Why it matters:** Presence of `(no waker registered)` (a `BlockedOnReply` stranded ~30s) points at an IPC/lost-wakeup wedge; presence of the deadlock-guard line is a smoking gun for a lazy-file-fault-under-lock; **absence of both** (the watchdog is log-only and exempts `BlockedOnRecv`-no-deadline) is consistent with a silent shootdown/oversubscription wedge — the three cases map to different hypotheses.

**Acceptance:**
- [ ] A reproduced stall is captured with `M3OS_SMOKE_SERIAL_DUMP` (or the standalone serial tee) and grepped for both markers; presence/absence is recorded.
- [ ] The `DEADLOCK_GUARD_BUDGET` (capped at 64, `interrupts.rs:913`) is confirmed not exhausted by benign early-boot faults before step 26.

### A.6 — `-smp 1` vs `-smp 4` discriminator

**File:** `xtask/src/main.rs` (`M3OS_SMP` plumbing, `qemu_smp_count`)
**Symbol:** `qemu_smp_count`
**Why it matters:** A clean, decisive split: every cross-core hypothesis (munmap shootdown — whose remote mask collapses to 0 single-core via the `tlb.rs:313` fast path — and any cross-core lost-wakeup) requires the stall to **vanish** at `-smp 1`; persistence single-core would refute them and point at same-core logic/latency.

**Acceptance:**
- [ ] The repro is run at `M3OS_SMP=1` and at `-smp 4` under the same load; whether the stall is multi-core-only is recorded.
- [ ] The result is treated as a gate on the Track B root-cause conclusion (a cross-core fix is not committed unless `-smp 1` is clean).

---

## Track B — Root-Cause Fix (keyed to Area A's confirmed cause) + always-safe hardenings

### B.1 — Confirm or refute the munmap-shootdown hypothesis; record the verdict

**Files:**
- `kernel/src/smp/tlb.rs`
- `docs/roadmap/97-dlopen-smoke-tcg-stall.md`

**Symbol:** `tlb_shootdown_range` / `wait_for_shootdown_acks`
**Why it matters:** No fix is committed against an unconfirmed mechanism — the synthesis missed this path and the critique only added it, so it must be proven from Area A's data before a patch.

**Acceptance:**
- [ ] A written verdict (in the design doc) states whether the `dlclose` munmap shootdown is the confirmed cause, a contributor, or excluded, citing the A.4/A.6 data.
- [ ] If excluded, the next-ranked hypothesis (TCG oversubscription latency, a cross-core lost-wakeup in `fork`/`exec`/`waitpid`, a concurrent lazy-file-fault-under-lock, or a harness/budget confound) is named with its confirming/refuting evidence.

### B.2 — Apply the minimal fix for the confirmed cause

**Files:**
- `kernel/src/smp/tlb.rs` (if the shootdown is confirmed — e.g. coalesce the `dlclose`-path range shootdowns, or skip the broadcast when the range was never shared to a remote core)
- `xtask/src/main.rs` (if the cause is TCG oversubscription posture — e.g. pin the step to a lower `-smp` under TCG)

**Symbol:** `tlb_shootdown_range` / `AddressSpace::active_cores` / `qemu_smp_count`
**Why it matters:** The fix must target what A confirmed, not the falsified `vfs` path; for the shootdown case the lever is reducing or bounding the cross-core handshake on the unmap path (or the oversubscription that makes it wall-clock-unbounded under TCG).

**Acceptance:**
- [ ] The change addresses the **confirmed** manifestation(s); if a latency tail remains, it is explicitly labeled posture (C.3), not fix.
- [ ] `cargo xtask check` passes (clippy `-D warnings` + rustfmt + host tests), and the relevant always-on SMP gates (`smp-smoke`, `dynamic-hello-smoke`) still pass.

### B.3 — Hoist the fatal-pattern fail-fast into `run_smoke_script`

**File:** `xtask/src/main.rs`
**Symbol:** `run_smoke_script` (the `WaitEither`/`Wait` executor, ~`7736`); reuse the pattern set from `cmd_smp_smoke` (~`18329`)
**Why it matters:** Today every always-on step that doesn't match its sentinel collapses into one opaque 120s timeout; hoisting the existing `KERNEL PANIC` / `no waker registered` / `RECURSIVE KERNEL PAGE FAULT` / `process killed` scanner makes *every* step fail fast with a named cause — the cheapest honest-gate win and pure harness change.

**Acceptance:**
- [ ] `run_smoke_script` aborts immediately with the matched fatal line when any of the shared fatal patterns appears during any step.
- [ ] The dlopen step (and others) report the named fatal cause instead of a generic timeout when a panic/wedge occurs.

### B.4 — Always-safe hardening: lazy-file-fault-under-lock prevention + `vfs_server` self-read guard

**Files:**
- `kernel/src/arch/x86_64/interrupts.rs` (the deadlock-guard, ~`905-934`)
- `kernel/src/mm/user_mem.rs` (`copy_from_user` / `copy_to_user` ~`200-215`)
- `kernel/src/arch/x86_64/syscall/mod.rs` (`vfs_service_read_kernel` ~`9495`; the `vfs_write_routable` self-guard exemplar ~`9562`)

**Symbol:** a new `pre_fault_user_range` helper; `vfs_service_read_kernel`; `is_current_exec_path`
**Why it matters:** The log-only deadlock-guard has twice caught a real wedge class (Phase 95b worked it around ad-hoc at `sys_rt_sigprocmask` `mod.rs:3840` and futex `mod.rs:19833`); turning detection into prevention at `copy_*_user`-under-lock sites, and adding the `/bin/vfs_server` self-read guard the write path already has, eliminates a real and a latent all-core wedge regardless of which hypothesis A confirms.

**Acceptance:**
- [ ] A targeted test that faults a `vfs`-backed user page under a kernel `IrqSafeMutex` no longer wedges all cores (the page is pre-faulted before the lock is taken).
- [ ] The `VfsService` demand-read arm rejects a self-`call_msg` from `/bin/vfs_server` (`is_current_exec_path` guard present), verified by a host/unit test or an asserted log — closing the latent self-deadlock for a future dynamic `vfs_server`.

### B.5 — Record the explicit deferral of the kernel demand-read timeout / watchdog-kill

**File:** `docs/roadmap/97-dlopen-smoke-tcg-stall.md` (Deferred Until Later)
**Symbol:** `vfs_service_read_kernel` `call_msg` (no timeout, `mod.rs:9454/9511`); `watchdog_scan`
**Why it matters:** Bounding the demand-read `call_msg` and making the watchdog kill-capable targets the genuinely `vfs`-backed gates (`rustc`/`clang`/install), **not** this gate's ramdisk-synchronous path — it must be explicitly out of scope so a future contributor doesn't mistake it for the dlopen fix.

**Acceptance:**
- [ ] The deferral and its rationale (wrong target for this gate's ramdisk path) are written into the design doc's Deferred Until Later.

---

## Track C — Honest, CI-Deterministic Regression Gate

### C.1 — Serial-direct `dlopen_test` step with a real FAIL pattern + ordering assert

**Files:**
- `xtask/src/main.rs` (the `dlopen-test-smoke` `WaitEither` ~`8365`; `SmokeStep::WaitPassOrFail`)
- `userspace/smoke-runner/src/main.rs` (`run_command_capture` / `run_command_expect_dlopen_order` ~`740-799`)

**Symbol:** `smoke_test_script` (the dlopen step); `WaitPassOrFail`
**Why it matters:** The current PASS-or-SKIP matcher hides the child's progress in an unlinked tmpfs file, so slow / stall / fault are indistinguishable; a serial-direct sequence (`Send "/bin/dlopen_test"` → `Wait FINI_PENDING` → `Wait LIBHELLO_FINI:RAN` → `WaitPassOrFail{pass: DLOPEN_TEST:PASS, fail:[DLOPEN_TEST:FAIL, fault markers]}`) localizes exactly which sentinel a stall sits before and adds the FAIL alternative the matcher lacks.

**Acceptance:**
- [ ] The step matches each `dlopen_test` sentinel directly on serial, with a `[timing]` print bracketing each, and preserves the `FINI_PENDING < LIBHELLO_FINI:RAN < PASS` ordering assertion host-side.
- [ ] A deliberately broken `dlopen_test` (e.g. a forced bad `dlclose`) makes the gate **FAIL fast** naming the failing sentinel rather than timing out at 120s.

### C.2 — COM1-RX integrity guard for the injected command

**File:** `xtask/src/main.rs` (the new `Send`/`Wait` step from C.1)
**Symbol:** `SmokeStep::Send` / the new step's echo assertion
**Why it matters:** The `smp-smoke` doc names the COM1-RX-under-SMP byte-drop as a real regression class; a redesigned gate that *injects* a command over COM1 under `-smp 4` TCG adds that failure surface — a dropped byte garbles the command, the guest never runs the test, and "no output" gets misattributed to a kernel stall.

**Acceptance:**
- [ ] The injected-command step asserts the command was received verbatim (echo-back or a guest-side acknowledgement) before waiting on `dlopen_test` output, so a serial byte-drop fails with a *distinct* RX-integrity error, not a generic timeout.

### C.3 — Decide and wire the TCG posture

**File:** `xtask/src/main.rs` (`cmd_smoke_test` ~`9603`, `smoke_test_script(false)` ~`9654`; the `cmd_node_jit_smoke` KVM-gate exemplar ~`18774`)
**Symbol:** `smoke_test_script` (add a `kvm: bool` parameter); the dlopen step's posture branch
**Why it matters:** If Area A confirms an irreducible TCG-oversubscription latency tail, the established posture is KVM-gate-with-skip-with-reason (like `tls`/`pku`/`node-jit`) — but `smoke_test_script` currently takes no `kvm` flag, so the posture decision requires threading it through, not a one-liner.

**Acceptance:**
- [ ] The posture is decided (always-on-but-fixed, or KVM-gated skip-with-reason under TCG) and recorded in the design doc with the confirming evidence from Track A/B.
- [ ] If KVM-gated, `smoke_test_script` is given the `kvm` flag (`cmd_smoke_test` threads `smoke_args.kvm` in) and the TCG branch prints a skip-with-reason; a TCG **SKIP does not count as a green run** in the soak (C.4).

### C.4 — Controlled-load soak on the original execution path

**File:** `xtask/src/main.rs`
**Symbol:** the `dlopen-test-smoke` step (post-fix)
**Why it matters:** A green soak on an idle host proves nothing about an oversubscription-gated bug, and a green *redesigned* gate can pass merely by no longer exercising the trigger (the `fork`/`waitpid`/capture wrapper is itself in the suspect surface) — so the fix must be shown on the original path under the measured base-rate load.

**Acceptance:**
- [ ] **20/20** consecutive green TCG `smoke-test` runs on the A.2 oversubscribed host with no dlopen-step timeout, run count powered by the A.2 base rate.
- [ ] If the gate was redesigned, the bug is additionally shown reproduced-then-fixed on the **original** `fork()`+`dup2(capture)`+`waitpid()` path (not only the new serial-direct path).

---

## Track D — Docs & Version Bump

### D.1 — Author the Phase 97 design + task docs

**Files:**
- `docs/roadmap/97-dlopen-smoke-tcg-stall.md`
- `docs/roadmap/tasks/97-dlopen-smoke-tcg-stall-tasks.md`

**Symbol:** n/a (documentation)
**Why it matters:** Captures the `readelf`-verified falsification of the handoff's leading hypothesis and the reframed surviving-hypothesis set so the implementation work is grounded and no contributor re-chases the falsified `vfs` path.

**Acceptance:**
- [x] Design doc conforms to the phase-design template (all required sections through Deferred Until Later).
- [x] Task doc conforms to the phase-task template (Track Layout table + per-track tasks with File/Symbol/Why it matters/Acceptance).

### D.2 — Repoint the roadmap README row

**File:** `docs/roadmap/README.md` (the Phase 97 row, ~`494`)
**Symbol:** n/a (documentation)
**Why it matters:** The row's Milestone cell still points at the handoff and its Tasks cell is an em-dash; with the docs landed it must point at the phase docs and reflect In-Progress status.

**Acceptance:**
- [x] Status → **In Progress**; Milestone → `[Phase 97](./97-dlopen-smoke-tcg-stall.md)`; Tasks → `[Tasks](./tasks/97-dlopen-smoke-tcg-stall-tasks.md)`.

### D.3 — Kernel version bump when the fix lands

**File:** `AGENTS.md` (the `kernel **v0.96.0**` line, `AGENTS.md:7`)
**Symbol:** n/a (version string)
**Why it matters:** Per the AGENTS.md maintenance policy the kernel version is bumped **when a phase lands**; a debugging/fix phase bumps on the implementation PR, not on this planning-docs PR.

**Acceptance:**
- [ ] When Tracks A–C land, `AGENTS.md` is bumped `v0.96.0` → `v0.97.0` in the implementation PR (not in the docs PR).

### D.4 — Keep Phase 97 standalone; cross-link the handoff

**Files:**
- `docs/handoffs/2026-06-26-dlopen-smoke-tcg-stall.md`
- `docs/roadmap/README.md` (Phase 98 row notes it folds in Phase 97)

**Symbol:** n/a (documentation)
**Why it matters:** Phase 98 references folding in Phase 97 + the Phase 96 deferred items; Phase 97 is delivered standalone first, and the original handoff should point forward to the phase docs so its falsified hypothesis isn't taken at face value later.

**Acceptance:**
- [x] The 2026-06-26 handoff is annotated to point at the Phase 97 design doc and note its leading hypothesis was falsified.
- [ ] Phase 97 remains a standalone phase; Phase 98's audit folds in its deferred items.

### D.5 — Author the Phase 97 learning doc

**File:** `docs/97-dlopen-smoke-tcg-stall.md`

**Symbol:** n/a (documentation)
**Why it matters:** Every recent phase ships a `docs/NN-*.md` learning doc (90a–96), and a debugging phase's teaching value (observability-first falsification, the `DT_RELR` relocation format, why "intermittent" lied, honest-CI verdicts) is exactly the transferable lesson worth a standalone doc.

**Acceptance:**
- [x] `docs/97-dlopen-smoke-tcg-stall.md` exists, conforms to the learning-doc template (Overview / What This Doc Covers / Core Implementation / Key Files / How This Phase Differs / Related Roadmap Docs / Deferred), and teaches the `DT_RELR` root cause, the falsification of the SMP-race hypotheses, and the honest-gate change.
- [x] The roadmap README Phase 97 row links the learning doc.

---

## Documentation Notes

- The handoff (`docs/handoffs/2026-06-26-dlopen-smoke-tcg-stall.md`) named the blocking-`vfs_server` demand-read lost-wakeup as the leading hypothesis; Phase 97 **falsifies** it for this gate at the artifact level (`readelf` — all `dlopen_test` DSOs are ramdisk-embedded → synchronous in-kernel memcpy via `kernel_read_fd_at`'s `FdBackend::Ramdisk` arm, no `call_msg`, no parking). Do not re-chase the `vfs`/IPC-reply path for `dlopen-test-smoke`.
- **The cross-core TLB-shootdown hypothesis (below) was ALSO refuted** — see the
  design doc's Investigation Findings and the Outcome section above. The
  *confirmed* root cause is the loader's missing `DT_RELR` support, not any
  shootdown/`vfs`/lazy-file mechanism. The original note is kept (struck through
  in spirit) only as a record of the planning hypothesis:
  - ~~The leading **surviving** hypothesis is the cross-core TLB shootdown that `dlclose`'s `munmap` runs twice on the `FINI`→`PASS` path... under `-smp 4` TCG host oversubscription.~~ The reproduced serial dump contains **zero** `[tlb]` lines, so the shootdown never fired; the failure is a userspace near-NULL `INSTRUCTION_FETCH` from an unrelocated `DT_RELR`-encoded `DT_FINI_ARRAY` destructor pointer.
- Observability (Track A) is sequenced **before** any kernel fix (Track B) deliberately: the current gate is PASS-or-SKIP with the child's output `dup2`'d to an unlinked tmpfs file and matched serial consumed, so slow / stalled / faulted are indistinguishable today.
- Per the AGENTS.md policy, the kernel version bump (D.3) lands with the implementation, not this planning-docs PR.
