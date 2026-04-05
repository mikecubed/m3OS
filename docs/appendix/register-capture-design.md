# Register Capture Design: Current Approach and Future Direction

**Date:** 2026-04-05
**Phase:** 43a (Crash Diagnostics)
**Related:** [`kernel-race-debugging-strategy.md`](./kernel-race-debugging-strategy.md), [Phase 43a learning doc](../43a-crash-diagnostics.md)

## Problem

When the kernel panics or takes a fatal fault, developers need the full CPU
register state to diagnose the root cause. The Phase 43a `dump_crash_context()`
function captures 15 GPRs, RFLAGS, CR2, and CR3 via inline assembly inside a
Rust function. This works but has two known limitations:

1. **Registers reflect the capture point, not the fault point.** By the time
   `dump_crash_context()` runs, the compiler has already used caller-saved
   registers (RAX, RCX, RDX, RSI, RDI, R8-R11) for function call setup.
   Callee-saved registers (RBX, RBP, R12-R15) are more likely to be preserved
   but are not guaranteed to hold their original values.

2. **RBX/RBP use shared global statics.** LLVM reserves RBX and RBP, so they
   cannot appear as `lateout` operands in inline assembly. We store them to
   `static mut SNAP_RBX`/`SNAP_RBP` via RIP-relative `sym` addressing. These
   are shared across all cores, creating a race window if two cores panic
   simultaneously.

## How Linux Handles It

Linux **never captures registers in C code**. Instead:

1. **Hardware exception** pushes SS, RSP, RFLAGS, CS, RIP onto the kernel stack.
2. **Assembly entry stub** (`arch/x86/entry/entry_64.S`) immediately pushes
   every GPR with explicit `pushq` instructions via the `PUSH_AND_CLEAR_REGS`
   macro, constructing a `struct pt_regs` on the stack.
3. **C handler** receives `pt_regs*` as an argument. `show_regs()` prints from
   this struct — no inline asm needed.

RBX and RBP are captured by the assembly stub *before* the compiler touches
anything. The LLVM-reserved-register problem does not exist.

For SMP, Linux uses `atomic_t panic_cpu` so only the first core proceeds. For
crash dumps, it sends NMI IPIs to all other cores. Each core's NMI entry stub
saves its own `pt_regs` into per-CPU ELF note buffers, capturing exact register
state for every core at the moment the NMI arrived.

## How Redox OS Handles It

Redox uses the same fundamental approach:

1. **Naked function macros** (`interrupt_stack!`) generate assembly stubs.
2. `push_scratch!()` saves caller-saved registers; `push_preserved!()` saves
   RBX, RBP, R12-R15.
3. Rust exception handlers receive `&mut InterruptStack` with all registers.

Redox's panic handler only captures RBP (for stack unwinding) — full register
dumps are only available from hardware exception frames. No cross-CPU
coordination exists for crash dumps.

## Current m3OS Approach (Phase 43a)

```
panic!() / fault handler
    └── dump_crash_context()
            └── capture_registers()    ← inline asm in Rust function
                    ├── Single asm block with lateout for 13 GPRs
                    ├── sym SNAP_RBX/SNAP_RBP for LLVM-reserved regs
                    ├── Separate asm for RSP and RFLAGS
                    └── Rust reads CR2/CR3 via x86_64 crate
```

**What works:**
- 13 of 15 GPRs captured correctly in a single asm block
- CR2, CR3, RFLAGS always correct
- Per-core state (task info, run queues) always correct
- Deadlock-safe output via `try_lock()` and fallback serial

**Known limitations:**
- Caller-saved registers may not reflect fault-time values
- RBX/RBP race on concurrent panics (shared statics)
- No registers available from other cores

## Recommended Future Direction

### Phase 1: Assembly entry stub capture (recommended for next debugging phase)

Modify the IDT exception entry stubs to save all GPRs into a `RegisterFrame`
struct on the kernel stack before calling the Rust handler:

```rust
#[repr(C)]
pub struct RegisterFrame {
    // Pushed by assembly stub
    pub r15: u64, pub r14: u64, pub r13: u64, pub r12: u64,
    pub rbp: u64, pub rbx: u64,
    pub r11: u64, pub r10: u64, pub r9: u64, pub r8: u64,
    pub rsi: u64, pub rdi: u64, pub rdx: u64, pub rcx: u64,
    pub rax: u64,
    // Pushed by hardware
    pub error_code: u64, // or padding for no-error exceptions
    pub rip: u64, pub cs: u64, pub rflags: u64, pub rsp: u64, pub ss: u64,
}
```

The assembly stub would be:
```asm
pushq %rax
pushq %rcx
pushq %rdx
pushq %rdi
pushq %rsi
pushq %r8
pushq %r9
pushq %r10
pushq %r11
pushq %rbx
pushq %rbp
pushq %r12
pushq %r13
pushq %r14
pushq %r15
mov %rsp, %rdi          // RegisterFrame* as first argument
call rust_exception_handler
```

This eliminates:
- The LLVM-reserved-register problem (assembly runs before the compiler)
- Register-value drift (captured at exception entry, not later)
- The need for global statics

**Scope:** Requires modifying the IDT setup in `arch/x86_64/interrupts.rs` to
use naked function stubs or global asm entry points instead of the
`x86-interrupt` calling convention. The `x86_64` crate's `HandlerFunc` type
may need to be bypassed for enriched handlers.

**Complexity:** Medium. The `x86-interrupt` calling convention already saves
some registers, but the exact set is compiler-determined and not accessible to
Rust code. Switching to explicit assembly stubs gives full control.

### Phase 2: Panic-time register capture via naked wrapper

For `panic!()` calls (not hardware exceptions), there is no hardware-pushed
frame. A naked function wrapper around the panic entry point could capture
registers before the compiler touches them:

```rust
#[naked]
extern "C" fn panic_entry_wrapper(info: &PanicInfo) -> ! {
    unsafe {
        asm!(
            "push r15", "push r14", /* ... all GPRs ... */
            "mov rdi, rsp",     // RegisterFrame*
            "mov rsi, rdi",     // PanicInfo* (was in rdi)
            "call {handler}",
            handler = sym panic_with_regs,
            options(noreturn)
        );
    }
}
```

This is less valuable than Phase 1 (panics are usually logic errors where the
call site matters more than register values) but would give complete coverage.

### Phase 3: NMI-based cross-CPU register capture

Send NMI IPIs to all other cores on panic. Each core's NMI handler saves its
own `RegisterFrame` into a per-CPU buffer. The panicking core then dumps all
buffers. This requires:

- NMI handler implementation (not yet present in m3OS)
- Per-CPU register save buffers
- IPI infrastructure for NMI delivery (distinct from the existing reschedule IPI)

This is the highest-value improvement for SMP race debugging but also the
highest complexity.

## Decision Log

| Date | Decision | Rationale |
|---|---|---|
| 2026-04-05 | Inline asm capture in `dump_crash_context()` | Fastest path to useful crash output; no IDT changes required |
| 2026-04-05 | `sym` statics for RBX/RBP | Avoids `in(reg)` pointer clobber; accepted SMP race tradeoff |
| 2026-04-05 | Deferred assembly entry stubs | Requires IDT restructuring; Phase 43a scope is diagnostics, not IDT refactor |
| 2026-04-05 | Deferred NMI cross-CPU capture | Requires NMI handler (not yet implemented) |
