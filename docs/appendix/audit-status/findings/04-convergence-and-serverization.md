# Audit Findings: Convergence and Serverization Phases (48–54a)

**Audit scope:** Phases 48, 49, 50, 51, 52, 52a, 52b, 52c, 52d, 53, 53a, 54, 54a
**Date:** 2026-05-07
**Sources read:** Design docs + audit doc for Phase 48; task docs for 48, 49, 50, 52, 52a–52d, 53, 53a, 54, 54a; no task docs exist for 51 or 53 (noted in each phase entry)

---

## Phase 48 — Security Foundation

**Declared status:** Complete

**Acceptance criteria not actually met:** None documented as unmet. All nine tracks in the task doc carry status Complete. The acceptance criteria in the design doc map directly onto track outcomes.

**Deferred items (verbatim):**
> - Full privilege separation across all network services
> - Advanced key-management, rotation, and audit infrastructure
> - Rich multi-factor or hardware-backed authentication flows
> - General sandboxing beyond the repaired trust floor

**Documented shortcuts:** The audit doc (48-security-foundation-audit.md) records the pre-Phase-48 state in detail: `setuid`/`setgid` with zero privilege checks; `rdtsc`-only seeding with a `0xDEAD_BEEF_CAFE_BABE` constant fallback; single-iteration SHA-256 using username-derived salts; telnetd `restart=always` by default. These are the items the phase claimed to repair.

**Cross-references:** Remediates trust-floor gaps from Phase 27 (User Accounts), Phase 42 (Crypto Primitives), Phase 43 (SSH), Phase 46 (System Services). The audit note in Track B.2 of the task doc records that `login` and `su` rely on euid=0 when calling `setuid`/`setgid`, so the enforcement change preserves those flows.

**Red flags:**
- The task doc uses unchecked `[ ]` boxes (not `[x]`) for all acceptance items — the doc format does not indicate completion in machine-readable form. Textual `Status: Complete` in the header is the only explicit completion signal. This is a consistent pattern across all Phase 48 tracks.
- Track B.2 acceptance item B.2.2 (`large shell output over SSH completes without stalling`) was explicitly noted as "expect pattern matching issues with SSH escape sequences prevented automated verification" — this item carried forward as unverified at 52a's close.
- The iterated hash format `$sha256i$<rounds>$<hex_salt>$<hex_hash>` is documented as the target, while the Phase 53 regression description mentions both `$sha256$` (pre-seeded image) and `$sha256i$10000$` (after `passwd`) — implying the default image still ships with the old single-iteration format in the seed step. The regression explicitly accepts this two-format situation as intended behavior, but it means the pre-seeded root password never got the upgraded work factor.

---

## Phase 48 Audit Doc — Security Foundation: Trust-Floor Audit

**Status:** Complete (date 2026-04-07; covers Tasks A.1–A.4 only — Track A documentation-only audit)

**Key findings recorded:**
- A.1a: `setuid`/`setgid` CRITICAL — zero privilege checks
- A.1b: `setreuid`/`setregid` MEDIUM — missing saved-UID support
- A.2a/b/c: Entropy HIGH — TSC-only seeding, xorshift64 PRNG, `0xDEAD_BEEF` fallback
- A.3a/b/c: Password HIGH — zero work factor, deterministic salts, hardcoded hashes in xtask
- A.4a/b: Service MEDIUM — telnetd auto-starts, no enable/disable toggle

**Note:** The audit doc is a historical record of the pre-implementation state, not an implementation artifact. Its findings directly drove the Phase 48 task tracks B–I.

---

## Phase 49 — Architectural Declaration

**Declared status:** Complete

**Acceptance criteria not actually met:** None explicitly documented as unmet.

**Deferred items (verbatim):**
> - Full service extraction for storage, networking, and display
> - Broad POSIX/libc boundary redesign
> - Strong automated architecture-lint enforcement beyond documentation and review rules

**Documented shortcuts:** None — this phase is primarily documentation and structural decomposition. The outcome is the keep/move/transition matrix in `docs/appendix/architecture-and-syscalls.md`.

**Cross-references:** Establishes the ownership contract that Phase 50 (IPC Completion) and Phase 52 (First Service Extractions) rely on. The Phase 50 Evaluation Gate Results (embedded in the design doc) explicitly cross-references the matrix: keyboard/input subsystem was identified as a **gap** (not listed), and was resolved by declaring it **Move — Stage 2** in Track H.4.

