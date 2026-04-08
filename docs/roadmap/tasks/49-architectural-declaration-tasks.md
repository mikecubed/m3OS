# Phase 49 — Architectural Declaration: Task List

**Status:** Complete
**Source Ref:** phase-49
**Depends on:** Phase 48 (Security Foundation) ✅
**Goal:** Make the kernel/userspace boundary explicit and enforceable by decomposing the syscall surface, classifying subsystem ownership, and adopting a userspace-first rule for new policy.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Evaluation gate and subsystem inventory | None | ✅ Done |
| B | Syscall surface decomposition | A | ✅ Done |
| C | Keep/move/transition matrix and ownership tagging | A | ✅ Done |
| D | Userspace-first rule | A | ✅ Done |
| E | Tests and validation | B | ✅ Done |
| F | Documentation, versioning, roadmap integration | B, C, D, E | ✅ Done |

---

## Track A — Evaluation Gate and Subsystem Inventory

### A.1 — Audit current kernel subsystem boundaries

**File:** `docs/appendix/architecture-and-syscalls.md`
**Symbol:** `Current Architecture` section
**Why it matters:** Without knowing the real state of the kernel, any architectural declaration is aspirational fiction.

**Acceptance:**
- [x] Current Architecture section exists with accurate subsystem inventory
- [x] Gap between documented ideal and shipped implementation is explicitly described

---

## Track B — Syscall Surface Decomposition

### B.1 — Create syscall module directory structure

**File:** `kernel/src/arch/x86_64/syscall/mod.rs`
**Symbol:** `syscall_handler`
**Why it matters:** The monolithic syscall handler mixed all subsystem concerns in one file, making ownership boundaries invisible.

**Acceptance:**
- [x] `kernel/src/arch/x86_64/syscall/` directory exists with `mod.rs` and 8 subsystem modules
- [x] `mod.rs` contains only the dispatcher; subsystem logic lives in dedicated modules

### B.2 — Filesystem syscall module

**File:** `kernel/src/arch/x86_64/syscall/fs.rs`
**Symbol:** `sys_open`, `sys_read`, `sys_write`, `sys_close`, `sys_stat`, etc.
**Why it matters:** Filesystem operations are the largest group of syscalls and the primary extraction target for later phases.

**Acceptance:**
- [x] All filesystem-related syscall implementations moved to `fs.rs`
- [x] Module carries ownership header comment

### B.3 — Memory management syscall module

**File:** `kernel/src/arch/x86_64/syscall/mm.rs`
**Symbol:** `sys_mmap`, `sys_munmap`, `sys_brk`, `sys_mprotect`
**Why it matters:** Memory syscalls are kernel mechanisms that stay permanently in ring 0.

**Acceptance:**
- [x] All memory-related syscall implementations moved to `mm.rs`
- [x] Module carries ownership header comment

### B.4 — Process lifecycle syscall module

**File:** `kernel/src/arch/x86_64/syscall/process.rs`
**Symbol:** `sys_fork`, `sys_exec`, `sys_exit`, `sys_wait`
**Why it matters:** Process lifecycle is split between kernel mechanisms (scheduling) and policy (session management).

**Acceptance:**
- [x] All process-related syscall implementations moved to `process.rs`
- [x] Module carries ownership header comment

### B.5 — Network syscall module

**File:** `kernel/src/arch/x86_64/syscall/net.rs`
**Symbol:** `sys_socket`, `sys_bind`, `sys_connect`, `sys_sendto`, `sys_recvfrom`
**Why it matters:** Networking is a primary extraction target for future serverization.

**Acceptance:**
- [x] All network-related syscall implementations moved to `net.rs`
- [x] Module carries ownership header comment

### B.6 — Signal syscall module

**File:** `kernel/src/arch/x86_64/syscall/signal.rs`
**Symbol:** `sys_kill`, `sys_sigaction`, `sys_sigreturn`
**Why it matters:** Signal handling is transitional — delivery mechanism stays in kernel, policy may move.

**Acceptance:**
- [x] All signal-related syscall implementations moved to `signal.rs`
- [x] Module carries ownership header comment

### B.7 — I/O multiplexing syscall module

**File:** `kernel/src/arch/x86_64/syscall/io.rs`
**Symbol:** `sys_poll`, `sys_select`, `sys_epoll_create`
**Why it matters:** I/O multiplexing is a kernel mechanism that stays in ring 0.

**Acceptance:**
- [x] All I/O multiplexing syscall implementations moved to `io.rs`
- [x] Module carries ownership header comment

### B.8 — Time syscall module

**File:** `kernel/src/arch/x86_64/syscall/time.rs`
**Symbol:** `sys_clock_gettime`, `sys_gettimeofday`, `sys_nanosleep`
**Why it matters:** Time syscalls are kernel mechanisms.

**Acceptance:**
- [x] All time-related syscall implementations moved to `time.rs`
- [x] Module carries ownership header comment

### B.9 — Miscellaneous syscall module

**File:** `kernel/src/arch/x86_64/syscall/misc.rs`
**Symbol:** `sys_ioctl`, `sys_uname`, `sys_cap_grant`
**Why it matters:** Catches remaining syscalls that do not fit a single subsystem.

**Acceptance:**
- [x] All remaining syscall implementations moved to `misc.rs`
- [x] Module carries ownership header comment

---

## Track C — Keep/Move/Transition Matrix

### C.1 — Create ownership classification in architecture docs

