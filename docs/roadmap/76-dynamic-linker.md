# Phase 76 - Dynamic Linker / Shared Libraries (Scaffolding + Handoff)

**Status:** In Progress
**Source Ref:** phase-76
**Depends on:** Phase 11 (Process Model) ✅, Phase 12 (POSIX Compatibility) ✅, Phase 75 (W^X Enforcement) ✅, Phase 31 (TCC Compiler Bootstrap) ✅
**Builds on:** Extends the Phase 11 ELF loader to honor `PT_INTERP`, scaffolds the `ld.so` userspace crate, and proves the kernel→ld.so→main binary handoff end to end. The full dynamic-linker semantics (`DT_NEEDED` resolution, relocations, `dlopen`, GNU hash, PLT lazy resolve) land in subsequent phases.
**Primary Components:** `kernel/src/mm/elf.rs`, `userspace/ld-musl-x86_64.so.1/`, `xtask/src/main.rs`, `userspace/tests/dynlink_smoke/`

## Subphase Split (added during implementation)

Phase 76 turned out to be too large to land in a single PR after design review. It has been split into four subphases so each PR is reviewable and each lands with a green smoke gate:

| Subphase | Scope | Status |
|---|---|---|
| **76** | Kernel `PT_INTERP` + auxv, `ld.so` crate scaffold (no_std PIE), xtask `build_ldso` + stage to `/lib/`, minimal `_dlstart` that just transfers control to the main binary's entry, `dynlink_smoke` test that proves the kernel → ld.so → main handoff | **In Progress** |
| **76b** | Real `ld.so` bring-up: `DT_NEEDED` dependency graph, x86_64 relocations (`GLOB_DAT`/`JUMP_SLOT`/`RELATIVE`/`64`), `DT_INIT`/`DT_INIT_ARRAY` constructors, `libhello.so` + `dynlink_hello` end-to-end | Planned |
| **76c** | `dlopen` / `dlsym` / `dlclose` + `dlopen_test` binary | Planned |
| **76d** | PLT lazy resolution (`_dl_runtime_resolve`), `DT_GNU_HASH`, basic symbol versioning | Planned |

This file documents only the **76 scaffolding** scope. See `76b-dynamic-linker-bringup.md`, `76c-dlopen.md`, `76d-dynamic-linker-polish.md` for the follow-on phases.

## Milestone Goal

A dynamically linked binary (`dynlink_smoke`) carries `PT_INTERP = /lib/ld-musl-x86_64.so.1`. The kernel detects `PT_INTERP`, loads the interpreter at a non-overlapping load bias, builds the full SysV-ABI auxiliary vector on the user stack (`AT_PHDR`, `AT_PHNUM`, `AT_PHENT`, `AT_ENTRY`, `AT_BASE`, `AT_PAGESZ`, `AT_RANDOM`, `AT_NULL`), and transfers control to the interpreter's entry. The interpreter's `_dlstart` self-relocates, locates the main binary's entry via `AT_ENTRY`, and jumps to it. The main binary prints a sentinel to serial and exits cleanly. All existing statically linked binaries continue to function.

## Why This Phase Exists

Dynamic linking is the single largest userspace change in the m3OS roadmap. Cramming it into one PR sacrifices reviewability and forces the smoke gate to wait until the entire stack is bottom-up correct. Splitting along the `PT_INTERP` boundary lets us:

1. land the kernel ELF-loader change with a guaranteed green smoke test (the interpreter binary is just a self-relocating stub),
2. prove the SysV-ABI auxiliary vector matches what musl `_dlstart` expects before any real relocation logic depends on it,
3. give subsequent subphases (76b–76d) a stable, regression-tested foundation to grow against.

SOLID/SRP: this phase delivers exactly two responsibilities — kernel `PT_INTERP` handoff and a userspace crate that proves the handoff works. The `dlopen`/relocation/PLT concerns belong to later subphases and are kept out of this PR's diff.

## Learning Goals

