# Phase 93 - Dynamic C Runtime (`libc.so` + shared objects)

**Status:** Planned
**Source Ref:** phase-93
**Depends on:** Phase 76 (Dynamic Linker) ✅, Phase 36 (Expanded Memory) ✅, Phase 85c (Python — surfaced the gap), the kernel syscall layer
**Builds on:** Completes the dynamic-linking story begun in Phase 76: the linker *machinery* exists, but m3OS ships no dynamic C library for it to load, so real dynamic C programs (and runtime-`dlopen` users) cannot run. This phase provides the missing `libc.so` and the syscall coverage a dynamic libc needs.
**Primary Components:** a real musl `libc.so` for m3OS (`userspace/ld-musl-x86_64.so.1/` loader, the kernel syscall surface), the `ports/lang/python` recipe (re-enable a dynamic build + real `lib-dynload`), `ctypes`, `docs/python-roadmap.md`

## Milestone Goal

A genuinely **dynamically linked** C program runs on m3OS: a dynamic `python3` (`PT_INTERP=/lib/ld-musl-x86_64.so.1`, `DT_NEEDED libc.so`) boots, resolves its libc symbols against a real on-disk `libc.so`, and loads its C extensions from `lib-dynload/*.so` via `dlopen` at runtime — and `ctypes` can `dlopen` an arbitrary shared object. Static binaries (the Phase 85b `git`, the Phase 85c static `python3`) keep working as the conservative fallback.

## Why This Phase Exists

Phase 85c discovered, by running CPython inside m3OS, that **the dynamic loader works but has nothing to load.** The from-scratch Rust loader (`userspace/ld-musl-x86_64.so.1/`, Phase 76) correctly parses the program's `DT_NEEDED libc.so`, builds the path `/usr/lib/libc.so`, and `sys_open`s it — and gets `-ENOENT`, because **m3OS ships no dynamic `libc.so`**. Its userland is `no_std` Rust; the only musl libc present is the *static* archive that gets linked *into* static binaries. The loader itself exports only `_start`/`_dlstart` — it is a loader, not a C library — so even symlinking `libc.so` to the loader would leave every `malloc`/`memcpy`/`__errno_location` relocation undefined.

The dynamic-link bring-up to date (`libhello.so`, the cyclic `libcyca.so`/`libcycb.so`) used self-contained test libraries that reference no libc symbols, so the gap never surfaced. CPython is the first real C program that needs a libc.

Consequences of the gap, all of which this phase unblocks:
- **Real `lib-dynload`** — CPython's natural extension layout (`*.cpython-*.so` loaded by `dlopen`) instead of the all-static, frozen-`python312.zip` workaround Phase 85c shipped.
- **`ctypes` / cffi** — runtime `dlopen` of arbitrary shared objects.
- **pip C-extension wheels** — compiled wheels are shared objects loaded at runtime.
- **Node.js native addons** (Phase 89) — `.node` files are `dlopen`ed shared objects.
- **A shared-library userland generally** — multiple programs sharing one mapped `libc.so` instead of each statically embedding a copy.

## Learning Goals

- Why, on real musl, `ld-musl-x86_64.so.1` *is* `libc.so` (one file is both the program interpreter and the C library), and what it means that m3OS split them.
- How a dynamic libc differs from a static one at the ABI boundary: the dynamic symbol table, `DT_NEEDED`, copy relocations, IFUNC/`STT_GNU_IFUNC`, TLS (`DT_FLAGS`/initial-exec vs general-dynamic), and `__libc_start_main`/`_start` hand-off.
- Which Linux syscalls a real dynamic libc invokes that a static-only bring-up never exercised (e.g. `mremap`/25, observed unhandled during the 85c run), and how musl degrades on `ENOSYS`.
- How runtime `dlopen` (already implemented for the loader) interacts with a real libc's lazy-binding and constructor/`DT_INIT_ARRAY` ordering.

## Feature Scope

### Area A — Ship a real `libc.so`

Decide and document the libc-provider strategy, then deliver it:

- **Option A1 (recommended baseline): ship the real upstream musl libc as a shared object** built for m3OS's Linux-compatible syscall ABI, installed at `/usr/lib/libc.so` (and the `/lib/ld-musl-x86_64.so.1` interp path). On real musl these are the same artifact; the cleanest path may be to make m3OS's `/lib/ld-musl-x86_64.so.1` *be* upstream musl (loader + libc combined) and retire the loader-only stub for the dynamic path — or to keep the Rust loader and ship a companion `libc.so`.
- **Option A2: grow the Rust loader into a libc** — export the musl C ABI from the existing crate. Far larger surface; likely only if the educational/control goals outweigh reusing upstream musl.

