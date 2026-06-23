# Phase 95c — VFS / Block-I/O Performance: Task List

**Status:** Planned
**Source Ref:** phase-95c
**Depends on:** Phase 95b (demand-side loader) ✅ (A+B), Phase 95 ✅ (rust toolchain + `pkg install rust`), Phase 88 ✅ (vfs_server ext2 ownership), Phase 87 ✅ (VFS bulk-I/O + `/proc/blkstats`)
**Goal:** Make the ring-3 VFS read/write path fast enough that `pkg install rust` completes inside the install-step timeout and `rustc --version` cold-loads in reasonable time — flipping the Phase 95b `rustc-smoke` INSIDE-m3OS arm to PASS (`RUSTC_OK`). The supply-side complement to 95b's demand-side lazy loader; together they close the 95-series goal.

> **Planning task list.** 95c is `Planned`; it starts from the 95b diagnosis: the ring-3 VFS is ~100–200 KB/s effective (per-read-IPC-bound), so the 368 MB rust install is ~40 min of I/O — at/over the timeout — and cold loads crawl. The headline is Areas A–C (server bulk reads, the kernel read-only ext2-bypass, a real cache); Areas D–E are the installer + the gate; Area F is the milestone.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | `vfs_server` read-path throughput (bulk + server-side readahead) | 87 | Planned |
| B | Kernel read-only demand-paging fast path (in-kernel ext2-bypass) | 95b, 88 | Planned |
| C | Block / demand-page cache with eviction (kill the fill-and-hold thrash) | — | Planned |
| D | Installer read/verify/write coalescing | A | Planned |
| E | Throughput measurement + regression gate | A–D | Planned |
| F | Milestone: flip `rustc-smoke` to PASS (`RUSTC_OK`) | A–E, 95b | Planned |
| G | Docs, learning doc, kernel version bump | A–F | Planned |

---

## Track A — `vfs_server` read-path throughput

### A.1 — Server-side readahead + larger bulk reads

**File:** `userspace/vfs_server/src/main.rs`; `kernel-core/src/fs/vfs_protocol.rs` (`VFS_MAX_PREAD` / `MAX_BULK_LEN`)
**Symbol:** the `VFS_READ` handler; `VFS_MAX_PREAD`
**Why it matters:** A sequential reader (the installer, a cold-loading binary) currently pays one IPC round-trip per `VFS_MAX_PREAD` chunk. Raising the cap and prefetching the next run server-side cuts round-trips proportionally — the dominant cost at ~100–200 KB/s effective.

**Acceptance:**
- [ ] `VFS_READ` serves larger bulk replies and/or prefetches ahead; a sequential read of an N MB file issues materially fewer `VFS_READ` round-trips (asserted via a counter or `/proc/blkstats`).
- [ ] `vfs-bulkio-smoke` + `dynamic-hello-smoke` stay green (no correctness regression).

---

## Track B — Kernel read-only demand-paging fast path

### B.1 — Demand-fill `/usr` pages via the in-kernel ext2 engine (bypass per-page IPC)

**File:** `kernel/src/arch/x86_64/syscall/mod.rs` (`demand_read_file_page`); `kernel/src/process/mod.rs` (`FdBackend::VfsService { meta }` → `VfsFileMeta::inode`)
**Symbol:** `demand_read_file_page`; the `VfsService` arm
**Why it matters:** A `MAP_LAZY_FILE` demand fault on a `/usr` file currently does a synchronous `vfs_server` IPC per page. The `VfsService` fd carries the ext2 inode, so the page can be read straight from `EXT2_VOLUME` (virtio only, no IPC) — the biggest cold-load win. 95b prototyped this (reverted as unvalidated); 95c lands it with the coherence + concurrency argument.

**Acceptance:**
- [ ] A demand fault on a read-only `/usr` `MAP_LAZY_FILE` page reads via `EXT2_VOLUME` by inode, falling back to vfs IPC when ext2 is unavailable; cold-load `VFS_READ` round-trips drop to ~0.
- [ ] **Coherence proof:** a regression that writes a file via `vfs_server` then reads it back through the fast path returns the new bytes (the kernel ext2 read cache invalidation on vfs write is exercised); `MAP_PRIVATE` writes never reach the file.
- [ ] **Concurrency:** no deadlock/corruption with `vfs_server` concurrently using `EXT2_VOLUME` (the yielding-lock path); `dynamic-hello-smoke` + `smp-smoke` green.

---

## Track C — Block / demand-page cache with eviction

### C.1 — Replace the fill-and-hold ext2 block cache with an evicting cache

