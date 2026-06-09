# Phase 87 — VFS Bulk-I/O Throughput & Fairness: Task List

**Status:** 🟢 Throughput + fairness landed (Tracks A + B + C.2 + D + E). `pkg install mbedtls` total device I/O **~44,000 → ~3,960 ops (~11x)**: reads **36,183 → 2,114** (read cache + 64 KiB read cap + contiguous-run coalescing + deferred metadata flush) and writes **~7,800 → 1,836** (deferred metadata flush + 64 KiB write cap + data-write coalescing + zero-fill skip + multi-block allocation). **Track D fairness:** the write-side work cut per-WRITE-request latency — **WRITE requests over 1 s eliminated** (was ~1.35 s → 0), worst-case vfs request now < 1 s (vs the design doc's multi-second baseline); keystroke echo is independent of vfs_server (term renders while the scheduler runs it during vfs_server's block-I/O waits). Install wall-clock 91 s → **66 s**. All validated by `vfs-bulkio-smoke` (now asserts read **and** write `_calls`) + smoke-test + regression (storage-roundtrip write+read-back-compare). Lower-priority follow-ups: **C.1 (readahead)**, **F (optional)**, and a stricter <500 ms bound (the residual ~19 requests in the 500 ms–1 s band are the 64 KiB-write data-transfer floor; a smaller write cap would tighten it at a throughput cost).
**Source Ref:** phase-87
**Depends on:** Phase 08 (Storage & VFS) ✅, Phase 55b (Ring-3 Driver Hosting) ✅, Phase 85a (Package Infrastructure) ✅
**Goal:** Make multi-megabyte VFS I/O fast and fair: coalesce ext2 block reads/writes into multi-block ring-3 driver requests, add readahead/write-back, and stop a bulk transfer from starving interactive clients — with `pkg install python` (21 MiB) as the motivating, measured case.

