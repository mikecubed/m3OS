# Findings: Phases 33–47 (Kernel Infrastructure through DOOM)

Audit date: 2026-05-07. Scope: design docs (`docs/roadmap/NN-slug.md`) and task docs
(`docs/roadmap/tasks/NN-slug-tasks.md`) for phases 33 through 47. No source code was
read; all findings are based on the docs as written.

---

## Phase 33 — Kernel Memory Improvements

**Declared status:** Complete

**Task checkboxes:** 5 unchecked items across Tracks A, C, D, E, G:
- A.4: OOM stress test (`[ ]`)
- C.4: Slab migration — `"Deferred: The slab-cache infrastructure is in place, but the
  broad object-allocation migration from the original plan remains deferred"` (`[ ]`)
- D.4: mmap/munmap loop binary (`[ ]`)
- E.3: Heap coalescing test binary (`[ ]`)
- G.2: Memory stress test (`[ ]`)

**Deferred items** (verbatim from design doc "Shipped state" note and deferred section):
> "Broad migration of hot kernel object families onto slab caches"

The design doc itself carries an explicit audit note: "Shipped state (audited in Phase
53a): The slab-cache infrastructure and direct cache tests landed, but the main hot
kernel object families were not broadly migrated to use it. Most kernel allocations still
flow through the global linked-list heap."

**Documented shortcuts:**
- C.4 is the original plan's main deliverable (slab migration of hot kernel objects).
  The slab *infrastructure* shipped, but no kernel code was migrated onto it.

**Cross-references to later remediation:** Design doc cites a Phase 53a audit of this
gap; the migration remains unresolved in these docs.

**Red flags:**
- The design doc explicitly admits the primary slab-cache goal (C.4, broad migration) was
  not completed. Five out of ~20 tasks are unchecked. The phase is marked Complete despite
  the headline deliverable (migrating kernel allocations to slab caches) being deferred.

---

## Phase 34 — Real-Time Clock

**Declared status:** Complete

**Task checkboxes:** 1 unchecked item:
- E.2: QEMU RTC verification test (`[ ]`) — "Deferred — the RTC is currently verified
  at boot via the log message rather than a dedicated QEMU test"

**Deferred items:**
- "Dedicated QEMU-level test for RTC accuracy" (E.2)

**Documented shortcuts:**
- RTC correctness is verified by observing the boot log message rather than an automated
  QEMU test binary.

**Cross-references to later remediation:** None.

**Red flags:** Minor. One unchecked test item; the functional clock implementation appears
complete.

---

## Phase 35 — True SMP Multitasking

**Declared status:** Complete

**Task checkboxes:** Multiple critical items unchecked:
- E.1: Per-run-queue `queue_length: AtomicU32` counter not added (`[ ]`)
- E.2: Load balancing hook (`maybe_load_balance()`) commented out (`[ ]`) —
  `"Deferred — the scheduler loop currently leaves the maybe_load_balance() hook
  commented out pending the follow-up noted in code"`
- G.2: Pipe wait-queue attachment deferred (`[ ]`)
- G.3: IPC wait-queue attachment deferred (`[ ]`)
- H.3: Child `tms_cutime`/`tms_cstime` stubbed as zero (`[ ]`)

**Deferred items** (from task doc verbatim):
- `maybe_load_balance()` hook is commented out in the scheduler loop
- Per-run-queue length counter not added
- Pipe and IPC wait-queue attachments not connected
- Child CPU times reported as zero

**Documented shortcuts (design doc "Shipped state" note):**
> "Shipped state (audited Phase 52d): Per-core run queues, work-stealing, load-balancing
> with migration cooldown, and dead-slot recycling all landed. However, the global
> `SCHEDULER` lock is still acquired on every dispatch iteration for task-state reads,
> transitions, and post-switch bookkeeping."

This directly contradicts the phase's premise that per-CPU run queues reduce lock
contention on the hot path.

**Note — Phase 25 vs Phase 35 SMP overlap:** Phase 25 established basic multi-core boot
(APs, GDT, IDT, APIC, idle loops). Phase 35 claims to add true per-CPU run queues,
work-stealing, and load balancing. The design doc's own audit note confirms these data
structures were added, but the actual load-balancing dispatch hook remains commented out.
So Phase 35 added the *scaffolding* but not the *behavior* that distinguishes it from
Phase 25's round-robin scheduler.

