# Phase 95b — On-Device `rustc` Code Generation: Task List

**Status:** Planned
**Source Ref:** phase-95b
**Depends on:** Phase 95 ✅ (host toolchain + on-device `pkg install rust` + the on-device-load diagnosis), Phase 93 ✅ (`libc.so` + loader TLS), Phase 76 → 76d ✅ (the `ld-musl` loader), Phase 85d ✅ (streaming exec / LLD), Phase 87 ✅ (VFS bulk-I/O), the SMP/TLB/kstack handoff `docs/handoffs/2026-06-14-claude-smp-tlb-shootdown-kstack-panic.md`
**Goal:** Land the Phase 95 milestone the host toolchain was blocked on — make the installed dynamic musl `rustc` actually **run on-device** and generate code (`rustc /usr/src/hello.rs` → `RUSTC_OK`), by reworking the `ld-musl` loader + kernel mm from a whole-file read+copy strategy to a streaming / file-backed-mmap one (so the ~162 MB `librustc_driver.so` demand-pages instead of being read+copied in full), batching SMP TLB shootdowns, and landing a targeted kernel-stack strategy. Then take the `cargo` + proc-macro stretch (the old Phase 95 Track D): `cargo build` proc-macro-free (`CARGO_OK`) and a derive-macro crate via on-device `dlopen` of the proc-macro `.so` against the Phase 93 `libc.so` (`CARGO_PROCMACRO_OK`).

> **Planning task list — acceptance items are forward-looking (`[ ]`).** Phase 95b is
> `Planned`; it starts from the diagnosed-but-blocked state Phase 95 reached (host
> build complete + sealed, `pkg install rust` works on-device, the `rustc-smoke`
> gate passes through install and fails at the `rustc --version` load). The headline
> is Areas A–D (clear the load wall + the milestone); Area E (cargo + proc-macros) is
> the stretch.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Streaming / file-backed-mmap loader (the real blocker) | 76, 93, 95 | Planned |
| B | SMP TLB-shootdown batching | A, 2026-06-14 handoff | Planned |
| C | Targeted/lazy kernel-stack strategy | 2026-06-14 handoff | Planned |
| D | On-device `RUSTC_OK` milestone — flip `rustc-smoke` to PASS | A, B, C | Planned |
| E | (Stretch) `cargo` + proc-macros via on-device `dlopen` | D, 93 | Planned |
| F | Docs, learning doc, kernel version bump (`0.95.0` → `0.95.1`) | A–E | Planned |

---

## Track A — Streaming / file-backed-mmap loader

### A.1 — Loader-only scratch elimination (no kernel change)

**File:** `userspace/ld-musl-x86_64.so.1/src/main.rs`
**Symbol:** `load_dso` / `load_dso_impl` (the per-DSO mmap + `sys_read` + `copy_nonoverlapping` path)
**Why it matters:** The loader currently double-buffers — it mmaps a full-file anonymous **scratch** region, `sys_read`s the whole file into it, mmaps a second full-image anonymous region, then `copy_nonoverlapping`s each `PT_LOAD` between them. The scratch buffer + the full-image RAM copy are pure overhead: the loader already has `lseek` (`SYS_LSEEK`) + `read`, so each `PT_LOAD` can be read directly into `load_bias + p_vaddr`. For a 162 MB DSO this drops ~162 MB of anonymous mmap and ~162 MB of intra-RAM `copy` per invocation (~halving anon footprint + copy traffic). It does not remove the eager full read (that is A.2) but is a self-contained, low-risk first cut.

**Acceptance:**
- [ ] After parsing the program headers (one small header read), each `PT_LOAD` is loaded via `lseek(fd, p_offset)` + `read` of `p_filesz` bytes directly into `load_bias + p_vaddr`; the BSS tail (`p_memsz - p_filesz`) is zeroed; the whole-file scratch mmap + the `copy_nonoverlapping` between scratch and image are gone.
- [ ] `dynamic-hello-smoke` and `dynamic-python-smoke` still PASS (the loader rework does not regress the Phase 93 dynamic C/Python path), and the staged-anon high-water for a large DSO load drops measurably.

### A.2 — Kernel lazy file-backed `mmap` + loader switch to file-backed `PT_LOAD` mapping

**Files:**
- `kernel/src/arch/x86_64/syscall/mod.rs` (`sys_mmap_file_backed`)
- `kernel/src/mm/` (the VMA representation + the page-fault demand-fill path; cf. the "File-backed mmap (eager)" note in `kernel/src/mm/pkey.rs`)
- `userspace/ld-musl-x86_64.so.1/src/main.rs` (`load_dso` — map each `PT_LOAD` file-backed)

