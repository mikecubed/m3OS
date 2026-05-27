# Phase 76c - Dynamic Linker: `dlopen` / `dlsym` / `dlclose`

**Status:** Planned
**Source Ref:** phase-76c
**Depends on:** Phase 76 ✅, Phase 76b
**Builds on:** Adds the runtime plugin-loading API (`dlopen`, `dlsym`, `dlclose`, `dlerror`) on top of the Phase 76b dependency-graph + relocation machinery. Required for Node.js native modules (Phase 87) and any application that loads backend implementations at runtime.
**Primary Components:** `userspace/ld-musl-x86_64.so.1/src/dl.rs`, `userspace/ld-musl-x86_64.so.1/src/handle.rs`, `userspace/dlopen_test/`

## Milestone Goal

A C program built as `dlopen_test.c` can call `dlopen("/usr/lib/libhello.so", RTLD_NOW)`, look up `hello_str` via `dlsym`, call the function and print the result, then `dlclose` the handle and exit cleanly. Re-opening the same library returns the same handle with a higher refcount. Missing-symbol and missing-library paths populate `dlerror()` with a meaningful message and return `NULL` / `-1` as appropriate. `DT_FINI_ARRAY` and `DT_FINI` run on the last-close of a library.

## Why This Phase Exists

Phase 76b brings up `DT_NEEDED` resolution: every shared library a binary depends on is loaded at process-start time, before `main`. That suffices for static plugin shapes (compile-time-known dependencies), but it cannot load a plugin chosen at runtime — a configuration-driven backend (e.g. "use the OpenGL backend if available, fall back to software"), a JIT runtime's native-module loader (Node.js, Python C extensions), or a debugger's per-binary instrumentation hook.

Phase 76c lifts the linker from "load-time graph resolution" to "load-time + on-demand graph resolution." The same dependency-graph, relocation, and constructor machinery from 76b is reused — 76c adds the four runtime entry points (`dlopen` / `dlsym` / `dlclose` / `dlerror`), refcounted handle management, and `DT_FINI` / `DT_FINI_ARRAY` invocation on last-close.

## Learning Goals

- The distinction between load-time linking (`DT_NEEDED`) and runtime linking (`dlopen`) and why both shapes are needed.
- The `RTLD_LAZY` / `RTLD_NOW` / `RTLD_GLOBAL` / `RTLD_LOCAL` flag semantics and how each affects symbol resolution and visibility.
- Reference counting on shared-library handles: the open/close balance that lets `dlclose` actually unmap.
- The destructor pipeline (`DT_FINI_ARRAY` in reverse-array order then `DT_FINI`) and why it runs on last-close, not every close.
- The libdl ABI as the OS-stable plugin contract.

## Feature Scope

### `dlopen(path, flags)`

Opens a shared library by absolute path or by name (searching the standard load paths if no `/` is present). Honored flags:

- `RTLD_NOW` — apply every relocation eagerly at open time; surfaces unresolved symbols as a failed `dlopen` rather than a runtime crash later.
- `RTLD_LAZY` — defer PLT relocations until first call. In 76c this is implemented identically to `RTLD_NOW` because PLT lazy resolution does not land until Phase 76d.
- `RTLD_GLOBAL` — the opened library's exported symbols are visible to subsequent symbol searches across the whole process.
- `RTLD_LOCAL` (default) — exported symbols are only visible via the returned handle.

Repeat `dlopen` of the same `SONAME` increments the existing handle's refcount and returns the same handle.

### `dlsym(handle, name)`

Resolves a symbol name to a runtime address. Two handle shapes:

- A real handle returned by `dlopen` — search starts in that library and walks its dependency chain.
- `RTLD_DEFAULT` (passed as a sentinel `NULL`) — searches the global scope (all `RTLD_GLOBAL`-opened libraries plus the main binary).

Returns `NULL` on not-found; `dlerror()` is populated with a typed message.

### `dlclose(handle)`

Decrements the refcount. When the refcount drops to zero:

1. Run `DT_FINI_ARRAY` in reverse-array order.
2. Run `DT_FINI` (if present).
3. Remove the DSO from the global scope and the load list.
4. Unmap the DSO's `PT_LOAD` segments.

Closing a never-opened or already-freed handle returns `-1` and populates `dlerror()`.

### `dlerror()`

Returns the last libdl error message (or `NULL` if there is none). Calling `dlerror()` clears the slot — a subsequent call without an intervening libdl failure returns `NULL`. Storage is a process-global `static mut` slot until TLS lands; the thread-safety gap is documented in `docs/76-dynamic-linker.md`.

## Important Components and How They Work

### `userspace/ld-musl-x86_64.so.1/src/dl.rs`

The libdl entry-point module. Exports `dlopen`, `dlsym`, `dlclose`, `dlerror` as `#[no_mangle] extern "C"` functions. Each wraps a `Mutex`-guarded `DlState` carrying the list of `LoadedDso` records shared with 76b's load-time linker, a `BTreeMap<DsoId, u32>` of refcounts, and a `DlError` slot. The same `dynlink::load_needed` and `reloc::apply_rela_table` paths from 76b are invoked by `dlopen` — the only new code is handle bookkeeping, flag interpretation, and the destructor pipeline.

