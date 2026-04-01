# Phase 50 - Cross-Compiled Toolchains

## Milestone Goal

Three major development tools — git, Python, and Clang — are cross-compiled on the
host and bundled on the m3OS disk image. Users can version-control code with git,
run Python scripts, and compile optimized C/C++ programs with Clang, all inside the
OS. This follows the same pattern as TCC (Phase 31): build on host with musl, copy
to disk image, run inside the OS.

## Learning Goals

- Understand how large C and C++ projects are cross-compiled with musl for static
  linking.
- Learn how `xtask` integrates external tool builds into the disk image pipeline.
- See how demand paging (Phase 36) enables running binaries much larger than TCC.

## Feature Scope

### git (Local Operations)

Cross-compile git with musl, `NO_CURL=1`, `NO_OPENSSL=1` (no network support —
that comes in Phase 51).

**What works:** `git init`, `add`, `commit`, `log`, `diff`, `status`, `branch`,
`merge`, `checkout`, `stash`, `blame`, `rebase`, `tag`.

**Binary:** ~15 MB static, stripped. Plus `git-core` subcommands (hard-linked) and
templates (~1 MB).

**xtask integration:** `build_git()` function following the TCC pattern. Cached in
`target/git-staging/`.

See [git roadmap](../git-roadmap.md) for full details.

### Python (CPython Interpreter)

Cross-compile CPython 3.12+ with musl, `--disable-shared`, `--without-ensurepip`,
`--without-pymalloc`. Single-threaded, no networking modules.

**What works:** REPL, script execution, full stdlib (`json`, `re`, `math`,
`collections`, `pathlib`, `os`, `sys`, `argparse`, `csv`, `datetime`, etc.).

**Binary:** ~8 MB static. Stdlib `.py` files ~30 MB.

**xtask integration:** `build_python()` function. Cached in `target/python-staging/`.

See [Python roadmap](../python-roadmap.md) for full details.

### Clang/LLD (C/C++ Compiler)

Cross-compile Clang and LLD with musl, `LLVM_ENABLE_THREADS=OFF`,
`LLVM_TARGETS_TO_BUILD="X86"`, `MinSizeRel`. Includes libc++ and compiler-rt.

**What works:** `clang -O2 hello.c -o hello`, `clang++ app.cpp -o app`,
`clang -fuse-ld=lld`, full optimization levels (`-O0` through `-O3`, `-Os`, `-Oz`).

**Binary:** ~100 MB (clang) + ~40 MB (lld), both static stripped. Plus libc++ (~8 MB),
headers (~25 MB).

**xtask integration:** `build_clang()` function. Cached in `target/clang-staging/`.

See [Clang/LLVM roadmap](../clang-llvm-roadmap.md) for full details.

### Filesystem Layout

```
/usr/
  bin/
    tcc               -- existing (~300 KB)
    git               -- git binary (~15 MB)
    python3           -- CPython (~8 MB)
    clang             -- Clang compiler (~100 MB)
    ld.lld            -- LLD linker (~40 MB)
  lib/
    python3.12/       -- Python stdlib (~30 MB)
    libc++.a          -- C++ standard library (~8 MB)
    libc++abi.a       -- C++ ABI (~1 MB)
    libunwind.a       -- unwinding (~500 KB)
    libclang_rt.builtins.a  -- compiler-rt (~2 MB)
    clang/<ver>/include/    -- Clang built-in headers
  libexec/
    git-core/         -- git subcommands (hard-linked)
  include/
    c++/v1/           -- libc++ headers (~5 MB)
  share/
    git-core/templates/
  src/
    hello.c, hello.py, hello.cpp  -- test programs
```

**Total disk footprint:** ~300 MB. Requires the 1 GB ext2 partition from Phase 36.

## Dependencies

- **Phase 33** (Kernel Memory) — working `munmap()`, OOM retry
- **Phase 36** (Expanded Memory) — demand paging for large binaries, 1 GB disk, 1 GB RAM

## Acceptance Criteria

- [ ] `git init && git add . && git commit -m "test"` works inside m3OS.
- [ ] `git log --oneline` shows commit history.
- [ ] `git diff` shows file changes.
- [ ] `python3 -c "print('hello from m3OS')"` works.
- [ ] `python3 -c "import json; print(json.dumps({'os': 'm3OS'}))"` works.
- [ ] `python3 /usr/src/fibonacci.py` runs a script.
- [ ] `clang -O2 /usr/src/hello.c -o /tmp/hello && /tmp/hello` works.
- [ ] `clang++ /usr/src/hello.cpp -o /tmp/hello_cpp && /tmp/hello_cpp` works.
- [ ] `clang -O2 /usr/src/tcc/tcc.c -o /tmp/tcc-opt` compiles TCC with optimization.

## Deferred Items

- **git remote operations** (clone, push, pull) — requires TLS/DNS (Phase 51).
- **Python networking** (ssl, pip, asyncio) — requires Phases 37, 40, 42.
- **Clang self-hosting** — building LLVM from source inside m3OS. See Stage 2 in the
  [Clang roadmap](../clang-llvm-roadmap.md).
- **npm/Node.js** — separate phase (Phase 52) due to heavier kernel requirements.
