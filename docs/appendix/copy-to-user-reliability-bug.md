# copy_to_user Intermittent Reliability Bug

**Status:** Partially Fixed (Phase 52a)
**Severity:** High — affects any syscall that writes to a userspace buffer
**Discovered:** Phase 52 (First Service Extractions)
**Exposed by:** Moving stdin_feeder to userspace, where termios reads via
`copy_to_user` intermittently return zeros

## Summary

`copy_to_user` in `kernel/src/mm/user_mem.rs` sometimes writes correct data
to the physical frame (confirmed by kernel-side logging) but the userspace
process reads stale zeros from the same virtual address. The failure is
intermittent and non-deterministic — the same call succeeds on some
invocations and fails on others within the same process.

## Observed Symptoms

### Primary: stdin_feeder termios reads

The userspace `stdin_feeder` calls `ioctl(TCGETS)` (via `tcgetattr`) or the
custom `get_termios_flags` syscall on every keystroke. Both use
`copy_to_user` to write the termios struct to a stack buffer. Intermittently,
the buffer stays zeroed despite the kernel confirming it wrote the correct
data.

With zeroed termios flags:
- `c_lflag = 0` means ICANON and ECHO are off — the stdin_feeder switches
  to raw mode without echo
- `c_iflag = 0` means ICRNL is off — Enter produces `\r` instead of `\n`,
  which changes canonical mode line delivery
- Characters are randomly lost, duplicated, or delivered in the wrong mode

### Secondary: login termios poison cascade

The `login` binary calls `tcgetattr` to save terminal state before disabling
echo for password entry. If `copy_to_user` fails, the saved termios is all
zeros. When `restore_echo` writes these zeros back via `tcsetattr_flush`,
`TTY0.termios` is poisoned with `c_lflag = 0`, `c_iflag = 0`, etc. All
subsequent login attempts inherit the broken state.

### Diagnostic evidence

Kernel-side debug logging (`[INFO] [get_termios] c_lflag=0x801b ...`)
confirmed:
- The kernel reads the correct `c_lflag` value (0x801b) from `TTY0.termios`
- The `copy_to_user` call returns `Ok(())` (no error)
- The buffer pointer and length are valid (`buf_ptr=0x7ffffeffee90`, `buf_len=32`)
- But userspace reads `c_lflag = 0x0000`

A hex dump of the edit buffer on newline delivery showed inconsistent
corruption: characters duplicated (`72 72 6f 6f 74` = "rroot") or missing
(`6f 6f 74` = "oot" instead of "root"), confirming the stdin_feeder was
switching between canonical and raw mode mid-input due to intermittent
termios read failures.

## What copy_to_user Does

`kernel/src/mm/user_mem.rs:107` — `copy_to_user(dst_vaddr, src)`:

1. Validates address range (user-space, non-null, bounded)
2. For each page spanned by the buffer:
   a. Creates an `OffsetPageTable` mapper from the current CR3
   b. Checks `is_user_writable` — if not, tries demand fault or CoW resolution
   c. Translates the virtual page base to a physical address via `mapper.translate_addr`
   d. Computes `frame_virt = phys_off + physical_addr + page_offset`
   e. Writes via `core::ptr::copy_nonoverlapping(src, frame_virt as *mut u8, len)`
3. Returns `Ok(())`

The write goes through the kernel's physical-offset direct mapping, not
through the userspace virtual address. The assumption is that reading from
the userspace virtual address will see the same physical frame.

## What We Ruled Out

### Compiler optimization (ruled out)
- Added `compiler_fence(SeqCst)` after the syscall — no effect
- Added `core::ptr::read_volatile` on the struct fields — no effect
- The default `asm!` block (without `options(nomem)`) is treated by LLVM as
  a full memory barrier per the Rust reference
- Other syscalls using the same `asm!` wrappers (`read`, `write`) work
  correctly

### Copy-on-Write pages (ruled out)
- `execve` allocates a **fresh page table** (`new_process_page_table`) —
  the forked CoW pages are discarded entirely
