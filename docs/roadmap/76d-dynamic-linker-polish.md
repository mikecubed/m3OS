# Phase 76d - Dynamic Linker: PLT Lazy Resolve + GNU Hash + Symbol Versioning

**Status:** Planned
**Source Ref:** phase-76d
**Depends on:** Phase 76 ✅, Phase 76b, Phase 76c
**Builds on:** Adds the performance and compatibility polish layer that lets m3OS load glibc-built and musl-built `.so` files in the wild: PLT lazy resolution (`_dl_runtime_resolve`), `DT_GNU_HASH` lookup, and graceful `DT_VERSYM` / `DT_VERNEED` handling. Completes the Phase 76 dynamic-linker theme.
**Primary Components:** `userspace/ld-musl-x86_64.so.1/src/plt.rs`, `userspace/ld-musl-x86_64.so.1/src/sym.rs`, `userspace/ld-musl-x86_64.so.1/src/ver.rs`

## Milestone Goal

A `.so` file built with `-Wl,--hash-style=gnu` (the modern default for glibc and musl toolchains) loads and runs end-to-end under m3OS: `DT_GNU_HASH` resolves every symbol, PLT entries resolve lazily on first call via `_dl_runtime_resolve`, and a versioned `.so` (one with `DT_VERSYM` and `DT_VERNEED` present) loads with exact-version matches against its dependencies. The Phase 76 family is now production-shape complete.

## Why This Phase Exists

Phase 76b and 76c restrict the bring-up to `--hash-style=sysv` (`DT_HASH`) artifacts and resolve every relocation eagerly. That works for in-tree libraries we control — `libhello.so` is built with the right flags — but it cannot load a wild `.so` plucked from a Debian or Alpine package, because:

1. Modern toolchains default to `--hash-style=gnu` (or `--hash-style=both`); glibc's `libc.so.6` ships only `DT_GNU_HASH`.
2. Eager PLT resolution makes startup costly: the linker has to resolve every imported function whether or not it gets called. For a binary that imports 800 libc symbols but uses 40, that's ~95% of the resolution work wasted.
3. Versioned symbols (`DT_VERSYM` / `DT_VERNEED`) are universal in glibc-built `.so` files. Without graceful handling, every such library either fails to load or resolves to the wrong implementation.

Phase 76d closes all three gaps. The 76b `DT_HASH` path stays in place as a fallback; the 76c `dlopen` path is unaffected at the API level but now benefits from lazy resolution and GNU hash internally.

## Learning Goals

- The PLT/GOT contract: how the first call goes through `_dl_runtime_resolve`, how the trampoline rewrites the GOT slot, and why subsequent calls bypass the trampoline entirely.
- The shape of `DT_GNU_HASH` (Bloom filter + bucket + chain) and why it is faster than `DT_HASH` (single-bucket chain).
- Symbol versioning: `DT_VERSYM` indexes the binary's own symbols by version, `DT_VERNEED` declares the versions a binary requires from each dependency, `DT_VERDEF` declares the versions a library provides.
- W^X discipline at the trampoline boundary: the GOT must be writable (the resolver writes to it) but the resolved jump target must remain non-writable.
- The `LD_BIND_NOW` environment variable as the canonical "diagnose-now-not-later" knob.

## Feature Scope

### `_dl_runtime_resolve` PLT trampoline

A short x86_64 asm stub installed at `GOT[2]` by the linker. The PLT's plt0 stub jumps through `GOT[2]`, passing the link-map pointer (`GOT[1]`) and the relocation index on the stack. `_dl_runtime_resolve`:

1. Saves all caller-saved registers (the function being called from the PLT may not yet have a stack frame).
2. Calls into Rust `plt::resolve_pltrel(link_map, reloc_index)` which returns the resolved address.
3. Writes the resolved address into the GOT slot for this PLT entry.
4. Restores caller-saved registers.
5. Jumps to the resolved address (so the caller never knows the trampoline ran).

### `DT_GNU_HASH` lookup

A three-stage check that is faster than `DT_HASH` because most lookups short-circuit at the Bloom filter:

