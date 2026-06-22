# Phase 95 — Native Rust Toolchain (on-device `rustc` + `cargo`): Task List

**Status:** Planned
**Source Ref:** phase-95
**Depends on:** Phase 12 ✅ (Linux-syscall compat), Phase 44 ✅ (Rust cross-compilation lineage), Phase 76 ✅ (dynamic loader machinery), Phase 85a ✅ (`.m3pkg` substrate), Phase 85d ✅ (on-device LLVM-class delivery + streaming exec / `pread64` / `fstat`-identity kernel work + LLD), Phase 87 ✅ (VFS bulk-I/O), Phase 88 ✅ (VFS `stat` conformance), Phase 93 ✅ (`libc.so` + loader TLS — the proc-macro `dlopen` prerequisite), Phase 94 ✅ (Rust-cargo musl port class)
**Goal:** Ship a fully-static `x86_64-unknown-linux-musl` `rustc` (host-cross-built) packaged behind an `M3OS_WITH_RUST` image feature so that, on m3OS, `rustc /usr/src/hello.rs -o /tmp/hello && /tmp/hello` compiles a Rust source file to a native ELF, links it via the bundled `rust-lld`, and runs it (`RUSTC_OK`) against a prebuilt `std` sysroot resolved relative to the `rustc` binary — the Rust analog of Phase 85d's on-device Clang. The `cargo` + proc-macro half (Track D) is a stretch, now unblocked by Phase 93's `libc.so` + loader TLS, and may split into a `95b`.

> **Planning task list authored ahead of implementation.** Phase 95 is `Planned`; acceptance items below are forward-looking (`[ ]`). The Area A–C core is the milestone; Track D (cargo + proc-macros) is a stretch. The bundle/install path, kernel runtime substrate, and LLD linker are all reused from Phase 85d — this phase adds the host-side `build_rust` recipe, a userspace Rust sysroot/target, and two gates, not new kernel work for the core.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Bootstrap a fully-static musl `rustc` (host-cross-built) + prove it loads on m3OS | 85d, 94 | In progress — `build_rust` recipe landed; host build running |
| B | Userspace target spec + prebuilt `std` sysroot + bundled `rust-lld` (relocation contract) | A | In progress — stock `x86_64-unknown-linux-musl` target + bundled `rust-lld`; on-device check pending |
| C | On-device `rustc hello.rs` milestone (`RUSTC_OK`) — no proc-macros | A, B | In progress — fixture + gate landed; QEMU validation pending |
| D | (Stretch) `cargo` + proc-macros via on-device `dlopen` of the proc-macro `.so` | C, 93 | Deferred (stretch / `95b`) |
| E | Packaging (`M3OS_WITH_RUST`) + `rustc-smoke` / `cargo-smoke` gates | A–D | In progress — `M3OS_WITH_RUST` + `rustc-smoke` landed; `cargo-smoke` deferred |
| F | Documentation, learning doc, capability bullet, kernel version bump | A–E | In progress — docs + version bump underway |

> **Implementation status (live).** Host-side scaffolding for Tracks A/B/C/E is
> landed and `cargo xtask check`-green (PR #264): `ports/lang/rust/Portfile` (Rust
> 1.96.0), `build_rust` (x.py cross-build → static musl rustc + std sysroot +
> bundled `rust-lld`, reusing the Phase 85d musl libc++ sysroot), the dispatch /
> `build_recipe_id` / `BUILDABLE_PORTS` wiring, the `M3OS_WITH_RUST` image-feature
> gate, the `/usr/src/hello.rs` fixture, and `cmd_rustc_smoke` + `rustc_smoke_steps`
> + CLI dispatch + `usage()`. The heavy host build (static musl `rustc` over the
> reused LLVM-22 musl libc++ sysroot) is running; on-device `RUSTC_OK` validation
> via the `rustc-smoke` gate follows once it lands. Bootstrap decisions made:
> stock `x86_64-unknown-linux-musl` target (not a new spec); bundled `rust-lld`
> (no `DEPS=clang`); full `x.py` static-musl build (not `mrustc`).

---

