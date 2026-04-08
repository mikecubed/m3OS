# Phase 49 - Architectural Declaration

**Status:** Complete
**Source Ref:** phase-49
**Depends on:** Phase 6 (IPC Core) ✅, Phase 7 (Core Servers) ✅, Phase 8 (Storage and VFS) ✅, Phase 11 (Process Model) ✅, Phase 12 (POSIX Compat) ✅, Phase 20 (Userspace Init and Shell) ✅, Phase 48 (Security Foundation) ✅
**Builds on:** Converts the project's documented microkernel intent into an implementation contract that constrains future work instead of remaining only a design aspiration
**Primary Components:** docs/appendix/architecture-and-syscalls.md, docs/roadmap/README.md, kernel/src/arch/x86_64/syscall.rs, kernel/src/main.rs, kernel/src/fs, kernel/src/net

## Milestone Goal

m3OS gains an explicit, enforceable architecture contract for what belongs in ring 0, what is transitional, and what must move to userspace over time. The phase also turns the giant syscall surface into a structure that can be unwound safely in later phases.

## Why This Phase Exists

The project already uses the language of a microkernel, but the implementation still absorbs high-level policy into the kernel whenever it is convenient. That is survivable for early bring-up, but it becomes dangerous once the roadmap claims stronger service isolation, deeper hardening, or a future userspace display and driver story.

This phase exists to stop architectural drift. It makes the target boundary explicit before more extraction, transport, or GUI work lands on top of the wrong assumptions.

## Learning Goals

- Understand the difference between a documented architecture and an architecture that constrains implementation choices.
- Learn how to separate kernel mechanisms from compatibility shims and high-level policy.
- See why large syscall surfaces become architectural debt when ownership boundaries are unclear.
- Understand how an explicit keep/move/transition matrix reduces future migration risk.

## Feature Scope

### Kernel ownership contract

Write down what permanently belongs in ring 0, what is transitional for compatibility, and what is expected to become a userspace service. The result should be strong enough to guide code review and later roadmap work.

### Syscall surface decomposition

Break up the single syscall mega-surface into subsystem modules and classify which entry points are fundamental kernel mechanisms versus compatibility or policy shims. This reduces the cost of later moving logic outward.

### Mechanism-versus-policy audit

Label major subsystems according to their long-term role: kernel mechanism, transitional kernel policy, or future ring-3 service. The project should stop pretending these categories are already obvious when they are not.

### Userspace-first rule for new high-level policy

Adopt a documented rule that new policy-heavy behavior defaults to userspace unless there is a clear ring-0 requirement. This phase is where the roadmap stops digging the hole deeper.

## Critical and Non-Deferrable Items

| Item | Why it cannot be deferred in this phase |
|---|---|
| Explicit keep/move/transition matrix | Later phases need a stable target instead of restating the argument from scratch |
| Syscall decomposition plan reflected in code layout | The current syscall surface is already a maintenance and migration hazard |
| Userspace-first rule for new policy | Without it, later phases can be undercut by new ring-0 growth |

## Evaluation Gate

| Check | Required state before closing the phase | If missing, add it to this phase |
|---|---|---|
| Current-vs-target architecture split | The docs clearly distinguish shipped reality from target architecture | Add the missing mapping tables or diagrams instead of leaving the distinction implicit |
| Ownership inventory | Kernel subsystems and "server" tasks are classified by long-term ownership | Add the audit work for any unclassified subsystem |
| Refactor readiness | The syscall decomposition path is concrete enough to guide later implementation | Add the missing module plan or initial splits before closing the phase |
| Review discipline | Documentation and review checklists can reject new policy drift into ring 0 | Add the missing guidance to roadmap or contributor docs |

## Important Components and How They Work

### Architecture contract and roadmap alignment

The architecture contract is primarily documentation, but it must align with the actual source tree and roadmap. It should explain both the current state and the intended destination, without collapsing them into one flattering description.

### Syscall subsystem boundaries

The syscall layer is the clearest place where compatibility policy and kernel mechanisms currently blur together. Splitting it into smaller ownership domains is both a structural cleanup and a prerequisite for later extraction phases.

### Transitional-service inventory

The current kernel-resident "servers" and policy-heavy modules are the migration backlog. Making them explicit lets later phases choose targets by design rather than by folklore.

## How This Builds on Earlier Phases

- Builds on Phase 6 by treating IPC as a real architectural primitive rather than an isolated subsystem.
- Reinterprets the original Core Server and VFS phases through the lens of actual ownership boundaries.
- Builds on the process and POSIX-compat layers from Phases 11 and 12 by classifying which compatibility behavior should stay thin and which should move outward later.
- Depends on the repaired security floor from Phase 48 so later architecture claims are not undermined by basic trust failures.

## Implementation Outline

1. Inventory the current kernel, syscall, and service boundaries against the documented microkernel ideal.
2. Create a keep/move/transition matrix and link it from the architecture and roadmap docs.
3. Split `syscall.rs` into subsystem-oriented modules or an equivalent staged decomposition.
4. Tag major modules and service paths by long-term ownership.
5. Document the userspace-first rule for future policy-heavy work.
6. Update roadmap dependencies and contributor-facing docs to reflect the declared architecture.
7. Validate that the refactor still builds cleanly and that the docs match the code layout.

## Learning Documentation Requirement

- Create `docs/49-architectural-declaration.md` using the aligned learning-doc template in `docs/appendix/doc-templates.md`.
- Explain the current-versus-target architecture split, the mechanism/policy distinction, the syscall refactor plan, and why this phase matters before deeper serverization.
- Link the learning doc from `docs/README.md` when this phase lands.

## Related Documentation and Version Updates

- Update `docs/appendix/architecture-and-syscalls.md`, `docs/README.md`, and `docs/roadmap/README.md` so they reflect the new ownership contract.
- Update `docs/evaluation/current-state.md`, `docs/evaluation/microkernel-path.md`, and `docs/evaluation/roadmap/R02-architectural-declaration.md` to point at the official implementation phase.
- Update any contributor or review guidance that discusses what belongs in the kernel.
- When the phase lands, bump `kernel/Cargo.toml` and any release/version references to `0.49.0`.

## Acceptance Criteria

- A documented keep/move/transition matrix exists for the major kernel and service subsystems.
- The syscall surface is decomposed enough that subsystem ownership is obvious in the source tree.
- New high-level policy is explicitly documented as userspace-first unless justified otherwise.
- The main architecture docs clearly distinguish current implementation reality from target architecture.
- `cargo xtask check` still passes after the structural refactor.

## Companion Task List

- [Phase 49 Task List](./tasks/49-architectural-declaration-tasks.md)

## How Real OS Implementations Differ

- Monolithic kernels like Linux do not need this phase because their answer is already "the kernel owns it."
- Mature microkernels usually enforced the boundary earlier and more aggressively than m3OS has so far.
- m3OS is taking the slower but more teachable route: declare the target explicitly, then migrate toward it without pretending the gap does not exist.

## Deferred Until Later

- Full service extraction for storage, networking, and display
- Broad POSIX/libc boundary redesign
- Strong automated architecture-lint enforcement beyond documentation and review rules
