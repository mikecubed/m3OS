# Phase 95b - On-Device `rustc` Code Generation (Native Rust toolchain, part B)

**Status:** Partial — crash chain FIXED, **rustc EXECUTES on-device** (`--version`→1.96.0, `--print sysroot`→/usr); the `RUSTC_OK` milestone now gates on the `rustc hello.rs` multithreaded-compile stall (scheduler/futex), NOT VFS throughput. ➜ Plan: [`docs/handoffs/2026-06-24-phase-95-completion-plan.md`](../handoffs/2026-06-24-phase-95-completion-plan.md). (The "Outcome" below is the pre-page-table-fix record.)
**Source Ref:** phase-95b
**Depends on:** Phase 95 ✅ (host toolchain + on-device `pkg install rust` + the precise on-device-load diagnosis), Phase 93 ✅ (`libc.so` + loader TLS — the dynamic `rustc` interpreter and the proc-macro `dlopen` target), Phase 76 → 76d ✅ (the from-scratch `ld-musl` dynamic loader 95b reworks), Phase 85d ✅ (streaming ELF exec / `pread64` / LLD), Phase 87 ✅ (VFS bulk-I/O), the SMP/TLB/kstack handoff [`docs/handoffs/2026-06-14-claude-smp-tlb-shootdown-kstack-panic.md`](../handoffs/2026-06-14-claude-smp-tlb-shootdown-kstack-panic.md)
**Builds on:** Phase 95 cross-built a **dynamic** musl `rustc` 1.96.0 (+ prebuilt `std` sysroot + bundled `rust-lld`), packaged it behind `M3OS_WITH_RUST`, and proved `pkg install rust` works on-device — but the on-device **code-generation milestone** (`rustc hello.rs` → `RUSTC_OK`) hit a wall: loading the ~162 MB dynamic `librustc_driver.so` through the loader's whole-file read+copy strategy is CPU-bound and times out. Phase 95b clears that wall and lands the milestone, then takes the `cargo` + proc-macro stretch (Phase 95's old Track D).
**Primary Components:** `userspace/ld-musl-x86_64.so.1/src/main.rs` (the per-DSO load path), `kernel/src/arch/x86_64/syscall/mod.rs` (`sys_mmap_file_backed`), `kernel/src/mm/` (file-backed demand-fault VMA backing), `kernel/src/smp/tlb.rs` (shootdown batching), the kernel-stack allocator + the `#PF`/`#DF` recovery path, `xtask/src/main.rs` (`cmd_rustc_smoke` `RUSTC_OK` arm, `cmd_cargo_smoke`), `xtask/src/port_build.rs` (`build_rust` — stage `cargo`)

## Outcome (this pass)

> **➜ SUPERSEDED IN PART (2026-06-24).** This Outcome predates the page-table fix
> (`841fd53f`) and the KVM measurements. Corrections: rustc **does** now run userspace
> (`--version`/`--print sysroot` pass); the "rustc never runs userspace / VFS throughput is
> the binding constraint / ~40-min install" diagnosis below was a **pre-fix, TCG** picture
> (under KVM the install is ~25 s). The real `RUSTC_OK` blocker is the `rustc hello.rs`
> compile-thread stall. See the
> [completion plan](../handoffs/2026-06-24-phase-95-completion-plan.md).

**The infrastructure landed; the milestone moved but is still blocked.**

