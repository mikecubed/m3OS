# Phase 76d - Dynamic Linker: PLT Lazy Resolve + GNU Hash + Symbol Versioning

**Status:** Planned
**Source Ref:** phase-76d
**Depends on:** Phase 76 ✅, Phase 76b, Phase 76c
**Builds on:** Adds the performance and compatibility polish layer that lets m3OS load glibc-built and musl-built `.so` files in the wild: PLT lazy resolution (`_dl_runtime_resolve`), `DT_GNU_HASH` lookup, and graceful `DT_VERSYM`/`DT_VERNEED` handling.
**Primary Components:** `userspace/ld-musl-x86_64.so.1/src/plt.rs`, `userspace/ld-musl-x86_64.so.1/src/sym.rs`

## Feature Scope

- `_dl_runtime_resolve` x86_64 asm trampoline: saves all caller-saved regs, calls Rust symbol-resolution, writes the resolved address into the GOT slot, jumps to the function
- `DT_GNU_HASH` lookup: Bloom filter + bucket + chain (the format glibc/musl `.so` files use by default)
- `DT_HASH` fallback for older `.so` files without GNU hash
- `DT_VERSYM` / `DT_VERNEED` graceful handling: exact-version match where present, fall back to unversioned lookup, log a warning if neither resolves

## Acceptance Criteria

- A `.so` built with `-Wl,--hash-style=gnu` loads and its symbols resolve correctly
- The first call to a PLT-protected function goes through `_dl_runtime_resolve`; subsequent calls bypass the trampoline (GOT entry now holds the resolved address)
- A `.so` with versioned symbols (`DT_VERSYM` present) loads without aborting; an unresolvable version logs a warning but does not block the load
- The GOT slot updated by the trampoline lives in a `RW-` region; the resolved jump target lives in `R-X` (W^X compliant)

## Companion Task List

- [Phase 76d Task List](./tasks/76d-dynamic-linker-polish-tasks.md)