- Understand how `PT_INTERP` changes the kernel's ELF loading path: the kernel still parses the main binary's program headers (to populate `AT_PHDR` / `AT_PHNUM` / `AT_ENTRY`), but transfers control to the interpreter instead of the main binary's `e_entry`.
- See the exact SysV AMD64 ABI initial-stack layout (argc → argv → NULL → envp → NULL → auxv → AT_NULL → string region) and the 16-byte `rsp` alignment requirement at the moment control reaches the interpreter's `_dlstart`.
- Learn how a no_std PIE binary is built (custom target spec, `-Crelocation-model=pic`, `crate-type = ["bin"]` with a linker script that produces `ET_DYN`).
- Observe the kernel→userspace handoff end to end: kernel boots, loads main + interp, transfers to interp's entry, interp self-relocates, interp jumps to `AT_ENTRY`, main prints sentinel.

## Feature Scope (Phase 76 only)

### Kernel `PT_INTERP` parsing and interpreter loading

`kernel/src/mm/elf.rs::load_elf_into` scans program headers for a `PT_INTERP` segment. If present, it reads the interpreter path from the segment content, calls `read_file_from_disk(path)` to fetch the interpreter ELF, parses its headers, and maps its `PT_LOAD` segments into the new process at a load bias chosen to avoid colliding with the main binary. The kernel returns a `LoadedElf` whose `entry` is the **interpreter's** entry point (`interp.e_entry + interp_load_bias`) instead of the main binary's. `AT_ENTRY` carries the main binary's entry so the interpreter can transfer to it after self-relocation.

### Full SysV-ABI auxiliary vector

`setup_abi_stack_with_envp` is extended to take an `interp_load_bias: Option<u64>` and emit `AT_BASE` (interpreter load bias) when present. The auxv ordering matches what musl `_dlstart` reads: `AT_PHDR`, `AT_PHENT`, `AT_PHNUM`, `AT_ENTRY` (main binary entry), `AT_BASE` (interpreter bias, only when `PT_INTERP` was honored), `AT_PAGESZ` (4096), `AT_RANDOM` (16 bytes from kernel CSPRNG, hashed via the existing `0xAB`-pattern seed for determinism in tests), `AT_NULL`. Initial `rsp` is 16-byte aligned at the moment control transfers to the interpreter.

### `ld-musl-x86_64.so.1` crate scaffold (PIE, no_std)

A new workspace crate `userspace/ld-musl-x86_64.so.1/` with:
- `crate-type = ["bin"]`, `#![no_std]`, `#![no_main]`
- Custom target spec `x86_64-m3os-ldso.json` with `relocation-model = "pic"` and `position-independent-executables = true`
- `_dlstart` entry point in inline asm that recovers `argc`/`argv`/`envp`/`auxv` from `rsp`, walks the auxv for `AT_ENTRY`, and jumps to that address with a clean stack
- BSS/data zero-init verified by an early serial log

In this subphase the linker is intentionally a **transfer-only stub**: it does NOT apply relocations, walk `DT_NEEDED`, or run constructors. Those land in 76b.

### `xtask` build pipeline

A new `build_ldso` helper invokes the Rust toolchain with the custom target spec, stages the binary to `target/generated-libs/ld-musl-x86_64.so.1`, and `populate_ext2_files` copies it to `/lib/ld-musl-x86_64.so.1` on the ext2 disk. The directory `/lib` is created if absent. `cargo xtask clean && cargo xtask run` produces a disk that contains `/lib/ld-musl-x86_64.so.1`.

### `dynlink_smoke` test binary

A new test binary `userspace/dynlink_smoke/` built with musl-gcc as a dynamically linked ELF (`PT_INTERP = /lib/ld-musl-x86_64.so.1`, no `DT_NEEDED` entries beyond what the C startup needs). The binary's `main` writes `DYNLINK_SMOKE:PASS` to serial via the existing `sys_write(stdout)` syscall, then exits 0. The smoke harness asserts the sentinel appears.

## Important Components and How They Work

