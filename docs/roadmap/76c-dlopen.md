# Phase 76c - Dynamic Linker: `dlopen` / `dlsym` / `dlclose`

**Status:** Planned
**Source Ref:** phase-76c
**Depends on:** Phase 76 ✅, Phase 76b
**Builds on:** Adds the runtime plugin-loading API (`dlopen`, `dlsym`, `dlclose`) on top of the Phase 76b dependency-graph + relocation machinery. Required for Node.js native modules (Phase 87) and any application that loads backend implementations at runtime.
**Primary Components:** `userspace/ld-musl-x86_64.so.1/src/dl.rs`, `userspace/dlopen_test/`

## Feature Scope

- `dlopen(path, flags)` with `RTLD_LAZY` / `RTLD_NOW` / `RTLD_GLOBAL`
- `dlsym(handle, name)` searching the loaded DSO + `RTLD_DEFAULT` global scope
- `dlclose(handle)` reference-counting + `DT_FINI` / `DT_FINI_ARRAY` running on last close
- `dlerror()` storage (process-global slot until TLS lands)
- `dlopen_test` binary exercising the full open → sym → call → close cycle

## Acceptance Criteria

- `dlopen_test` opens `/usr/lib/libhello.so`, calls `dlsym("hello_str")`, calls the function, and asserts the returned string matches; `dlclose` returns 0
- A second `dlopen` of the same path increments the refcount and returns the same handle
- `dlsym` on a missing symbol returns NULL and `dlerror()` returns a non-empty message
- `dlclose` on a never-opened handle returns -1 and `dlerror()` is populated

## Companion Task List

- [Phase 76c Task List](./tasks/76c-dlopen-tasks.md)
