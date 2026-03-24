# Phase 12 — POSIX Compatibility Layer

Implementation reference for the Linux-compatible syscall ABI, musl libc integration,
safe user-memory access, and the C runtime startup sequence.

---

## Linux Syscall Number Mapping (T034)

Phase 12 introduces a Linux-compatible syscall dispatch table alongside the existing
Phase 11 kernel-native numbers. The `syscall_handler` match arms map Linux numbers
directly:

| Linux # | Name          | Implementation              | Notes                          |
|---------|---------------|-----------------------------|--------------------------------|
| 0       | read          | `sys_linux_read`            | reads from static ramdisk fd table |
| 1       | write         | `sys_linux_write`           | stdout/stderr to kernel serial log |
| 2       | open          | `sys_linux_open`            | static ramdisk path lookup     |
| 3       | close         | `sys_linux_close`           | fd table release               |
| 5       | fstat         | `sys_linux_fstat`           | minimal stat struct            |
| 8       | lseek         | `sys_linux_lseek`           | per-fd offset update           |
| 9       | mmap          | `sys_linux_mmap`            | anonymous MAP_PRIVATE only     |
| 11      | munmap        | `sys_linux_munmap`          | stub (no-op)                   |
| 12      | brk           | `sys_linux_brk`             | frame-backed heap              |
| 16      | ioctl         | `sys_linux_ioctl`           | TIOCGWINSZ only                |
| 19      | readv         | `sys_linux_readv`           | loop over read                 |
| 20      | writev        | `sys_linux_writev`          | loop over write                |
| 39      | getpid        | `sys_getpid`                | Phase 11 path                  |
| 57      | fork          | `sys_fork`                  | Phase 11 path                  |
| 59      | execve        | `sys_execve`                | Phase 11 path                  |
| 60      | exit          | `sys_exit`                  | Phase 11 path                  |
| 61      | wait4         | `sys_waitpid`               | Phase 11 waitpid               |
| 63      | uname         | `sys_linux_uname`           | fixed identity string          |
| 79      | getcwd        | `sys_linux_getcwd`          | always returns "/"             |
| 80      | chdir         | `sys_linux_chdir`           | stub (always ok)               |
| 110     | getppid       | `sys_getppid`               | Phase 11 path                  |
| 158     | arch_prctl    | `sys_linux_arch_prctl`      | ARCH_SET_FS only (TLS)         |
| 218     | set_tid_address | `sys_linux_set_tid_address` | stub, returns PID            |
| 231     | exit_group    | `sys_exit`                  | alias for exit                 |
| 257     | openat        | `sys_linux_open`            | ignores dirfd                  |
| 262     | newfstatat    | `sys_linux_fstatat`         | fstat via path lookup          |

### Dual-dispatch strategy

The Phase 11 kernel-native syscall numbers (4, 6, 7, 10) overlap with Linux numbers
(stat, lstat, poll, mprotect). Phase 12 resolved this by:

1. Reassigning the custom debug-print syscall from 12 → 0x1000.
2. Keeping IPC syscalls on numbers 4, 7, 10 (Phase 6 kernel-task-only paths).
3. Adding Linux numbers for all musl-required calls.

Unrecognised syscall numbers return `-ENOSYS` (negative errno convention) so musl's raw syscall wrappers observe a standard error.

### Syscall register ABI

The entry stub (`syscall_entry` in `arch/x86_64/syscall.rs`) maps the Linux syscall
register convention to the Rust SysV calling convention:

| Register | Linux role       | SysV param for `syscall_handler` |
|----------|------------------|----------------------------------|
| rax      | syscall number   | rdi (1st param)                  |
| rdi      | arg0             | rsi (2nd param)                  |
| rsi      | arg1             | rdx (3rd param)                  |
| rdx      | arg2             | rcx (4th param)                  |
| r10      | arg3             | saved to `SYSCALL_ARG3` static   |
| r8       | arg4             | r8 (5th param = user_rip)        |
| r9       | arg5             | r9 (6th param = user_rsp)        |

