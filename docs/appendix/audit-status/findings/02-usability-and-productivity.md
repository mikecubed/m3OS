# Findings: Phases 17–32 (Usability and Productivity)

Audit date: 2026-05-07. Sources: design docs (`docs/roadmap/NN-slug.md`), task
docs (`docs/roadmap/tasks/NN-slug-tasks.md`), and `docs/appendix/phase-21-handoff.md`.

---

## Phase 17 — Memory Reclamation

**Declared status:** Complete (design doc). Task doc: "Complete (stress test deferred to Phase 18+)".

**Task checkboxes:** Table-format task doc (no checkbox column). All tracks listed as Done in the track-layout table. No per-item checkbox granularity.

**Deferred items:**
- Stress test deferred to Phase 18+
- Buddy allocator (design doc "Deferred Until Later")
- Slab/SLUB allocator
- Swap
- Huge pages
- NUMA awareness
- `mmap(MAP_PRIVATE)` copy-on-write
- Kernel stack guard pages
- OOM killer

**Documented shortcuts:** Stress test explicitly removed from phase scope and pushed to a future phase. No automated verification of reclamation correctness under load.

**Acceptance gaps:** Without a stress test, the acceptance criteria "memory can be reclaimed and reused without fragmentation over time" is unverified. Table format means no visual record of which individual items were actually validated.

**Cross-references to later remediation:** "Phase 18+" for stress test (unspecified target).

**Red flags:** None severe. The phase's declared scope is narrow (basic reclamation plumbing, not advanced allocators), and the deferrals are all clearly scoped future work. The only concern is the missing stress test — "works under load" cannot be claimed.

---

## Phase 18 — Directory VFS

**Declared status:** Complete (both docs).

**Task checkboxes:** Table-format task doc. All tracks listed as Done. No checkbox items; no deferred entries outside the "Deferred Until Later" section.

**Deferred items (from "Deferred Until Later" section only):** Inode-based hard links; `readdir` with `getdents64` streaming; symlink resolution beyond the first hop; VFS mount namespace; `tmpfs` writeback; `proc` filesystem.

**Documented shortcuts:** None noted in task body.

**Acceptance gaps:** None identified. The scope is well-bounded and the deferred items are all explicitly out-of-scope features, not acceptance criteria.

**Cross-references to later remediation:** Phase 28 (ext2) for richer filesystem support.

**Red flags:** None.

---

## Phase 19 — Signal Handlers

**Declared status:** Design doc says **Complete**. Task doc says **Not started**.

**Task checkboxes:** All six tracks explicitly marked "Not started" in the track-layout table:
- Track A (Signal mask + rt_sigprocmask): Not started
- Track B (sigframe layout + handler dispatch): Not started
- Track C (sigreturn syscall): Not started
- Track D (sigaltstack): Not started
- Track E (rt_sigaction upgrade + sa_restorer): Not started
- Track F (Validation and tests): Not started

**Deferred items (from task doc "Deferred Until Later"):** Full siginfo_t; SA_SIGINFO three-argument form; queued real-time signals (sigqueue); signal coalescing suppression for RT signals.

**Documented shortcuts:** None (the task doc records zero progress).

**Acceptance gaps:** Every acceptance criterion is unverified according to the task doc. The design doc's acceptance criteria (signal delivery, handler invocation, sigreturn, signal masking) all have corresponding tasks that are listed as "Not started".

**Cross-references to later remediation:** None documented. The feature is referenced as a dependency by Phase 21 (ion shell) and Phase 29 (PTY) — both of which are marked Complete.

**Red flags:** CRITICAL STATUS MISMATCH. The design doc declares the phase Complete while the task doc declares all tracks Not started. One of the two documents is wrong. Either:
1. The task doc was created after the fact and reflects intended (not actual) work, or
2. The design doc was prematurely marked Complete before the task doc was reconciled.
This is the most severe audit finding in phases 17–32. Any downstream phase claiming signal handling as a satisfied dependency is built on an unverified assumption.

