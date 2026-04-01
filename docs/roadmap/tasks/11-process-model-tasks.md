# Phase 11 — ELF Loader and Process Model: Task List

**Status:** Complete
**Source Ref:** phase-11
**Depends on:** Phase 8 ✅, Phase 9 ✅
**Goal:** Load and run statically linked ELF binaries in ring 3 with fork, execve, exit, and waitpid, establishing the full process lifecycle.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | ELF loader | — | ✅ Done |
| B | Process table and kernel state | — | ✅ Done |
| C | System V AMD64 ABI entry setup | A, B | ✅ Done |
| D | Syscalls (fork, execve, exit, wait) | A, B, C | ✅ Done |
| E | Init integration | D | ✅ Done |
| F | Validation | A–E | ✅ Done |
| G | Documentation | A–F | ✅ Done |

---

## Track A — ELF Loader

### A.1 — ELF64 header and program header parser

**File:** `kernel/src/mm/elf.rs`
**Symbol:** `parse_ehdr`, `Ehdr`, `ElfError`
**Why it matters:** Correct parsing and validation of the ELF header prevents loading corrupt or incompatible binaries.

**Acceptance:**
- [x] Validates magic, class (64-bit), machine (x86_64), and entry point fields
- [x] Rejects malformed ELFs with typed errors (`ElfError`)

---

### A.2 — Map PT_LOAD segments into a new address space

**File:** `kernel/src/mm/elf.rs`
**Symbol:** `parse_phdr`, `Phdr`
**Why it matters:** Each loadable segment must be placed at its specified virtual address with correct permissions.

**Acceptance:**
- [x] Iterates `PT_LOAD` segments and maps each at the correct virtual address
- [x] Allocates a fresh page table hierarchy for the new process

---

### A.3 — Set page permissions per segment

**File:** `kernel/src/mm/elf.rs`
**Why it matters:** Enforcing R+X for text, R+W for data, and read-only for rodata prevents accidental or malicious memory corruption.

**Acceptance:**
- [x] Text segments are mapped R+X
- [x] Data and BSS segments are mapped R+W
- [x] Read-only data segments are mapped read-only

---

### A.4 — Zero the BSS region

**File:** `kernel/src/mm/elf.rs`
**Why it matters:** Uninitialized global variables must start at zero per the C/Rust ABI.

**Acceptance:**
- [x] The portion of a `PT_LOAD` segment where `filesz < memsz` is zeroed

---

### A.5 — Allocate userspace stack with guard page

**Files:** `kernel/src/mm/elf.rs`, `kernel/src/mm/user_space.rs`
**Symbol:** `USER_STACK_TOP`, `USER_STACK_PAGES`
**Why it matters:** A guard page below the stack catches stack overflows with a page fault instead of silent corruption.

**Acceptance:**
- [x] Userspace stack is allocated at a fixed high virtual address
- [x] An unmapped guard page sits immediately below the stack

---

## Track B — Process Table and Kernel State

### B.1 — Define the Process struct

**File:** `kernel/src/process/mod.rs`
**Symbol:** `Process`, `ProcessState`
**Why it matters:** The Process struct is the kernel's authoritative record of each running program's state.

**Acceptance:**
- [x] Holds pid, parent pid, state (Ready/Running/Blocked/Zombie), page table root, kernel stack pointer, exit code
- [x] State transitions are well-defined and auditable

---

### B.2 — Per-process kernel stack allocation

**File:** `kernel/src/process/mod.rs`
**Why it matters:** Each process needs its own kernel stack so the kernel can handle syscalls and interrupts concurrently.

**Acceptance:**
- [x] Each process gets a dedicated kernel stack separate from the boot stack

---

### B.3 — Monotonically incrementing PIDs

**File:** `kernel/src/process/mod.rs`
**Why it matters:** Unique PIDs are essential for process identification; PID 0 is reserved for idle.

**Acceptance:**
- [x] PIDs start at 1 and increment monotonically
- [x] PID 0 is reserved as idle

---

### B.4 — Process table with spinlock protection

**File:** `kernel/src/process/mod.rs`
**Symbol:** `ProcessTable`
**Why it matters:** A single locked accessor for all process table mutations ensures consistent state under concurrency.

**Acceptance:**
- [x] Process table is protected by a spinlock
- [x] All modifications go through a single `ProcessTable` accessor

---

## Track C — System V AMD64 ABI Entry Setup

### C.1 — Push argc, argv, envp onto userspace stack

**File:** `kernel/src/mm/elf.rs`
**Symbol:** `setup_abi_stack`, `setup_abi_stack_with_envp`
**Why it matters:** The System V AMD64 ABI requires argc/argv/envp in a specific stack layout for _start to work correctly.

**Acceptance:**
- [x] `argc`, `argv`, and `envp` are pushed in System V AMD64 ABI layout before entering ring 3

---

### C.2 — Provide minimal envp array

**File:** `kernel/src/mm/elf.rs`
**Symbol:** `setup_abi_stack_with_envp`
**Why it matters:** Programs that walk the environment pointer must not fault on a missing null terminator.