- The stdin_feeder's stack pages are newly allocated, not shared with init
- `copy_to_user` handles CoW resolution explicitly (checks `is_cow_page`,
  calls `resolve_cow_fault`, re-translates after resolution)

### Struct layout mismatch (ruled out)
- `TermiosFlags` is `#[repr(C)]` with correct field order
- `size_of::<TermiosFlags>()` returns 32, matching the kernel's expected size
- Verified with a standalone Rust test

### Single-consumer buffer race (ruled out for scancodes)
- `read_scancode()` uses atomic head/tail with single-consumer semantics
- The kernel's kbd_server_task was removed; only the userspace kbd_server
  reads from `SCANCODE_BUF`
- The scheduler prevents double-dispatch (marks task Running under lock)

### Syscall ABI mismatch (ruled out)
- Syscall numbers match between userspace and kernel
- Register mapping (rdi=arg0, rsi=arg1) is correct
- Kernel debug log confirmed receiving the correct `buf_ptr` and `buf_len`

## Working Workarounds (in place)

### Register-return syscalls for stdin_feeder
`GET_TERMIOS_LFLAG` (0x100D), `GET_TERMIOS_IFLAG` (0x100E),
`GET_TERMIOS_OFLAG` (0x100F) return individual termios fields directly in
`rax`. No buffer, no `copy_to_user`. The `c_cc` array is cached once at
startup via `tcgetattr`.

### Login termios validation
`disable_echo()` in the login binary validates the saved termios: if
`c_lflag == 0` (never valid for a console TTY), it substitutes
`default_cooked` values to prevent the poison cascade.

## Investigation Tasks

### Task 1: Reproduce with a minimal test binary

Create a dedicated test binary (`userspace/copy-to-user-test/`) that:
- Allocates a buffer on the stack
- Calls a test syscall that writes a known pattern via `copy_to_user`
- Reads the buffer and verifies the pattern
- Loops thousands of times, counting failures
- Reports failure rate and any patterns (e.g., always fails on first call,
  fails more under load, etc.)

**Why:** Isolate the bug from the complexity of the stdin_feeder/kbd_server
IPC loop. Determine if the failure rate depends on process age, stack depth,
CPU load, or specific virtual address ranges.

### Task 2: Add kernel-side verification after copy_to_user

In `copy_to_user`, after the write, immediately read back from the same
`frame_virt` address and verify the bytes match `src`. If they don't match,
log a panic-level diagnostic with the virtual address, physical address,
expected bytes, and actual bytes.

**Why:** Confirm whether the write to the direct mapping succeeds. If the
readback matches but userspace reads zeros, the issue is in the TLB or page
table translation. If the readback fails, the direct mapping itself is wrong.

### Task 3: Add TLB flush after copy_to_user writes

After the `copy_nonoverlapping` in `copy_to_user`, add
`x86_64::instructions::tlb::flush(VirtAddr::new(vaddr))` for each page
written. This forces the TLB to reload the PTE on the next userspace access.

**Why:** If a stale TLB entry points to a different physical frame (e.g.,
from a previous mapping), the flush would fix it. If the flush fixes the
bug, the root cause is missing TLB invalidation somewhere in the page table
management code.

### Task 4: Audit TLB shootdowns on SMP page table modifications

Search the kernel for all places that modify user page table entries:
- `resolve_cow_fault` in `kernel/src/arch/x86_64/interrupts.rs`
- `try_demand_fault` / `try_demand_fault_writable` in `kernel/src/mm/user_mem.rs`
- `sys_linux_mmap`, `sys_linux_munmap` in syscall handler
- `sys_execve` page table setup
- `sys_fork` CoW marking

For each, verify:
- Is `invlpg` called for the modified virtual address?
- On SMP, is a TLB shootdown sent to other cores?
- Is there a window between PTE modification and TLB flush where another
  core could cache a stale entry?

