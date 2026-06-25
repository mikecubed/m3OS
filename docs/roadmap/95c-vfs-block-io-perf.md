# Phase 95c — VFS / Block-I/O Performance (unblock the on-device rust build)

**Status:** Partial — A (zero-copy + readahead) / C (LRU eviction) / E (throughput gate) landed; B (kernel page cache) + D (installer coalescing) deferred; F **rejected** (arch decision). Reframed (2026-06-24): not the `RUSTC_OK` blocker under KVM — see the [completion plan](../handoffs/2026-06-24-phase-95-completion-plan.md). Matches the [README row](./README.md).
**Source Ref:** phase-95c
**Depends on:** Phase 95b (the demand-side streaming/demand-paged file-backed loader — `MAP_LAZY_FILE` + the blocking vfs-IPC read from the page-fault handler) ✅ (Areas A+B landed), Phase 95 ✅ (the host rust toolchain + `pkg install rust`), Phase 88 ✅ (vfs_server as the single ext2 owner), Phase 87 ✅ (VFS bulk-I/O + `/proc/blkstats`)
**Builds on:** Phase 87 made the ext2 read/write path *coalesce contiguous runs* and added `/proc/blkstats`; Phase 95b made large-DSO loading *demand-paged* so only the touched working set is read. 95c is the **supply-side** complement: make the ring-3 VFS path itself fast enough that the heavy-toolchain install + cold-load story stops being I/O-bound — finishing the `RUSTC_OK` milestone Phase 95b is gated on.
**Primary Components:** `userspace/vfs_server/` (the ring-3 ext2 read/write service), `kernel/src/arch/x86_64/syscall/mod.rs` (`kernel_read_fd_at` / `vfs_service_read` / `demand_read_file_page`), `kernel/src/fs/ext2.rs` (the in-kernel ext2 engine + block cache), `kernel-core/src/fs/ext2.rs` (the coalescing reader), `userspace/pkg/` (the installer's read/verify/write loop), `kernel/src/blk/` (`/proc/blkstats`)

> **➜ Reframe (2026-06-24) — read with the [completion plan](../handoffs/2026-06-24-phase-95-completion-plan.md).**
> Two corrections to this doc's premise, learned after it was written:
> 1. **The FS is not the `RUSTC_OK` wall.** Measured under **KVM**, `pkg install rust` is
>    ~25 s and the cold-load ~9.6 s; the "~100–200 KB/s / ~40-min install" numbers below are
>    a **TCG artifact**. 95c is therefore the path to a **TCG-runnable** `rustc-smoke` gate
>    (and to faster repeat/shared loads via the page cache), **not** the milestone's
>    correctness blocker — that is the `rustc hello.rs` multithreaded-compile stall (a
>    scheduler/futex fix; completion-plan Step 1).
> 2. **Area F (in-kernel ext2 read fast path) is REJECTED** by an architecture decision
>    (owner, 2026-06-24): it violates the microkernel boundary and conflicts with the
>    ext2-engine-unification (vfs_server = sole post-boot reader). Fix perf in the ring-3
>    driver (Areas A/B); F is reconsidered only if A+B+D + a recorded measurement prove IPC
>    itself is the wall. The "Area F" text below stands as the (now last-resort) description.

## Milestone Goal

`pkg install rust` (the ~368 MB toolchain) **completes well within** the install-step timeout, and `rustc --version` cold-loads in a reasonable time — so the **Phase 95b `rustc-smoke` INSIDE-m3OS arm reaches PASS** (`RUSTC_OK`). The same throughput win materially shortens the clang / node / python / claude installs and cold loads, letting their gates relax the 90-minute timeouts. This is the subphase that *finishes* the 95-series goal: a native rust toolchain that actually runs and generates code on m3OS.

## Why This Phase Exists

Phase 95b cleared the Phase 95 *eager-load* wall (the 162 MB read+copy) by making DSO loading demand-paged. But instrumenting the 95b runs surfaced the **deeper, shared bottleneck**: the ring-3 VFS read/write path runs at only **~100–200 KB/s effective** — dominated by **per-read IPC round-trips to `vfs_server`**, not raw device bandwidth.

- **The 368 MB rust install is ~40 minutes of pure I/O** at that rate — at or over the 50-minute install-step timeout — so it risks **timing out and leaving a partial/broken install** (the immediate, observable Phase 95b `rustc --version` blocker: the on-device rustc never loads a single DSO).
- **Every heavy toolchain pays this twice** — once on install (read + SHA-verify + write hundreds of MB) and again on each cold load — which is why clang / node / python / claude / rust all run behind `5400s` (90-minute) gate timeouts and `3000s` install steps. The slowness is structural, worked around rather than fixed.
- **Phase 95b is necessary but not sufficient.** It reduces *how many* bytes are read (skip the untouched ~most of a 162 MB DSO); 95c reduces the *per-byte cost* of the bytes that are read. The `RUSTC_OK` milestone needs both: read less **and** read faster.

## Design stance: microkernel-idiomatic, not ext2-in-the-kernel

`vfs_server` stays the **sole ext2 authority** for reads and writes. The 95b prototype that
read `/usr` pages straight from the in-kernel `EXT2_VOLUME` (bypassing the server) is fast
but **not microkernel-pure** — it widens ring-0's filesystem role and creates a
two-readers-of-one-disk coherence hazard. Real userspace-FS microkernels get fast `mmap`
from three things, and 95c leads with all three:

1. **Zero-copy bulk transfer** — never copy file data through IPC *messages*; the server reads
   into a region the kernel already maps (m3OS's own rule: "bulk data = page grants, never IPC
   payloads"). The 95b demand-fill *violates* this (it copies the reply bulk into the frame).
2. **Server-side readahead** — one round-trip serves a large cluster (hundreds of pages), so the
   per-page IPC cost is amortised without leaving the server.
3. **A kernel-owned page cache** (the external-pager amortizer — Mach memory objects, Zircon
   VMOs + pager API, L4Re dataspaces) — physical-page management *must* be in the kernel, so
   the kernel owns the cache. A fault on a resident page (a re-fault, a second `rustc` run,
   another process mapping the same DSO) is served by the kernel VM with **zero** server IPC;
   the per-access cost collapses to per-*miss*.

The in-kernel ext2 read fast path is kept **only as a measurement-gated, documented, retireable
fallback** (Area F) for the case where m3OS's raw IPC cost is itself the wall — and even then
the right fix is faster IPC, not ext2 permanently in ring 0.

## Learning Goals

- Why a **per-read IPC round-trip** to a ring-3 filesystem server caps throughput far below
  device bandwidth, and the three idiomatic ways microkernels fix it without an in-kernel FS.
- **Zero-copy transfer** via shared/granted memory, and why copying bulk through IPC payloads
  (what 95b's demand-fill did) is the wrong shape under m3OS's own IPC discipline.
- The **external-pager / memory-object model** (Mach, Fuchsia/Zircon VMOs, L4Re): kernel owns
  pages + the cache, userspace owns files; faults hit the pager only on a *miss*.
- Why a *fill-and-hold, no-eviction* block cache thrashes on a 162 MB sequential scan, and what
  an evicting cache buys for the hot indirect-pointer blocks.
- How to **measure** honestly (`/proc/blkstats` deltas + per-IPC cost + wall-clock) so an
  optimization is provably one — and so the fallback decision is data-driven, not assumed.

## Feature Scope

### Area A — Zero-copy + readahead demand-fill (the idiomatic primary)
Two parts. **(A.1) Zero-copy fill:** the kernel grants `vfs_server` the destination frame and it
reads ext2 **directly into it** — no second copy (replacing 95b's `call_msg` + `take_bulk_data`
copy). **(A.2) Large readahead per IPC:** raise `VFS_MAX_PREAD` / `MAX_BULK_LEN` and the fault
cluster (e.g. 256 KiB–1 MiB) so one round-trip serves hundreds of pages. Keeps `vfs_server` the
sole FS reader; cuts both the copy and the round-trip count in-model.

### Area B — Kernel page cache for file-backed pages (external-pager amortizer)
The Mach/Zircon move: cache faulted read-only file pages in a kernel page cache keyed by
`(file identity, offset)`. On a fault **miss** the kernel asks the pager (`vfs_server`, via Area A);
on a **hit** — re-fault, second `rustc` run, two processes mapping the same `librustc_driver.so` —
it maps the resident page with **zero** server IPC. Bounded (LRU/cap); a write through
`vfs_server` invalidates the relevant cached pages. Approaches monolithic `mmap` performance
while keeping the FS in userspace.

### Area C — ext2-reader cache eviction
Both ext2 block caches (`vfs_server`'s `Ext2State`, the kernel's `EXT2_VOLUME`) are bounded
**fill-and-hold, no eviction**, so a 162 MB scan fills them in the first few MB and every later
block — including the hot single/double-indirect pointer blocks re-read per data block — misses
to the device. Add eviction (LRU) or a small dedicated indirect-block cache so the readahead
reads aren't dominated by indirect re-reads.

### Area D — Installer read/verify/write coalescing
`pkg install` reads + SHA-verifies + writes hundreds of MB through small buffers. Use the larger
bulk caps from Area A, avoid the redundant verify re-read where the write path can hash in-line,
and write in coalesced runs.

### Area E — Measurement + gate (decides the fallback)
Two jobs. **(1) Measure** the cost of one `VFS_READ` round-trip and the throughput achieved at a
256 KiB–1 MiB readahead cluster — if a big-readahead IPC already yields multiple MB/s, Area F is
**unnecessary**; if the IPC/context-switch cost *itself* dominates even at large clusters, Area F
is justified (and the lesson is "make IPC faster"). **(2) Guard** with a `/proc/blkstats`-backed
throughput gate (request-count + wall-clock), reusing the Phase 87 `vfs-bulkio-smoke` plumbing.

### Area F — (Conditional fallback) in-kernel ext2 read fast path
**⛔ REJECTED (arch decision, owner, 2026-06-24) — NOT implemented.** This microkernel-boundary
departure is off the table (it also conflicts with the ext2-engine-unification that made
`vfs_server` the sole reader). Reconsidered **only if** Areas A+B+D are landed AND a recorded
Area-E measurement proves the VFS path categorically cannot reach acceptable throughput — and
even then the fix is faster IPC, not ext2-in-ring-0. The original (now last-resort)
description follows. Landed **only if** Area E proves IPC cost itself is the wall. A `/usr` file's `VfsService` fd
carries its ext2 inode (`VfsFileMeta::inode`), so a read-only `MAP_LAZY_FILE` demand fault could
read straight from `EXT2_VOLUME` (no IPC). This is the 95b prototype, landed **with** its coherence
proof (`MAP_PRIVATE` → writes never reach the file; `/usr` read-only at runtime; kernel ext2 read
cache invalidated on every vfs write) + concurrency validation, an **explicit "model departure"**
docstring, and a **retirement condition** (retire once IPC is fast enough). A last resort, not the
design — `vfs_server` otherwise remains the sole reader.

### Area G — Milestone: the `RUSTC_OK` unblock
With A–F, `pkg install rust` completes inside the timeout and `rustc --version` → `--print sysroot`
→ `rustc hello.rs` → `RUSTC_OK` passes. The Phase 95b `rustc-smoke` INSIDE-m3OS arm flips to PASS —
closing the 95-series goal.

## Implementation Outline

1. **Confirm the immediate cause** — a fresh (non-`FAST_ITER`) `rustc-smoke` run: does
   `pkg install: rust: OK` appear, and how long does the install take? (Install timeout ⇒ the
   install path, Area D + A; complete-but-slow ⇒ cold load, Area A + B.)
2. **Area A (zero-copy + readahead)** — the idiomatic primary; eliminate the demand-fill copy and
   amortise the round-trip with a large cluster. Measure (Area E.1) the resulting throughput.
3. **Area B (kernel page cache)** — the external-pager amortizer for re-faults / shared maps /
   second run. **Area C (cache eviction)** for the indirect-block thrash.
4. **Area D** installer coalescing; **Area E** the gate.
5. **Area F** *only if* the Area E measurement shows IPC cost is the binding constraint at large
   clusters — else skip it (documented as intentionally not implemented).
6. **Area G** flip `rustc-smoke` to PASS.

## Acceptance Criteria

- `pkg install rust` completes in **< the install-step timeout with comfortable margin** (target: a large multiple faster than today), proven by a fresh `rustc-smoke` run reaching `pkg install: rust: OK`.
- `rustc --version` cold-loads and prints `rustc 1.96.0`; `rustc /usr/src/hello.rs` → `RUSTC_OK`; **`rustc-smoke` PASSES end-to-end** under `M3OS_RUST_REGRESSION=1`.
- The demand-fill transfer is **zero-copy** (page grant, no IPC-payload copy) and a sequential cold load issues `VFS_READ` round-trips in proportion to `N / cluster` (Area A), and a `/proc/blkstats`-backed throughput gate asserts the path is materially faster; `vfs-bulkio-smoke` + `dynamic-hello-smoke` stay green.
- The kernel page cache (Area B) serves a re-fault / second-process / second-run on the same `(file, offset)` with **zero** `VFS_READ` IPC (asserted by the round-trip counter), is bounded, and is coherence-correct (a write through `vfs_server` invalidates the cached pages).
- **If** the conditional fallback (Area F) is landed, it is coherence-correct (write-via-vfs then read-back-via-fast-path returns the new bytes) and concurrency-safe with `vfs_server`, and carries an explicit model-departure + retirement docstring — and the Area E measurement that justified it is recorded.

## Companion Task List

- [Phase 95c Task List](./tasks/95c-vfs-block-io-perf-tasks.md)

## How Real OS Implementations Differ

- This is exactly how **userspace-filesystem** systems get fast `mmap`: **Mach** memory objects + external pagers (→ macOS), **Fuchsia/Zircon** VMOs + the pager API, **L4Re** dataspaces — the kernel owns physical pages + the page cache, the userspace server is the pager consulted only on a miss, and bulk transfer is zero-copy via shared memory. 95c brings m3OS toward that model (Areas A + B) rather than toward an in-kernel FS.
- Monolithic kernels put the filesystem *in* the kernel, so there is no IPC at all; m3OS keeps ext2 in a ring-3 server (the microkernel choice) and pays for it with IPC — which 95c attacks with zero-copy + readahead + a kernel page cache, **not** by moving ext2 into ring 0. The in-kernel ext2 read fast path (Area F) is a last-resort fallback precisely because it would erode that boundary.

## Deferred Until Later

- **A full unified page cache** (shared across mappings + processes, writeback, reclaim) — a broad mm refactor.
- **Write-path throughput beyond the installer** (general `MAP_SHARED` writeback performance).
- **Async / batched block I/O** (multi-request virtio queues, io_uring-style submission) — orthogonal device-layer work.
