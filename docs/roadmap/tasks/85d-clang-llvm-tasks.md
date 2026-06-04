# Phase 85d — Clang/LLVM/LLD (+ Release): Task List

**Status:** Planned (authored ahead of implementation)
**Source Ref:** phase-85d
**Depends on:** Phase 85a (Package & Build-Cache Infrastructure); 85b + 85c land first
**Goal:** Host-cross-build a static Clang + LLD (X86-only, `MinSizeRel`), package it via the Phase 85a `.m3pkg` substrate behind an opt-in image feature, install it with `pkg install clang`, validate C/C++ sample builds inside m3OS — and cut the umbrella learning doc + capability inventory + README finalization that close out the Phase 85 family.

> **Planning task list authored ahead of implementation.** All acceptance items are intentionally **unchecked `[ ]`**. This is the "+ Release" sub-phase. Builds on the 85a substrate; lands last because its heavyweight artifact (multi-GB-RAM, multi-hour build; several-hundred-MB install) is exactly what the 85a content-addressed cache exists to make affordable.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Clang + LLD CMake cross-build + sysroot + C++ runtime + `clang++` | 85a | Planned |
| B | Opt-in packaging + install + validation gate | A, 85a | Planned |
| C | Release closeout — umbrella learning doc, capability bullet, README, version bump | A, B | Planned |

---

## Track A — Clang + LLD cross-build

### A.1 — Add the LLVM Portfile + `build_llvm`

**Files:**
- `ports/lang/llvm/Portfile` (new — pinned LLVM version + SHA-256)
- `xtask/src/port_build.rs` (new `build_llvm` CMake-template port, registered in `PORTS` + dispatch)

**Symbol:** `build_llvm`
**Why it matters:** Clang/LLD is the largest artifact and the one whose rebuild cost justifies all of 85a; building it via the standard port path + 85a cache is what makes repeat image builds free.

**Acceptance:**
- [ ] `build_llvm` runs a CMake cross-build via `musl_toolchain()` for the C/C++ compilers, `CMAKE_SYSROOT` at the m3OS musl sysroot, `LLVM_HOST_TRIPLE` for the m3OS target, `DESTDIR` staging.
- [ ] The Portfile pins the LLVM version + SHA-256.

### A.2 — Size-minimized configuration

**File:** `xtask/src/port_build.rs` (`build_llvm`)
**Symbol:** the CMake invocation
**Why it matters:** the size levers are the difference between a few-hundred-MB and a multi-GB artifact, and a single target (`X86`) is the biggest lever.

**Acceptance:**
- [ ] CMake configures with `-DLLVM_ENABLE_PROJECTS="clang;lld" -DLLVM_ENABLE_RUNTIMES="libcxx;libcxxabi;libunwind" -DLLVM_TARGETS_TO_BUILD="X86" -DCMAKE_BUILD_TYPE=MinSizeRel -DLLVM_ENABLE_THREADS=OFF -DLLVM_ENABLE_ZLIB=OFF -DLLVM_ENABLE_ZSTD=OFF -DLLVM_ENABLE_TERMINFO=OFF -DLLVM_INCLUDE_TESTS=OFF -DLLVM_INCLUDE_BENCHMARKS=OFF -DCLANG_ENABLE_STATIC_ANALYZER=OFF`, statically linked.
- [ ] `ninja clang lld` builds; both binaries are `install/strip`-ped; the musl sysroot (`libc.a`, CRT objects), Clang builtin headers, and `compiler-rt` builtins are bundled.

### A.3 — Resource-dir relocation

**File:** `build_llvm` + the 85a relocation-contract doc
**Symbol:** the staged `lib/clang/<ver>/{include,lib}` layout
**Why it matters:** Clang locates builtin headers + builtins via the resource dir; if it is not relative to the executable, the `.m3pkg` is not relocatable — the hardest relocation case in the phase.

**Acceptance:**
- [ ] Clang resolves its resource dir (`lib/clang/<ver>/include` + builtins) relative to the `clang` binary, and a fixed `--sysroot` supplies libc headers/CRT; `clang -print-resource-dir` inside m3OS points under `/usr`.

### A.4 — Bundle the C++ runtime

**Files:** `xtask/src/port_build.rs` (`build_llvm`), the staged tree
**Symbol:** the `libcxx;libcxxabi;libunwind` runtimes + `c++/v1` headers staging
**Why it matters:** the B.2 gate compiles `hello.cpp`; without `libc++`/`libc++abi`/`libunwind` and the `c++/v1` headers, a C++ program cannot **link**, so the C++ acceptance criterion would be impossible to satisfy. The standalone `docs/clang-llvm-roadmap.md` (lines 161–172) lists exactly these.

**Acceptance:**
- [ ] `libc++.a`, `libc++abi.a`, `libunwind.a` and the `c++/v1` headers are built (via `LLVM_ENABLE_RUNTIMES`, A.2) and staged into the `.m3pkg`.
- [ ] `clang++ /usr/src/hello.cpp -o app && ./app` links against the bundled runtime and runs inside m3OS.

### A.5 — Provide a working `clang++`

**Files:** `build_llvm` + the staged tree
**Symbol:** the `clang++` driver entry
**Why it matters:** `clang++` is normally a symlink to `clang`; the clang roadmap (lines 225–244) flags that symlink/`/proc/self/exe` behavior is the documented hazard on a from-scratch OS, and the B.2 gate runs `clang++` — so its provisioning must be explicit, not assumed.

