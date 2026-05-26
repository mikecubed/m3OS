# W^X Enforcement (Phase 75)

**Aligned Roadmap Phase:** Phase 75
**Status:** Complete
**Source Ref:** phase-75
**Supersedes Legacy Doc:** new

## Overview

Write-XOR-execute (W^X) is the rule that no userspace page may be
simultaneously writable and executable. Every modern OS since
OpenBSD 3.3 (2003) has enforced some form of W^X, because it
eliminates an entire class of memory-corruption exploits: even if an
attacker manages to write into the process's address space, they cannot
write into a page they can then jump to. The classic shellcode pattern
— allocate a buffer, fill it with machine code, return to it — stops
working at step three.

m3OS shipped most of W^X long before Phase 75. The Phase 11 ELF loader
already derived per-segment page-table flags from each `PT_LOAD`'s
`p_flags`: `PF_X` only → `R-X`, `PF_W` only → `RW-`, neither → `R--`.
The `brk` path and the anonymous `mmap` demand-fault path both applied
`NO_EXECUTE` to data pages. What was missing were the two paths an
attacker (or a mis-linked binary) could use to slip out the side:

1. **A malformed `PT_LOAD` with both `PF_W` and `PF_X` set.** The Phase
   11 `segment_flags()` helper would compute `WRITABLE`-and-not-
   `NO_EXECUTE` for such a segment, producing an executable code
   page that was also writable. `kernel/src/mm/elf.rs:map_load_segment`
   had no guard against this shape.
2. **`mprotect(PROT_WRITE | PROT_EXEC)` after the fact.** Even if every
   loaded segment was W^X-correct, a process could `mmap` an `RW`
   region, fill it with shellcode, then ask `mprotect` to flip the
   page to `RWX`. `sys_mprotect` in
   `kernel/src/arch/x86_64/syscall/mod.rs` had no W^X check.

Phase 75 closes both gaps and adds three smaller deliverables: the
legacy `setup_user_memory` dead-code path (`kernel/src/mm/user_space.rs`,
which had two `// W^X enforcement is deferred to Phase 6+` markers) is
gone; the JIT exception pattern is documented in
`docs/appendix/architecture-and-syscalls.md`; and a
`userspace/wx-violation` regression binary exercises both the negative
(`PROT_WRITE | PROT_EXEC` → `EINVAL`) and positive (`PROT_READ | PROT_EXEC`
JIT-pattern) cases inside the smoke-runner gate.

## What This Doc Covers

- The ELF-loader W^X guard at `map_load_segment` and how it surfaces
  to `execve(2)` as `ENOEXEC`.
- The `sys_mprotect` W^X guard and the exact placement that keeps the
  rejection atomic (no partial PTE state).
- The audit of `brk` / `mmap` to confirm `NO_EXECUTE` is applied to
  every data page.
- The JIT-pattern (allocate `RW-`, write code, flip to `R-X`) that
  Phase 80's Node.js port will use.
- The smoke-runner regression
  (`userspace/wx-violation/src/main.rs`) and how it locks the
  invariants in.

## Why W^X Matters (the 60-second version)

A typical memory-corruption exploit looks like this:

1. Get a memory bug — buffer overflow, use-after-free, out-of-bounds
   write.
2. Use the bug to write attacker-controlled bytes (shellcode) into
   the process.
3. Use a second corruption — return-address overwrite, vtable
   overwrite, exception-handler overwrite — to jump to those bytes.

W^X breaks step three. If every writable page is non-executable, the
hardware (via the `NX` bit in the page-table entry) raises a page
fault the moment the CPU tries to execute from a writable page.
Attackers have to escalate to ROP / JOP / data-only attacks, which
are dramatically harder to pull off.

W^X costs nothing at runtime. Normal code execution does not touch
writable pages, and normal data writes do not touch executable pages.
The `NX` bit was added to x86_64 in 2003; the hardware support has
been universal for ~20 years.

## Key Files

| File | Role |
|---|---|
| `kernel/src/mm/elf.rs` | `map_load_segment` rejects `PT_LOAD` with `PF_W | PF_X`; per-segment trace log proves applied PTE flags. |
| `kernel/src/mm/user_space.rs` | Legacy `setup_user_memory` (the W+X dead-code path) removed; Phase 75 commentary cites the audit. |
| `kernel/src/arch/x86_64/syscall/mod.rs` | `sys_mprotect` W^X guard returns `EINVAL` before address/VMA validation; `execve` maps `ElfError` to `ENOEXEC`. |
| `docs/appendix/architecture-and-syscalls.md` | JIT pattern (`mmap(RW)` → write → `mprotect(R-X)`) documented as the supported alternative. |
| `userspace/wx-violation/src/main.rs` | Negative + positive regression: `EINVAL` for `RWX`, success for `R-X`. |

## Core Concepts

### The Phase 11 ELF flag mapping (existing, unchanged in Phase 75)

`kernel/src/mm/elf.rs:segment_flags` derives `PageTableFlags` from
each `PT_LOAD`'s `p_flags`:

| `PF_X` | `PF_W` | Result |
|---|---|---|
| set | clear | `R-X` (no `WRITABLE`, no `NO_EXECUTE`) — the text segment |
| clear | set | `RW-` (`WRITABLE` + `NO_EXECUTE`) — the data segment |
| clear | clear | `R--` (`NO_EXECUTE`) — read-only data, rodata |
| **set** | **set** | **rejected by Phase 75 guard** |

