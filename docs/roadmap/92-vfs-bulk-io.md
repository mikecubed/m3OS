# Phase 92 - VFS Bulk-I/O Throughput & Fairness

**Status:** Planned
**Source Ref:** phase-92
**Depends on:** Phase 08 (Storage & VFS) ✅, Phase 55b (Ring-3 Driver Hosting / `RemoteBlockDevice`) ✅, Phase 85a (Package Infrastructure) ✅
**Builds on:** Extends the kernel VFS + ext2 read/write path and the kernel→ring-3 block-driver round-trip with request batching, readahead, write-back, and server fairness — without changing the on-disk format or the block protocol's isolation/safety model.
**Primary Components:** `kernel/src/fs/ext2.rs`, `kernel/src/blk/mod.rs`, `kernel/src/fs/vfs.rs`, `kernel/src/fs/protocol.rs`, `userspace/pkg`

## Milestone Goal

Large file I/O over m3OS's ring-3-backed VFS becomes fast enough, and fair enough, that a multi-megabyte operation — the canonical case being `pkg install python`, a 21 MiB package — completes in a fraction of the current time and no longer freezes interactive clients (the compositor and `term` in GUI mode). The learner sees how a microkernel pays for ring-3 driver isolation in per-request latency, and how batching, readahead/write-back, and scheduling fairness recover throughput without giving up that isolation.

## Why This Phase Exists

m3OS keeps block drivers in ring 3 (Phase 55b): every disk block crosses a ring0↔ring3 IPC boundary. The VFS/ext2 layer (Phase 08) issues I/O one filesystem block at a time — `Ext2Fs::read_file_data` loops calling `read_block`/`read_block_into_slice` per logical block, and each maps to a single `blk::read_sectors(.., sectors_per_block, ..)` round-trip to the ring-3 driver. For small files this is invisible; for a 21 MiB package it is **~5,376 serialized round-trips**, on top of the userspace reader's own per-chunk syscalls.

This was surfaced concretely by Phase 85c: `pkg install python` takes minutes, and because the kernel VFS and the single-queue ring-3 block driver service requests serially, one bulk transfer starves every other VFS client — in GUI mode the whole UI appears to freeze (`vfs_server: slow req … STAT_PATH/LIST_DIR elapsed_us=80000-200000` is the routine-case symptom of the same per-request cost).

Phase 85c already landed the cheap userspace-side wins as a baseline: `pkg` now reads the artifact in 256 KiB chunks into a stat-pre-sized buffer (cutting the read from ~5,400 syscalls to ~84 and removing the `mremap`-less realloc churn), and prints per-phase / per-large-file progress so the operation is visibly working rather than a silent hang. This phase fixes the **structural** causes that those userspace tweaks cannot touch: per-block round-trips, no readahead/write-back, and no fairness between a bulk job and interactive clients.

## Learning Goals

- The latency cost of ring-3 driver isolation, and why request *granularity* dominates bulk-I/O throughput in a microkernel.
- Coalescing physically-contiguous filesystem blocks into multi-block device requests.
- Readahead and write-back batching as throughput-recovery techniques layered on an indirect-block filesystem that has no on-disk extents.
- Server-side fairness: keeping a long bulk job from starving interactive clients via bounded work quanta / yield points / request interleaving.
- Bulk data movement via shared memory / page grants vs per-syscall copies — the microkernel's own "bulk data via page capability grants, never IPC payloads" rule, applied to file I/O.

## Feature Scope

### Area A — Batched block I/O (throughput)

`Ext2Fs::read_file_data` (and the write path) coalesce runs of physically-contiguous blocks into a single `blk::read_sectors(count = N·sectors_per_block)` / `write_sectors` call. The block layer already accepts a multi-sector `count`; the change is in the FS layer, which currently resolves and fetches one block at a time. A 21 MiB file collapses from ~5,376 per-block round-trips to a small multiple of its contiguous-run count.

### Area B — Readahead + write-back (throughput)

Sequential-access detection issues readahead for the next contiguous run while the current run is consumed, and large writes are buffered and flushed as big multi-block requests instead of per-block. This is added in the reader/writer, not the on-disk format (ext2 has no extents).

### Area C — VFS fairness (responsiveness)

A bulk transfer is split into bounded work quanta with yield/preemption points (and/or interleaved request servicing) so interactive VFS clients — the compositor, `term`, and app `exec` loads — are not blocked for the duration of a multi-MiB job. This is the direct fix for the GUI freeze.

### Area D — (Optional) bulk transfer + package compression

Move bulk file payloads through a shared-memory / page-grant region (reusing the Phase 74 grant / `sys_shm` machinery) instead of per-syscall copies, and/or compress the `.m3pkg` payload — which is currently **uncompressed** (`pkg_format::serialize` concatenates raw bytes + per-entry SHA-256) — to shrink the read volume. Either is an additive win on top of A–C.

