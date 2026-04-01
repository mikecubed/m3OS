# Road to Clang/LLVM on m3OS

This document details the path to running Clang/LLVM on m3OS. The strategy is
two stages: first cross-compile Clang on the host and run it inside the OS
(like we did with TCC in Phase 31), then eventually build LLVM from source
natively. Getting Clang running is the priority -- self-hosting comes later.

## Current State (Phase 32 complete)

What we have today:

- **TCC** -- Tiny C Compiler compiles C programs and itself (self-hosting)
- **pdpmake** -- POSIX-compatible make for multi-file builds
- **musl libc** -- statically linked against all C programs
- **ext2 filesystem** -- 128 MB data partition
- **Kernel heap** -- growable to 64 MiB, no buddy/slab allocator
- **`munmap()`** -- stubbed (does not reclaim memory)
- **No threading** -- single-threaded processes only
- **No dynamic linking** -- everything is statically linked
- **No C++ support** -- TCC is C-only, no C++ runtime/stdlib

---

# Stage 1: Cross-Compiled Clang

The goal: build a static Clang binary on the host, bundle it on the disk
image, and compile C programs inside m3OS with a real production compiler.
This follows the same pattern as TCC (Phase 31) -- cross-compile on host,
run inside the OS.

## What Cross-Compiled Clang Gives Us

Compared to TCC, a cross-compiled Clang provides:

| Capability | TCC (today) | Clang (cross-compiled) |
|---|---|---|
| Optimization | None (`-O0` only) | Full (`-O0` through `-O3`, `-Os`, `-Oz`) |
| C standard | C99/partial C11 | Full C17, partial C23 |
| C++ compilation | Not supported | Full C++17, C++20, partial C++23 |
| Warnings/diagnostics | Basic | Industry-leading diagnostics |
| Static analysis | None | `-Weverything`, scan-build, `-fsanitize` |
| Code generation | Simple x86_64 | LLVM backend: vectorization, instruction scheduling |
| Link-time optimization | None | ThinLTO, full LTO |
| Cross-compilation | x86_64 only | Any LLVM target (if backends enabled) |
| Self-hosting | Yes (C only) | No (needs C++ runtime to build LLVM) |
| Binary size | ~300 KB | ~100-150 MB (static, stripped, minimal backends) |
| Compilation speed | Very fast | Slower (optimizing compiler) |
| Standard conformance | Loose | Strict, well-tested |

**What this unlocks:**
- Compile optimized C programs inside the OS (real `-O2` with vectorization)
- Compile C++ programs (with statically-linked libc++)
- Better error messages for development inside the OS
- A stepping stone toward self-hosting: Clang can compile C/C++ code that
  TCC cannot handle

## Host-Side Cross-Compilation

### Building the Clang Binary

Build a minimal static Clang on the host, targeting musl libc. This produces
a single `clang` binary with the C++ runtime statically linked in.

```bash
# Clone LLVM
git clone --depth 1 https://github.com/llvm/llvm-project.git
cd llvm-project

# Minimal CMake configuration for a static, musl-linked Clang
cmake -S llvm -B build -G Ninja \
  -DCMAKE_BUILD_TYPE=MinSizeRel \
  -DCMAKE_C_COMPILER=x86_64-linux-musl-gcc \
  -DCMAKE_CXX_COMPILER=x86_64-linux-musl-g++ \
  -DCMAKE_EXE_LINKER_FLAGS="-static" \
  -DLLVM_ENABLE_PROJECTS="clang;lld" \
  -DLLVM_TARGETS_TO_BUILD="X86" \
  -DLLVM_ENABLE_THREADS=OFF \
  -DLLVM_ENABLE_ZLIB=OFF \
  -DLLVM_ENABLE_ZSTD=OFF \
  -DLLVM_ENABLE_TERMINFO=OFF \
  -DLLVM_ENABLE_LIBXML2=OFF \
  -DLLVM_BUILD_DOCS=OFF \
  -DLLVM_INCLUDE_TESTS=OFF \
  -DLLVM_INCLUDE_BENCHMARKS=OFF \
  -DLLVM_INCLUDE_EXAMPLES=OFF \
  -DCLANG_ENABLE_STATIC_ANALYZER=OFF \
  -DCLANG_ENABLE_ARCMT=OFF \
  -DLLVM_STATIC_LINK_CXX_STDLIB=ON \
  -DLLVM_BUILD_STATIC=ON

ninja -C build clang lld
strip build/bin/clang
strip build/bin/lld
```