- **Area A landed & validated.** The `ld-musl` loader + kernel mm were reworked from
  whole-file read+copy to **streaming / demand-paged file-backed** loading: a new
  opt-in `MAP_LAZY_FILE` flag installs a frameless file-backed VMA, and the page-fault
  handler demand-fills a faulting page straight from the backing file — including the
  ring-3 `vfs_server` case via a **blocking IPC issued from the page-fault handler**
  (the kernel's first). `dynamic-hello-smoke` PASSES (`DYNAMIC_HELLO:ok` + `DLOPEN:ok`),
  proving the loader + the novel fault-context read end-to-end. A 64 KiB **readahead**
  cluster amortises the per-page reads. **This cleared the Phase 95 wall** (the eager
  162 MB read+copy that was CPU/IPC-bound).
- **Area B landed.** Demand-fault page commits (lazy-file, anon, stack/brk) now issue
  **zero** cross-core TLB-shootdown IPIs — a not-present → present transition needs no
  invalidation. `smp-smoke` is the guard.
- **Area C is unnecessary.** The kstack overflow was a symptom of the eager-read chain;
  A.2 removed it, and rustc no longer overflows the 64 KiB kstack.
- **Area D (the `RUSTC_OK` milestone) is STILL BLOCKED — re-diagnosed.** With the eager
  load gone, `rustc --version` now blocks for a **different, deeper** reason:
  instrumentation (a timer-ISR userspace-RIP sampler + a demand-fill page counter +
  a syscall sampler, on SMP=1 and SMP=4 under KVM) showed **rustc never runs userspace**
  (zero RIP samples), **demand-pages < 1 MB** of `librustc_driver.so`, and is **blocked
  in the kernel** — the loader loads the small `libc.so` fine but **wedges/never loads
  the 162 MB `librustc_driver.so`**. Further tracing pointed *upstream* of A.2: the
  dominant cost is the ~368 MB `pkg install rust` over the **~100–200 KB/s ring-3 VFS**
  (≈40 min of I/O, at/over the 50-min install-step timeout — so the on-device rustc is
  likely never properly installed). The binding constraint is **VFS / block-I/O
  throughput**, which A.2's demand-side laziness cannot fix. **The `RUSTC_OK` milestone
  is carried into [Phase 95c](./95c-vfs-block-io-perf.md)** — the supply-side
  VFS/block-I/O performance subphase whose explicit goal is to flip this `rustc-smoke`
  arm to PASS and close the 95-series.
- **Area E (cargo + proc-macros) was not started** (gated behind D).

The remainder of this doc describes the originally-planned design; the
[task list](./tasks/95b-on-device-rustc-tasks.md) has the per-track outcomes.

## Milestone Goal

A native `rustc` **runs on m3OS** and compiles a Rust source file to a working
native executable that also runs on m3OS — `rustc /usr/src/hello.rs -o /tmp/hello
&& /tmp/hello` prints `RUSTC_OK`, via the bundled `rust-lld` against the prebuilt
`std` sysroot — turning the always-asserted-install `rustc-smoke` gate into a full
PASS. m3OS becomes self-hosting for Rust the way Phase 85d made it self-hosting for
C/C++. The stretch adds `cargo build` of a proc-macro-free crate (`CARGO_OK`) and,
via on-device `dlopen` of a proc-macro `.so` against the Phase 93 `libc.so`, a
derive-macro crate (`CARGO_PROCMACRO_OK`).

## Why This Phase Exists

Phase 95 delivered everything *except the milestone*: the host toolchain builds and
seals, and `pkg install rust` materializes `rustc` + the sysroot + `rust-lld` on the
device. But the first time the installed `rustc` is *run*, it never finishes. Phase
95 diagnosed this thoroughly (`rustc --version` was driven to failure under
single-core and multi-core, KVM and TCG):

- **The real blocker is a CPU-bound load of the ~162 MB `librustc_driver.so`.**
  `rustc` is a ~5 KB launcher that `DT_NEEDED`s `librustc_driver.so` (~162 MB, with
  LLVM **statically linked in**) + `libc.so`. The Phase 76/93 `ld-musl` loader's
  per-DSO strategy is "mmap a file-sized anonymous **scratch** → `sys_read` the
  whole file → mmap a second file-sized anonymous **image** → `copy` each
  `PT_LOAD` → relocate", i.e. ~324 MB of anonymous mmap + ~162 MB read + ~162 MB
  intra-RAM copy + reloc *per invocation* on a 2 GiB guest — hundreds of CPU-
  seconds, single-core under KVM. It is CPU-bound (qemu busy, not idle), not a
  deadlock, not a VFS-latency stall, not the symbol resolver (gnu-hash is O(1)).
  The dynamic-`python3` Phase 93 path works because those `.so`s are tens of MB,
  not 162 MB.
- **Multi-core makes it worse:** the loader's tens-of-thousands of
  `map`/`mprotect`/CoW page operations each broadcast a TLB-shootdown IPI, pegging
  the other cores (~380 % CPU at `-smp 4`).
- **An intermittent 64 KiB per-task kernel-stack overflow** surfaces while
  servicing rustc (the kernel recovers — kills the task — thanks to the Phase 95
  `#PF`-path diagnostic + the controlled-kill recovery, but a 256 KiB eager bump
  costs +104 MiB at boot, so the right fix is targeted/lazy kstacks).

None of these are reachable without first building and installing a real on-device
`rustc`, which Phase 95 did. They are the cost of running the **heaviest, and first
*dynamic + multi-threaded*, on-device program** — exactly the kind of bring-up the
Phase 90b Claude-Code integration surfaced for the kernel. Splitting them into 95b
keeps the Phase 95 host-toolchain deliverable mergeable while the deep kernel/loader
work proceeds on its own branch.

## Learning Goals

- Why a **whole-file read+copy** dynamic loader is O(file size) in both memory and
  CPU, and why that is fine for tens-of-MB `.so`s but fatal for a 162 MB one.
- The difference between **eager** file-backed `mmap` (allocate every page + read
  the file up front) and **demand-paged / file-backed** `mmap` (map a VMA whose
  pages fault in from the file on first touch), and why only the latter makes a
  162 MB code object load in proportion to the pages actually executed.
- How a streaming loader composes with the kernel: a file-backed VMA + a
  demand-fault handler that reads the backing file page-by-page, vs. the loader
  doing all I/O itself.
- Why bulk address-space mutation (a loader mapping thousands of segments/pages)
  amplifies under SMP via **TLB-shootdown IPIs**, and how batching/coalescing
  shootdowns bounds the cost.
- Kernel-stack sizing trade-offs: a flat large per-task stack (simple, wasteful)
  vs. **guard-backed lazy growth** (pay for depth only when used).
- Why **proc-macros** are the wall to the mainstream crate ecosystem, and how a
  proc-macro `.so` `dlopen`'d by `rustc` binds `malloc`/`memcpy`/TLS relocations
  against the Phase 93 `/usr/lib/libc.so`.

## Feature Scope

### Area A — Streaming / file-backed-mmap loader (the real blocker)

Two sub-parts, smallest-first:

- **A loader-only partial win (no kernel change).** Today `load_dso` double-buffers:
  a full-file anonymous **scratch** region *and* a full-image anonymous region, then
  `copy_nonoverlapping`s each `PT_LOAD` between them. The scratch is unnecessary —
  the loader already has `lseek` + `read`, so it can `lseek(fd, p_offset)` + read
  `p_filesz` bytes **directly into** `load_bias + p_vaddr` for each `PT_LOAD`,
  eliminating the ~162 MB scratch mmap and the ~162 MB intra-RAM copy (~halving the
  per-DSO anonymous footprint and copy traffic). This does **not** remove the eager
  full read, so by itself it does not unblock the milestone — but it is a real,
  self-contained, low-risk reduction that lands first.
- **Kernel demand-fault-from-file VMA backing (the fix that unblocks the milestone).**
  `sys_mmap_file_backed` is **eager** today (it loops over every page, allocates a
  frame, and reads the file content up front — `kernel/src/mm/pkey.rs` labels it
  "File-backed mmap (eager)"). 95b makes it lazy: a `MAP_PRIVATE` file-backed `mmap`
  installs a VMA that records `(file, offset, len, prot)` but allocates no frames;
  the page-fault handler demand-fills a faulting page by reading the one page from
  the backing file (CoW-on-write for writable `PT_LOAD`s). The loader then maps each
  `PT_LOAD` as a file-backed region instead of read-into-anon, so **only the code
  pages rustc actually executes fault in** — turning a 162 MB up-front load into a
  small working set.

### Area B — SMP TLB-shootdown batching

Under multi-core, the loader's bulk `map`/`mprotect`/CoW operations each broadcast a
shootdown IPI to every other core, so a single `rustc` load pegs the idle cores. 95b
batches/coalesces shootdowns during bulk address-space mutation (a single IPI per
batch, or a deferred-flush window), bounding the multi-core amplification. This is
the continuation of the Track A–D work in the
[2026-06-14 SMP handoff](../handoffs/2026-06-14-claude-smp-tlb-shootdown-kstack-panic.md);
the always-on `smp-smoke` gate is the regression guard.

### Area C — Kernel-stack strategy

The 64 KiB per-task kernel stack intermittently overflows servicing rustc. The
Phase 95 `#PF`-path diagnostic + the controlled-kill recovery already turn the
overflow into a clean task kill (the box survives), and a 64 → 256 KiB bump removes
the overflow outright — but at +104 MiB eager at boot. 95b lands the *targeted* fix:
either lazy/guard-backed kstack growth (commit pages on demand up to a cap) or a
per-task-class stack size, so depth is paid for only where it is used. The
`kstack-overflow-smoke` gate covers the recovery path either way.

### Area D — The on-device `RUSTC_OK` milestone

With Areas A–C in place, `rustc --version` loads in seconds, `rustc --print sysroot`
resolves under `/usr`, and `rustc /usr/src/hello.rs -o /tmp/hello` compiles + links
(via the bundled `rust-lld` against the staged `libLLVM.so`) + runs (`RUSTC_OK`),
no proc-macros in the dependency graph. The `rustc-smoke` gate's currently-blocked
INSIDE-m3OS arm flips from "fails at the `rustc --version` load" to a full PASS.

### Area E — `cargo` + proc-macros (moved from Phase 95 Track D)

Bundle `cargo` alongside `rustc`; prove `cargo build` (offline / vendored) of a
proc-macro-free fixture crate produces a runnable binary (`CARGO_OK`). Then a
derive-macro crate: `rustc` `dlopen`s the proc-macro `.so`, which binds its
`malloc`/`memcpy`/TLS relocations against the Phase 93 `/usr/lib/libc.so`, and the
consumer binary runs (`CARGO_PROCMACRO_OK`). A new `cargo-smoke` gate
(`M3OS_CARGO_REGRESSION`) validates both, kept separate from `rustc-smoke` so the
Area D milestone gate stays independent of the stretch.

## Important Components and How They Work

### `load_dso` in `userspace/ld-musl-x86_64.so.1/src/main.rs`

The per-DSO load routine. Phase 95b removes the anonymous scratch buffer (Area A,
part 1) and, once the kernel supports it, switches each `PT_LOAD` from
`mmap-anon` + `read` + `copy` to a file-backed `mmap` (Area A, part 2), so segment
content demand-faults from the file rather than being read+copied in full.

### `sys_mmap_file_backed` + the page-fault handler (`kernel/src/`)

Today eager. 95b adds a lazy file-backed VMA type and a demand-fill page-fault path
that reads one page from the backing file on first touch (and CoW for writable
segments). This is the kernel half of the streaming loader and the dominant
performance win.

### `kernel/src/smp/tlb.rs`

The TLB-shootdown path. 95b batches shootdowns during bulk mapping so a loader that
maps thousands of pages issues a bounded number of IPIs instead of one per page.

### The kernel-stack allocator + `#PF`/`#DF` recovery

The recovery (kill the offending task, core returns to the scheduler) is already
in place (Phase 95 added the `#PF`-path backtrace diagnostic). 95b changes the
*sizing* policy to lazy/targeted so the overflow stops happening without the
+104 MiB eager cost.

## How This Builds on Earlier Phases

- **Extends Phase 95** by clearing the on-device-load wall its host toolchain hit;
  the `.m3pkg`, the `M3OS_WITH_RUST` packaging, the `rustc-smoke` scaffold, and the
  `DEPS=musl`/`libLLVM.so` packaging fixes are all reused unchanged.
- **Reworks the Phase 76 → 76d `ld-musl` loader** from whole-file read+copy to
  streaming/file-backed mapping — a general capability that also speeds every other
  large dynamic binary (dynamic `python3`, `ctypes` `.so`s).
- **Continues the Phase 93** `libc.so` + loader-TLS line: the dynamic `rustc` *is*
  the heaviest consumer of it, and Area E's proc-macro `dlopen` is the Rust analog
  of the Phase 93 `ctypes.CDLL` proof.
- **Continues the 2026-06-14 SMP/TLB/kstack handoff** (Areas B and C).

## Implementation Outline

1. **Loader partial win (Area A.1).** `lseek`+`read` each `PT_LOAD` directly into
   the image; drop the scratch buffer + full-image copy. Re-run `dynamic-hello` /
   `dynamic-python` smokes (no regression).
2. **Kernel lazy file-backed mmap (Area A.2).** Add the file-backed VMA + demand-
   fill page-fault path; switch `load_dso` to file-backed `PT_LOAD` mapping. Measure
   `rustc --version` wall-clock single-core.
3. **SMP shootdown batching (Area B).** Batch/coalesce shootdowns during bulk
   mapping; re-run `smp-smoke`.
4. **Kstack strategy (Area C).** Lazy/targeted kstacks; re-run
   `kstack-overflow-smoke`.
5. **Milestone (Area D).** Flip the `rustc-smoke` INSIDE-m3OS arm to PASS
   (`rustc --version` → `--print sysroot` → `rustc hello.rs` → `RUSTC_OK`).
6. **cargo + proc-macros (Area E).** Stage `cargo`; `cargo-smoke` with `CARGO_OK` +
   `CARGO_PROCMACRO_OK`.
7. **Document + version.** Learning-doc update, README row flip, bump
   `kernel/Cargo.toml` `0.95.0` → `0.95.1` on landing.

## Acceptance Criteria

- On m3OS (behind `M3OS_WITH_RUST`), `rustc --version` completes in seconds (not the
  Phase 95 25-min timeout), single-core under KVM and on the default `-smp` count.
- `rustc /usr/src/hello.rs -o /tmp/hello` compiles + links via the bundled
  `rust-lld` (against the staged `libLLVM.so`) and `/tmp/hello` prints `RUSTC_OK`,
  with no proc-macros in the dependency graph.
- `rustc-smoke` PASSES end-to-end (the INSIDE-m3OS arm, not just install) under
  `M3OS_RUST_REGRESSION=1`.
- `smp-smoke` and `kstack-overflow-smoke` stay green with the batching + kstack
  changes; `dynamic-hello-smoke` / `dynamic-python-smoke` stay green with the loader
  rework.
- **Area E (stretch):** `cargo build` of a proc-macro-free crate prints `CARGO_OK`,
  and a derive-macro crate prints `CARGO_PROCMACRO_OK` via on-device `dlopen` of the
  proc-macro `.so` against `/usr/lib/libc.so`; `cargo-smoke` PASSES under
  `M3OS_CARGO_REGRESSION=1`.
- `kernel/Cargo.toml` `version` is `0.95.1` and `AGENTS.md` matches.

## Companion Task List

- [Phase 95b Task List](./tasks/95b-on-device-rustc-tasks.md)

## How Real OS Implementations Differ

- Real loaders (glibc/musl `ld.so`) `mmap` shared objects **file-backed** so segments
  demand-page from the file by default — there is no whole-file read+copy. 95b brings
  m3OS's from-scratch loader to that baseline for large objects.
- Real kernels demand-fault file-backed mappings from the page cache; m3OS reads the
  page from its VFS on fault (no unified page cache), so the working-set win is the
  same shape but the I/O path is the ring-3 VFS.
- Production `cargo` does network dependency resolution, `build.rs`/`cc`-crate
  execution, incremental caching, and parallel codegen; 95b targets offline
  proc-macro-free + a single derive-macro proof, not feature parity.

## Deferred Until Later

- **crates.io registry access** (wire `cargo`'s HTTPS fetch to the Phase 86c TLS
  stack) and **`build.rs` with `cc`-crates** (on-device clang invocation).
- **A self-hosting `rustc` bootstrap on-device** (building `rustc` *on* m3OS) — the
  analog of clang self-hosting, itself still deferred in Phase 85d.
- **Incremental compilation, parallel codegen, and performance** beyond "it compiles
  in a reasonable time".
- **A unified page cache** (95b reads fault pages straight from the VFS; a shared
  cache across mappings is a broader mm refactor).
- **A bespoke `x86_64-m3os-user.json` Rust target with `-Zbuild-std`** (95b keeps
  stock `x86_64-unknown-linux-musl`).