## Track A — Static `rustc` bootstrap (host-cross-built)

### A.1 — Decide and de-risk the bootstrap method; pin the toolchain version

**Files:**
- `ports/lang/rust/Portfile` (new — pinned Rust version + source SHA-256)
- `docs/roadmap/95-native-rust-toolchain.md` (the *Alternative bootstrap: `mrustc`* note)

**Symbol:** `build_rust` (bootstrap-strategy comment block); `build_recipe_id("rust")`
**Why it matters:** Upstream ships a **dynamically-linked** toolchain, so producing a fully-static musl `rustc` is a deliberate multi-stage host build (the same discipline Phase 85d applied to clang). The method must be chosen and recorded before the heavy build: either rustc's `x.py`/`config.toml` bootstrap configured for an `x86_64-unknown-linux-musl` host with `crt-static`, or the smaller LLVM-free `mrustc` first cut (no proc-macro `dlopen`, reduced language coverage). Picking the wrong one wastes a multi-hour build.

**Acceptance:**
- [ ] A bootstrap-method decision is recorded as a comment in `build_rust` and reflected in the design doc: the full LLVM-based `x.py` static-musl `rustc` is the recommended path; `mrustc` is recorded as the smaller Phase-93-independent alternative (and explicitly **not** chosen for a general toolchain).
- [ ] The Portfile pins an exact Rust toolchain version (channel + version, matching the `rust-toolchain.toml` nightly pin or a chosen stable) and a source SHA-256, so the artifact's content key is stable.
- [ ] `build_recipe_id("rust")` returns a distinct, non-empty id transcribing the build's defining flags (the static/musl host triple + `crt-static` + the LLD/codegen levers), asserted by the existing `build_recipe_id_is_distinct_and_nonempty_per_host_port` test (`xtask/src/port_build.rs:7026`).

### A.2 — `build_rust()` host cross-build → static musl `rustc` + dispatch registration

**Files:**
- `xtask/src/port_build.rs` (new `build_rust`)
- `xtask/src/main.rs` (`PORTS` list, `xtask/src/main.rs:26664`)

