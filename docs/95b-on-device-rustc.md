# On-Device `rustc` Code Generation

**Aligned Roadmap Phase:** Phase 95b
**Status:** ✅ Complete (milestone) — Areas A + B landed & validated **and the `RUSTC_OK` milestone (Area D) is ACHIEVED + MULTITHREADED (2026-06-25)**: `rustc hello.rs` → multithreaded `rust-lld` → native binary runs (`RUSTC_OK`) under `M3OS_KVM=1`. Beyond Areas A+B's streaming loader, the milestone needed a crash-chain fix, a **cross-DSO TLS-at-offset-0** loader fix (drops `--threads=1`), and a **thread-group fatal-kill** (`addr=0x8`) robustness fix (see "How the milestone was actually unblocked" below). Deferred: a TCG-runnable gate (Phase 95c) + the cargo/proc-macro stretch (Track E).
**Source Ref:** phase-95b
**Supersedes Legacy Doc:** N/A (extends `docs/95-native-rust-toolchain.md` with the unblocking kernel + loader work)

## Overview

Phase 95b clears the wall Phase 95 ran into: the installed dynamic musl `rustc`
1.96.0 (`pkg install rust` works, `librustc_driver.so` + the prebuilt sysroot
are on-device) — but the first time `rustc` is *run*, it never finishes. The
root cause is a **whole-file read+copy per-DSO loader strategy** that is
CPU-bound on a 162 MB shared object. The fix is a two-part rework of the
`ld-musl` loader + the kernel's memory-management layer, from eager all-at-once
loading to **streaming / demand-paged file-backed** loading that touches only
the pages `rustc` actually executes.

The phase also addresses two multi-core amplifiers Phase 95's diagnosis
exposed — a per-mapping **TLB-shootdown IPI storm** during the bulk address-
space construction the loader performs (Track B), and an intermittent **64 KiB
per-task kernel-stack overflow** deep in the page-fault read chain (Track C) —
then lands the headline milestone: `rustc /usr/src/hello.rs -o /tmp/hello &&
/tmp/hello` prints `RUSTC_OK` on m3OS (Track D). The stretch (Track E) adds
`cargo` and the first on-device **proc-macro** compile via `dlopen` of a
proc-macro `.so` against the Phase 93 `libc.so`.

This doc is the pedagogical companion to the implementation-focused
[design doc](./roadmap/95b-on-device-rustc.md): it teaches the loader and
kernel memory concepts the phase exercises — concepts that also apply to every
other large dynamic binary (dynamic `python3`, `ctypes` `.so`s) after the
rework lands.

## How the milestone was actually unblocked (2026-06-25)