**Critical**: The Linux ABI requires all registers except rax, rcx, r11 to be
preserved across syscalls. The entry stub saves rdi/rsi/rdx/r10/r8/r9 on the
kernel stack before the SysV rearrangement and restores them before `sysretq`.
Without this, musl's `__stdout_write` (which stores FILE* in r8 across an ioctl
syscall) would receive a NULL FILE pointer and crash.

---

## musl vs. glibc (T035)

### Why musl is the right first target

| Factor              | musl                              | glibc                            |
|---------------------|-----------------------------------|----------------------------------|
| Static linking      | First-class (`musl-gcc -static`)  | Supported but discouraged        |
| Binary size         | ~30 KB for hello world            | ~800 KB+ for hello world         |
| Syscall surface     | Minimal, direct syscall wrappers  | Large, many internal syscalls    |
| TLS init            | Simple `arch_prctl(ARCH_SET_FS)`  | Complex, uses `clone`, `futex`   |
| Thread model        | Optional, simple                  | Always initialised               |
| Error tolerance     | Graceful fallback for missing calls | Often hard-crashes             |

musl's philosophy of small, correct, static-first design matches a toy OS kernel
that implements a minimal syscall set. A statically-linked musl binary needs only
~25 syscalls to run a hello world, versus ~60+ for glibc.

### Build integration

`cargo xtask run` calls `build_musl_bins()` which invokes `musl-gcc -static -O2`
for each C source in `userspace/`. The resulting ELF is placed in `kernel/initrd/`
and embedded via `include_bytes!` in `kernel/src/fs/ramdisk.rs`.

---

## C Runtime Entry Sequence (T036)

When the kernel loads a musl-linked static binary and enters userspace:

```
kernel: fork_child_trampoline
  → Cr3::write(process PML4)
  → enter_userspace_with_retval(entry, rsp, 0)
  → IRETQ to ring 3

user:   _start                          (musl crt/x86_64/crt_arch.h)
  → reads argc, argv, envp from stack
  → calls __libc_start_main(main, argc, argv)

        __libc_start_main
          → computes envp = argv + argc + 1
          → computes auxv = envp + n + 1  (scans past envp NULL)
          → calls __init_libc(envp, argv[0])

              __init_libc
                → builds aux[AUX_CNT] array from auxv entries
                → reads AT_PAGESZ → libc.page_size
                → calls __init_tls(aux)

                    __init_tls
                      → reads aux[AT_PHDR], aux[AT_PHNUM]
                      → scans phdrs for PT_TLS segment
                      → allocates TLS area (builtin or via mmap)
                      → calls __set_thread_area (arch_prctl ARCH_SET_FS)
                      → calls set_tid_address
                      → returns

          → calls main(argc, argv, envp)
          → calls exit(main's return value)
```

### Auxiliary vector requirements

The kernel populates the auxiliary vector on the user stack in `setup_abi_stack`:

| Key        | Value                          | Why musl needs it                  |
|------------|--------------------------------|------------------------------------|
| AT_PHDR=3  | `min_vaddr + load_bias + phoff`| `__init_tls` scans phdrs for PT_TLS|
| AT_PHNUM=5 | ELF header `e_phnum`           | bounds the phdr scan loop          |
| AT_PAGESZ=6| 4096                           | mmap alignment, stack guard        |
| AT_RANDOM=25| pointer to 16 stack bytes     | stack canary seed                  |
| AT_NULL=0  | 0                              | terminates the auxv scan           |

---

## Syscall Implementation Status (T037)

### Real implementations (behaviour matches Linux semantics)

- **read/write/writev/readv**: Global kernel-side FD table; reads from static ramdisk, writes to kernel serial log.
- **open/close**: Global FD table with ramdisk file lookup.
- **fstat/fstatat**: Returns file size, mode, block size from ramdisk.
- **lseek**: Per-fd offset tracking with SEEK_SET/CUR/END.
- **mmap**: Anonymous MAP_PRIVATE|MAP_ANONYMOUS with frame allocation.
- **brk**: Frame-backed heap growth, per-process `brk_current`.
- **exit/exit_group**: Zombie + `mark_current_dead` + CR3 restore.
- **fork**: Eager page copy, child process table entry, kernel task.
- **waitpid**: Spin-yield loop with CR3/CURRENT_PID restore on resume.
- **getpid/getppid**: Read from process table.
- **arch_prctl(ARCH_SET_FS)**: Writes FS.base MSR for TLS.
- **uname**: Fixed identity string.

