# 03 — Red Flags and Status Mismatches

A "red flag" in this audit means **the project's claim about a phase materially contradicts evidence inside the same doc set**. Honest deferrals are not red flags. Documented shortcuts are not red flags. Items where the design doc admits "we did the easy thing now and the hard thing is Phase X" are not red flags. The flags below are places where reading the README would mislead a reader about the state of the system.

The 16 items below are ranked by severity. The 🛑 / 🔴 / 🟠 / 🟡 tags match the ones in `01-completion-truth-matrix.md`.

---

## 🛑 1. Phase 19 — design doc Complete, task doc all six tracks Not started

**Evidence:** `findings/02-usability-and-productivity.md` § Phase 19. The design doc declares `Status: Complete`. The task doc track-layout table reads:

> Track A (Signal mask + rt_sigprocmask): **Not started**
> Track B (sigframe layout + handler dispatch): **Not started**
> Track C (sigreturn syscall): **Not started**
> Track D (sigaltstack): **Not started**
> Track E (rt_sigaction upgrade + sa_restorer): **Not started**
> Track F (Validation and tests): **Not started**

**Why it matters:** Phases 21 (ion shell), 29 (PTY subsystem), 30 (telnet server), 43 (SSH), and the entire job-control story depend on signal handling working. All those phases are marked Complete. Either signals work and Phase 19's task doc is grossly stale, or they don't and several downstream phases are over-claimed.

**Recommended action:** Reconcile the two docs. If signals work, flip the task doc to Complete with verified checkboxes. If they don't, demote Phase 19 to In Progress and re-evaluate dependents.

---

## 🛑 2. Phases 42b, 43b, 43c, 46, 47 — `Status: Complete` with universally unchecked task docs

**Evidence:** `findings/03-kernel-infra-and-apps.md` § Pattern 1.

| Phase | Tracks | Tasks unchecked | Subject |
|---|---|---|---|
| 42b | 7 | All | Async executor (also missing design doc) |
| 43b | 9 | All | Kernel trace ring, `sys_ktrace`, userspace `ktrace` tool |
| 43c | 11 | All | Regression/stress CI: `cargo xtask regression`, `cargo xtask stress`, proptest, loom |
| 46 | 8 (22 tasks) | All | System services: service manager, syslogd, crond, sys_reboot, shutdown/reboot, who/w/last |
| 47 | 6 | All | DOOM: 3 new framebuffer/input syscalls (0x1002–0x1004), doomgeneric platform layer, xtask integration, WAD placement |

**Why it matters:** AGENTS.md cites syslogd, crond, the service manager, the kernel trace ring, and DOOM as shipped features. The task docs provide zero machine-verifiable record of completion. Either:
1. The work was done and the task docs were never updated (documentation failure), or
2. The phases were declared complete prematurely (work failure).