**Symbol:** `build_rust`; the `fn port_build` `match name` arm (`xtask/src/port_build.rs:1409`)
**Why it matters:** This is the first *toolchain*-producing Rust port (Phase 94's `build_uutils` builds a Rust *program*; this builds the Rust *compiler*). Like `build_go` / `build_llvm`, it branches **before** the shared musl-gcc plumbing (`find_musl_cc` / `musl_extra_ldflags_joined`) because the Rust toolchain bootstraps with its own compiler, not `x86_64-linux-musl-gcc`. It stages a DESTDIR-style `usr/` tree that `seal_package` strips and packs.

**Acceptance:**
- [ ] `build_rust` produces a **fully-static** `x86_64-unknown-linux-musl` `rustc` binary; `file` reports `ELF 64-bit LSB executable, x86-64, ... statically linked`.
- [ ] `build_rust` is reachable from `fn port_build` via a `go`/`llvm`-style early-return branch (before the `musl_toolchain()` requirement — the toolchain self-bootstraps, no external musl-gcc), and `rust` is added to `PORTS` so `cargo xtask port list` shows it with `RECIPE=yes`.
- [ ] `build_rust` stages a DESTDIR `usr/` tree (the static `rustc` binary + the Track B sysroot + Track B linker) and routes it through `seal_package` → `strip_stage` → `pkg_format::pack`; the second build logs `PKGCACHE: hit … zero compiler invocations` (the 85a payoff on a heavy artifact).

### A.3 — Prove `rustc` loads and runs on m3OS via the streaming-exec path

**Files:**
- `xtask/src/main.rs` (the `rustc-smoke` boot/exec assertions — see E.2)
- `kernel/src/mm/elf.rs` (`load_elf_streaming` / `ElfBytes` streaming-loader machinery — relied upon, unchanged)
- `kernel/src/arch/x86_64/syscall/mod.rs` (`DiskElfSource` + `open_exec_stream` — the disk-backed `ElfBytes` source feeding the loader)

**Symbol:** `DiskElfSource`; `env!("CARGO_PKG_VERSION")`-independent `rustc --version`
**Why it matters:** A multi-tens-of-MB static `rustc` is the heaviest exec class; it must load through the **Phase 85d streaming ELF exec loader** (binaries larger than the kernel heap) without new kernel work. This is the single biggest feasibility checkpoint before investing in the sysroot/linker tracks — the Rust analog of "clang loads at all".

**Acceptance:**
- [ ] On m3OS, `rustc --version` prints the pinned toolchain version over serial (the static musl `rustc` ET_EXEC runs via the Phase 12 compat layer + Phase 85d streaming loader), asserted by the gate.
- [ ] No new kernel syscall or loader change is required to *load and run* `rustc --version` (the Phase 85d streaming exec / `pread64` / rlimit / `fstat`-identity work already covers the LLVM-class binary); any syscall gap that surfaces during bring-up is recorded and triggers a patch bump (the Phase 94 precedent), not a redesign.

---

## Track B — Sysroot + linker (relocation contract)

### B.1 — Userspace Rust target spec + prebuilt `std`/`core`/`alloc` sysroot, resolved relative to the binary

**Files:**
- `ports/lang/rust/` (the new userspace Rust target spec JSON — e.g. `x86_64-m3os-user.json`, **distinct** from the existing `x86_64-m3os.json`)
- `xtask/src/port_build.rs` (`build_rust` sysroot staging)

**Symbol:** the staged `lib/rustlib/<target>/lib/` rlib layout; `rustc --print sysroot`
**Why it matters:** `rustc` finds `std` via a sysroot laid out **relative to its own binary**, exactly like clang's resource dir (the Phase 85d relocation contract). The existing `x86_64-m3os.json` is the m3OS userspace target (hardware-float `+sse,+sse2,+aes`) but carries `code-model: kernel` and is tailored to the hand-built `no_std` ring-3 binaries, so it is unsuitable as-is as a Rust std toolchain target; a dedicated userspace Rust target spec + a prebuilt `std`/`core`/`alloc` rlib set is new work with no precedent in the tree (Go shipped no toolchain; clang ships a C/C++ sysroot, not a Rust one).

**Acceptance:**
- [ ] A userspace Rust target spec is added (hardware-float or soft-float as appropriate for ring-3, **not** `code-model: kernel`), distinct from `x86_64-m3os.json`; the chosen target (this spec, or `x86_64-unknown-linux-musl` if the milestone program targets stock musl) is documented in `build_rust` with rationale.
- [ ] A prebuilt `std`/`core`/`alloc` rlib sysroot for the milestone target is staged under `lib/rustlib/<target>/lib/` inside the `.m3pkg`, relative to the `rustc` binary.
- [ ] On m3OS, `rustc --print sysroot` resolves under `/usr` (the relocation contract honored — the `.m3pkg` is position-independent of the install root), asserted by the gate.

### B.2 — Bundle `rust-lld` (or `DEPS=clang`) so `rustc` links on-device

**Files:**
- `xtask/src/port_build.rs` (`build_rust` linker staging)
- `ports/lang/rust/Portfile` (`DEPS=` — empty if bundling `rust-lld`, or `DEPS=clang` to reuse the 85d `ld.lld`)

**Symbol:** the staged `rust-lld` driver; `rustc`'s default `linker-flavor`/`-C linker=`
**Why it matters:** `rustc` does not link — it invokes an external linker, and m3OS has no GNU `ld`. Phase 85d already proved **LLD** links real programs on m3OS (`clang -fuse-ld=lld`). This phase reuses that work rather than re-solving "no system linker": either bundle Rust's vendored `rust-lld` (rustc's default) in the `.m3pkg`, or declare `DEPS=clang` to reuse the Phase 85d `ld.lld`.

**Acceptance:**
- [ ] The chosen linker is available on-device: either `rust-lld` is staged in the `.m3pkg` and `rustc` invokes it by default, or `DEPS=clang` pulls the 85d `ld.lld` and `rustc` is configured (`-C linker-flavor`/`-C link-self-contained`) to use it. The choice is recorded in `build_rust` + the Portfile `DEPS`.
- [ ] On m3OS, `rustc` links a hello-world ELF without invoking any absent `cc`/`ld` (no `error: linker ... not found`), proven by the Track C milestone.