**Acceptance:**
- [ ] `clang++` is provided as a symlink to `clang` if ext2 symlinks are reliable, else via `argv[0]` driver-mode dispatch or a copied binary; `clang++ --version` succeeds inside m3OS.

---

## Track B — Opt-in packaging + validation

### B.1 — Seal behind an opt-in image feature

**Files:** `xtask/src/port_build.rs` (85a seal step), `xtask/src/main.rs` (an opt-in image feature gate for the heavy artifact)
**Symbol:** the 85a `seal_package` + an `M3OS_WITH_CLANG`-style image feature
**Why it matters:** the heavyweight artifact must not bloat default images; it is opt-in, and the 85a cache makes its repeat builds free.

**Acceptance:**
- [ ] `cargo xtask port build llvm` produces a `.m3pkg`; a second image build is a pkgcache hit with **zero** compiler invocations (the 85a payoff, proven on the heaviest artifact).
- [ ] The Clang `.m3pkg` is bundled only when the opt-in image feature is set; default images omit it and the disk delta is documented.

### B.2 — C/C++ build validation gate

**Files:** `xtask/src/main.rs` (`clang-smoke` serial gate), `AGENTS.md` (opt-in row `M3OS_CLANG_REGRESSION=1`), bundled `/usr/src/hello.c` + `/usr/src/hello.cpp`
**Symbol:** `cmd_clang_smoke`
**Why it matters:** proves Clang + LLD actually compile + link + run inside m3OS.

**Acceptance:**
- [ ] Inside m3OS: `clang -O2 /usr/src/hello.c -o hello && ./hello` prints "hello, world"; `clang++ /usr/src/hello.cpp` builds + runs (links the A.4 C++ runtime); `clang -fuse-ld=lld /usr/src/hello.c` links via LLD.
- [ ] The `/usr/src/hello.c` + `/usr/src/hello.cpp` fixtures are written into the data disk via `populate_ext2_files`, with `cargo xtask clean` run to recreate the disk.
- [ ] The gate is wired as an opt-in pre-push regression (`M3OS_CLANG_REGRESSION=1`) in `AGENTS.md`.

---

## Track C — Release closeout (umbrella)

### C.1 — Create the umbrella learning doc

**Files:**
- `docs/85-cross-compiled-toolchains.md` (new — one learning doc for the whole 85 family, per the Phase 78 precedent)
- `docs/README.md` (link it)

**Symbol:** a learning doc following the aligned learning-doc template (`docs/appendix/doc-templates.md`) and the shape of `docs/78-usb-host-foundation.md`
**Why it matters:** every phase ships a learning doc (the roadmap "Required Documentation for Every Phase" rule); this teaches the build-once packaging substrate, the relocation contract, the disk/RAM implications, and how git/Python/Clang fit the post-1.0 developer story.

**Acceptance:**
- [ ] `docs/85-cross-compiled-toolchains.md` exists, follows the learning-doc template, explains 85a's content-addressed cache + `.m3pkg` + offline `pkg`, and covers git/Python/Clang in learner-friendly terms with the disk/RAM budget.
- [ ] It is linked from `docs/README.md`'s phase-aligned learning-docs table and links the four 85a–d design + task docs.

### C.2 — Capability inventory + README finalization

**Files:**
- `AGENTS.md` (capability inventory bullet + kernel version line + opt-in gate rows)
- `docs/roadmap/README.md` (the umbrella 85 + 85a–d rows flip to Complete)

**Symbol:** the AGENTS.md capability bullet; the README Status cells
**Why it matters:** AGENTS.md is the always-loaded inventory and the README is the authoritative phase index; both must reflect the landed toolchain capability class.

**Acceptance:**
- [ ] An AGENTS.md capability bullet is added for the cross-compiled developer-toolchain + packaging class (a genuinely new capability class per the maintenance policy), with the kernel version line reading `0.85.3` and the `git`/`python`/`clang` opt-in gate rows present.
- [ ] The `docs/roadmap/README.md` umbrella 85 row + 85a–d rows are flipped to Complete with their `0.85.x` versions.

### C.3 — Bump kernel crate `0.85.2` → `0.85.3`

**File:** `kernel/Cargo.toml`
**Symbol:** `[package] version = "0.85.3"`
**Why it matters:** the 85d cut is the final Phase 85 sub-phase and the family's release version.

**Acceptance:**
- [ ] `kernel/Cargo.toml` reads `0.85.3` (+ `Cargo.lock`); `cargo xtask check` clean; boot banner / `uname` report `0.85.3`.

---

## Documentation Notes

- **What changed relative to the standalone roadmap.** `docs/clang-llvm-roadmap.md` Stage 1 (host-cross-built static clang+lld) is this sub-phase; its Stage 2 (self-hosting LLVM inside m3OS) remains deferred.
- **The 85a cache payoff is proven here.** Clang is the worst-case artifact; B.1's "zero compiler invocations on a second build" is the headline validation of the whole umbrella phase.
- **Honesty.** Clang is opt-in and X86-only, statically linked, no `opt`/`llc`/sanitizers, no self-hosting — the docs must not imply a full LLVM toolchain.
- **Prefer exact targets.** Reference the exact CMake flags and the resource-dir layout, not "the LLVM build options".
