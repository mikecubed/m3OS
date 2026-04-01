# Road to Clang/LLVM on m3OS

This document details the phases and work required to run Clang/LLVM natively
inside m3OS. Clang is an enormous C/C++ compiler frontend (~30M lines of C++)
backed by the LLVM optimizer and code generator. For comparison, TCC (our
current compiler) is ~100K lines of C.

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

## What Clang/LLVM Requires

| Requirement | Why | Current Status |
|---|---|---|
| C++ runtime (libc++/libstdc++) | Clang is written in C++ | Not available |
| Exception handling + RTTI | C++ features used throughout LLVM | Not available |
| Threading (pthreads) | LLVM uses threads for parallel compilation passes | Not available |
| Real `mmap`/`munmap` | LLVM's allocator relies on mmap for large regions | `munmap` is a stub |
| ~500 MB+ RAM for compilation | LLVM's IR and symbol tables are memory-hungry | Kernel heap caps at 64 MiB |
| ~2 GB disk space | LLVM installed size with headers/libraries | ext2 partition is 128 MB |
| Dynamic linking (optional) | Clang/LLVM is modular with shared libraries | Not available |
| A real linker (lld or GNU ld) | TCC's built-in linker cannot handle LLVM's output | Not available |
| Symlinks | LLVM build system expects symlinks for tool aliases | Not available |
| `/proc` filesystem | Some LLVM features read `/proc/self/exe` | Not available |
| `select`/`poll`/`epoll` | Build parallelism and I/O multiplexing | Not available |
| Wall-clock time | Build timestamps, profiling | Not available |

## Phase-by-Phase Path

### Phase 33 -- Kernel Memory Improvements (in progress)

**Status:** Almost complete. Assumed ready for the rest of this plan.

**Delivers:**
- Slab allocator for O(1) fixed-size kernel object allocation
- Buddy allocator for page-granularity allocations (4 KiB to 2 MiB)
- OOM retry -- grows heap on demand instead of panicking
- Working `munmap()` -- actually reclaims physical frames
- Userspace heap coalescing to reduce fragmentation
- Heap diagnostics

**Clang relevance:** Foundation for all subsequent memory work. Without
working `munmap()` and OOM recovery, no large compiler can run.

---

### Phase 34 -- Real-Time Clock (planned)

**Delivers:** CMOS RTC driver, wall-clock time, `CLOCK_REALTIME`,
`gettimeofday()`.

**Clang relevance:** Build systems (make, CMake) depend on file timestamps
for incremental builds. LLVM's `__DATE__`/`__TIME__` macros need wall-clock
time. Profiling and build timing require real timestamps.

---

### Phase 35 -- True SMP Multitasking (planned)

**Delivers:** Per-core syscall stacks, SMP-aware scheduling with load
balancing, priority levels.

**Clang relevance:** LLVM compilation is CPU-intensive. Spreading work
across cores matters for build times, but is not strictly required -- Clang
can compile single-threaded, just slowly.

---

### Phase 36 -- I/O Multiplexing (planned)

**Delivers:** `select()`, `epoll`, non-blocking I/O, `O_NONBLOCK`.

**Clang relevance:** Parallel build systems (`make -j`, `ninja`) use
non-blocking I/O to manage multiple compiler processes. Without this, builds
are strictly sequential.

---

### Phase 37 -- Filesystem Enhancements (planned)

**Delivers:** Symlinks, hard links, `/proc` filesystem, permission
enforcement on all paths, device nodes (`/dev/null`, `/dev/zero`).

**Clang relevance:**
- LLVM's build system creates symlinks for tool aliases (`clang++` -> `clang`)
- `/proc/self/exe` is used by Clang to locate its own resource directory
- `/dev/null` is needed for discarding output during builds
- Without symlinks, the entire LLVM tool naming convention breaks

---

### Phase 38 -- Unix Domain Sockets (planned)

**Delivers:** `AF_UNIX` stream and datagram sockets, `socketpair()`.

**Clang relevance:** Minor direct relevance. Some build tools and test
harnesses use Unix domain sockets for IPC, but Clang itself does not require
them. Useful for running LLVM's test suite (lit) which communicates between
test processes.