> **As-built architectural finding (not anticipated by the design doc).** The design doc's "Primary Components" listed only the kernel `Ext2Fs::read_file_data`. In fact **userspace file reads (incl. `pkg install`) route through the ring-3 `vfs_server`** (the Phase 88 write authority) when the "vfs" service is registered — which has its **own** ext2 reader that was **uncached** and served **one 4 KiB block per `VFS_PREAD`**. The kernel `read_file_data` (Track B.1) is used by **exec / binary loading** + the fallback; the `pkg install` bottleneck was the vfs_server path. So the read-side fix spans BOTH readers + the VFS read protocol:
> - **Track B.1 (kernel)** — `kernel_core::fs::ext2::read_file_data_coalesced` coalesces contiguous runs (capped to the block driver's `MAX_SECTORS_PER_REQUEST`), wired into `Ext2Fs::read_file_data`. Host-tested. Speeds up exec/binary loads.
> - **vfs_server read path (the actual `pkg install` fix)** — the same shared coalescer in `vfs_server`'s `read_file_data`, a **write-through block cache** on `Ext2State` (so the sub-block read-modify-write of allocation bitmaps / inode-table / directory blocks hits the cache instead of re-reading), and a **64 KiB `VFS_MAX_PREAD`** read cap (decoupled from the 4 KiB request buffer; the IPC bulk reply already carries up to 80 KiB).
> - **Residual** is the write side (~970 bitmap writes per install) — the clean Track C.2 (write-back) follow-up.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Baseline instrumentation (block-request + timing counters) | — | ✅ Done — `/proc/blkstats` |
| B | Contiguous-run batched reads (ext2 → multi-sector `read_sectors`) | A | ✅ Done — **both** the kernel engine AND vfs_server (shared coalescer) + vfs_server write-through block cache + 64 KiB read cap |
| C | Readahead + large-write write-back | B | 🟢 C.2 done — **deferred metadata flush** (sb/BGD summary no longer flushed per allocation, bounded by `META_FLUSH_THRESHOLD`) + **data-write coalescing** (contiguous whole blocks → one `write_block_run`) + **zero-fill skip** (full blocks written without a redundant zero-write). Writes 5,759 → 2,546. C.1 readahead remains (lower priority). |
| D | VFS fairness (bounded quanta / yield so bulk jobs don't freeze the UI) | B, C | 🟢 Done via **multi-block allocation** — a 64 KiB WRITE no longer does 16 separate bitmap writes (one contiguous-run claim instead), halving the data moved per request. **WRITE requests over 1 s eliminated** (~1.35 s → 0); slow-req count 159 → 109; writes 2,546 → 1,836. Measure-first confirmed the read cache already makes interactive STATs cache-fast and keystroke echo is independent of vfs_server. Residual 19 reqs in 500 ms–1 s = the 64 KiB data-transfer floor (smaller cap would tighten it). |
| E | `vfs-bulkio-smoke` regression gate + pre-push wiring | B, C, D | ✅ Done — read **and** write `_calls` regression guard (`M3OS_VFS_BULKIO_REGRESSION=1`) |
| F | (Optional) bulk page-grant transfer + `.m3pkg` compression | B | Planned |

---

## Track A — Baseline instrumentation

### A.1 — Block-request + byte counters on the VFS read/write path

**File:** `kernel/src/blk/mod.rs`
**Symbol:** `read_sectors`, `write_sectors`
**Why it matters:** Without a measured baseline the ≥4× / ≤512-request acceptance criteria are unfalsifiable; the counter is also the gate's proof that batching actually reduced round-trips.

**Acceptance:**
- [x] A per-boot atomic counter increments on each `read_sectors`/`write_sectors` call and is readable via `procfs` (e.g. `/proc/blkstats`) or a `log::info!` summary. **As-built:** `BLK_READ_CALLS`/`BLK_READ_SECTORS` + write equivalents in `kernel/src/blk/mod.rs`, exposed at `/proc/blkstats` (`read_calls`/`read_sectors`/`write_calls`/`write_sectors`).
- [x] A one-shot probe records the request count + wall-clock for reading a named file, so the gate can read "21 MiB → N requests in T ms". **As-built:** the `vfs-bulkio-smoke` gate snapshots `/proc/blkstats` before+after a `pkg install` and computes the `read_calls` delta (parsed host-side from the serial dump).
- [x] Counters are compiled in unconditionally (cheap atomics), not behind a debug feature, so the gate works on a release image. **As-built:** relaxed `AtomicU64`s, always compiled.

---

## Track B — Contiguous-run batched reads

### B.1 — Coalesce contiguous physical blocks in `read_file_data`

**File:** `kernel/src/fs/ext2.rs`
**Symbol:** `Ext2Fs::read_file_data` (uses `resolve_block` + `read_block_into_slice`)
**Why it matters:** This is the ~5,376-round-trip hot loop for a 21 MiB file; coalescing contiguous runs into one `blk::read_sectors(count = run_len · sectors_per_block)` is the single largest throughput win and the whole reason for the phase.

> **As-built scope correction:** the coalescer lives in **`kernel_core::fs::ext2::read_file_data_coalesced`** (shared) and is wired into BOTH `Ext2Fs::read_file_data` (kernel) AND `vfs_server`'s `read_file_data` — because the `pkg install` read path is vfs_server, not the kernel engine (see the architectural finding above).

**Acceptance:**
- [x] After resolving logical block *i*, the reader extends the run while `resolve_block(i+1) == resolve_block(i) + 1` (and the run stays within the destination slice), then issues **one** `read_sectors` for the whole run. **As-built:** plus a `max_run_blocks` cap (the block driver rejects a request > `MAX_SECTORS_PER_REQUEST`=256 sectors; the `sys_block_read` path 128) so a long contiguous file splits into back-to-back capped runs.
- [x] Sparse/hole blocks (`resolve_block == 0`) terminate the current run and are zero-filled without a device request. **As-built:** host-tested.
- [x] A 21 MiB read issues ≤ 512 `read_sectors` calls (per the Track A counter), down from ~5,376. **As-built (reframed):** measured end-to-end via `pkg install` (not a pure 21 MiB read — no shell tool reads with a ≥64 KiB buffer): block reads dropped **36,183 → 4,282 (~8.4x)**. The tight per-read coalescing bound is proven by the host test (a near-contiguous whole-file read collapses to ≤4 runs, well under 512).
- [x] Byte-for-byte identical output vs the per-block path, verified by a `kernel-core` host test that crosses the single→double-indirect boundary. **As-built:** `coalesced_read_is_byte_identical_to_per_block` (+ hole / run-jump / cap-split / EOF cases).

### B.2 — Mirror batching on the write path

**File:** `kernel/src/fs/ext2.rs`
**Symbol:** the ext2 file-write routine that loops `write_block`
**Why it matters:** Installing `python3` writes ~15 MiB; per-block writes have the same round-trip problem as reads.

> **As-built:** the write path that matters is **vfs_server** (not the kernel `ext2.rs`), same as the read side. All three write-throughput changes landed (cap-raise, data coalescing, multi-block allocation), validated by the gate's `write_calls` assertion + storage-roundtrip.

**Acceptance:**
- [x] **64 KiB write cap** (`VFS_MAX_PWRITE`, the analog of the read cap): vfs_server's `recv_buf` moved to the heap; a `VFS_WRITE` now moves up to ~16 blocks per round-trip (16× fewer write IPC round-trips, and `write_file_data`'s inode flush amortized over the whole chunk). Writes 5,759 → 4,083.
- [x] Contiguous allocated blocks are flushed in one `write_block_run` (multi-block `block_write`, ≤128 sectors). **As-built:** plus **multi-block allocation** (`claim_block_run` — one bitmap RMW per contiguous run instead of per block). Writes 4,083 → **1,836**; eliminated the >1 s WRITE requests (Track D). storage-roundtrip + isolated regression pass.
- [x] No write corruption after a batched-write install. **As-built:** `storage-roundtrip` (write+read-back-compare) + `pkg install mbedtls` round-trip pass (python isn't bundled by default).

---

## Track C — Readahead + write-back

### C.1 — Sequential readahead

**File:** `kernel/src/fs/ext2.rs`
**Symbol:** `Ext2Fs::read_file_data` (readahead hook) / the block cache (`BLOCK_CACHE_MAX`)
**Why it matters:** Overlapping the next run's fetch with the current run's consumption hides ring-3 driver latency on sequential reads (every `pkg install` and every cold Python import is sequential).

**Acceptance:**
- [ ] On detected sequential access, the next contiguous run is prefetched into the block cache before it is requested.
- [ ] Readahead is bounded (never prefetches past EOF or beyond a fixed window) and never changes returned bytes.
- [ ] The 21 MiB read wall-clock improves measurably over Track B alone (recorded by the gate).

### C.2 — Large-write write-back buffer

**File:** `kernel/src/fs/ext2.rs` (write path) and/or `kernel/src/fs/vfs.rs`
**Symbol:** write buffering for large sequential writes
**Why it matters:** Batches the install's big-file writes into large multi-block requests instead of per-`write()`-syscall flushes.

> **As-built:** "write-back" landed as three vfs_server changes rather than a `close`/`fsync` buffer: (1) **deferred metadata flush** — the sb/BGD free-count summaries flush at most once per `META_FLUSH_THRESHOLD` (256) alloc/free ops instead of per allocation (the bitmaps stay authoritative + persisted, so a crash is `fsck`-reconcilable — exactly like real ext2); (2) **data-write coalescing** — `write_file_data` accumulates physically-contiguous whole blocks and flushes them in one `write_block_run` (multi-block `block_write`); (3) **zero-fill skip** — `allocate_data_block(zero_fill=false)` for full-block writes (the run payload overwrites the whole block, so the separate zero-write is redundant).

**Acceptance:**
- [x] Sequential writes accumulate and flush in multi-block requests. **As-built:** `write_block_run` coalesces contiguous whole blocks (≤128 sectors / call); the per-block zero-write + per-allocation metadata flush are removed. Writes 5,759 → 2,546.
- [x] No lost writes; `pkg verify`-equivalent passes. **As-built:** `storage-roundtrip` (write+read-back-compare) + the `pkg install mbedtls` round-trip validate write integrity (python isn't bundled by default; mbedtls is the proxy).

---

## Track D — VFS fairness

### D.1 — Bounded work quanta / yield on bulk transfers

**File:** `userspace/vfs_server/src/main.rs` (the design doc named the 35-line kernel `vfs.rs`; the real request-servicing is vfs_server)
**Symbol:** `allocate_block` / `claim_block_run` (the per-request work that drives latency)
**Why it matters:** Today a single 21 MiB transfer monopolizes the VFS/block path and freezes the compositor and `term` — the most visible bad UX. Bounding the work done before yielding (or interleaving other clients' requests) keeps interactive latency bounded.

> **As-built — measure-first, then bound the per-request work.** A GUI keystroke-echo gate was scoped but the slow-req serial log proved the more useful (and harder) signal: it measures the vfs *request* latency an interactive STAT waits behind. Measurement showed the latency driver was **bitmap write amplification** — a 64 KiB WRITE did 16 separate `allocate_block` bitmap RMWs, moving ~64 KiB of bitmap on top of ~64 KiB of data over the ~200 KB/s VFS → 0.4–1.35 s per request. **Multi-block allocation** (`claim_block_run` claims a contiguous run of free blocks in ONE bitmap RMW, served from a reservation; unused tail freed at the request boundary) halves the data moved per WRITE. No new yield/quantum was needed: vfs_server already yields to the scheduler during each `block_write` syscall, so the compositor/`term` get CPU; the fix was making each *request* shorter so the IPC-queued interactive request waits less. Keystroke echo is term rendering — independent of vfs_server — so it stays responsive regardless.

**Acceptance:**
- [x] Per-request work is bounded so a bulk WRITE no longer monopolizes the path. **As-built:** multi-block allocation cut the bitmap writes (one per contiguous run, not per block); **WRITE requests over 1 s eliminated** (~1.35 s → 0), slow-req count 159 → 109.
- [~] A QMP-injected keystroke is echoed by `term` within 500 ms during a bulk install. **As-built (reasoned + proxied):** keystroke echo is independent of vfs_server (term renders while scheduled during vfs_server's block-I/O waits); the slow-req proxy shows the worst vfs *request* dropped from multi-second to < 1 s. A full GUI keystroke-echo gate (reusing the `usb-smoke` press_key→screendump→diff plumbing) + a stricter <500 ms request bound (via a smaller write cap, throughput-traded) are noted follow-ups.
- [x] No deadlock or priority inversion (existing `smoke-test` + `regression` still pass — incl. `storage-roundtrip`).

---

## Track E — Regression gate

### E.1 — `vfs-bulkio-smoke` gate

**Files:**
- `xtask/src/main.rs`
- `.githooks/pre-push`

**Symbol:** `cmd_vfs_bulkio_smoke`
**Why it matters:** Locks in the throughput + fairness wins so a later change cannot silently regress them; mirrors the existing opt-in gate pattern.

**Acceptance:**
- [x] Boots m3OS, reads a ≥16 MiB file, and asserts the block-request count is ≤ a threshold and wall-clock ≤ a threshold (Track A counters). **As-built (reframed):** measures a `pkg install mbedtls` (~3.8 MiB read through the VFS) rather than a pure ≥16 MiB read — no shell tool reads with a ≥64 KiB buffer, so a pure-read measurement isn't available from the serial console; the install exercises the same coalesced read path + the cache. Asserts the `read_calls` delta ≤ 3,500 (~2,114 as-built, ~36,200 pre-Phase-87).
- [x] Asserts the write side / fairness mechanism. **As-built:** the gate also asserts `write_calls` delta ≤ 2,400 (~1,836 as-built). The write reduction *is* the Track D fairness mechanism (fewer device writes per WRITE request → lower per-request latency), so this guards both — a regression that reintroduced the per-block bitmap writes (and the >1 s WRITE requests) would fail it.
- [x] Added to `AGENTS.md`'s pre-push gate table behind an `M3OS_VFS_BULKIO_REGRESSION=1` env var. **As-built:** AGENTS.md row + `.githooks/pre-push` block + `cargo xtask vfs-bulkio-smoke`.

---

## Track F — (Optional) bulk transfer + compression

### F.1 — Page-grant bulk file transfer

**File:** `kernel/src/fs/protocol.rs` / `kernel/src/mm/shm.rs`
**Symbol:** bulk read/write over a granted shared region
**Why it matters:** Removes the per-syscall copy for bulk payloads, aligning file I/O with the microkernel's "bulk data via page grants" rule.

**Acceptance:**
- [ ] A reader can map a granted region and receive multi-block payloads without a per-block copy through the syscall boundary.
- [ ] Isolation preserved: the grant is revoked on completion; no writable sharing persists.

### F.2 — Compress the `.m3pkg` payload

**Files:**
- `pkg-format/src/lib.rs`
- `userspace/pkg/src/main.rs`

**Symbol:** `pkg_format::serialize` / `unpack`, `pkg::install_one`
**Why it matters:** The `.m3pkg` is currently uncompressed; deflating the payload shrinks the 21 MiB read (the static `python3` compresses well), trading CPU (fast under KVM) for I/O.

**Acceptance:**
- [ ] `.m3pkg` payload is optionally deflated; `unpack`/`install_one` decompress in-OS (no_std decompressor).
- [ ] `python.m3pkg` artifact size shrinks measurably; `python-smoke` still PASSes.
- [ ] Format stays back-compatible (version byte) so existing uncompressed packages still install.

---

## Documentation Notes

- This phase changes **no on-disk format** and **no block-protocol wire format** — it changes request *granularity* and *scheduling* in the kernel FS layer, plus an already-landed userspace baseline (`pkg` chunked read + progress, Phase 85c).
- The motivating regression is `pkg install python` (Phase 85c); the heavy-I/O consumers are Phases 89 (Node.js) and 88 (Claude Code).
- Prefer exact symbols: `Ext2Fs::read_file_data`, `blk::read_sectors`, `resolve_block`, `read_block_into_slice`.
- Track A must land first — every other track's acceptance is stated in terms of its counters.
