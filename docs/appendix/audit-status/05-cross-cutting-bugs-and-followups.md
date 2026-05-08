# 05 — Cross-Cutting Bugs and Open Follow-ups

This document distils the appendix bug-investigation docs and the `follow-ups/` tracker into a single status view. The goal is to identify which bugs are genuinely closed and which are described as fixed but have residual sub-issues that no doc explicitly closes.

Sources: `docs/appendix/cc-ssh-bug-analysis.md`, `copy-to-user-reliability-bug.md`, `kernel-race-debugging-strategy.md`, `pr-116-review.md`, `register-capture-design.md`, `scheduler-fairness-regression.md`, `scheduler-fairness-h9-resume.md`, `sshd-hang-analysis.md`, `sshd-multi-task-debug.md`, `phase-21-handoff.md`, `state-analysis-march-2026.md`, `docs/roadmap/follow-ups/55c-net-remote-rx-test-bug.md`. Synthesised from `findings/07-bug-docs-and-followups.md`.

---

## Per-doc status summary

| Doc | Resolution | Open follow-ups |
|---|---|---|
| `pr-116-review.md` | ✅ Resolved (2026-04-20) | None |
| `scheduler-fairness-regression.md` | ✅ Root cause (IrqSafeMutex, 2026-04-21) — 15/15 clean runs post-fix | H9 late-wedge (KEX stall in `sunset-local`); early-wedge SYN-drop variant; H9 sub-hypotheses uncaptured |
| `scheduler-fairness-h9-resume.md` | ✅ Superseded by post-mortem | — |
| `sshd-multi-task-debug.md` | ✅ Resolved 2026-04-04 (historical) | — |
| `cc-ssh-bug-analysis.md` | 🟡 Partial — primary hang fix landed | Fix 3 (progress_notify), Fix 4 (mutex liveness), Fix 5 (PTY EAGAIN data loss) — no closing references |
| `copy-to-user-reliability-bug.md` | 🟡 Partial — Phase 52a fixed stale per-core state vector | Phys/virt divergence "deferred to Phase 52b" — 7 investigation tasks unclosed; SMP TLB shootdown audit never confirmed |
| `kernel-race-debugging-strategy.md` | 🟡 Partial — sshd async wakeup fix landed | Fork/handoff `RIP=0x4` crash — no explicit closing reference; trace-ring + regression infra became Phases 43b/c (status drift — see audit) |
| `sshd-hang-analysis.md` | 🟡 Partial — session fix landed 2026-04-04 | Vendored `sunset-local::Channel::wake_write()` waker-field bug may not be patched; focused regression test recommended but not confirmed added |
| `register-capture-design.md` | 🟠 Workaround | IDT stub capture (Phase 1), naked panic wrapper (Phase 2), NMI cross-CPU capture (Phase 3) — all deferred |
| `phase-21-handoff.md` | 🟡 Partial — register-corruption fixed | Ion interactive items deferred to Phase 22 (likely addressed but handoff doc cannot confirm) |
| `state-analysis-march-2026.md` | 🟡 Largely stale (March snapshot) | 18 of 19 cited gaps now covered by completed phases; xtask musl-gcc silent-failure item has no confirmed fix |
| `follow-ups/55c-net-remote-rx-test-bug.md` | 🔴 Open | 3 RX-path unit tests use wrong encoder (`encode_net_send` instead of `encode_net_rx_notify`); ~20-min, 6-line fix; blocked on PR #124 frame-allocator landing on `main` |

---

## Bug-doc-implied technical debt (synthesised)

These are issues the docs themselves admit are not fully closed. Each is a candidate for explicit phase ownership.

### 1. `copy_to_user` SMP TLB coherency audit

**Source:** `copy-to-user-reliability-bug.md` Task 4.
**Status:** Never confirmed complete. The document explicitly lists all page-table-modifying code paths and asks whether per-core `invlpg` + IPI shootdown are correct. Phase 52a fixed *one* vector (stale per-core state). The physical-mapping vs. virtual-address divergence remains theoretically possible.
**Why it matters:** This is the diagnostic question for a class of intermittent termios-corruption-style bugs. Without the audit, future analogous bugs will require another full investigation cycle.

### 2. `async-rt` mutex third-branch waker loss

**Source:** `cc-ssh-bug-analysis.md` Fix 4.
**Status:** Latent deadlock. Will fire as soon as any future `sshd` / `async-rt` code adds `.await` inside a lock scope. No closing reference in any doc.
**File:** `userspace/async-rt/src/sync/mutex.rs:80-84`.
**Why it matters:** The current workaround is "no current code yields while holding the lock" — fragile and undocumented as a coding constraint. The next person who adds yielding logic inside the mutex will hit the deadlock with no doc trail.