---

## Track C — On-device `rustc hello.rs` (the milestone)

### C.1 — `rustc /usr/src/hello.rs` compiles + links + runs on m3OS (`RUSTC_OK`)

**Files:**
- `xtask/src/main.rs` (`populate_ext2_files` — write the `/usr/src/hello.rs` fixture, mirroring the `/usr/src/hello.{c,cpp}` clang fixtures)
- `userspace/` or a staged fixture source for `hello.rs`

**Symbol:** `populate_ext2_files`; the `RUSTC_OK` serial sentinel
**Why it matters:** This is the phase milestone — the Rust analog of Phase 85d's `CLANG_C_OK`: proving the installed `rustc` generates native machine code on-device, links it via the bundled LLD, and runs the result. It is achievable **without** exercising proc-macros because the distribution sysroot's `std` is precompiled (the program links rlibs and never asks `rustc` to `dlopen` anything).

**Acceptance:**
- [ ] A `/usr/src/hello.rs` fixture (a `fn main` that prints a `RUSTC_OK` sentinel) is written into the data disk via `populate_ext2_files`, and the gate force-recreates the data disk each run.
- [ ] On m3OS: `rustc /usr/src/hello.rs -o /tmp/hello` compiles and links via the bundled LLD with no proc-macro in the dependency graph, and `/tmp/hello` runs and prints `RUSTC_OK` over serial.
- [ ] The compile uses the Track B sysroot (`rustc --print sysroot` under `/usr`) and the Track B linker (no absent-linker error).

### C.2 — Confirm the reused Phase 85d kernel substrate (no new kernel work for Area C)

**Files (relied upon, unchanged):**
- `kernel/src/mm/elf.rs` (`load_elf_streaming` / `ElfBytes` streaming ELF exec machinery)
- `kernel/src/arch/x86_64/syscall/mod.rs` (`DiskElfSource`/`open_exec_stream`, `pread64`/`pwrite64`, `getrlimit`/`prlimit64`)
- `kernel/src/fs/` (the Phase 88 `fill_stat` inode identity)