**Symbol:** `sys_mmap_file_backed`; the page-fault handler's file-backed demand-fill case
**Why it matters:** This is the fix that unblocks the milestone. `sys_mmap_file_backed` is **eager** today — it loops over every page, allocates a frame, and reads the file content up front, so a 162 MB DSO costs 162 MB of frames + 162 MB of read regardless of how little of `rustc` a given invocation executes. Making it lazy (a VMA that records `(file, offset, len, prot)` and faults pages in from the backing file on first touch, CoW for writable `PT_LOAD`s) turns the up-front 162 MB load into a small working set — only the code pages `rustc` actually runs fault in.

**Acceptance:**
- [ ] A `MAP_PRIVATE` file-backed `mmap` installs a VMA that allocates **no** frames up front; a fault on an unbacked page reads exactly one page from the backing file (writable pages CoW on first write). `kernel/src/mm/pkey.rs`'s "File-backed mmap (eager)" note is gone.
- [ ] `load_dso` maps each `PT_LOAD` as a file-backed region (read-only segments shared, writable segments CoW) instead of read-into-anon; the BSS tail stays anonymous-zero.
- [ ] On m3OS, `rustc --version` completes in **seconds** (single-core under KVM), down from the Phase 95 25-min timeout; the resident set after `rustc --version` is a fraction of 162 MB (only touched pages faulted in), asserted by a `/proc`-style RSS check or the gate wall-clock.

---

## Track B — SMP TLB-shootdown batching

### B.1 — Batch/coalesce TLB shootdowns during bulk address-space mutation

**File:** `kernel/src/smp/tlb.rs`
**Symbol:** the shootdown-IPI broadcast path (`mm`-side `map`/`mprotect`/CoW invalidation)
**Why it matters:** Under multi-core, the loader's tens-of-thousands of `map`/`mprotect`/CoW page operations each broadcast a shootdown IPI to every other core, so a single `rustc` load pegs the idle cores (~380 % CPU at `-smp 4` in the Phase 95 diagnosis). Batching/coalescing shootdowns (one IPI per batch, or a deferred-flush window over a bulk mapping operation) bounds the multi-core amplification. This continues the Track A–D work in the 2026-06-14 SMP handoff.

**Acceptance:**
- [ ] A bulk mapping operation issues a bounded number of shootdown IPIs (batched/coalesced) rather than one per page; correctness is preserved (no stale-TLB access on any core).
- [ ] `smp-smoke` stays green, and multi-core `rustc --version` no longer pegs the idle cores for the duration of the load (CPU on non-loading cores stays near idle except during flush windows).

---

## Track C — Targeted/lazy kernel-stack strategy

### C.1 — Replace the eager 64 KiB-vs-256 KiB kstack trade-off with on-demand depth

**Files:**
- the per-task kernel-stack allocator (`KERNEL_STACK_SIZE` + slot allocation)
- `kernel/src/arch/x86_64/interrupts.rs` (the `#PF`/`#DF` recovery path — the Phase 95 backtrace diagnostic + the controlled-kill recovery, relied upon)

**Symbol:** `KERNEL_STACK_SIZE`; the kstack guard-page classifier; `try_recover_kstack_overflow`
**Why it matters:** Servicing `rustc` intermittently overflows the 64 KiB per-task kernel stack. The kernel already recovers (kills the offending task, the core returns to the scheduler — Phase 95 added the `#PF`-path diagnostic and the controlled kill), so the box survives, but a deeper kstack is needed to *not* overflow. A flat 64 → 256 KiB bump removes the overflow (proven) but costs +104 MiB eager at boot, so 95b lands a targeted fix: lazy/guard-backed kstack growth (commit pages on demand up to a cap) or a per-task-class stack size, paying for depth only where it is used.

**Acceptance:**
- [ ] The per-task kernel stack grows on demand (guard-backed commit up to a cap) or is sized per task class, so the `rustc`-servicing overflow stops occurring **without** the +104 MiB eager boot cost of a flat 256 KiB bump.
- [ ] `kstack-overflow-smoke` stays green (the controlled-kill recovery path is unchanged), and boot-time committed kstack memory is materially lower than a flat 256 KiB-per-task allocation.

---

## Track D — On-device `RUSTC_OK` milestone

### D.1 — Flip the `rustc-smoke` INSIDE-m3OS arm from blocked to PASS