### 3. Vendored `sunset-local::Channel::wake_write()` waker-field bug

**Source:** `sshd-hang-analysis.md`.
**Status:** May be unpatched. The doc notes `self.read_waker.take()` should be `self.write_waker.take()` and observes the same shape exists in upstream `mkj/sunset` — *"may be a vendored upstream bug rather than a branch-only local edit."* No confirmation the vendored copy was patched.
**File:** `sunset-local/src/channel.rs:840-845`.
**Why it matters:** Under any write-backpressure condition, channel write-side wake events silently misdirect to read-side waiters. The session-side fix that landed 2026-04-04 may be papering over the same root cause from a different angle.

### 4. SSH H9 late-wedge — KEX stall after host keys

**Source:** `scheduler-fairness-regression.md`.
**Status:** Explicitly deferred to "future work in `sunset-local/`". Attributed to `sunset-local/` not advancing KEX after the application provides host keys. Not closed.
**Why it matters:** This is the residual SSH-wedge case after the IrqSafeMutex fix closed the dominant scheduler-deadlock path. The scheduler is now correct; SSH still occasionally hangs. The hang is no longer a scheduler problem and no fix is assigned.

### 5. Early-wedge SYN-drop variant

**Source:** `scheduler-fairness-regression.md`.
**Status:** Open. Variant where SYN never reaches `handle_tcp` (0 `[tcp-wake]` calls). Likely missed virtio-net IRQ at first-packet time or a QEMU user-mode hostfwd race. Not addressed by H6/H8 / IrqSafeMutex fixes.
**Why it matters:** This is a separate hardware/QEMU timing interaction, not a kernel scheduler bug. Severity and frequency are unknown post-fix.

### 6. Fork/scheduler handoff `RIP=0x4` crash

**Source:** `kernel-race-debugging-strategy.md`.
**Status:** Doc describes intermittent kernel instruction-fetch fault during overlapping SSH sessions. Recommended splitting the snapshot branch into 4 tracks (fork-handoff publication, scheduler switch-out safety, IPC lost-wakeup, regression-only). Whether these tracks were fully applied and validated is not confirmed in any reviewed doc.
**Why it matters:** The IrqSafeMutex fix closed the dominant wedge, but the specific `RIP=0x4` failure mode has no explicit closing reference. May still trigger.

### 7. IDT fault-point register capture

**Source:** `register-capture-design.md`.
**Status:** Phase 1 deferred. Current `dump_crash_context()` captures registers at the handler call site, not at the fault point. Caller-saved registers are unreliable for ISR-level crash diagnosis. The recommended IDT stub approach is still deferred.
**Why it matters:** Future kernel-mode crashes — especially under preemption (Phases 57d/57e) — will be harder to diagnose because the captured register state may not reflect fault-time values.

### 8. NMI cross-CPU register capture

**Source:** `register-capture-design.md`.
**Status:** Phase 3 deferred. Highest-value SMP debugging improvement; NMI handler not yet present at doc date. No closing reference.
**Why it matters:** Catching state on a remote core (during e.g., a deadlock) requires NMI; without it, only the local core's state is observable.

### 9. `sys_device_claim` path-prefix authorization

**Source:** `pr-116-review.md` Resolution.
**Status:** Implemented gate is `exec_path.starts_with("/drivers/")`. No capability-model depth. The follow-up scope (capability-table depth vs. path-prefix) is not closed.
**Why it matters:** A driver process can be authorised purely by its launch path. There is no per-device capability the kernel checks against — the path prefix is the trust boundary.

### 10. `progress_notify` signal vs. `yield_once()` busy-loop

**Source:** `cc-ssh-bug-analysis.md` Fix 3.
**Status:** Documented as needed but no confirmation it was applied. Progress task busy-loops via `yield_once()`, monopolising executor cycles; upstream sunset-async uses a proper `Notify`. No closing reference.
**Why it matters:** Structurally different from upstream sunset-async. Performance and correctness implications under load not characterised.

### 11. Silent EAGAIN data loss in PTY write direction 1

**Source:** `cc-ssh-bug-analysis.md` Fix 5.
**Status:** Labelled "data integrity bug" with no closing reference. Unwritten bytes silently dropped when non-blocking PTY write returns EAGAIN.
**Why it matters:** Data integrity issue under PTY backpressure.

### 12. `execve` does not overwrite task debug name

**Source:** `scheduler-fairness-regression.md`.
**Status:** Pre-existing minor bug — forked-then-execve'd tasks keep `fork-child` label in scheduler warnings. No fix referenced.
**Why it matters:** Cosmetic but actively misleading during post-mortem analysis. Easy fix.

