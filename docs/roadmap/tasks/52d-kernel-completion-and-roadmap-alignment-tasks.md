# Phase 52d — Kernel Completion and Roadmap Alignment: Task List

**Status:** In Progress
**Source Ref:** phase-52d
**Depends on:** Phase 52a (Kernel Reliability Fixes) ✅, Phase 52b (Kernel Structural Hardening) ✅, Phase 52c (Kernel Architecture Evolution) ✅
**Goal:** Close the audit-verified gaps between the documented outcomes of Phases 52a/52b/52c and the current implementation, then restore trustworthy smoke and regression gates for the Phase 52 kernel surface.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Audit-backed roadmap alignment | — | In Progress |
| B | Task-owned return-state completion | — | Planned |
| C | Keyboard input convergence | B | Planned |
| D | Scheduler and notification scope reconciliation | A | Planned |
| E | Validation and release-gate repair | B, C, D | Planned |

---

## Track A — Audit-Backed Roadmap Alignment

### A.1 — Record the audited status of 52a/52b/52c

**Files:**
- `docs/roadmap/52a-kernel-reliability-fixes.md`
- `docs/roadmap/52b-kernel-structural-hardening.md`
- `docs/roadmap/52c-kernel-architecture-evolution.md`
- `docs/roadmap/README.md`

**Symbol:** phase headers, milestone summary rows, dependency map
**Why it matters:** The roadmap must stop claiming more than the code currently delivers. Engineers should be able to read the 52-series docs and understand which parts are complete, which were superseded, and which still require work.

**Acceptance:**
- [x] 52a notes that the manual IPC/futex restore pattern was superseded by the 52b return-state design
- [x] 52b notes which Track C items remain partial in the checked-in code
- [x] 52c no longer implies that the keyboard path, scheduler hot path, or notification pool are fully complete if they remain open
- [x] `docs/roadmap/README.md` includes Phase 52d in both the dependency map and milestone summary

### A.2 — Add explicit regression coverage for exec-time signal reset

**Files:**
- `userspace/signal-test/signal-test.c`
- `xtask/src/main.rs`

**Symbol:** `main`, new exec-reset test helper, smoke/regression registration if needed
**Why it matters:** Phase 52a implemented the POSIX rule that caught signal handlers reset across `exec`, but the current `signal-test` binary does not exercise that behavior directly.

**Acceptance:**
- [x] `signal-test` installs a handler, forks or spawns an exec path, and proves the exec'd program does not inherit the handler
- [x] The failure mode distinguishes signal-reset bugs from generic exec failure
- [x] The test is wired into an existing validation path (`signal-test`, smoke, or regression)

---

## Track B — Task-Owned Return-State Completion

### B.1 — Save user return state at syscall entry

**Files:**
- `kernel/src/task/mod.rs`
- `kernel/src/arch/x86_64/syscall/mod.rs`
- `kernel/src/task/scheduler.rs`

**Symbol:** `UserReturnState`, `syscall_handler`, `save_user_return_state`
**Why it matters:** The 52b design says user return state should be captured once at syscall entry before any blocking can occur. The current implementation still treats block/yield paths as the primary save points.

**Acceptance:**
- [ ] `UserReturnState` contains every field required by the chosen syscall-return contract
- [ ] `syscall_handler` snapshots the state once before any blocking or yield path
- [ ] Block/yield sites no longer act as the primary source of truth for userspace resume state

### B.2 — Make scheduler dispatch the authoritative restore path

**Files:**
- `kernel/src/task/scheduler.rs`
- `kernel/src/process/mod.rs`
- `kernel/src/smp/mod.rs`

**Symbol:** `run`, dispatch restore block, `PerCoreData`
**Why it matters:** The scheduler currently restores only part of the return state from `Task.user_return` and still sources `kernel_stack_top` / `fs_base` from `Process`. That split keeps the architecture in a half-migrated state.

**Acceptance:**
- [ ] Scheduler dispatch restores `syscall_user_rsp`, syscall stack/TSS state, `FS.base`, and CR3 from one coherent task-owned or task-associated contract
- [ ] `Task.user_return` and `Process` no longer split ownership of thread-local return-state fields
- [ ] Address-space and resume-state invariants are enforced in non-debug builds through warnings or hard failures

### B.3 — Activate generation tracking for mapping mutations and user-copy diagnostics

**Files:**
- `kernel/src/mm/mod.rs`
- `kernel/src/mm/user_mem.rs`
- `kernel/src/arch/x86_64/syscall/mod.rs`
- `kernel/src/arch/x86_64/interrupts.rs`

**Symbol:** `bump_generation`, `copy_to_user`, `copy_from_user`, `sys_linux_munmap`, `sys_mprotect`, `resolve_cow_fault`
**Why it matters:** 52b added the `AddressSpace::generation` mechanism but left it effectively dormant. Without generation bumps and checks, the diagnostic path for mapping divergence remains mostly theoretical.

**Acceptance:**
- [ ] Mapping-changing operations bump the address-space generation counter
- [ ] User-copy paths can detect or report generation divergence during a copy
- [ ] The related docs explain what a generation mismatch means and how to reproduce it

---

## Track C — Keyboard Input Convergence

### C.1 — Simplify `stdin_feeder` to a raw-input bridge

**Files:**
- `userspace/stdin_feeder/src/main.rs`
- `userspace/syscall-lib/src/lib.rs`