**File:** `kernel/src/fs/ext2.rs` (`block_cache`, `BLOCK_CACHE_MAX`)
**Symbol:** `read_block` / the block cache
**Why it matters:** The ext2 block cache is bounded fill-and-hold (no eviction), so a 162 MB sequential scan fills it in the first few MB and every later block — including the hot single/double-indirect blocks re-read per data block — misses to virtio. An LRU (or a dedicated indirect-block cache) keeps the hot blocks resident.

**Acceptance:**
- [ ] The block cache evicts (LRU or equivalent) so a large sequential read keeps hot indirect blocks cached; indirect-block re-reads to the device drop measurably (`/proc/blkstats`).
- [ ] Correctness preserved (the cache stays coherent with writes); host tests for the cache pass.

---

## Track D — Installer read/verify/write coalescing

### D.1 — Coalesce the `pkg install` read/verify/write loop

**File:** `userspace/pkg/`
**Symbol:** the install extract loop (read → SHA-verify → write)
**Why it matters:** `pkg install` reads, SHA-verifies (re-read), and writes hundreds of MB through small buffers. Larger buffers + hashing in-line with the read (skip the verify re-read where possible) + coalesced writes cut the install I/O substantially.

**Acceptance:**
- [ ] The installer uses the Area A bulk caps and avoids the redundant verify re-read where the write path can hash in-line; install `read_calls`/`write_calls` (`/proc/blkstats`) drop materially.
- [ ] `pkg-smoke` + `vfs-bulkio-smoke` stay green.

---

## Track E — Throughput measurement + regression gate

### E.1 — A VFS-throughput gate

**File:** `xtask/src/main.rs`
**Symbol:** a new `cmd_*_smoke` (or extend `vfs-bulkio-smoke`); `M3OS_*_REGRESSION`
**Why it matters:** Performance work needs a falsifiable guard. Assert install/cold-load throughput via `/proc/blkstats` deltas + wall-clock so a regression is caught.

**Acceptance:**
- [ ] A gate asserts a minimum VFS read throughput (request-count and/or wall-clock) on install + cold-load; opt-in env var; skip-with-reason when prerequisites absent.

---

## Track F — Milestone: `RUSTC_OK` unblock

### F.1 — Flip the Phase 95b `rustc-smoke` INSIDE-m3OS arm to PASS

**File:** `xtask/src/main.rs` (`cmd_rustc_smoke` — reused from 95/95b, unchanged)
**Symbol:** the `RUSTC_OK` sentinel
**Why it matters:** This is the 95-series goal. With A–E, `pkg install rust` completes inside the timeout and `rustc --version` cold-loads fast enough; `rustc /usr/src/hello.rs` → `RUSTC_OK`.

**Acceptance:**
- [ ] `pkg install rust` completes well within the install-step timeout (fresh, non-`FAST_ITER`).
- [ ] `rustc --version` → `--print sysroot` → `rustc hello.rs` → `RUSTC_OK`; `rustc-smoke` PASSES under `M3OS_RUST_REGRESSION=1`.

---

## Track G — Docs, learning doc, version bump

### G.1 — Docs + version

**Files:** `docs/95c-vfs-block-io-perf.md` (learning doc) + registration; `docs/roadmap/README.md` row + mermaid edge `P95b --> P95c`; `kernel/Cargo.toml` version; `AGENTS.md`
**Symbol:** the FS-perf capability bullet
**Why it matters:** Roadmap traceability + the version bump for the kernel mm/FS work; the AGENTS.md bullet records the on-device rust unblock once F lands.

**Acceptance:**
- [ ] Roadmap README carries a Phase 95c row + `P95b --> P95c` edge; design-doc Status flips on landing; learning doc registered.
- [ ] Kernel version bumped; `AGENTS.md` updated to mark on-device `rustc` code generation as landed once Track F passes.

## Documentation Notes

- **95c is the supply-side; 95b was the demand-side.** 95b read *less* (lazy demand paging); 95c reads *faster* (bulk + ext2-bypass + cache). Together they unblock rust.
- **Track B is the reverted 95b ext2-bypass, done right** — with the coherence + concurrency validation 95b skipped.
- **Confirm the immediate cause first** (Implementation Outline step 1): does the install time out (Area D/A) or complete-but-slow (Area B)? A fresh non-`FAST_ITER` `rustc-smoke` run answers it.
- Prefer exact symbols: `demand_read_file_page`, `VfsFileMeta::inode`, `EXT2_VOLUME`, `block_cache`, `BLOCK_CACHE_MAX`, `VFS_MAX_PREAD`, `/proc/blkstats`.
