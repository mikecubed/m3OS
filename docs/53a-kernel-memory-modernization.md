# Kernel Memory Modernization

**Aligned Roadmap Phase:** Phase 53a
**Status:** Complete
**Source Ref:** phase-53a
**Supersedes Legacy Doc:** (none -- new content)

## Overview

Phase 53a replaces the old "everything goes through one lock" kernel allocator
stack with an SMP-aware design. Where Phase 33 introduced a correct buddy
allocator, heap growth, and slab scaffolding, Phase 53a turns those pieces into
a coherent hot-path allocator: per-CPU page caches take most single-page traffic
off the global buddy lock, magazines cache small objects per CPU, a size-class
`GlobalAlloc` routes ordinary kernel allocations away from the linked-list heap,
cross-CPU frees stop taking the victim CPU's slab lock, and allocator-local
reclaim flushes hidden pages and objects before high-order allocation or OOM
failure.

## What This Doc Covers

- Per-CPU frame caching and the zero-before-exposure frame contract
- Host-testable buddy, slab, magazine, and cross-CPU free structures in
  `kernel-core`
- The fixed 13-class size table and dense page-metadata side table
- Kernel slab fast paths, cross-CPU free routing, and owner-CPU reclaim ordering
- Memory-accounting compatibility surfaces and the rollout fallback/validation
  strategy

## Core Implementation

### Buddy backend plus per-CPU frame hot path

Phase 53a keeps the buddy allocator as the cold physical-frame backend, but it
no longer sits directly on every hot allocation and free. The pure data
structure in `kernel-core/src/buddy.rs` now uses hierarchical summary bitmaps so
free-block discovery and removal do not depend on `Vec::position()` scans.

`kernel/src/mm/frame_allocator.rs` then layers a per-CPU cache on top:

- each CPU gets a 64-frame `PerCpuPageCache`
- cache miss refill and cache drain happen in 32-frame batches
- frees trigger a drain once the cache rises above the 48-frame high watermark
- hot-path cache mutation runs under `without_interrupts` plus a same-core
  non-reentrancy guard

This keeps the common single-page path CPU-local while preserving the buddy
allocator as the authoritative cold pool.

The same file also codifies the Phase 53a frame contract: `free_frame()` and
`free_contiguous()` no longer zero on free, but every user-visible frame path
goes through `allocate_frame_zeroed()` / `allocate_contiguous_zeroed()` or an
equivalent full-page overwrite. That preserves the stale-data hardening from the
52-series without paying the memset cost on every free.

### Embedded-freelist slab and magazine layer

The old slab scaffolding from Phase 33 is replaced by a real embedded-freelist
allocator in `kernel-core/src/slab.rs`. The free-list pointer lives inside each
free object, but it is encoded with a per-cache secret plus the slot address so
obvious corruption and cross-slot swaps decode to invalid pointers instead of
silently linking attacker-controlled chains.

Span metadata lives out of line:

- slab page base
- freelist head
- in-use count
- allocation bitmap for validation
- partial-list position

That keeps the full 4096-byte page available for the 4096-byte size class.

`kernel-core/src/magazine.rs` adds the per-CPU object cache layer: fixed
32-pointer magazines plus a shared depot of full and empty magazines. The kernel
integration in `kernel/src/mm/slab.rs` gives each CPU a `loaded` and `previous`
magazine for all 13 size classes, so balanced alloc/free traffic usually stays
local and never touches a global lock.

### Size-class `GlobalAlloc` and dense page metadata

`kernel/src/mm/heap.rs` replaces the linked-list-heap-first strategy with a
size-class allocator that activates after early boot. Small allocations route
through the slab magazine path. Larger or stronger-aligned layouts use
page-backed buddy frames mapped in the physmap region.

The exact Phase 53a size table lives in `kernel-core/src/size_class.rs`:

`32, 48, 64, 96, 128, 192, 256, 384, 512, 768, 1024, 2048, 4096`

This is a fixed contract, not a runtime-tuned table. The geometric 32..=1024
region stays below about 34% internal waste, while the 2048->4096 jump keeps
overall worst-case waste below 50% for requests in 32..=4096.

To free without guessing from `Layout`, `heap.rs` maintains a dense
page-number-keyed `PageMeta` side table:

- slab-backed pages record owning CPU plus size-class index
- large allocations record the buddy order
- bootstrap-heap addresses are still recognized by range and handled by the
  bootstrap allocator directly

That side table is what makes cross-CPU slab frees, 4096-byte objects, and the
large-allocation path coexist cleanly.

### Cross-CPU frees and allocator-local reclaim

If CPU B frees an object that belongs to CPU A's slab page, it must not mutate
CPU A's local magazine pair in place. `kernel-core/src/cross_cpu_free.rs`
solves that with a lock-free intrusive MPSC list:

- producers CAS-push freed objects allocation-free
- the owning CPU collects the whole chain with one `take_all()`
- only the first pointer-sized word of the freed object is reused while queued

`kernel/src/mm/slab.rs` wires that queue into the slab fast path and defines the
reclaim order that makes the new allocator reliable under memory pressure:

1. drain per-CPU page caches back to the buddy
2. flush cross-CPU free queues and local magazines back into slab metadata on
   the owning CPUs
3. drain depot-held magazines
4. reclaim now-empty slab pages directly to the buddy pool

That order matters. Empty slab pages are not actually reclaimable until hidden
objects in magazines and remote queues have been made visible to the backing
slab cache. `kernel/src/mm/frame_allocator.rs` and `kernel/src/mm/heap.rs` both
retry after this allocator-local reclaim so order-0 hoarding does not cause
false high-order allocation failure.

### Accounting, fallback, and validation

Phase 53a keeps the memory-accounting surfaces readable even though the allocator
now has more layers than a single global frame pool.

| Surface | Meaning |
|---|---|
| `MemFree` | Buddy-managed pages immediately free without draining per-CPU caches |
| `MemAvailable` | `MemFree` plus reclaimable per-CPU cached pages |
| `Allocated` | `MemTotal - MemAvailable` |
| `PerCpuCached` | Pages currently hoarded in per-CPU frame caches |

Those semantics are reflected in `kernel/src/fs/procfs.rs`,
`kernel/src/arch/x86_64/syscall/mod.rs`, and
`userspace/coreutils-rs/src/meminfo.rs`.

The phase also ships with a conservative rollout fallback. Building the kernel
with `--features legacy-bootstrap-allocator` leaves the size-class cutover
disabled so bring-up can stay on the bootstrap allocator while the new paths are
debugged.

Validation is layered the same way the allocator is layered:

- `cargo xtask check` for formatting, clippy, and host allocator tests
- `cargo xtask test` for QEMU integration coverage
- `cargo test -p kernel-core --target x86_64-unknown-linux-gnu` for host-side
  buddy/slab/magazine tests
- `RUSTFLAGS='--cfg loom' cargo test -p kernel-core --target x86_64-unknown-linux-gnu --test allocator_loom`
  for ordering-sensitive queue coverage

`kernel/src/main.rs` also carries a regression that proves allocator-local
reclaim can recover a contiguous high-order block after it was broken apart into
order-0 pages and hoarded in CPU-local caches.

## Key Files

| File | Purpose |
|---|---|
| `kernel-core/src/buddy.rs` | Host-testable buddy allocator with summary bitmaps for fast free-block tracking |
| `kernel/src/mm/frame_allocator.rs` | Per-CPU page cache, refcount ordering, zero-before-exposure helpers, high-order reclaim retry |
| `kernel-core/src/slab.rs` | Embedded-freelist slab cache with D.5 pointer hardening and out-of-line span metadata |
| `kernel-core/src/magazine.rs` | Fixed-capacity magazines and shared depot for per-CPU object caching |
| `kernel-core/src/size_class.rs` | Exact 13-class table and waste-bound tests |
| `kernel-core/src/cross_cpu_free.rs` | Lock-free intrusive MPSC queue for cross-CPU slab frees |
| `kernel/src/mm/slab.rs` | Kernel magazine fast paths, cross-CPU routing, reclaim ordering, empty-slab return |
| `kernel/src/mm/heap.rs` | Size-class `GlobalAlloc`, dense `PageMeta`, bootstrap fallback, allocator-local reclaim coordination |
| `kernel/src/fs/procfs.rs` | `/proc/meminfo` reporting for the new allocator layers |
| `userspace/coreutils-rs/src/meminfo.rs` | User-visible memory reporting consistent with the new accounting policy |
| `kernel/src/main.rs` | QEMU regression coverage for allocator-local reclaim and related phase tests |

## How This Phase Differs From Later Work

- Phase 53a modernizes the kernel allocator hot path; it does not add NUMA-aware
  memory policies or a full shrinker framework.
- CPU-local fast paths are guarded with interrupt masking and same-core
  non-reentrancy bits, not a fully lock-free preemptible allocator protocol.
- The size classes are a fixed 13-bucket contract, not a runtime-tuned or
  workload-adaptive table.
- The fallback from size-class allocation to bootstrap allocation is compile-time
  only (`legacy-bootstrap-allocator`), not a runtime switch.
- This phase improves allocator-local reclaim only. Broader memory-pressure
  policies for page cache, VFS caches, or userspace servers remain later work.

## Related Roadmap Docs

- [Phase 53a roadmap doc](./roadmap/53a-kernel-memory-modernization.md)
- [Phase 53a task doc](./roadmap/tasks/53a-kernel-memory-modernization-tasks.md)
- [Phase 52d learning doc](./52d-kernel-completion-and-roadmap-alignment.md)
- [Phase 52c learning doc](./52c-kernel-architecture-evolution.md)
- [Phase 52b learning doc](./52b-kernel-structural-hardening.md)
- [Phase 33 learning doc](./33-kernel-memory.md)

## Deferred or Later-Phase Topics

- NUMA-aware per-domain page and slab caches
- Shrinker-style reclaim hooks for non-allocator caches
- Constructor/destructor object caching for expensive kernel object types
- Richer GFP-style allocation flags and sleep/reclaim policy
- Full memory-debugging features such as red zones, poison fill, and KFENCE-like
  sampling
- Type-state wrappers for frame ownership and mapping states
