# Phase 11 — ELF Loader and Process Model

**Branch:** `phase-11-process-model`
**Depends on:** Phases 8 (Storage + VFS) and 9 (Framebuffer + Shell) — both complete.

## Track Status

| Track | Scope | Status |
|---|---|---|
| A | ELF parser + memory (`mm/elf.rs`) | ✅ done |
| B | Process table (`process/`) | ✅ done |
| C | ABI + syscalls | ✅ done |
| D | Init integration + validation | ✅ done |
| E | Documentation | ✅ done |

---

## Track A — ELF Parser and Process Memory

| Task | Description | Status |
|---|---|---|
| P11-T001 | ELF64 header + phdr parser (magic, class, machine, entry) | ✅ |
| P11-T002 | Map PT_LOAD segments into fresh page table hierarchy | ✅ |
| P11-T003 | Page permissions: R+X text, R+W data/BSS, RO for rodata | ✅ |
| P11-T004 | Zero BSS (filesz < memsz portion of PT_LOAD) | ✅ |
| P11-T005 | Stack at fixed high vaddr + unmapped guard page below | ✅ |

## Track B — Process Table and Kernel State

| Task | Description | Status |
|---|---|---|
| P11-T006 | `Process` struct: pid, ppid, state, page table root, kstack ptr, exit code | ✅ |
| P11-T007 | Per-process kernel stack (separate from boot stack) | ✅ |
| P11-T008 | Monotonic PID counter starting at 1; PID 0 reserved for idle | ✅ |
| P11-T009 | Spinlock-protected process table; all writes via single accessor | ✅ |

## Track C — ABI Entry + Syscalls

| Task | Description | Status |
|---|---|---|
| P11-T010 | Push argc/argv/envp in System V AMD64 ABI layout before ring-3 entry | ✅ |
| P11-T011 | Minimal envp (null terminator) so env-walking programs don't fault | ✅ |
| P11-T012 | `execve(path, argv, envp)`: load ELF, replace current image | ✅ |
| P11-T013 | `fork()`: eager copy of page tables; child returns 0, parent returns child pid | ✅ |
| P11-T014 | `exit(code)` / `exit_group(code)`: zombie, free pages, wake waiting parent | ✅ |
| P11-T015 | `waitpid(pid, status, flags)`: block until child zombie, reap | ✅ |
| P11-T016 | `getpid()` / `getppid()` trivial table lookups | ✅ |

## Track D — Init Integration and Validation

| Task | Description | Status |
|---|---|---|
| P11-T017 | Update `init_task` to exec a userspace ELF from disk | ✅ |
| P11-T018 | Verify init can fork, exec child, wait for exit | ✅ |
| P11-T019 | Load minimal ELF calling `exit(0)`; confirm kernel receives code 0 | ✅ |
| P11-T020 | Binary reads argc/argv, writes to serial; confirm values match | ✅ |
| P11-T021 | Fork child that exits 42; confirm waitpid returns 42 | ✅ |
| P11-T022 | Two concurrent processes write counters; no address space corruption | ✅ |
| P11-T023 | Malformed ELF (bad magic, wrong arch, truncated) → error, no panic | ✅ |
| P11-T024 | Stack overflow in userspace → kernel catches fault, kills process cleanly | ✅ |

## Track E — Documentation

| Task | Description | Status |
|---|---|---|
| P11-T025 | ELF loading sequence: parse → validate → alloc → map → zero BSS → stack → enter | ✅ |
| P11-T026 | Process struct fields, lifecycle states, state transition diagram | ✅ |
| P11-T027 | fork + page tables: why eager copy, what COW would require | ✅ |
| P11-T028 | System V AMD64 ABI stack layout: argc, argv, envp, aux vectors, rsp alignment | ✅ |
| P11-T029 | "How real OSes differ": COW fork, PT_INTERP, process groups, clone, ptrace | ✅ |