1. Compute the GNU hash of the symbol name (a variant of djb2: `h = h * 33 + c` over each byte).
2. Probe the Bloom filter; if it says "definitely not present," return immediately.
3. Index the bucket array by `hash % nbuckets` to find the chain start.
4. Walk the chain — each entry is a symbol-table index; stop at the first chain entry whose top bit is set.

### `DT_HASH` fallback

When a library has only `DT_HASH`, the 76b SysV-hash path is used. When a library has both, GNU is preferred. When a library has only `DT_GNU_HASH`, GNU is required. The dispatcher is a single function in `sym.rs` so all consumers (load-time relocation, `dlsym`, lazy-resolve callback) share one code path.

### `DT_VERSYM` / `DT_VERNEED` graceful handling

For each undefined symbol in `DT_VERNEED`, the linker has a `(library, version-name)` tuple. When resolving the symbol, the linker:

1. Walks the providing library's `DT_VERSYM` table to find symbols whose version matches.
2. Returns the matched symbol's address.
3. If no exact-version match is found, falls back to an unversioned lookup and emits a `log::warn!` recording the version mismatch.
4. If neither finds a match, the symbol is treated as unresolved (existing 76b/76c behavior).

This "warn but don't fail on mismatch" policy lets m3OS load `.so` files whose versioning data refers to glibc-specific symbol versions that have no m3OS equivalent. Strict mode is reserved for `LD_BIND_NOW=1`.

### `LD_BIND_NOW` environment-variable handling

`LD_BIND_NOW=1` in the environment forces every PLT entry to resolve eagerly at load time, matching the 76b/76c behavior. This is useful for surfacing missing-symbol errors at startup rather than at first call.

## Important Components and How They Work

### `userspace/ld-musl-x86_64.so.1/src/plt.rs`

