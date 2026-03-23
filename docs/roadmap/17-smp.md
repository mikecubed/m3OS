# Phase 17 - Symmetric Multiprocessing

## Milestone Goal

Boot all available CPU cores, give each its own Local APIC timer, and run the
scheduler across all cores simultaneously. Two processes should be able to execute
truly in parallel.

```mermaid
flowchart TD
    BSP["Bootstrap Processor\n(BSP, core 0)"] -->|"SIPI"| AP1["AP core 1"]
    BSP -->|"SIPI"| AP2["AP core 2"]
    BSP -->|"SIPI"| APn["AP core N"]

    subgraph PerCore["per-core state (×N)"]
        LAPIC["Local APIC\n(timer + IPI)"]
        GS["GS base\n(per-core data)"]
        TSS["TSS\n(kernel stack)"]
        RunQ["run queue"]
    end

    AP1 --> PerCore
    BSP --> PerCore

    RunQ -->|"schedule"| Task1["task A"]
    RunQ -->|"schedule"| Task2["task B"]
    Task1 <-->|"spinlock / IPC"| Task2
```

## Learning Goals

- Understand the AP startup sequence: why a 16-bit real-mode trampoline is needed.
- See what "per-core" means in terms of kernel data structures.
- Learn why spinlocks must be audited before enabling a second core.

## Feature Scope

- **AP startup**: write a 16-bit trampoline page, send INIT + SIPI IPIs to each AP,
  bring APs through real mode → protected mode → long mode → kernel entry
- **Per-core data**: each core gets its own GDT, TSS, kernel stack, `gs_base`
  pointer, and Local APIC mapping
- **Per-core run queues**: the scheduler maintains one queue per core; idle cores
  steal work from busy cores (simple work stealing)
- **Spinlock audit**: all global kernel data structures get `spin::Mutex` guards;
  any lock held across a timer interrupt is identified and fixed
- **IPI support**: the BSP can send inter-processor interrupts to wake idle APs or
  request TLB shootdowns
- **TLB shootdown**: when a page mapping is removed, the kernel sends a shootdown IPI
  to every core that might have cached the entry

## Implementation Outline

1. Determine AP count and APIC IDs from the MADT parsed in Phase 15.
2. Allocate a 4 KB trampoline page at a sub-1 MB physical address; write the 16-bit
   startup stub.
3. For each AP: send INIT IPI, wait 10 ms, send two SIPI IPIs with the trampoline
   physical page number.
4. In the trampoline: enable protected mode, set up a temporary GDT, enable long mode,
   jump to a Rust `ap_entry` function.
5. In `ap_entry`: initialize the per-core GDT, TSS, LAPIC, and kernel stack; signal
   the BSP that this AP is alive; enter the scheduler loop.
6. Split the global run queue into per-core queues; add a work-stealing path.
7. Implement `send_ipi` using LAPIC ICR registers.
8. Add TLB shootdown IPI handler.

## Acceptance Criteria

- All cores reported in the MADT appear in the boot log as online.
- Two CPU-bound tasks run simultaneously with no corruption of shared kernel state.
- A TLB shootdown triggered by `munmap` in one process does not leave stale mappings
  on another core.
- The system remains stable under a workload that context-switches rapidly across
  multiple cores.

## Companion Task List

- [Phase 17 Task List](./tasks/17-smp-tasks.md)

## Documentation Deliverables

- explain the AP startup sequence step by step: INIT IPI → SIPI → real mode → long mode
- document the per-core data layout and how `gs_base` is used to address it
- explain TLB shootdowns: why they are needed and what happens without them
- document the spinlock audit process and what makes a lock SMP-safe
- explain work stealing at a high level

## How Real OS Implementations Differ

Production kernels use NUMA-aware memory allocation (allocating from the NUMA node
local to each core), per-core page allocators to avoid cross-core lock contention,
and sophisticated load-balancing heuristics. Linux's scheduler (CFS) tracks CPU
utilization, task affinity, cache topology, and energy efficiency. This phase uses
the simplest correct implementation: equal-weight round-robin with naive work
stealing.

## Deferred Until Later

- NUMA-aware memory allocation
- CPU affinity (`sched_setaffinity`)
- real-time scheduling classes
- CPU frequency scaling (P-states)
- CPU hotplug
- per-core page allocator (SLUB/slab style)
