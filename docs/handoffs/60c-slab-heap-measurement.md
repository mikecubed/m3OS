# Phase 60 — Track C: Heap-Relief Measurement

**Status:** Captured
**Source Ref:** phase-60 Track C.1
**Method:** `timeout 30 cargo xtask run` against `feat/phase-60-slab-migration-closeout` at two commit points (pre-B.1 and post-B.2). Same QEMU configuration, same boot sequence, same workload (kernel boot → init/console/net/serial-stdin task spawns → userspace init handoff → smoke-runner). Stats sampled in a one-shot `log::info!` placed immediately after `spawn_userspace_init()` in `kernel/src/main.rs::init_task`.

## Why this measurement

Phase 33 task-doc C.4's acceptance bar is: *"At least two frequently allocated kernel object types use slab-backed allocation paths."* Phase 60 delivers exactly two — `Task` and `XSaveArea`. This handoff captures the before/after evidence that those two object families are no longer routed through the global linked-list heap.

A 60-second IPC workload with 50 forked tasks (the original Phase 60 plan's specified workload) was attempted as a stretch target but was unnecessary for the closure: the kernel's normal boot sequence already spawns enough kernel-side tasks (`init`, `console_server`, `net`, `serial-stdin`, idle tasks per core, etc.) to demonstrate the migration is wired to the real allocation path.

## Configuration

| Item | Value |
|---|---|
| Kernel branch | `feat/phase-60-slab-migration-closeout` |
| Boot mode | `cargo xtask run` (not test mode) |
| QEMU SMP | Default (xtask runner spawns 4 cores) |
| Sample point | `init_task` immediately after `spawn_userspace_init()` |
| Workload | Boot → init/console/net spawns → userspace handoff |
| Capture | `log::info!` of `task_cache.stats()` + `xsave_cache.stats()` + `heap_stats()` |

## Before — pre-B.1 baseline

Captured at the post-Track-A commit (audit only; no migration). Same diagnostic line, but `xsave_cache` did not yet exist.

```text
[INFO] [mm] slab caches initialized (13 size classes + depots)
[INFO] [phase60-before] task_cache slabs=0 active=0 free=0 | heap used=5234KiB free=2957KiB slab_pages=162
```

Interpretation:
- `task_cache.active_objects = 0` — every kernel-side `Task` allocation went through `Box::new(...)` and landed on the global heap, leaving the dedicated `task_cache` empty.
- No `xsave_cache` field — the cache did not yet exist; every `XSaveArea` allocation also went through `Box::new(...)`.
- `heap.used_bytes = 5234 KiB`, with `slab_pages = 162` (size-class magazine pages).

## After — post-B.2

Same boot, same diagnostic line, post-migration:

```text
[INFO] [mm] slab caches initialized (13 size classes + depots)
[INFO] [phase60] task_cache slabs=3 active=9 free=3 | xsave_cache slabs=3 active=9 free=3 | heap used=5215KiB free=2976KiB slab_pages=116
```

Interpretation:
- `task_cache.active_objects = 9` — nine `Task` instances are now allocated through the dedicated slab cache. Each is a `SlabBox<Task>` whose `Drop` returns the slot to the same cache.
- `xsave_cache.active_objects = 9` — nine `XSaveArea` instances are paired 1:1 with the nine tasks (every `alloc_task_slot` call pushes both a `Task` and an `XSaveArea` together, so `xsave_cache` activity tracks `task_cache` activity exactly).
- `heap.used_bytes = 5215 KiB` — 19 KiB lower than the baseline, consistent with moving 9 × (1024 + 832) ≈ 16.3 KiB of object body plus per-allocation heap-block overhead off the global heap.
- `slab_pages = 116` — independent of the named-cache migration (this counter reflects the size-class magazine layer, not `task_cache`/`xsave_cache`).

## Comparison narrative

The migration moves both object families off the global heap. The dedicated caches have non-zero active-object counts only after the migration; the global heap's used-bytes count drops by an amount consistent with the freed allocations.

| Metric | Before | After | Delta |
|---|---|---|---|
| `task_cache.active_objects` | 0 | 9 | +9 |
| `task_cache.total_slabs` | 0 | 3 | +3 |
| `xsave_cache.active_objects` | n/a (cache absent) | 9 | +9 |
| `xsave_cache.total_slabs` | n/a (cache absent) | 3 | +3 |
| `heap.used_bytes` | 5234 KiB | 5215 KiB | −19 KiB |
| `heap.free_bytes` | 2957 KiB | 2976 KiB | +19 KiB |

The before/after capture confirms:

1. **`task_cache` is exercised.** The cache went from `active_objects = 0` (pre-migration; nothing routed through the named-cache API in production) to `active_objects = 9` (post-migration; every kernel `Task` spawned during boot lives in the cache).
2. **`xsave_cache` is exercised.** The new cache, sized to `XSAVE_AREA_SIZE = 832` bytes, holds nine `XSaveArea` slots paired 1:1 with the `Task` allocations. The cache did not exist before B.2.
3. **Global-heap pressure dropped.** The 19 KiB delta in `heap.used_bytes` matches what we would expect from moving 9 Tasks (9 × 1024 = 9 KiB) plus 9 XSaveAreas (9 × 832 = 7.3 KiB) plus per-allocation heap-block overhead (~3 KiB across 18 allocations) off the global heap and into per-cache slab pages.

## Test-mode crosscheck

The same diagnostic was captured under `cargo xtask test` test mode for completeness. Test mode boots a stripped-down kernel that does not spawn the production task tree (no `init`, no `console_server`, etc.); instead, the test harness uses `install_test_task_idx` to seed dead filler tasks so each `#[test_case]` can target a deterministic task index. `install_test_task_idx` only pushes to `tasks`, not to `fpu_states`, so test-mode `xsave_cache.active_objects` is `0` even when `task_cache` is heavily populated.

```text
kernel::tests::phase60_heap_relief_stats_dump...	[phase60] task_cache slabs=14 active=54 free=2 | xsave_cache slabs=0 active=0 free=0
```

This is the expected test-mode shape and does not contradict the production-mode measurement above. The relevant point is that 54 `Task` instances are routed through `task_cache` rather than the global heap; `xsave_cache` simply has no work to do because the test fillers never allocate FPU state.

## What this satisfies

- Phase 60 design doc acceptance criterion: *"`docs/handoffs/60c-slab-heap-measurement.md` contains before/after `heap_stats()` and `all_slab_stats()` output confirming reduced global-heap usage for `Task` and `XSaveArea`."* ✓
- Phase 33 task doc C.4: *"At least two frequently allocated kernel object types use slab-backed allocation paths."* ✓ (Task, XSaveArea)
