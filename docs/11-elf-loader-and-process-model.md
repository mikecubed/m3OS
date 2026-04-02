# Phase 11 — ELF Loader and Process Model

**Aligned Roadmap Phase:** Phase 11
**Status:** Complete
**Source Ref:** phase-11

Implementation reference for the ELF loader, per-process address spaces, System V AMD64
ABI stack layout, fork with eager page copy, and process lifecycle syscalls.

---

## T025: ELF Loading Sequence

Loading a userspace binary happens in five steps: parse → validate → allocate → map → enter.

### 1. Parse and validate the ELF header (`parse_ehdr`)

The kernel reads 64 bytes at offset 0 (the ELF64 Ehdr). Validation checks:

| Field | Expected | Error if wrong |
|---|---|---|
| `e_ident[0..4]` | `\x7fELF` | `ElfError::InvalidMagic` |
| `e_ident[EI_CLASS]` | `2` (64-bit) | `ElfError::Not64Bit` |
| `e_ident[EI_DATA]` | `1` (little-endian) | `ElfError::NotLittleEndian` |
| `e_machine` | `0x3E` (x86-64) | `ElfError::NotX86_64` |
| file length | ≥ 64 bytes | `ElfError::TruncatedHeader` |

All integer reads use explicit little-endian byte helpers (`read_u16_le`, `read_u64_le`) to
avoid alignment and padding issues with `repr(C)` structs.

### 2. Iterate program headers

`e_phoff` locates the first program header. The loader iterates `e_phnum` entries, each
`e_phentsize` bytes apart, and processes only `PT_LOAD` (type 1) segments.

### 3. Allocate frames and map segments (`map_load_segment`)

For each `PT_LOAD` segment:

1. Round the virtual range `[p_vaddr, p_vaddr + p_memsz)` down/up to 4 KiB page boundaries.
2. Allocate one fresh physical frame per page from the global frame allocator.
3. Map the frame into the **target** page table (which may not be the current CR3).
4. **Zero the entire frame** via `phys_off + frame.start_address()` — this handles BSS
   (the `p_memsz > p_filesz` region) as a side effect.
5. Copy file bytes for the `[p_offset, p_offset + p_filesz)` region.

**Why write via physical offset, not via virtual address?**
Writing through the virtual address would only work if the target page table is the current
CR3. Using `phys_offset + frame_phys_addr` is always valid regardless of which CR3 is loaded.

**Page permission flags** (P11-T003):

| ELF flags | Page table flags |
|---|---|
| `PF_W` set | `PRESENT | USER_ACCESSIBLE | WRITABLE` |
| `PF_X` set (no `PF_W`) | `PRESENT | USER_ACCESSIBLE` |
| neither | `PRESENT | USER_ACCESSIBLE | NO_EXECUTE` |

`PF_R` (readable) is implied for all segments.

### 4. Allocate the user stack (`map_user_stack`)

A fixed region at `[ELF_STACK_TOP - STACK_PAGES*4096, ELF_STACK_TOP)` is mapped with
`PRESENT | WRITABLE | USER_ACCESSIBLE | NO_EXECUTE`. One page directly below this region
is intentionally left unmapped — this is the **guard page**. A stack overflow will hit it
and trigger a page fault that the kernel catches and reports (T024).

```
ELF_STACK_TOP = 0x0000_7FFF_FFFF_F000
                ↑ top of stack (initial RSP points near here)
[ 8 × 4 KiB mapped writable stack pages ]
[ 1 × 4 KiB unmapped guard page ]
```

### 5. Enter ring 3

After loading, `setup_abi_stack` is called to build the SysV initial stack and return
the ABI RSP (the virtual address of `argc`). The kernel then either:
- calls `enter_userspace(entry, user_rsp)` (new process via execve), or
- calls `enter_userspace_with_retval(entry, user_rsp, 0)` (fork child, rax=0).