### Kernel ELF loader `PT_INTERP` branch

After the main binary's PT_LOAD scan, `load_elf_into` iterates program headers a second time looking for `PT_INTERP`. If found, it reads the interpreter path bytes, validates that they form a UTF-8 NUL-terminated string, calls `crate::arch::x86_64::syscall::read_file_from_disk(path)` (re-exported via a thin wrapper because `mm::elf` cannot directly depend on the syscall module), and recursively parses + maps the interpreter ELF. The interpreter's load bias is computed as `INTERP_LOAD_BASE_HINT (0x4000_0000)` rounded up past the main binary's highest mapped vaddr to guarantee no collision. The function returns a `LoadedElf` whose `entry` field points at the interpreter's entry; `AT_ENTRY` is populated separately with the main binary's entry.

### Auxiliary vector ordering

The auxv must appear in the exact order musl's `arch/x86_64/crt_arch.h::_dlstart` walks. Order from low addresses upward (after envp NULL):
1. `AT_PHDR` (3) — main binary phdr vaddr
2. `AT_PHENT` (4) — main binary phentsize
3. `AT_PHNUM` (5) — main binary phnum
4. `AT_PAGESZ` (6) — 4096
5. `AT_BASE` (7) — interpreter load bias (only emitted when `PT_INTERP` was honored)
6. `AT_ENTRY` (9) — main binary entry vaddr
7. `AT_RANDOM` (25) — pointer to 16 bytes on the stack
8. `AT_NULL` (0) — sentinel `{0, 0}`

The 16-byte alignment requirement on `rsp` is enforced by padding the pointer table downward so that after `argc` is pushed, the resulting `rsp` is `0 (mod 16)`. (SysV-ABI is "8 mod 16" at function-call boundaries, but `_dlstart` is the program entry point — its convention is `0 mod 16` for the initial stack pointer.)

### `_dlstart` transfer-only stub

```asm
.global _dlstart
_dlstart:
    xor  %rbp, %rbp                  # mark the outermost frame
    mov  %rsp, %rdi                  # pass argv-style stack pointer
    call dlstart_rust                # find AT_ENTRY, returns it in rax
    jmp  *%rax                       # transfer to main binary
```

`dlstart_rust` walks the stack: skip `argc`, skip `argv[0..argc] + NULL`, skip `envp[..] + NULL`, then iterate the auxv looking for `AT_ENTRY`. Returns the value. No allocator, no relocations, no syscalls — pure stack walking.

### `xtask::build_ldso`

Invokes `cargo build --release --package ld-musl-x86_64.so.1 --bin ld-musl-x86_64.so.1 --target userspace/ld-musl-x86_64.so.1/x86_64-m3os-ldso.json -Zbuild-std=core,compiler_builtins`. The resulting binary is copied to `target/generated-libs/ld-musl-x86_64.so.1`. `populate_ext2_files` ensures `/lib` exists on the ext2 disk and stages the binary there with `mode = 0o755`.

## How This Builds on Earlier Phases

- Extends Phase 11's `load_elf_into` with the `PT_INTERP` branch and the auxv `AT_BASE` slot
- Reuses Phase 31's TCC + musl toolchain to build `dynlink_smoke` as a dynamic ELF
- Honors Phase 75 W^X by mapping the interpreter's text `R-X` and data `RW-|NX`, identical to the main-binary path
- Reuses Phase 36's VMA bookkeeping (no new VMA types — the interpreter's segments are recorded the same way as the main binary's)

## Implementation Outline

