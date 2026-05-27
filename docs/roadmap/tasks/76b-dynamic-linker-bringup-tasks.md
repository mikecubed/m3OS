# Phase 76b — Dynamic Linker: `DT_NEEDED` Resolution + Relocations: Task List

**Status:** Planned
**Source Ref:** phase-76b
**Depends on:** Phase 76 ✅
**Goal:** Replace the Phase 76 transfer-only `_dlstart` stub with a real bring-up linker that resolves `DT_NEEDED`, applies the four core x86_64 relocations, runs constructors, and supports building `.so` files in `xtask`.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| B1 | `_dlstart` self-relocation in inline asm before any Rust global access | Phase 76 ✅ | Planned |
| B2 | `PT_DYNAMIC` parser + `DT_NEEDED` dependency graph + topological sort | B1 | Planned |
| B3 | x86_64 relocation application (`R_X86_64_GLOB_DAT`, `R_X86_64_JUMP_SLOT`, `R_X86_64_RELATIVE`, `R_X86_64_64`) | B2 | Planned |
| B5 | `DT_INIT` / `DT_INIT_ARRAY` constructors, deepest-first | B3 | Planned |
| E3 | `xtask::build_shared_lib(name, srcs, output)` + stage to `/usr/lib/` | Phase 31 ✅ | Planned |
| F1 | `libhello.so` + `dynlink_hello` + xtask gate | B5, E3 | Planned |
| H | Design-doc updates + version bump | All | Planned |

## Documentation Notes

- The original (pre-split) Phase 76 task list's B.1 / B.2 / B.3 / B.5 / E.1 (`build_shared_lib` portion) / F.1 acceptance items migrate here verbatim.
- B.4 (PLT lazy resolve `_dl_runtime_resolve`) is intentionally **not** in 76b — it lands in 76d. 76b applies relocations eagerly at load time.
- D.1 (`DT_GNU_HASH`) is intentionally **not** in 76b — 76b uses `DT_HASH` (the older flat-bucket format) only. Both `libhello.so` and the linker's own dynamic section will be built with `-Wl,--hash-style=sysv` to force `DT_HASH`.
