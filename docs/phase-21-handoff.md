# Phase 21 (Ion Shell Integration) — Handoff Document

**Branch:** `docs/phase-21-ion-shell`
**PR:** https://github.com/mikecubed/m3OS/pull/27
**Date:** 2026-03-27

## What's Done

### Infrastructure (fully working)
- Ion cross-compiled as static non-PIE ET_EXEC binary (3.1 MB stripped)
- `build_ion()` in xtask: clone from GitHub, build with `RUSTFLAGS="-C relocation-model=static"`, strip, cache
- Ion embedded in ramdisk at `/bin/ion` and `/bin/ion.elf`
- Phase 20 shell renamed to `/bin/sh0` (regression harness)
- Init uses sh0 as primary interactive shell, ion available for script mode
- Serial stdin feeder for QEMU testing via piped input
- `/bin/PROMPT` binary for ion's prompt expansion
- `/bin/stdin-test.elf` for pipeline verification

### Syscall stubs (all working)
18 new syscalls added: `access`, `mprotect`, `clone`, `socketpair` (pipe-based), `fcntl`, `getuid/gid/euid/egid`, `getpgrp`, `set_robust_list`, `prlimit64`, `getrandom`, `ioctl TCGETS/TCSETS/-ENOTTY`, `pipe2`, `dup3`, `futex`, `clock_gettime`, `gettimeofday`, `sendto`, `recvfrom` (non-blocking), `/dev/null` device.

### Critical kernel bugs fixed
1. **Futex yield CR3 corruption** — `sys_futex` called `yield_now()` without `restore_caller_context()`, leaving another process's page table active
2. **fault_kill_trampoline IRET stack** — page fault/GPF handlers didn't set RSP/SS in modified interrupt frame, trampoline ran on user stack
3. **Fork child callee-saved registers** — `fork_child_trampoline` entered userspace with kernel-garbage in R12-R15/RBX/RBP via `global_asm` trampoline
4. **FS.base (TLS) not saved/restored** — `arch_prctl(ARCH_SET_FS)` wrote the MSR but didn't save per-process; context switches corrupted TLS
5. **fault_kill_trampoline missing cleanup** — added `close_all_fds_for` and `send_sigchld_to_parent`
6. **free_process_page_table safety** — added bounds validation for physical addresses

## Remaining Blockers (2 issues)

### Blocker 1: Musl child processes crash writing above stack top

**Symptom:** Every musl C binary exec'd from sh0 (echo, ls, pwd, etc.) page faults at `ELF_STACK_TOP + N*4096` in `__libc_malloc_impl` (rip=0x403003).

**Root cause:** Musl's `__init_tls` allocates the TLS/TCB block at an address ABOVE the initial stack pointer. On Linux, the kernel maps a much larger stack region (8 MB default) and the area above RSP is valid. Our kernel only maps `STACK_PAGES` (64) pages below `ELF_STACK_TOP`. Musl writes above our mapped region.

**Key evidence:**
- Crash address is always exactly `ELF_STACK_TOP + (extra_pages * 4096)` — the first unmapped page
- rip=0x403003 is `__libc_malloc_impl` — musl's internal allocator
- Disassembly shows no TLS prefix (not `fs:` access) — it's a regular memory write through a pointer
- Adding extra pages above shifts the fault address proportionally

**Recommended fix approach:**
1. **Read musl's `__init_tls` source** (`src/env/__init_tls.c` in musl) to understand exactly how it calculates the TLS block address and size
2. The TLS block is likely allocated via `__syscall(SYS_mmap, ...)` at a specific address, OR musl's `_start` sets up the thread-control block above the stack
3. **Most likely fix:** Our `ELF_STACK_TOP` should not be the literal top of mapped memory. Instead, map a generous region ABOVE the stack (e.g., 1 MB) for musl's TLS/TCB, OR handle the page fault by dynamically mapping pages when the access is within a reasonable range above the stack (demand paging)
4. **Quick-and-dirty fix:** Map 256 extra pages (1 MB) above `ELF_STACK_TOP` — this should cover musl's TLS needs. The current 16 pages isn't enough.

**Files to investigate:**
- `kernel/src/mm/elf.rs:365` — `map_user_stack` function
- `kernel/src/mm/elf.rs:44` — `ELF_STACK_TOP` constant (currently `0x7FFF_FF00_0000`)
- musl source: `src/env/__init_tls.c`, `src/internal/pthread_impl.h`

### Blocker 2: Ion interactive mode panics (SIGABRT)

**Symptom:** Ion starts, prints warnings (ENOTTY, config/history errors), forks child for PROMPT command, child runs `/bin/PROMPT` which outputs "ion# " and exits(0), then ion panics with SIGABRT.

**Root cause:** Ion's `liner` line-editing library or ion's command pipeline reading fails in non-TTY mode. The panic isn't in `prompt.rs` (that was patched) — it's deeper in ion's runtime, likely in `readln.rs:9` (`fcntl(...).unwrap()`) or in liner's stdin handling.