**Symbol:** `main`, `push_raw_input`
**Why it matters:** 52c introduced `LineDiscipline` and `push_raw_input`, but the live keyboard path still duplicates terminal policy in userspace. That duplication undermines the intent of the phase and keeps the keyboard path coupled to workaround syscalls.

**Acceptance:**
- [ ] `stdin_feeder` no longer calls `get_termios_lflag`, `get_termios_iflag`, or `get_termios_oflag`
- [ ] `stdin_feeder` no longer implements `ICANON`, `ISIG`, echo, `ICRNL`, or canonical-editing behavior
- [ ] `stdin_feeder` only decodes scancodes or escape sequences and forwards raw bytes via `push_raw_input`

### C.2 — Remove or isolate workaround-only termios return syscalls

**Files:**
- `kernel/src/arch/x86_64/syscall/mod.rs`
- `userspace/syscall-lib/src/lib.rs`
- `docs/appendix/copy-to-user-reliability-bug.md`

**Symbol:** `GET_TERMIOS_LFLAG`, `GET_TERMIOS_IFLAG`, `GET_TERMIOS_OFLAG`
**Why it matters:** These special register-return syscalls were introduced as a workaround for the earlier `copy_to_user` investigation. Once the in-tree keyboard path no longer depends on them, they should either disappear or be clearly documented as temporary compatibility interfaces.

**Acceptance:**
- [ ] No in-tree binary depends on the register-return termios workaround syscalls
- [ ] The workaround syscalls are either deleted or documented as temporary compatibility only
- [ ] Keyboard login and shell behavior remain compatible with the tty expectations in smoke and regression tests

---

## Track D — Scheduler and Notification Scope Reconciliation

### D.1 — Reconcile the scheduler hot-path claim with the actual implementation

**Files:**
- `kernel/src/task/scheduler.rs`
- `docs/roadmap/52c-kernel-architecture-evolution.md`
- `docs/roadmap/tasks/52c-kernel-architecture-evolution-tasks.md`
- `docs/roadmap/52d-kernel-completion-and-roadmap-alignment.md`

**Symbol:** `SCHEDULER`, `run`, `pick_next`, `wake_task`
**Why it matters:** The 52c roadmap claimed that scheduler dispatch no longer acquires a global lock, but the current hot path still uses `SCHEDULER.lock()` throughout wake, drain, pick, and dispatch.

**Acceptance:**
- [ ] Either the global scheduler lock is removed from the hot path, or the roadmap explicitly re-defers true per-core scheduling
- [ ] The chosen scope is reflected consistently in the code comments, 52c docs, and 52d docs
- [ ] Load-balancing and stealing behavior are documented against the chosen design

### D.2 — Reconcile notification-pool claims with ISR-safe constraints

**Files:**
- `kernel/src/ipc/notification.rs`
- `docs/roadmap/52c-kernel-architecture-evolution.md`
- `docs/roadmap/tasks/52c-kernel-architecture-evolution-tasks.md`

**Symbol:** `MAX_NOTIFS`, `WAITERS`, `ALLOCATED`, `IsrWakeQueue`
**Why it matters:** 52c claimed dynamic notification pools, but the live implementation still uses fixed-size arrays to preserve ISR-safe behavior. The roadmap should either finish that design or document the fixed-size constraint honestly.

**Acceptance:**
- [ ] Notification allocation either becomes growable or remains fixed with an explicit documented reason
- [ ] The notification-capacity and ISR-wakeup model are described consistently in code and roadmap docs
- [ ] Exhaustion behavior is covered by a test, diagnostic, or documented limit

---

## Track E — Validation and Release-Gate Repair

### E.1 — Repair smoke and targeted regressions for the failing Phase 52 flows

**Files:**
- `xtask/src/main.rs`
- `userspace/fork-test/src/main.rs`
- `userspace/pty-test/src/main.rs`
- `userspace/stdin_feeder/src/main.rs`

**Symbol:** `smoke_test_script`, regression registry, `test_dual_ion_prompts`, `test_ion_prompt`
**Why it matters:** The Phase 52 failures currently surface as boot/login/fork/ion/PTTY smoke failures. The harness must preserve enough signal to distinguish real kernel/runtime bugs from capture noise or stale expectations.

**Acceptance:**
- [ ] Smoke covers the boot/login/fork/ion/PTTY path without depending on known flaky matches
- [ ] Targeted regressions exist for the keyboard input path and the exec signal-reset path
- [ ] Smoke and regression logs preserve enough context to distinguish harness failures from kernel/runtime failures

### E.2 — Close Phase 52d on release-gate evidence

**Files:**
- `.github/workflows/pr.yml`
- `.github/workflows/build.yml`
- `docs/roadmap/52d-kernel-completion-and-roadmap-alignment.md`

**Symbol:** smoke/regression workflow steps, acceptance section
**Why it matters:** 52d is only meaningful if the same gates used in CI are the ones the roadmap treats as closure evidence.

**Acceptance:**
- [ ] `cargo xtask check` passes
- [ ] `cargo xtask smoke-test --timeout 180` passes
- [ ] `cargo xtask regression --timeout 90` passes
- [ ] CI workflow documentation and the 52d acceptance criteria reference the same validation gates

---

## Documentation Notes

- Phase 52d exists because the 52a/52b/52c documents drifted away from the
  implementation in different ways: superseded stop-gaps, partial migrations,
  and overstated completion claims.
- Prefer documenting the real implementation over preserving a cleaner but false
  historical story.
- When a 52c scalability claim is not finished, either complete it here or move
  it to a later phase explicitly; do not leave it silently marked complete.