---

## Phase 20 — Userspace Init and Shell

**Declared status:** Complete (both docs).

**Task checkboxes:** Table-format task doc. All tracks listed as Done. No deferred entries in task body rows.

**Deferred items (from "Deferred Until Later" section):** Job control (`SIGTSTP`, `SIGCONT`, `bg`, `fg`); pipeline stderr redirection; `&&`/`||` conditional chaining; command history; tab completion; `alias`/`function` builtins; environment variable export persistence across exec; `/etc/profile` sourcing.

**Documented shortcuts:** sh0 is a minimal shell; the deferrals are all enrichment features, not core acceptance criteria.

**Acceptance gaps:** None identified — the defined scope was met.

**Cross-references to later remediation:** Phase 21 for ion shell (richer interactive experience).

**Red flags:** None.

---

## Phase 21 — Ion Shell Integration

**Declared status:** Complete (both docs). However, the `docs/appendix/phase-21-handoff.md` document records significant incomplete work passed forward.

**Task checkboxes:** Task doc status Complete. The per-task acceptance boxes in the ion task doc show specific items deferred. The handoff doc lists tasks P21-T028, T030–T034, T038–T044 as explicitly deferred to Phase 22.

**Deferred items (verbatim from handoff doc and task doc):**
- `ion -c 'cmd'` script mode exits with code 1 (ENOTTY) — deferred to Phase 22
- Ion interactive mode (all interactive acceptance tests T030–T034, T038–T044)
- History persistence (`~/.local/share/ion/history`) — requires writable home dir
- Tab completion with reedline-style highlighting — depends on ion's reedline integration
- Vendoring of ion source deferred — build caches `ion.elf` between builds

**Documented shortcuts:**
- Init was changed to use sh0 as the primary interactive shell, with ion as a fallback (design doc intended the reverse — ion as primary, sh0 as fallback)
- Ion interactive mode disabled at boot; users get sh0, not ion, as their default shell
- The handoff doc notes multiple kernel bugs were discovered and fixed during bring-up (futex CR3 corruption, fork child register corruption, demand paging for stack region, FS.base TLS save/restore) — these fixes are real progress but they were not planned Phase 21 work

**Acceptance gaps:**
- "Ion can execute a script via `ion -c 'echo hello'`" — unverified (exits 1 with ENOTTY)
- "Ion interactive mode works" — unverified (all interactive acceptance tasks deferred)
- The design doc's milestone goal ("ion is the primary interactive shell") was not achieved; sh0 remained primary

**Cross-references to later remediation:** Phase 22 (TTY refinement) is the explicit target for all deferred ion interactive items.

**Red flags:** Phase marked Complete despite the primary milestone goal not being achieved. The handoff doc is honest about the deferrals, but the design doc status does not reflect them. The init boot sequence inversion (sh0 as primary instead of ion) represents a functional regression relative to the stated goal.

---

## Phase 22 — TTY Refinement / PTY

**Declared status:** Complete (design doc). Task doc uses table format without an explicit status header line, but all track rows show "Done" or "Done + deferred items resolved".

**Task checkboxes:** Table-format task doc. Track rows all marked Done. A "Phase 20/21 Deferred Items Resolved by This Phase" section confirms resolution of ion `-c` ENOTTY, ion interactive mode, and pgrp/session syscalls.

**Deferred items resolved by this phase:** ion `-c` script mode (now exits 0); ion interactive mode; TIOCSPGRP/TIOCGPGRP; tcsetpgrp; setsid skeleton.

**Items deferred again:**
- History persistence (`~/.local/share/ion/history`) — deferred again, requires writable home dir
- Tab completion with reedline-style highlighting — deferred again

**Documented shortcuts:** None beyond the carry-forward from Phase 21.