**Red flags:** None material. The missing keyboard matrix entry gap was documented and resolved within the same phase scope.

---

## Phase 50 — IPC Completion

**Declared status:** Complete

**Acceptance criteria not actually met:** None documented as unmet. The Evaluation Gate Results section (embedded in the design doc) records direct audit of the IPC subsystem.

**Documented shortcuts/residuals:** Three server loops in `kernel/src/main.rs` still use raw pointer or kernel-task patterns at close:
- `console_server_task` — `copy_nonoverlapping` kernel-task shortcut; migration planned for ring-3 move
- `fat_server_task` — raw pointers, delegates to `ramdisk::handle(&msg)`
- `vfs_server_task` — forwards raw-pointer messages, inherits `fat_server` assumptions

These are documented as planned migrations, not as unintended residuals.

**Deferred items (verbatim):**
> - Deep performance tuning of zero-copy paths
> - Rich typed service IDLs or code-generated message bindings
> - Advanced delegation patterns beyond the basic capability and buffer model

**Cross-references:** Closes the transport gap from Phase 6 (IPC Core). Depends on Phase 49 ownership matrix. Directly enables Phase 52 service extraction.

**Red flags:** None material. The in-kernel server loop residuals are honestly documented and are the extraction targets for Phases 52 and 54.

---

## Phase 51 — Service Model Maturity

**Declared status:** In Progress

**What is actually missing:** No task doc exists for Phase 51 (the tasks directory has no `51-service-model-maturity-tasks.md`). The design doc contains acceptance criteria but no track-level breakdown, completion checkboxes, or gate evidence. The roadmap README confirms status "In Progress."

**Acceptance criteria (from design doc):**
1. Service definitions stable enough to describe dependencies, restart rules, and privileges
2. Service restart, stop, status, and log inspection behave predictably
3. Shutdown and reboot drain services in a defined order
4. Validation coverage exists for service restart, shutdown, and operator-facing status flows
5. Later extracted services have a documented and working path into the service model

**What blocks closure:** No task doc, no explicit per-criterion gate evidence. The phase has been bypassed in practice — Phase 52, 52a–52d, 53, and 54 all landed with Phase 51 still "In Progress." The Phase 53 design doc lists Phase 51 as a dependency but marks it without ✅ in the dependency list header, suggesting Phase 53 was closed despite Phase 51 remaining open.

**Deferred items (verbatim):**
> - Advanced service sandboxing and capability confinement
> - Socket activation and readiness protocols
> - Rich health probes, backoff tuning, and multi-instance orchestration
> - Structured journaling and long-term log retention policy

**Cross-references:** Extends Phase 46 (System Services). Enables Phase 52 (First Service Extractions). Phase 53 explicitly uses Phase 51 service model for its headless workflow claims.

**Red flags:** Phase 51 is the most problematic status in this audit range. It is "In Progress" with no task doc and no explicit closure criteria met, yet later phases (53, 54) depend on it and have been marked Complete. The service-model maturity claims embedded in Phase 53's Gate Bundle implicitly assert that Phase 51 work is done, but Phase 51 itself is never formally closed. This is a structural gap in the remediation chain.

---

## Phase 52 — First Service Extractions

**Declared status:** In Progress

**What is actually missing:** The design doc's acceptance criteria require:
1. At least one previously kernel-resident core service runs as a real supervised ring-3 process
2. The extracted service uses the Phase 50 IPC path without shared-address-space shortcuts
3. Restarting or crashing the extracted service does not require a full machine reboot
4. The service graph, build system, and docs all describe the extracted service as part of the normal system model
5. Boundary measurements and trade-offs are written down for later phases

Phase 52a, 52b, 52c, and 52d were created specifically to handle bugs and structural issues discovered during Phase 52 work, but Phase 52 itself was never closed. The remediation sub-phases treated 52 as an ongoing work surface rather than bringing it to a defined close.

**Task doc status:** A task doc exists (`52-first-service-extractions-tasks.md`) but its completion state is not auditable from the design docs alone — the task doc was not read in full for this audit pass given the sub-phase structure.

**Cross-references:** All four 52a/b/c/d phases are children of this phase. Phase 53 lists Phase 52d (not Phase 52) as a dependency, suggesting the closure path went through 52d without formally closing 52.

