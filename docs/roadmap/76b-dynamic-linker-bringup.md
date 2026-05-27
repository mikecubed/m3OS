# Phase 76b - Dynamic Linker: `DT_NEEDED` Resolution + Relocations + Constructors

**Status:** Planned
**Source Ref:** phase-76b
**Depends on:** Phase 76 (kernel `PT_INTERP` + ld.so scaffold) ✅
**Builds on:** Replaces the Phase 76 transfer-only `_dlstart` stub with a real bring-up linker that walks `PT_DYNAMIC`, resolves `DT_NEEDED` shared libraries, applies the four core x86_64 relocation types, runs `DT_INIT` / `DT_INIT_ARRAY` constructors, then transfers control to the main binary's entry. Adds `xtask` `.so` build support and the `dynlink_hello` / `libhello.so` end-to-end demo.
**Primary Components:** `userspace/ld-musl-x86_64.so.1/src/dynlink.rs`, `userspace/ld-musl-x86_64.so.1/src/reloc.rs`, `userspace/ld-musl-x86_64.so.1/src/start.rs`, `userspace/lib/libhello/`, `userspace/dynlink_hello/`, `xtask/src/main.rs` (`build_shared_lib`)

## Milestone Goal

A C program built as `dynlink_hello.c` against `libhello.so` (declared in its `DT_NEEDED`) runs end-to-end under m3OS: the kernel honors `PT_INTERP` (Phase 76 ✅), the linker walks the dependency graph, loads `libhello.so` from `/usr/lib/`, applies `R_X86_64_GLOB_DAT` / `R_X86_64_JUMP_SLOT` / `R_X86_64_RELATIVE` / `R_X86_64_64` relocations, runs any constructors (`DT_INIT_ARRAY`), and transfers to `main`. `main` calls `hello_str()` and prints the result to stdout.

## Why This Phase Exists

Phase 76 proved the kernel → ld.so → main handoff with a transfer-only stub: `_dlstart` walks the SysV-ABI stack for `AT_ENTRY` and jumps. That stub cannot resolve a single external symbol, cannot apply a single relocation, and cannot load a single shared library — every `dynlink_smoke` binary so far is `-nostdlib -nostartfiles` with zero `DT_NEEDED` entries.

Real dynamic ELF binaries require a linker that walks `PT_DYNAMIC`, parses the symbol and string tables, loads dependencies, applies relocations against resolved symbol addresses, and runs constructors before entering `main`. Phase 76b is the smallest scope that lifts the system from "static-style PIEs with a passthrough loader" to "real dynamic linking against a shared library written and built in-tree." It deliberately defers `dlopen` / lazy PLT / `DT_GNU_HASH` / versioning to 76c and 76d to keep the bring-up scope bounded.

## Learning Goals

- How a position-independent dynamic linker bootstraps itself before its own GOT is relocated (the `_dlstart` self-relocation chicken-and-egg problem).
- The structure of `PT_DYNAMIC` and the role of `DT_HASH`, `DT_STRTAB`, `DT_SYMTAB`, `DT_RELA`, `DT_JMPREL`, `DT_NEEDED`, `DT_INIT`, `DT_INIT_ARRAY`.
- The four bring-up-critical x86_64 relocation types and what each one writes into the relocated image.
- Dependency-graph resolution: search-path order, refcounting on repeat loads, cycle detection, deepest-first constructor order.
- The host-side `.so` build pipeline (`musl-gcc -shared -fPIC -Wl,--hash-style=sysv`) and how m3OS stages the result onto the ext2 disk and the kernel ramdisk.

## Feature Scope

### `_dlstart` self-relocation

The linker is itself a `-pie` ELF: its own GOT entries must be relocated before any Rust global is read. The Phase 76 stub never touches a global, so it gets away with a one-shot jump; 76b cannot. The new `_dlstart` does the minimum in inline assembly: parses the auxv passed by the kernel to find its own load bias, walks its own `PT_DYNAMIC` to find `DT_RELA` and `DT_RELASZ`, and applies every `R_X86_64_RELATIVE` against itself. Only after self-relocation is complete does control transfer to a real Rust entry (`dl_main`).

### `PT_DYNAMIC` parser + dependency graph

