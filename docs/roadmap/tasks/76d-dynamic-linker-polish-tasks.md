# Phase 76d — Dynamic Linker: PLT Lazy + GNU Hash + Versioning: Task List

**Status:** Planned
**Source Ref:** phase-76d
**Depends on:** Phase 76 ✅, Phase 76b, Phase 76c
**Goal:** Add PLT lazy resolution, `DT_GNU_HASH` lookup, and graceful symbol-versioning handling.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| B4 | `_dl_runtime_resolve` asm trampoline + GOT slot rewrite | Phase 76b | Planned |
| D1 | `DT_GNU_HASH` Bloom + bucket + chain lookup | Phase 76b | Planned |
| D2 | `DT_VERSYM` / `DT_VERNEED` graceful handling | D1 | Planned |
| F | New gate variant: a `.so` built with `-Wl,--hash-style=gnu` runs end-to-end | B4, D1 | Planned |
| H | docs/76-dynamic-linker.md polish-pass; mark Phase 76 family Complete | All | Planned |

## Documentation Notes

- The original (pre-split) Phase 76 task list's B.4 / D.1 / D.2 acceptance items migrate here verbatim.
- The Phase 75 W^X invariant for PLT trampolines (GOT in `RW-`, resolved target in `R-X`) is reaffirmed here as the acceptance gate for B4.