**Why:** The most likely root cause on SMP. A single-core `invlpg` is
insufficient when the process might run on a different core after a context
switch. Even though CR3 load flushes the TLB, there may be windows where a
core executes with a stale TLB entry for a recently-modified PTE.

### Task 5: Test on single-core QEMU

Run the OS with `-smp 1` (single core) and test whether the `copy_to_user`
failure still occurs. If it disappears, the bug is definitively an SMP TLB
coherency issue.

**Why:** Eliminates the entire class of cross-core TLB bugs with one test.

### Task 6: Audit get_mapper() usage during copy_to_user

`get_mapper()` reads the current CR3 to create an `OffsetPageTable`. Verify:
- Is the CR3 always the calling process's page table during a syscall?
- Could a context switch between `get_mapper()` and `translate_addr()` cause
  the mapper to use a stale page table? (Should not happen since the mapper
  caches the CR3 value, and the process's CR3 is restored on reschedule.)
- Does `translate_addr` correctly handle all page sizes (4K, 2M, 1G)?

### Task 7: Check for ABA races in page frame reuse

If a physical frame is freed and immediately reallocated to another process,
and the TLB still maps a virtual address to the old frame, reads would see
the new process's data (or zeros if the frame was zeroed). Verify:
- Are freed frames zeroed before reuse?
- Is there a window between frame free and TLB flush where the old mapping
  is still live?

## Affected Code Paths

| File | Function | Role |
|---|---|---|
| `kernel/src/mm/user_mem.rs:107` | `copy_to_user` | The failing function |
| `kernel/src/mm/user_mem.rs:40` | `copy_from_user` | Same mechanism, read direction (appears to work) |
| `kernel/src/mm/paging.rs:51` | `get_mapper` | Creates page table mapper from CR3 |
| `kernel/src/arch/x86_64/interrupts.rs:80` | `resolve_cow_fault` | CoW resolution + TLB flush |
| `kernel/src/arch/x86_64/syscall/mod.rs:8147` | TCGETS handler | ioctl path that uses copy_to_user |
| `kernel/src/arch/x86_64/syscall/mod.rs:7699` | `sys_get_termios_flags` | Custom syscall path |
| `userspace/stdin_feeder/src/main.rs` | Main loop | Primary victim of the bug |
| `userspace/login/src/main.rs:436` | `disable_echo` | Secondary victim (poison cascade) |

## Phase 52a Fix: Stale Per-Core State on Blocking Paths

The IPC blocking path analysis (Phase 52a, Track A) confirmed that all six
IPC blocking syscalls (recv, call, reply_recv, notify_wait, recv_msg,
reply_recv_msg) and the FUTEX_WAIT path returned through `sysretq` with a
stale per-core `syscall_user_rsp` after a context switch. This causes the
wrong user stack to be restored.

**Fix:** Added `restore_caller_context` (restores CR3, PID, user RSP, kernel
stack top, FS.base) to all seven IPC blocking paths in `kernel/src/ipc/mod.rs`
dispatch and to the `sys_futex` FUTEX_WAIT path. This matches the existing
`sys_waitpid` pattern.

This fix addresses the per-core state corruption vector. The underlying
`copy_to_user` physical-vs-virtual address divergence remains a separate
open question for Phase 52b (task-owned return state).

## Related Commits

- `cd5bc5b` — Changed hardcoded 32 to `size_of::<TermiosFlags>()` (initially
  suspected as cause, turned out to be a red herring — size was already 32)
- `683c017` — Added kernel-side debug logging confirming kernel writes
  correct data
- `c316a3b` — Added `compiler_fence(SeqCst)` (no effect)
- `96b3240` — Added `read_volatile` (no effect)
- `3c172fd` — Added `GET_TERMIOS_LFLAG` register-return workaround
- Series of commits switching to `tcgetattr` then to register-return
  syscalls for all three flags
- Phase 52a: `restore_caller_context` added to IPC dispatch + futex WAIT