The Areas A+B streaming/demand-paged loader (below) cleared Phase 95's
**eager-load** wall, but it turned out **not** to be the last thing between us
and `RUSTC_OK`. The original diagnosis ("the loader still wedges loading the
162 MB DSO" / "the FS-throughput is the binding constraint") was a **plain-TCG
artifact** — measured under `M3OS_KVM=1` the install is ~25 s and the cold load
~10 s. Once that red herring was set aside, three concrete bugs stood between a
*running* `rustc` and a *code-generating* one (full record:
[`docs/handoffs/2026-06-24-phase-95-completion-plan.md`](./handoffs/2026-06-24-phase-95-completion-plan.md)):

1. **A crash chain so `rustc` even starts.** A process-page-table fix (commit
   `841fd53f`), a `FIONBIO` `ioctl` that was returning `ENOTTY` (rust's pipe
   setup looped on it), and — so rust-lld can find its sibling `libLLVM.so` —
   kernel `AT_EXECFN` support plus loader `DT_RUNPATH`/`$ORIGIN` expansion (the
   loader previously ignored `RUNPATH`, so the `$ORIGIN`-relative `libLLVM.so`
   next to the `rust-lld` binary was never searched).
2. **The cross-DSO TLS-at-offset-0 loader bug** (the reason `--threads=1` was a
   temporary workaround). rust-lld's parallel relocation scan indexes
   `relocsVec[llvm::parallel::threadIndex]`; `threadIndex` is a `thread_local`
   *exported by one DSO and read initial-exec by another*, and it legitimately
   lives at **TLS offset 0** in its module. The loader's symbol lookup used
   `st_value != 0` as the "is this symbol defined?" test — which silently
   rejected a thread-local *defined* at offset 0, so every worker read a stale
   `threadIndex` (`UINT_MAX`) and indexed out of bounds → a deterministic
   userspace fault on the pool workers. The fix threads an `accept_tls_zero`
   flag through the TLS lookup path so it tests `st_shndx != SHN_UNDEF`
   (genuine definedness) instead of `st_value != 0`. Guarded forever by the
   `dynamic-tls` reproducer (a `DT_NEEDED` DSO thread-local written
   general-dynamic + read initial-exec across pthreads) in `dynamic-hello-smoke`.
3. **The thread-group fatal-kill (`addr=0x8`) robustness hole.** The worker
   fault in (2) then exposed a second, independent kernel bug: an unhandled
   fatal fault in *one* thread of a multithreaded process killed only the
   faulting TID and freed the **shared** address space, stranding sibling
   threads parked `BlockedOnFutex` (single-core hang) or — on SMP — racing the
   page-table free into a kernel NULL+8 deref. Fixed by routing the fatal-fault
   kill through the same group-quiesce + full-process-exit path as
   `exit_group(2)` (SIGSEGV-encoded). Guarded by the `thread-fault` reproducer
   (`leader-ok` + `worker-ok` arms) in `dynamic-hello-smoke`. This is a general
   kernel-robustness fix, not strictly on the `RUSTC_OK` critical path once (2)
   was fixed (the workers no longer fault), but it closes the hole that any
   multithreaded crash would hit.

With (1)+(2) fixed, **multithreaded** rust-lld links a native ELF and
`rustc /usr/src/hello.rs && /tmp/hello` prints `RUSTC_OK` on m3OS — the
`--threads=1` constraint is dropped. The streaming loader (Areas A+B) below
remains the substrate that makes loading the 162 MB DSO tractable in the first
place; the rest of this doc teaches it.

## What This Doc Covers

- Why a **whole-file read+copy** loader is O(file size) in both RAM and CPU,
  and why that is fine for tens-of-MB `.so`s but fatal for a 162 MB one.
- The **A.1 partial win**: read only a 64 KiB header window, then stream each
  `PT_LOAD` straight into the image — drops the scratch buffer and the intra-RAM
  copy, but still reads the full file eagerly.
- The difference between **eager** file-backed `mmap` (allocate every frame +
  read the whole file up front) and **demand-paged** file-backed `mmap` (a VMA
  that faults pages in from the file on first touch), and why only the latter
  turns a 162 MB load into a small working set.
- The **A.2 unblocking fix**: the new `MAP_LAZY_FILE` flag and how `load_dso`
  uses it to overlay each `PT_LOAD` as a lazy file-backed region over the image
  reservation.
- The **novel kernel detail**: a blocking IPC from `#PF` context — why it is
  sound on m3OS, and why it had never been needed before.
- Why bulk address-space mutation amplifies under SMP via **TLB-shootdown IPIs**,
  and how skipping the shootdown on a not-present → present demand fault bounds
  the storm.
- **Kernel-stack sizing trade-offs**: flat large per-task stacks vs.
  commit-on-claim.
- Why **proc-macros** are the wall to the mainstream crate ecosystem, and what
  the Track E stretch validates.

## Core Implementation

### The Phase 95 diagnosis: a CPU-bound 162 MB load

`rustc` is a small (~5 KB) launcher binary that `DT_NEEDED`s two shared
objects: the ~162 MB `librustc_driver.so` (LLVM **statically linked in**) and
`libc.so`. The Phase 76/93 `ld-musl` loader's per-DSO strategy was:

1. `lseek(SEEK_END)` to get the file size.
2. `mmap` a **scratch** anonymous region of that size.
3. `sys_read` the **whole file** into the scratch.
4. `mmap` a second anonymous region of the same size — the **image**.
5. For each `PT_LOAD`: `copy_nonoverlapping` from scratch into image.
6. Relocate in place. Drop the scratch.

For a 162 MB DSO this is ~324 MB of anonymous `mmap` (scratch + image) + a
~162 MB read into the scratch + a ~162 MB intra-RAM copy. Per invocation, on a
2 GiB guest. Phase 95 drove the load to failure under single-core KVM and
multi-core — not a deadlock, not a VFS-latency stall, not the symbol resolver
(GNU hash is O(1)): just the read + copy, CPU-bound for hundreds of seconds.
The dynamic `python3` (Phase 93) worked because its `.so`s are tens of MB, not
162 MB.

Multi-core made it materially worse. Each `mmap`/`mprotect`/CoW operation in
the loader's bulk address-space construction broadcasts a **TLB-shootdown IPI**
to every other core, pegging the idle cores at ~380% CPU on a 4-core guest
during the load.

An intermittent **64 KiB per-task kernel-stack overflow** also surfaced
servicing the deeper `#PF` read chain. The kernel already recovered (killed the
offending task, the core returned to the scheduler — the `#PF`/`#DF` controlled-
kill recovery from the 2026-06-14 SMP handoff), but the overflow must stop
happening without the +104 MiB eager boot cost of a flat 64 → 256 KiB bump.

### A.1 — Loader scratch elimination (landed)

The first fix required **no kernel changes**. The full-file scratch buffer
existed as a read target before the loader even knew how many `PT_LOAD`
segments there were or where they landed. But the ELF header and all program-
header table entries are located at the file start — a single **64 KiB header
window** is large enough for any program-header table in practice. So A.1
reworks `load_dso_impl` as follows:

- Read a 64 KiB header window to parse the ELF header + all `PT_LOAD` entries.
- `mmap` one anonymous image of the computed image size (unchanged).
- For each `PT_LOAD`: `lseek(fd, ph.p_offset, SEEK_SET)` + `sys_read` of
  `ph.p_filesz` bytes **directly** into `load_bias + ph.p_vaddr`. The BSS tail
  (`p_memsz - p_filesz`) stays zero via the anonymous-zeroed image.
- An RAII `FdGuard` keeps the fd open through segment streaming and closes it
  on every return path.

For a 162 MB DSO this drops the ~162 MB scratch `mmap` and the ~162 MB intra-
RAM `copy_nonoverlapping` — roughly halving anon footprint and copy traffic per
DSO. It does **not** remove the eager full read (each `PT_LOAD` `sys_read` still
reads the whole file content, just directly into the image), so it is a real,
self-contained reduction but does not by itself unblock the milestone. That is
A.2's job.

Validated: `cargo xtask check` clean; `dynamic-hello-smoke` PASS under KVM
(`DYNAMIC_HELLO:ok` + `DLOPEN:ok` — the Phase 93 dynamic C / dlopen path is
unaffected; the full-file scratch is gone by construction).

### A.2 — Kernel lazy file-backed `mmap` + loader switch to file-backed `PT_LOAD` mapping (landed)

This is the fix that unblocks the milestone. The key insight: a DSO's code
pages are read-only. On a real OS they are **demand-paged** — the first
`CALL`/`JMP` into a page triggers a page fault, the kernel reads that one page
from the file, and execution resumes. Only the working set — the pages
`rustc` actually executes — ever lands in RAM. For a 162 MB DSO of which
`rustc --version` executes a fraction, this is a far smaller cost.

m3OS's `sys_mmap_file_backed` was **eager**: it looped over every page,
allocated a frame, and read the file content up front. Area A.2 adds a lazy
alternative via a new kernel-internal flag `MAP_LAZY_FILE` (bit 32, above every
POSIX `MAP_*` flag, defined in `kernel_core::mm`):

```
pub const MAP_LAZY_FILE: u64 = 1 << 32;
```

A `MAP_LAZY_FILE | MAP_PRIVATE` file `mmap` installs a VMA that records
`(fd, offset)` but **allocates no frames**. The page-fault handler's demand-
fill path gains a new branch: when a fault address falls inside a lazy-VMA, it
calls `shared_vma_demand_file` to extract `(prot, pkey, fd, page_file_offset)`
(releasing the `PROCESS_TABLE` lock before any I/O), reads exactly one page
from the backing file, then calls `demand_map_user_page_from_buf_locked` to
allocate a zeroed frame, copy the file bytes in, and install the PTE —
zero-filling the tail past EOF for the last partial page.

A plain `MAP_PRIVATE` file `mmap` without `MAP_LAZY_FILE` stays **eager**
(POSIX mmap-then-close is preserved for callers like `lld` that map an input
file and immediately close the fd — the flag is strictly opt-in, used only by
the loader).

The loader (`load_dso_impl`) is updated to use the lazy path for each
`PT_LOAD`:

1. Reserve the whole image span as one anonymous `mmap` (the load bias anchor).
2. For each `PT_LOAD`, **`MAP_FIXED`-overlay** the file part as a lazy file-
   backed region (`MAP_LAZY_FILE | MAP_PRIVATE | MAP_FIXED`, with the page-
   aligned `p_offset` as the file offset).
3. Map the BSS tail (from the page just past the file part to `p_memsz`)
   as a separate anonymous-zero `MAP_FIXED` overlay.
4. Zero the BSS bytes that **share** the last file page with file content
   (write to the boundary byte range — this faults the last file page in and
   then zeroes its `[filesz, page_end)` tail, correcting any trailing bytes
   from the next segment's file content that a page-sized read would have
   fetched).
5. After relocation, drop text segments to `R-X`.
6. **Keep the fd open** (leak the `FdGuard` via `core::mem::forget`) for the
   mapping's lifetime, so the kernel can demand-page from it. Every error path
   above the success point still drops the guard and closes the fd (the failed
   DSO's mappings go unused).

Writable `PT_LOAD` segments (the GOT, `.data`, BSS) receive `PROT_READ |
PROT_WRITE` at mapping time so the relocation engine can patch them, and are
left writable (they are data, not code). Text is dropped to `R-X` after
relocation in the existing mprotect pass.

Validated: `cargo xtask check` clean; `dynamic-hello-smoke` PASS under KVM —
`DYNAMIC_HELLO:ok` (the `libc.so` demand-paged from `vfs_server` via a blocking
IPC in the `#PF` handler, the kernel's first blocking IPC from fault context)
and `DLOPEN:ok`.

### The novel kernel detail: a blocking IPC from `#PF` context

On m3OS's default boot configuration, `/usr` files (where `librustc_driver.so`
lives after `pkg install rust`) are served by the **ring-3 `vfs_server`** over
IPC, not the in-kernel ext2 engine. So when a lazy `PT_LOAD` page faults in,
the kernel must read one page from the backing file — and that read is a
synchronous `call_msg` to the ring-3 `vfs_server`.

This is m3OS's **first blocking IPC from page-fault handler context**. Why is it
sound?

- A **ring-3 fault** is entered by the `#PF` ISR, which pops to the kernel
  page-fault handler (`page_fault_handler` in `interrupts.rs`). At that point,
  **no kernel locks are held** (the faulting task's kernel thread is in ring 0
  handling the fault, but the page-table lock is not held across I/O — the
  `shared_vma_demand_file` lookup releases it before returning, and the actual
  PTE install happens inside a fresh `demand_map_user_page_from_buf_locked` call
  that re-acquires the lock only for the PTE write).
- Blocking simply means the faulting task is switched out and rescheduled when
  `vfs_server` replies — exactly the same mechanism as a `sys_read` syscall
  blocking on a disk read. The scheduler is in a clean state.
- `vfs_server` is a **static binary** (no dynamic linking, no lazy VMAs of its
  own), so it never recurses into the lazy demand-fault path itself while
  servicing the kernel's read request. The ring-3 → kernel → `vfs_server` call
  chain terminates cleanly.

A not-present → present demand fill also needs **no cross-core TLB-shootdown
IPI**: a page that was never present cannot be cached in any other core's TLB,
so making it present is purely local. This is the initial, most important IPI
reduction — Track B extends the batching/coalescing to the rest of the anon
demand-fault and `mprotect` paths.

### Track B — SMP TLB-shootdown batching

Under multi-core, the loader's tens-of-thousands of `mmap`/`mprotect`/CoW page
operations during DSO load each broadcast a shootdown IPI to every other core,
so a single `rustc` load pegs the idle cores. Phase 95b batches and coalesces
shootdowns during bulk address-space mutation so a bounded number of IPIs are
issued per load, rather than one per page.

The work continues the Track A–D recovery from the
[2026-06-14 SMP/TLB/kstack handoff](./handoffs/2026-06-14-claude-smp-tlb-shootdown-kstack-panic.md).
The always-on `smp-smoke` gate (futex-heavy libuv-threadpool stress under 4
cores) is the regression guard for correctness (no stale-TLB access on any
core after a batched flush).

### Track C — Targeted/lazy kernel-stack strategy

The per-task kernel stack overflows intermittently while servicing `rustc`
because the demand-fault-from-file read chain is deeper than the simple
anonymous demand-fill path (it adds at least one IPC frame on the kernel
stack). The controlled-kill recovery (introduced in Phase 95: a `#PF`/`#DF`
guard-page fault kills the offending task and returns the core to the
scheduler) prevents the box from hanging, but the overflow must not happen at
all for the milestone.

A flat 64 → 256 KiB per-task stack bump removes the overflow (proven at
diagnosis time) but costs **+104 MiB** at boot across all 542 task slots,
allocated eagerly whether or not a task ever overflows. Phase 95b lands a
targeted fix instead: **commit-on-claim** — map a slot's guard-page-backed
frames only when a task actually claims the slot, so only live tasks pay, and
**demand-commit depth** — grow the committed region incrementally on guard-page
faults up to a per-task-class cap. The `kstack-overflow-smoke` gate (the
`SYS_KSTACK_OVERFLOW_TEST` probe → `KSTACK_OVF:killed:ok` → `KSTACK_OVF:survivor:ok` →
`KSTACK_OVF:done` sequence) validates the recovery path regardless of strategy;
boot-time kstack memory is asserted materially lower than a flat 256 KiB bump.

### Track D — The on-device `RUSTC_OK` milestone (✅ ACHIEVED — re-diagnosis below SUPERSEDED)

> **➜ SUPERSEDED (2026-06-25).** The milestone is **ACHIEVED + MULTITHREADED** — see
> "How the milestone was actually unblocked" above. The blocked re-diagnosis below is the
> **pre-page-table-fix, TCG-only** record (rustc *does* run; the FS was never the milestone
> blocker under KVM; the real blocker was the multithreaded cross-DSO TLS loader bug, now
> fixed). Preserved for the forensic trail.

Area A.2 cleared the **Phase 95 wall** (the CPU/IPC-bound eager 162 MB read+copy).
At the time, `rustc --version` (step 14 of `rustc-smoke`) still timed out — for a **new,
deeper reason** this phase instrumented and pinned down:

- **rustc never runs userspace.** A timer-ISR userspace-RIP sampler (logging the
  interrupted ring-3 RIP on every `timer_handler_user` entry) produced **zero**
  samples across the full 25-minute window. rustc is **blocked in the kernel**, not
  CPU-grinding in userspace.
- **rustc demand-pages < 1 MB of `librustc_driver.so` then stops.** A demand-fill
  page counter stayed at **0 MiB** (1 MiB threshold). The loader loads the small
  `libc.so` (same ext2 / `vfs_server` path A.2 validates) but **wedges loading the
  162 MB `librustc_driver.so`** — before relocating its bulk.
- **The only active work is the headless GUI servers** busy-looping on input /
  frame-tick syscalls (~41 M syscalls/window: `READ_KBD_SCANCODE`,
  `READ_MOUSE_PACKET`, `FRAME_TICK_DRAIN`). On SMP=1 they also starve the single
  core; on SMP=4 cores are free yet rustc **still** makes no progress — confirming
  rustc is **blocked**, not merely starved.

So the loader's handling of the 162 MB DSO **blocks in the kernel** — a demand-fill
read that never completes, a large-DSO loader/mm path that wedges, or a silent
early loader exit. The exact wedge is **not yet pinned** (the loader's `serial()`
diagnostics are release-suppressed), and is a **tracked Phase-95b follow-up**: a run
with loader-serial enabled + a demand-fill enter/block trace will localize it.

The `rustc-smoke` gate scaffold, the `rust`/`musl` `.m3pkg` bundling, and on-device
`pkg install rust` (from Phase 95) are reused unchanged — only the INSIDE-m3OS arm
remains blocked.

### Track E — `cargo` + proc-macros (the stretch)

`cargo` is staged alongside `rustc` in the `.m3pkg` and a bundled proc-macro-
free fixture crate proves `cargo build` (`CARGO_OK`) with no network fetch (all
dependencies are in the prebuilt sysroot). Then the real wall: a crate whose
dependency graph includes a **derive macro** — a `cdylib` `.so` that `rustc`
`dlopen`s at **compile time**.

A proc-macro `.so` is an ordinary shared library. `rustc` calls `dlopen` on it
during compilation, and the proc-macro crate's code — which references `malloc`,
`memcpy`, and TLS — binds those relocations against `/usr/lib/libc.so` (the
Phase 93 musl). This is the Rust analog of the Phase 93 `ctypes.CDLL(...)` proof
that a `dlopen`'d shared object can bind libc + use TLS through the m3OS loader.
If that proof holds for a *Rust* proc-macro `.so`, the gate to the mainstream
crate ecosystem (saturated with `serde_derive`, `thiserror`, `clap` derive, …)
is open.

Track E validates this with a minimal derive-macro fixture crate + consumer and
asserts `CARGO_PROCMACRO_OK`. A separate `cargo-smoke` gate
(`M3OS_CARGO_REGRESSION`) keeps this independent of the `rustc-smoke` milestone
so the Area D gate stays green regardless of the stretch's status.

## Key Files

| File | Purpose |
|---|---|
| `userspace/ld-musl-x86_64.so.1/src/main.rs` | `load_dso_impl` — the A.1 header-window + stream and A.2 lazy-file-backed `PT_LOAD` mapping paths; `FdGuard` fd leak on success |
| `kernel-core/src/mm.rs` | `MAP_LAZY_FILE` constant (bit 32); `FileBacking` — reused for the lazy demand-fault source |
| `kernel/src/arch/x86_64/interrupts.rs` | `demand_map_vma_page` (the lazy-VMA dispatch in the `#PF` handler); `demand_map_user_page_from_buf_locked` (fill a new frame from a file-bytes buffer) |
| `kernel/src/arch/x86_64/syscall/mod.rs` | `sys_mmap_file_backed` — lazy branch for `MAP_LAZY_FILE` alongside the existing eager `MAP_SHARED` writeback path |
| `kernel/src/process/mod.rs` | `shared_vma_demand_file` — copies `(prot, pkey, fd, page_file_offset)` out before releasing the `PROCESS_TABLE` lock so the `#PF` handler can read without holding it |
| `kernel/src/smp/tlb.rs` | Track B: shootdown-IPI batching/coalescing during bulk mapping |
| `kernel/src/arch/x86_64/interrupts.rs` (kstack) | Track C: `try_recover_kstack_overflow`; the per-task kstack allocator |
| `xtask/src/main.rs` (`cmd_rustc_smoke`, `rustc_smoke_steps`) | The gate: `pkg install rust` → `rustc --version` → `--print sysroot` → `rustc hello.rs` → `RUSTC_OK` |
| `xtask/src/main.rs` (`cmd_cargo_smoke`) | Track E: the separate `cargo-smoke` gate (`M3OS_CARGO_REGRESSION`) |
| `docs/roadmap/95b-on-device-rustc.md` | The authoritative design doc for this phase |

## How This Phase Differs From Later Work

- Phase 95b adds **lazy file-backed mmap** as a loader-only opt-in (`MAP_LAZY_FILE`).
  A full **unified page cache** — shared across multiple mappings, reclaimed under
  memory pressure, consistent across `mmap` + `read` views — is a broader mm
  refactor deferred to a later phase.
- The `MAP_LAZY_FILE` path covers **`MAP_PRIVATE` read (demand-fault from file)**
  and **anonymous BSS tail** layout exactly as required by DSO loading. A `MAP_SHARED`
  file mmap with writeback (e.g. memory-mapped I/O to a shared file) stays on the
  existing eager path, which is correct for that case.
- Track E (proc-macros) validates the **on-device `dlopen`-by-rustc** path for a
  Rust `.so`. Full `cargo` feature parity — `build.rs` / `cc`-crate invocation,
  crates.io network fetch, incremental codegen — is deferred.
- Phase 95b targets **stock `x86_64-unknown-linux-musl`** with the prebuilt `std`
  sysroot (the Phase 94 proven path). A bespoke `x86_64-m3os-user.json` Rust
  target with `-Zbuild-std` is an orthogonal future refinement.
- The **`rustc` binary itself stays dynamic** (`crt-static = false`, `DEPS=musl`):
  a fully-static rustc proved infeasible in Phase 95 (a `crt-static` musl host
  can't build rustc's own proc-macro deps). This is different from the static
  clang/python/go discipline but exactly the same as the dynamic Python launched in
  Phase 93.

## Related Roadmap Docs

- [Phase 95b design doc](./roadmap/95b-on-device-rustc.md)
- [Phase 95b task doc](./roadmap/tasks/95b-on-device-rustc-tasks.md)
- [Phase 95 — Native Rust Toolchain](./95-native-rust-toolchain.md) (the host-side cross-build + packaging this phase extends; the `pkg install rust` groundwork)
- [Phase 93 — Dynamic C Runtime](./93-dynamic-c-runtime.md) (`libc.so` + loader TLS — the `DEPS=musl` foundation the dynamic `rustc` and proc-macro `.so`s both depend on)
- [Phase 76 — Dynamic Linker](./roadmap/76-dynamic-linker.md) (the from-scratch Rust `ld-musl` loader `load_dso` reworks here)
- [Phase 85d — Clang/LLVM/LLD on-device](./roadmap/85d-clang-llvm.md) (the direct precedent: on-device LLVM-class compiler + bundled LLD + the streaming-exec/`pread64` kernel substrate)
- [Phase 87 — VFS bulk I/O](./roadmap/87-vfs-bulk-io.md) (the batched VFS read path `vfs_server` uses to serve demand-fault pages; the bulk-IO coalescing that keeps demand-fault latency acceptable)
- [SMP/TLB/kstack handoff](./handoffs/2026-06-14-claude-smp-tlb-shootdown-kstack-panic.md) (the TLB-shootdown + kstack-overflow diagnosis Tracks B and C continue)

## Deferred or Later-Phase Topics

- **A unified page cache.** Phase 95b reads fault pages straight from the VFS
  (one IPC per faulted page). A shared cache across `mmap` and `read` views,
  evictable under memory pressure, eliminates redundant reads when multiple
  processes map the same `.so`. This is a broader mm refactor with no targeted
  landing phase yet.
- **`crates.io` registry access and networked `cargo`.** Wiring `cargo`'s HTTPS
  client to the Phase 86c mbedTLS/curl TLS stack so `cargo add` / `cargo build`
  can fetch crates.io dependencies on-device. Track E targets offline-vendored
  crates only.
- **`build.rs` / `cc`-crate execution.** A `build.rs` that invokes a C
  compiler drives the on-device clang port (Phase 85d); wiring `cargo` to
  invoke it correctly is a follow-on.
- **A self-hosting `rustc` bootstrap on-device** (building `rustc` *on* m3OS
  from source) — the Rust analog of clang self-hosting, itself still deferred
  in Phase 85d.
- **Incremental compilation and parallel codegen.** Correctness and "compiles at
  all" come first; the slow ring-3 VFS makes performance a Phase 87-class
  concern regardless.
- **A bespoke `x86_64-m3os-user.json` Rust target with `-Zbuild-std`.** The
  prebuilt-std musl target is the Phase 94 proven path and is sufficient for
  the milestone; a custom target is an orthogonal future refinement.
- **General-dynamic TLS for multiple concurrent `dlopen`'d TLS libraries.** The
  Phase 93 loader handles the main-exe local-exec TLS block; per-thread DTV
  growth for `dlopen`'d TLS `.so`s (the general-dynamic model) is deferred as
  before.