---

### Phase 39 -- Threading Primitives (planned)

**Delivers:** `clone(CLONE_THREAD)`, `futex()`, thread-local storage (TLS),
thread groups, `pthread_create`/`join`/`mutex`/`cond` via musl.

**Clang relevance:** **Critical.** LLVM uses threads for:
- Parallel code generation (splitting work across ThinLTO threads)
- Parallel header parsing
- Thread-safe data structures throughout the codebase
- pthreads mutexes for concurrent access to shared state

LLVM can technically be built with `-DLLVM_ENABLE_THREADS=OFF`, but this
disables major functionality and is not the normal mode of operation.

---

### NEW PHASE: Expanded Memory (not yet on roadmap)

**Suggested phase number:** 49 or insert between 39 and 40.

**What it delivers:**
- **Demand paging / lazy allocation** -- map virtual pages without backing
  physical frames until first access (page fault handler allocates on demand)
- **Kernel heap ceiling raised** to 256 MiB+ or made fully dynamic
- **Per-process address space limits** enforced via `setrlimit()`
- **Large `mmap()` support** -- allocations up to hundreds of megabytes for
  LLVM's IR buffers, symbol tables, and JIT memory
- **`mprotect()`** -- change page permissions (needed for JIT: `RW` -> `RX`)
- **Optional: swap to disk** -- page out cold memory to virtio-blk when
  physical RAM is exhausted

**Clang relevance:** **Critical.** A real Clang compilation of even a modest
source file allocates hundreds of megabytes via `mmap()`. Without demand
paging and large virtual allocations, LLVM will OOM immediately. This is
arguably the single biggest blocker.

---

### NEW PHASE: C++ Toolchain (not yet on roadmap)

**Suggested phase number:** 50 or wherever it fits after threading.

**What it delivers:**
- **Cross-compile a C++ compiler** -- either GCC's C++ frontend (`g++`) or
  a minimal Clang built on the host, statically linked against musl and
  a C++ standard library