**Acceptance gaps:** The two re-deferred items (history persistence and tab completion) are enrichment features, not acceptance criteria for Phase 22. No gaps in the defined Phase 22 scope.

**Cross-references to later remediation:** History persistence and tab completion remain unassigned to a specific future phase.

**Red flags:** Minor. The double-deferral of history/tab-completion without assignment to a concrete future phase could make them permanent deferrals.

---

## Phase 22b — ANSI Escape Sequences

**Declared status:** No design doc exists (doc-template deviation — only a task doc file). The task doc has no top-level Status field. The validation track (Track F) records all items as "Deferred (manual QEMU visual test)".

**Task checkboxes:** Mixed table/checklist hybrid. Tracks A–E are table-format (no checkboxes). Track F is a table where the Status column reads "Deferred (manual QEMU visual test)" for every row (P22b-T040 through P22b-T046).

**Deferred items (verbatim):**
- P22b-T040: "Ion prompt redraws in-place on each keystroke (no appending/garbling)" — Deferred (manual QEMU visual test)
- P22b-T041: "Ion prompt appears at correct position after running a command" — Deferred (manual QEMU visual test)
- P22b-T042: "`echo -e '\x1b[2J\x1b[H'` clears the screen" — Deferred (manual QEMU visual test)
- P22b-T043: "backspace in Ion erases the character and cursor moves back visually" — Deferred (manual QEMU visual test)
- P22b-T044: "long command lines wrap correctly and can be edited" — Deferred (manual QEMU visual test)
- P22b-T045: "external command output (`ls`, `cat`) displays correctly after Ion prompt" — Deferred (manual QEMU visual test)
- P22b-T046: "sh0 still works correctly (cooked mode echo + line discipline unaffected)" — Deferred (manual QEMU visual test)

**Documented shortcuts:** All 7 validation items (the entire Track F) are deferred. The implementation tracks (A–E) are marked Done without a corresponding verified acceptance baseline.

**Acceptance gaps:** Every acceptance criterion for this phase is listed as deferred. The phase has no automated verification path and no interactive verification was completed.

**Cross-references to later remediation:** None assigned.

**Red flags:**
1. Missing design doc — template violation.
2. Entire validation track deferred while implementation is considered complete.
3. The most critical item (sh0 still works correctly — P22b-T046) is deferred, meaning no regression check was performed after ANSI parser changes.

---

## Phase 23 — Socket API

**Declared status:** Complete (design doc). Task doc uses table format without a top-level Status line. All tracks show Done in the track layout.

**Task checkboxes:** Table-format. No deferred entries in task body rows; no unchecked checkbox items.

**Deferred items (from "Deferred Until Later" section):** `SO_REUSEADDR` / `SO_REUSEPORT`; `SO_LINGER`; `sendmsg`/`recvmsg`; `select`/`poll`/`epoll`; non-blocking sockets (`O_NONBLOCK` on AF_UNIX); Unix socket credential passing (`SCM_CREDENTIALS`); abstract namespace sockets; datagram sockets (`SOCK_DGRAM`).

**Documented shortcuts:** None identified in task body.

**Acceptance gaps:** None for defined scope. The deferred items are enrichment.

**Cross-references to later remediation:** Non-blocking I/O referenced as needed for Phase 30 (telnet server).

**Red flags:** None. This is a clean phase with well-scoped deferrals.

---

## Phase 24 — Persistent Storage

**Declared status:** Complete (design doc). Task doc uses table format with Status column.

**Task checkboxes:** Table-format. Several rows have "deferred" in the Status column:
- P24-T034 (rename syscall): "deferred"
- P24-T042 (shell mount builtin): "deferred"
- P24-T043 (end-to-end reboot persistence test): "deferred (requires interactive QEMU)"
- P24-T044 (host-side mount visibility): "deferred (requires interactive QEMU + host mount)"
- P24-T047 (QEMU integration test): "deferred (requires QEMU integration test harness extension)"
- P24-T048 (multi-cluster file test): "deferred (requires interactive QEMU)"