The fourth row is the entire point of Phase 75 Track A: a mis-linked
or hostile binary that places code in a writable segment used to slip
through; now it is refused before any frame is allocated.

### The `sys_mprotect` guard

`sys_mprotect` previously parsed `prot` into `new_flags` and walked
the requested address range to update PTEs. Phase 75 inserts a guard
immediately after the `prot &= 0x7` mask:

```rust
if prot & PROT_WRITE != 0 && prot & PROT_EXEC != 0 {
    return NEG_EINVAL;
}
```

Crucially the guard runs **before** the page-alignment check, the
canonical-address check, the VMA walk, and any PTE mutation. A
rejected request therefore leaves no partial state — the address space
looks exactly as it did before the syscall.

### The JIT exception pattern

JIT engines (Node.js, V8, Cranelift, the Lua LuaJIT, etc.) need to
write machine code at runtime and then execute it. They cannot use
`PROT_WRITE | PROT_EXEC` — that combination is rejected. The
supported pattern is the two-step toggle:

```c
// 1. Reserve writable scratch.
void *code = mmap(NULL, size, PROT_READ | PROT_WRITE,
                  MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);

// 2. Emit machine code. Page is non-executable while we write.
memcpy(code, generated_bytes, size);

// 3. Flip to read-execute. `PROT_WRITE | PROT_EXEC` would fail with EINVAL.
mprotect(code, size, PROT_READ | PROT_EXEC);

// 4. Call into the freshly-generated code.
((void (*)())code)();
```

This is the standard pattern across modern OSes. m3OS Phase 80
(Node.js) will be the first in-tree consumer.

### The smoke-runner regression

`userspace/wx-violation/src/main.rs` runs four checks under the
in-tree `smoke-runner` (boot path: `cargo xtask smoke-test`):

1. `mmap(PROT_READ | PROT_WRITE)` for one page succeeds and the page
   is writable.
2. `mprotect(PROT_WRITE | PROT_EXEC)` on that page returns `EINVAL`
   (the new guard fired).
3. The page is still readable + writable after the rejection (no
   partial mutation).
4. `mprotect(PROT_READ | PROT_EXEC)` succeeds — the JIT pattern works.

The binary prints `WX_VIOLATION:smoke:ok` on success;
`smoke-runner` pattern-matches that marker and the boot transcript
captures it.

## How This Builds on Earlier Phases

- **Phase 11 (Process Model)** introduced the ELF loader with the
  per-segment `segment_flags()` helper. Phase 75 adds the
  malformed-segment guard at the loader's entry point. Phase 11's
  "Deferred Until Later" W^X entry is updated to point here.
- **Phase 36 (Memory Subsystem Expansion)** introduced `mprotect`.
  Phase 75 adds the validation guard. Phase 36's "Deferred Until
  Later" `mprotect` validation entry is updated to point here.
- **Phase 2 / 3 (Memory Management)** plumbed the `NO_EXECUTE`
  page-table bit through the kernel's `PageTableFlags`. Phase 75 is
  the first phase that uses it as the basis for a per-syscall
  invariant — earlier phases set the bit, Phase 75 enforces that
  every data mapping carries it.

## Related Roadmap Docs

- [`docs/roadmap/75-wx-enforcement.md`](roadmap/75-wx-enforcement.md)
- [`docs/roadmap/tasks/75-wx-enforcement-tasks.md`](roadmap/tasks/75-wx-enforcement-tasks.md)
- [`docs/roadmap/11-process-model.md`](roadmap/11-process-model.md) —
  origin of the ELF loader Phase 75 hardens.
- [`docs/roadmap/36-expanded-memory.md`](roadmap/36-expanded-memory.md)
  — origin of the `mprotect` syscall Phase 75 hardens.
- [`docs/appendix/architecture-and-syscalls.md`](appendix/architecture-and-syscalls.md)
  — JIT code-generation pattern reference.

## Known Follow-ups

- **ASLR.** Phase 75 does not randomize segment placement; the ELF
  loader still maps PIE binaries at a fixed offset above
  `USER_VADDR_MIN`. Address-space layout randomization is the next
  obvious memory-safety hardening item.
- **Shadow stacks (Intel CET).** Hardware-assisted return-address
  protection is out of scope for Phase 75; it requires both CPU
  feature gating and userspace ABI changes.
- **SMEP / SMAP audit.** The CR4 bits are set during boot but a full
  audit of every kernel-side userspace pointer dereference (for
  proper `stac`/`clac` bracketing) is deferred to a later security
  phase.

## Trade-offs and Alternatives

- **OpenBSD's strict mode.** OpenBSD refuses to run any binary that
  links with a W|X PT_LOAD segment. m3OS logs a warning and returns
  `ENOEXEC` instead of refusing the kernel to boot — functionally
  equivalent at runtime, slightly more diagnostic.
- **Linux's personality flags.** Linux allows
  `mprotect(PROT_WRITE | PROT_EXEC)` by default and only refuses it
  under explicit security policy (SELinux `execmem`, etc.). m3OS has
  no legacy compatibility burden, so the guard is unconditional.
- **gVisor / hypervisor-level enforcement.** Sandbox layers can
  enforce W^X at the hypervisor page table in addition to the guest
  OS level. m3OS's single page table is the only enforcement point;
  the cost is that a kernel exploit can in principle disable W^X,
  but the kernel TCB stays small.