The artifact must export the full musl dynamic symbol set CPython links (`malloc`/`free`/`realloc`, `mem*`/`str*`, `*printf`, `f*`/stdio, `getenv`, `dl*`, `pthread_*`, the syscall wrappers, `__errno_location`, the math `libm` symbols musl folds into libc, etc.) and self-identify with `DT_SONAME = libc.so` so the loader's existing dedup keys on it.

### Area B — Close the syscall gaps a dynamic libc needs

Audit the syscalls a dynamic `libc.so` + CPython exercise that the static bring-up did not, and implement (or deliberately `ENOSYS`-with-rationale) each:

- `mremap` (25) — observed unhandled in the 85c run; musl `realloc` of large `mmap` chunks uses it (falls back to map-copy-unmap on `ENOSYS`, slow). Implement in-place where possible.
- Re-audit `mmap`/`mprotect`/`munmap` flag coverage for the loader's `MAP_*`/`PROT_*` needs (incl. W^X transitions for lazy PLT).
- Anything `__libc_start_main`, TLS setup (`set_thread_area`/`arch_prctl(ARCH_SET_FS)`), and the dynamic constructors touch at startup.

### Area C — Re-enable a dynamic CPython + `lib-dynload` + `ctypes`

Add a dynamic build path to `ports/lang/python` (alongside, or replacing, the static one once dynamic is proven): drop `-static`/`MODULE_BUILDTYPE=static`, build the `*.so` extensions, ship `lib-dynload/`, and re-enable `_ctypes` (provide `libffi` as a port). Validate the dynamic interpreter imports a `lib-dynload` extension and `ctypes.CDLL` opens a shared object inside m3OS.

## Critical and Non-Deferrable Items

| Item | Why it cannot be deferred in this phase |
|---|---|
| A real `libc.so` the loader can map + bind against | Without it the phase has no content — dynamic C still cannot run |
| Syscall coverage the dynamic libc/CPython invoke at startup | A missing startup syscall faults before `main`; the program never runs |
| A boot-validated dynamic program (dynamic `python3`) | "the loader resolved the symbols" must be proven by an interpreter that actually runs, not asserted from the build |
| The static path stays the conservative fallback | Regressing `git` / static `python3` would be a strict loss while dynamic stabilizes |

## Evaluation Gate

| Check | Required state before closing the phase | If missing, add it to this phase |
|---|---|---|
| libc ABI baseline | The shipped `libc.so` exports every symbol the dynamic interpreter references (no undefined-symbol abort) | Add the missing symbols / pick a more complete musl build |
| Syscall baseline | Every syscall the dynamic libc invokes at startup + during the gate is handled (or `ENOSYS` with a musl-tolerated fallback) | Implement the missing syscall or narrow the workload |
| Loader baseline | Phase 76's relocation/PLT/versioning/`dlopen` machinery handles a real libc (copy-relocs, IFUNC, TLS) | Extend the loader for the reloc/TLS shapes a real libc needs |
| Regression baseline | The static `git` and static `python3` gates stay green | Keep the static recipes; gate dynamic behind its own opt-in until stable |

## Important Components and How They Work

### The loader is already done; the library is missing

`userspace/ld-musl-x86_64.so.1/` (Phase 76) implements relocations (`R_X86_64_RELATIVE`/`GLOB_DAT`/`JUMP_SLOT`/`64`), GNU-hash + SysV symbol lookup, symbol versioning, PLT lazy resolve, and `dlopen`/`dlsym`/`dlclose`. `load_dso` (`main.rs`) opens `/usr/lib/<DT_NEEDED-name>` and maps it; `DT_NEEDED not found: libc.so` is a plain `ENOENT` from that open. So this phase adds a **file**, not loader logic — though a real libc will likely exercise reloc/TLS shapes (copy relocations, IFUNC resolvers, general-dynamic TLS) the self-contained test libs never did, so expect targeted loader extensions.

### Linux-compatible syscall ABI

m3OS already uses Linux syscall numbers (the 85c run logged `unhandled syscall 25` = `mremap`), so a real musl built for Linux is mostly ABI-compatible. The work is filling the specific syscalls a dynamic libc relies on, not inventing a new ABI.

### Why static was the right 85c call

