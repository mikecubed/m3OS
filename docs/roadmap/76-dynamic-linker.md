# Phase 76 - Dynamic Linker / Shared Libraries

**Status:** Planned
**Source Ref:** phase-76
**Depends on:** Phase 11 (Process Model) ✅, Phase 12 (POSIX Compatibility) ✅, Phase 75 (W^X Enforcement) ✅, Phase 31 (TCC Compiler Bootstrap) ✅
**Builds on:** Extends the Phase 11 ELF loader to honor `PT_INTERP`, bringing up musl's `ld.so` as the runtime linker and enabling shared library (`DT_NEEDED`) resolution for the first time
**Primary Components:** `kernel/src/elf/`, `userspace/ld-musl-x86_64.so.1/`, `xtask/src/main.rs`, `userspace/syscall-lib`, `userspace/tests/dynlink_hello/`, `userspace/tests/dlopen_test/`

## Milestone Goal

m3OS supports dynamic linking via `PT_INTERP` and a ported or purpose-written `ld.so`. A program that declares a `DT_NEEDED` shared library dependency resolves it from `/lib` or `/usr/lib` at runtime; `dlopen`/`dlsym`/`dlclose` work from a C program; a `tcc -shared`-produced `.so` loads and executes correctly. This unblocks Phase 87 (Node.js) native modules and Phase 88 (Claude Code) dependencies.

## Why This Phase Exists

Every m3OS binary today is statically linked: the runtime, libc, and all dependencies are baked into the binary at link time. This works for small, purpose-built OS binaries, but it makes porting toolkit-class software (GTK, Qt, browsers, scripting runtimes) impractical — those projects expect to link against shared libraries and use `dlopen` for plugin loading.

The audit (§ F6) identified dynamic linking as a Stage-3 gap. Phase 75 W^X enforcement is a prerequisite because a dynamic linker's relocation pass must map GOT/PLT as `RW-` and text as `R-X`; the W^X machinery must be correct before the linker exercises it. The `wayland-gap-analysis.md` document lists dynamic linking as the first prerequisite for every Wayland path.

SOLID/SRP: the dynamic linker lives entirely in userspace (`ld.so`) and the kernel contributes only `PT_INTERP` loading and auxiliary vector construction — a clean boundary with a single concern on each side. Dependency inversion: `ld.so` resolves `DT_NEEDED` against `LD_LIBRARY_PATH` before the standard search paths, meaning the runtime composition strategy is injected at launch rather than baked into the binary at compile time. TDD: auxiliary vector parsing is pure logic that host-tests cleanly in `kernel-core`; for `dlopen`/`dlsym`/`dlclose`, write failing host tests against a fixture `.so` first, then implement until they pass — musl's own test suite transfers directly if the port route is taken.

## Learning Goals

- Understand how a dynamic linker resolves `DT_NEEDED` entries from `PT_DYNAMIC` and loads shared objects
- Learn how the GOT (Global Offset Table) and PLT (Procedure Linkage Table) enable lazy symbol resolution
- See how `PT_INTERP` changes the kernel's ELF loading behavior: the kernel loads the interpreter, not the binary directly
- Understand how `dlopen`/`dlsym`/`dlclose` are implemented on top of the dynamic linker's symbol table
- Learn the build-system implications: separate compilation units, position-independent code (`-fPIC`), `.so` output format

## Feature Scope

### `PT_INTERP` honored in the kernel ELF loader

When the kernel's ELF loader encounters a `PT_INTERP` segment, it loads the interpreter binary (typically `/lib/ld-musl-x86_64.so.1`) instead of executing the main binary directly. It maps the interpreter into the process address space at a load bias, passes the auxiliary vector (`AT_PHDR`, `AT_PHNUM`, `AT_ENTRY`, `AT_BASE`, etc.) on the initial stack, and transfers control to the interpreter's entry point. The interpreter then loads the main binary, resolves its dependencies, and transfers to the main binary's entry point.

### `ld.so` bring-up