Key decisions:
- **`LLVM_ENABLE_THREADS=OFF`** -- avoids pthreads dependency until Phase 39
- **`LLVM_TARGETS_TO_BUILD="X86"`** -- only x86_64 backend, saves ~80% size
- **`MinSizeRel`** -- optimize for binary size
- **Static musl linkage** -- no dynamic linker needed on m3OS
- **lld included** -- LLVM's linker replaces TCC's built-in linker

### Expected Binary Sizes

| Binary | Approximate size (stripped) |
|---|---|
| `clang` | ~100 MB (static, X86 only, MinSizeRel) |
| `lld` | ~40 MB (static, stripped) |
| `libc.a` + `libc++.a` | ~15 MB |
| musl + libc++ headers | ~20 MB |
| **Total disk footprint** | **~175 MB** |

### What Gets Bundled on the Disk Image

```
/usr/
  bin/
    tcc               -- existing TCC (~300 KB)
    clang             -- Clang compiler (~100 MB)
    ld.lld            -- LLD linker (~40 MB)
  lib/
    libc.a            -- musl static C library (existing)
    libc++.a          -- LLVM C++ standard library (~8 MB)
    libc++abi.a       -- C++ ABI library (~1 MB)
    libunwind.a       -- Stack unwinding for C++ exceptions (~500 KB)
    libclang_rt.builtins.a  -- compiler-rt builtins (~2 MB)
    crt1.o, crti.o, crtn.o  -- CRT startup objects (existing)
    clang/
      <version>/
        include/      -- Clang built-in headers (stdarg.h, etc.)
  include/
    stdio.h, ...      -- musl system headers (existing)
    c++/
      v1/             -- libc++ headers (~5 MB)
  src/
    hello.c           -- existing test program
    hello.cpp         -- C++ test program (new)
```

### xtask Integration

Add a `build_clang()` function to xtask following the TCC pattern:

```
Host (build time)                       Guest (m3OS, run time)
+----------------------------------+    +------------------------------------------+
| cargo xtask image                |    |  C compilation:                          |
|   build_clang()                  |    |    $ clang /tmp/prog.c -o /tmp/prog      |
|     cmake + ninja (LLVM)         |    |    $ /tmp/prog                           |
|     static musl linkage          |    |                                          |
|   populate_clang_files()         |    |  C++ compilation:                        |
|     /usr/bin/clang               |    |    $ clang++ /tmp/app.cpp -o /tmp/app    |
|     /usr/bin/ld.lld              |    |    $ /tmp/app                            |
|     /usr/lib/libc++.a, etc.      |    |                                          |
|     /usr/include/c++/v1/*        |    |  Optimized build:                        |
|     clang built-in headers       |    |    $ clang -O2 /tmp/fast.c -o /tmp/fast  |
+----------------------------------+    +------------------------------------------+
```

## Kernel/OS Prerequisites for Stage 1

Cross-compiled Clang still needs kernel features beyond what TCC requires.
Clang is a much larger binary that makes heavier use of the OS.

### Phase 33 -- Kernel Memory Improvements (in progress, assumed ready)

**What it delivers:**
- Slab allocator for O(1) fixed-size kernel object allocation
- Buddy allocator for page-granularity allocations (4 KiB to 2 MiB)
- OOM retry -- grows heap on demand instead of panicking
- Working `munmap()` -- actually reclaims physical frames
- Userspace heap coalescing to reduce fragmentation

**Why Clang needs it:** Clang allocates and frees memory aggressively during
compilation (AST nodes, IR, codegen buffers). Without working `munmap()`,
every compilation leaks all memory. Without OOM retry, compiling anything
non-trivial panics the kernel.

---

