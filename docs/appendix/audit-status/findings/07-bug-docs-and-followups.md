# Findings: Bug Investigation Docs and Open Follow-ups

## Per-doc summary

### `cc-ssh-bug-analysis.md`

**Problem:** Shell output never reaches the SSH client after authentication in the multi-task sshd architecture; pressing a key occasionally nudges data through.

**Resolution status:** Partially Resolved — the doc carries a historical-note header stating "treat it as archived analysis rather than the current implementation status," implying fixes landed, but several sub-issues it raised were addressed at different times and with different completeness (see below).

**Phase tie:** Phase 43 (SSH Server) / Phase 43a (Crash Diagnostics)

**Closing reference:** No single closing commit cited in this document. The doc references commit `6f97aaf` (in `kernel-race-debugging-strategy.md`) for the sshd async wakeup fix. The scheduler root cause (`SCHEDULER.lock` IRQ-safety) was fixed and validated separately (see `scheduler-fairness-regression.md`).

**Open follow-ups:**
- Fix 3 (replace `yield_once()` with a `progress_notify` signal) — documented as needed but no confirmation it was applied.
- Fix 4 (mutex liveness bug in `userspace/async-rt/src/sync/mutex.rs:80-84`) — the doc explicitly calls it a latent correctness bug that will deadlock if any future code adds `.await` inside a lock scope. No closing reference.
- Fix 5 (handle EAGAIN in PTY write direction 1, silent data loss) — labelled a "data integrity bug" with no closing reference.
- Optional targeted debug logging — noted as useful if write-waker fix was insufficient.

---

### `copy-to-user-reliability-bug.md`

**Problem:** `copy_to_user` sometimes writes correct data to the physical frame but userspace reads stale zeros from the same virtual address; caused intermittent termios corruption cascades.

**Resolution status:** Partially Resolved — Phase 52a fixed the stale per-core state vector (`restore_caller_context` added to all seven IPC blocking syscalls and FUTEX_WAIT). The doc explicitly states "the underlying `copy_to_user` physical-vs-virtual address divergence remains a separate open question for Phase 52b (task-owned return state)."

**Phase tie:** Phase 52 / Phase 52a (Kernel Reliability Fixes)

**Closing reference:**
- `cd5bc5b` — size_of fix (red herring)
- `683c017` — debug logging
- `c316a3b` — compiler_fence (no effect)
- `96b3240` — read_volatile (no effect)
- `3c172fd` — register-return workaround syscalls
- Phase 52a: `restore_caller_context` added to IPC dispatch + futex WAIT

**Open follow-ups:**
- Tasks 1–7 are all still listed as "Investigation Tasks" without any closure markers — covering: minimal test binary, kernel-side verification readback, TLB flush after write, SMP TLB shootdown audit, single-core QEMU test, `get_mapper()` audit during copy, ABA race audit in page frame reuse.
- The root divergence between physical-mapping write and virtual-address read is explicitly deferred to Phase 52b.
- Register-return syscalls (`GET_TERMIOS_LFLAG`, etc.) are marked `#[deprecated]` as temporary compatibility stubs — these are not removed.

---

### `kernel-race-debugging-strategy.md`

**Problem:** A still-intermittent kernel fork/scheduler handoff failure (kernel instruction-fetch fault at `RIP=0x4`) after overlapping SSH sessions, plus a methodology gap (no structured kernel trace ring, no looped SMP stress lane, no dual-session regression test).

**Resolution status:** Partially Resolved — the sshd async wakeup bug is confirmed fixed (`6f97aaf`). The kernel fork/scheduler handoff race was still intermittent on the clean branch as of the doc date (2026-04-05) and the doc explicitly says "the branch is not fully stable yet." The scheduler ISR-deadlock root cause was separately resolved later (2026-04-21 via `IrqSafeMutex`), but the doc's specific fork-handoff crash path is not shown to be closed.

**Phase tie:** Phase 43 / Phase 43a (Crash Diagnostics) / Phase 43b (Kernel Trace Ring) / Phase 43c (Regression & Stress)

**Closing reference:** Partial — the scheduler fix (IrqSafeMutex) closed the dominant wedge. The doc's recommended trace ring, dual-session regression, loom/proptest coverage, and lockdep-lite became the scope of Phases 43b and 43c (both listed Complete in the README).

