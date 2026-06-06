# Phase 85d — Parallel-Impl Track Report

Durable batch/track report for the Phase 85d (Clang/LLVM/LLD + Release) implementation
run on branch `feat/phase-85d-clang-llvm`. Updated as tracks progress.

## Batch context

- **Integration branch:** `feat/phase-85d-clang-llvm` → PR to `main`.
- **Toolchain reality:** no musl C++ compiler in this environment (only the C-only
  `musl-tools` wrapper). Host **clang 18.1.3** is used as the cross-compiler
  (`--target=x86_64-linux-musl` + an assembled musl sysroot). LLVM pinned **18.1.8**.
- **Validation surface:** `cargo xtask check`; `cargo xtask port build llvm`;
  `cargo xtask <clang-smoke>` (new gate); pkgcache-hit assertion; in-OS C/C++ build.

## Tracks

### Track A — Clang + LLD cross-build
- **Owned tasks:** A.1–A.5
- **Owned files:** `ports/lang/llvm/Portfile`, `xtask/src/port_build.rs`
  (`build_llvm`, `build_llvm_port`, `build_recipe_id`, `port_deps`, dispatch),
  `xtask/src/main.rs` (`PORTS`/`BUNDLE_ONLY_PORTS`).
- **Validation:** `cargo xtask port build llvm` seals a `.m3pkg`; staged tree has
  `bin/clang`, `bin/lld`, `bin/clang++`, `lib/clang/<ver>/include`, libc++/abi/unwind,
  CRT + musl sysroot; resource dir relative to the binary.
- **State:** active (coordinator-driven, main tree).

### Track B — Opt-in packaging + validation
- **Owned tasks:** B.1, B.2
- **Owned files:** `xtask/src/main.rs` (`cmd_clang_smoke`, image-feature gate,
  `populate_ext2_files` fixtures), `AGENTS.md` (regression row — coordinator),
  data-disk fixtures `/usr/src/hello.c` + `/usr/src/hello.cpp`.
- **Validation:** in-OS `pkg install clang`; `clang -O2 hello.c -o hello && ./hello`;
  `clang++ hello.cpp`; `clang -fuse-ld=lld`; `clang -print-resource-dir` under `/usr`;
  second image build = pkgcache hit, zero compiler invocations.
- **State:** pending (depends on Track A artifact).

### Track C — Release closeout (docs)
- **Owned tasks:** C.1, C.2 (docs/README parts), C.3 partial
- **Owned files:** `docs/85-cross-compiled-toolchains.md` (new), `docs/README.md`,
  `docs/roadmap/README.md`. Coordinator handles `AGENTS.md` + `kernel/Cargo.toml`.
- **Worktree:** isolated worktree, separate agent, background.
- **State:** active (parallel agent).

## Rescue history
- (none)

## Outcome measures (filled at batch close)
- discovery-reuse: n/a (coordinator inline discovery)
- rescue-attempts: 0
- abandonment-events: 0
- re-review-loops: 0
