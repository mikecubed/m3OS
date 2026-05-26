# Phase 76b - Dynamic Linker: `DT_NEEDED` Resolution + Relocations + Constructors

**Status:** Planned
**Source Ref:** phase-76b
**Depends on:** Phase 76 (kernel `PT_INTERP` + ld.so scaffold) ✅
**Builds on:** Replaces the Phase 76 transfer-only `_dlstart` stub with a real bring-up linker that walks `PT_DYNAMIC`, resolves `DT_NEEDED` shared libraries, applies the four core x86_64 relocation types, runs `DT_INIT` / `DT_INIT_ARRAY` constructors, then transfers control to the main binary's entry. Adds `xtask` `.so` build support and the `dynlink_hello` / `libhello.so` end-to-end demo.
**Primary Components:** `userspace/ld-musl-x86_64.so.1/src/dynlink.rs`, `userspace/ld-musl-x86_64.so.1/src/reloc.rs`, `userspace/lib/libhello/`, `userspace/dynlink_hello/`, `xtask/src/main.rs` (`build_shared_lib`)

## Milestone Goal

A C program built as `dynlink_hello.c` against `libhello.so` (declared in its `DT_NEEDED`) runs end-to-end under m3OS: the kernel honors `PT_INTERP` (Phase 76 ✅), the linker walks the dependency graph, loads `libhello.so` from `/usr/lib/`, applies `R_X86_64_GLOB_DAT` / `R_X86_64_JUMP_SLOT` / `R_X86_64_RELATIVE` / `R_X86_64_64` relocations, runs any constructors (`DT_INIT_ARRAY`), and transfers to `main`. `main` calls `hello_str()` and prints the result to stdout.

## Feature Scope

- `_dlstart` self-relocation in inline asm before any Rust global access (the linker is a `-pie` ELF; its own GOT must be relocated before constants are read)
- `PT_DYNAMIC` parsing: `DT_NEEDED`, `DT_STRTAB`, `DT_SYMTAB`, `DT_RELA`, `DT_RELASZ`, `DT_JMPREL`, `DT_PLTRELSZ`, `DT_INIT`, `DT_INIT_ARRAY`, `DT_INIT_ARRAYSZ`, `DT_HASH` (basic flat lookup for 76b; `DT_GNU_HASH` lands in 76d)
- Dependency-graph walk: load each `DT_NEEDED` from `LD_LIBRARY_PATH`, `/lib`, `/usr/lib`, `/usr/local/lib`; topological sort; detect cycles
- x86_64 relocations applied eagerly (no PLT lazy resolve — that's 76d): `R_X86_64_GLOB_DAT`, `R_X86_64_JUMP_SLOT`, `R_X86_64_RELATIVE`, `R_X86_64_64`
- Constructors: `DT_INIT` then `DT_INIT_ARRAY` per DSO, deepest-dependency-first
- `xtask::build_shared_lib(name, srcs, output)` calling `musl-gcc -shared -fPIC` or `rustc --crate-type cdylib`; output staged to `target/generated-libs/` and copied to `/usr/lib/` on the ext2 disk
- `libhello.so` (one exported `hello_str()` returning a static C string) + `dynlink_hello` (links `-lhello`)
- `dynlink_hello` xtask gate asserts the expected string on serial

## Acceptance Criteria

- `dynlink_hello` runs under QEMU and prints `HELLO_FROM_SHARED_LIB:OK` to serial
- `ld-musl-x86_64.so.1`'s own relocations are correctly applied during `_dlstart` (no panic on first global access)
- A second `dynlink_hello` invocation reuses the same `libhello.so` mapping (refcounted)
- A missing `DT_NEEDED` library logs the name and `execve` returns `NEG_ENOENT`
- A circular dependency between two test `.so` files is detected and logged; `execve` returns `NEG_ELIBBAD`
- All Phase 76 acceptance criteria continue to pass

## Companion Task List

- [Phase 76b Task List](./tasks/76b-dynamic-linker-bringup-tasks.md)

## Deferred Until Later

- `dlopen` / `dlsym` / `dlclose` runtime API → Phase 76c
- PLT lazy resolution (`_dl_runtime_resolve` trampoline) → Phase 76d
- `DT_GNU_HASH` Bloom + bucket + chain → Phase 76d
- `DT_VERSYM` / `DT_VERNEED` graceful handling → Phase 76d
- TLS, `RTLD_NEXT`, namespaces, IFUNC — deferred beyond Phase 76d
