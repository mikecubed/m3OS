# Phase 95c — VFS / Block-I/O Performance (unblock the on-device rust build)

**Status:** Planned
**Source Ref:** phase-95c
**Depends on:** Phase 95b (the demand-side streaming/demand-paged file-backed loader — `MAP_LAZY_FILE` + the blocking vfs-IPC read from the page-fault handler) ✅ (Areas A+B landed), Phase 95 ✅ (the host rust toolchain + `pkg install rust`), Phase 88 ✅ (vfs_server as the single ext2 owner), Phase 87 ✅ (VFS bulk-I/O + `/proc/blkstats`)
**Builds on:** Phase 87 made the ext2 read/write path *coalesce contiguous runs* and added `/proc/blkstats`; Phase 95b made large-DSO loading *demand-paged* so only the touched working set is read. 95c is the **supply-side** complement: make the ring-3 VFS path itself fast enough that the heavy-toolchain install + cold-load story stops being I/O-bound — finishing the `RUSTC_OK` milestone Phase 95b is gated on.
**Primary Components:** `userspace/vfs_server/` (the ring-3 ext2 read/write service), `kernel/src/arch/x86_64/syscall/mod.rs` (`kernel_read_fd_at` / `vfs_service_read` / `demand_read_file_page`), `kernel/src/fs/ext2.rs` (the in-kernel ext2 engine + block cache), `kernel-core/src/fs/ext2.rs` (the coalescing reader), `userspace/pkg/` (the installer's read/verify/write loop), `kernel/src/blk/` (`/proc/blkstats`)

## Milestone Goal

`pkg install rust` (the ~368 MB toolchain) **completes well within** the install-step timeout, and `rustc --version` cold-loads in a reasonable time — so the **Phase 95b `rustc-smoke` INSIDE-m3OS arm reaches PASS** (`RUSTC_OK`). The same throughput win materially shortens the clang / node / python / claude installs and cold loads, letting their gates relax the 90-minute timeouts. This is the subphase that *finishes* the 95-series goal: a native rust toolchain that actually runs and generates code on m3OS.

## Why This Phase Exists

Phase 95b cleared the Phase 95 *eager-load* wall (the 162 MB read+copy) by making DSO loading demand-paged. But instrumenting the 95b runs surfaced the **deeper, shared bottleneck**: the ring-3 VFS read/write path runs at only **~100–200 KB/s effective** — dominated by **per-read IPC round-trips to `vfs_server`**, not raw device bandwidth.

- **The 368 MB rust install is ~40 minutes of pure I/O** at that rate — at or over the 50-minute install-step timeout — so it risks **timing out and leaving a partial/broken install** (the immediate, observable Phase 95b `rustc --version` blocker: the on-device rustc never loads a single DSO).
- **Every heavy toolchain pays this twice** — once on install (read + SHA-verify + write hundreds of MB) and again on each cold load — which is why clang / node / python / claude / rust all run behind `5400s` (90-minute) gate timeouts and `3000s` install steps. The slowness is structural, worked around rather than fixed.
- **Phase 95b is necessary but not sufficient.** It reduces *how many* bytes are read (skip the untouched ~most of a 162 MB DSO); 95c reduces the *per-byte cost* of the bytes that are read. The `RUSTC_OK` milestone needs both: read less **and** read faster.

## Learning Goals

- Why a **per-read IPC round-trip** to a ring-3 filesystem server caps throughput far below device bandwidth, and how **bulk reads + server-side readahead** amortise the round-trip.
- When a kernel can safely **bypass the ring-3 server** for *read-only* demand paging by reading the backing store directly (the file is `MAP_PRIVATE` and `/usr` package files are immutable at runtime), and the coherence rules that make it sound.
- The difference a **block/page cache** makes for write-then-verify and repeated-cold-load workloads, and why a *fill-and-hold, no-eviction* cache (the current ext2 block cache) thrashes on a 162 MB sequential scan.
- How to **measure** filesystem performance honestly (`/proc/blkstats` request-count deltas + wall-clock throughput gates) so an optimization is provably an optimization.

## Feature Scope

### Area A — `vfs_server` read-path throughput
Raise the bulk-read size and add **server-side readahead** so a sequential reader (an installer, a cold-loading binary) issues far fewer IPC round-trips. Push `VFS_MAX_PREAD` / `MAX_BULK_LEN` and coalesce the server's own ext2 reads.

### Area B — Kernel read-only demand-paging fast path (the big cold-load win)
A `/usr` file's `VfsService` fd carries its **ext2 inode** (`VfsFileMeta::inode`). For a `MAP_LAZY_FILE` demand fault — read-only, immutable-at-runtime file content — read the page(s) **directly from the in-kernel `EXT2_VOLUME` engine**, bypassing the per-page `vfs_server` IPC entirely (virtio block reads only). Phase 95b prototyped this and reverted it as unvalidated; 95c lands it **with** the coherence argument (MAP_PRIVATE → writes never reach the file; `/usr` is read-only at runtime; the kernel ext2 read cache is invalidated on every vfs write) and concurrency validation against `vfs_server`.

### Area C — Block / page cache for re-reads
The installer reads each file, SHA-verifies it (re-read), and writes it; cold loads re-read the same DSO across invocations. The current ext2 block cache is **bounded fill-and-hold (no eviction)** — it fills and stops, so a 162 MB sequential scan gets no benefit past the first few MB. Add proper eviction (LRU) or a dedicated **demand-page cache** keyed by `(inode, offset)` so hot indirect blocks and re-read data stay resident.

### Area D — Installer read/verify/write coalescing
`pkg install` reads + SHA-verifies + writes hundreds of MB through small buffers. Use the larger bulk caps from Area A, avoid the redundant verify re-read where the write path can hash in-line, and write in coalesced runs.

### Area E — Measurement + gate
A throughput regression gate (`/proc/blkstats` deltas + wall-clock): e.g. "install ≥ N MB at ≥ X KB/s" and "cold-load an M MB binary in < T". Reuse the Phase 87 `vfs-bulkio-smoke` plumbing.

### Area F — Milestone: the `RUSTC_OK` unblock
With A–E, `pkg install rust` completes inside the timeout and `rustc --version` → `--print sysroot` → `rustc hello.rs` → `RUSTC_OK` passes. The Phase 95b `rustc-smoke` INSIDE-m3OS arm flips to PASS — closing the 95-series goal.

## Implementation Outline

1. **Confirm the immediate cause** — a fresh (non-`FAST_ITER`) `rustc-smoke` run: does `pkg install: rust: OK` appear, and how long does the install take? (If it times out, Area D/A is the unblock; if it completes-but-slow, Area B is.)
2. **Area B first** (highest cold-load leverage, smallest blast radius): the read-only demand-paging ext2-bypass, validated for coherence + concurrency.
3. **Area A** (server bulk reads + readahead) and **Area C** (cache eviction) for the install + write-verify path.
4. **Area D** installer coalescing.
5. **Area E** the throughput gate; **Area F** flip `rustc-smoke` to PASS.

## Acceptance Criteria

- `pkg install rust` completes in **< the install-step timeout with comfortable margin** (target: a large multiple faster than today), proven by a fresh `rustc-smoke` run reaching `pkg install: rust: OK`.
- `rustc --version` cold-loads and prints `rustc 1.96.0`; `rustc /usr/src/hello.rs` → `RUSTC_OK`; **`rustc-smoke` PASSES end-to-end** under `M3OS_RUST_REGRESSION=1`.
- A `/proc/blkstats`-backed throughput gate asserts the VFS read path is materially faster (request-count and/or wall-clock), and `vfs-bulkio-smoke` + `dynamic-hello-smoke` stay green (no read-path regression).
- The read-only demand-paging fast path (Area B) is coherence-correct (a `dynamic-*`/`vfs` regression proves a freshly-written file still reads back correctly) and concurrency-safe with `vfs_server`.

## Companion Task List

- [Phase 95c Task List](./tasks/95c-vfs-block-io-perf-tasks.md)

## How Real OS Implementations Differ

- Real kernels demand-fault file-backed mappings from a **unified page cache** shared across `read`/`mmap` and all processes, with writeback and LRU reclaim; m3OS reads from its VFS per fault (no unified cache yet). 95c narrows the gap (Area B + C) without the full page-cache refactor.
- Real filesystems live **in the kernel** (or a tightly-coupled FUSE fast path); m3OS's ext2 write authority is a ring-3 server, so the IPC round-trip is the cost 95c attacks — Area B is the m3OS analog of an in-kernel read fast path.

## Deferred Until Later

- **A full unified page cache** (shared across mappings + processes, writeback, reclaim) — a broad mm refactor.
- **Write-path throughput beyond the installer** (general `MAP_SHARED` writeback performance).
- **Async / batched block I/O** (multi-request virtio queues, io_uring-style submission) — orthogonal device-layer work.
