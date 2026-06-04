# Phase 85d - Clang/LLVM/LLD (+ Release)

**Status:** Planned
**Source Ref:** phase-85d
**Depends on:** Phase 85a (Package & Build-Cache Infrastructure); 85b and 85c land first (smaller artifacts validate the substrate before the ~1 GB one)
**Builds on:** Adds the largest toolchain on the Phase 85a substrate — a host-cross-built static Clang + LLD — and carries the umbrella "+ Release" items for the whole Phase 85 family (learning doc, capability inventory, README finalization).
**Primary Components:** `ports/lang/llvm/Portfile`, `xtask/src/port_build.rs` (`build_llvm`), LLVM CMake cross-build, the Phase 85a `.m3pkg` pipeline + opt-in image feature, `docs/clang-llvm-roadmap.md`, `docs/85-cross-compiled-toolchains.md`

## Milestone Goal

A host-cross-built static **Clang + LLD** (X86-only, `MinSizeRel`) is bundled on the m3OS image behind an opt-in feature and compiles + links C/C++ sample programs that run inside m3OS, installed from a Phase 85a `.m3pkg`. This sub-phase also cuts the umbrella learning doc and the capability-inventory bump that close out the Phase 85 family.

## Why This Phase Exists

Clang/LLD is the post-TCC step toward a real native toolchain. It is also the single artifact (several hundred MB installed — verify on a real build; multi-GB-RAM, multi-hour build) whose rebuild cost is the entire justification for Phase 85a — so it lands last, on a proven substrate, and is gated behind an opt-in image feature so default images stay small. As the final sub-phase it carries the family's release closeout.

## Learning Goals

- The LLVM CMake cross-build (`LLVM_ENABLE_PROJECTS`, `LLVM_TARGETS_TO_BUILD`, `LLVM_HOST_TRIPLE`, `CMAKE_SYSROOT`, `DESTDIR`) and the genuine size levers (single target, `MinSizeRel`, tests/benchmarks/static-analyzer off, `install/strip`) — with `LLVM_ENABLE_THREADS=OFF` chosen for a single-threaded m3OS target rather than as a size lever.
- Why Clang must resolve its resource dir (`lib/clang/<ver>/{include,lib}`) relative to the executable to be relocatable, and how the m3OS sysroot supplies libc headers/CRT.
- The difference between a host cross-clang and an on-device native clang, and why the heavy artifact is feature-gated.

## Feature Scope

### Area A — Clang + LLD cross build

`cmake -DLLVM_ENABLE_PROJECTS="clang;lld" -DLLVM_ENABLE_RUNTIMES="libcxx;libcxxabi;libunwind" -DLLVM_TARGETS_TO_BUILD="X86" -DCMAKE_BUILD_TYPE=MinSizeRel -DLLVM_ENABLE_THREADS=OFF -DLLVM_ENABLE_ZLIB=OFF -DLLVM_ENABLE_ZSTD=OFF -DLLVM_ENABLE_TERMINFO=OFF -DLLVM_INCLUDE_TESTS=OFF -DLLVM_INCLUDE_BENCHMARKS=OFF -DCLANG_ENABLE_STATIC_ANALYZER=OFF` against the musl toolchain, statically linked, `ninja clang lld` + strip. Bundle the musl sysroot (`libc.a`, CRT objects), Clang builtin headers, `compiler-rt` builtins, **and the C++ runtime (`libc++.a`, `libc++abi.a`, `libunwind.a` + `c++/v1` headers)** so the C++ sample can actually link. Provide a working **`clang++`** (symlink, or `argv[0]` driver-mode dispatch if ext2 symlinks are unreliable).

### Area B — Packaging (opt-in) + validation + release closeout

Seal into a `.m3pkg`; gate it behind an opt-in image feature (default images omit the ~1 GB artifact); validate C/C++ compiles inside m3OS. Then cut the umbrella learning doc `docs/85-cross-compiled-toolchains.md`, the AGENTS.md capability bullet, and the README finalization for the family.

## Important Components and How They Work

### `build_llvm` in `port_build.rs`

A new CMake-template port `build_*` function using `musl_toolchain()` for the C/C++ compilers, `CMAKE_SYSROOT` pointed at the m3OS musl sysroot, `DESTDIR` staging, then the 85a sealing step. Registered in `PORTS` + dispatch. Because the build is heavy, the 85a content-addressed cache is what makes repeat image builds free.

### Resource-dir relocation

Clang's builtin headers + `compiler-rt` builtins install under `lib/clang/<ver>/` resolved relative to the `clang` binary; a fixed m3OS `--sysroot` supplies libc headers/CRT — both 85a relocation-contract requirements, here at their hardest.

## How This Builds on Earlier Phases

- Consumes the Phase 85a `.m3pkg` pipeline + content-addressed cache (essential — it is what makes the ~1 GB artifact's repeat builds free) + relocation contract.
- Builds on the TCC precedent (Phase 31) as the next-generation C/C++ compiler, and the Phase 36 large-mmap baseline for clang's working set.

## Implementation Outline

1. Add `ports/lang/llvm/Portfile` (pinned version + SHA-256) and `build_llvm` (CMake cross-build).
2. Cross-build clang + lld (X86-only, MinSizeRel, threads off); `install/strip`; bundle sysroot + resource dir.
3. Seal `.m3pkg` behind the opt-in image feature; `pkg install clang`.
4. Validate C/C++ sample builds inside m3OS.
5. Cut the umbrella learning doc + capability bullet + README finalization; bump kernel to `0.85.3`.

## Acceptance Criteria

- Clang + LLD build reproducibly via `cargo xtask port build llvm` and seal into a `.m3pkg`; a second image build is a cache hit with **zero** compiler invocations (the 85a payoff, asserted on the heaviest artifact).
- Inside m3OS: `clang -O2 /usr/src/hello.c -o hello && ./hello` prints "hello, world"; `clang++ /usr/src/hello.cpp` links against the bundled `libc++`/`libc++abi`/`libunwind` and runs; `clang -fuse-ld=lld` links via LLD (serial-validated gate).
- `clang++` is provided and works (`clang++ --version` succeeds), and `clang -print-resource-dir` resolves under `/usr` (relocation contract).
- The Clang `.m3pkg` is behind an opt-in image feature; default images omit it and stay small (documented disk delta).
- The umbrella learning doc `docs/85-cross-compiled-toolchains.md` exists, follows the learning-doc template, and is linked from `docs/README.md`; the AGENTS.md capability inventory + kernel version (`0.85.3`) are updated.

## Companion Task List

- [Phase 85d Task List](./tasks/85d-clang-llvm-tasks.md)

## How Real OS Implementations Differ

- Distributions ship full LLVM (all targets, `opt`/`llc`/sanitizers/clang-tools) and dynamic linking that shrinks install size; 85d ships clang+lld only, statically, X86-only.
- Self-hosting (building LLVM *on* m3OS) needs C++ exception handling, threading, and build infra (CMake/Ninja) and remains deferred.

## Deferred Until Later

- Self-hosting LLVM inside m3OS; multi-threaded compilation (`LLVM_ENABLE_THREADS=ON`); runtime sanitizers; dynamic linking of the toolchain.
- Additional LLVM targets beyond X86; `opt`/`llc`/full tool suite.