**Deferred items (verbatim):**
- rename(2) syscall — deferred entirely
- shell `mount` builtin — deferred entirely
- "A file written to /data/hello.txt inside the shell is visible on the next boot" — the design doc's primary acceptance criterion for persistence — maps to P24-T043 which is deferred (requires interactive QEMU)
- Host-side visibility of ext2 image via `losetup` — deferred
- Multi-cluster write/read test — deferred
- QEMU integration test — deferred

**Documented shortcuts:** The acceptance criterion "files survive reboot" is deferred to interactive QEMU. Persistence is the core promise of Phase 24; this deferral means the central claim is unverified.

**Acceptance gaps:**
- The design doc's milestone goal ("a file written inside the OS survives reboot and is readable on the next boot") is unverified because P24-T043 is explicitly deferred.
- `rename(2)` is absent — file renaming does not work.
- The shell `mount` builtin is absent — users cannot manually mount/unmount filesystems.

**Cross-references to later remediation:** None assigned for the deferred acceptance tests.

**Red flags:**
- The primary acceptance criterion (persistence across reboot) is deferred as requiring interactive QEMU, which means it was never validated even once.
- `rename` being missing means file-move operations return ENOSYS or similar, which breaks common user workflows.

---

## Phase 25 — SMP

**Declared status:** Complete (design doc). Task doc uses table format; all tracks show "Done" with parenthetical caveats.

**Task checkboxes:** Table-format with Status column. Track caveats:
- Track C: "Done (BSP-only dispatch; per-core syscall deferred)"
- Track E: "Done (handler+API; munmap hook deferred)"
- Track F: "Done (audit complete; BSP-only mitigation)"

**Deferred items (verbatim):**
- P25-T033 (hook tlb_shootdown into munmap): explicitly deferred — munmap does not trigger TLB shootdown
- Per-core syscall dispatch: deferred — all syscalls still dispatched through BSP
- "BSP-only mitigation" for spinlock audit: full per-core spinlock correctness not achieved, BSP-only fallback used

**Documented shortcuts:**
- Syscall dispatch routes through BSP only, not through the core that issued the syscall
- TLB shootdown is implemented but not wired into munmap (the primary consumer)
- "BSP-only mitigation" in the spinlock audit track — multi-core spinlock hazards are mitigated by not exercising them, not by fixing them

**Acceptance gaps:**
- Design doc acceptance criterion: "A TLB shootdown triggered by munmap in one process does not leave stale mappings on another core" — this is explicitly unverified because the munmap hook was deferred
- Design doc acceptance criterion: "system calls issued from any core are handled correctly" — unverified because per-core dispatch was deferred; all calls go through BSP

**Cross-references to later remediation:** Phase 35 ("true SMP") referenced in the design doc for per-core syscall dispatch and full spinlock audit.

**Red flags:**
- Two of the three concrete SMP acceptance criteria are unverified.
- The munmap TLB shootdown gap is a correctness hazard: a process unmapping a page on one core can leave stale TLB entries on other cores, causing silent memory corruption or security bugs.
- "BSP-only mitigation" essentially means the system is not yet running as a true SMP kernel for syscall paths.

---

## Phase 26 — Text Editor

**Declared status:** Complete (both docs).

**Task checkboxes:** Checklist format. All items across all tracks (A through G) are marked `[x]`. No unchecked items.

**Deferred items (from "Deferred Until Later" section):**
- Syntax highlighting (kibi supports it; deferred to Phase 26b)
- Multiple file editing (split views, tabs)
- Undo/redo beyond single-character level
- Copy/paste (requires clipboard abstraction)
- Mouse support
- ncurses/TUI library
- terminfo/termcap database
- Line wrapping mode
- Full Unicode / multi-byte character editing
- Plugin or macro system
- Configuration file support (`.kibirc`)
- Colorscheme / theme support

