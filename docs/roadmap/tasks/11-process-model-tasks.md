# Phase 11 Tasks - ELF Loader and Process Model

**Depends on:** Phases 8 (Storage and VFS) and 9 (Framebuffer and Shell)

```mermaid
flowchart LR
    A["ELF parser"] --> B["address space setup"]
    B --> C["process table"]
    C --> D["execve + fork"]
    D --> E["exit + wait"]
    E --> F["validation"]
    F --> G["docs"]
```

## Implementation Tasks

### ELF Loader

- [x] P11-T001 Write an ELF64 header and program header parser that validates the magic,
  class, machine, and entry point fields before doing anything else.
- [x] P11-T002 Iterate `PT_LOAD` segments and map each one at the correct virtual address
  into a freshly allocated page table hierarchy.
- [x] P11-T003 Set page permissions per segment: `R+X` for text, `R+W` for data and BSS,
  read-only for read-only data.
- [x] P11-T004 Zero the BSS region (the portion of a `PT_LOAD` segment where `filesz < memsz`).
- [x] P11-T005 Allocate and map a userspace stack at a fixed high virtual address; place a
  guard page (unmapped) immediately below it.

### Process Table and Kernel State

- [x] P11-T006 Define a `Process` struct in the kernel holding: pid, parent pid, state
  (running / blocked / zombie), page table root, kernel stack pointer, and exit code.
- [x] P11-T007 Allocate a per-process kernel stack separate from the kernel's own boot stack.
- [x] P11-T008 Assign monotonically incrementing PIDs starting at 1; reserve PID 0 as idle.
- [x] P11-T009 Protect the process table with a spinlock; all modifications go through
  a single `proc_table` accessor so locking discipline is easy to audit.

### System V AMD64 ABI Entry Setup

- [x] P11-T010 Push `argc`, `argv`, and `envp` onto the userspace stack in the layout
  the System V AMD64 ABI specifies before entering ring 3.
- [x] P11-T011 Pass a minimal `envp` array (at least a null terminator) so programs that
  walk the environment do not fault.

### Syscalls

- [x] P11-T012 Implement `execve(path, argv, envp)`: load the named ELF into the calling
  process's address space, replacing the existing image.
- [x] P11-T013 Implement `fork()`: allocate a new process entry, copy the parent's page
  tables (eager copy, no COW yet), duplicate the kernel stack frame so the child
  returns 0 from `fork` and the parent returns the child's pid.
- [x] P11-T014 Implement `exit(code)` / `exit_group(code)`: mark the process zombie,
  store the exit code, free userspace pages, and wake any thread blocked in `waitpid`.
- [x] P11-T015 Implement `waitpid(pid, status, flags)`: block the caller until the target
  child becomes zombie, write the exit code into `*status`, and reap the child entry.
- [x] P11-T016 Implement `getpid()` and `getppid()` as trivial lookups into the process
  table.

### Init Integration

- [x] P11-T017 Update `init_task` to use `execve` to hand off to a userspace ELF binary
  stored on the disk image rather than running as a kernel thread forever.
- [x] P11-T018 Verify `init` can `fork`, execute a child binary with `execve`, and `wait`
  for it to exit before continuing.

## Validation Tasks

- [x] P11-T019 Load and run a minimal statically linked ELF (written in Rust with `#[no_std]`
  and a raw `_start`) that calls `exit(0)`; confirm the kernel receives exit code 0.
- [x] P11-T020 Load a binary that reads `argc` and `argv` from the stack and writes them
  to serial via a `write` syscall; confirm the values match what was passed.
- [x] P11-T021 Fork a child that exits with code 42; confirm `waitpid` in the parent
  returns 42.
- [x] P11-T022 Run two processes concurrently that each write a counter to serial; confirm
  neither corrupts the other's address space or register state.
- [x] P11-T023 Attempt to load a malformed ELF (bad magic, wrong architecture, truncated
  segment); confirm the kernel returns an error without panicking.
- [x] P11-T024 Stack overflow in a userspace process should fault and be caught by the
  kernel; the kernel should kill the process and not corrupt kernel state.

## Documentation Tasks

- [x] P11-T025 Document the ELF loading sequence step by step: parse → validate → allocate
  page table → map segments → zero BSS → set up stack → enter ring 3.
- [x] P11-T026 Document the `Process` struct fields, lifecycle states, and the state
  transition diagram (new → running → blocked → zombie → reaped).
- [x] P11-T027 Document what `fork` does to page tables and why eager copying is used
  instead of copy-on-write; note what COW would require.
- [x] P11-T028 Document the System V AMD64 ABI stack layout at process entry: `argc`,
  `argv`, `envp`, auxiliary vectors, and the initial `rsp` alignment requirement.
- [x] P11-T029 Add a "how real OSes differ" note covering: COW fork, dynamic linking via
  `PT_INTERP`, process groups, `clone` for threads, and `ptrace`.