**Open follow-ups (as stated in the doc's concrete recommendations):**
- Add a kernel trace ring for fork/scheduler/IPC events — became Phase 43b (Complete per README).
- Promote dual-session SSH overlap flow into `xtask` as a named regression — became Phase 43c (Complete per README).
- Add a looped SMP stress lane — became Phase 43c (Complete per README).
- Add `xtask` debug modes for QEMU gdbstub with suggested breakpoints.
- Extract more fork/scheduler/IPC logic into host-testable code with proptest + loom coverage — scope of Phase 43c.
- Keep `fix/fork-handoff-investigation-snapshot` split from clean branch and reintroduce in smaller slices — outcome unclear from docs alone.
- The specific `RIP=0x4` kernel fork/handoff crash: no explicit closing reference in this doc. Whether the IrqSafeMutex fix and the Phase 43b/c infrastructure fully closed this failure mode is not confirmed by any document in the set.

---

### `pr-116-review.md`

**Problem:** PR #116 (Phase 55b ring-3 driver host) was found "not ready" due to three blockers (permissionless `sys_device_claim`, cap-table rollback nukes all device claims for PID, write grant consumed before restart logic), one fix-now item (driver runtime drops real bulk length), and one follow-up (Mmio cap field lying about ownership).

**Resolution status:** Resolved — the doc's Resolution section (dated 2026-04-20) states every item above was triaged and either fixed or addressed, bringing the verdict to "resolved." All five items (three blockers, one fix-now, one follow-up) have documented fixes.

**Phase tie:** Phase 55b (Ring-3 Driver Host)

**Closing reference:** Resolution section dated 2026-04-20; references companion `docs/appendix/phase-55b-adversarial-review.md` where overlapping findings were closed by shared changes. Validation: `cargo xtask check`, `cargo test -p kernel-core`, `cargo xtask test` (QEMU), and `cargo xtask image` all pass.

**Open follow-ups:** None — the doc explicitly states `final-gate-result: resolved` with zero re-review loops on any item. The one follow-up (Mmio cap field) was addressed by renaming and adding a `device_cap()` accessor; `cap()` retained as deprecated alias.

---

### `register-capture-design.md`

**Problem:** The Phase 43a `dump_crash_context()` register capture has two known limitations: caller-saved registers may not reflect fault-time values, and RBX/RBP use shared global statics with a race window on concurrent panics.

**Resolution status:** Workaround — the inline-asm approach was chosen as the fastest path to useful crash output. Three future phases are described (assembly entry stub capture, naked panic wrapper, NMI cross-CPU capture) but all are explicitly deferred.

**Phase tie:** Phase 43a (Crash Diagnostics)

**Closing reference:** Decision log entry 2026-04-05 documents each deferral reason.

**Open follow-ups:**
- Phase 1 (recommended next): Modify IDT exception entry stubs to save all GPRs into `RegisterFrame` before calling Rust handler — eliminates LLVM reserved-register problem and register drift. Deferred because it requires IDT restructuring beyond Phase 43a scope.
- Phase 2: Naked function wrapper around panic entry point for non-hardware panics.
- Phase 3: NMI-based cross-CPU register capture — highest value for SMP race debugging, highest complexity; NMI handler not yet present in m3OS at doc date.
- The SMP race on shared `SNAP_RBX`/`SNAP_RBP` statics is explicitly accepted as a known tradeoff.

---

### `scheduler-fairness-regression.md`

**Problem:** SSH sessions wedge at multiple points (before key exchange, during KEX, before password prompt, after login, after first typed input) due to scheduler starvation; virtio-net ISR calls `wake_task` while the same core holds `SCHEDULER.lock` (plain `spin::Mutex`), causing IRQ re-entry deadlock.

**Resolution status:** Resolved — the doc's header and the closing "Early-wedge" section confirm root cause found and fixed 2026-04-21. `SCHEDULER: Mutex<Scheduler>` converted to `SCHEDULER: IrqSafeMutex<Scheduler>`; 15/15 clean validation runs post-fix (pre-fix baseline 30-40%).

**Phase tie:** Phase 55b / Phase 52c (Kernel Architecture Evolution) — the doc notes Phase 52c already planned per-core scheduler evolution.

**Closing reference:** Post-mortem at `docs/post-mortems/2026-04-21-scheduler-lock-isr-deadlock.md`. In-doc fix description: `IrqSafeMutex`, `without_interrupts` wrapper on `enqueue_to_core`, wake path folded to single lock acquisition.

**Open follow-ups (explicitly stated in the doc as still remaining after root-cause fix):**
- H6 patch (wake-ping-pong suppression in `userspace/sshd/src/session.rs`) — "still worth landing as a semantic improvement" but not confirmed as shipped.
- H9 late-wedge: attributed to SSH protocol-layer issue — `sunset-local/` does not advance KEX after application provides host keys; "future work belongs in `sunset-local/`." Explicitly not closed.
- Early-wedge variant where the SYN never reaches `handle_tcp` (0 `[tcp-wake]` calls): "likely a missed virtio-net IRQ at first-packet time or a QEMU user-mode hostfwd race — not addressed by H6/H8 fixes." Explicitly open.
- H9 sub-hypotheses (wake-chain broken / stuck inside flush / stuck inside `runner.lock`): "remain uncaptured — any late-wedge captured with instrumentation would pin exactly one of them."
- `execve` does not overwrite task debug name — tasks display `fork-child` even after `execve` to sshd/syslogd (noted as a minor pre-existing bug).

---

### `scheduler-fairness-h9-resume.md`

**Problem:** Resume pointer for the H1–H9 SSH-wedge investigation while it was open; now superseded.

**Resolution status:** Resolved — the doc states "Resolved 2026-04-21. Superseded by the post-mortem." It redirects to `docs/post-mortems/2026-04-21-scheduler-lock-isr-deadlock.md`.

**Phase tie:** Phase 55b (same investigation as `scheduler-fairness-regression.md`)

**Closing reference:** Post-mortem `docs/post-mortems/2026-04-21-scheduler-lock-isr-deadlock.md`.

**Open follow-ups:** The in-tree regression harness (`scripts/ssh-wedge-regression.sh`, `scripts/ssh-wedge-regression-batch.sh`) is documented as available, but `class=late-wedge` (no longer observed post-fix) and `class=boot-failed` paths are not explicitly confirmed closed.

---

### `sshd-hang-analysis.md`

**Problem:** Post-authentication hang where shell output never reaches the SSH client; missing channel write waker registration in the PTY relay, plus a concrete bug in vendored sunset's `Channel::wake_write()` firing `read_waker` instead of `write_waker`.

**Resolution status:** Partially Resolved — the doc's historical note states "this analysis captures the suspected root cause before the final session fix landed later the same day." The primary write-waker fix is implied to have landed. However, the sunset `wake_write()` vendor bug (`sunset-local/src/channel.rs:840-845`) may persist in the vendored copy regardless of the session-side fix.

**Phase tie:** Phase 43 (SSH Server)

**Closing reference:** Fix landed 2026-04-04 (same day as doc date). The doc references the async executor producing `channel_relay_task` drop bug as separately fixed. No specific commit cited in this document.

**Open follow-ups:**
- Upstream sunset `wake_write()` bug — doc notes "the same `wake_write()` shape appears in upstream `mkj/sunset`" so this "may be a vendored upstream bug rather than a branch-only local edit." Whether the vendored copy was patched is not confirmed.
- Focused regression test for the write-backpressure failure mode (fill conditions so `write_channel()` returns `Ok(0)`, release capacity, assert writer-side task wakes without client input) — listed as recommended but no confirmation it was added.
- QEMU smoke step verifying first shell prompt reaches the SSH client without extra keystroke — recommended but not confirmed.

---

### `sshd-multi-task-debug.md`

**Problem:** Historical debug handoff documenting the state before the post-authentication shell-output hang was fixed; captures the multi-task session architecture and five attempted fixes that did not resolve the final hang.

**Resolution status:** Resolved (historically) — the doc's status field states "The post-auth shell-output hang described here was fixed later on 2026-04-04; this document captures the branch-local state before that fix landed."

**Phase tie:** Phase 43 (SSH Server)

**Closing reference:** Fixed 2026-04-04; exact commit not cited in this document (see `cc-ssh-bug-analysis.md` and `kernel-race-debugging-strategy.md` for context commits `6f97aaf`, `004046f`, etc.).

**Open follow-ups:** This doc is historical; no open items are tracked here independently. The issues it raised were resolved or tracked in successor documents.

---

### `phase-21-handoff.md`

**Problem:** Phase 21 Ion shell integration — fork child caller-saved register corruption causing musl C binary crashes, plus ion interactive mode blocked on termios (`tcsetpgrp` returning ENOTTY).

**Resolution status:** Partially Resolved — Blocker 1 (register corruption) is marked RESOLVED with commit `b6af358`. Blocker 2 (ion interactive/script mode) is explicitly "Deferred to Phase 22" and was a known open item at handoff time.

**Phase tie:** Phase 21 (Ion Shell Integration) / Phase 22 (TTY and Terminal Control)

**Closing reference:** Commit `b6af358` (fork child register fix). PR #27 (`docs/phase-21-ion-shell`).

**Open follow-ups (deferred to Phase 22):**
- Ion interactive mode (raw-mode line editing, history, tab completion).
- Ion script mode (`ion -c`) — needs `tcsetpgrp` to succeed.
- `isatty()` returning true for console fd.
- Ion's liner library TTY handling.
- Interactive acceptance tests P21-T028, T030-T034, T038-T044.
- Note: The README shows Phase 22 (TTY and Terminal Control) as Complete, so these deferred items are nominally addressed — but this doc predates that completion and cannot confirm it.

---

### `state-analysis-march-2026.md`

**Problem:** Multi-model state analysis from March 2026 cataloguing critical gaps blocking interactive use: no `getdents64`, frame allocator never frees, no userspace shell/init, `chdir`/`getcwd` stubs, fixed kernel heap, missing exception handlers, IPC security bug, yield-loop blocking, and more.

**Resolution status:** Largely Stale — this is a snapshot from March 2026 describing the state at approximately Phase 14–17. The current README (May 2026) shows Phases 1–57d complete or in progress, meaning most items the doc lists as missing are now addressed.

**Phase tie:** Cross-cutting; primarily Phases 14–25 and their gap backlog.

**Closing reference:** Not applicable — this is an analysis document, not a bug report with a fix.

**Open follow-ups:** See "Stale claims" table below for doc-vs-README discrepancies.

---

## Stale claims in `state-analysis-march-2026.md`

| Doc claim (March 2026) | Current README claim (May 2026) | Discrepancy nature |
|---|---|---|
| SMP: Phase 17 = 0% complete; "AP startup, per-CPU queues, IPIs" all missing | Phase 25 (SMP): **Complete**; Phase 35 (True SMP): **Complete** | Doc predates both phases by a wide margin — fully stale |
| TTY/PTY: Phase 18 = 0% complete; "line discipline, PTY allocation" all missing | Phase 22 (TTY and Terminal Control): **Complete**; Phase 29 (PTY Subsystem): **Complete** | Doc predates both phases — fully stale |
| Persistent storage: Phase 19 = 0% complete; "ATA/AHCI, FAT32/ext2 driver" missing | Phase 24 (Persistent Storage, virtio-blk + FAT32): **Complete**; Phase 28 (ext2): **Complete** | Doc predates both phases — fully stale |
| "No userspace shell" (ring-0 kernel shell only) | Phase 20 (Userspace Init and Shell): **Complete**; Phase 21 (Ion Shell): **Complete** | Doc predates both — fully stale |
| "No userspace init (PID 1)" | Phase 20 (Userspace Init and Shell): **Complete** | Fully stale |
| `getdents64` returns ENOSYS — "ls broken" | Phase 18 (Directory and VFS): **Complete** — includes `getdents64`, directory fds, real cwd | Fully stale |
| Frame allocator never frees — OOM after ~20 fork/exec cycles | Phase 17 (Memory Reclamation): **Complete** — includes free-list allocator, CoW fork | Fully stale |
| No CoW fork — "eager full copy" | Phase 17: **Complete** (CoW fork included) | Fully stale |
| `chdir`/`getcwd` stubs (always `/`) | Phase 18: **Complete** — includes real cwd | Fully stale |
| Kernel heap fixed at 1 MiB | Phase 17: **Complete** (heap growth included); Phase 33 (Kernel Memory): **Complete** — buddy allocator | Fully stale |
| Missing exception handlers #0 (divide) and #6 (invalid opcode) → kernel halt | Phase 43a (Crash Diagnostics): **Complete** — enriched fault handlers | Likely addressed, though doc only confirms Phase 43a scope broadly |
| IPC registry `ipc_register/lookup_service` dereferences userspace ptr without `copy_from_user` (security bug) | Phase 49 (Architectural Declaration): **Complete** — makes kernel/userspace boundary explicit and enforceable | Stale; addressed by Phase 48/49 security work |
| No `mprotect` | Phase 36 (Expanded Memory): **Complete** — includes mprotect | Fully stale |
| No `poll`/`select`/`epoll` | Phase 37 (I/O Multiplexing): **Complete** | Fully stale |
| No socket syscalls / network unreachable from userspace | Phase 23 (Socket API): **Complete** | Fully stale |
| No persistent block device — all data lost on reboot | Phase 24 (Persistent Storage): **Complete** | Fully stale |
| Signal trampolines / `sigreturn` missing | Phase 19 (Signal Handlers): **Complete** | Fully stale |
| No `clone` (threads) | Phase 40 (Threading): **Complete** | Fully stale |
| Capability grants missing (`sys_cap_grant` not exposed) | Phase 50 (IPC Completion): **Complete** — capability grants included | Fully stale |
| Kernel stacks never freed on process exit | Phase 17 (Memory Reclamation): **Complete** — stack cleanup included | Fully stale |
| `static mut` syscall globals — NMI/nested interrupt corruption risk | Phase 52b (Kernel Structural Hardening): **Complete** — includes typed UserBuffers, structural hardening | Likely addressed |
| `get_mapper()` creates `&'static mut PageTable` — aliasing UB | Phase 52b: **Complete**; Phase 52d: **Complete** | Likely addressed by structural hardening |
| xtask silently creates empty ELFs if musl-gcc absent | Phase doc recommends fail-fast; no explicit confirmation of fix | May still be present — no README phase directly addresses this specific xtask behavior |
| Yield-loop blocking (`stdin`/`waitpid`/`nanosleep` burn 100% CPU) | Phase 52c (Kernel Architecture Evolution): **Complete** — includes ISR wakeup and deferred scheduler closure | Likely addressed |

---

## Open follow-ups (cross-cutting)

The following items are explicitly tracked as still open across the documents reviewed:

- **`copy_to_user` physical-vs-virtual divergence root cause** (`copy-to-user-reliability-bug.md`) — seven investigation tasks (minimal test binary, kernel-side readback, TLB flush, SMP TLB shootdown audit, single-core test, `get_mapper()` audit, ABA frame-reuse race) remain unconfirmed closed; deferred to Phase 52b, which README marks Complete but the bug doc does not confirm resolution.
- **`async-rt` mutex liveness bug** (`cc-ssh-bug-analysis.md`, Fix 4) — `userspace/async-rt/src/sync/mutex.rs:80-84` latent deadlock if any future code adds `.await` inside a lock scope. No closing reference in any doc.
- **Silent EAGAIN data loss in PTY write direction 1** (`cc-ssh-bug-analysis.md`, Fix 5) — unwritten bytes silently dropped when non-blocking PTY write returns EAGAIN. No closing reference.
- **`progress_notify` signal / yield-once busy-loop replacement** (`cc-ssh-bug-analysis.md`, Fix 3) — progress task busy-loops via `yield_once()`, monopolising executor cycles; upstream sunset-async uses a proper `Notify`. No closing reference.
- **Vendored `sunset-local` `Channel::wake_write()` bug** (`sshd-hang-analysis.md`) — `self.read_waker.take()` should be `self.write_waker.take()`; may be upstream origin. No confirmation the vendored copy was patched.
- **H9 late-wedge: SSH KEX stall after host keys provided** (`scheduler-fairness-regression.md`) — attributed to `sunset-local/` not advancing KEX; explicitly deferred to future work in `sunset-local/`. Not closed.
- **Early-wedge SYN-never-reaches-handle_tcp variant** (`scheduler-fairness-regression.md`) — likely missed virtio-net IRQ at first-packet time or QEMU user-mode hostfwd race; not addressed by the IrqSafeMutex fix.
- **`execve` does not overwrite task debug name** (`scheduler-fairness-regression.md`) — forked-then-execve'd tasks keep `fork-child` label in scheduler warnings; noted as a pre-existing minor bug, no fix referenced.
- **IDT assembly entry stub register capture** (`register-capture-design.md`) — recommended Phase 1 improvement to eliminate LLVM reserved-register problem and register drift at fault point; deferred, no closing reference.
- **NMI cross-CPU register capture** (`register-capture-design.md`) — highest-value SMP debugging improvement; NMI handler not yet present at doc date; deferred, no closing reference.
- **`55c-net-remote-rx-test-bug.md`** — three RX-path tests in `kernel/src/net/remote.rs` use `encode_net_send` where they should use `encode_net_rx_notify`, causing all three tests to fail. Status: **Open**. Estimated fix: ~20 minutes, 6-line diff. Cannot be merged until PR #124 (frame_allocator fix) lands on `main` first.
- **Ion interactive mode deferred items** (`phase-21-handoff.md`) — ion script mode (`ion -c`), `isatty()` for console fd, liner library TTY handling, interactive acceptance tests P21-T028/T030-T034/T038-T044. Phase 22 is Complete per README, so these are likely addressed, but the handoff doc does not confirm it.
- **`sys_device_claim` path guard is `/drivers/` prefix only** (`pr-116-review.md` Resolution) — the implemented authorization gate is an `exec_path.starts_with("/drivers/")` check; no capability model depth is described. Scope of the follow-up (capability-table depth vs. path-prefix model) is not closed.

---

## Bug-doc-implied technical debt

The following issues are admitted as not fully closed by the documents themselves, synthesised across the full doc set:

1. **`copy_to_user` root cause unknown** — The per-core stale-state fix (Phase 52a) addressed one confirmed vector. The physical-frame-vs-virtual-address divergence remains theoretically possible; no test binary was created to isolate and measure the residual failure rate.

2. **SMP TLB coherency not audited** — `copy-to-user-reliability-bug.md` Task 4 explicitly lists all page-table-modifying code paths and asks whether per-core `invlpg` + IPI shootdown are correct. This audit was never confirmed complete.

3. **`async-rt` mutex third-branch waker loss** — A latent deadlock will fire as soon as any future sshd/async-rt code adds `.await` inside a lock scope. The workaround (no current code yields while holding the lock) is fragile and undocumented as a coding constraint.

4. **Progress task busy-loop in sshd executor** — The `yield_once()` pattern keeps the executor run-queue permanently non-empty, preventing the blocking reactor poll from ever being reached. This is structurally different from the upstream sunset-async design.

5. **Vendored `sunset-local` waker bug** — If the `Channel::wake_write()` waker-field bug was not patched in the local vendor copy, channel write-side wake events silently misdirect to read-side waiters. This would resurface under any write-backpressure condition.

6. **SSH KEX stall (`sunset-local` KEX advancement)** — After H9 attribution, the late-wedge SSH hang is now understood to be a protocol-layer issue in `sunset-local/` that is separate from both the scheduler and async-rt layers. No fix is assigned.

7. **Missed virtio-net IRQ at first-packet time** — The early-wedge variant (SYN never arrives at `handle_tcp`) is a separate hardware/QEMU timing interaction that was not addressed by the IrqSafeMutex fix. Severity and frequency are unknown post-fix.

8. **IDT fault-point register capture** — The current `dump_crash_context()` approach captures registers at the handler call site, not at the fault point. Caller-saved registers are unreliable for diagnosis of ISR-level crashes. The recommended IDT stub approach is still deferred.

9. **`55c-net-remote-rx-test-bug`** — Three kernel unit tests in `net::remote` are definitively broken (wrong encoder). They have been silently suppressed by a pre-existing frame-allocator test failure. Once the frame-allocator fix lands on `main`, these will surface as a CI failure if not fixed first.

10. **Fork/scheduler handoff `RIP=0x4` crash** — `kernel-race-debugging-strategy.md` describes an intermittent kernel instruction-fetch fault during overlapping SSH sessions. The doc recommends splitting the snapshot branch into four tracks (fork handoff publication, scheduler switch-out safety, IPC lost-wakeup, regression-only). Whether these tracks were fully applied and validated is not confirmed in any doc in the reviewed set.