## Important Components and How They Work

### `kernel/src/fs/ext2.rs` — `read_file_data` / `read_block`

The hot loop. `read_file_data` walks logical blocks, calling `resolve_block` (direct / single- / double-indirect) then `read_block_into_slice` once per block. Batching means: after resolving block *i*, extend the run while `resolve_block(i+1) == phys(i)+1`, then issue one `read_sectors` for the whole run. The double-indirect path (files > ~4 MiB, e.g. the 21 MiB package) is exactly where per-block cost hurts most.

### `kernel/src/blk/mod.rs` — `read_sectors` / `write_sectors`

The dispatch point to `RemoteBlockDevice` (ring-3 driver via IPC) or `virtio_blk`. Already multi-sector capable via `count`; this phase just feeds it larger `count`s. The per-call cost (the IPC round-trip + driver/device latency) is fixed-ish, so fewer, larger calls is the whole game.

### `kernel/src/fs/vfs.rs` — VFS request servicing

Where fairness lives: a single client's bulk read/write must not hold the VFS (or the block path) long enough to starve others. Bounded quanta + a yield point between runs, and/or interleaving other clients' requests, keeps interactive latency bounded.

### `userspace/pkg` (baseline, already landed in 85c)

`read_file_bytes` (256 KiB chunks, stat-pre-sized buffer) and `install_one` progress output. Listed so the phase's "before" baseline is reproducible and the gate can compare against it.

## How This Builds on Earlier Phases

- Extends Phase 08's ext2 `read_file_data`/`read_block` with contiguous-run batching, readahead, and write-back.
- Reuses Phase 55b's `RemoteBlockDevice` block protocol **unchanged** — same isolation model, just larger per-request transfers.
- Reuses Phase 74's capability page-grant / `sys_shm` for the optional bulk-transfer path (Area D).
- Motivated by Phase 85c (`pkg install python`) and a prerequisite for the heavy-I/O Phases 87 (Node.js) and 88 (Claude Code), which move far more data than Python.

## Implementation Outline

1. Add a block-request counter + timing probe (reads-per-file, wall-clock) to quantify the baseline and gate the improvement.
2. Coalesce contiguous physical blocks in `read_file_data` into multi-sector `read_sectors` calls; mirror for the write path.
3. Add sequential readahead and a write-back buffer for large writes.
4. Add VFS fairness: bounded work quanta + yield points (and/or request interleaving) so bulk jobs don't starve interactive clients.
5. (Optional, Area D) bulk page-grant transfer and/or `.m3pkg` payload compression.
6. Add a `vfs-bulkio-smoke` regression gate (throughput + fairness) and wire its opt-in env var into the pre-push table.

## Acceptance Criteria

- `pkg install python` (21 MiB) read+install wall-clock is reduced **≥4×** versus the Phase 85c baseline on the same host/QEMU config, measured by the gate.
- An ext2 read of the 21 MiB package issues **≤ 512** block-layer requests (down from ~5,376), proven by a request counter exposed via log or `procfs`.
- During `pkg install python` in GUI mode, an interactive probe (a QMP-injected keystroke echoed by `term`, or a compositor frame captured via the PPM screenshot harness) completes within **500 ms** — i.e. no multi-second UI freeze.
- The `python-smoke`, `pkg-smoke`, and `git-local-smoke` gates still PASS unchanged (no install/verify correctness regression).
- All existing `kernel-core` ext2 host tests still pass, plus new tests covering contiguous-run batching and the run boundary across the single→double-indirect transition.

## Companion Task List

- [Phase 92 Task List](./tasks/92-vfs-bulk-io-tasks.md)

## How Real OS Implementations Differ

- Linux uses a unified page cache with readahead heuristics, per-bdi writeback threads, and multi-page `bio` submission; block drivers run in-kernel, so there is no ring-3 round-trip per request.
- Production microkernels (seL4-based systems, QNX) amortize the driver-crossing cost with shared-memory rings and asynchronous batched submission/completion queues (NVMe-style SQ/CQ), not per-request synchronous IPC.
- Mature filesystems record extents (contiguous ranges) in the on-disk format; ext2's indirect-block scheme has none, so this phase detects contiguity at read time in the FS layer rather than changing the format.

## Deferred Until Later

- A full unified page cache + general writeback subsystem (this phase does targeted readahead + write-back, not a global page cache).
- Asynchronous queue-pair (SQ/CQ) block submission to the ring-3 driver — this phase batches *synchronous* requests; a full async ring is a later driver-side change.
- Network-FS / IPv6 bulk paths.
- Atomic `pwrite64` / write-path *correctness* — this phase is write-back *throughput*; positional-write correctness (`pwrite64` not mutating the shared fd offset) is a correctness concern tracked in **Phase 93**.