### `userspace/ld-musl-x86_64.so.1/src/handle.rs`

The handle table. `dlopen` returns an opaque `*mut c_void` that is in fact a pointer to a `Handle { dso_id: DsoId, generation: u32 }` allocated in a kernel-style slab. `dlclose` validates the handle by checking the generation matches the live `LoadedDso` generation; this catches forged or already-freed handles.

### Destructor pipeline

On last-close (`refcount == 0`), `dlclose` iterates `DT_FINI_ARRAY` in reverse-array order then calls `DT_FINI` (if present). Destructors are called as `extern "C" fn()` through a register-loaded address, matching the constructor convention from 76b. The unmap happens after destructors return — a destructor that captures a function pointer from its own DSO can still call it.

### `userspace/dlopen_test/`

Single-file C binary that exercises the full open → sym → call → close cycle against `libhello.so`. Also exercises the four negative paths (missing library, missing symbol, double-close, close of never-opened handle).

## How This Builds on Earlier Phases

- Reuses the Phase 76b `dynlink::load_needed`, `reloc::apply_rela_table`, and `dynlink::run_constructors` paths in their entirety — `dlopen` is structurally a wrapper around the load-time entry point with refcount bookkeeping bolted on.
- Reuses the Phase 76b `DT_HASH` symbol lookup; 76d will reroute `dlsym` through the new GNU-hash dispatcher.
- Reuses the Phase 76b W^X-compliant mapping behavior; no new mapping shapes are introduced.
- Reuses the Phase 75 `NEG_ENOENT` / error-code convention for missing libraries.
- Closes the "`dlopen` is not yet implemented" gap called out in `docs/12-posix-compatibility-layer.md`.

## Implementation Outline

1. Add `DlState` + `handle.rs` slab; stub out `dlopen` / `dlsym` / `dlclose` / `dlerror` to return well-defined errors so the linkage shape is in place before any real work.
2. Wire `dlopen` to call `dynlink::load_needed` for new opens; refcount-increment for repeat opens; honor `RTLD_GLOBAL` / `RTLD_LOCAL` scope insertion.
3. Wire `dlsym(handle, name)` to call the existing `DT_HASH` lookup against the handle's scope; wire `dlsym(RTLD_DEFAULT, name)` to search the global scope.
4. Implement the destructor pipeline in `dlclose` (`DT_FINI_ARRAY` reverse-order then `DT_FINI`) and the unmap-after-destructors ordering.
5. Wire `dlerror()` with the process-global slot.
6. Write `dlopen_test` and the `cargo xtask dlopen-test-smoke` gate.
7. Bump the kernel to `0.76.2`; extend `docs/76-dynamic-linker.md` with the libdl section; update `docs/12-posix-compatibility-layer.md` to remove the `dlopen` not-yet-implemented entry.

## Acceptance Criteria

- `dlopen_test` opens `/usr/lib/libhello.so`, calls `dlsym("hello_str")`, calls the function, and asserts the returned string matches; `dlclose` returns 0.
- A second `dlopen` of the same path increments the refcount and returns the same handle.
- `dlsym` on a missing symbol returns `NULL` and `dlerror()` returns a non-empty message; a subsequent `dlerror()` call returns `NULL`.
- `dlclose` on a never-opened handle returns `-1` and `dlerror()` is populated.
- A library with a `DT_FINI_ARRAY` entry runs the destructor on last-close (verified by the destructor writing a sentinel that the test then asserts).
- All Phase 76 and 76b acceptance criteria continue to pass.
- Kernel version is `0.76.2`.
- `docs/76-dynamic-linker.md` covers the libdl runtime entry points.
- `docs/12-posix-compatibility-layer.md` no longer lists `dlopen` as not-yet-implemented.

## Companion Task List

- [Phase 76c Task List](./tasks/76c-dlopen-tasks.md)

## How Real OS Implementations Differ

- glibc and musl both implement libdl as inline functions in the dynamic linker itself, not as a separate `libdl.so`. The 76c shape matches that — the libdl entry points live in `ld-musl-x86_64.so.1` and not in a separate library. Programs that link `-ldl` find their symbols via a stub `.so` that redirects to the linker.
- glibc supports `dlmopen` (namespaced loading) and `RTLD_DEEPBIND` (per-DSO symbol-scope inversion). 76c implements neither; both are deferred indefinitely.
- glibc's `dlerror` is thread-local from glibc 2.0 onwards; 76c uses a process-global slot until TLS lands.
- glibc supports `dlinfo()` for handle introspection and `dladdr()` for reverse address-to-symbol lookup. 76c does not.

## Deferred Until Later

- PLT lazy resolution (so that `RTLD_LAZY` defers PLT entries) → Phase 76d
- `DT_GNU_HASH`-preferred symbol lookup for `dlsym` → Phase 76d
- `dlmopen` / `RTLD_DEEPBIND` / link-namespaces — deferred beyond Phase 76d
- Thread-local `dlerror()` storage — gated on TLS implementation
- `dladdr()` / `dlinfo()` — deferred indefinitely
