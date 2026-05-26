# Phase 76c — Dynamic Linker: `dlopen` / `dlsym` / `dlclose`: Task List

**Status:** Planned
**Source Ref:** phase-76c
**Depends on:** Phase 76 ✅, Phase 76b
**Goal:** Ship a libdl-compatible `dlopen` / `dlsym` / `dlclose` on top of the Phase 76b dependency-graph + relocation machinery.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| C1 | `dlopen(path, flags)` with `RTLD_LAZY` / `RTLD_NOW` / `RTLD_GLOBAL` | Phase 76b | Planned |
| C2 | `dlsym(handle, name)` + `dlclose(handle)` with `DT_FINI`/`DT_FINI_ARRAY` | C1 | Planned |
| F2 | `dlopen_test` binary + xtask gate | C2 | Planned |
| H | Phase 12 doc closure (`dlopen` not-yet-implemented entry) + docs/76-dynamic-linker.md update | F2 | Planned |

## Documentation Notes

- The original (pre-split) Phase 76 task list's C.1 / C.2 / F.2 acceptance items migrate here verbatim.
- `dlerror()` storage uses a process-global `static mut` slot until TLS lands (see original Phase 76 task list "Documentation Notes" for the deferred-TLS rationale).