**Symbol:** `DiskElfSource`; `sys_pread64`/`sys_pwrite64`; `fill_stat`
**Why it matters:** Running a multi-tens-of-MB `rustc` reuses the exact bring-up clang forced: the streaming exec loader (binaries far larger than the kernel heap, 512 MiB cap), positional I/O (`pread64`/`pwrite64` — "THE compile blocker" for LLVM-class I/O), generous `rlimit`s (all landed in Phase 85d), and consistent `fstat` inode identity (the `st_ino=0` collapse 85d's clang surfaced, fixed systemically by `fill_stat` in Phase 88). Documenting the reuse pins the "no new kernel work expected for Area C" claim and makes any surfaced gap visible.

**Acceptance:**
- [ ] The phase introduces **no** new always-on kernel code path for Area C; the gate boots a stock kernel image and `rustc` compiles end-to-end. Any syscall gap that does surface is enumerated in the Documentation Notes + carried as a patch bump (the Phase 94 `*at`-family precedent), not a silent kernel feature.
- [ ] The "binary exceeds the kernel heap" class stays closed — the static `rustc` (and `cargo`, Track D) load via `DiskElfSource` without a heap-size regression.

---

## Track D — `cargo` + proc-macros (stretch; unblocked by Phase 93)

### D.1 — Bundle `cargo`; `cargo build` of a proc-macro-free crate on-device

**Files:**
- `xtask/src/port_build.rs` (`build_rust` — stage `cargo` alongside `rustc`)
- a staged proc-macro-free fixture crate (e.g. `/usr/src/hello-crate/`)

**Symbol:** the staged `cargo` binary; `CARGO_OK` serial sentinel
**Why it matters:** `cargo` is the everyday driver; proving a proc-macro-free `cargo build` works on-device is the natural step between the bare `rustc` milestone and the proc-macro wall. A proc-macro-free crate uses only the precompiled sysroot rlibs, so it does **not** require Phase 93 — it is the safe first cargo cut.

**Acceptance:**
- [ ] `cargo` is staged in the `.m3pkg` (alongside `rustc`) and `cargo --version` runs on m3OS.
- [ ] On m3OS, `cargo build` (or `cargo build --offline`) of a bundled **proc-macro-free** fixture crate produces a runnable binary that prints `CARGO_OK` over serial; the build performs no network fetch (offline / vendored deps).

### D.2 — Derive-macro crate compiles via on-device `dlopen` of the proc-macro `.so` (`CARGO_PROCMACRO_OK`)

**Files:**
- a staged derive-macro fixture crate (a minimal `proc-macro` crate + a consumer)
- `userspace/ld-musl-x86_64.so.1/` (the Rust loader — relied upon, Phase 93)
- `ports/lib/musl/` (the companion `libc.so` — relied upon, Phase 93)

**Symbol:** the Phase 93 loader `dlopen`/`dlsym` path binding the proc-macro `.so` against `/usr/lib/libc.so`; `CARGO_PROCMACRO_OK`
**Why it matters:** A proc-macro is a `cdylib`/`dylib` `.so` that `rustc` **`dlopen`s at compile time**; it references libc + the Rust runtime. With no `libc.so` in scope every external relocation is undefined and the load fails — the same "static-only" wall clang/Python/Go hit, harder here because derive macros are pervasive. Phase 93 landed `libc.so` + loader TLS (proven by dynamic `python3` + `ctypes.CDLL`), so this is the on-device validation against a *Rust* proc-macro `.so`.

**Acceptance:**
- [ ] On m3OS, `cargo build` of a crate whose dependency graph includes a derive/proc-macro crate succeeds: `rustc` `dlopen`s the proc-macro `.so`, which binds its `malloc`/`memcpy`/TLS relocations against the Phase 93 `/usr/lib/libc.so`, and the consumer binary runs and prints `CARGO_PROCMACRO_OK`.
- [ ] If the Rust proc-macro `.so` exercises a loader/TLS path Phase 93's C/Python validation did not cover, the gap is filed against the loader (`userspace/ld-musl-x86_64.so.1/`) and recorded; the Area C milestone is unaffected (it is proc-macro-free).

---

## Track E — Packaging + smoke gates

### E.1 — Portfile + seal `.m3pkg` + bundle behind `M3OS_WITH_RUST`

**Files:**
- `ports/lang/rust/Portfile`
- `xtask/src/main.rs` (`BUNDLE_ONLY_PORTS`, `xtask/src/main.rs:26784`; the `M3OS_WITH_RUST` image-feature gate)

**Symbol:** `BUNDLE_ONLY_PORTS`; `M3OS_WITH_RUST` (mirroring `M3OS_WITH_CLANG`)
**Why it matters:** The 200–500 MB toolchain must be **opt-in**, exactly like clang's ~125 MB artifact behind `M3OS_WITH_CLANG`. Default images must omit it; an opt-in image bundles `rust.m3pkg` into `/usr/pkg/` for `pkg install rust`.

**Acceptance:**
- [ ] `cargo xtask port build rust` seals a valid `rust.m3pkg` (`pkg_format::verify` passes on the read-back bytes — the `BUNDLE_ONLY_PORTS` verify-before-bundle guard).
- [ ] `rust` is bundled into `/usr/pkg/` **only** when the opt-in `M3OS_WITH_RUST` feature is set (mirroring the `M3OS_WITH_CLANG` block in `xtask/src/main.rs`); default images omit `rust.m3pkg` (debugfs-verifiable) and the disk delta is documented.
- [ ] On m3OS, `pkg install rust` materializes `rustc` (+ `cargo`, Track D) + the sysroot + the linker into the install root and `rustc --version` runs.

### E.2 — `rustc-smoke` gate (Area C, opt-in)

**Files:**
- `xtask/src/main.rs` (`cmd_rustc_smoke` + the `Some("rustc-smoke")` CLI dispatch arm + the `usage()` entry)
- `.githooks/pre-push` (the `M3OS_RUST_REGRESSION` guarded block)
- `AGENTS.md` (the opt-in gate row)

**Symbol:** `cmd_rustc_smoke`; `M3OS_RUST_REGRESSION`
**Why it matters:** Validates the Area C milestone end-to-end (install → `rustc --version` → `--print sysroot` under `/usr` → `rustc hello.rs` → run → `RUSTC_OK`), mirroring `clang-smoke` / `git-https-smoke`. Without an always-falsifiable gate the milestone could regress unnoticed.

**Acceptance:**
- [ ] The gate is invokable: `cmd_rustc_smoke` exists, a `Some("rustc-smoke") => …` arm is added to the top-level CLI `match`, and `rustc-smoke` is listed in `usage()` — all three, mirroring `clang-smoke`.
- [ ] The gate builds the `M3OS_WITH_RUST` image, boots m3OS, `pkg install rust`, asserts `rustc --version` + `rustc --print sysroot` under `/usr`, compiles + links + runs `/usr/src/hello.rs` (`RUSTC_OK`), and **fails** on a missing sentinel.
- [ ] The gate is opt-in via `M3OS_RUST_REGRESSION=1` (a guarded block in `.githooks/pre-push` + an `AGENTS.md` row), **skips-with-reason** when the host Rust toolchain (or the prebuilt-std musl target) is absent, and runs at a long `--timeout` (the multi-hundred-MB install + cold rustc load over the slow ring-3 VFS — clang-gate class, e.g. `5400`).

### E.3 — `cargo-smoke` gate (Track D stretch, opt-in)

**Files:**
- `xtask/src/main.rs` (`cmd_cargo_smoke` + dispatch + `usage()`)
- `.githooks/pre-push`; `AGENTS.md`

**Symbol:** `cmd_cargo_smoke`; `M3OS_CARGO_REGRESSION`
**Why it matters:** Validates Track D — proc-macro-free `cargo build` (`CARGO_OK`) and, when Phase 93's loader path covers it, the derive-macro `dlopen` (`CARGO_PROCMACRO_OK`). Kept separate from `rustc-smoke` so the core milestone gate stays green independently of the stretch.

**Acceptance:**
- [ ] `cmd_cargo_smoke` exists with its CLI dispatch arm + `usage()` entry; opt-in via `M3OS_CARGO_REGRESSION=1`, skip-with-reason when prerequisites are absent.
- [ ] The gate asserts `cargo --version`, a proc-macro-free `cargo build` (`CARGO_OK`), and the derive-macro `cargo build` (`CARGO_PROCMACRO_OK`) on-device; it may ship after `rustc-smoke` (a `95b` cut) without blocking the Area C milestone.

---

## Track F — Documentation, learning doc, version bump

### F.1 — Design doc conformance + roadmap README row + status flip + Tasks-cell link

**Files:**
- `docs/roadmap/95-native-rust-toolchain.md`
- `docs/roadmap/README.md` (the Phase 95 summary row, `docs/roadmap/README.md:481`)

**Symbol:** the Phase 95 summary row (Phase / Theme / Primary Outcome / Status / Source Ref / Milestone / Tasks)
**Why it matters:** Roadmap traceability; the README row is required by the doc templates, the design doc must carry every template section (including the `Learning Documentation Requirement` + `Related Documentation and Version Updates` sections the Phase 94 design doc established), and the Status cell + Tasks link must reflect reality across the phase's life.

**Acceptance:**
- [ ] The design doc conforms to the phase-design template (all sections populated, including `Learning Documentation Requirement` + `Related Documentation and Version Updates`), the `Companion Task List` links this task doc, and the stale `post-91` / pre-Phase-93 framing is corrected (Phase 93/94 marked ✅).
- [ ] The roadmap README Phase 95 row's **Tasks** cell links `./tasks/95-native-rust-toolchain-tasks.md` (replacing the "Deferred until implementation planning" placeholder); Theme / Primary Outcome / Source Ref / Milestone are present.
- [ ] At landing, the roadmap README Phase 95 row Status is flipped `Planned` → `Complete` (and records the `0.95.0` version).

### F.2 — Capability-bullet decision in `AGENTS.md`

**File:** `AGENTS.md`
**Symbol:** the developer-toolchain capability bullet (the Package-management / cross-compiled-toolchain inventory line)
**Why it matters:** The maintenance policy permits a capability-inventory edit only for a **new capability class**. On-device `rustc` is a new class — the project's first native Rust *code generator* (distinct from the Phase 85d on-device C/C++ generator) — so a minimal one-line edit is warranted; per "keep it small", rewrite an existing bullet rather than adding a new one.

**Acceptance:**
- [ ] A **one-line** addition/rewrite of the existing developer-toolchain capability bullet names on-device `rustc` (fully-static musl, `M3OS_WITH_RUST`, `pkg install rust`, `rustc hello.rs` → native ELF via bundled `rust-lld`, proc-macro half gated on Phase 93). No new bullet, no reflow.

### F.3 — Create the Phase 95 learning doc + register it

**Files:**
- `docs/95-native-rust-toolchain.md` (new)
- `docs/README.md` (the Phase-Aligned Learning Docs table, after the Phase 94 row at `docs/README.md:79`)
- `docs/appendix/codebase-map.md` (the Documentation Index table, after the Phase 94 row at `docs/appendix/codebase-map.md:178`)

**Symbol:** the *aligned legacy learning doc* template (`docs/appendix/doc-templates.md`) — fields: Aligned Roadmap Phase (95) / Status / Source Ref (`phase-95`) / Supersedes Legacy Doc (N/A) / Overview / What This Doc Covers / Core Implementation / Key Files / How This Phase Differs From Later Work / Related Roadmap Docs / Deferred or Later-Phase Topics
**Why it matters:** The design doc's *Learning Documentation Requirement* mandates it (mirroring Phase 94 task E.3 / Phase 93 task F.1). It teaches the *running a program* vs *running the compiler* distinction, why an LLVM-based compiler is the heaviest on-device class, the sysroot relocation contract, the reused 85d LLD, and the proc-macro `dlopen`/`libc.so` wall — the pedagogical companion to the implementation-focused design doc.

**Acceptance:**
- [ ] `docs/95-native-rust-toolchain.md` exists and follows the seven-section aligned-learning-doc template (Overview / What This Doc Covers / Core Implementation / Key Files / How This Phase Differs From Later Work / Related Roadmap Docs / Deferred or Later-Phase Topics; header carries Aligned Roadmap Phase 95 / Status / Source Ref `phase-95`).
- [ ] It is linked from the `docs/README.md` Phase-Aligned Learning Docs table in the verbatim row format, in phase order after the Phase 94 row.
- [ ] It is registered in the `docs/appendix/codebase-map.md` Documentation Index after the Phase 94 row (the "Before touching …" guidance cites `build_rust`, the userspace Rust target/sysroot, and the bundled `rust-lld`).

### F.4 — Bump the kernel version (`0.94.1` → `0.95.0`)

**Files:**
- `kernel/Cargo.toml` (the `version` field)
- `Cargo.lock` (the kernel package version)
- `AGENTS.md` (the "kernel **v0.94.1**" reference in the Project Overview, `AGENTS.md:7`)

**Symbol:** `version = "0.94.1"`
**Why it matters:** Every phase lands with an **unconditional** kernel version bump — the design doc's Implementation Outline (step 7) + *Related Documentation and Version Updates* call for it, and the `AGENTS.md` maintenance policy explicitly permits bumping the version line when a phase lands (mirrors Phase 94 task E.4). No kernel *code* change is expected for Area C — the banner (`kernel/src/lib.rs`), `/proc/version` (`kernel/src/fs/procfs.rs`), and `uname` utsname (`kernel/src/arch/x86_64/syscall/mod.rs`) all derive from `env!("CARGO_PKG_VERSION")`, so the single `Cargo.toml` edit propagates everywhere.

**Acceptance:**
- [ ] `kernel/Cargo.toml` `version` is `0.95.0` (the standard minor bump `0.94.1` → `0.95.0`; a further **patch** bump applies only if a syscall gap surfaces during bring-up — the Phase 94 precedent) and `Cargo.lock` matches; `cargo xtask check` is clean.
- [ ] The `AGENTS.md` "kernel **v0.94.1**" reference (`AGENTS.md:7`) is updated to `v0.95.0`.
- [ ] No other source edit is needed for the version string (the three derived sites pick it up from `CARGO_PKG_VERSION`); prior-phase `0.94.x` mentions in `docs/roadmap/` are historical and left unchanged.

---

## Documentation Notes

- **Precedent is Phase 85d, not Phase 86d.** Phase 86d *runs a pre-built Go binary* — its compiler never runs on-device. This phase runs the `rustc` *compiler* and generates machine code on m3OS, which is the Phase 85d (on-device Clang) problem class. m3OS userspace has run native Rust *binaries* since Phase 5; the new thing is the *toolchain*.
- **No new kernel work is expected for the Area C core.** The streaming ELF exec loader (`DiskElfSource`, >heap binaries), positional `pread64`/`pwrite64`, generous `rlimit`s, and the Phase 88 `fill_stat` inode identity all landed for clang (85d) and are reused unchanged. Any syscall gap that surfaces during bring-up is enumerated here and carried as a patch bump (the Phase 94 `unlinkat`/`fchmodat`/`fchownat`/`mkdirat` precedent), never a silent kernel change.
- **The bootstrap is a deliberate static build.** Upstream ships a *dynamic* toolchain; m3OS builds `rustc`/`cargo` fully static (the same discipline as clang/python/go), even though Phase 93's `libc.so` now exists — the toolchain *binary* stays static while proc-macro `.so`s loaded *by* `rustc` bind against `libc.so`.
- **The userspace target/sysroot is new.** `x86_64-m3os.json` is the existing userspace target (hardware-float `+sse,+sse2,+aes`) but carries `code-model: kernel` and is tailored to the hand-built `no_std` ring-3 binaries, so it is unsuitable as-is as a Rust std toolchain target; a dedicated userspace Rust target spec + a prebuilt `std`/`core`/`alloc` rlib sysroot resolved relative to the `rustc` binary has no precedent in the tree (Go shipped no toolchain; clang ships a C/C++ sysroot, not a Rust one).
- **The linker is reused.** Phase 85d already proved LLD links on m3OS; this phase bundles `rust-lld` or `DEPS=clang`s the 85d `ld.lld` — the "no system linker" problem is solved upstream of this phase.
- **Proc-macros are the wall — now cleared by Phase 93.** A proc-macro `.so` is `dlopen`'d at compile time and references libc + Rust-runtime symbols (incl. TLS). The dynamic-linker machinery has existed since Phase 76; the missing `libc.so` + loader TLS landed in Phase 93 (proven by dynamic `python3` + `ctypes.CDLL`). Track D validates that path against a *Rust* proc-macro `.so`; the Area C milestone stays proc-macro-free by construction (precompiled `std` rlibs).
- **`cargo` registry + `build.rs`/`cc`-crates are deferred.** crates.io HTTPS fetch (wire cargo to the Phase 86c TLS stack) and `build.rs` with `cc`-built C dependencies (on-device clang invocation, the Phase 85d toolchain) are explicitly out of scope; Track D builds offline / vendored.
- **Sizing.** The toolchain is opt-in (`M3OS_WITH_RUST`) exactly like clang — a 200–500 MB artifact must not bloat default images. The 85a content-addressed cache makes repeat builds free (zero compiler invocations on a warm second build).
- **`mrustc` alternative.** A C++ Rust-subset compiler that emits C and needs no LLVM (and sidesteps proc-macro `dlopen` for its bootstrap subset) is recorded as a smaller, Phase-93-independent first cut — an option, not the recommended path for a general toolchain.
- **Prefer exact symbols:** `build_rust`, `build_recipe_id`, `port_build`, `PORTS`, `BUNDLE_ONLY_PORTS`, `M3OS_WITH_RUST`, `cmd_rustc_smoke`, `cmd_cargo_smoke`, `populate_ext2_files`, `DiskElfSource`, `seal_package`, `strip_stage`, `pkg_format::{pack,verify}`.