**Recommended fix approach:**
1. This is fundamentally a **Phase 22 (Termios) issue** — ion's liner library expects a real TTY
2. For Phase 21, the pragmatic approach is: **sh0 is the interactive shell, ion is script-only**
3. To make `ion -c "echo hello"` work, the script-mode code path needs to not trigger liner at all. Currently the `-c` flag doesn't produce output — needs investigation of whether our argv passing to execve is correct for ion's argument parsing
4. Consider testing with `echo "echo hello" | /bin/ion` from sh0 to pipe a script into ion's stdin

**Files:**
- `userspace/init/src/main.rs:69-81` — shell spawn logic (currently sh0 primary, ion fallback)
- `/tmp/ion-build/ion/src/binary/readln.rs` — liner's non-TTY handling
- `/tmp/ion-build/ion/src/binary/prompt.rs` — patched to not panic on non-subprocess errors

## Remaining Tasks

### Must-fix for PR merge
- [ ] **Fix musl stack-top crash** (Blocker 1) — without this, no musl binary can run from the shell
- [ ] Verify `echo hello` works from sh0 prompt after fix
- [ ] Verify all Phase 20 acceptance criteria still pass with sh0

### Should-do for Phase 21 completeness
- [ ] Get `ion -c "echo hello"` working (script mode)
- [ ] Update task list with final status
- [ ] Write `docs/19-ion-shell.md` (P21-T050)
- [ ] Update CI assertions (P21-T023)

### Deferred to Phase 22 (Termios)
- Ion interactive mode (raw-mode line editing, history, tab completion)
- `isatty()` returning true for console fd
- Ion's liner library TTY handling
- All interactive acceptance tests (P21-T028, T030-T034, T038-T044)

## Architecture Notes

### Syscall entry/exit flow
```
User code → SYSCALL instruction → syscall_entry (asm)
  → saves RSP, R10, callee-saved regs to globals
  → switches to kernel stack
  → calls syscall_handler (Rust)
  → syscall_handler dispatches to sys_* functions
  → returns result in RAX
  → syscall_entry restores all regs from stack
  → SYSRETQ back to user code
```

### Fork child entry flow
```
sys_fork → cow_clone_user_pages → push_fork_ctx (saves user regs from globals)
  → task::spawn(fork_child_trampoline)
  → fork_child_trampoline:
    → pops ForkChildCtx from queue
    → sets CURRENT_PID, CR3, kernel stack, FS.base
    → calls fork_enter_userspace (global_asm)
    → global_asm loads callee-saved regs from FORK_ENTRY_CTX static
    → builds IRET frame with user CS/SS/RSP/RIP/RFLAGS
    → RAX=0 (child return value)
    → IRETQ to ring 3
```

### Context switch state
Per-process state that MUST be saved/restored on context switch:
- CR3 (page table root) — in `restore_caller_context`
- SYSCALL_USER_RSP — in `restore_caller_context`
- SYSCALL_STACK_TOP — in `restore_caller_context`
- TSS.RSP0 (kernel stack) — in `restore_caller_context`
- FS.base MSR (TLS pointer) — in `restore_caller_context` (Phase 21 fix)
- CURRENT_PID — in `restore_caller_context`

### Key file locations
| File | Purpose |
|---|---|
| `kernel/src/arch/x86_64/syscall.rs` | Syscall dispatcher + all sys_* implementations |
| `kernel/src/arch/x86_64/interrupts.rs` | Page fault, GPF handlers, CoW resolution, fault_kill_trampoline |
| `kernel/src/arch/x86_64/mod.rs` | `enter_userspace_fork` (global_asm), `ForkEntryCtx` static |
| `kernel/src/mm/elf.rs` | ELF loader, stack allocation, ABI stack setup |
| `kernel/src/mm/mod.rs` | `free_process_page_table`, page table management |
| `kernel/src/process/mod.rs` | Process struct (incl. `fs_base`), `fork_child_trampoline`, `ForkChildCtx` |
| `kernel/src/main.rs` | `serial_stdin_feeder_task`, `stdin_feeder_task`, `spawn_userspace_init` |
| `userspace/init/src/main.rs` | Init process: spawn sh0 primary, ion fallback |
| `xtask/src/main.rs` | `build_ion()`, `build_musl_bins()` including PROMPT and stdin-test |

### Testing commands
```bash
cargo xtask check          # clippy + fmt + 63 host tests
cargo xtask image           # build disk image with ion
cargo xtask run             # launch QEMU (headless, serial stdio)

# Pipe commands to test interactively:
(sleep 10; printf 'echo hello\n'; sleep 3) | timeout 20 \
  qemu-system-x86_64 -bios /usr/share/ovmf/OVMF.fd \
  -drive format=raw,file=target/x86_64-unknown-none/release/boot-uefi-m3os.img \
  -serial stdio -display none -m 256 \
  -device virtio-net-pci,netdev=net0 -netdev user,id=net0 -no-reboot
```

### Ion build location
Ion source is cloned to `target/ion-src/` on first build. The binary is cached at `kernel/initrd/ion.elf`. Delete it to force rebuild. Ion's `prompt.rs` was patched to remove a panic — the patch is in the cloned source only (not committed).