**File:** `docs/appendix/architecture-and-syscalls.md`
**Symbol:** `Keep/Move/Transition Matrix` section
**Why it matters:** Without a formal classification, every future phase must re-derive what belongs in ring 0.

**Acceptance:**
- [x] Matrix table exists classifying every major kernel subsystem
- [x] Categories are: Keep (kernel mechanism), Move (future userspace), Transition (evaluate)

### C.2 — Add ownership headers to kernel source modules

**Files:**
- `kernel/src/arch/x86_64/syscall/fs.rs`
- `kernel/src/arch/x86_64/syscall/mm.rs`
- `kernel/src/arch/x86_64/syscall/process.rs`
- `kernel/src/arch/x86_64/syscall/net.rs`
- `kernel/src/arch/x86_64/syscall/signal.rs`
- `kernel/src/arch/x86_64/syscall/io.rs`
- `kernel/src/arch/x86_64/syscall/time.rs`
- `kernel/src/arch/x86_64/syscall/misc.rs`

**Symbol:** ownership header comments
**Why it matters:** Code-level ownership markers make the architectural contract visible where developers actually work.

**Acceptance:**
- [x] Each syscall module has a comment header indicating its ownership classification
- [x] Headers reference the architecture-and-syscalls.md matrix

---

## Track D — Userspace-First Rule

### D.1 — Document the userspace-first rule

**File:** `docs/appendix/architecture-and-syscalls.md`
**Symbol:** `Architecture Review Checklist` section
**Why it matters:** Without an explicit rule, new policy-heavy code continues to land in ring 0 by default.

**Acceptance:**
- [x] Userspace-first rule is documented with concrete evaluation questions
- [x] Architecture review checklist exists for evaluating new kernel additions

### D.2 — Update CLAUDE.md/AGENTS.md with userspace-first rule

**File:** `AGENTS.md`
**Symbol:** `Userspace-first rule` section under Critical Conventions
**Why it matters:** The rule must be visible to all contributors, including AI agents.

**Acceptance:**
- [x] AGENTS.md contains userspace-first rule in Critical Conventions
- [x] Rule references the architecture review checklist

---

## Track E — Tests and Validation

### E.1 — Verify build passes after syscall decomposition

**File:** `xtask/src/main.rs`
**Symbol:** `cargo xtask check`
**Why it matters:** The syscall decomposition must not break the build.

**Acceptance:**
- [x] `cargo xtask check` passes (clippy, rustfmt, kernel-core host tests)

---

## Track F — Documentation, Versioning, and Roadmap Integration

### F.1 — Create Phase 49 learning doc

**File:** `docs/49-architectural-declaration.md`
**Symbol:** aligned learning doc
**Why it matters:** Every phase must ship with a learning document explaining what was done and why.

**Acceptance:**
- [x] Learning doc follows the aligned legacy learning doc template
- [x] Covers current-vs-target, mechanism-vs-policy, syscall decomposition, matrix summary, userspace-first rule

### F.2 — Update docs/README.md

**File:** `docs/README.md`
**Symbol:** Phase-Aligned Learning Docs table
**Why it matters:** The learning doc must be discoverable from the documentation index.

**Acceptance:**
- [x] Phase 49 row added to the learning docs table

### F.3 — Update docs/roadmap/README.md

**File:** `docs/roadmap/README.md`
**Symbol:** Phase 49 row in Convergence and Release-Critical Phases table
**Why it matters:** The roadmap must reflect phase completion status.

**Acceptance:**
- [x] Phase 49 status changed to Complete
- [x] Tasks column links to the task doc

### F.4 — Update docs/roadmap/tasks/README.md

**File:** `docs/roadmap/tasks/README.md`
**Symbol:** Convergence and Release-Critical Phases table
**Why it matters:** The task index must include the new task doc.

**Acceptance:**
- [x] Phase 49 row added to the task documents table
- [x] Phase 49 removed from "Future Task Docs" deferred list

### F.5 — Link task doc from phase design doc

**File:** `docs/roadmap/49-architectural-declaration.md`
**Symbol:** Companion Task List section
**Why it matters:** The design doc must link to the actual task doc.

**Acceptance:**
- [x] Companion Task List links to `./tasks/49-architectural-declaration-tasks.md`

### F.6 — Update evaluation current-state.md

**File:** `docs/evaluation/current-state.md`
**Symbol:** architectural reality check sections
**Why it matters:** The evaluation must reflect the new architectural work.

**Acceptance:**
- [x] References to `syscall.rs` updated to `syscall/` directory
- [x] Syscall decomposition and ownership classification noted
- [x] Userspace-first rule adoption noted

### F.7 — Review root README.md

**File:** `README.md`
**Symbol:** project layout section
**Why it matters:** Root README must not contradict the current-vs-target distinction.

**Acceptance:**
- [x] No references to `syscall.rs` as a single file (none existed)
- [x] Architecture claims consistent with declared boundary

### F.8 — Bump kernel version to 0.49.0

**File:** `kernel/Cargo.toml`
**Symbol:** `version`
**Why it matters:** Version must reflect the completed phase.

**Acceptance:**
- [x] `kernel/Cargo.toml` version is `"0.49.0"`

---

## Documentation Notes

- The syscall handler was decomposed from a single 3000+ line file into a directory with 8 focused modules.
- Ownership classification uses three categories: Keep (permanent kernel mechanism), Move (future userspace server), Transition (evaluate per-phase).
- The userspace-first rule was added to both the architecture reference doc and the project contributor guidelines (AGENTS.md).
- Phase 49 is a documentation and structural phase; no kernel behavior changed.