**Documented shortcuts:** Syntax highlighting explicitly deferred to Phase 26b. All acceptance items are checked, including interactive ones (G.1–G.15 all `[x]`).

**Acceptance gaps:** None for defined scope. All acceptance items are marked verified, including the runtime QEMU validation (G.14: "Editor launches, edits, saves, and quits without panics in both serial and framebuffer modes").

**Cross-references to later remediation:** Phase 26b for syntax highlighting.

**Red flags:** None. This is one of the cleanest phases in the audit range — scope is bounded, deferrals are genuine future work, and all acceptance items are checked.

---

## Phase 27 — User Accounts

**Declared status:** Complete (both docs).

**Task checkboxes:** Checklist format. All items marked `[x]` throughout. One item notes entropy deferral: "PRNG seeded from TSC (`rdtsc`) or equivalent (proper entropy deferred)".

**Deferred items (verbatim):**
- Proper entropy source (only TSC-seeded PRNG, not cryptographic)
- ext2 filesystem — deferred to Phase 28 (FAT32 used for password file storage)
- `sudo` — deferred (su is sufficient for Phase 27)
- PAM or pluggable authentication
- Password aging / expiry
- Group management beyond static `/etc/group`
- LDAP / NIS integration
- Full POSIX ACLs

**Documented shortcuts:**
- Passwords hashed with a TSC-seeded PRNG (not cryptographically secure random source)
- User data stored on FAT32 (not ext2); persistence depends on Phase 24's FAT32, which itself has deferred acceptance tests (see Phase 24 findings)

**Acceptance gaps:** The entropy quality caveat is documented in-line but not treated as a separate acceptance criterion. A TSC-seeded PRNG is deterministic and not suitable for password hashing in any realistic threat model; the caveat acknowledgment does not remediate the gap.

**Cross-references to later remediation:** Phase 28 for ext2 storage of user files.

**Red flags:** Minor. The entropy weakness is documented but not tracked for remediation. Downstream phases using the user account system (login, PAM-equivalent) inherit this weakness.

---

## Phase 28 — ext2 Filesystem

**Declared status:** Complete (both docs).

**Task checkboxes:** Table-format. All tracks marked Done in layout table. Track I has a hedged acceptance item: "`.m3os_permissions` handling removed from FAT32 (or kept as fallback for FAT32-only setups)".

**Deferred items:**
- Triple-indirect block addressing (noted in implementation notes as deferred; files limited to ~64 MB with 4K blocks)
- Full directory entry deletion (unlink) — verify from task body
- Symlink creation

**Documented shortcuts:**
- Track I acceptance: "`.m3os_permissions` handling removed from FAT32 (or kept as fallback for FAT32-only setups)" — the parenthetical hedge indicates the FAT32 overlay was not actually removed; it was kept as a fallback. The original goal was to remove the workaround once ext2 was available.

**Acceptance gaps:**
- The FAT32 `.m3os_permissions` workaround was retained rather than removed, meaning the codebase still carries an acknowledged technical debt item.
- Triple-indirect blocks cap files at ~64 MB — this is documented but unaddressed.

**Cross-references to later remediation:** None explicitly assigned for the `.m3os_permissions` cleanup.

**Red flags:** The "or kept as fallback" hedge in Track I's acceptance criterion is a typical audit signal that a cleanup task was accepted incomplete. The workaround removal was a stated goal and was not completed.

---

## Phase 29 — PTY Subsystem

**Declared status:** Complete (design doc). Task doc has no top-level Status field; track layout shows all tracks as Done.

**Task checkboxes:** Table-format task doc. All track rows show Done in the layout table. The track task tables (Tasks A–I) use row format without individual checkbox status. The validation track (Track I, tasks P29-T047 through P29-T059) tasks are listed without Status column values — they appear as plain task descriptions, not as verified/deferred.

