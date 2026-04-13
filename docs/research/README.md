# Memory Allocator Research

Research for redesigning m3OS memory allocation into a modern, SMP-safe, high-performance subsystem.

## Documents

| Document | Description |
|----------|-------------|
| [Memory Allocator Survey](./memory-allocator-survey.md) | Cross-system survey of Linux SLUB/buddy/PCP, FreeBSD UMA, Redox, Theseus, jemalloc, mimalloc, tcmalloc. Feature matrices, locking strategies, comparison tables, and architecture recommendation for m3OS. |
| [m3OS Allocator Analysis](./m3os-allocator-analysis.md) | Deep analysis of the current m3OS memory subsystem. All four allocator layers documented, 27 cataloged deficiencies, SMP safety analysis, allocation path diagrams, and a 5-phase migration plan with size class recommendations. |
| [Allocator Theory](./allocator-theory.md) | Foundational theory covering slab allocators (Bonwick), magazine layer, buddy systems, size class design, lock-free techniques, per-CPU patterns, interrupt safety, cache coloring, memory pressure/reclamation, and debugging (red zones, poisoning, KFENCE-style sampling). |

## Key Findings

- The #1 bottleneck is the single global `spin::Mutex` on every allocation path — all cores serialize.
- Per-CPU caching (magazines/buckets) eliminates lock contention for ~95% of operations.
- All surveyed production systems converge on: lock-free per-CPU fast path + batched slow path + global buddy cold path.
- Recommended architecture: UMA-style magazines + SLUB-style embedded freelist + Linux PCP-style page cache + Theseus-style type-state frames.
- Expected 10-50x improvement on object allocation throughput with near-linear SMP scaling.

## Roadmap

This research informs [Phase 53a — Kernel Memory Modernization](../roadmap/53a-kernel-memory-modernization.md) ([tasks](../roadmap/tasks/53a-kernel-memory-modernization-tasks.md)).