1. Add `INTERP_LOAD_BASE_HINT` and `compute_interp_load_bias` helpers to `kernel/src/mm/elf.rs`
2. Add `read_interpreter_data` callback indirection (kernel pure-logic vs. VFS read split)
3. Add `PT_INTERP` detection + `map_interp_segments` to `load_elf_into`
4. Extend `setup_abi_stack_with_envp` with `aux_extras: Option<{at_base, at_entry}>`; emit `AT_BASE` + override `AT_ENTRY` when present
5. Scaffold `userspace/ld-musl-x86_64.so.1/` crate, custom target spec, `_dlstart` asm + `dlstart_rust`
6. Add `xtask::build_ldso`, custom-target probe, stage to `target/generated-libs/`
7. Extend `populate_ext2_files` to create `/lib` and stage `ld-musl-x86_64.so.1`
8. Scaffold `userspace/dynlink_smoke/` (musl-gcc dynamic ELF), wire into `xtask::build_musl_bins`
9. Add `dynlink-smoke` xtask gate that boots m3OS, drives `dynlink_smoke`, asserts `DYNLINK_SMOKE:PASS` on serial
10. Update Phase 11 design doc (note the `PT_INTERP` extension); the Phase 12 `dlopen` deferral is closed by 76c, not 76
11. Bump kernel version to 0.76.0 and author `docs/76-dynamic-linker.md` learner doc

## Acceptance Criteria

- A musl-built `dynlink_smoke` binary with `PT_INTERP = /lib/ld-musl-x86_64.so.1` runs under QEMU and prints `DYNLINK_SMOKE:PASS` to serial
- The serial log shows the kernel "elf: PT_INTERP=/lib/ld-musl-x86_64.so.1 interp_bias=0x..." line followed by "ldso: _dlstart entry=0x..." followed by `DYNLINK_SMOKE:PASS`
- `readelf -h target/generated-libs/ld-musl-x86_64.so.1` reports `Type: DYN`
- `readelf -d target/generated-initrd/dynlink_smoke` shows a `PT_INTERP` segment pointing at `/lib/ld-musl-x86_64.so.1`
- All existing static-binary smoke tests (`cargo xtask smoke-test`) continue to pass — no regression on the no-`PT_INTERP` path
- `cargo xtask check` and `cargo xtask test` are clean
- `cargo test -p kernel-core` includes new auxv-layout host tests that pin the byte-exact stack layout

## Companion Task List

- [Phase 76 Task List](./tasks/76-dynamic-linker-tasks.md)

## How Real OS Implementations Differ

- glibc's `ld-linux-x86-64.so.2` does substantially more than transfer to `AT_ENTRY`: it walks the main binary's `PT_DYNAMIC`, builds the dependency graph, applies all relocations, runs constructors, and only then jumps. m3OS phases 76b–76d add these layers incrementally.
- Production kernels populate `AT_HWCAP`, `AT_PLATFORM`, `AT_CLKTCK`, `AT_UID`/`AT_EUID`/`AT_GID`/`AT_EGID`, `AT_EXECFN`, and `AT_SECURE` in the auxv. m3OS emits only the eight entries musl's `_dlstart` actually reads in 76; the rest land if/when a real libc binary needs them.
- Linux's interpreter-load bias is randomized for ASLR (`mmap_rnd_bits`); m3OS uses a fixed `0x4000_0000` hint in 76 with the option to randomize later.

## Deferred Until Later

- `DT_NEEDED` resolution, dependency graph, topological sort → Phase 76b
- x86_64 relocations (`R_X86_64_GLOB_DAT`, `R_X86_64_JUMP_SLOT`, `R_X86_64_RELATIVE`, `R_X86_64_64` inside the interpreter and inside loaded `.so` files) → Phase 76b (the existing `R_X86_64_RELATIVE` path in `load_elf_into` is unaffected and still runs for kernel-loaded PIE binaries)
- `DT_INIT` / `DT_INIT_ARRAY` constructor running → Phase 76b
- `dlopen` / `dlsym` / `dlclose` → Phase 76c
- PLT lazy resolution (`_dl_runtime_resolve`) → Phase 76d
- `DT_GNU_HASH` / `DT_HASH` symbol lookup → Phase 76d
- `DT_VERSYM` / `DT_VERNEED` symbol versioning → Phase 76d
- Thread-local storage (TLS) blocks, `RTLD_NEXT`, namespace isolation (`dlmopen`), IFUNC — deferred beyond Phase 76d