A static interpreter embeds libc and never performs a `libc.so` lookup, so it runs today with zero new kernel/loader work — which is exactly why Phase 85c shipped static (matching the static `git` port). This phase is the deliberate, separately-scoped step to lift that constraint rather than smuggling a heavy runtime/loader change into a toolchain phase.

## How This Builds on Earlier Phases

- Consumes Phase 76's dynamic linker (relocations, symbol resolution, `dlopen`) — the machinery that already works.
- Builds on Phase 36's demand-paging/large-mmap baseline and the kernel syscall layer.
- Directly lifts the Phase 85c deferral (dynamic `python3` + real `lib-dynload` + `ctypes`) and is a prerequisite for Phase 89 (Node.js native addons) and a fuller Python (`pip` C-extension wheels).

## Implementation Outline

1. Choose the libc-provider strategy (Area A) and document the non-goals; produce a `libc.so` artifact installed at `/usr/lib/libc.so`.
2. Audit + implement the syscalls a dynamic libc/CPython invoke at startup and during a minimal import (Area B), starting with `mremap`.
3. Bring up a trivial dynamic C "hello" (`DT_NEEDED libc.so`, calls `printf`/`malloc`) end to end before attempting CPython.
4. Add a dynamic build path to `ports/lang/python` + a `libffi` port; build the `lib-dynload` `.so` set + `_ctypes`.
5. Validate inside m3OS: dynamic `python3` imports a `lib-dynload` extension and `ctypes.CDLL` opens a shared object; the static path stays green. Bump `kernel/Cargo.toml` to the next post-1.0 version.

## Learning Documentation Requirement

- Create `docs/93-dynamic-c-runtime.md` using the aligned learning-doc template in `docs/appendix/doc-templates.md`.
- Explain the loader-vs-libc split, the libc-provider decision, the syscall gap audit, and the dynamic-vs-static trade-offs CPython exposed.
- Link it from `docs/README.md` when the phase lands.

## Related Documentation and Version Updates

- Update `docs/python-roadmap.md` (its Stage 2 dynamic-extension assumptions), `docs/roadmap/85c-python.md` (the deferral pointer), `docs/README.md`, and `docs/roadmap/README.md`.
- Update `AGENTS.md`'s **Dynamic linking** capability bullet once a real `libc.so` ships (today it correctly describes a loader without a libc).
- When the phase lands, bump `kernel/Cargo.toml` and any release/version references.

## Acceptance Criteria

- A real `libc.so` is installed at `/usr/lib/libc.so` (self-identifying `DT_SONAME = libc.so`) and the loader maps it for a program that `DT_NEEDED`s it.
- A trivial dynamic C program (`DT_NEEDED libc.so`, `printf`/`malloc`) runs inside m3OS — no `DT_NEEDED not found` and no undefined-symbol abort.
- A **dynamic** `python3` (`PT_INTERP` + `DT_NEEDED libc.so`) boots inside m3OS, prints its version, and imports at least one `lib-dynload` `*.so` extension via `dlopen`.
- `ctypes.CDLL(...)` opens a shared object and calls a function inside m3OS.
- Every syscall the dynamic path invokes is handled or `ENOSYS`-with-musl-fallback (no silent fault); `mremap` is implemented.
- The static `git` and static `python3` gates remain green (no regression of the fallback path).
- The behaviour is validated by a serial gate (a `dynamic-python-smoke` / `ctypes-smoke`), not asserted from the build.

## Companion Task List

- Phase 93 task list — defer until implementation planning begins.

## How Real OS Implementations Differ

- On real musl/Linux, `ld-musl-x86_64.so.1` and `libc.so` are the **same file** — the loader *is* the C library; m3OS split them (a from-scratch Rust loader with no libc counterpart), which is exactly why a dynamic C program has no libc to bind to.
- Distributions ship a full dynamic libc plus a populated `/usr/lib` of shared objects, NSS plugins, `dlopen`-based locale/iconv, and IFUNC-dispatched SIMD variants; this phase targets the subset a dynamic CPython + `ctypes` needs, not the whole ecosystem.
- Glibc compatibility is explicitly out of scope — m3OS is a musl world.

## Deferred Until Later

- glibc ABI compatibility / a glibc `libc.so`.
- `dlopen`-based NSS, locale/iconv module loading, and IFUNC SIMD dispatch beyond what CPython/ctypes need.
- `LD_PRELOAD` / `LD_AUDIT` and the richer dynamic-loader tooling surface.
- A general shared-library SDK for arbitrary third-party `.so` distribution (beyond the validated CPython/ctypes path).