**Cross-references to later remediation:** Design doc references a Phase 52d audit.

**Red flags:**
- CRITICAL: The primary new behavior of Phase 35 over Phase 25 — tasks distributing
  across cores via load balancing — has its dispatch hook (`maybe_load_balance()`)
  commented out. The phase is marked Complete while the core differentiating feature is
  inoperative.
- The design doc itself admits the global `SCHEDULER` lock is held on every dispatch
  iteration, negating the per-CPU queue contention-reduction benefit.
- Missing template fields: the design doc header lacks a `Status:` field and `Source Ref:`
  field (both required by the doc-template spec in `docs/appendix/doc-templates.md`).

---

## Phase 36 — Expanded Memory Management

**Declared status:** Complete

**Task checkboxes:** All checked. Tracks A–F are fully checked.

**Deferred items:** None explicitly in the design doc's deferred section.

**Documented shortcuts:** None.

**Cross-references to later remediation:** None.

**Red flags:** None. This is one of the cleanest phases in the 33–47 range.

---

## Phase 37 — I/O Multiplexing

**Declared status:** Complete

**Task checkboxes:** All checked. Tracks A–G are fully checked.

**Deferred items:** None explicitly listed in a deferred section.

**Documented shortcuts:**
- Design doc Documentation Notes state: "Timeout support uses tick-based polling
  (~100 Hz / 10ms granularity) since the kernel does not yet have a proper timer
  subsystem. Accuracy is within ~10ms."

This affects the semantics of `poll`, `select`, and `epoll` timeouts across all programs
using I/O multiplexing. Programs that require sub-10ms timeout accuracy will observe
incorrect behavior.

**Cross-references to later remediation:** Note implies a proper timer subsystem is
needed but does not reference a specific later phase.

**Red flags:** The 10ms polling granularity is a documented accuracy gap in a feature
that other phases build on. No later phase is explicitly cross-referenced to fix it.

---

## Phase 38 — Filesystem Enhancements

**Declared status:** Complete

**Task checkboxes:** All checked. An extensive list across Tracks A–H.

**Deferred items:** None explicitly in a deferred section.

**Documented shortcuts:** None.

**Cross-references to later remediation:** None.

**Red flags:** None. Clean phase.

---

## Phase 39 — Unix Domain Sockets

**Declared status:** Complete

**Task checkboxes:** Track J (integration test J.1) has all three acceptance items
unchecked (`[ ]`):
- J.1: "deferred — existing test harness uses kernel-level `#[test_case]` only; userspace
  test binary is built and embedded in initrd for manual/smoke testing"

