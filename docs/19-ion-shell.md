# Phase 21: Ion Shell Integration

This document describes integrating the [Ion shell](https://github.com/redox-os/ion)
from Redox OS into m3OS. Ion is a full-featured shell written in Rust, originally
designed for Redox. This phase cross-compiles it for Linux/musl and embeds it in
the ramdisk alongside the Phase 20 minimal shell (renamed to `sh0`).

## Why Ion

Ion is the natural choice for a Rust OS because:

1. **Written in Rust.** Same language as the kernel; fewer toolchain dependencies.
2. **Designed for a microkernel OS.** Ion was built for Redox, which has a
   similar syscall surface to m3OS.
3. **Static musl linking works.** Ion compiles cleanly with
   `x86_64-unknown-linux-musl` producing a single static binary with no
   `PT_INTERP` (no dynamic linker required).
4. **Reasonable size.** ~3.1 MB stripped — large but manageable in the ramdisk.

## Build Pipeline

Ion is cross-compiled as a static, non-PIE (`ET_EXEC`) binary:

```bash
# In xtask build_ion():
RUSTFLAGS="-C relocation-model=static" \
  cargo build --release --target x86_64-unknown-linux-musl
strip target/x86_64-unknown-linux-musl/release/ion
```

Key build decisions:

- **`relocation-model=static`** produces an `ET_EXEC` binary (fixed addresses)
  instead of `ET_DYN` (PIE). This avoids the need for `R_X86_64_RELATIVE`
  relocations, simplifying ELF loading.
- **`x86_64-unknown-linux-musl`** links against musl libc statically. No
  `PT_INTERP` segment, no dynamic linker needed.
- **Stripped** to reduce size from ~6 MB to ~3.1 MB.
- **Cached** at `kernel/initrd/ion.elf` between builds. Delete to force rebuild.

Ion source is cloned to `target/ion-src/` on first build. The `prompt.rs` file
is patched to remove a panic in non-subprocess error handling.

## Shell Configuration

Init (`/sbin/init`) uses sh0 as the primary interactive shell:

```rust
// Phase 21: sh0 is the interactive shell.
// Ion requires Phase 22 (termios) for interactive mode.
execve("/bin/sh0", &argv, &envp);
// Fall back to ion if sh0 unavailable.
execve("/bin/ion", &argv, &envp);
```

Ion is available at `/bin/ion` for script execution once termios support
is added in Phase 22.

## Syscall Stubs Added

Ion's runtime (via musl libc and the `nix` crate) calls syscalls beyond
what Phase 20 required. Most are stubbed with harmless return values:

| Syscall | Number | Implementation |
|---|---|---|
| `access` | 21 | Check ramdisk/tmpfs/dev paths |
| `mprotect` | 10 | No-op (ELF loader sets up guard pages) |
| `clone` | 56 | Delegate `SIGCHLD` to `sys_fork` |
| `socketpair` | 53 | Pipe-based (for ion's signal self-pipe) |
| `fcntl` | 72 | `F_DUPFD`, `F_DUPFD_CLOEXEC`, `F_GETFD/SETFD`, `F_GETFL/SETFL` |
| `getuid/gid/euid/egid` | 102/104/107/108 | Return 0 (root) |
| `getpgrp` | 111 | Delegate to `getpgid(0)` |
| `set_robust_list` | 273 | No-op |
| `prlimit64` | 302 | Return `-ENOSYS` |
| `getrandom` | 318 | TSC-seeded xorshift64* PRNG |
| `ioctl TCGETS/TCSETS` | 16 | Return `-ENOTTY` |
| `pipe2` | 293 | Delegate to `sys_pipe` |
| `dup3` | 292 | Delegate to `sys_dup2` |
| `futex` | 202 | `FUTEX_WAIT` yields, `FUTEX_WAKE` wakes |
| `clock_gettime` | 228 | LAPIC tick-based approximation |
| `gettimeofday` | 96 | LAPIC tick-based approximation |
| `sendto` | 44 | Delegate to `sys_write` |
| `recvfrom` | 45 | Non-blocking read for pipe-based socketpair |
| `/dev/null` | — | Zero-length reads, discarded writes |

## Critical Kernel Bugs Fixed

### 1. Fork child caller-saved register corruption

**Symptom:** Every musl binary exec'd from sh0 crashed in `memcpy` (`rep movsq`)
with a bogus destination pointer.

**Root cause:** The Linux syscall ABI preserves all registers except RAX
(return value), RCX (return address), and R11 (RFLAGS). Our fork child
trampoline only restored callee-saved registers (RBX/RBP/R12-R15) via
IRETQ. Caller-saved registers (RDI/RSI/RDX/R8/R9/R10) contained garbage
kernel values, so the child's `memcpy` (copying stack frames after fork
return) wrote to an address computed from kernel-mode RDI.

**Fix:** Save all syscall-preserved registers at `syscall_entry`, pass them
through `ForkChildCtx`, and restore them in `fork_enter_userspace` before
IRETQ.

### 2. Stack demand paging

**Symptom:** musl's `__init_tls` writes above the initial RSP during
process startup. On Linux the kernel maps an 8 MB stack region, so these
writes always succeed. Our kernel only mapped a fixed number of pages.

**Fix:** Added demand paging in the page fault handler: writes to unmapped
pages within 8 MiB above `ELF_STACK_TOP` allocate a fresh zeroed frame
instead of killing the process.

### 3. Futex CR3 corruption

`sys_futex` called `yield_now()` without `restore_caller_context()`,
leaving another process's page table active after the yield.

### 4. Fork child callee-saved registers

`fork_child_trampoline` entered userspace with kernel garbage in
R12-R15/RBX/RBP. Fixed via a `global_asm` trampoline that loads saved
values from a static context struct.

### 5. FS.base (TLS) not saved on context switch

`arch_prctl(ARCH_SET_FS)` wrote the MSR but didn't save the value
per-process. Context switches corrupted TLS pointers. Fixed by saving
`fs_base` in the process struct and restoring it in
`restore_caller_context`.

### 6. fault_kill_trampoline IRET stack

Page fault and GPF handlers redirected to `fault_kill_trampoline` but
didn't set RSP/SS in the modified interrupt frame. The trampoline ran on
the user stack, causing a GPF.

## Cooked vs Raw Mode (Phase 22 Preview)

Ion's interactive mode requires raw terminal mode for line editing:

- **Cooked mode** (current): the kernel's stdin feeder delivers bytes
  one at a time. sh0 handles its own line editing. This works because
  sh0's `read_line` is a simple byte-by-byte loop.
- **Raw mode** (Phase 22): ion's `liner` library expects `tcgetattr`/
  `tcsetattr` to switch between raw and cooked modes. Without this,
  ion panics when trying to read lines interactively.

Phase 22 will implement termios (`TCGETS`/`TCSETS`) and make `isatty()`
return true for the console fd, enabling ion's interactive features.

## Ion Syntax Overview

Ion extends POSIX shell syntax with Rust-inspired features:

```ion
# Variables
let name = "world"
echo $name

# Arrays
let arr = [1 2 3]
for x in @arr { echo $x }

# String methods
echo $join(arr, ", ")

# Pipelines (standard)
ls | grep ".rs"

# Conditionals
if test -f /bin/ion { echo "ion exists" }
```

These features require ion's interactive mode (Phase 22) or script mode
(`ion -c 'cmd'`). Script mode currently exits with code 1 because ion's
startup calls `tcsetpgrp` which fails with `ENOTTY`, causing early exit
even before the `-c` command is processed.

## Key Files

| File | Purpose |
|---|---|
| `xtask/src/main.rs` | `build_ion()`: clone, cross-compile, strip, cache |
| `kernel/initrd/ion.elf` | Cached ion binary (delete to rebuild) |
| `userspace/init/src/main.rs` | Shell spawn: sh0 primary, ion fallback |
| `userspace/shell/src/main.rs` | sh0 (Phase 20 shell, renamed) |
| `kernel/src/arch/x86_64/syscall.rs` | 18 new syscall stubs |
| `kernel/src/arch/x86_64/mod.rs` | `ForkEntryCtx` with full register set |
| `kernel/src/arch/x86_64/interrupts.rs` | Stack demand paging |