### Phase 37 -- Filesystem Enhancements (planned)

**What it delivers:**
- Symlinks (`symlink`, `readlink`, path resolution follows symlinks)
- `/dev/null`, `/dev/zero` device nodes
- `/proc/self/exe` for binary self-location

**Why Clang needs it:**
- Clang uses `/proc/self/exe` to find its resource directory (headers,
  libraries) relative to the binary path. Without this, Clang cannot locate
  its own built-in headers. **Workaround:** set `--sysroot` and
  `-resource-dir` explicitly, or patch Clang's driver to use a hardcoded
  path like `/usr`.
- `/dev/null` is used by Clang for syntax-only checks (`clang -fsyntax-only`)
  and discarding output. **Workaround:** redirect to a tmpfs file.
- Tool aliases (`clang++` -> `clang`) normally use symlinks.
  **Workaround:** use `clang -x c++` or copy the binary (wastes 100 MB).

**Verdict:** Not strictly blocking for Stage 1. Workarounds exist for all
three. But symlinks and `/dev/null` are high-value quality-of-life features.

---

### NEW: Expanded Memory Phase (not yet on roadmap)

**Suggested placement:** After Phase 33, before or alongside Phase 35.

This is the single biggest kernel requirement for running Clang. TCC needs
~10 MB to compile a program. Clang needs ~200-500 MB.

**What it delivers:**

1. **Demand paging (lazy allocation)** -- `mmap()` maps virtual pages without
   immediately allocating physical frames. The page fault handler allocates
   frames on first access. This is how every real OS works, and it is
   critical: Clang's allocator calls `mmap()` for large regions and expects
   only the touched pages to consume physical memory.

2. **Large `mmap()` regions** -- support allocations of 256+ MB contiguous
   virtual address space. The current `mmap` implementation may not handle
   regions this large.

3. **`mprotect()`** -- change page permissions on mapped regions. LLVM's JIT
   (`lli`) and sanitizers need to transition pages from `RW` to `RX`. Not
   needed for basic `clang` compilation, but needed for `-fsanitize` and
   any future JIT work.

4. **QEMU RAM increase** -- raise default QEMU memory from 256 MB to 1+ GB.
   Clang's working set during compilation of a non-trivial file can reach
   500 MB. This is a one-line change in xtask (`-m 1G`).

5. **Optional: overcommit** -- allow `mmap()` to promise more virtual memory
   than physical RAM available, relying on demand paging. Linux does this by
   default. Conservative approach: only overcommit up to 2x physical RAM.

**What it does NOT include (deferred to self-hosting stage):**
- Swap to disk
- `setrlimit()` per-process limits
- OOM killer (kernel still panics on true physical memory exhaustion)

**Implementation complexity:** Demand paging is a significant kernel feature.
The page fault handler must distinguish between:
- Lazy allocation faults (allocate a frame, map it, resume)
- Stack growth faults (extend stack, map frame, resume)
- True invalid accesses (segfault the process)
- Copy-on-write faults (already implemented in Phase 17)

Estimate: comparable to Phase 17 (Memory Reclamation) in scope.

---

### Disk Image Expansion

The ext2 data partition must grow from 128 MB to at least 512 MB (ideally
1 GB) to hold Clang, lld, libc++, and their headers alongside TCC.

**Changes needed:**
- xtask: increase ext2 partition size in `create_disk_image()`
- xtask: increase raw disk image size accordingly
- QEMU: adjust `-drive` size if needed

This is a straightforward xtask change, not a kernel feature.

---

## Stage 1 Dependency Graph

```
Phase 33 (Kernel Memory)         -- IN PROGRESS
    |
    +--- munmap works, OOM retry, slab/buddy allocators
    |
    v
NEW: Expanded Memory             -- NOT YET ON ROADMAP
    |
    +--- demand paging, large mmap, QEMU RAM increase
    |
    v
Disk Image Expansion             -- xtask change only
    |
    +--- ext2 partition grows to 512 MB - 1 GB
    |
    v
Cross-Compile Clang on Host      -- xtask build_clang()
    |
    +--- static clang + lld + libc++ + headers
    |
    v
    Clang runs inside m3OS!       C and C++ compilation works
```

