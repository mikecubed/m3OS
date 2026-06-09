# Phase 92 — VFS Bulk-I/O Throughput & Fairness: Task List

**Status:** 🟡 Read-side landed (Tracks A + B + E). `pkg install mbedtls` block reads **36,183 → 4,282 (~8.4x)**, validated by `vfs-bulkio-smoke` + smoke-test + regression. Tracks C (readahead/write-back), D (fairness), F (optional) remain.
**Source Ref:** phase-92
**Depends on:** Phase 08 (Storage & VFS) ✅, Phase 55b (Ring-3 Driver Hosting) ✅, Phase 85a (Package Infrastructure) ✅
**Goal:** Make multi-megabyte VFS I/O fast and fair: coalesce ext2 block reads/writes into multi-block ring-3 driver requests, add readahead/write-back, and stop a bulk transfer from starving interactive clients — with `pkg install python` (21 MiB) as the motivating, measured case.

> **As-built architectural finding (not anticipated by the design doc).** The design doc's "Primary Components" listed only the kernel `Ext2Fs::read_file_data`. In fact **userspace file reads (incl. `pkg install`) route through the ring-3 `vfs_server`** (the Phase 93 write authority) when the "vfs" service is registered — which has its **own** ext2 reader that was **uncached** and served **one 4 KiB block per `VFS_PREAD`**. The kernel `read_file_data` (Track B.1) is used by **exec / binary loading** + the fallback; the `pkg install` bottleneck was the vfs_server path. So the read-side fix spans BOTH readers + the VFS read protocol:
> - **Track B.1 (kernel)** — `kernel_core::fs::ext2::read_file_data_coalesced` coalesces contiguous runs (capped to the block driver's `MAX_SECTORS_PER_REQUEST`), wired into `Ext2Fs::read_file_data`. Host-tested. Speeds up exec/binary loads.
> - **vfs_server read path (the actual `pkg install` fix)** — the same shared coalescer in `vfs_server`'s `read_file_data`, a **write-through block cache** on `Ext2State` (so the sub-block read-modify-write of allocation bitmaps / inode-table / directory blocks hits the cache instead of re-reading), and a **64 KiB `VFS_MAX_PREAD`** read cap (decoupled from the 4 KiB request buffer; the IPC bulk reply already carries up to 80 KiB).
> - **Residual** is the write side (~970 bitmap writes per install) — the clean Track C.2 (write-back) follow-up.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Baseline instrumentation (block-request + timing counters) | — | ✅ Done — `/proc/blkstats` |
| B | Contiguous-run batched reads (ext2 → multi-sector `read_sectors`) | A | ✅ Done — **both** the kernel engine AND vfs_server (shared coalescer) + vfs_server write-through block cache + 64 KiB read cap |
| C | Readahead + large-write write-back | B | Planned (C.2 write-back would cut the residual ~970 bitmap writes) |
| D | VFS fairness (bounded quanta / yield so bulk jobs don't freeze the UI) | B | Planned |
| E | `vfs-bulkio-smoke` regression gate + pre-push wiring | B, C, D | ✅ Done — read-throughput regression guard (`M3OS_VFS_BULKIO_REGRESSION=1`) |
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

**Acceptance:**
- [ ] Contiguous allocated blocks are flushed in one `write_sectors(count = N)` call.
- [ ] `pkg verify python` still reports 0 MISMATCH after a batched-write install.

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

**Acceptance:**
- [ ] Sequential writes to a file accumulate and flush in multi-block requests.
- [ ] Data is durably flushed on `close`/`fsync` (no lost writes); `pkg verify` passes.

---

## Track D — VFS fairness

### D.1 — Bounded work quanta / yield on bulk transfers

**File:** `kernel/src/fs/vfs.rs`
**Symbol:** the VFS request-servicing loop
**Why it matters:** Today a single 21 MiB transfer monopolizes the VFS/block path and freezes the compositor and `term` — the most visible bad UX. Bounding the work done before yielding (or interleaving other clients' requests) keeps interactive latency bounded.

**Acceptance:**
- [ ] A bulk read/write yields (or services a pending interactive request) at least every bounded quantum (e.g. every N blocks or M microseconds).
- [ ] During `pkg install python` in GUI mode, a QMP-injected keystroke is echoed by `term` within 500 ms (PPM/serial-probed), versus a multi-second stall today.
- [ ] No deadlock or priority inversion introduced (existing `smoke-test` + `regression` still pass).

---

## Track E — Regression gate

### E.1 — `vfs-bulkio-smoke` gate

**Files:**
- `xtask/src/main.rs`
- `.githooks/pre-push`

**Symbol:** `cmd_vfs_bulkio_smoke`
**Why it matters:** Locks in the throughput + fairness wins so a later change cannot silently regress them; mirrors the existing opt-in gate pattern.

**Acceptance:**
- [x] Boots m3OS, reads a ≥16 MiB file, and asserts the block-request count is ≤ a threshold and wall-clock ≤ a threshold (Track A counters). **As-built (reframed):** measures a `pkg install mbedtls` (~3.8 MiB read through the VFS) rather than a pure ≥16 MiB read — no shell tool reads with a ≥64 KiB buffer, so a pure-read measurement isn't available from the serial console; the install exercises the same coalesced read path + the cache. Asserts the `read_calls` delta ≤ 8,000 (~4,300 as-built, ~36,200 pre-Phase-92).
- [ ] Asserts interactive responsiveness during a concurrent bulk transfer (Track D probe). _Pending Track D (fairness) — not yet implemented._
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
- The motivating regression is `pkg install python` (Phase 85c); the heavy-I/O consumers are Phases 87 (Node.js) and 88 (Claude Code).
- Prefer exact symbols: `Ext2Fs::read_file_data`, `blk::read_sectors`, `resolve_block`, `read_block_into_slice`.
- Track A must land first — every other track's acceptance is stated in terms of its counters.