### 13. `kernel::net::remote::tests` RX-path encoder mismatch (open)

**Source:** `docs/roadmap/follow-ups/55c-net-remote-rx-test-bug.md`.
**Status:** **Open.** Three test cases in `kernel/src/net/remote.rs` use `encode_net_send` instead of `encode_net_rx_notify`. The bug, root cause, and one-line-per-test fix are documented. Estimated effort: 20 minutes, 6-line diff. Blocked on PR #124 (frame-allocator fix) landing on `main` first.
**Why it matters:** Once the masking failure (frame-allocator) is fixed, these will surface as a CI failure. Time-bomb.

---

## Stale claims in `state-analysis-march-2026.md`

This March-2026 multi-model analysis is largely superseded by subsequent phase work. Listed for completeness — most of these are now addressed:

| Claim (March 2026) | Current state | Status |
|---|---|---|
| SMP 0% complete | Phase 25 + 35 Complete | ✅ stale claim |
| TTY/PTY 0% complete | Phase 22 + 29 Complete | ✅ stale |
| Persistent storage 0% complete | Phase 24 + 28 Complete | ✅ stale |
| No userspace shell | Phase 20 + 21 Complete | ✅ stale |
| `getdents64` returns ENOSYS | Phase 18 Complete | ✅ stale |
| Frame allocator never frees | Phase 17 Complete | ✅ stale |
| No CoW fork | Phase 17 Complete | ✅ stale |
| `chdir`/`getcwd` stubs | Phase 18 Complete (real cwd) | ✅ stale |
| Kernel heap fixed at 1 MiB | Phase 17 + 33 Complete | ✅ stale |
| Missing exception handlers | Phase 43a Complete | likely stale |
| IPC registry security bug | Phase 48 + 49 Complete | likely stale |
| No `mprotect` | Phase 36 Complete | ✅ stale |
| No `poll`/`select`/`epoll` | Phase 37 Complete | ✅ stale |
| No socket syscalls | Phase 23 Complete | ✅ stale |
| No persistent block device | Phase 24 Complete | ✅ stale |
| Signal trampolines / `sigreturn` missing | Phase 19 (claimed) Complete | ⚠️ — see Red Flag #1 |
| No `clone` (threads) | Phase 40 Complete | ✅ stale |
| Capability grants missing | Phase 50 Complete | partial — code still says "Phase 7+ deferred" in `kernel/src/ipc/mod.rs:34-35`; design doc claim and code disagree |
| Kernel stacks never freed | Phase 17 Complete | ✅ stale |
| `static mut` syscall globals | Phase 52b Complete | likely stale |
| `get_mapper()` UB | Phase 52b/d Complete | likely stale |
| **xtask silently creates empty ELFs if musl-gcc absent** | **No phase explicitly addresses this** | 🟡 may still be present |
| Yield-loop blocking | Phase 52c Complete | likely stale |

---

## Synthesis: what's actually open

Discounting stale doc claims and confirmed-resolved items, the cross-cutting open work is:

1. **`copy_to_user` SMP TLB coherency audit** — never confirmed complete (item #1 above)
2. **`async-rt` mutex third-branch waker loss** — latent deadlock, no fix (item #2)
3. **Vendored `sunset-local::wake_write()` waker-field bug** — may be unpatched (item #3)
4. **SSH H9 late-wedge: KEX stall** — deferred to `sunset-local/` future work (item #4)
5. **Early-wedge SYN-drop variant** — open (item #5)
6. **Fork/scheduler handoff `RIP=0x4`** — closing reference unconfirmed (item #6)
7. **IDT stub register capture (Phase 1)** — deferred (item #7)
8. **NMI cross-CPU capture (Phase 3)** — deferred (item #8)
9. **`progress_notify` replacement for `yield_once()`** — unconfirmed (item #10)
10. **PTY write direction 1 EAGAIN data loss** — unconfirmed (item #11)
11. **`execve` debug name overwrite** — pre-existing, unfixed (item #12)
12. **`kernel::net::remote` 3-test encoder bug** — definitely open, time-bomb (item #13)
13. **xtask silent-empty-ELF on missing musl-gcc** — no confirmed fix (state-analysis stale-claims table)

Items 1, 2, 3, 4, 5, 6 cluster around SSH and scheduler/IPC. The IrqSafeMutex fix (2026-04-21) closed the dominant wedge but a long tail of related issues remains. Items 7, 8 are debugging-infrastructure improvements that would help diagnose future occurrences. Items 9, 10, 11 are async-executor and PTY hygiene. Items 12, 13 are easy wins with known fixes.