**Files:**
- `xtask/src/main.rs` (`cmd_rustc_smoke` / `rustc_smoke_steps` — the `rustc --version` / `--print sysroot` / `rustc hello.rs` → `RUSTC_OK` arm)
- `userspace/ld-musl-x86_64.so.1/`, `kernel/src/mm/`, `kernel/src/smp/tlb.rs` (the Track A–C work, relied upon)

**Symbol:** `cmd_rustc_smoke`; the `RUSTC_OK hello from rustc` sentinel
**Why it matters:** This is the phase milestone — the Rust analog of Phase 85d's `CLANG_C_OK`. With Areas A–C in place the installed `rustc` loads in seconds, compiles `/usr/src/hello.rs`, links via the bundled `rust-lld` against the staged `libLLVM.so`, and the result runs and prints `RUSTC_OK`. The Phase 95 gate already wires the steps and bundles `musl.m3pkg` + `rust.m3pkg`; 95b makes the INSIDE-m3OS arm actually PASS.

**Acceptance:**
- [ ] On m3OS, `rustc --version` reports 1.96.0 and `rustc --print sysroot` resolves under `/usr` (both complete, no timeout).
- [ ] `rustc /usr/src/hello.rs -o /tmp/hello` compiles + links via the bundled `rust-lld` (no absent-`cc`/`ld` error), `/tmp/hello` prints `RUSTC_OK`, no proc-macros in the dependency graph.
- [ ] `rustc-smoke` PASSES end-to-end under `M3OS_RUST_REGRESSION=1` (skip-with-reason still applies when the host toolchain is absent).

---

## Track E — `cargo` + proc-macros (stretch)

### E.1 — Stage `cargo`; `cargo build` of a proc-macro-free crate on-device (`CARGO_OK`)

**Files:**
- `xtask/src/port_build.rs` (`build_rust` — stage `cargo` alongside `rustc`)
- a staged proc-macro-free fixture crate (e.g. `/usr/src/hello-crate/`)

**Symbol:** the staged `cargo` binary; the `CARGO_OK` serial sentinel
**Why it matters:** `cargo` is the everyday driver; a proc-macro-free `cargo build` uses only the precompiled sysroot rlibs, so it is the safe first cargo cut (no Phase 93 `dlopen` required). It is the natural step between the bare `rustc` milestone and the proc-macro wall.

**Acceptance:**
- [ ] `cargo` is staged in the `.m3pkg` (alongside `rustc`) and `cargo --version` runs on m3OS.
- [ ] `cargo build` (`--offline` / vendored) of a bundled proc-macro-free fixture crate produces a runnable binary that prints `CARGO_OK`; the build performs no network fetch.

### E.2 — Derive-macro crate via on-device `dlopen` of the proc-macro `.so` (`CARGO_PROCMACRO_OK`)

**Files:**
- a staged derive-macro fixture crate (a minimal `proc-macro` crate + a consumer)
- `userspace/ld-musl-x86_64.so.1/` (the Phase 93 loader `dlopen`/`dlsym` path — relied upon)
- `ports/lib/musl/` (the companion `libc.so` — relied upon)

**Symbol:** the Phase 93 loader `dlopen` path binding the proc-macro `.so` against `/usr/lib/libc.so`; `CARGO_PROCMACRO_OK`
**Why it matters:** A proc-macro is a `cdylib`/`dylib` `.so` that `rustc` `dlopen`s at compile time; it references libc + the Rust runtime (incl. TLS). Phase 93 landed `libc.so` + loader TLS (proven by dynamic `python3` + `ctypes.CDLL`); this is the on-device validation against a *Rust* proc-macro `.so` — the gate to the mainstream crate ecosystem.

**Acceptance:**
- [ ] `cargo build` of a crate whose dependency graph includes a derive/proc-macro crate succeeds on m3OS: `rustc` `dlopen`s the proc-macro `.so`, which binds its `malloc`/`memcpy`/TLS relocations against `/usr/lib/libc.so`, and the consumer binary runs and prints `CARGO_PROCMACRO_OK`.
- [ ] Any loader/TLS path the Rust proc-macro `.so` exercises that the Phase 93 C/Python validation did not cover is filed against `userspace/ld-musl-x86_64.so.1/` and recorded; the Area D milestone is unaffected (it is proc-macro-free).

### E.3 — `cargo-smoke` gate

**Files:**
- `xtask/src/main.rs` (`cmd_cargo_smoke` + the CLI dispatch arm + `usage()`)
- `.githooks/pre-push`; `AGENTS.md`

**Symbol:** `cmd_cargo_smoke`; `M3OS_CARGO_REGRESSION`
**Why it matters:** Validates Track E independently of `rustc-smoke` so the Area D milestone gate stays green regardless of the stretch (the same separation Phase 95 planned with the original E.3).