### Stubs (return success but do minimal/no work)

- **munmap**: No-op (bump allocator cannot free).
- **getcwd**: Always returns "/".
- **chdir**: Always returns 0 (success).
- **ioctl(TIOCGWINSZ)**: Returns fixed 80×25 terminal size.
- **set_tid_address**: Returns PID, ignores pointer.

### Known gaps (return -ENOSYS / u64::MAX)

- **mprotect** (10): No page permission changes after mapping.
- **clone/clone3**: No threads. fork() creates processes only.
- **futex**: No futex support (no threads to synchronize).
- **sigaction/rt_sigaction**: No signal delivery framework.
- **pipe/dup/dup2**: No pipe or fd duplication.
- **socket/connect/bind**: No networking.
- **madvise**: No memory advice.

Most gaps are acceptable because musl's static single-threaded path doesn't
require them for basic stdio + malloc + exit programs.

---

## Safe User-Memory Access (T038)

### Why direct pointer casts are unsafe

The kernel and userspace share the same virtual address space (split at PML4[256]).
A naive `*(user_ptr as *const T)` in kernel code has three problems:

1. **No permission check**: The pointer might refer to kernel memory (the upper
   half) or an unmapped page. The kernel would read/write kernel data thinking
   it was user data, or trigger a kernel page fault.

2. **TOCTOU races**: Even if you check the address range, another thread (or
   interrupt) could unmap the page between the check and the access. (Not yet
   relevant on our single-CPU system, but the API should be correct by design.)

3. **Canonical address trap**: Non-canonical addresses (bits 48–63 not sign-extended)
   cause a #GP, not a page fault. A malicious user could trigger a kernel panic.

### `copy_from_user` / `copy_to_user` design

Both functions in `kernel/src/mm/user_mem.rs` follow the same pattern:

1. **Range validation**: Reject null pointers, non-canonical addresses, addresses
   above `USER_VADDR_MAX` (128 TiB), and copies larger than 64 KiB.

2. **Page-table walk**: Use `OffsetPageTable::translate()` to resolve each page.
   Check `PRESENT` and `USER_ACCESSIBLE` flags (plus `WRITABLE` for writes).
   This confirms the user actually has access to the page.

3. **Physical-memory copy**: Read/write through `phys_offset + frame_addr + offset`,
   which is always valid in the kernel's address space.

Because `paging::get_mapper()` operates on the current CR3, callers must ensure the
target process's page table is active before calling `copy_from_user` / `copy_to_user`.

---

## Page Table Isolation

### `new_process_page_table` design

Each user process gets a private PML4 with three regions:

| PML4 range | Source                | Purpose                              |
|------------|-----------------------|--------------------------------------|
| [0]        | Deep-copied from kernel (if present) | User code/data at USER_VADDR_MIN; private PDPT+PD prevents ELF loader from contaminating kernel structures |
| [1..256]   | Shallow-copied from kernel | Kernel binary + physmem offset at PML4[2]; never touched by ELF loader |
| [256..512] | Shallow-copied from kernel | Kernel heap, stacks, physmem upper half |

### CR3 lifecycle

- **Process start**: `fork_child_trampoline` calls `Cr3::write(process_pml4)`.
- **Syscall handling**: Runs with process CR3 (SYSCALL doesn't change CR3).
- **Process exit**: `sys_exit` calls `restore_kernel_cr3()` before `mark_current_dead()`.
- **Blocking syscalls**: `sys_waitpid` restores the caller's CR3 from the process
  table after `yield_now()` returns (the dying child may have set kernel CR3).
- **Kernel PML4**: Stored once in `KERNEL_PML4_PHYS` during `mm::init` and used
  by `new_process_page_table` instead of `Cr3::read()` to avoid inheriting a
  dead process's mappings.