**Deferred items (from "Deferred Until Later" section):**
- `/dev/tty` device node (controlling terminal access by path)
- Packet mode (`TIOCPKT`) for flow control
- Window size change propagation via SIGWINCH from kernel resize events (only manual `TIOCSWINSZ` implemented)
- Terminal multiplexer (screen/tmux-style)
- Dynamic PTY allocation beyond fixed 16-slot pool
- PTY ownership and permission enforcement (`grantpt()` is a no-op)
- Orphaned process group handling (full POSIX job control semantics)
- Background process group stop on terminal read (`SIGTTIN`/`SIGTTOU`)
- Multiple line disciplines (only N_TTY supported)
- PTY slave `/dev/pts` filesystem (slaves opened by path convention, not devpts)

**Documented shortcuts:**
- `grantpt()` is a no-op (no permission enforcement)
- Fixed 16-slot PTY pool (not dynamically sized)
- Full POSIX session semantics (orphaned process groups) deferred

**Acceptance gaps:** The Track I validation tasks (P29-T047 through P29-T059) have no Status column in the task doc and appear as plain descriptions without verification records. The task doc format for Phase 29 does not carry explicit Done/Deferred markers for individual acceptance items. Without per-item status, it is not possible to confirm from the doc which acceptance tests were actually run.

**Cross-references to later remediation:** Phase 30 (telnet server) depends on PTY. Phase 35 (true-SMP) may require revisiting per-PTY locking.

**Red flags:** The absence of per-item status in the validation track means the audit cannot confirm which acceptance tests were run. This is a documentation quality issue rather than a confirmed gap, but it warrants verification. The 22/Phase 29 PTY overlap: Phase 22 (TTY/PTY) included PTY skeleton stubs; Phase 29 delivered the full implementation. The Phase 22 stubs (`alloc_pty()`, `FdBackend::PtyMaster/PtySlave`) were replaced by Phase 29 — this is documented correctly.

---

## Phase 30 — Telnet Server

**Declared status:** Complete (task doc). Design doc status: Complete.

**Task checkboxes:** Mixed table/checklist format. Tracks A–E use table format (Done rows). Track F (validation) uses checklist format — ALL items are unchecked `[ ]` with "(manual QEMU test)" annotations. Track G.3 (roadmap README update) is also unchecked.

**Deferred items (verbatim — all Track F items):**
- `[ ]` `cargo xtask run` boots with telnetd running _(manual QEMU test)_
- `[ ]` Serial output shows telnetd startup message _(manual QEMU test)_
- `[ ]` No panics or regressions in existing functionality _(manual QEMU test)_
- `[ ]` `telnet localhost 2323` shows `login:` prompt _(manual QEMU test)_
- `[ ]` Valid credentials authenticate and drop into a shell _(manual QEMU test)_
- `[ ]` `echo hello`, `ls /`, `cat /etc/passwd`, `pwd`, `mkdir`/`rmdir` all produce correct output _(manual QEMU test)_
- `[ ]` `echo hello | cat` and `ls / | grep bin` produce correct output _(manual QEMU test)_
- `[ ]` `edit` opens, accepts edits, saves, and exits over the telnet connection _(manual QEMU test)_
- `[ ]` ≥ 4 concurrent `telnet localhost 2323` sessions, each independent _(manual QEMU test)_
- `[ ]` Closing one session does not affect the others _(manual QEMU test)_
- `[ ]` Disconnecting the telnet client terminates the remote login/shell via SIGHUP _(manual QEMU test)_
- `[ ]` PTY pair is freed; reconnecting works _(manual QEMU test)_
- `[ ]` NAWS from host client updates PTY window size _(manual QEMU test)_
- `[ ]` Text editor layout reflects the updated dimensions _(manual QEMU test)_
- `[ ]` `cargo xtask run` and `cargo xtask run-gui` both boot with telnetd running _(manual QEMU test)_
- `[ ]` Phase 30 marked complete in `docs/roadmap/README.md` _(deferred until manual tests pass)_

