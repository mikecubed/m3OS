# Phase 05 — Userspace Entry: Task List

**Status:** Complete
**Source Ref:** phase-05
**Depends on:** Phase 4 ✅
**Goal:** Define a process abstraction, build a user address space with ring-3 mappings, implement the ring-0 to ring-3 transition, install the syscall gate, and run the first userspace program.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Process model + user address space | Phase 4 | ✅ Done |
| B | Ring 3 entry + syscall gate | A | ✅ Done |
| C | First userspace binary + validation + docs | B | ✅ Done |

---

## Track A — Process Model + User Address Space

### A.1 — Define a minimal process abstraction

**File:** `kernel/src/process/mod.rs`
**Symbols:** `Process`, `ProcessState`
**Why it matters:** Processes need their own page tables, file descriptors, and lifecycle state — separate from the kernel task that executes them.

**Acceptance:**
- [x] `Process` struct holds entry point, stack pointer, state, and per-process metadata
- [x] `ProcessState` enum tracks `Ready`, `Running`, `Zombie`, etc.
- [x] `Process::new()` initializes a process descriptor

### A.2 — Build a user address space

**File:** `kernel/src/mm/user_space.rs`
**Symbol:** `copy_to_user`
**Why it matters:** Userspace code must execute in its own virtual address space with code, stack, and kernel-protected mappings to enforce isolation.

**Acceptance:**
- [x] User code and stack are mapped into a separate address space region
- [x] Kernel memory is protected from userspace access

---

## Track B — Ring 3 Entry + Syscall Gate

### B.1 — Implement the transition into ring 3

**File:** `kernel/src/arch/x86_64/mod.rs`
**Symbol:** `enter_userspace`
**Why it matters:** The ring transition is the security boundary — it drops privilege from ring 0 to ring 3 using `iretq` with the correct segment selectors.

**Acceptance:**
- [x] `enter_userspace(entry, user_stack_top)` performs `iretq` to ring 3
- [x] Segment selectors use the user code/data GDT entries (RPL=3)

### B.2 — Install the syscall entry point and dispatcher

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbols:** `syscall_entry`, `syscall_handler`
**Why it matters:** The syscall gate is the only controlled path from ring 3 back to ring 0 — it must save user state, switch to the kernel stack, and dispatch to the correct handler.

**Acceptance:**
- [x] `syscall_entry` assembly stub saves user registers and switches to the kernel stack
- [x] `syscall_handler` dispatches based on the syscall number in `rax`
- [x] Syscall ABI follows the documented register convention (rdi, rsi, rdx, r10, r8, r9)

---

## Track C — First Userspace Binary + Validation + Docs

### C.1 — Implement debug_print and exit syscalls

**Component:** Syscall dispatcher in `kernel/src/arch/x86_64/syscall.rs`
**Why it matters:** The first userspace program needs at minimum a way to produce output and a way to terminate cleanly.

**Acceptance:**
- [x] `debug_print` syscall writes user-provided data to serial
- [x] `exit` syscall terminates the process and reclaims resources

### C.2 — Create the first userspace binary

**Component:** Userspace test binary (e.g., `userspace/exit0/`)
**Why it matters:** A working userspace binary proves the entire pipeline — ELF loading, address space setup, ring transition, syscall, and exit.

**Acceptance:**
- [x] Tiny userspace binary exercises the syscall and exit path
- [x] Userspace program prints and exits cleanly

### C.3 — Validate isolation and document the model

**Why it matters:** Ring 3 isolation must be verified, and the syscall ABI must be documented for all future userspace development.

**Acceptance:**
- [x] Invalid userspace access to kernel memory faults cleanly
- [x] Syscall path returns to the correct userspace location
- [x] Syscall ABI and ring transition are documented at a high level
- [x] First userspace memory layout and process assumptions are documented
- [x] A note explains how mature kernels support richer executable loading, memory permissions, and process models

---

## Documentation Notes

- Adds `kernel/src/process/mod.rs`, `kernel/src/arch/x86_64/syscall.rs`, and the first userspace binary.
- Builds on the scheduler from Phase 4 to run userspace processes as scheduled tasks.
