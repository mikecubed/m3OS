# Dynamic C Runtime (`libc.so` + shared objects)

**Aligned Roadmap Phase:** Phase 93
**Status:** In Progress
**Source Ref:** phase-93
**Supersedes Legacy Doc:** N/A (new capability)

## Overview

Phase 93 ships m3OS's first **real dynamic C library** — an upstream musl
`libc.so` the Phase 76 loader maps and binds a genuinely dynamically-linked
program against — and closes the loader/kernel gaps a real libc exercises. The
headline result: a dynamic `python3` (`PT_INTERP=/lib/ld-musl-x86_64.so.1`,
`DT_NEEDED libc.so`) boots, loads its C extensions from `lib-dynload/*.so` via
`dlopen`, and `ctypes.CDLL(...)` opens a shared object and calls a function.
The static `git` / static `python3` paths stay the conservative green fallback.

This doc is the pedagogical companion to the implementation-focused
[design doc](./roadmap/93-dynamic-c-runtime.md): it teaches **why m3OS splitting
the loader and the C library is the crux of the whole phase**, and what
"startup glue" a from-scratch loader must provide that musl's own `ld.so`
normally does.

## What This Doc Covers

- Why, on real musl, `ld-musl-x86_64.so.1` **is** `libc.so` — and what it cost
  m3OS to split them (a Rust loader with no libc counterpart).
- The libc-provider decision (Option A1: ship upstream musl as a companion
  `libc.so`, keep the Rust loader as the interp).
- The **six bring-up surprises** booting a real dynamic binary surfaced — each
  a piece of the startup glue the foreign loader must supply.
- How `dlopen` routes to the loader (not libc), enabling `lib-dynload`/`ctypes`.

## Core Implementation

### The loader and the library are normally the same file

On real musl/Linux, `/lib/ld-musl-x86_64.so.1` and `libc.so` are the **same
ELF** — the program interpreter *is* the C library. It self-bootstraps:
`_dlstart` → `__dls2` → `__dls3` self-relocates, sets up TLS, builds the dynamic
symbol scope, runs constructors, and hands off to the program. Because the
loader and libc are one image, all of musl's internal startup state (the TLS
template, the constructor queue, the loaded-DSO list `dlopen` walks) is
naturally initialized.

m3OS **split them**: Phase 76 wrote a from-scratch Rust loader
(`userspace/ld-musl-x86_64.so.1/`) that is a *loader, not a C library*. So when
Phase 85c ran CPython, the loader parsed `DT_NEEDED libc.so`, built the path
`/usr/lib/libc.so`, `open`'d it — and got `-ENOENT`, because **m3OS shipped no
dynamic `libc.so`**. Phase 93 ships that file. But shipping the file is the
*easy* half: the foreign loader must also provide the startup glue musl's own
`ld.so` would have, because the shared `libc.so` is built **expecting `__dls3`
to have run**.

### The libc-provider decision (Option A1)

We build **upstream musl 1.2.5 `--disable-static --enable-shared`** into a
companion `/usr/lib/libc.so` (`DT_SONAME=libc.so`, via `-Wl,-soname,libc.so`,
which musl's default link omits) and **keep the Rust loader** as the interp.
Rejected: growing the Rust loader into a C-ABI libc (Option A2) — a far larger
surface than reusing upstream musl, sacrificing the loader's bounds-checked,
W^X, GNU-hash assets. Non-goals: no glibc; musl's own `ld.so` is *not*
installed.

### The six bring-up surprises (the startup glue)

Booting a real dynamic binary surfaced six issues — exactly the "loader
provides the TLS/startup musl's own ld normally does" work the design doc
predicted for A1. The first is a plain capacity bug; the rest are the
loader-vs-libc split made concrete:

1. **64 KiB scratch.** `load_dso` read the whole DSO into a fixed 64 KiB buffer;
   `libc.so` is 711 KiB. Now `lseek(SEEK_END)`-sizes the scratch to the file.
2. **Weak undefined symbols.** crt objects emit weak refs
   (`_ITM_registerTMCloneTable`, `__gmon_start__`) that real libc never defines;
   the loader treated an unresolved symbol as a hard error. A `value==0`
   `STB_WEAK` reference now resolves to 0 (the consumer guards `if (sym) sym()`).
3. **`__init_tls` is a no-op stub.** In the *shared* libc.so, the weak
   `static_init_tls` is overridden by the dynamic linker's `__init_tls`, which
   is `endbr64; ret` — musl assumes its `ld.so` already set TLS up. The foreign
   loader must build the **x86_64 variant-II TCB itself** (`setup_static_tls`): a
   musl `struct pthread` with `self`@TP+0 / `dtv`@TP+8, the main exe's `PT_TLS`
   local-exec block copied below TP, and `arch_prctl(ARCH_SET_FS, TP)` — *before*
   constructors. Without it libc's first `%fs:` access (errno, the `%fs:0x28`
   stack canary, locale) faults.