- **C++ standard library** -- port `libc++` (LLVM's) or `libstdc++` (GNU),
  statically linked. Must support:
  - Exceptions (`__cxa_throw`, personality routines, `.eh_frame` unwinding)
  - RTTI (`typeid`, `dynamic_cast`)
  - STL containers, `<algorithm>`, `<iostream>`, `<string>`, `<memory>`
  - `<thread>`, `<mutex>`, `<atomic>` (requires Phase 39 threading)
- **`libunwind`** -- stack unwinding library for C++ exception handling
- **ELF `.eh_frame` support** -- the ELF loader must parse and preserve
  exception handling metadata
- **C++ ABI** -- Itanium C++ ABI (used by both GCC and Clang on x86_64)

**Clang relevance:** **Absolute prerequisite.** Clang/LLVM is written in
C++17. Without a working C++ runtime, Clang cannot even load, let alone
compile anything.

**Approach options:**
1. **Cross-compile Clang on host** -- build a static `clang` binary on
   Linux using `musl-gcc`/`musl-clang` and copy it to the disk image. This
   avoids needing to bootstrap a C++ compiler inside the OS, but still
   requires the C++ runtime and exception handling support.
2. **Port GCC first** -- GCC's C frontend is written in C (self-hosting),
   so TCC could potentially compile it. Then use GCC to build its own C++
   frontend. Then use G++ to build LLVM. This is the traditional bootstrap
   path but extremely laborious.
3. **Use a simpler C++ compiler** -- `cxx` (a minimal C++ to C translator)
   or `cfront`-style approaches as stepping stones.

---

### NEW PHASE: Dynamic Linking (not yet on roadmap)

**Suggested phase number:** 51 or wherever appropriate.

**What it delivers:**
- **ELF dynamic linker** (`ld.so` / `ld-musl-x86_64.so.1`)
- **Shared library loading** -- `dlopen()`, `dlsym()`, `dlclose()`
- **PLT/GOT** -- procedure linkage table and global offset table support
  in the ELF loader
- **`RPATH`/`RUNPATH`** -- library search paths
- **Position-independent code** -- `mmap()` shared libraries at arbitrary
  addresses

**Clang relevance:** Not strictly required (Clang can be built fully
static), but LLVM's plugin architecture and Clang's `-load` flag for
analysis plugins require dynamic linking. The installed size is also much
smaller with shared libraries (~400 MB vs ~2 GB static).

---

### NEW PHASE: Expanded Disk and Build Infrastructure (not yet on roadmap)

**Suggested phase number:** 52 or wherever appropriate.

**What it delivers:**
- **Larger disk images** -- ext2 partition expanded to 4+ GB
- **CMake or Ninja** -- LLVM's build system requires CMake; bare-minimum
  Ninja support would also work
- **Python or a CMake interpreter** -- CMake generates build files; either
  port Python 3 (CMake uses it for LLVM's build) or implement a minimal
  CMake-to-Makefile converter
- **GNU binutils or llvm-tools** -- `nm`, `objdump`, `readelf`, `ranlib`,
  `strip` for inspecting and managing object files
- **A real linker** -- GNU `ld` or LLVM's `lld`; TCC's linker cannot handle
  LLVM's large object files and link-time requirements

**Clang relevance:** You cannot build LLVM without CMake. The build produces
thousands of object files and requires a production-grade linker. The final
linked binary alone exceeds 100 MB.

---

## Dependency Graph

```
Phase 33 (Kernel Memory)          -- IN PROGRESS
    |
    v
Phase 34 (Real-Time Clock)        -- planned
    |
    v
Phase 35 (True SMP)               -- planned
    |
    v
Phase 36 (I/O Multiplexing)       -- planned
    |
    v
Phase 37 (Filesystem Enhancements) -- planned
    |
    v
Phase 38 (Unix Domain Sockets)    -- planned
    |
    v
Phase 39 (Threading Primitives)   -- planned, CRITICAL for LLVM
    |
    +-----+-----+
    |           |
    v           v
NEW: Expanded   NEW: C++ Toolchain
Memory          (cross-compiled C++ compiler,
(demand paging,  libc++, libunwind,
mprotect, large  exception handling)
mmap, optional       |
swap)                |
    |                |
    +-------+--------+
            |
            v
    NEW: Dynamic Linking (optional)
            |
            v
    NEW: Expanded Disk and
         Build Infrastructure
         (CMake, 4 GB disk,
          real linker, binutils)
            |
            v
      Clang/LLVM runs natively
```

## Effort Estimate

| Category | Phases | Rough Complexity |
|---|---|---|
| Already planned (33-39) | 7 phases | Significant but scoped |
| Expanded memory | 1 new phase | Very high -- demand paging is one of the hardest kernel features |
| C++ toolchain | 1 new phase | Very high -- exception handling and ABI are deeply complex |
| Dynamic linking | 1 new phase (optional) | High -- ELF dynamic linker is non-trivial |
| Build infrastructure | 1 new phase | Moderate -- mostly porting existing tools |

**Total new phases needed beyond current roadmap: 3-4.**
**Total phases before Clang runs: ~11** (33 through 39, plus 3-4 new ones).

## Alternative: Cross-Compiled Clang (Shortcut)

If the goal is "Clang binary runs inside m3OS" rather than "m3OS can build
Clang from source", the path is shorter:

1. Complete Phases 33-39 (memory, threading, filesystem, I/O)
2. Complete Expanded Memory phase (demand paging, large mmap)
3. Cross-compile a static Clang binary on the host with musl + static libc++
4. Bundle it on the disk image (like we do with TCC today)
5. Expand the ext2 partition to hold Clang + headers + libraries

This skips the C++ bootstrap, dynamic linking, and CMake phases entirely.
You still need the runtime support (threading, memory, filesystem), but you
avoid the hardest problem: building LLVM inside the OS.

## What We Explicitly Do Not Need

- **GPU support** -- LLVM's GPU backends (AMDGPU, NVPTX) can be disabled
- **libffi** -- only needed for LLVM's interpreter, not the compiler
- **zlib/zstd** -- compression for debug info; can be disabled
- **XML/JSON parsers** -- only for LLVM's tooling, not core compilation
- **Curses/readline** -- only for interactive LLDB debugger