Either interpretation makes the task docs untrustworthy. The 33–47 range has the highest concentration of this pattern; downstream phases (especially Phase 56, which depends on Phase 47's framebuffer baseline) inherit unverified premises.

**Recommended action:** A single reconciliation pass: walk each phase's acceptance items, mark the genuinely-done ones `[x]`, mark the genuinely-not-done ones with an explicit deferral note and an owner phase. Where any item is genuinely not done, flip the phase status accordingly.

---

## 🛑 3. Phase 35 — `maybe_load_balance()` commented out; SMP load balancing is inoperative

**Evidence:** `findings/03-kernel-infra-and-apps.md` § Phase 35. Task doc Track E.2 unchecked with note: *"Deferred — the scheduler loop currently leaves the `maybe_load_balance()` hook commented out pending the follow-up noted in code"*. Design doc's own audit note: *"the global `SCHEDULER` lock is still acquired on every dispatch iteration for task-state reads, transitions, and post-switch bookkeeping."*

**Why it matters:** Phase 35 ("True SMP Multitasking") exists specifically to deliver per-CPU run queues, work-stealing, and load balancing on top of Phase 25's basic AP boot. The data structures (per-CPU queues) landed; the dispatch behaviour that uses them did not. The phase delivers Phase 25-shaped scheduling with the scaffolding for something more, marked as if the something-more is in production.

**Recommended action:** Either (a) uncomment `maybe_load_balance` and verify, or (b) explicitly demote Phase 35 to "structural work — dispatch hot path deferred" and assign an owner phase (the "true per-core scheduling" deferral re-stated in 52d is the natural target, but it has no phase number).

---

## 🛑 4. Phase 33 — slab migration (the headline deliverable) never done

**Evidence:** `findings/03-kernel-infra-and-apps.md` § Phase 33. Task doc C.4 unchecked with note: *"Deferred: The slab-cache infrastructure is in place, but the broad object-allocation migration from the original plan remains deferred"*. Design doc carries an explicit audit note: *"Shipped state (audited in Phase 53a): The slab-cache infrastructure and direct cache tests landed, but the main hot kernel object families were not broadly migrated to use it. Most kernel allocations still flow through the global linked-list heap."*

**Why it matters:** Phase 33's premise was buddy + slab + working munmap. The slab *infrastructure* shipped, but the migration that would justify the slab cache as a Phase-33 deliverable was deferred. The phase is closed; the deferral has no owner phase. Phase 53a (Kernel Memory Modernization) added per-CPU page caches and magazine-based slabs but did not perform the broad object-family migration either.

**Recommended action:** Decide whether broad slab migration is part of the 1.0 quality bar. If so, assign it to a phase. If not, document that explicitly and stop relying on the implicit assumption that Phase 33 delivered it.

---

## 🛑 5. Phases 30, 31, 32 — entire validation tracks deferred behind "manual QEMU test"

**Evidence:** `findings/02-usability-and-productivity.md` §§ Phase 30, 31, 32.

- **Phase 30 (Telnet Server):** Track F has 16 items, all unchecked, all annotated "manual QEMU test". Includes "telnetd boots", "login prompt appears", "auth → shell", "≥4 concurrent sessions", "SIGHUP on disconnect", "PTY freeing on reconnect". G.3 "Phase 30 marked complete in roadmap README" itself unchecked, deferred until manual tests pass.
- **Phase 31 (Compiler Bootstrap):** Tracks D/E/F (~15 items) all deferred. Includes "tcc --version produces a version line", "Compile hello.c with `tcc -run hello.c`", **all of Track E (TCC self-hosting)**, F.1 "`cargo xtask check` passes with all new code".
- **Phase 32 (Build Tools):** Tracks B(partial)/C(partial)/D/E/F (~20 items) deferred. **Track D adds POSIX shell features (for/if/$?/$()) to sh0 — substantial new behaviour, never validated.** Track E includes "Full build completes", "Incremental rebuild", "make clean", "ar archive creation".

**Why it matters:** Each of these phases ships a substantial user-visible feature (telnetd, TCC, make + sh0 features). Each phase is marked Complete. None of the runtime validation that would distinguish "the binary built" from "the feature works" was performed. A user invoking `telnet localhost 2323` or `tcc -run hello.c` after a fresh build is testing each phase's headline claim for the first time.

**Recommended action:** Run these manual tests once and check the boxes. The work is bounded and the tests are documented.

---

## 🛑 6. Phase 51 — In Progress with no task doc, but bypassed by Phase 53/54

**Evidence:** `findings/04-convergence-and-serverization.md` § Phase 51.

> No task doc exists for Phase 51 (the tasks directory has no `51-service-model-maturity-tasks.md`). The design doc contains acceptance criteria but no track-level breakdown, completion checkboxes, or gate evidence. Phase 53 lists Phase 51 as a dependency without ✅, suggesting Phase 53 was closed despite Phase 51 remaining open. The Phase 53 Gate Bundle implicitly exercises Phase 51's service model claims (`service list`, `service status`, `service restart`) without Phase 51 being formally closed.

**Why it matters:** Phase 51 is the foundational phase for service-model maturity. Phase 52 (extractions), Phase 53 (gates), and Phase 54 (deep serverization) all assume the service model is mature. The chain works because the underlying code presumably works — but the phase that formally certifies it is open with no closure plan and no task doc.

**Recommended action:** Either close Phase 51 (write the task doc, run the acceptance gates, flip the status) or explicitly merge it into 52d's scope retrospectively and remove from the dependency chains.

---

## 🛑 7. Phases 55a, 55b, 56, 57a — design docs say "Planned" while every downstream phase treats them as done

**Evidence:** `findings/05-hardware-display-preemption.md` § Status Metadata Inconsistencies.

| Phase | Status in design doc | Contradicting evidence |
|---|---|---|
| 55a | Planned | 55c and 56 list 55a as `✅`; AGENTS.md describes IOMMU as operational |
| 55b | Planned | 55c, 56, 57 list 55b as `✅`; AGENTS.md describes ring-3 drivers as operational |
| 56 | Planned | Completion-gaps doc closing checklist fully ticked; kernel bumped to 0.56.0; 9/9 closing boxes checked |
| 57a | Planned | AGENTS.md describes scheduler rewrite as complete; 57b/57c list 57a as `✅` |

**Why it matters:** The design-doc status field is the most prominent place a reader looks. When AGENTS.md and downstream phases consistently disagree with the status field, the README is no longer a reliable source of truth — readers must learn which fields to ignore.

**Recommended action:** Flip these four phases' status fields to "Complete" (or "Complete (with caveats)" where appropriate). If any of the four genuinely is not complete, the downstream phases that depend on it must be demoted.

---

## 🔴 8. Phase 13 — Complete with no task doc

**Evidence:** `findings/01-foundation-and-userspace.md` § Phase 13. README literally reads: *"Tasks: not yet created"*. The five acceptance criteria in the design doc have no task-level evidence. Phase is marked Complete.

**Why it matters:** The other 60+ phases all carry task docs. Phase 13 is the lone exception. Either write the task doc retroactively, or document explicitly why this phase doesn't need one.

---

## 🔴 9. Phase 16 — Complete with planning-table task doc and no checkboxes

**Evidence:** `findings/01-foundation-and-userspace.md` § Phase 16. The task doc uses pipe tables listing P16-T001 through P16-T073, no `[x]`/`[ ]` checkboxes, no `Status:` header. Track G is described as "Acceptance:" prefixed text in table cells.

**Why it matters:** Same as Phase 13 — no per-task evidence of completion. The phase is marked Complete on the strength of the design doc alone.

---

## 🔴 10. Phase 22b — no design doc, validation track entirely deferred

**Evidence:** `findings/02-usability-and-productivity.md` § Phase 22b. Only a task doc exists. Track F has 7 items, every one marked "Deferred (manual QEMU visual test)". P22b-T046 ("sh0 still works correctly — cooked mode echo + line discipline unaffected") is deferred — meaning the most basic regression check after ANSI parser changes was never run.

**Why it matters:** The implementation tracks (A–E) shipped without an acceptance baseline. If the ANSI parser broke sh0's line discipline, no test would have caught it.

---

## 🔴 11. Phase 25 — TLB shootdown not wired into munmap; per-core syscall dispatch deferred

**Evidence:** `findings/02-usability-and-productivity.md` § Phase 25. Track E "Done (handler+API; munmap hook deferred)" and Track C "Done (BSP-only dispatch; per-core syscall deferred)". 2 of 3 SMP acceptance criteria unverified.

**Why it matters:** A `munmap` on one core can leave stale TLB entries on other cores. This is a silent correctness hazard — programs may briefly see freed memory contents under SMP. The "fix" was deferred to Phase 35; Phase 35 also did not deliver it (the dispatch hook is commented out — see flag #3).

**Recommended action:** Wire `tlb_shootdown` into `munmap` and validate. Without it, the SMP claim is qualified.

---

## 🔴 12. Phase 43 — SSH end-to-end tests all unchecked + pubkey format incompatible

**Evidence:** `findings/03-kernel-infra-and-apps.md` § Phase 43.

- G.1 (password auth): `[ ]` Manual
- G.2 (pubkey auth): `[ ]` Manual
- G.3 (Wireshark traffic inspection): `[ ]` Manual
- Pubkey format: hex-encoded 32-byte Ed25519 (64 hex chars per line), not OpenSSH wire format

**Why it matters:** SSH is a flagship feature — the README cites it as a major capability. The three end-to-end tests that would prove it works (against any standard SSH client) are all unchecked. The pubkey format choice means a user with a standard `~/.ssh/authorized_keys` cannot copy it into m3OS.

**Recommended action:** Run the three manual tests; either flip checkboxes or fix what they reveal. Decide whether OpenSSH-format pubkey support is part of 1.0 — if so, give it an owner phase.

---

## 🟠 13. Phase 21 — milestone-goal inversion (sh0 became primary, not ion)

**Evidence:** `findings/02-usability-and-productivity.md` § Phase 21 + `docs/appendix/phase-21-handoff.md`. The design doc's milestone goal was "ion as primary interactive shell, sh0 as fallback". The implemented outcome is "sh0 as primary, ion as fallback". Tasks T028, T030–T034, T038–T044 deferred to Phase 22.

**Why it matters:** The phase achieved register-corruption fixes and other useful work, but the headline goal was inverted. Phase 22 is marked Complete, suggesting the inversion was eventually undone — but Phase 21's status field never reflected the goal-inversion period.

---

## 🟠 14. `fat_server` is a permanent ENOSYS stub but Phase 54 is Complete

**Evidence:** `findings/06-code-side-scan.md` § Userspace. `userspace/fat_server/src/main.rs:67` — entire service replies `-ENOSYS` to every request. The Phase 54 userspace FAT storage server is supervised, registered, and deployed but does nothing.

**Why it matters:** Phase 54 (Deep Serverization) claims storage-and-VFS extraction. The FAT-server slice is listed as one of the extracted services. The slice is structurally extracted (a service is registered) but functionally non-existent. VFS callers hitting `fat_server` get a clean errno but no data.

**Recommended action:** Either implement the FAT operations in `fat_server` (real Phase 54 work) or remove the supervised stub and document that FAT migration was deferred.

---

## 🟠 15. Phase 56 — D-E4 subscription-push not transmitted; D-A0 modifier-key wire format change

**Evidence:** `findings/05-hardware-display-preemption.md` § Phase 56 + `findings/06-code-side-scan.md` § Display server. Four `publish_*` functions in `userspace/display_server/src/control.rs` (lines 670, 690, 696, 703) queue events but never transmit them. The module doc admits: "only the wire transmission remains." `m3ctl`-style consumers subscribed to `SurfaceCreated`, `SurfaceDestroyed`, `FocusChanged`, `BindTriggered` events receive nothing.

**Why it matters:** Display server subscription events are the mechanism for any out-of-process tool that wants to react to surface lifecycle. Today they silently fail. Marked as a Phase 56 deferral but no owner phase assigned.

---

## 🟠 16. Phase 57a — pi_lock wiring missing at 4 scheduler sites; Phase 57b soak result undocumented

**Evidence:** `findings/05-hardware-display-preemption.md` § Phase 57a/57b + `findings/06-code-side-scan.md` § Kernel ring-0.

- Four `// TODO(57a-C/D): route through pi_lock + with_block_state` markers in `kernel/src/task/scheduler.rs` at lines 829, 3782, 3789, 3988 — bare `task.state = ...` stores bypassing the abstraction Phase 57a Tracks C/D were supposed to deliver.
- Phase 57b status is "Complete pending soak (PR #132)". Acceptance requires "A 30-minute soak with `cargo xtask run-gui --fresh` produces zero panics from this assertion." No soak result document exists in any accessible doc.

**Why it matters:** Phase 57e (Full Kernel Preemption) is in active development on the current branch. Its safety depends on the `preempt_count` discipline established by 57b — and on the pi_lock + with_block_state protocol established by 57a. Both have outstanding integrity gaps. The git log shows recent fixes for Bug #12 and Bug #13 in 57e commits, suggesting active stress is exposing exactly the kinds of races these abstractions are meant to prevent.

**Recommended action:** Either run the 57b soak and record the result, or remove the "pending soak" qualifier with rationale. Land the four pi_lock callsites before 57e closes.

---

## Lower-tier doc-quality issues

These are not red flags about feature completion, but about the integrity of the roadmap itself. Listed for completeness:

- **Phase 35 design doc missing required `Status:` and `Source Ref:` header fields** (template violation per `docs/appendix/doc-templates.md`).
- **Phase 22b has no design doc** (template violation).
- **Phase 42b has no design doc** (template violation).
- **AGENTS.md version string stale at v0.51.0** — current version is 0.57.0+; correction is an explicit acceptance item in Phase 54a Task C.4 (still Planned).
- **Nine task docs use a no-checkbox table format** (Phases 16, 17, 18, 20, 22, 23, 25, 28, 29). For phases declared Complete, this means there is no per-item evidence of which acceptance criteria were actually verified.
- **Phase 55a's "Known Open Bug" section was never updated** to reflect the closure that 55c R2 claims (the VT-d MMIO `CTRL.RST` issue is reported as open in 55a's doc and as closed in 55c's task list).