**Red flags:** Phase 52 is the root "In Progress" parent whose child work became four separate phases. The original phase scope — console and keyboard service extraction — appears to have been partially delivered, but the formal acceptance criteria were never checked off. This may represent intentional scope management (child phases doing the work) or a tracking gap.

---

## Phase 52a — Kernel Reliability Fixes

**Declared status:** Complete

**Post-phase audit note (verbatim from design doc):**
> Phase 52a's `restore_caller_context` work was the correct stop-gap when it landed, but the current codebase no longer uses that exact mechanism on the main IPC/futex resume paths. Phase 52b replaced it with scheduler-restored task-owned return state, and Phase 52d closed that handoff explicitly while adding the missing regression coverage for the exec-time signal-reset contract.

**Gaps that 52d documented (from 52d's own account of 52a):**
- The manual `restore_caller_context` stop-gap was immediately superseded by the 52b task-owned return-state direction and the roadmap never recorded that handoff clearly. 52d explicitly closed this.

**Acceptance criteria not met at close of 52a (per task doc):**
- Track B.2 item: "Large shell output (e.g., `cat` a 100-line file) over SSH completes without stalling" — marked `[ ]` with note: "expect pattern matching issues with SSH escape sequences prevented automated verification." This item was acknowledged incomplete.

**Documented shortcuts:** The `restore_caller_context` approach itself was acknowledged as a stop-gap at the time.

**Deferred items (verbatim):**
> - Moving `syscall_user_rsp` to task-owned state (Phase 52b structural hardening)
> - Typed `UserBuffer` wrappers to make `copy_to_user` auditable (Phase 52b)
> - AddressSpace object for the underlying mapping divergence (Phase 52b)

**Cross-references:** Fixes bugs from Phase 52; remediates issues traced to Phase 40 (Threading), Phase 19 (Signal Handlers), Phase 11 (Process Model). All three deferred items explicitly assigned to Phase 52b.

---

## Phase 52b — Kernel Structural Hardening

**Declared status:** Complete

**The "partial" language explained:** The roadmap README tagline reads "partial task-owned return-state groundwork." This refers to the following: `UserReturnState` was defined and the scheduler was updated to restore from it, but the contract was incomplete — state was still saved at block/yield points rather than at syscall entry, and `kernel_stack_top` / `fs_base` were still split between `Task`, `Process`, and per-core scratch. This half-migration was completed by Phase 52d.

**Gaps that 52d documented (from 52d's own account of 52b):**
> Phase 52b had landed the structure of task-owned return state, but not the full contract. `UserReturnState` existed, yet the implementation still saved state at block points, restored only part of the resume path from the task, and left `kernel_stack_top` / `fs_base` split between `Task`, `Process`, and per-core scratch.

**Post-phase audit note (verbatim from design doc):**
> Most of Phase 52b landed as designed, and the follow-up audit gaps are now closed. Phase 52d moved the primary `UserReturnState` snapshot to syscall entry, made scheduler dispatch the authoritative restore path for resumed userspace tasks, and activated `AddressSpace::generation` bump/report plumbing across mapping mutations and user-copy diagnostics.

**Task doc completion state:** All five tracks carry "Complete" status. However, the individual acceptance checkboxes in Tracks A–E use `[ ]` (unchecked) format — the completion is asserted only at the track-header level. This means the task doc acceptance items cannot be verified as individually satisfied from the doc alone.

**Deferred items (verbatim):**
> - VMA tree (BTreeMap or interval tree) — deferred to Phase 52c
> - Per-core scheduler with work-stealing — deferred to Phase 52c
> - Dynamic IPC resource pools — deferred to Phase 52c
> - ISR-direct notification wakeup — deferred to Phase 52c

All four deferrals were addressed in Phase 52c (with the scheduler hot path and notification pool subsequently re-deferred by 52d).

---

## Phase 52c — Kernel Architecture Evolution

**Declared status:** Complete

**What "deferred scheduler/keyboard/notification closure" means:** The roadmap README tagline mentions this deferred closure. Specifically:
1. **Keyboard input path** — `stdin_feeder` still duplicated line-discipline logic in userspace despite `LineDiscipline` and `push_raw_input` being implemented. Closed by Phase 52d Track C.
2. **Scheduler hot path** — Per-core run queues and work-stealing landed, but the global `SCHEDULER` lock was still acquired on every dispatch iteration. Explicitly re-deferred by 52d with truthful code/doc comments.
3. **Notification pool** — Remained fixed-size (`MAX_NOTIFS = 64`) due to ISR-safety constraints. Explicitly re-deferred by 52d with ISR-safety rationale documented.

**Gaps that 52d documented (from 52d's own account of 52c):**
> Phase 52c had landed kernel-side line-discipline infrastructure, but the live keyboard path still duplicated that logic in userspace. `push_raw_input` and `LineDiscipline` existed, while `userspace/stdin_feeder` still read termios flags and implemented `ICANON`, `ISIG`, echo, and canonical editing itself.
> Some 52c scalability claims had been marked complete before the code matched them.

**Acceptance criteria with explicit post-phase revisions (from design doc):**
- "Scheduler dispatch path does not acquire a global lock" — annotated: *(deferred — see Post-Phase Audit Note; global lock still acquired in HEAD; true lock-free per-core dispatch deferred to a future phase)*
- "Only one line discipline implementation exists in the codebase (kernel-side)" — annotated: *(partial — see Post-Phase Audit Note; stdin_feeder still duplicates ldisc logic)*
- "`stdin_feeder` does not contain any canonical editing, echo, or ISIG logic" — annotated: *(partial — see Post-Phase Audit Note)*

**Deferred items (verbatim, final state after 52d reconciliation):**
> - Full fair scheduler with virtual runtime (Zircon WAVL / Linux CFS)
> - **True per-core scheduling** (lock-free dispatch hot path) — per-core run queues and work-stealing landed, but task state transitions still require the global `SCHEDULER` lock; splitting task ownership per-core is a larger architectural change deferred past Phase 52
> - **Growable notification pool** — notifications remain fixed-size (`MAX_NOTIFS = 64`) because ISR-safe access requires lock-free fixed-size arrays
> - Atomic `reply_recv` (seL4-style)
> - Preemptive scheduling from interrupt context
> - Dynamic PTY pool

---

## Phase 52d — Kernel Completion and Roadmap Alignment

**Declared status:** Complete

**This is the meta-audit phase.** 52d exists specifically to document and close the gaps in 52a/b/c. Every gap 52d documented is captured here.

**The five gaps 52d identified and closed:**

1. **52a underspecification after later work.** The `restore_caller_context` stop-gap was historically correct but immediately superseded by 52b's direction. The handoff was never recorded in the roadmap. 52d added the Post-Phase Audit Note to 52a's design doc.

2. **52b's incomplete task-owned return-state contract.** `UserReturnState` existed but: state was still saved at block points (not syscall entry), the resume path only partially restored from the task, and `kernel_stack_top` / `fs_base` were split between `Task`, `Process`, and per-core scratch. 52d Track B completed this by: snapshotting state once at syscall entry, making scheduler dispatch the authoritative restore path, and activating `AddressSpace::generation` tracking.

3. **52c's keyboard path still duplicated ldisc in userspace.** `stdin_feeder` still read termios flags and implemented `ICANON`, `ISIG`, echo, and canonical editing despite `push_raw_input` and `LineDiscipline` existing. 52d Track C reduced `stdin_feeder` to scancode decode + `push_raw_input` and quarantined the workaround-only termios return syscalls as deprecated.

4. **52c scalability claims overstated vs. code.** Scheduler still used global `SCHEDULER` lock on dispatch; notifications still fixed-size. 52d Track D explicitly re-deferred both with documented rationale: global lock split requires per-core task ownership (architectural change beyond Phase 52 scope); growable notification pool requires ISR-safe allocator not currently available.

5. **Validation and branch hygiene.** The integrated `feat/phase-52d` branch exposed a boot-time deadlock before `login:` (caused by `user_mem`/`rt_sigaction` lock reentrancy). Generated initrd payloads were dirtying the source tree. 52d Track E fixed the boot blocker and moved generated payloads to `target/generated-initrd/`.

**What 52d explicitly left deferred (verbatim):**
> - Full fair scheduling or EEVDF/CFS-style runtime accounting
> - Cluster-aware or NUMA-aware work-stealing policy
> - **True per-core scheduling** (lock-free dispatch hot path) — per-core run queues and work-stealing landed in 52c, but the global `SCHEDULER` lock is still acquired on every dispatch iteration for task state reads and transitions; splitting task ownership per-core requires a larger architectural change deferred past Phase 52
> - **Growable ISR-safe notification pool** — a sound design (two-level: fixed ISR-visible fast table + growable overflow) exists conceptually but is not needed at current scale (`MAX_NOTIFS = 64` covers foreseeable demand); exhaustion diagnostics are in place
> - Broader cleanup of compatibility/debugging syscalls that are not exercised by in-tree code after the Phase 52 closure work is complete

---

## Phase 53a — Kernel Memory Modernization

**Declared status:** Complete

**Acceptance criteria not actually met:** None documented as unmet. The design doc's acceptance criteria list is detailed and the phase is marked Complete.

**Deferred items (verbatim):**
> - NUMA-aware per-domain slab and page caches
> - Constructor/destructor object caching
> - Full memory debugging suite (red zones, poison fill, KFENCE-style sampling)
> - Memory pressure callbacks (shrinker interface) — deferred to Phase 54
> - Full GFP-like allocation-context flags
> - Type-state `Frames<Free/Allocated/Mapped>` wrappers

**Cross-references:** Extends Phase 33 (Kernel Memory), Phase 35 (True SMP), Phase 36 (Expanded Memory). Preserves Phase 52b's zero-before-user-exposure guarantee while moving zeroing off the free path. Uses Phase 52c architectural evolution pattern (same approach as VMA tree replacement).

**Phase 53 / 53a closure contract:** Phase 53 explicitly required the gate bundle to pass on the allocator-sensitive post-53a baseline before Phase 53 could be marked Complete. This is an unusual explicit sequencing constraint.

**Red flags:** None material. The phase design is detailed and the closure contract is explicit.

---

## Phase 53 — Headless Hardening

**Declared status:** Complete

**Acceptance criteria:** All criteria in the design doc are structural (documented headless workflow, exact gate bundle, support boundary, security floor location documented). Phase 53 defines gates rather than shipping new features, so its closure criteria are largely documentation and validation artifacts.

**Notable gate elements:**
- The `security-floor` regression verifies (a) `id` confirms uid=0, (b) `/etc/shadow` contains `$sha256$` or `$sha256i$10000$` (two-format accepted), (c) `su` authenticates, (d) `whoami` resolves. This explicitly accepts both hash formats.
- Shutdown/reboot verified by manual checklist only — not automated due to QEMU-exit coordination fragility under CI load.
- Nightly stress (`cargo xtask stress --test ssh-overlap --iterations 50 --timeout 90`) classified as sustaining evidence, not a per-PR gate.

**Phase 51 dependency note:** Phase 53's dependency list includes Phase 51 (Service Model Maturity) but without a ✅ marker. Phase 53 was closed while Phase 51 remains "In Progress." The Phase 53 Gate Bundle implicitly exercises Phase 51's service model claims (`service list`, `service status`, `service restart`) without Phase 51 being formally closed.

**Deferred items (verbatim):**
> - Broad outbound developer networking (HTTPS/TLS clients, DNS resolution, git remotes, GitHub tooling)
> - GUI / display compositor / graphical session / local desktop
> - Mouse input, audio output
> - Large third-party runtime ecosystems (Python, Node.js, JVM)
> - Broad hardware certification beyond QEMU x86_64 with OVMF
> - Package feeds, remote package repositories, dynamic linking
> - Full POSIX compliance testing

---

## Phase 54 — Deep Serverization

**Declared status:** Complete

**Acceptance criteria not actually met:** None documented as unmet. All five tracks (A–E) are marked Complete.

**What was actually extracted:**
- Track A: Read-only `/etc/...` rootfs reads traverse `vfs_server`/`fat_server`
- Track B: Metadata, access, `getdents`, and mount-policy flow through `vfs_server` with kernel fallback
- Track C: UDP policy/state moves into `net_server` with kernel handle ownership preserved
- Track D: `init` degraded-mode rules documented
- Track E: Regression, quality gates, docs closure; signal/IPC shutdown bug fixed

**Items surfaced at closure that triggered Phase 54a:**
1. Every non-pipe, non-socket, non-epoll `FdEntry` construction site hardcodes `cloexec: false, nonblock: false` — `open(path, O_RDONLY | O_CLOEXEC)` silently drops the CLOEXEC guarantee. Phase 54's new `vfs_service_open` added another such site, actively growing the problem.
2. Four `arch::x86_64::syscall::*_pub` layer-crossing wrappers in `kernel/src/process/mod.rs` (`release_socket_pub`, `epoll_free_pub`, `reap_unused_ext2_inode`, `vfs_service_close_pub`). The closure review flagged one; a coherent fix requires relocating all four.

Both items were intentionally scoped out of PR #108 because they touch code beyond the PR's surface. This triggered Phase 54a.

**Deferred items (verbatim):**
> - Broader filesystem matrix beyond the first migrated path
> - Full network-service ecosystem and higher-level userland daemons
> - Aggressive performance tuning once the boundary is correct
> - Complete POSIX policy removal from the kernel

Additionally, from `docs/debug/54-followups.md` (referenced in Phase 54a task doc):
- MOUNT_OP_LOCK yielding primitive — long-term scheduling work
- Scheduler diagnostic threshold tuning
- Full epoll extraction (Phase 54a hoists only the cleanup helper)
- virtio_blk IRQ completion (spin-poll → IRQ-driven) — routed to Phase 55 Track C.5
- `/var/run → /run` symlink — routed to Phase 45 deferred list

---

## Phase 54a — Post-Serverization Kernel Hygiene

**Declared status:** Planned

**What triggered its creation:** Phase 54's closure review surfaced two items that were correctly deferred from the closure PR because they affect code the PR did not otherwise touch:
1. **CLOEXEC/NONBLOCK plumbing gap** — every non-pipe/non-socket/non-epoll `FdEntry` construction site hardcodes `cloexec: false, nonblock: false`. Security impact is bounded (pipe2, socket(SOCK_CLOEXEC), epoll_create1, accept4, socketpair, fcntl F_SETFD already honor the flag) but `open`, `openat`, `openat2`, and the new `vfs_service_open` do not.
2. **Four `*_pub` wrappers** — `kernel/src/process/mod.rs` imports from `crate::arch::x86_64::syscall` for generic cleanup, creating an arch-specific dependency for non-arch logic.

**Why it follows the precedent of 52a/b/c/d/53a:** The pattern of named aftermath phases for closure items that cannot fit cleanly into the parent PR is now explicit project practice.

**All acceptance criteria:** Planned (no checkboxes checked). None met yet.

**What remains open within 54a's scope:**
- Full epoll extraction remains deferred (54a only moves the cleanup helper)
- MOUNT_OP_LOCK yielding — long-term, tracked in `docs/debug/54-followups.md`
- Scheduler diagnostic threshold tuning — tracked in `docs/debug/54-followups.md`

**Version note:** Phase 54a is a patch-level bump to `v0.54.1`. The `AGENTS.md` project-overview version string is currently stale at `v0.51.0` — correction is an explicit acceptance item in Task C.4.

---

## Cross-Phase Synthesis: Remediation Phase Effectiveness

### Did 52a/b/c/d actually close the gaps from earlier phases?

| Item | Assigned to | Actually closed? | Notes |
|---|---|---|---|
| Stale `syscall_user_rsp` on IPC blocking paths | 52a | Partially — stop-gap only | `restore_caller_context` was the 52a fix; superseded by 52b/52d task-owned return-state |
| Sunset `Channel::wake_write()` bug | 52a | Already fixed pre-52a | Track B.2 in the task doc notes it was correct in checked-in code at 52a start |
| `clear_child_tid` on thread exit | 52a | Already fixed pre-52a | Task doc Track C notes "Already Fixed (pre-52a)" |
| Exec signal action reset | 52a | Yes, with regression coverage added in 52d | 52a implemented the fix; 52d added the missing regression proof |
| First-class `AddressSpace` object | 52b | Yes (structure landed) | Generation tracking was dormant until 52d activated it |
| Task-owned `UserReturnState` | 52b | Partial at 52b close | State saved at block points, not syscall entry; split ownership of `kernel_stack_top`/`fs_base`; fully closed in 52d |
| Typed `UserSliceRo`/`UserSliceWo` wrappers | 52b | Claimed complete; task doc uses unchecked `[ ]` boxes | Structural completion asserted but individual acceptance items not verifiable from docs |
| Batch TLB shootdown | 52b | Claimed complete; task doc uses unchecked `[ ]` boxes | Same note as above |
| Frame zeroing on free | 52b | Claimed complete | No contradicting evidence found |
| Per-core scheduler with work-stealing | 52c | Partial — queues/stealing landed, global lock persists | True lock-free dispatch deferred; explicitly documented in 52d and 52c post-phase notes |
| Growable IPC endpoint/capability pools | 52c | Yes | Endpoints, capabilities, service registry all growable |
| Growable notification pool | 52c | No — fixed-size `MAX_NOTIFS = 64` remains | ISR-safety constraint prevents growable pool; documented as intentional limit |
| Unified kernel-side line discipline | 52c | Infrastructure landed; live path partial at 52c close | `stdin_feeder` still duplicated ldisc; closed by 52d Track C |
| VMA tree (BTreeMap) | 52c | Yes | Landed in 52c |
| ISR-direct notification wakeup | 52c | Yes (with `drain_pending_waiters` fallback retained) | `IsrWakeQueue` per-core; BSP safety fallback preserved |
| `AddressSpace::generation` tracking active | 52b/52d | Closed in 52d | 52b added the mechanism dormant; 52d wired it into mapping mutations and user-copy diagnostics |
| Keyboard path convergence (single ldisc) | 52c/52d | Closed in 52d | `stdin_feeder` converted to scancode decode + `push_raw_input` |
| Boot blocker (`rt_sigaction` stall) | 52d | Yes | PROCESS_TABLE lock reentrancy in `user_mem` / `rt_sigaction` fixed |
| Generated initrd artifact noise | 52d | Yes | Moved to `target/generated-initrd/` |
| Exec-time signal-reset regression coverage | 52d | Yes | Added in 52d Track A.2 |
| Scheduler roadmap/code alignment | 52d | Yes (as documentation correction) | Global lock re-deferred with honest code/doc comments; not eliminated |

**Summary:** 52a/b/c/d collectively closed the bugs they targeted, but the "completion" story for each sub-phase is more complex than the status headers suggest:
- 52a closed two of four bugs; two were already fixed pre-52a; the stop-gap mechanism it introduced was superseded before 52a's own ink was dry.
- 52b shipped the right structures but left `UserReturnState` in a half-migrated state that required 52d to complete. The task doc acceptance items use unchecked `[ ]` boxes, making individual-item verification impossible from the docs.
- 52c overclaimed on scheduler and notification pool; the roadmap required 52d to correct this.
- 52d is the honest closure phase — it documented what was actually done, completed the kernel path convergence, and re-deferred the remaining scalability work with explicit rationale.

### What's still open after 54a (Planned)?

- **Phase 51 (Service Model Maturity)** — "In Progress" with no task doc and no formal closure. Later phases (53, 54) have been marked Complete while this foundational phase remains open. Its service-model claims are exercised by Phase 53's gate bundle but never formally satisfied.
- **Phase 52 (First Service Extractions)** — "In Progress." The sub-phases 52a–52d did the structural work but Phase 52 itself was never formally closed.
- **True per-core scheduling (lock-free dispatch hot path)** — explicitly deferred past Phase 52, no assigned phase.
- **Growable ISR-safe notification pool** — explicitly deferred; `MAX_NOTIFS = 64` remains the hard limit with exhaustion diagnostics.
- **Phase 54a Track A** — CLOEXEC/NONBLOCK plumbing gap. `open`/`openat`/`openat2`/`vfs_service_open` silently drop `O_CLOEXEC`. Planned but not started.
- **Phase 54a Track B** — Four `arch::x86_64::syscall::*_pub` wrappers in `kernel/src/process/mod.rs`. Planned but not started.
- **MOUNT_OP_LOCK yielding** — long-term deferred in `docs/debug/54-followups.md`, no owner phase.
- **Scheduler diagnostic threshold tuning** — tracked in `docs/debug/54-followups.md`, no owner phase.
- **Full epoll module extraction** — Phase 54a only moves the cleanup helper; full extraction deferred.
- **`AGENTS.md` version string** — stale at `v0.51.0`, correction is an acceptance item in Phase 54a Task C.4 (not yet done).
- **virtio_blk/virtio-net IRQ-driven completion** (spin-poll replacement) — routed to Phase 55 Track C.5.
- **`/var/run → /run` symlink** — routed to Phase 45 deferred list.
- **Memory pressure shrinker interface** — deferred from 53a to Phase 54; not observed as delivered in Phase 54.
- **Broader filesystem extraction beyond `/etc/...` read-only slice** — Phase 54 moved one slice; full storage extraction deferred.
- **Full network-service ecosystem** — Phase 54 moved UDP policy only; TCP and other protocols remain in-kernel.
- **Complete POSIX policy removal from kernel** — long-term deferred.
- **Pre-seeded image password hashing** — Phase 48 upgraded `passwd`/`adduser` to iterated SHA-256i, but the pre-seeded default image still ships with single-iteration SHA-256 hashes for root and user accounts (accepted behavior documented in Phase 53 regression).