`user_rsp` points to `argc` on the ABI stack — **not** the raw top of the stack
allocation. Both helpers use `iretq` with CS set to the user code selector (ring-3
RPL=3), SS to the user data selector, and RFLAGS with `IF` set.

---

## T026: Process Struct, Lifecycle, and State Transitions

### `Process` struct (`kernel/src/process/mod.rs`)

```rust
pub struct Process {
    pub pid: Pid,               // unique process ID (u32, 1-based)
    pub ppid: Pid,              // parent PID (0 = orphan / kernel)
    pub state: ProcessState,    // current lifecycle state
    pub page_table_root: Option<PhysAddr>,  // PML4 physical address
    pub kernel_stack_top: u64,  // top of this process's kernel stack
    pub entry_point: u64,       // ring-3 entry point (set before execve)
    pub user_stack_top: u64,    // initial ring-3 RSP
    pub exit_code: Option<i32>, // set by exit() or a fault handler
}
```

`PROCESS_TABLE: Mutex<ProcessTable>` is the single authoritative source. All mutations
go through `PROCESS_TABLE.lock()`. `CURRENT_PID: AtomicU32` tracks which process is
running on the current CPU (updated during context switches and `sys_execve`).

### State machine

```
              spawn_process()
                    │
                    ▼
               ┌─────────┐
          ┌───►│  Ready  │◄──────────── (future: SIGCONT)
          │    └────┬────┘
          │         │ scheduled
          │         ▼
          │    ┌─────────┐
          │    │ Running │
          │    └────┬────┘
          │         │
          │    ┌────┴─────────┬────────────────┐
          │    │              │                │
          │    ▼              ▼                ▼
          │  yield       waitpid()        exit() / fault
          │    │         block            │
          │    │              │           ▼
          └────┘         ┌───┴───┐   ┌────────┐
                         │Blocked│   │ Zombie │
                         └───┬───┘   └────────┘
                             │            │
                             │  child      │ parent reaps
                             │  exits      │ (waitpid)
                             └────────────┘
```

Phase 11 does not implement full preemption — the `Blocked` state is entered only by
`waitpid` (yield-polling) and by kernel tasks that block on IPC. `Zombie` is the terminal
state until the parent reaps via `waitpid`.

---

## T027: Fork and Eager Page Copy

### Why eager copy, not COW?

Copy-on-Write (COW) fork requires:
1. Mark all shared pages read-only in both parent and child page tables.
2. On the first write fault in either process, copy the frame and re-map it writable.
3. A reference-counting system for shared frames.

For Phase 11, eager copy is sufficient and dramatically simpler: no reference counting,
no write-fault handling, no shared frame lifecycle. The toy OS only forks for short-lived
test binaries so the memory overhead is acceptable.

### What eager copy does (`copy_user_pages`)

Walk the parent's PML4 (indices 0–255, the user half):

```
for each PML4 entry (index 0..255):
  for each PDPT entry:
    for each PD entry:
      for each PT entry:
        if present and user-accessible:
          allocate new frame
          copy 4 KiB via physical offset
          map new frame at same virtual address in child page table
```

The kernel upper half (PML4 indices 256–511) is **shared** — not copied. Both processes
see the same kernel text, data, and heap, which is correct since the kernel is identical
for all processes and is mapped read-only (or protected by privilege level).

### What COW would require (Phase 12+)

- A `FrameRef` reference counter per physical frame.
- `map_to` with `WRITABLE` cleared, setting a `COW` software bit.
- A write-fault path in `page_fault_handler` that copies and re-maps.
- A `drop_frame` function that only frees when the refcount reaches zero.

---

## T028: System V AMD64 ABI Initial Stack Layout

When a process starts, `rsp` points to `argc`. The full layout from high to low address:

```
High address (ELF_STACK_TOP)
│
│  argv[0] string bytes (null-terminated)
│  argv[1] string bytes (null-terminated)
│  ... (all packed, growing downward)
│  [8-byte alignment padding if needed]
│
│  0x0000000000000000  ← AT_NULL value  (aux vector terminator)
│  0x0000000000000000  ← AT_NULL type
│
│  0x0000000000000000  ← envp[0] = NULL (empty environment, P11-T011)
│
│  0x0000000000000000  ← argv[argc] = NULL
│  ptr → argv[argc-1]
│  ...
│  ptr → argv[1]
│  ptr → argv[0]
│  argc                ← rsp on entry
│
Low address
```

Key ABI rules:
- `[rsp]` = `argc` (a `u64`), **not** a pointer.
- `[rsp + 8]` = `argv[0]`, a pointer to the first null-terminated string.
- `[rsp + 8*(1+argc)]` = NULL (argv terminator).
- `[rsp + 8*(2+argc)]` = NULL (envp terminator — minimal empty environment).
- `[rsp + 8*(3+argc)]` = AT_NULL type = 0; `[rsp + 8*(4+argc)]` = AT_NULL value = 0.
- On entry `rsp % 16 == 8` — the same state as after a `call` instruction has pushed
  a return address. The SysV AMD64 ABI requires `(rsp + 8) % 16 == 0` so that when
  `_start` calls its first function the stack is 16-byte aligned. Because our ELFs
  enter at `_start` directly (no `call` wraps it), `setup_abi_stack` pads to ensure
  the returned RSP satisfies this requirement.

The `setup_abi_stack` function builds this layout by writing through the physical-memory
offset so the writes are valid even when the target page table is not the current CR3.

---

## T029: How Real OSes Differ

### COW fork (Linux, macOS)

Real kernels implement `fork()` with Copy-on-Write: all pages are shared and marked
read-only; a write page fault triggers a copy. This makes `fork()` O(1) for large
address spaces (only page table entries are copied, not frame contents).

Linux uses `mm_struct` and `vm_area_struct` to describe virtual memory regions. Each
region has a `vm_flags` field (`VM_READ`, `VM_WRITE`, `VM_EXEC`, `VM_SHARED`). The
`copy_page_range()` function in `mm/memory.c` handles the actual PTE walk and COW marking.

### PT_INTERP and the dynamic linker

Most ELF binaries for Linux have a `PT_INTERP` segment that names the dynamic linker
(`/lib64/ld-linux-x86-64.so.2`). The kernel loads both the main binary and the interpreter,
then enters at the interpreter's entry point — not the main binary's. The dynamic linker
resolves shared library references before jumping to `main`.

Our ELF loader ignores `PT_INTERP` — all binaries must be statically linked
(`x86_64-unknown-none` ensures this).

### Process groups, sessions, signals

Linux processes belong to a **process group** (for job control) and a **session** (for
terminal association). Signals (`SIGKILL`, `SIGTERM`, `SIGCHLD`) are delivered per-process
or per-group. `wait4()` returns detailed status in a `siginfo_t`.

Phase 11 has no signals: exit codes are integers, faults use a fixed SIGSEGV exit code
of -11, and there is no signal delivery mechanism.

### `clone()` and threads

Linux `fork()` is actually `clone(CLONE_VM | CLONE_FS | ... | SIGCHLD)`. Threads are
`clone()` with `CLONE_VM` set — they share the same page tables and address space.
`pthread_create()` calls `clone()` with a new stack.

Phase 11 has no thread model — every process has one kernel task and one ring-3 execution
context.

### `ptrace` and `/proc`

Linux exposes process state through `/proc/PID/maps`, `/proc/PID/mem`, and the `ptrace`
syscall (used by debuggers and strace). Phase 11 has no `/proc` filesystem and no `ptrace`
— inspection is only possible via kernel log output.

### execve arguments and environment

Real `execve(path, argv, envp)` passes a full null-terminated string array for both argv
and envp. The kernel copies these into the new process's stack before entering ring 3.
Phase 11 passes only the binary name as argv[0] and an empty envp for simplicity.
