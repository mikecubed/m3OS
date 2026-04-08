# Architectural Declaration

**Aligned Roadmap Phase:** Phase 49
**Status:** Complete
**Source Ref:** phase-49
**Depends on:** Phase 48 (Security Foundation) ✅
**Builds on:** Phase 6 (IPC Core), Phase 12 (POSIX Compat)
**Primary Components:** kernel/src/arch/x86_64/syscall/, docs/appendix/architecture-and-syscalls.md

## Overview

Phase 49 makes the kernel/userspace boundary explicit and enforceable. Before this
phase, m3OS described itself as a microkernel but lacked a formal contract for what
belongs in ring 0, what is transitional, and what must eventually move to userspace.
This phase closes that gap by decomposing the syscall surface, classifying every
kernel subsystem by long-term ownership, and adopting a userspace-first rule for
new policy-heavy code.

## What This Doc Covers

- Current versus target architecture: how the shipped implementation differs from the documented microkernel ideal
- Mechanism versus policy: why the distinction matters for a microkernel and how m3OS now classifies its subsystems
- Syscall decomposition: why the monolithic syscall handler was split into subsystem modules and what that enables
- Keep/move/transition matrix: a summary of which kernel subsystems stay, which migrate, and which are transitional
- Userspace-first rule: the adopted policy that new high-level behavior defaults to ring 3

## Core Implementation

### Current versus target architecture

m3OS has always aspired to a microkernel design where only memory management,
scheduling, IPC, and interrupt routing live in ring 0. The reality is that
filesystems, networking, TTY/PTY handling, signal dispatch, and a large syscall
surface all still execute in the kernel address space. Phase 49 makes this gap
visible and actionable rather than leaving it implicit.

The architecture-and-syscalls reference document now contains explicit "Current
Architecture" and "Target Architecture" sections with a gap analysis, so that
future phases can plan against the real starting point instead of the aspirational
endpoint.

### Mechanism versus policy

A microkernel distinguishes between mechanisms (low-level primitives the kernel
must provide) and policies (higher-level decisions about how those primitives are
used). For example, the page allocator is a mechanism; deciding which files to
cache in memory is policy.

m3OS now labels its kernel subsystems according to this distinction:

- **Kernel mechanism (keep):** memory management, scheduling, IPC/capabilities,
  interrupt routing, syscall gate, context switching, SMP boot
- **Transitional kernel policy (move later):** VFS and filesystems, network
  stack, TTY/PTY, signal handling, device drivers, process lifecycle policy
- **Compatibility shim (evaluate):** POSIX syscall translations that may stay
  thin or be pushed to a userspace compatibility layer

### Syscall decomposition

The single `syscall.rs` file was the largest maintenance and migration hazard in
the kernel. It mixed filesystem, networking, process, memory, authentication, and
device operations in one flat dispatch function. Phase 49 decomposed it into a
`syscall/` directory with a thin `mod.rs` dispatcher and eight subsystem modules:

| Module | Scope |
|---|---|
| `fs.rs` | Filesystem and path operations |
| `mm.rs` | Memory management (mmap, brk, mprotect) |
| `process.rs` | Process lifecycle (fork, exec, exit, wait) |
| `net.rs` | Network and socket operations |
| `signal.rs` | Signal delivery and handling |
| `io.rs` | I/O multiplexing (poll, select, epoll) |
| `time.rs` | Clock and timer operations |
| `misc.rs` | Miscellaneous (ioctl, uname, capabilities) |

This decomposition makes subsystem ownership obvious in the source tree and
reduces the cost of later extraction phases that move policy out of ring 0.

### Keep/move/transition matrix

The architecture-and-syscalls document now contains a formal classification of
every major kernel subsystem:

- **Keep permanently:** Frame allocator, page table manager, scheduler, IPC
  engine, capability table, interrupt/APIC routing, syscall gate, GDT/IDT/TSS,
  SMP AP boot
- **Move to userspace (future phases):** VFS and filesystem implementations,
  network protocol stack, block device drivers, NIC drivers, console/framebuffer
  server, keyboard server
- **Transition (evaluate per-phase):** TTY/PTY layer, signal dispatch, process
  group and session management, POSIX compatibility shims

Each kernel source module now carries an ownership header comment indicating its
classification.

### Userspace-first rule

Phase 49 adopted a documented rule: new policy-heavy behavior defaults to
userspace unless there is a clear ring-0 requirement. The architecture review
checklist in the architecture-and-syscalls document provides concrete questions
to evaluate whether new code belongs in the kernel. The CLAUDE.md and AGENTS.md
files reference this rule so it applies to all future development.

## Key Files

| File | Purpose |
|---|---|
| `kernel/src/arch/x86_64/syscall/mod.rs` | Thin syscall dispatcher routing to subsystem modules |
| `kernel/src/arch/x86_64/syscall/fs.rs` | Filesystem syscall implementations |
| `kernel/src/arch/x86_64/syscall/mm.rs` | Memory management syscall implementations |
| `kernel/src/arch/x86_64/syscall/process.rs` | Process lifecycle syscall implementations |
| `kernel/src/arch/x86_64/syscall/net.rs` | Network syscall implementations |
| `kernel/src/arch/x86_64/syscall/signal.rs` | Signal syscall implementations |
| `kernel/src/arch/x86_64/syscall/io.rs` | I/O multiplexing syscall implementations |
| `kernel/src/arch/x86_64/syscall/time.rs` | Time syscall implementations |
| `kernel/src/arch/x86_64/syscall/misc.rs` | Miscellaneous syscall implementations |
| `docs/appendix/architecture-and-syscalls.md` | Architecture contract with keep/move/transition matrix |

## How This Phase Differs From Later Architectural Work

- This phase declares the boundary and classifies subsystems. It does not move any code out of ring 0.
- Later Phase 50 completes IPC transport so ring-3 servers can handle bulk data.
- Later Phase 51 matures the service model with supervised restarts and health checks.
- Later Phase 52 performs the first actual service extractions, moving console and keyboard servers to ring 3.
- Later Phase 54 tackles deep serverization for storage, networking, and namespace policy.

## Related Roadmap Docs

- [Phase 49 roadmap doc](./roadmap/49-architectural-declaration.md)
- [Phase 49 task doc](./roadmap/tasks/49-architectural-declaration-tasks.md)

## Deferred or Later-Phase Topics

- Actual extraction of filesystem, network, or driver code to userspace servers
- IPC bulk-data transport required for efficient ring-3 servers
- Automated architecture-lint enforcement beyond documentation and review rules
- Broad POSIX/libc boundary redesign