**Optional but recommended (can be done in parallel or after):**
- Phase 37 (Filesystem Enhancements) -- symlinks, /dev/null, /proc/self/exe

**Not required for Stage 1:**
- Phase 34 (RTC) -- Clang doesn't need wall-clock time to compile
- Phase 35 (True SMP) -- Clang runs single-threaded
- Phase 36 (I/O Multiplexing) -- Clang doesn't use select/epoll
- Phase 38 (Unix Domain Sockets) -- not needed
- Phase 39 (Threading) -- built with `LLVM_ENABLE_THREADS=OFF`

## Stage 1 Acceptance Criteria

```bash
# C compilation with optimization
$ clang -O2 /usr/src/hello.c -o /tmp/hello
$ /tmp/hello
hello, world

# C++ compilation
$ clang++ /usr/src/hello.cpp -o /tmp/hello_cpp
$ /tmp/hello_cpp
hello from C++

# Clang compiles TCC (proving it can handle real C codebases)
$ clang -O2 /usr/src/tcc/tcc.c -o /tmp/tcc-opt
$ /tmp/tcc-opt --version
tcc version 0.9.28rc ...

# LLD works as the linker
$ clang -fuse-ld=lld /usr/src/hello.c -o /tmp/hello-lld
$ /tmp/hello-lld
hello, world

# Optimized binary is faster than TCC-compiled version
$ time /tmp/tcc-opt /usr/src/hello.c -o /dev/null    # should be measurably faster
```

## What Stage 1 Does NOT Give Us

- **Self-hosting** -- Clang cannot compile itself inside m3OS (needs CMake,
  Python, 2+ GB disk space, hours of compute time)
- **Multi-threaded compilation** -- threads are disabled in the cross-compiled
  binary
- **Dynamic linking** -- everything remains statically linked
- **Sanitizers at runtime** -- AddressSanitizer/UBSan need runtime support
  that m3OS doesn't provide yet
- **Incremental compilation** -- no `clang-scan-deps`, no modules, no PCH
  (these require filesystem features we don't have yet)

---

# Stage 2: Self-Hosting (Building LLVM Inside m3OS)

This is the long-term goal. Once Clang runs via cross-compilation, the next
frontier is building LLVM from source inside the OS.

## Additional Prerequisites Beyond Stage 1

### Phase 34 -- Real-Time Clock (planned)

**Delivers:** CMOS RTC driver, wall-clock time, `CLOCK_REALTIME`,
`gettimeofday()`.

**Why self-hosting needs it:** CMake and make depend on file timestamps for
incremental builds. Without real timestamps, every `make` invocation
rebuilds everything. LLVM has ~3000 source files -- full rebuilds are not
viable.

---

### Phase 35 -- True SMP Multitasking (planned)

**Delivers:** Per-core syscall stacks, SMP-aware scheduling with load
balancing, priority levels.

**Why self-hosting needs it:** Building LLVM is CPU-intensive. Single-core
builds take hours on fast hardware. With SMP scheduling, `make -j4` can
actually use multiple cores.

---

### Phase 36 -- I/O Multiplexing (planned)

**Delivers:** `select()`, `epoll`, non-blocking I/O, `O_NONBLOCK`.

**Why self-hosting needs it:** Parallel build tools (make -j, ninja) use
non-blocking I/O to manage multiple compiler processes. Without this, builds
are strictly sequential even if threading is available.

---

### Phase 37 -- Filesystem Enhancements (planned)

**Delivers:** Symlinks, hard links, `/proc`, permission enforcement, device
nodes.

**Why self-hosting needs it (beyond Stage 1 workarounds):**
- LLVM's build creates hundreds of symlinks for tool aliases
- CMake uses symlinks extensively during the build
- `/proc` is needed for various build system introspection
- `/dev/null` is needed throughout the build process
- At the self-hosting scale, workarounds from Stage 1 are no longer viable

---

### Phase 39 -- Threading Primitives (planned)

**Delivers:** `clone(CLONE_THREAD)`, `futex()`, TLS, thread groups,
pthreads via musl.

**Why self-hosting needs it:** A threaded Clang is dramatically faster.
Rebuilding LLVM is already slow; single-threaded Clang makes it impractical.
The self-hosted Clang should be built with `LLVM_ENABLE_THREADS=ON`.

---

### NEW: Build Infrastructure Phase (not yet on roadmap)

**What it delivers:**
- **CMake** -- LLVM's build system requires it. Cross-compile CMake (it's
  a single static C++ binary, ~30 MB) and bundle on disk.