Houses the `_dl_runtime_resolve` asm trampoline and the Rust resolver callback. The asm side is `#[naked]` with a precise calling-convention contract (the PLT calls it with two stack arguments and expects the function never to clobber any register that isn't explicitly saved). The Rust resolver is a thin wrapper around `sym::lookup` that writes the resolved address into the GOT and returns it to the asm.

### `userspace/ld-musl-x86_64.so.1/src/sym.rs`

The unified symbol-lookup module. Replaces the inline `DynamicSection::lookup_symbol` from 76b with a structured `lookup(scope, name, version) -> Option<ResolvedSymbol>` API that internally dispatches between GNU-hash and SysV-hash paths. The 76c `dlsym` path is rerouted through here so both load-time and runtime resolution share one code path.

### `userspace/ld-musl-x86_64.so.1/src/ver.rs`

`DT_VERSYM` / `DT_VERNEED` / `DT_VERDEF` parsing. Builds a `VersionTable` per DSO that records the required version names for each undefined symbol and the provided version names for each defined symbol. `sym::lookup` consults this table when a version constraint is present.

### GOT-slot-rewrite W^X invariant

The trampoline writes into the GOT — so the GOT region must be mapped `RW-` (writable, not executable). The resolved jump target lives in the called function's `.text` section, which is mapped `R-X` (read+exec, not writable). The acceptance gate asserts both invariants by checking the page tables at runtime; if either is violated, the assert fails. This is the third explicit W^X gate in the dynamic-linker family (the first two are the relocation-writes-into-rodata check from 76b and the destructor-runs-in-RX check from 76c).

## How This Builds on Earlier Phases

- Reuses the Phase 76b dependency-graph and constructor machinery in full; 76d only adds new lookup and resolution paths inside the existing flow.
- Replaces the eager `apply_jmprel_table` path from 76b: 76d defers `JUMP_SLOT` relocations to first call, falling back to eager only when `RTLD_NOW` was passed to `dlopen` or `LD_BIND_NOW=1` is set.
- Reuses the Phase 76c `dlopen` API surface unchanged — only the internal lookup path changes (rerouted through `sym::lookup`).
- Reuses the Phase 75 W^X enforcement as the explicit acceptance gate for the GOT-rewrite path.

## Implementation Outline

1. Write `sym.rs` as a refactor of 76b's `DynamicSection::lookup_symbol` with the new structured API; keep `DT_HASH` as the only backend so behavior does not change. Route 76b's load-time-relocation path and 76c's `dlsym` through `sym::lookup`.
2. Add `DT_GNU_HASH` parsing + Bloom + bucket + chain walk; switch the lookup dispatch to prefer GNU.
3. Add `ver.rs` with `DT_VERSYM` / `DT_VERNEED` / `DT_VERDEF` indexing; thread version names through `sym::lookup`.
4. Write `_dl_runtime_resolve` asm trampoline + `plt::resolve_pltrel` Rust callback. Install the trampoline address at `GOT[2]` at load time.
5. Switch the 76b eager `JUMP_SLOT` path to lazy when the DSO is not opened with `RTLD_NOW` and `LD_BIND_NOW` is unset.
6. Add the `--hash-style=gnu` gate variant (a new `libhello_gnu.so` built with GNU hash + a `dynlink_hello_gnu` consumer).
7. Bump the kernel to `0.76.3`; mark the Phase 76 family Complete in the roadmap README; extend `docs/76-dynamic-linker.md` with the lazy-resolve + GNU-hash + versioning sections.

## Acceptance Criteria

- A `.so` built with `-Wl,--hash-style=gnu` loads and its symbols resolve correctly.
- The first call to a PLT-protected function goes through `_dl_runtime_resolve`; subsequent calls bypass the trampoline (GOT entry now holds the resolved address). Verified by an in-binary check that reads the GOT slot before and after the first call.
- A `.so` with versioned symbols (`DT_VERSYM` present) loads without aborting; an unresolvable version logs a warning but does not block the load.
- The GOT slot updated by the trampoline lives in a `RW-` region; the resolved jump target lives in `R-X` (W^X compliant).
- `LD_BIND_NOW=1` forces eager resolution and surfaces missing symbols at load time.
- All Phase 76, 76b, and 76c acceptance criteria continue to pass.
- Kernel version is `0.76.3`.
- The Phase 76 family is marked Complete in the roadmap README.
- `docs/76-dynamic-linker.md` covers PLT lazy resolution, GNU hash, and symbol versioning.

## Companion Task List

- [Phase 76d Task List](./tasks/76d-dynamic-linker-polish-tasks.md)

## How Real OS Implementations Differ

- glibc's `_dl_runtime_resolve` is hand-written asm in `sysdeps/x86_64/dl-trampoline.h` with separate xmm/avx variants. 76d ships a single non-SIMD variant; m3OS already disables SSE in target flags (`-mmx,-sse`) so the AVX variant has nothing to save.
- glibc's `DT_GNU_HASH` walker uses prefetch hints and Bloom filters tuned for very large symbol tables (`libc.so.6` exports ~3000 symbols). 76d uses a simpler implementation; performance tuning is deferred.
- glibc honors `LD_BIND_NOW=1` (force eager resolution) and `LD_BIND_NOT` (don't update GOT — diagnostic mode). 76d honors `LD_BIND_NOW` only.
- glibc supports indirect functions (`STT_GNU_IFUNC`) — symbols that resolve at runtime via a constructor-style indirect call. 76d does not; IFUNC is deferred indefinitely.
- glibc supports `DT_FILTER` and `DT_AUXILIARY` (filter libraries that delegate symbol resolution). 76d does not.

## Deferred Until Later

- `STT_GNU_IFUNC` indirect functions — deferred indefinitely
- `DT_FILTER` / `DT_AUXILIARY` — deferred indefinitely
- TLS (`DT_TLSDESC`, `R_X86_64_DTPMOD64`, `R_X86_64_TPOFF64`) — separate phase
- `dlmopen` / namespace-isolated linking — deferred indefinitely
- Per-symbol weak-binding override (`STB_WEAK` interaction with version constraints) — partial support in 76d, full support deferred
- AVX/xmm-saving variant of `_dl_runtime_resolve` — not applicable while m3OS disables SIMD
- `LD_BIND_NOT` diagnostic mode — deferred indefinitely