**Documented shortcuts:** The entire validation track was left unchecked. The implementation tracks (A–E) were marked Done without any interactive verification.

**Acceptance gaps:** Every runtime acceptance criterion for Phase 30 is unverified. The telnet server may be built and linked, but there is no recorded evidence that:
- The service starts successfully
- A client can connect
- Authentication works
- A shell session runs over the PTY
- Concurrent sessions work
- Disconnect/SIGHUP works

**Cross-references to later remediation:** G.3 says "deferred until manual tests pass" — implying a future verification pass was intended but not scheduled.

**Red flags:** Severe. Phase is marked Complete with the entire validation track consisting of unchecked checkboxes. The roadmap README update (G.3) is itself deferred, which is unusual (normally the README is updated when a phase completes). The telnet server is the primary milestone product of Phase 30 and none of its runtime behavior has been verified.

---

## Phase 31 — Compiler Bootstrap

**Declared status:** Task doc: "Complete (automated smoke test; runtime validation deferred)". Design doc: Complete.

**Task checkboxes:** Mixed format. Implementation tracks use table format (Done rows). Validation tracks D, E, F use checklist format — the majority of items are unchecked and explicitly deferred.

**Deferred items (verbatim — Track D through F):**

Track D (Runtime validation — TCC inside OS):
- D.1: `tcc --version` produces a version line — deferred
- D.2: Compile `hello.c` with `tcc -run hello.c` — deferred
- D.3: Compiled binary runs and prints "Hello, World!" — deferred
- D.5: Fibonacci program compiles and runs — deferred
- D.6: Multi-file compile test — deferred

Track E (Self-hosting milestone):
- E.1: TCC compiles itself inside the OS — deferred
- E.2: TCC compiled by TCC produces a working binary — deferred
- E.3: Self-hosted TCC binary passes basic tests — deferred
- E.4: Self-hosting documented in phase doc — deferred

Track F (Integration and documentation):
- F.1: `cargo xtask check` passes with all new code — deferred
- F.2 through F.5: QEMU boot, regression tests — deferred
- F.9: Phase marked complete in roadmap README — deferred

**Documented shortcuts:** An automated smoke test (build pipeline integration) passed, but all interactive runtime tests are deferred. The "automated smoke test" verifies the build produces an artifact, not that the artifact functions inside the OS.

**Acceptance gaps:**
- The design doc's milestone goal ("TCC compiles a C program inside the OS") is unverified (D.2, D.3 deferred)
- The design doc's secondary milestone ("TCC compiles itself") is unverified (entire Track E deferred)
- `cargo xtask check` passage (F.1) is deferred — the codebase may not even pass lint

**Cross-references to later remediation:** None assigned. The task doc status note "runtime validation deferred" does not name a target phase.

**Red flags:** Severe. The central claims of Phase 31 ("a C compiler runs inside the OS" and "TCC compiles itself inside the OS") are entirely unverified. The phase is marked Complete based solely on an automated smoke test that validates the build pipeline, not the runtime behavior. Track E (self-hosting) is 100% deferred.

---

## Phase 32 — Build Tools

**Declared status:** Task doc: "Complete (interactive QEMU validation deferred)". Design doc: Complete.

**Task checkboxes:** Mixed format. Implementation tracks use table format. Validation tracks D, E, F use checklist format — almost entirely unchecked and deferred.

**Deferred items (verbatim):**

Track B (make):
- B.5: `make --version` works — deferred
- B.6: Simple Makefile parses and executes — deferred

Track C (shell utilities):
- C.6: `time` utility works — deferred

Track D (POSIX shell features):
- D.1: for loop in sh0 — deferred
- D.2: conditional (`if`/`else`) in sh0 — deferred
- D.3: command substitution (`$(...)`) in sh0 — deferred
- D.4: exit status (`$?`) in sh0 — deferred

