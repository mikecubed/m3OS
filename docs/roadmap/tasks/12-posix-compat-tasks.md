# Phase 12 — POSIX Compatibility Layer: Task List

**Status:** Complete
**Source Ref:** phase-12
**Depends on:** Phase 11 ✅
**Goal:** Implement Linux-compatible syscall numbers and bundle musl libc so that standard C programs compiled on the host run unmodified inside the OS.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Phase 11 deferred cleanup | — | ✅ Done |
| B | Safe user-memory access | — | ✅ Done |
| C | Build infrastructure | — | ✅ Done |
| D | Syscall gate expansion | A, B | ✅ Done |
| E | musl integration | D | ✅ Done |
| F | Validation | D, E | ✅ Done |
| G | Documentation | A–F | ✅ Done |

---

## Track A — Phase 11 Deferred Cleanup

### A.1 — Replace exception-handler context switch with two-phase kill path

**File:** `kernel/src/arch/x86_64/interrupts.rs`
**Why it matters:** Context-switching inside an exception handler violates the interrupt-handler contract and can corrupt kernel state.

**Acceptance:**
- [x] Per-CPU `KILL_PENDING` atomic flag set in exception handler
- [x] IRET to kernel-mode trampoline that calls `sys_exit(-11)` outside interrupt context

---

### A.2 — Free old address space on execve success

**File:** `kernel/src/mm/mod.rs`
**Symbol:** `free_process_page_table`
**Why it matters:** Without freeing the old page tables, every execve leaks the entire previous address space.

**Acceptance:**
- [x] `free_process_page_table(cr3_phys)` walks PML4 indices 0-255 (user half), frees mapped frames and page-table structure frames
- [x] Called in `sys_execve` after switching to the new CR3

---

### A.3 — Reap dead kernel tasks from scheduler

**File:** `kernel/src/task/scheduler.rs`
**Symbol:** `TaskState::Dead`
**Why it matters:** Without reaping, dead tasks accumulate in the scheduler queue indefinitely.

**Acceptance:**
- [x] `TaskState::Dead` state is defined
- [x] Scheduler skips and removes dead entries on scheduling passes

---

### A.4 — Mask TRAP_FLAG in SFMASK for SYSCALL

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `SFMask::write`
**Why it matters:** Without masking the trap flag, a userspace program with single-step enabled causes spurious #DB exceptions in the kernel syscall handler.

**Acceptance:**
- [x] `RFlags::TRAP_FLAG` is included in the SFMASK written during SYSCALL setup

---

## Track B — Safe User-Memory Access

### B.1 — Implement copy_from_user and copy_to_user

**File:** `kernel/src/mm/user_mem.rs`
**Symbol:** `copy_from_user`, `copy_to_user`
**Why it matters:** Direct pointer casts from syscall arguments cause ring-0 page faults on unmapped addresses; these functions validate mappings first.

**Acceptance:**
- [x] `copy_from_user(dst, src_vaddr)` translates via page tables and copies safely
- [x] `copy_to_user(dst_vaddr, src)` validates writability and copies safely
- [x] Both return `Result<(), ()>` on unmapped or invalid addresses

---

### B.2 — Replace sys_debug_print direct pointer cast

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `sys_debug_print`
**Why it matters:** The direct `from_raw_parts` pattern cannot detect unmapped-but-in-range addresses.

**Acceptance:**
- [x] `sys_debug_print` uses `copy_from_user` instead of direct `from_raw_parts`

---

### B.3 — Replace path buffer direct pointer cast

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Why it matters:** Path resolution from userspace pointers must go through validated copy, not raw casts.

**Acceptance:**
- [x] Path name buffer reads use `copy_from_user`

---

### B.4 — Replace sys_waitpid direct status_ptr write

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Why it matters:** Writing to a userspace status pointer via raw cast can trigger ring-0 fault on unmapped addresses.

**Acceptance:**
- [x] `sys_waitpid` status pointer write uses `copy_to_user`

---

### B.5 — Replace setup_abi_stack raw pointer pattern

**File:** `kernel/src/mm/elf.rs`
**Symbol:** `setup_abi_stack_with_envp`
**Why it matters:** Stack setup must propagate failures instead of silently dropping writes to unmapped addresses.

**Acceptance:**
- [x] Stack writes use safe copy equivalents with `Result` propagation

---

## Track C — Build Infrastructure

### C.1 — Move userspace ELF generation out of version control

**Why it matters:** Committed binaries cause stale binary diffs and noisy version control history.

**Acceptance:**
- [x] Userspace ELF binaries are generated during build, not committed to `kernel/initrd/`

---

## Track D — Syscall Gate Expansion

### D.1 — Audit musl-required Linux syscall numbers

**Why it matters:** Understanding the gap between implemented and required syscalls scopes the integration work.

**Acceptance:**
- [x] Linux syscall numbers required by musl are audited against existing implementations

---

### D.2 — Add Linux ABI dispatch table

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Why it matters:** musl emits Linux syscall numbers; the kernel must map them to internal implementations.

**Acceptance:**
- [x] Linux syscall number dispatch table maps to existing internal implementations
- [x] Phase 11 custom syscall numbers continue to work alongside

---

### D.3 — Implement read, write, open/openat, close

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Why it matters:** These are the fundamental I/O syscalls that every C program needs.

**Acceptance:**
- [x] `read(fd, buf, count)` works over the VFS IPC path
- [x] `write(fd, buf, count)` routes stdout/stderr to console
- [x] `open`/`openat`/`close` work over VFS

---

### D.4 — Implement fstat/fstatat, lseek

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Why it matters:** File metadata and seek are required by musl's stdio implementation.