`dynlink.rs` walks `PT_DYNAMIC` and indexes the entries the bring-up scope needs: `DT_NEEDED`, `DT_STRTAB`, `DT_SYMTAB`, `DT_RELA`, `DT_RELASZ`, `DT_JMPREL`, `DT_PLTRELSZ`, `DT_INIT`, `DT_INIT_ARRAY`, `DT_INIT_ARRAYSZ`, `DT_HASH`. `DT_GNU_HASH` is intentionally deferred to 76d, so every artifact built by the 76b pipeline forces `--hash-style=sysv`. The dependency loader resolves each `DT_NEEDED` by searching `LD_LIBRARY_PATH`, `/lib`, `/usr/lib`, `/usr/local/lib` in order, topologically sorts the graph, and detects cycles.

### Relocation application

Eager only — no PLT lazy resolution in 76b (that's 76d). Four relocation types cover the bring-up surface:

- `R_X86_64_RELATIVE` — adjusts an in-image pointer by the load bias. Used by `_dlstart` against itself and by `dynlink.rs` against each loaded DSO.
- `R_X86_64_GLOB_DAT` — writes a resolved symbol's runtime address into a GOT entry.
- `R_X86_64_JUMP_SLOT` — same as `GLOB_DAT` but for PLT entries (resolved eagerly in 76b).
- `R_X86_64_64` — writes a resolved symbol's runtime address plus an addend into an arbitrary location.

### Constructors

`DT_INIT` (if present) then `DT_INIT_ARRAY` (in array order) per DSO, run deepest-dependency-first so that by the time `main` runs every transitive dependency has initialized.

### `xtask` `.so` build support

New `xtask::build_shared_lib(name, srcs, output)` invokes `musl-gcc -shared -fPIC -Wl,--hash-style=sysv` (or `rustc --crate-type cdylib` with equivalent flags for Rust libraries). Output is staged to `target/generated-libs/` and copied to `/usr/lib/` on the ext2 disk by `populate_ext2_files`. Kernel ramdisk embedding follows the same pattern as Phase 76's `ld-musl-x86_64.so.1` so that `/usr/lib/libhello.so` resolves before any disk mount.

### `libhello.so` + `dynlink_hello` end-to-end demo

`libhello.so` exports a single `hello_str()` that returns a static C string. `dynlink_hello` links `-lhello`, calls `hello_str()`, and writes the result to stdout. A new `cargo xtask dynlink-hello-smoke` gate asserts the expected `HELLO_FROM_SHARED_LIB:OK` line on serial.

## Important Components and How They Work

### `userspace/ld-musl-x86_64.so.1/src/start.rs` (`_dlstart`)

Inline-asm entry point. Reads its own load bias from the auxv stack (`AT_BASE`), walks its own `PHDR` array (`AT_PHDR`/`AT_PHNUM`) to locate its own `PT_DYNAMIC`, then walks `DT_RELA` applying every `R_X86_64_RELATIVE` against itself. Only after this completes is it safe to call a Rust function — the very first Rust function called (`dl_main`) is invoked through a register-loaded address, not a GOT slot, so the call site itself does not require a relocated GOT.

### `userspace/ld-musl-x86_64.so.1/src/dynlink.rs` (`dl_main`)

The real Rust entry. Takes the auxv pointer, locates the main binary via `AT_PHDR`/`AT_PHNUM`/`AT_ENTRY`, parses its `PT_DYNAMIC`, walks `DT_NEEDED` to build the dependency list, recursively loads each dependency (refcount on repeat), topologically sorts, applies relocations on every DSO (including the main binary), runs constructors deepest-first, then jumps to `AT_ENTRY`.

### `userspace/ld-musl-x86_64.so.1/src/reloc.rs`

Per-architecture relocation table. For Phase 76b, four x86_64 handlers: `R_X86_64_RELATIVE`, `R_X86_64_GLOB_DAT`, `R_X86_64_JUMP_SLOT`, `R_X86_64_64`. Every handler is a pure-logic function over (relocation entry, load bias, resolved symbol address); pure-logic chunks are host-testable from `kernel-core`-style tests under the `std` feature.

### `xtask::build_shared_lib`

New helper alongside the existing `build_userspace` / `build_musl_static`. Invokes `musl-gcc` with `-shared -fPIC -Wl,--hash-style=sysv` and writes the resulting `.so` to `target/generated-libs/<name>.so`. `populate_ext2_files` then copies every entry from `target/generated-libs/` onto the ext2 disk under `/usr/lib/`.

### `userspace/lib/libhello/`

Single-file C library exporting `const char *hello_str(void)` returning `"HELLO_FROM_SHARED_LIB:OK"`. Built via `xtask::build_shared_lib`.

### `userspace/dynlink_hello/`

Single-file C binary linking `-lhello` (`DT_NEEDED = libhello.so`). `main` writes `hello_str()` to stdout via the existing musl write path. The companion `cargo xtask dynlink-hello-smoke` gate asserts the expected string on serial.

## How This Builds on Earlier Phases

- Extends Phase 76 by replacing the transfer-only `_dlstart` stub with a real bring-up linker that walks `PT_DYNAMIC` and resolves dependencies.
- Reuses the Phase 76 `AT_BASE` / `AT_ENTRY` auxv plumbing in `kernel-core::elf::auxv` — the linker now actually consumes `AT_BASE` to locate itself for self-relocation.
- Reuses the Phase 31 `populate_ext2_files` and Phase 76 ramdisk-embedding patterns to stage `libhello.so` and `dynlink_hello` onto the disk and into the kernel image.
- Reuses the Phase 75 W^X enforcement: every `PT_LOAD` segment in `libhello.so` and `dynlink_hello` is mapped under W^X, and the linker writes relocations through writable mappings without flipping any segment to `PROT_WRITE | PROT_EXEC`.

## Implementation Outline

1. Add `xtask::build_shared_lib` and stage an empty placeholder `.so` to validate the pipeline end-to-end before any linker work.
2. Write `userspace/lib/libhello/` and `userspace/dynlink_hello/` as the target artifacts — these are what 76b is bringing up.
3. Rewrite `_dlstart` to do self-relocation in inline asm; host-test the relocation-application pure-logic helpers from `reloc.rs`.
4. Implement `PT_DYNAMIC` parsing + dependency-graph load + topological sort in `dynlink.rs`, gated initially on the main binary having zero `DT_NEEDED` to keep the surface bounded.
5. Implement the four relocation handlers in `reloc.rs` and apply them across every loaded DSO.
6. Add constructor invocation (`DT_INIT` then `DT_INIT_ARRAY`, deepest-first).
7. Wire `cargo xtask dynlink-hello-smoke` and gate it in the smoke-runner.
8. Bump the kernel to `0.76.1` and write the Phase 76b learning doc.

## Acceptance Criteria

- `dynlink_hello` runs under QEMU and prints `HELLO_FROM_SHARED_LIB:OK` to serial.
- `ld-musl-x86_64.so.1`'s own relocations are correctly applied during `_dlstart` (no panic on first global access).
- A second `dynlink_hello` invocation reuses the same `libhello.so` mapping (refcounted).
- A missing `DT_NEEDED` library logs the name and `execve` returns `NEG_ENOENT`.
- A circular dependency between two test `.so` files is detected and logged; `execve` returns `NEG_ELIBBAD`.
- All Phase 76 acceptance criteria continue to pass.
- Kernel version is `0.76.1`.
- `docs/76b-dynamic-linker.md` learning doc exists and conforms to the aligned legacy learning-doc template.

## Companion Task List

- [Phase 76b Task List](./tasks/76b-dynamic-linker-bringup-tasks.md)

## How Real OS Implementations Differ

- glibc's `ld-linux.so.2` and musl's `ldso/dlstart.c` both do self-relocation in C against a small subset of relocations, leaning on `-fno-stack-protector` and careful PIC discipline rather than a separate `start.rs`. Phase 76b's inline-asm `_dlstart` is the cleaner pedagogical shape for a `no_std` Rust linker.
- Real linkers walk `DT_GNU_HASH` first and fall back to `DT_HASH`; 76b walks only `DT_HASH` and 76d adds `DT_GNU_HASH`. Production binaries built with modern toolchains often ship only `DT_GNU_HASH` — 76b sidesteps this by forcing `--hash-style=sysv` on every artifact it builds.
- Real linkers resolve PLT entries lazily via `_dl_runtime_resolve`; 76b resolves every `JUMP_SLOT` at load time. This trades startup cost for runtime cost and is acceptable in the bring-up phase but is replaced in 76d.
- Real linkers handle TLS (`DT_TLSDESC`, `R_X86_64_DTPMOD64`, `R_X86_64_TPOFF64`), IFUNC, symbol versioning (`DT_VERSYM` / `DT_VERNEED`), and `RTLD_NEXT`. All deferred beyond 76d.

## Deferred Until Later

- `dlopen` / `dlsym` / `dlclose` runtime API → Phase 76c
- PLT lazy resolution (`_dl_runtime_resolve` trampoline) → Phase 76d
- `DT_GNU_HASH` Bloom + bucket + chain → Phase 76d
- `DT_VERSYM` / `DT_VERNEED` graceful handling → Phase 76d
- TLS, `RTLD_NEXT`, namespaces, IFUNC — deferred beyond Phase 76d