Port musl's `ldso` (from the musl libc source tree) or implement a fresh dynamic linker following musl's design. The linker must: parse `PT_DYNAMIC` to find `DT_NEEDED`, `DT_SONAME`, `DT_RPATH`/`DT_RUNPATH`; load each needed shared object from `LD_LIBRARY_PATH` and the standard search paths (`/lib`, `/usr/lib`, `/usr/local/lib`); apply relocations (`R_X86_64_GLOB_DAT`, `R_X86_64_JUMP_SLOT`, `R_X86_64_RELATIVE`, `R_X86_64_64`); run `DT_INIT` and `DT_INIT_ARRAY` constructors; then transfer to the main binary's entry point. The linker itself is position-independent and maps its own text as `R-X`, GOT as `RW-`, compliant with Phase 75 W^X rules.

### `dlopen` / `dlsym` / `dlclose`

The dynamic linker exports a `libdl`-compatible interface: `dlopen(path, flags)` loads and relocates an additional shared object at runtime, returning a handle; `dlsym(handle, name)` searches the shared object's exported symbol table and returns the address; `dlclose(handle)` decrements the reference count and unmaps when it reaches zero. `RTLD_LAZY` defers PLT resolution to the first call; `RTLD_NOW` resolves immediately. `RTLD_GLOBAL` adds the library's symbols to the global lookup scope.

### Symbol versioning (basic)

musl uses limited `DT_GNU_HASH` and `DT_VERSYM`/`DT_VERNEED` entries. The linker must not crash on versioned symbols; it should resolve them using the baseline (unversioned) lookup if exact version matching is not implemented in this phase.

### Build-system support

`xtask` gains the ability to compile shared libraries (`.so` output) for in-tree libraries that benefit. Initially: `userspace/lib/crypto-lib/` and `userspace/lib/passwd_lib/` can optionally be built as `.so` files. `xtask` calls `tcc -shared` (Phase 31 TCC) to produce the `.so`; the `.so` files are placed in the ext2 data disk under `/usr/lib/`. The kernel ramdisk does not need to embed `.so` files because they are loaded at runtime from the filesystem.

### Test application

A small `userspace/tests/dynlink_hello/` binary links against a `libhello.so` (also in the test directory). `libhello.so` exports one function `hello_str()` returning a static string. The test binary calls `hello_str()` and writes the result to stdout. A separate `userspace/tests/dlopen_test/` binary uses `dlopen`/`dlsym` to call `hello_str()` at runtime.

## Important Components and How They Work

### Kernel ELF loader `PT_INTERP` branch

