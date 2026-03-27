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

## Resolved Blockers

### Blocker 1: RESOLVED — Fork child caller-saved register corruption

**Original symptom:** Every musl C binary exec'd from sh0 crashed in `memcpy`
(`rep movsq` at rip=0x403003) with the fault address tracking the exact boundary
of mapped pages above ELF_STACK_TOP.

**Actual root cause (corrected):** The original diagnosis of "musl TLS writing
above stack" was wrong. The crash was in sh0's own `memcpy`, called in the
**forked child before execve**. The Linux syscall ABI preserves all registers
except RAX/RCX/R11, but our fork child trampoline only restored callee-saved
registers (RBX/RBP/R12-R15). Caller-saved registers (RDI/RSI/RDX/R8/R9/R10)
contained garbage kernel values, so the forked child's `memcpy` used a bogus
destination pointer from kernel-mode RDI.

**Fix (commit b6af358):**
1. Save RDI/RSI/RDX/R8/R9/R10 at `syscall_entry` to new global statics
2. Pass them through `ForkChildCtx` and `ForkEntryCtx`
3. Restore them in `fork_enter_userspace` assembly before IRETQ
4. Also added demand paging for stack region (8 MiB above ELF_STACK_TOP)
   as defense-in-depth for musl's actual TLS allocation needs

### Blocker 2: Deferred to Phase 22 — Ion interactive/script mode

Ion's `-c` mode exits with code 1 because `set_unique_pid` calls `tcsetpgrp`
which returns ENOTTY, causing ion to abort before processing the `-c` command.
This is fundamentally a termios issue (Phase 22).

## Completed Tasks

### Must-fix for PR merge
- [x] **Fix musl stack-top crash** (Blocker 1) — fork child register corruption fixed
- [x] Verify `echo hello` works from sh0 prompt — confirmed
- [x] Verify all Phase 20 acceptance criteria still pass with sh0 — confirmed
  (echo, pwd, ls, cd, cat, output redirection, pipelines all work)

### Should-do for Phase 21 completeness
- [x] ~~Get `ion -c "echo hello"` working~~ — investigated; deferred to Phase 22
- [x] Update task list with final status
- [x] Write `docs/19-ion-shell.md` (P21-T050)
- [x] Update CI assertions (P21-T023)

### Deferred to Phase 22 (Termios)
- Ion interactive mode (raw-mode line editing, history, tab completion)
- Ion script mode (`ion -c`) — needs `tcsetpgrp` to succeed
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
