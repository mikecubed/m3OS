# Release Phase R02 — Architectural Declaration

**Status:** Proposed  
**Depends on:** none  
**Official roadmap phases covered:** [Phase 6](../../roadmap/06-ipc-core.md),
[Phase 7](../../roadmap/07-core-servers.md),
[Phase 8](../../roadmap/08-storage-and-vfs.md),
[Phase 11](../../roadmap/11-process-model.md),
[Phase 12](../../roadmap/12-posix-compat.md),
[Phase 20](../../roadmap/20-userspace-init-shell.md)  
**Primary evaluation docs:** [Path to a Proper Microkernel Design](../microkernel-path.md),
[Current State](../current-state.md),
[Rust OS Comparison](../rust-os-comparison.md)

## Why This Phase Exists

m3OS already speaks the language of a microkernel: capabilities, synchronous
IPC, notifications, ring-3 processes, and a conceptual split between kernel
mechanism and system services. But the implementation still keeps a large amount
of policy and subsystem ownership inside ring 0.

This phase exists to make the target architecture explicit in a way that changes
future decisions. It is less about moving code immediately and more about
stopping architectural drift before the later migrations begin.

```mermaid
flowchart TD
    C["Current state<br/>microkernel-inspired, broad ring 0"] --> D["Explicit target contract"]
    D --> E["Kernel responsibilities documented"]
    D --> F["Policy modules marked transitional"]
    D --> G["New features default to userspace"]
```

## Current vs. required vs. later

| Area | Current state | Required in this phase | Later extension |
|---|---|---|---|
| Kernel scope | Broad, with large policy concentration in `syscall.rs` and subsystem modules | Clear keep/move/transition declaration | Actual code movement out of ring 0 |
| Syscall surface | Large compatibility and policy surface in one place | Decompose by subsystem and classify primitives vs. shims | Potentially move more POSIX translation outward |
| Documentation | Strong intent, but reality and target are easy to blur | Separate current architecture from target architecture explicitly | Keep docs aligned as code migrates |
| Engineering rule | New code can still drift into ring 0 by convenience | Userspace-first rule for new high-level policy | Tighten review and architecture checks around the rule |

## Detailed workstreams

| Track | What changes | Why now |
|---|---|---|
| Kernel contract | Write down what permanently belongs in ring 0 and what is transitional | Later phases need a stable target |
| Syscall decomposition | Split `kernel/src/arch/x86_64/syscall.rs` into subsystem modules and identify compatibility shims | The current layout hides policy growth |
| Mechanism vs. policy audit | Tag major kernel modules as mechanism, transitional policy, or future userspace service | Prevents "temporary" kernel code from becoming permanent by accident |
| Compatibility stance | Decide which Linux/POSIX behaviors remain kernel facades during migration | This is the hardest boundary question in the whole roadmap |
| Documentation alignment | Make docs describe both the current system and the target system separately | Honest architecture docs help release honesty |

## How This Differs from Linux, Redox, and production systems

- **Linux** is monolithic on purpose. It does not need this phase because its
  architectural answer is already clear.
- **Redox** enforced its boundary earlier through scheme-based userspace
  services and driver daemons. m3OS has the right primitives, but not yet the
  same enforced boundary.
- **Production microkernels** are usually strict about what remains in the
  kernel. m3OS does not need instant purity, but it does need a rule that keeps
  the kernel from expanding while the migration is under way.

## What This Phase Teaches

This phase teaches the difference between an architecture that is **admired in
docs** and an architecture that **constrains implementation choices**. A system
does not become microkernel-like merely by using IPC. It becomes microkernel-like
when the kernel is small because it has no easy way to absorb more policy.

It also teaches that "make the design explicit" is real engineering work. Good
roadmaps do not just add features; they remove ambiguity.

## What This Phase Unlocks

Once the target is explicit, later phases can finish IPC, build a service
manager, and extract subsystems without arguing from scratch about what m3OS is
trying to be. That dramatically reduces the risk of halfway migrations.

## Acceptance Criteria

- There is a clear and maintained statement of ring-0 vs. ring-3 ownership
- `syscall.rs` is decomposed enough that kernel mechanisms and compatibility
  facades are visibly distinct
- New high-level subsystem work defaults to userspace unless there is a written
  exception
- Key docs explicitly distinguish **current implementation** from **target
  architecture**
- The project can explain, in one page, what "properly enforced microkernel"
  means for m3OS specifically

## Key Cross-Links

- [Path to a Proper Microkernel Design](../microkernel-path.md)
- [Current State](../current-state.md)
- [Architecture and Syscalls](../../appendix/architecture-and-syscalls.md)
- [Phase 6 — IPC Core](../../roadmap/06-ipc-core.md)
- [Phase 12 — POSIX Compatibility](../../roadmap/12-posix-compat.md)

## Open Questions

- How much POSIX/Linux ABI translation should remain in the kernel for 1.0?
- Is there a small set of policy-heavy kernel code that is intentionally kept in
  ring 0 even after the migration, or is the long-term direction stricter?