In `kernel/src/elf/loader.rs`, a new branch after segment scanning: if `PT_INTERP` is present, load the interpreter ELF from the filesystem path given in the segment content, compute a load bias for the interpreter, and map its PT_LOAD segments into the new process's address space. Build the auxiliary vector on the user stack: `AT_PHDR` (address of the main binary's program headers), `AT_PHNUM`, `AT_ENTRY` (main binary's `e_entry`), `AT_BASE` (interpreter load bias), `AT_PAGESZ`, `AT_RANDOM` (16 random bytes), `AT_NULL`. Set the initial instruction pointer to the interpreter's entry point.

### musl `ldso` or fresh implementation

The design closely follows musl's `ldso` (`musl/ldso/dynlink.c`). Key internal data structures: `struct dso` (one per loaded shared object, linked in load order), symbol hash tables (`DT_GNU_HASH` preferred over `DT_HASH`), a global symbol table for `RTLD_GLOBAL` scope. Relocation application is architecture-specific: only x86_64 is in scope. Constructor order follows topological sort of the dependency graph (deepest dependency runs first).

### PLT lazy resolution stubs

Each PLT entry initially jumps to the linker's `_dl_runtime_resolve` trampoline. On first call, the trampoline looks up the symbol in the dependency graph, writes the resolved address into the GOT slot, and jumps to the function. Subsequent calls go directly to the function via the pre-filled GOT slot. This requires the GOT to be mapped `RW-` and the PLT to be mapped `R-X` — both consistent with Phase 75 W^X rules.

### `xtask` `.so` build path

A new `build_shared_lib(name, srcs, output)` helper in `xtask/src/main.rs`. For Rust-built shared libraries, `rustc` is invoked with `--crate-type cdylib`. For C-built shared libraries, `tcc -shared` is used. Output `.so` files are staged to `target/generated-libs/` and then embedded in the ext2 data disk by `populate_ext2_files`.

## How This Builds on Earlier Phases

- Extends Phase 11's ELF loader with the `PT_INTERP` branch and auxiliary vector construction
- Depends on Phase 75 W^X enforcement to ensure the linker's own GOT/PLT use the correct protection flags
- Depends on Phase 31 TCC for `tcc -shared` to build the test `.so` files
- Reuses Phase 36's VMA tracking to record the shared library load regions
- Phase 12 POSIX compatibility provides `dlopen`/`dlsym` in the libc surface; this phase delivers the actual implementation behind those symbols

## Implementation Outline

1. Add `PT_INTERP` detection and interpreter loading to `kernel/src/elf/loader.rs`
2. Implement auxiliary vector construction and stack setup for the interpreter entry path (SysV-ABI `argc / argv / NULL / envp / NULL / auxv / AT_NULL` ordering, 16-byte aligned `rsp`)
3. Scaffold the `userspace/ld-musl-x86_64.so.1/` crate: `no_std`, `-fPIC` / `-Crelocation-model=pic`, output `ET_DYN` ELF; wire into `xtask::build_userspace` and stage to `target/generated-libs/`
4. Port musl `ldso` or write a fresh dynamic linker inside that crate; copy the resulting binary to `/lib/ld-musl-x86_64.so.1` on the ext2 data disk via `populate_ext2_files`
5. Implement `DT_NEEDED` resolution, dependency graph construction, and topological load order
6. Implement x86_64 relocation types: `R_X86_64_GLOB_DAT`, `R_X86_64_JUMP_SLOT`, `R_X86_64_RELATIVE`, `R_X86_64_64`
7. Implement PLT lazy-resolution trampoline (`_dl_runtime_resolve`)
8. Implement `dlopen`/`dlsym`/`dlclose` on top of the linker's symbol table
9. Add `.so` build support to `xtask`; build `libhello.so` test library
10. Write `dynlink_hello` and `dlopen_test` test binaries; register in xtask and ramdisk
11. Update Phase 11 and Phase 12 design docs; mark dynamic-linking deferrals as closed
12. Bump the kernel version to 0.76.0 and author `docs/76-dynamic-linker.md` learning doc

## Acceptance Criteria

- `tcc -shared` produces a working `.so` from a C source file; the binary loads and `hello_str()` returns the expected string
- A dynamically linked binary runs correctly under QEMU: `PT_INTERP` triggers interpreter load; `ld.so` resolves `DT_NEEDED` from `/usr/lib/`
- `dlopen("libhello.so") → handle`, `dlsym(handle, "hello_str") → fn_ptr`, `fn_ptr()` returns the expected string
- `dlclose(handle)` decrements the reference count; a second `dlopen` of the same path reuses the already-loaded DSO
- All existing statically linked in-tree binaries continue to function (they have no `PT_INTERP` and are unaffected)

## Companion Task List

- [Phase 76 Task List](./tasks/76-dynamic-linker-tasks.md)

## How Real OS Implementations Differ

- glibc's `ld.so` implements full GNU symbol versioning, `RTLD_NEXT`, `__attribute__((constructor))` ordering, TLS (thread-local storage) blocks, and lazy IFUNC resolution; m3OS's linker defers all of these to post-Phase-76
- Linux's `ld.so` uses a namespace model (multiple independent symbol scopes) for plugin isolation; m3OS uses a single flat global scope in this phase
- Production dynamic linkers use `mmap` to memory-map `.so` files directly (zero-copy load from disk cache); m3OS may use a simpler read-then-map approach in the first version
- musl's `ldso` deliberately avoids `malloc` during early load to prevent initialization order issues; a fresh implementation must observe the same constraint

## Deferred Until Later

- Thread-local storage (TLS) blocks for `__thread` variables — requires per-thread TLS block allocation and `FS` register setup per thread
- GNU symbol versioning beyond the basic "don't crash" handling
- `RTLD_NEXT` (search scope continuation) — needed for libc interposition patterns
- Namespace isolation (`dlmopen`) — needed for plugin sandbox isolation
- Lazy IFUNC resolution — needed for glibc-compatible CPU feature dispatch (not a musl dependency)
- Shared library build support for languages beyond C and Rust `cdylib` (e.g., C++ with static constructors) — deferred to when a C++ runtime lands