**Acceptance:**
- [x] `fstat`/`fstatat` return minimal stat structs
- [x] `lseek` is implemented

---

### D.5 — Implement mmap (anonymous), munmap, brk/sbrk

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Why it matters:** musl's malloc depends on mmap and/or brk for heap allocation.

**Acceptance:**
- [x] `mmap(NULL, len, PROT_READ|PROT_WRITE, MAP_PRIVATE|MAP_ANONYMOUS)` allocates frames
- [x] Non-anonymous maps are rejected
- [x] `munmap` frees frames and unmaps pages
- [x] `brk`/`sbrk` backed by the frame allocator

---

### D.6 — Implement exit, exit_group, getpid

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Why it matters:** Process lifecycle syscalls must work under both custom and Linux ABI numbers.

**Acceptance:**
- [x] Linux-numbered `exit`, `exit_group`, and `getpid` route to existing Phase 11 implementations

---

### D.7 — Implement writev, readv

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Why it matters:** musl's printf uses writev for scatter-gather I/O.

**Acceptance:**
- [x] `writev`/`readv` implemented as loops over `write`/`read`

---

### D.8 — Implement getcwd, chdir

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Why it matters:** musl calls getcwd during initialization; even a stub returning "/" is sufficient.

**Acceptance:**
- [x] `getcwd` and `chdir` are implemented (initial stub returning `/`)

---

### D.9 — Implement ioctl (TIOCGWINSZ stub)

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Why it matters:** musl's terminal detection calls ioctl with TIOCGWINSZ; a stub prevents spurious failures.

**Acceptance:**
- [x] `ioctl` with TIOCGWINSZ stub satisfies musl's terminal detection

---

### D.10 — Implement uname

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Why it matters:** musl calls uname during initialization to identify the kernel.

**Acceptance:**
- [x] `uname` returns a fixed-string kernel identity

---

## Track E — musl Integration

### E.1 — Cross-compile musl for x86_64-unknown-none

**Why it matters:** musl is the libc that enables standard C programs to run on the OS.

**Acceptance:**
- [x] musl compiled targeting `x86_64-unknown-none` with `__syscall` stubs patched for Linux numbers

---

### E.2 — Write crt0 entry stub

**Why it matters:** The C runtime entry point bridges the System V ABI stack layout to `__libc_start_main`.

**Acceptance:**
- [x] `crt0.s` reads argc/argv/envp from stack, calls `__libc_start_main`, falls through to `exit`

---

### E.3 — Bundle musl headers and libc.a in disk image

**Why it matters:** Programs compiled on the host need musl headers and the static library to link against.

**Acceptance:**
- [x] musl headers and `libc.a` are available on the disk image

---

## Track F — Validation

### F.1 — C "hello world" runs inside the OS

**Why it matters:** The ultimate end-to-end test of the POSIX compatibility layer.

**Acceptance:**
- [x] A statically linked musl C binary compiled on the host runs unmodified inside the OS

---

### F.2 — printf, malloc, fopen, exit work correctly

**Why it matters:** These are the fundamental libc functions that exercise the syscall layer.

**Acceptance:**
- [x] `printf`, `malloc`, `fopen`, and `exit` all function correctly in the hello world binary

---

### F.3 — Phase 11 Rust binaries still work

**Why it matters:** The Linux ABI dispatch table must not break existing custom-ABI userspace.

**Acceptance:**
- [x] Existing Phase 11 Rust userspace binaries continue to work after adding Linux ABI dispatch

---

### F.4 — Trap flag does not cause spurious #DB in kernel

**Why it matters:** Validates that SFMASK correctly masks the trap flag during SYSCALL (Track A.4).

**Acceptance:**
- [x] A process with trap flag set in RFLAGS before SYSCALL does not generate spurious #DB in the kernel

---

## Track G — Documentation

### G.1 — Document Linux syscall number mapping and dual-dispatch

**Why it matters:** The dual-dispatch strategy (Phase 11 custom + Linux-compatible) must be understood by contributors.

**Acceptance:**
- [x] Linux syscall number mapping table and dual-dispatch strategy are documented

---

### G.2 — Document why musl over glibc

**Why it matters:** The choice of musl as the first libc target is a deliberate design decision.

**Acceptance:**
- [x] Explains what musl needs vs. glibc and why musl is the right first target for a toy OS

---

### G.3 — Document C runtime entry sequence

**Why it matters:** The _start -> __libc_start_main -> main -> exit chain is non-obvious.

**Acceptance:**
- [x] Documents the entry sequence with the System V stack layout each step expects

---

### G.4 — Document real vs. stubbed syscalls

**Why it matters:** Contributors and users need to know which syscalls are fully implemented vs. stubs.

**Acceptance:**
- [x] Documents which syscalls are real, which are stubbed, and what the gaps mean for compatibility

---

### G.5 — Document copy_from_user / copy_to_user design

**Why it matters:** The safe user-memory access pattern is a security-critical design decision.

**Acceptance:**
- [x] Documents the design and explains why direct pointer casts from syscall arguments are unsafe

---

## Documentation Notes

- Phase 12 built on the process model from Phase 11 to add Linux-compatible syscall numbers and musl libc support.
- Several items deferred from Phase 11 (exception handler cleanup, address space freeing, dead task reaping, SFMASK) were completed first.
- The safe user-memory access layer (`copy_from_user`/`copy_to_user`) replaced direct pointer casts throughout the syscall gate.
- Both custom Phase 11 syscall numbers and Linux-compatible numbers work simultaneously via dual dispatch.