**Acceptance:**
- [ ] `cmd_cargo_smoke` exists with its CLI dispatch arm + `usage()` entry; opt-in via `M3OS_CARGO_REGRESSION=1`, skip-with-reason when prerequisites are absent.
- [ ] The gate asserts `cargo --version`, the proc-macro-free `cargo build` (`CARGO_OK`), and the derive-macro `cargo build` (`CARGO_PROCMACRO_OK`) on-device, at a long `--timeout` (clang-gate class).

---

## Track F — Documentation, learning doc, version bump

### F.1 — Roadmap README row + design-doc status flip

**Files:**
- `docs/roadmap/95b-on-device-rustc.md`
- `docs/roadmap/README.md` (the Phase 95b summary row + the mermaid edge)

**Symbol:** the Phase 95b summary row (Phase / Theme / Primary Outcome / Status / Source Ref / Milestone / Tasks)
**Why it matters:** Roadmap traceability; the README row is required by the doc templates, and the Status cell must reflect reality across the phase's life.

**Acceptance:**
- [ ] The roadmap README carries a Phase 95b row linking this task doc + the 95b design doc, with a mermaid edge `P95 --> P95b`.
- [ ] At landing, the 95b design-doc Status flips `Planned` → `Complete` and the README row records the `0.95.1` version.

### F.2 — Learning doc + capability bullet

**Files:**
- `docs/95b-on-device-rustc.md` (new learning doc) + `docs/README.md` + `docs/appendix/codebase-map.md` registration
- `AGENTS.md` (rewrite the Phase 95 developer-toolchain bullet to mark on-device `rustc` code generation as **landed**, not blocked)

**Symbol:** the aligned-learning-doc template; the developer-toolchain capability bullet
**Why it matters:** The learning doc teaches the streaming-loader / file-backed-mmap distinction and the proc-macro `dlopen` wall; the AGENTS.md bullet must flip from "diagnosed but blocked → 95b" to "runs on m3OS" once the milestone lands.

**Acceptance:**
- [ ] `docs/95b-on-device-rustc.md` follows the seven-section aligned-learning-doc template and is registered in `docs/README.md` + `docs/appendix/codebase-map.md`.
- [ ] The `AGENTS.md` Phase 95 capability bullet is rewritten so it no longer says the on-device code-gen milestone is blocked — `pkg install rust` then `rustc hello.rs` → `RUSTC_OK` on m3OS (one-line edit, no new bullet).

### F.3 — Kernel version bump (`0.95.0` → `0.95.1`)

**Files:**
- `kernel/Cargo.toml` (`version`), `Cargo.lock`, `AGENTS.md:7`

**Symbol:** `version = "0.95.0"`
**Why it matters:** Phase 95b lands a real on-device capability (and kernel mm/SMP/kstack changes), so it takes the next version bump on top of Phase 95's `0.95.0`.

**Acceptance:**
- [ ] `kernel/Cargo.toml` `version` is `0.95.1` (a patch bump on Phase 95's `0.95.0`; a *minor* `0.96.0` applies only if the kernel mm/SMP work is large enough to warrant it) and `Cargo.lock` matches; `cargo xtask check` clean.
- [ ] `AGENTS.md:7` "kernel **v0.95.0**" → `v0.95.1`.

---

## Documentation Notes

- **95b is the sequel to Phase 95, not a redo.** Phase 95 delivered the host
  toolchain + on-device install + the diagnosis; 95b clears the diagnosed wall and
  lands the milestone. Everything in `build_rust` / the `.m3pkg` / the packaging is
  reused unchanged.
- **The loader rework is a general capability.** Streaming / file-backed-mmap
  loading speeds every large dynamic binary (dynamic `python3`, `ctypes` `.so`s),
  not just `rustc`; it is tracked under 95b because `rustc` is what forced it.
- **The SMP + kstack tracks continue the 2026-06-14 handoff** — `smp-smoke` and
  `kstack-overflow-smoke` are the always-on regression guards.
- **Area E is the proc-macro wall** — the Rust analog of the Phase 93 `ctypes.CDLL`
  proof; the Area D milestone stays proc-macro-free by construction so it is
  independent of the stretch.
- Prefer exact symbols: `load_dso`, `sys_mmap_file_backed`, `KERNEL_STACK_SIZE`,
  `try_recover_kstack_overflow`, `cmd_rustc_smoke`, `cmd_cargo_smoke`,
  `M3OS_CARGO_REGRESSION`.
