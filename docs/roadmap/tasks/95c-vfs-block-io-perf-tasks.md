# Phase 95c — VFS / Block-I/O Performance: Task List

**Status:** Planned
**Source Ref:** phase-95c
**Depends on:** Phase 95b (demand-side loader) ✅ (A+B), Phase 95 ✅ (rust toolchain + `pkg install rust`), Phase 88 ✅ (vfs_server ext2 ownership), Phase 87 ✅ (VFS bulk-I/O + `/proc/blkstats`)
**Goal:** Make the ring-3 VFS read/write path fast enough that `pkg install rust` completes inside the install-step timeout and `rustc --version` cold-loads in reasonable time — flipping the Phase 95b `rustc-smoke` INSIDE-m3OS arm to PASS (`RUSTC_OK`) — **using the microkernel-idiomatic techniques** (zero-copy page-grant transfer, server-side readahead, a kernel page cache acting as the external-pager amortizer) **rather than moving the filesystem into the kernel.** `vfs_server` stays the sole ext2 authority for reads and writes.

> **Design stance.** The 95b prototype that read `/usr` pages straight from the in-kernel
> `EXT2_VOLUME` (bypassing `vfs_server`) is fast but **not microkernel-pure** — it widens
> ring-0's filesystem role and creates a two-readers-of-one-disk coherence hazard. Real
> userspace-FS microkernels (Mach/macOS memory objects, Fuchsia/Zircon VMOs + pager API,
> L4Re dataspaces) instead get fast `mmap` from three things: **(1) zero-copy bulk transfer
> via shared/granted memory** (m3OS's own IPC rule: "bulk data = page grants, never IPC
> payloads"), **(2) server-side readahead** so one round-trip serves many pages, and **(3) a
> kernel-owned page cache** so a fault on an already-resident page — a re-fault, a second
> `rustc` run, or another process mapping the same DSO — is served by the kernel VM with
> **zero** server IPC (the per-access cost collapses to per-*miss*). 95c leads with those.
> The in-kernel ext2 fast path is retained **only as a measurement-gated, documented,
> retireable fallback** (Track F) for the case where m3OS's raw IPC cost is itself the wall —
> and even then the right long-term fix is faster IPC, not ext2-in-the-kernel.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Zero-copy + readahead demand-fill — `vfs_server` fills a kernel-**granted** page (no IPC-payload copy) and serves large readahead per IPC; keeps `vfs_server` the sole FS reader | 87, 95b | Planned |
| B | Kernel page cache for file-backed pages (the external-pager amortizer) — re-faults, shared maps, and a second `rustc` run hit the cache with **no** server IPC | A, 95b | Planned |
| C | ext2-reader cache eviction — kill the fill-and-hold indirect-block thrash in `vfs_server`'s `Ext2State` cache (and the kernel `EXT2_VOLUME` cache) | A | Planned |
| D | Installer read/verify/write coalescing | A | Planned |
| E | Throughput **+ IPC-cost** measurement + regression gate (gates Track F) | A–D | Planned |
| F | **(Conditional fallback)** in-kernel ext2 read fast path — landed **only if** E proves IPC cost itself is the wall; a documented, retireable departure from the single-owner model | E | Conditional |
| G | Milestone: flip `rustc-smoke` to PASS (`RUSTC_OK`) | A–F, 95b | Planned |
| H | Docs, learning doc, kernel version bump | A–G | Planned |

---

## Track A — Zero-copy + readahead demand-fill (the idiomatic primary)

### A.1 — Fill demand-fault pages by **page grant**, not IPC-payload copy

**Files:**
- `kernel/src/arch/x86_64/syscall/mod.rs` (`vfs_service_read_kernel` / `demand_read_file_page`)
- `kernel/src/arch/x86_64/interrupts.rs` (`demand_map_vma_page` lazy branch)
- `userspace/vfs_server/src/main.rs` (`VFS_READ` handler)

**Symbol:** `vfs_service_read_kernel`; the `VFS_READ` reply path
**Why it matters:** The 95b demand-fill does `call_msg` + `take_bulk_data` → **copies** the reply bulk into the freshly-allocated frame. That violates m3OS's own "bulk = page grants, never IPC payloads" rule and pays a full data copy per fault. Instead, the kernel should hand `vfs_server` the **destination frame** (grant/shared region) and have it read ext2 **directly into it**, so there is no second copy — the standard zero-copy FS-server transfer.

**Acceptance:**
- [ ] A `MAP_LAZY_FILE` demand fault transfers file data into the destination frame with **no intermediate bulk copy** (the kernel grants/maps the frame to `vfs_server`, which reads into it; or an equivalent shared-region handshake). `dynamic-hello-smoke` stays green.
- [ ] The data-copy on the demand-fault path is eliminated (verified by inspection + a counter, or by the absence of the `take_bulk_data` copy on this path).

### A.2 — Large readahead per IPC in `vfs_server`

**Files:** `userspace/vfs_server/src/main.rs` (`VFS_READ`); `kernel-core/src/fs/vfs_protocol.rs` (`VFS_MAX_PREAD` / `MAX_BULK_LEN`)
**Symbol:** the `VFS_READ` handler; `VFS_MAX_PREAD`
**Why it matters:** One IPC round-trip per 4 KiB makes a 162 MB sequential load thousands of round-trips. Serving a large cluster (e.g. 256 KiB–1 MiB) per `VFS_READ` amortises the round-trip over hundreds of pages — the in-model way to cut IPC, no bypass needed. (The kernel demand-fill already does 64 KiB readahead from 95b; A.2 raises the server cap and the fault cluster in lockstep.)

**Acceptance:**
- [ ] A sequential cold load of an N MiB DSO issues `VFS_READ` round-trips in proportion to `N / cluster` (hundreds, not thousands), asserted via a counter or `/proc/blkstats`.
- [ ] `vfs-bulkio-smoke` + `dynamic-hello-smoke` stay green.

---

## Track B — Kernel page cache for file-backed pages (external-pager amortizer)

### B.1 — Cache faulted file pages by `(file-id, offset)`; serve cache hits with no server IPC

**Files:** `kernel/src/mm/` (a page cache keyed by file identity + offset), `kernel/src/arch/x86_64/interrupts.rs` (the demand-fill: consult the cache before issuing a read), `kernel/src/process/mod.rs` (the VMA's file identity)
**Symbol:** the new file-backed page cache; `demand_map_vma_page`
**Why it matters:** This is the Mach/Zircon move: the kernel owns physical pages, so it owns the cache. On a fault **miss** it asks the pager (`vfs_server`, via Track A); on a **hit** — a re-fault, a *second* `rustc` invocation, or two processes mapping the same `librustc_driver.so` — it maps the resident page with **zero** server IPC. The per-access cost collapses to per-miss, and shared/repeat loads become free. Keeps the FS in userspace while approaching monolithic `mmap` performance.

**Acceptance:**
- [ ] A faulted read-only file page is retained in a kernel page cache keyed by `(file identity, page offset)`; a second fault on the same `(file, offset)` (re-fault, second process, second invocation) is served from the cache with **no** `VFS_READ` IPC (asserted via the round-trip counter).
- [ ] Eviction is bounded (LRU or a cap) so the cache cannot grow unbounded; correctness holds (a write through `vfs_server` invalidates the relevant cached pages — coherence with the write authority).
- [ ] `dynamic-hello-smoke` + a repeat-load micro-test stay green.

---

## Track C — ext2-reader cache eviction

### C.1 — Replace the fill-and-hold block cache with an evicting one

**Files:** `userspace/vfs_server/src/main.rs` (`Ext2State` block cache); `kernel/src/fs/ext2.rs` (`block_cache`, `BLOCK_CACHE_MAX`) if the kernel reader is also on a hot path
**Symbol:** `Ext2State::read_block` / the block cache
**Why it matters:** Both ext2 block caches are bounded **fill-and-hold (no eviction)**, so a 162 MB sequential scan fills the cache in the first few MB and every later block — including the hot single/double-indirect pointer blocks re-read per data block — misses to the device. An LRU (or a small dedicated indirect-block cache) keeps the hot pointer blocks resident so the readahead reads aren't dominated by indirect re-reads.

**Acceptance:**
- [ ] The block cache evicts (LRU or equivalent); a large sequential read keeps hot indirect blocks resident and device block reads drop measurably (`/proc/blkstats`).
- [ ] Cache stays coherent with writes; host tests pass.

---

## Track D — Installer read/verify/write coalescing

### D.1 — Coalesce the `pkg install` read/verify/write loop

**File:** `userspace/pkg/`
**Symbol:** the install extract loop (read → SHA-verify → write)
**Why it matters:** `pkg install` reads the `.m3pkg`, SHA-verifies (re-read), and writes hundreds of MB through small buffers — every read/write a `vfs_server` round-trip. Larger buffers (Track A caps), hashing in-line with the read (skip the verify re-read where possible), and coalesced writes cut the install I/O substantially.

**Acceptance:**
- [ ] The installer uses the Track A bulk caps and avoids the redundant verify re-read where the write path can hash in-line; install `read_calls`/`write_calls` (`/proc/blkstats`) drop materially.
- [ ] `pkg-smoke` + `vfs-bulkio-smoke` stay green.

---

## Track E — Throughput + IPC-cost measurement + gate

### E.1 — Measure the per-IPC cost; assert throughput

**File:** `xtask/src/main.rs`; a small in-OS probe
**Symbol:** a new `cmd_*_smoke` (or extend `vfs-bulkio-smoke`); `M3OS_*_REGRESSION`
**Why it matters:** Two jobs. (1) **Decide the fallback:** measure the cost of one `VFS_READ` round-trip and the achieved throughput at a 256 KiB–1 MiB cluster — if a big-readahead IPC already yields multiple MB/s, Track F (the in-kernel bypass) is unnecessary; if the IPC/context-switch cost *itself* dominates even at large clusters, F is justified (and the real lesson is "make IPC faster"). (2) **Guard:** a falsifiable throughput gate so a regression is caught.

**Acceptance:**
- [ ] A measurement reports per-`VFS_READ` round-trip cost and cold-load throughput at the chosen cluster size; the Track F go/no-go is recorded from it.
- [ ] A gate asserts a minimum VFS read throughput (request-count and/or wall-clock) on install + cold-load; opt-in env var; skip-with-reason when prerequisites absent.

---

## Track F — (Conditional fallback) in-kernel ext2 read fast path

### F.1 — Read-only `/usr` demand pages direct from `EXT2_VOLUME` — **only if E justifies it**

**File:** `kernel/src/arch/x86_64/syscall/mod.rs` (`demand_read_file_page`)
**Symbol:** `demand_read_file_page`; the `VfsService` → `EXT2_VOLUME` path
**Why it matters:** If — and only if — Track E shows m3OS's raw IPC cost is the wall (a large-readahead IPC still can't reach acceptable throughput), fall back to reading read-only `/usr` demand pages straight from the in-kernel ext2 engine by inode (no IPC). This is the 95b prototype, landed **with** its coherence + concurrency proof and an **explicit "model departure"** note + a retirement condition (retire once IPC is fast enough). It is a last resort, not the design.

**Acceptance:**
- [ ] **Gated on E:** landed only if the measurement records IPC cost as the binding constraint at large clusters; otherwise this track is **not implemented** and is documented as intentionally-skipped.
- [ ] If landed: coherence proof (write-via-vfs then read-back-via-fast-path returns new bytes; `MAP_PRIVATE` writes never reach the file) + concurrency-safe with `vfs_server` (`smp-smoke` + `dynamic-hello-smoke` green); a docstring marks it a deliberate departure with a retirement condition.

---

## Track G — Milestone: `RUSTC_OK` unblock

### G.1 — Flip the Phase 95b `rustc-smoke` INSIDE-m3OS arm to PASS

**File:** `xtask/src/main.rs` (`cmd_rustc_smoke` — reused from 95/95b, unchanged)
**Symbol:** the `RUSTC_OK` sentinel
**Acceptance:**
- [ ] `pkg install rust` completes well within the install-step timeout (fresh, non-`FAST_ITER`).
- [ ] `rustc --version` → `--print sysroot` → `rustc hello.rs` → `RUSTC_OK`; `rustc-smoke` PASSES under `M3OS_RUST_REGRESSION=1`.

---

## Track H — Docs, learning doc, version bump

### H.1 — Docs + version

**Files:** `docs/95c-vfs-block-io-perf.md` (learning doc) + registration; `docs/roadmap/README.md` row + mermaid edge; `kernel/Cargo.toml` version; `AGENTS.md`
**Acceptance:**
- [ ] Learning doc teaches the **external-pager / zero-copy / readahead** approach (and why it's preferred over ext2-in-the-kernel); registered in `docs/README.md` + `codebase-map.md`.
- [ ] Roadmap README row + `P95b --> P95c` edge; design-doc Status flips on landing; kernel version bumped; `AGENTS.md` marks on-device `rustc` codegen landed once Track G passes.

## Documentation Notes

- **The design is microkernel-idiomatic by default.** `vfs_server` stays the sole ext2 authority; performance comes from zero-copy (A.1), readahead (A.2), and a kernel page cache (B) — the Mach/Zircon/L4Re recipe — not from moving ext2 into the kernel.
- **Track F is a flagged, conditional, retireable fallback**, gated on the Track E measurement. Prefer A+B+C; only land F if IPC cost itself is proven to be the wall, and then document it as a departure with a retirement condition.
- **Confirm the immediate cause first**: a fresh non-`FAST_ITER` `rustc-smoke` run — does `pkg install: rust: OK` appear, and how long does the install take? (Install timeout ⇒ A/D/the install path; complete-but-slow ⇒ B/A for cold load.)
- Prefer exact symbols: `vfs_service_read_kernel`, `demand_read_file_page`, `demand_map_vma_page`, `VfsFileMeta::inode`, `Ext2State`, `EXT2_VOLUME`, `block_cache`, `VFS_MAX_PREAD`, `/proc/blkstats`.