Track E (integration tests):
- E.2: Full build completes — deferred
- E.3: Incremental rebuild (touch one file, re-make) — deferred
- E.4: `make clean` — deferred
- E.5: `ar` archive creation — deferred
- E.6: `build.sh` script — deferred

Track F (validation — all items):
- F.1 through F.9: ALL deferred

**Documented shortcuts:** Same pattern as Phase 31: implementation tracks marked Done, all validation deferred. Track D (POSIX shell features) represents new kernel/shell functionality (for loops, conditionals, command substitution) that was not validated.

**Acceptance gaps:**
- `make --version` not verified — make may not run
- Simple Makefile execution not verified — make may not parse correctly
- POSIX shell features (for, if, command substitution, exit status) not verified — these are substantial feature additions to sh0 that have zero runtime verification
- The design doc's milestone goal ("`make` builds a C project from source inside the OS") maps entirely to Track E which is 100% deferred

**Cross-references to later remediation:** None assigned. Status note "interactive QEMU validation deferred" does not name a target phase.

**Red flags:** Severe. Same pattern as Phase 31 but broader scope. sh0 received POSIX shell feature additions (for loops, conditionals, command substitution) that were never tested. `make` was added but never verified to parse a Makefile. The entire Track F validation (F.1–F.9) is deferred. The design doc's central milestone is unverified.

---

## Summary Table

| Phase | Design Doc Status | Task Doc Status | Unchecked `[ ]` items | Severity |
|---|---|---|---|---|
| 17 | Complete | Complete (stress test deferred) | 0 | Low |
| 18 | Complete | Complete | 0 | None |
| 19 | **Complete** | **Not started** | All tracks Not started | **Critical** |
| 20 | Complete | Complete | 0 | None |
| 21 | Complete | Complete | 7 deferred tasks (handoff doc) | Medium |
| 22 | Complete | (table, all Done) | 0 | Low |
| 22b | Complete (no design doc) | (no status header) | 7 (entire Track F) | High |
| 23 | Complete | (table, all Done) | 0 | None |
| 24 | Complete | (table, 6 deferred rows) | 6 deferred tasks | Medium |
| 25 | Complete | (table, 3 hedged Done rows) | 2 acceptance criteria unverified | High |
| 26 | Complete | Complete | 0 | None |
| 27 | Complete | Complete | 0 (entropy caveat noted) | Low |
| 28 | Complete | Complete | 0 (FAT32 overlay not removed) | Low |
| 29 | Complete | (table, no per-item status) | Unknown (no status column) | Medium |
| 30 | Complete | Complete | 16 (entire Track F) | **Severe** |
| 31 | Complete | Complete (runtime deferred) | ~15 across D/E/F | **Severe** |
| 32 | Complete | Complete (validation deferred) | ~20 across B/C/D/E/F | **Severe** |

### Critical Findings

1. **Phase 19 status mismatch**: Design doc says Complete; task doc says Not started across all 6 tracks. Any downstream phase depending on signal handling (Phases 21, 29, 30) may be built on an unverified foundation.

2. **Phases 30, 31, 32 — systemic deferred validation**: Entire validation tracks (all `[ ]` items) left unchecked while phases are declared Complete. The runtime behavior of telnetd, TCC, and make has never been verified inside the OS.

3. **Phase 22b — missing design doc**: Template deviation. No design doc exists for Phase 22b; only a task doc. The phase has no Status field at the doc level, and its entire acceptance track is deferred.

4. **Phase 25 SMP — partial implementation declared done**: The TLB shootdown munmap hook (the mechanism that makes SMP memory management safe) was deferred. The design doc's acceptance criterion for TLB coherence is unverified. Per-core syscall dispatch is also deferred.

5. **Phase 21 milestone inversion**: The declared goal was ion as primary interactive shell, sh0 as fallback. The implemented outcome is sh0 as primary, ion as fallback. The phase is marked Complete despite the milestone goal not being achieved.