- **Ninja** (alternative) -- lighter than make, LLVM's preferred generator.
  Cross-compile and bundle (~2 MB static).
- **Python 3** (optional) -- LLVM's test suite (lit) needs Python. Not
  required for building, only for running tests.
- **GNU binutils or llvm-tools** -- `nm`, `objdump`, `readelf`, `ranlib`,
  `strip` for inspecting and managing object files during the build.
- **4+ GB disk partition** -- LLVM source (~200 MB) + build artifacts
  (~2 GB) + installed toolchain (~500 MB).

---

### NEW: C++ Exception Handling (not yet on roadmap)

For the self-hosted Clang to be fully functional (not just the cross-compiled
one), the OS needs C++ exception handling support:

- **`libunwind`** -- stack unwinding library
- **`.eh_frame` ELF section** -- the ELF loader must preserve exception
  handling metadata in loaded binaries
- **`libcxxabi`** -- C++ ABI runtime (exception throwing, RTTI)
- **`sigaltstack`** -- alternate signal stack for stack overflow handling

The cross-compiled Clang avoids this because exceptions are statically linked
into the binary itself. But programs *compiled by* Clang that use C++
exceptions need this OS-level support.

---

### NEW: Dynamic Linking Phase (not yet on roadmap, optional)

**What it delivers:**
- ELF dynamic linker (`ld-musl-x86_64.so.1`)
- `dlopen()`, `dlsym()`, `dlclose()`
- PLT/GOT support in the ELF loader
- Position-independent code loading

**Why it helps but isn't required:** Clang can be built fully static. But
with dynamic linking, the installed size drops from ~2 GB to ~400 MB, and
LLVM plugin support (`-load`) works. This is a quality-of-life improvement,
not a hard requirement.

---

## Stage 2 Dependency Graph

```
Stage 1 complete (cross-compiled Clang works)
    |
    +---+---+---+---+
    |   |   |   |   |
    v   v   v   v   v
   P34 P35 P36 P37 P39
   RTC SMP I/O  FS  Threading
    |   |   |   |   |
    +---+---+---+---+
            |
            v
    NEW: Build Infrastructure
    (CMake, Ninja, 4 GB disk)
            |
            v
    NEW: C++ Exception Handling
    (libunwind, .eh_frame, libcxxabi)
            |
            v
    NEW: Dynamic Linking (optional)
            |
            v
    Self-hosted LLVM build inside m3OS
```

## Full Effort Summary

| Stage | Phases Required | Complexity |
|---|---|---|
| **Stage 1: Cross-compiled Clang** | Phase 33 (done), Expanded Memory (new), disk expansion (xtask) | Moderate-high |
| **Stage 2: Self-hosting** | Phases 34-39, Build Infra (new), C++ EH (new), Dynamic Linking (optional) | Very high |

**Stage 1 is achievable after ~2 phases of kernel work** (Phase 33 + Expanded
Memory) plus xtask changes. This is the near-term target.

**Stage 2 requires ~8-9 more phases** on top of Stage 1. This is a long-term
aspiration.

## What We Explicitly Do Not Need (Either Stage)

- **GPU backends** -- LLVM's AMDGPU, NVPTX, etc. can be disabled
- **libffi** -- only needed for LLVM's interpreter (`lli`), not the compiler
- **zlib/zstd** -- compression for debug info; can be disabled
- **XML/JSON parsers** -- only for LLVM's tooling, not core compilation
- **Curses/readline** -- only for interactive LLDB debugger
- **libedit** -- only for LLDB and optional Clang REPL