4. **`__libc_start_init` → `do_init_fini(main_ctor_queue)`.** The ctor queue is
   built by `queue_ctors()` inside `__dls3` → NULL under the foreign loader →
   NULL deref. A one-line musl patch
   (`ports/lib/musl/patches/0001-foreign-loader-null-ctor-queue.patch`) guards
   the NULL; the loader runs the constructors itself.
5. **COPY relocations applied too early.** The loader relocated the main binary
   *before* libc, so a `R_X86_64_COPY` of libc's `stdout` FILE\* captured the
   **un-rebased** low address `0xaf34c`; `fflush(stdout)` then dereferenced
   garbage. Fix: relocate every DSO **first**, the main binary **last** — the
   standard "copy relocations apply last" rule.
6. (harness) The shared `WaitPassOrFail` failure message was hardcoded
   "audio-demo failed at stage" for *every* gate — made generic.

### `dlopen` routes to the loader, not libc

A dynamic program's `dlopen`/`dlsym` resolve to the **m3OS loader's** exports
(it exports `dlopen`/`dlsym`/`dlclose` as `GLOBAL` dynsyms, ahead of libc in
scope order). So `ctypes.CDLL(...)` and CPython's `lib-dynload` import — which
both call libc's `dlopen` — actually run the loader's Phase 76c `dlopen`, which
maps + relocates the `.so` into the loader's own DSO state. This is *why* the
A1 split works for `lib-dynload`/`ctypes`: musl's own `dlopen` (which needs
`__dls3` state the foreign loader never built) is never reached.

### The dynamic CPython variant

`build_python_dynamic` mirrors the static `build_python` minus exactly three
things: drop `-static` (→ a dynamic `python3`), drop `MODULE_BUILDTYPE=static`
(→ C extensions build as `lib-dynload/*.so`), and re-enable `_ctypes` (static
`libffi.a` linked in, so the extension's only `DT_NEEDED` is `libc.so`). It is a
*separate* port (`python-dynamic`) with a distinct pkgcache key; the static
recipe is untouched, so its green fallback never regresses.

## Key Files

| File | Purpose |
|---|---|
| `ports/lib/musl/Portfile` + `patches/0001-…` | musl 1.2.5 pin + the foreign-loader NULL-ctor-queue patch |
| `xtask/src/port_build.rs` (`build_musl`/`build_libffi`/`build_python_dynamic`) | the shared `libc.so`, static `libffi.a`, and dynamic CPython recipes |
| `userspace/ld-musl-x86_64.so.1/src/main.rs` (`setup_static_tls`, COPY-order, weak-symbol, scratch sizing) | the loader startup glue + the relocation-order fix |
| `userspace/ld-musl-x86_64.so.1/src/{elf64,reloc}.rs` | COPY/IFUNC/TLS reloc types + `STB_WEAK`/`st_bind` |
| `kernel/src/arch/x86_64/syscall/mod.rs` (`sys_mremap`) | the `mremap` syscall a dynamic libc invokes |
| `xtask/src/main.rs` (`dynamic-hello-smoke`, `dynamic-python-smoke`) | the falsifiable serial gates |

## How This Phase Differs From Later Work

- This phase introduces a **real `libc.so`** + the loader glue (TLS/TCB, ctor
  queue, COPY-order, dlopen routing) a foreign loader needs.
- **General-dynamic TLS** across `dlopen`'d TLS `.so`s (per-thread `__tls_get_addr`
  DTV growth) is deferred — the target workloads use only main-exe local-exec TLS.
- Phase 95 (Native Rust Toolchain) reuses this `libc.so` + loader TLS for the
  proc-macro (`.so` dlopen'd at compile time) half.

## Related Roadmap Docs

- [Phase 93 roadmap doc](./roadmap/93-dynamic-c-runtime.md)
- [Phase 93 task doc](./roadmap/tasks/93-dynamic-c-runtime-tasks.md)
- [Phase 85c — Python](./roadmap/85c-python.md) (the static rationale this lifts)
- [Phase 76 — Dynamic Linker](./roadmap/76-dynamic-linker.md) (the loader extended here)

## Deferred or Later-Phase Topics

- glibc ABI compatibility / a glibc `libc.so`.
- `dlopen`-based NSS, locale/iconv module loading, IFUNC SIMD dispatch beyond
  what CPython/ctypes need.
- `LD_PRELOAD` / `LD_AUDIT`; a general third-party `.so` SDK.
- Loader-owned general-dynamic TLS (DTV growth) for `dlopen`'d TLS libraries.