**Acceptance:**
- [x] A minimal `envp` array (at least a null terminator) is provided

---

## Track D — Syscalls

### D.1 — Implement execve

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `sys_execve` (via dispatch)
**Why it matters:** execve replaces the current process image with a new ELF, enabling program execution.

**Acceptance:**
- [x] Loads the named ELF into the calling process's address space, replacing the existing image

---

### D.2 — Implement fork

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Why it matters:** fork creates new processes by duplicating the parent, enabling the Unix process model.

**Acceptance:**
- [x] Allocates a new process entry and copies the parent's page tables (eager copy, no COW)
- [x] Child returns 0, parent returns child's PID

---

### D.3 — Implement exit / exit_group

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Why it matters:** Clean process termination requires freeing resources and notifying waiters.

**Acceptance:**
- [x] Marks process zombie, stores exit code, frees userspace pages
- [x] Wakes any thread blocked in waitpid

---

### D.4 — Implement waitpid

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Why it matters:** waitpid lets a parent block until a child exits, enabling sequential process orchestration.

**Acceptance:**
- [x] Blocks the caller until the target child becomes zombie
- [x] Writes exit code into `*status` and reaps the child entry

---

### D.5 — Implement getpid and getppid

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Why it matters:** Basic process identity queries are needed by nearly every non-trivial program.

**Acceptance:**
- [x] `getpid()` and `getppid()` return correct values from the process table

---

## Track E — Init Integration

### E.1 — Update init to use execve for userspace handoff

**File:** `kernel/src/main.rs`
**Symbol:** `init_task`
**Why it matters:** init must transition from a kernel thread to executing a userspace ELF binary.

**Acceptance:**
- [x] `init_task` uses `execve` to hand off to a userspace ELF binary from the disk image

---

### E.2 — Verify init can fork, exec, and wait

**Why it matters:** The full fork-exec-wait cycle in init proves the process lifecycle is complete.

**Acceptance:**
- [x] init can fork, execute a child binary with execve, and wait for it to exit

---

## Track F — Validation

### F.1 — Minimal ELF exits with code 0

**Why it matters:** The simplest possible end-to-end test of ELF loading and process exit.

**Acceptance:**
- [x] A minimal statically linked ELF calls `exit(0)` and the kernel receives exit code 0

---

### F.2 — Binary reads argc/argv from stack

**Why it matters:** Validates the System V ABI stack layout is correct.

**Acceptance:**
- [x] A binary reads `argc` and `argv` from the stack and writes them to serial; values match

---

### F.3 — Fork child exits with code 42

**Why it matters:** Validates the fork + waitpid path returns the correct exit code.

**Acceptance:**
- [x] A forked child exits with code 42; `waitpid` in the parent returns 42

---

### F.4 — Two concurrent processes do not corrupt each other

**Why it matters:** Address space isolation is fundamental to process safety.

**Acceptance:**
- [x] Two processes writing counters to serial do not corrupt each other's address space or registers

---

### F.5 — Malformed ELF returns error without panic

**Why it matters:** The kernel must handle invalid input gracefully, not crash.

**Acceptance:**
- [x] Bad magic, wrong architecture, and truncated segments all return errors without panicking

---

### F.6 — Stack overflow is caught by the kernel

**Why it matters:** The guard page must trigger a fault that the kernel handles, not silent corruption.

**Acceptance:**
- [x] Userspace stack overflow faults and the kernel kills the process without corrupting kernel state

---

## Track G — Documentation

### G.1 — Document ELF loading sequence

**Why it matters:** The step-by-step loading process is the core of this phase.

**Acceptance:**
- [x] Documents: parse, validate, allocate page table, map segments, zero BSS, set up stack, enter ring 3

---

### G.2 — Document Process struct and lifecycle states

**Why it matters:** The state transition diagram (new -> running -> blocked -> zombie -> reaped) is essential for understanding process management.

**Acceptance:**
- [x] Process struct fields, lifecycle states, and state transition diagram are documented

---

### G.3 — Document fork's page table handling

**Why it matters:** The eager copy vs. COW decision affects performance and must be understood.

**Acceptance:**
- [x] Documents what fork does to page tables and why eager copying is used
- [x] Notes what COW would require

---

### G.4 — Document System V AMD64 ABI stack layout

**Why it matters:** The precise stack layout at process entry is an ABI contract that must be exact.

**Acceptance:**
- [x] Documents argc, argv, envp, auxiliary vectors, and initial rsp alignment requirement

---

### G.5 — Note on how real OSes differ

**Why it matters:** Sets expectations for production features beyond this toy implementation.

**Acceptance:**
- [x] Covers COW fork, dynamic linking via PT_INTERP, process groups, clone for threads, and ptrace

---

## Documentation Notes

- Phase 11 built on the VFS from Phase 8 and shell from Phase 9 to create a full process lifecycle.
- ELF loading, fork, execve, exit, and waitpid together enable the Unix process model.
- Eager page table copying (no COW) was chosen for simplicity; COW is noted as future work.