**Deferred items:**
- Dedicated socket node type in tmpfs/ext2 (design doc: "A dedicated socket node type in
  tmpfs/ext2 is deferred; the path map is the authoritative binding between paths and
  socket handles.")
- Automated QEMU integration test for AF_UNIX (J.1)

**Documented shortcuts:**
- Named sockets create a "regular file marker" in tmpfs rather than a real socket-type
  inode. The path-to-socket-handle map is the authoritative binding, not the filesystem
  node type. Programs that call `stat()` on a socket path and check `S_IFSOCK` will get
  incorrect results.

**Cross-references to later remediation:** None for the socket inode type issue.

**Red flags:** The socket inode type shortcut is a correctness gap for any program using
`stat()`/`fstat()` to identify socket files. J.1 end-to-end test left entirely unchecked.

---

## Phase 40 — Threading Primitives

**Declared status:** Complete

**Task checkboxes:** All checked across all tracks.

**Deferred items:** None explicitly in a deferred section.

**Documented shortcuts:** None.

**Cross-references to later remediation:** None.

**Red flags:** None. Threading (`clone(CLONE_THREAD)`, futex WAIT/WAKE, TLS via FS base)
all appear fully implemented per the task doc. This is a clean phase; later phases (SSH
server, async executor) that rely on threading build on this without revisiting it.

---

## Phase 41 — Expanded Coreutils

**Declared status:** Complete

**Task checkboxes:** All checked.

**Deferred items:**
- `bc` calculator (design doc notes it as explicitly out-of-scope; bc ships via the ports
  system in Phase 45).

**Documented shortcuts:** None.

**Cross-references to later remediation:** bc deferred to Phase 45 ports system.

**Red flags:** None. The bc exclusion is documented and intentional.

---

## Phase 42 — Cryptography Primitives

**Declared status:** Complete

**Task checkboxes:** All checked.

**Deferred items:** Hardening `getrandom()` to RDRAND/RDSEED.

**Documented shortcuts:**
- Design doc security note: "The crypto implementations in this phase are for learning.
  They have not been audited and should not be used to protect real secrets."
- CSPRNG note: "not cryptographically secure — hardening to RDRAND/RDSEED is deferred."

This is significant because Phase 43 (SSH server) depends on Phase 42's CSPRNG for key
material and session entropy.

**Cross-references to later remediation:** RDRAND/RDSEED hardening not cross-referenced to
any specific later phase.

**Red flags:** The SSH server (Phase 43) uses CSPRNG output that is explicitly documented
as not cryptographically secure. This is acknowledged but the gap has no remediation
cross-reference.

---

## Phase 42b — Async Executor

**Declared status:** Complete (task doc header)

**Template deviation:** NO design doc exists for Phase 42b. Only the task doc
(`docs/roadmap/tasks/42b-async-executor-tasks.md`) was found. This violates the
doc-template spec in `docs/appendix/doc-templates.md`, which requires a companion design
doc (`docs/roadmap/NN-slug.md`) for every phase. The task doc itself is the only
documentation for the entire async executor subsystem.

**Task checkboxes:** EVERY SINGLE TASK CHECKBOX IS UNCHECKED (`[ ]`) across all 7 tracks
(A through G). The task doc has no checked items despite its header declaring
`Status: Complete`.

**Deferred items:** Not formally listed (no design doc exists to carry a deferred section),
but the absence of any checked task implies the entire phase's work is either unrecorded
or not done.

**Documented shortcuts:** Cannot assess — no design doc.

**Cross-references to later remediation:** None.

**Red flags:**
- CRITICAL: Status declared `Complete` with zero checked tasks across all 7 tracks and
  all acceptance items. This is the most severe documentation integrity failure in the
  33–47 range.
- No design doc exists — this is a template violation. There is no phase-level description
  of why the async executor exists, what it builds on, or what it defers.
- Cannot determine whether the work was done and never checked off, or was never done.
  Either interpretation represents a documentation failure that makes the phase status
  untrustworthy.

---

## Phase 43 — SSH Server

**Declared status:** Complete

**Task checkboxes:** Several unchecked items:
- E.5: Window-change (`SIGWINCH`) notification (`[ ]`)
- G.1: End-to-end test — password auth (`[ ]`, labeled "Manual")
- G.2: End-to-end test — public key auth (`[ ]`, labeled "Manual")
- G.3: End-to-end test — traffic inspection with Wireshark (`[ ]`, labeled "Manual")

All three primary validation tests (G.1, G.2, G.3) are unchecked and labeled "Manual",
meaning they were not automated and are not verifiable from the task record alone.

**Deferred items:**
- Window-change (SIGWINCH) propagation to the PTY (E.5)

**Documented shortcuts:**
- Public key auth uses hex-encoded 32-byte Ed25519 public key (64 hex chars per line) not
  OpenSSH wire format or `authorized_keys` format. Documented as an explicit
  incompatibility with standard SSH clients.
- Design doc note on appendix cross-references: "The appendix SSH hang analyses are
  historical bring-up notes for a branch-local regression that was fixed during Phase 43
  review; they are not the current status of the shipped implementation."

**Cross-references to later remediation:** Design doc explicitly addresses `sshd-hang-analysis.md`
documents in the appendix, stating they document a fixed regression. No cross-reference to
`scheduler-fairness-regression.md` was found in this doc.

**Red flags:**
- The three end-to-end SSH tests (password auth, pubkey auth, traffic inspection) are all
  unchecked and labeled "Manual." A phase marked Complete where all primary validation
  tests are unautomated and unchecked provides no machine-verifiable record of correctness.
- Pubkey format incompatibility with OpenSSH is documented but has no remediation path.

---

## Phase 43a — Crash Diagnostics

**Declared status:** Complete

**Task checkboxes:** All checked across all tracks.

**Deferred items:** None.

**Documented shortcuts:** None.

**Cross-references:** No cross-references found to `scheduler-fairness-regression.md` or
`sshd-hang-analysis.md` in the appendix.

**Red flags:** None. This is a clean phase.

---

## Phase 43b — Kernel Trace Ring

**Declared status:** Complete (both design doc header and task doc header)

**Task checkboxes:** EVERY SINGLE TASK CHECKBOX IS UNCHECKED (`[ ]`) across all 9 tracks
(A through I). The task doc declares `Status: Complete` in its header while carrying zero
checked acceptance items.

**Deferred items:** Not formally listed in a separate section (the task doc carries no
formal deferred list since all items are unchecked).

**Documented shortcuts:** Cannot assess from task doc alone.

**Cross-references:** No cross-references found to `scheduler-fairness-regression.md` or
`sshd-hang-analysis.md` in either the design doc or task doc.

**Red flags:**
- CRITICAL: Same pattern as Phase 42b — status declared `Complete` with zero checked task
  checkboxes across all 9 tracks. The entire set of acceptance criteria for the per-core
  lockless `TraceRing<N>`, `TraceEvent` enum, `sys_ktrace` syscall, and userspace
  `ktrace` tool are unchecked.
- The design doc does declare a `Status: Complete` but the task doc's universal absence of
  checked items provides no evidence of completion.

---

## Phase 43c — Regression and Stress CI

**Declared status:** Complete (both design doc header and task doc header)

**Task checkboxes:** EVERY SINGLE TASK CHECKBOX IS UNCHECKED (`[ ]`) across all 11 tracks
(A through K). The task doc declares `Status: Complete` in its header while carrying zero
checked acceptance items.

**Deferred items:** None formally listed (all items unchecked).

**Documented shortcuts:** Cannot assess from task doc alone.

**Cross-references:** No cross-references found to `scheduler-fairness-regression.md` or
`sshd-hang-analysis.md` in either doc.

**Red flags:**
- CRITICAL: Same pattern as 42b and 43b. The entire regression harness (`cargo xtask
  regression`, `cargo xtask stress`), proptest property tests, loom concurrency tests, and
  CI integration tasks are all unchecked. This is the widest unchecked scope of the three
  (11 tracks vs. 7 and 9).
- The reliability infrastructure trilogy (43a / 43b / 43c) presents a striking asymmetry:
  43a (crash diagnostics) is the only one with all boxes checked. 43b and 43c — the
  dynamic observability and testing harness infrastructure — have universally unchecked
  tasks despite identical `Complete` status claims.

---

## Phase 44 — Rust Cross-Compilation Pipeline

**Declared status:** Complete

**Task checkboxes:** All checked across Tracks A–F.

**Deferred items:** None explicitly in a deferred section.

**Documented shortcuts:**
- Documentation Notes acknowledge that the `x86_64-m3os.json` custom target "is a naming
  exercise more than a functional change — it produces the same code as
  `x86_64-unknown-none` but with `os = 'm3os'`."
- musl Rust crates are not workspace members to avoid conflicts with the workspace-wide
  `x86_64-unknown-none` default target.

**Cross-references to later remediation:** Design doc references `docs/appendix/rust-std-userspace.md`
for a gap analysis of the musl-std path.

**Red flags:** None. All tasks checked. The shortcuts documented (naming-only custom target,
non-workspace musl crates) are acknowledged design choices, not gaps.

---

## Phase 45 — Ports and Package System

**Declared status:** Complete

**Task checkboxes:** All checked across Tracks A–G.

**Deferred items** (verbatim from design doc "Deferred Until Later"):
- "Binary package format (precompiled packages)"
- "Package signing and verification"
- "Version conflict resolution"
- "Automatic updates"
- "Mirror/repository support"
- "Network fetching of source tarballs"
- "Cross-compilation of ports on the host"
- "`/var/run → /run` compatibility symlink — add when the first port hardcodes `/var/run`
  for its PID file. Our own userspace (init, service, crontab) uses `/run` directly;
  `/var/run` is a Linux backwards-compatibility shim. At revisit time, add the symlink in
  the ext2 disk builder (or a runtime bootstrap) and verify kernel symlink resolution
  crosses the ext2 → tmpfs boundary cleanly. (Routed from `docs/debug/54-followups.md`
  item 3.)"

**Documented shortcuts:**
- All port sources bundled in the disk image; network fetching deferred.
- `port` command is a shell script rather than a compiled binary.
- Package tracking uses flat-file database (not binary/indexed format).

**Cross-references to later remediation:** `/var/run → /run` symlink routed from
`docs/debug/54-followups.md` item 3.

**Red flags:** None. The deferred items are appropriate scoping decisions for a toy ports
system, and all task items are checked.

---

## Phase 46 — System Services

**Declared status:** Complete

**Task checkboxes:** EVERY SINGLE TASK CHECKBOX IS UNCHECKED (`[ ]`) across all 8 tracks
(A through H) and all 22 tasks. This is the same pattern as Phases 42b, 43b, and 43c.

Specifically unchecked:
- All Track A items (service definition format, service files, init parser, dependency graph)
- All Track B items (ServiceStatus state machine, PID table, SIGCHLD handler, restart cap, shutdown)
- All Track C items (service command: list, status, start/stop/restart)
- All Track D items (syslogd daemon, log formatting, kern.log, logger command)
- All Track E items (crond daemon, crontab parser, next-run-time, job execution, crontab command)
- All Track F items (sys_reboot syscall, kernel shutdown sequence, syscall-lib wrapper)
- All Track G items (shutdown, reboot, hostname, who/w, last commands)
- All Track H items (integration tests, regression tests, documentation)

**Deferred items** (verbatim from design doc "Deferred Until Later"):
- "Socket activation (systemd-style, as in rustysd)"
- "sd_notify readiness protocol (rustysd supports this; we use timeout-based heuristics)"
- "Service sandboxing (cgroups, namespaces)"
- "Journal (structured logging)"
- "Log rotation"
- "Remote syslog"
- "NTP time synchronization"
- "systemd-compatible unit files"
- "Runlevels / targets"

**Documented shortcuts:**
- `service` command communicates with init via `/var/run/init.cmd` polling or direct
  signal — exact IPC mechanism deferred to implementation ("The exact IPC mechanism should
  be decided during implementation").

**Cross-references to later remediation:** None.

**Red flags:**
- CRITICAL: Phase 46 is the fourth phase in this range (alongside 42b, 43b, 43c) with
  `Status: Complete` in the header and zero checked task checkboxes. This phase covers
  the entire system services stack: service manager, syslogd, crond, sys_reboot,
  shutdown/reboot commands, who/w/last utilities. None of the acceptance criteria are
  checked.
- The design doc for Phase 46 is well-formed with all required template fields, making
  this a task-doc-only integrity failure, but the scope is substantial — 22 unchecked
  tasks across 8 tracks represent the largest unchecked surface in the 33–47 range.
- The `AGENTS.md` overview mentions syslogd, crond, and the service manager as shipped
  features; the task doc provides no record of completion for any of them.

---

## Phase 47 — DOOM

**Declared status:** Complete

**Task checkboxes:** EVERY SINGLE TASK CHECKBOX IS UNCHECKED (`[ ]`) across all 6 tracks
(A through F). This continues the same pattern as Phases 42b, 43b, 43c, and 46.

Specifically unchecked:
- All Track A items (`sys_framebuffer_info`, `sys_framebuffer_mmap`, syscall-lib constants)
- All Track B items (`sys_read_scancode`, syscall-lib constant)
- All Track C items (console yield/restore — `yield_console`, `restore_console`)
- All Track D items (entire doomgeneric platform layer: `DG_Init`, `DG_DrawFrame`,
  `DG_SleepMs`, `DG_GetTicksMs`, `DG_GetKey`, palette LUT)
- All Track E items (xtask `build_doom()`, initrd embedding, `doom1.wad` on ext2)
- All Track F items (manual smoke test, smoke-test step, doc update)

**Deferred items** (verbatim from design doc "Deferred Until Later"):
- "Multi-application composition and windowing"
- "Pointer-driven GUI policy"
- "Audio output for the graphical session"
- "Hardware-accelerated rendering"

**Documented shortcuts:**
- Design doc is deliberately abstract — specifies framebuffer and input requirements
  without dictating precise implementation. The task doc fills in the concrete syscall
  numbers (0x1002–0x1004) and symbol names.
- doomgeneric build follows `build_pdpmake` pattern (clone + musl-gcc), which already has
  a precedent of graceful empty-placeholder fallback when musl-gcc is unavailable.

**Cross-references to later remediation:** Design doc explicitly states DOOM is "a graphics
proof point, not a complete GUI architecture" and documents which later phases (display
server, local session) will supersede it.

**Red flags:**
- CRITICAL: Same pattern as 42b, 43b, 43c, 46 — `Status: Complete` with zero checked task
  checkboxes across all 6 tracks. The three new kernel syscalls (0x1002, 0x1003, 0x1004),
  the full doomgeneric platform layer, the xtask build integration, and the WAD disk
  placement are all unchecked.
- DOOM was cited in `AGENTS.md` as the graphics proof point that Phase 56 (display server)
  builds on. If Phase 47 tasks are genuinely incomplete, the display server's claimed
  framebuffer baseline is unvalidated.

---

## Summary of Critical Findings

### Pattern 1 — Universally unchecked task docs despite "Complete" header (5 phases)

Phases **42b**, **43b**, **43c**, **46**, and **47** all declare `Status: Complete` in
their task doc headers while having zero checked task checkboxes. This is the most
systemic finding in the 33–47 range.

| Phase | Tracks | Tasks unchecked | Subject |
|---|---|---|---|
| 42b | 7 | All | Async executor |
| 43b | 9 | All | Kernel trace ring |
| 43c | 11 | All | Regression/stress CI |
| 46 | 8 (22 tasks) | All | System services (syslogd, crond, service, shutdown) |
| 47 | 6 | All | DOOM (framebuffer syscalls, doomgeneric port) |

Possible explanations: (a) work was done but task docs were never updated (documentation
failure), or (b) phases were declared complete prematurely (work not done). Either
interpretation means the task docs cannot be used as evidence of completion for these
phases.

### Pattern 2 — Specific critical acceptance items unchecked in otherwise-checked phases

- **Phase 33 C.4**: Slab migration — the headline deliverable — never done. Design doc
  explicitly admits this. Symbols affected: slab cache in `kernel/src/mm/slab.rs`; all
  hot kernel object families remain on the linked-list heap.
- **Phase 35 E.2**: `maybe_load_balance()` hook commented out. SMP load balancing —
  the main thing Phase 35 adds over Phase 25 — is inoperative. Symbol:
  `maybe_load_balance` in scheduler dispatch loop.
- **Phase 39 J.1**: All three acceptance items of the AF_UNIX integration test unchecked.
- **Phase 43 G.1/G.2/G.3**: All three SSH end-to-end validation tests unchecked/manual.

### Pattern 3 — Template deviations

- **Phase 35** design doc is missing required `Status:` and `Source Ref:` header fields.
- **Phase 42b** has no design doc at all — only a task list exists for the async executor.

### Pattern 4 — Documented correctness gaps in shipped features

- **Phase 37**: I/O multiplexing timeouts have ~10ms granularity (no proper timer
  subsystem). Affects all programs using `poll`/`select`/`epoll` with timeouts.
- **Phase 39**: Named Unix domain sockets create regular file markers, not socket inodes.
  `stat()` on socket paths returns wrong `st_mode` type bits.
- **Phase 42**: CSPRNG is explicitly not cryptographically secure. SSH server (Phase 43)
  depends on it for key material.
- **Phase 43**: SSH pubkey format uses hex-encoded Ed25519, not OpenSSH wire format.
  Standard `ssh` clients cannot authenticate with standard `authorized_keys` files.
