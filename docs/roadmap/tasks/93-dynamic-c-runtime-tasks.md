# Phase 93 — Dynamic C Runtime (`libc.so` + shared objects): Task List

**Status:** In Progress — implementation underway (feat/phase-93-dynamic-c-runtime)
**Source Ref:** phase-93
**Depends on:** Phase 76 (Dynamic Linker) ✅, Phase 36 (Expanded Memory) ✅, Phase 85c (Python — surfaced the gap) ✅, Phase 90a (PKU / W^X v2 — the lazy-PLT W+X path) ✅
**Goal:** Ship m3OS's first real dynamic C library — a musl `libc.so` the Phase 76 loader can map and bind a genuinely dynamically-linked program against — and close the loader/kernel gaps a real libc exercises, so a dynamic `python3` (`PT_INTERP=/lib/ld-musl-x86_64.so.1`, `DT_NEEDED libc.so`) boots, imports `lib-dynload/*.so` extensions via `dlopen`, and `ctypes.CDLL(...)` opens a shared object — while the static `git` and static `python3` paths stay the conservative green fallback. Closes with the kernel version bump (`0.92.5` → `0.93.0`) and the Phase 93 learning doc (`docs/93-dynamic-c-runtime.md`). The tracks map onto the design doc's Areas: Track A = Area A (ship `libc.so`), Tracks B+C = Area B (loader + kernel syscall gaps), Track D = Area C (dynamic CPython + `ctypes`), Track E = the Evaluation Gate, Track F = closeout.

> **Authored ahead of implementation.** Every acceptance item below is intentionally unchecked `[ ]`; it records the planned, measurable result, not a delivered one. The plan is grounded in a current-state audit of the six subsystems Phase 93 touches (the loader `userspace/ld-musl-x86_64.so.1/`, the xtask musl/port plumbing, the kernel syscall surface, the CPython port, the smoke harness, and the docs), so each File/Symbol reference below is a real location in the tree today, and each task is framed as a delta against proven existing code rather than a green-field design.

> **Scope honesty — what is and isn't in Phase 93.** `ctypes` / `dlopen`-of-an-arbitrary-`.so` **is** in scope (the design doc's Acceptance Criteria require `ctypes.CDLL(...)` to open a shared object and call a function). Out of scope and tracked in the design doc's *Deferred Until Later*: glibc ABI compatibility, `dlopen`-based NSS/locale/iconv, IFUNC SIMD dispatch beyond what CPython/ctypes need, `LD_PRELOAD`/`LD_AUDIT`, and a general third-party `.so` SDK. The **single largest piece of real work is TLS** (Track B.3): the loader does no PT_TLS allocation, never sets the thread pointer, and exports no `__tls_get_addr` today — a real libc faults on the first thread-local access without it.

> **Provider-strategy note (Area A decision).** The design doc leaves the libc-provider strategy open (Option A1: ship upstream musl as a companion shared object; Option A2: grow the Rust loader into a libc). The plan below assumes the recommended **A1 baseline** — build upstream musl `--enable-shared` into a companion `/usr/lib/libc.so` and **keep the from-scratch Rust loader** as `/lib/ld-musl-x86_64.so.1` (m3OS's loader is a major asset: bounds-checked relocations, W^X, GNU-hash, versioning, lazy PLT). The key risk A1 must resolve (Task A.1): on real musl, `ld-musl-x86_64.so.1` **is** `libc.so` (one file bootstraps its own TLS/relocation via `_dlstart`/`__dls2`); a companion `libc.so` loaded by a *foreign* loader needs that loader to provide the TLS/startup musl's own ld normally does — which is exactly what Track B delivers.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Ship a real musl `libc.so` — a companion upstream-musl shared object at `/usr/lib/libc.so` (`DT_SONAME=libc.so`) + the Portfile / `build_musl` / `.so`-safe sealing / dispatch + install plumbing | — | ⏳ Planned |
| B | Loader extensions a real libc exercises that the self-contained test libs never did: copy relocations (`R_X86_64_COPY`), IFUNC (`R_X86_64_IRELATIVE`/`STT_GNU_IFUNC`), general-dynamic TLS (`DTPMOD64`/`DTPOFF64`/`TPOFF64` + `__tls_get_addr` + per-thread TLS block + TP setup), and a broadened DT_NEEDED/`dlopen` search path | — (independent loader work; A is the end-to-end validation target) | ⏳ Planned |
| C | Kernel syscall gaps a dynamic libc invokes at startup: implement `mremap` (25), audit/confirm the dynamic-startup set, and confirm the W^X-v2 / `pkey_mprotect` interaction for lazy-PLT GOT writes | — | ⏳ Planned |
| D | Dynamic CPython — a `build_python_dynamic` variant (`lib-dynload/*.so`, `PT_INTERP`+`DT_NEEDED libc.so`) + a `libffi` port + re-enabled `_ctypes`, shipped **opt-in** beside the unchanged static fallback | A, B, C | ⏳ Planned |
| E | Acceptance gates: `dynamic-hello-smoke`, `dynamic-python-smoke`, `ctypes-smoke` + the `M3OS_*_REGRESSION` wiring; the static `git` / `python3` gates stay green | A, B, C, D | ⏳ Planned |
| F | Documentation + release closeout: the learning doc, the design-doc + README + AGENTS.md updates, the deferral-pointer flips, and the kernel version bump `0.92.5` → `0.93.0` | E | ⏳ Planned |

---

## Track A — Ship a Real `libc.so`

### A.1 — Decide and document the libc-provider strategy

**File:** `docs/roadmap/93-dynamic-c-runtime.md` (Area A) + a short decision record in the Phase 93 learning doc (F.1)
**Symbol:** Option **A1** (companion upstream-musl shared object, Rust loader stays the interp) vs Option **A2** (grow `userspace/ld-musl-x86_64.so.1/` into a C-ABI libc)
**Why it matters:** every downstream task depends on the artifact's shape and on whether the Rust loader remains `PT_INTERP`. The decision must confront the core risk: on real musl the loader and libc are one file that self-bootstraps; a companion `libc.so` mapped by m3OS's foreign Rust loader needs that loader to supply the TLS/relocation startup musl's own ld would (Track B). Pinning this first prevents Tracks A.3/B from churning on an unsettled artifact contract.

**Acceptance:**
- [ ] A decision record selects A1 (build upstream musl `--enable-shared` as a companion `/usr/lib/libc.so`; keep the Rust loader as `/lib/ld-musl-x86_64.so.1`) or A2, with rationale and the rejected alternative recorded.
- [ ] The record pins the exact upstream musl version (e.g. `1.2.5`) and enumerates the relocation/TLS shapes that pin will require of the loader (the input list for Track B: copy-relocs, IFUNC, general-dynamic TLS).
- [ ] Non-goals recorded explicitly: no glibc; do not replace the Rust loader with musl's own `ld.so` unless A1 is proven infeasible.

### A.2 — `ports/lib/musl/Portfile`

**File:** `ports/lib/musl/Portfile` (new)
**Symbol:** `NAME=musl`, `VERSION`, `URL`, `SHA256`, `DEPS=` (none), `CATEGORY=lib`
**Why it matters:** musl source is **not vendored** today — `assemble_musl_sysroot()` (`xtask/src/port_build.rs:4901`) only *copies* a prebuilt static `libc.a` from the host toolchain. Shipping a shared `libc.so` requires fetching + SHA-verifying upstream musl from source through the same Portfile substrate every other port uses.

**Acceptance:**
- [ ] The Portfile parses with the xtask Portfile parser; `cargo xtask port list` shows `musl` with its version and empty `DEPS`.
- [ ] `URL` + `SHA256` pin a stable musl release; a comment documents the pin rationale (per the A.1 decision).

### A.3 — `build_musl` recipe (shared object)

**File:** `xtask/src/port_build.rs` (new `build_musl`, beside the other library recipes near `build_zlib`/`build_ncurses` ~L2100–2300)
**Symbol:** `build_musl(src, stage, toolchain)` — `./configure --prefix=/usr --disable-static --enable-shared --host=x86_64-linux-musl`, `CC`/`AR`/`RANLIB` from `musl_toolchain()` (`port_build.rs:111`), `CFLAGS="-O2 -fPIC"`, LDFLAGS composed via `musl_extra_ldflags_joined()` (`port_build.rs:105`); installs `/usr/lib/libc.so`
**Why it matters:** this produces the actual artifact the loader binds against. It **must** route through the shared musl-toolchain plumbing (the repo's "Adding a New Cross-Compiled Port" contract — without `musl_extra_ldflags_joined()` the link probe fails on toolchains missing the historical compat archives), and the `.so` must self-identify with `DT_SONAME=libc.so` so the loader's existing SONAME dedup (`load_dso`, `main.rs:739`) keys on it.

**Acceptance:**
- [ ] `build_musl` produces stage `/usr/lib/libc.so` where `readelf -d` shows `SONAME libc.so` and **zero** `DT_NEEDED` entries (musl is self-contained).
- [ ] The `.so` exports the musl dynamic symbol set CPython links — `malloc`/`free`/`realloc`, `mem*`/`str*`, `*printf` + stdio, `getenv`, `dl*`, `pthread_*`, `__errno_location`, and the `libm` symbols musl folds into libc — verified by a symbol-presence check, not assumed.
- [ ] The recipe resolves its toolchain via `musl_toolchain()` and composes LDFLAGS via `musl_extra_ldflags_joined()`; it builds on a toolchain lacking `libdl.a`/`libpthread.a`/`librt.a` (the auto-generated stub-libs path).

### A.4 — `.so`-safe sealing (do not strip `ET_DYN` dynsym)

**File:** `xtask/src/port_build.rs` (`seal_package` ~L1016, `strip_stage` ~L1082–1150)
**Symbol:** `strip_stage` — today it skips `ET_REL` objects (CRT `.o`) and archives; it must additionally spare the **dynamic** symbol table of `ET_DYN` shared objects
**Why it matters:** `strip_stage` recursively strips ELF symbol tables. `libc.so` is the **first shipped `ET_DYN` with load-bearing exports** — stripping its `.dynsym`/`.dynstr` destroys the table the loader resolves against, leaving every relocation undefined. (Static `.a` archives are unaffected, which is why this never bit before.)

**Acceptance:**
- [ ] `strip_stage` keeps `.dynsym`/`.dynstr`/hash sections of `ET_DYN` objects (it may still strip `.symtab`/debug), or skips `libc.so`/`libffi.so` outright.
- [ ] A regression guard — mirroring the existing llvm CRT `crt1.o` check (~`port_build.rs:1040`) — asserts the **sealed** `libc.so` still exports a sentinel symbol (e.g. `malloc`) after `seal_package`.

### A.5 — Register musl in the dispatch, dep graph, and image install path

**Files:**
- `xtask/src/port_build.rs` (`port_build` match arm ~L1495, `port_deps` ~L821)
- `xtask/src/main.rs` (`PORTS` list ~L17446, `BUNDLE_ONLY_PORTS` ~L25527, `populate_phase_69d_ports` image bundling)

**Symbol:** `PORTS += "musl"`; `port_build` match arm `"musl" => build_musl(&extracted, &stage, &toolchain)?`; install so the `.so` lands at `/usr/lib/libc.so` on the booted ext2 image
**Why it matters:** a port is invisible to `cargo xtask port build` and the in-OS solver until it is registered in all the wiring spots (the four-place port-registration contract). `libc.so` must also reach `/usr/lib/` on the data disk for the loader's hard-coded `/usr/lib/<name>` lookup (`main.rs:1706`) to find it at runtime.

**Acceptance:**
- [ ] `cargo xtask port build musl` builds + seals `musl.m3pkg`; a second build is a pure pkgcache hit (zero compiler invocations).
- [ ] The image places `libc.so` at `/usr/lib/libc.so` (plus any `libc.so.1` SONAME symlink) on the booted ext2 disk, readable `0755`.

---

## Track B — Loader Extensions for a Real libc

> The loader (`userspace/ld-musl-x86_64.so.1/`) implements exactly five relocation types today — `R_X86_64_{NONE(0), 64(1), GLOB_DAT(6), JUMP_SLOT(7), RELATIVE(8)}` (`src/elf64.rs:113`) — and errors with `ldso: unsupported reloc type <N>` on anything else. The self-contained Phase 76 test libs (`libhello.so`) reference no libc symbols, so copy-relocs, IFUNC, and TLS never surfaced. A real `libc.so` exercises all three on first load.

### B.1 — Copy relocations (`R_X86_64_COPY`)

**Files:** `userspace/ld-musl-x86_64.so.1/src/reloc.rs` (new `apply_copy`, beside `apply_relative`/`apply_glob_dat`/`apply_abs64` at L54/79/109), `src/main.rs` (`apply_rela` match, L1026)
**Symbol:** the `R_X86_64_COPY (11)` arm in `apply_rela`; `ldso_core::reloc::apply_copy`
**Why it matters:** a real libc defines data symbols (e.g. stdio globals, `__environ`) that the main executable **copy-relocates** into its own BSS for legacy interposition. The first `COPY` aborts the load today with `unsupported reloc type 11`.

**Acceptance:**
- [ ] `apply_rela` handles `R_X86_64_COPY`: resolve the provider symbol, copy `st_size` bytes from the provider into the consumer's `r_offset`.
- [ ] `r_offset` is validated to lie inside the **main image's** writable span before the write (preserving the loader's bounds-checking invariant); an out-of-range target is rejected.
- [ ] A test links an executable + a libc-like `.so` sharing a data symbol and asserts the consumer's copy holds the provider's value.

### B.2 — IFUNC resolvers (`R_X86_64_IRELATIVE` + `STT_GNU_IFUNC`)

**Files:** `src/reloc.rs` (new `apply_irelative`), `src/sym.rs` (detect `STT_GNU_IFUNC` in `st_info`; `lookup` at L88), `src/main.rs` (`apply_rela`)
**Symbol:** `R_X86_64_IRELATIVE (37)`; `STT_GNU_IFUNC (10)` (high nibble of `st_info`)
**Why it matters:** musl uses IFUNC to select CPU-optimized `memcpy`/`memset`/`strlen` at load time. The resolver is a zero-argument function whose return value is the real implementation address; without IRELATIVE dispatch those symbols resolve to the resolver itself (or fail).

**Acceptance:**
- [ ] `R_X86_64_IRELATIVE` calls the resolver (address from `r_addend`/`st_value`) with clean argument registers and writes the **returned** address into the GOT slot.
- [ ] A `STT_GNU_IFUNC` symbol reached via `GLOB_DAT`/`JUMP_SLOT` also routes through the resolver, not the raw resolver address.
- [ ] A test with an IFUNC symbol asserts the resolver runs once at load and the subsequent call lands on the resolved implementation.

### B.3 — TLS relocations + per-thread TLS block + `__tls_get_addr`

**Files:** `src/tls.rs` (new), `src/reloc.rs` (`apply_dtpmod64`/`apply_dtpoff64`/`apply_tpoff64`), `src/main.rs` (PT_TLS parse + post-load/pre-constructor TLS setup), `src/dynlink.rs` (PT_TLS `p_memsz`/`p_filesz` already parsed from PHDRs)
**Symbol:** `R_X86_64_DTPMOD64 (16)` / `DTPOFF64 (17)` / `TPOFF64 (18)`; exported `__tls_get_addr`; thread-pointer (FS base) initialization
**Why it matters:** a real libc and its threads use TLS for `errno`, locale, and stdio locks. The loader does **no** PT_TLS allocation, never sets the TP, and exports **no** `__tls_get_addr` — so any thread-local access faults. This is the crux of running real dynamic C and the largest single loader gap.

**Acceptance:**
- [ ] PT_TLS segments are parsed; each module receives a TLS block (`.tdata` initialized, `.tbss` zeroed) and a stable module id.
- [ ] `DTPMOD64` writes the module id (a per-DSO constant, **not** `st_value`); `DTPOFF64` writes the in-block offset (no `load_bias` added — TLS `st_value` is an offset, not an address); `TPOFF64` writes the TP-relative offset.
- [ ] `__tls_get_addr((module, offset))` returns the correct thread-local address; the TP is established via the kernel `arch_prctl(ARCH_SET_FS)` path (Track C.2) **before** constructors run.
- [ ] A multi-threaded (`CLONE_VM`) test observes per-thread TLS copies (two threads see independent values for the same TLS symbol).

### B.4 — Broadened DT_NEEDED / `dlopen` search path

**Files:** `src/main.rs` (DT_NEEDED path build, L1706–1731), `src/dl.rs` (`dlopen` path resolution, L337/~407)
**Symbol:** the hard-coded `/usr/lib/<name>` prefix → add `/lib` (and optionally an `LD_LIBRARY_PATH` read from `envp`)
**Why it matters:** the bring-up loader searches only `/usr/lib/`. `libc.so` lands there so the baseline resolves, but `lib-dynload/*.so` and `ctypes.CDLL("libc.so")` benefit from a broadened search so a bare soname resolves predictably; this is also where the `ldso: DT_NEEDED not found: libc.so` error (`main.rs:1731`) originates.

**Acceptance:**
- [ ] `DT_NEEDED libc.so` resolves to `/usr/lib/libc.so`; `dlopen("libc.so")` searches `/usr/lib` then `/lib`.
- [ ] The `DT_NEEDED not found: libc.so` exit path no longer triggers for the shipped `libc.so`.

---

## Track C — Kernel Syscall Gaps

### C.1 — Implement `mremap` (syscall 25)

**File:** `kernel/src/arch/x86_64/syscall/mod.rs` (`syscall_nr` constants ~L1363, dispatch `match` ~L1841, new `sys_mremap`)
**Symbol:** `MREMAP = 25`; `sys_mremap(old_addr, old_size, new_size, flags)`
**Why it matters:** musl `realloc` of large `mmap` chunks calls `mremap` first; today syscall 25 is undefined (the `syscall_nr` module defines 9–12 but skips 25) and falls through to the `_ => NEG_ENOSYS` default (`mod.rs:2425`), so musl takes the slow map-copy-unmap fallback. The design doc names `mremap` as the headline syscall gap.

**Acceptance:**
- [ ] `sys_mremap` grows/shrinks the mapping in place when the adjacent range is free; `MREMAP_MAYMOVE` is implemented (relocate) or rejected with a documented `EINVAL` policy.
- [ ] Syscall 25 no longer reaches the unhandled-syscall logger; a musl `realloc`-resize workload completes via `mremap` (no ENOSYS for 25 in the boot log).

### C.2 — Audit + confirm the dynamic-startup syscall set

**File:** `kernel/src/arch/x86_64/syscall/mod.rs`
**Symbol:** `arch_prctl(ARCH_SET_FS)` (158, L16758), `set_tid_address` (218, L17051), `brk` (12, L14096), file-backed `mmap` (`sys_mmap_file_backed`, L11634)
**Why it matters:** a dynamic `libc.so` + `__libc_start_main` exercise a startup path the static bring-up never ran; a single missing startup syscall faults before `main`. The audit confirmed these all exist today — this task is the deliberate verification under a **real dynamic binary**, plus confirming the TP set by `ARCH_SET_FS` survives a context switch so loader-initialized TLS (B.3) persists.

**Acceptance:**
- [ ] The trivial dynamic C binary (E.1) reaches `main` with **no** unhandled-syscall log line during startup.
- [ ] The FS base set by `arch_prctl(ARCH_SET_FS, ...)` is restored from `proc.fs_base` across a context switch (verified — loader-initialized TLS persists after a yield).

### C.3 — W^X v2 / `pkey_mprotect` for lazy-PLT GOT writes

**File:** `kernel/src/arch/x86_64/syscall/mod.rs` (`mprotect_worker` ~L12268, `wx_decision` ~L12296, `sys_pkey_mprotect` ~L12606)
**Symbol:** `wx_decision`; the `[wx] v2-guarded W+X mapping` path
**Why it matters:** a dynamic libc + lazy PLT writes resolved addresses into a GOT while the surrounding code stays R-X (the loader already `mprotect`s text R-X in `load_dso`, `main.rs:739`). The W^X-v2 invariant must permit the loader's protect-write-protect (or pkey-guarded) sequence without a spurious `EINVAL`/kill.

**Acceptance:**
- [ ] The loader's lazy-resolve GOT-write path completes under W^X v2 (GOT writable, executable code never simultaneously writable) — no spurious `EINVAL` and no wrongful process kill.
- [ ] `m3ctl mitigations status` posture is unchanged and the always-on `wx-violation` gate stays green.

---

## Track D — Dynamic CPython + `lib-dynload` + libffi + ctypes

### D.1 — `ports/lib/libffi/Portfile` + `build_libffi`

**Files:** `ports/lib/libffi/Portfile` (new), `xtask/src/port_build.rs` (new `build_libffi` + `port_build` dispatch arm), `xtask/src/main.rs` (`PORTS` list)
**Symbol:** `build_libffi(src, stage, toolchain)` → `/usr/lib/libffi.so` (+ `include/ffi.h`, `ffitarget.h`)
**Why it matters:** CPython's `_ctypes` links libffi; it is the one missing port the design doc and `docs/python-roadmap.md` both call out for Phase 93. `ffitarget.h` is generated per-target, so the recipe must cross-configure `--host=x86_64-linux-musl` (not reuse the host's header).

**Acceptance:**
- [ ] `cargo xtask port build libffi` produces a shared `libffi` (`DT_SONAME=libffi.so`, `DT_NEEDED libc.so`) + staged `ffi.h`/`ffitarget.h`; registered in `PORTS` and the `port_build` dispatch and routed through `musl_toolchain()`.
- [ ] A second build is a pure pkgcache hit.

### D.2 — `build_python_dynamic` variant (do not touch the static recipe)

**File:** `xtask/src/port_build.rs` (new `build_python_dynamic` alongside `build_python` at L3432; leave `build_python` byte-for-byte intact)
**Symbol:** drop `MODULE_BUILDTYPE=static` (L3632), `LDFLAGS=-static` (L3535), and `--disable-shared`; produce `PT_INTERP=/lib/ld-musl-x86_64.so.1` + `DT_NEEDED libc.so` + a non-empty `lib-dynload/*.cpython-312-*.so`; tag a distinct `recipe-v`/variant token so the pkgcache key does not collide with the static build
**Why it matters:** the static recipe must remain the green fallback the Evaluation Gate requires, so the dynamic interpreter is a **new, separate** codepath/artifact. The static build's `lib-dynload` prune (`assert_python_layout` at L5438 requires it empty; prune at ~L5321) must be **skipped** for the dynamic variant.

**Acceptance:**
- [ ] `build_python_dynamic` produces a `python3` with `PT_INTERP` + `DT_NEEDED libc.so` and a non-empty `lib-dynload`; `build_python`'s static output is unchanged (its gate stays green).
- [ ] The dynamic variant carries a distinct content key; static and dynamic `.m3pkg`s coexist in `target/pkgcache/` without collision.

### D.3 — Re-enable `_ctypes` in the dynamic build

**File:** `xtask/src/port_build.rs` (`build_python_dynamic`; the `disabled_modules` array at L3584–3603)
**Symbol:** remove `_ctypes` from `disabled_modules` (it is force-disabled `py_cv_module__ctypes=n/a` today); wire libffi `CFLAGS`/`LIBS` to the D.1 staged libffi
**Why it matters:** `_ctypes` is explicitly excluded in the current build because it needs libffi + runtime `dlopen`; re-enabling it against the D.1 libffi is what makes `ctypes.CDLL` exist on m3OS.

**Acceptance:**
- [ ] The dynamic `python3` ships `lib-dynload/_ctypes.cpython-312-*.so`; `_ctypes_test` stays pruned.
- [ ] `python3 -c "import ctypes"` succeeds inside m3OS (proven by gate E.3).
- [ ] The stale `_ctypes ... → Phase 93 (Dynamic C Runtime)` deferral comment near the `disabled_modules` array (`port_build.rs:~3566`) is updated to reflect that this phase delivers it (the inherited `Phase 91` typo was corrected when this task doc landed).

### D.4 — Opt-in bundling + dependency graph (keep static default)

**Files:** `xtask/src/main.rs` (`BUNDLE_ONLY_PORTS` ~L25527, `populate_phase_69d_ports` / `M3OS_WITH_*` gates), `xtask/src/port_build.rs` (`port_deps`, L821 — `python` gains `libffi`)
**Symbol:** a new `M3OS_WITH_DYNAMIC_PYTHON` image feature (mirroring `M3OS_WITH_CLANG`/`M3OS_WITH_NODE`) — bundles the dynamic variant + `libc.so` + `libffi`; static `python3` stays the default
**Why it matters:** the Evaluation Gate requires the static path remain the conservative fallback, so the dynamic variant ships behind its own feature flag until stable, and the in-OS solver must learn `python → libffi` so `pkg install python` (dynamic) pulls it.

**Acceptance:**
- [ ] Default images bundle static `python3` unchanged; `M3OS_WITH_DYNAMIC_PYTHON=1` bundles the dynamic variant + `libc.so` + `libffi` and writes the `.meta` `DEPS=` so the solver resolves `libffi`.
- [ ] On install, `/usr/lib/libc.so` + `lib-dynload/` land in the Phase 85a relocatable layout (`lib/python3.12/lib-dynload/`).

---

## Track E — Acceptance Gates

> **Critical placement constraint** (from the harness audit): a dynamic test program needs `PT_INTERP=/lib/ld-musl-x86_64.so.1` (in the ramdisk) **and** `DT_NEEDED libc.so` resolved at runtime from the **ext2 filesystem** `/usr/lib/libc.so`. A full `libc.so` (writable `.data`/`.bss`, TLS, relocations) cannot live in the `no_std`, read-only ramdisk — so every dynamic gate runs from a logged-in shell **after `init` mounts ext2** (mirroring `python-smoke`/`node-smoke`), never as an early-boot ramdisk exec.

### E.1 — `dynamic-hello-smoke` (a trivial dynamic C program runs)

**Files:** `xtask/src/main.rs` (`cmd_dynamic_hello_smoke` + `dynamic_hello_smoke_steps`, registered as a `cargo xtask` subcommand, mirroring `cmd_python_smoke` ~L17350), a `dynamic-hello.c` fixture cross-built with musl-gcc, ext2 staging of the binary + `/usr/lib/libc.so` via `populate_ext2_files` (~L23073)
**Symbol:** `dynamic_hello_smoke_steps` — boot + login, then run a dynamic `hello` (`PT_INTERP`+`DT_NEEDED libc.so`, calls `printf`+`malloc`), assert `DYNAMIC_HELLO:ok`
**Why it matters:** the design doc requires proving a trivial dynamic C program runs **before** attempting CPython — the falsifiable end-to-end proof that A (libc.so) + B (loader) + C (kernel) compose.

**Acceptance:**
- [ ] A dynamic `hello` (`DT_NEEDED libc.so`, `printf`+`malloc`) runs from the shell and prints `DYNAMIC_HELLO:ok` — no `DT_NEEDED not found` and no undefined-symbol abort.
- [ ] The gate is CI-deterministic (no network/hardware) and registered as `cargo xtask dynamic-hello-smoke`.

### E.2 — `dynamic-python-smoke` (dynamic `python3` imports a `lib-dynload` `.so`)

**Files:** `xtask/src/main.rs` (`cmd_dynamic_python_smoke` + `dynamic_python_smoke_steps`), built on the E.1 ext2 staging + the `M3OS_WITH_DYNAMIC_PYTHON` image
**Symbol:** `dynamic_python_smoke_steps` — `pkg install python` (the dynamic variant), `python3 --version`, then `import` of a `lib-dynload` extension (a C extension built as `.so`, e.g. `_curses` or `math`)
**Why it matters:** the milestone-goal proof — a `PT_INTERP`+`DT_NEEDED` `python3` boots, binds the real `libc.so`, and `dlopen`s a `lib-dynload/*.so` extension at runtime (the layout the static Phase 85c build had to flatten into `python312.zip`).

**Acceptance:**
- [ ] The dynamic `python3` prints its version and imports at least one `lib-dynload` `.so` extension inside m3OS (sentinel `DYNPY:import-ok`).
- [ ] Runs at a `python-smoke`-class `--timeout` (cold imports over the slow VFS), opt-in behind `M3OS_WITH_DYNAMIC_PYTHON`.

### E.3 — `ctypes-smoke` (`ctypes.CDLL` opens a `.so` and calls a function)

**Files:** `xtask/src/main.rs` (`cmd_ctypes_smoke` + `ctypes_smoke_steps`)
**Symbol:** `ctypes_smoke_steps` — `python3 -c "import ctypes; libc = ctypes.CDLL('/usr/lib/libc.so'); ..."` calling a libc function (e.g. `strlen`) and asserting the result
**Why it matters:** the design doc's explicit acceptance criterion — `ctypes.CDLL(...)` opens a shared object and calls a function inside m3OS. This is the proof runtime `dlopen` of an arbitrary `.so` works through the real libc.

**Acceptance:**
- [ ] `ctypes.CDLL` opens `/usr/lib/libc.so` (or a small bundled `.so` such as the Phase 76 `libhello.so`) and a called function returns the expected value (`CTYPES:ok`).

### E.4 — Regression wiring + static-path green guard

**Files:** `.githooks/pre-push` (mirroring the `M3OS_PYTHON_REGRESSION` block ~L544–559), `AGENTS.md` (the pre-push gate table)
**Symbol:** a `M3OS_DYNAMIC_C_REGRESSION` env var (and/or per-gate vars) + the AGENTS.md table rows; the existing static `git`/`python3` gates
**Why it matters:** the Evaluation Gate requires the static fallback stay green; the new gates follow the established opt-in `M3OS_*_REGRESSION` + pre-push + AGENTS.md-table convention so they are discoverable and CI-wired exactly like every prior gate.

**Acceptance:**
- [ ] `dynamic-hello-smoke`/`dynamic-python-smoke`/`ctypes-smoke` are added to the AGENTS.md pre-push gate table and `.githooks/pre-push` under `M3OS_DYNAMIC_C_REGRESSION`; `dynamic-hello-smoke` is wired to run CI-deterministically.
- [ ] `python-smoke` (static) and `git-local-smoke` stay green — no regression of the conservative fallback path.

---

## Track F — Documentation + Release Closeout

### F.1 — Create the Phase 93 learning doc

**File:** `docs/93-dynamic-c-runtime.md` (new)
**Symbol:** the *aligned legacy learning doc* template (`docs/appendix/doc-templates.md`, L167–215) — fields: Aligned Roadmap Phase (93), Status, Source Ref (`phase-93`), Supersedes Legacy Doc (N/A), Overview, What This Doc Covers, Core Implementation, Key Files, How This Phase Differs From Later Work, Related Roadmap Docs, Deferred or Later-Phase Topics
**Why it matters:** the design doc's *Learning Documentation Requirement* mandates it. It teaches the loader-vs-libc split (why on real musl the loader *is* the libc and what it meant that m3OS split them), the A1/A2 provider decision, the syscall-gap audit, and the dynamic-vs-static trade-offs CPython exposed — the pedagogical companion to the implementation-focused design doc.

**Acceptance:**
- [ ] `docs/93-dynamic-c-runtime.md` exists and uses the aligned-learning-doc template (not the phase-design template).
- [ ] It covers the loader-vs-libc split, the copy-reloc/IFUNC/TLS shapes a real libc needs, and the static-fallback rationale; it links the design doc + this task doc.

### F.2 — Fix the design doc's Companion Task List link

**File:** `docs/roadmap/93-dynamic-c-runtime.md` (the `## Companion Task List` section, L124–126)
**Symbol:** the Companion Task List bullet
**Why it matters:** it previously read "Phase 93 task list — defer until implementation planning begins"; once this task doc existed, the bullet had to become a live link to match the template and the 89/90b pattern. **Applied together with this task doc** (recorded here for traceability).

**Acceptance:**
- [x] The section reads `- [Phase 93 Task List](./tasks/93-dynamic-c-runtime-tasks.md)`.

### F.3 — Update `docs/README.md` learning-doc table

**File:** `docs/README.md` (the Phase-Aligned Learning Docs table)
**Symbol:** a new Phase 93 row mirroring the verbatim 91/92 row format
**Why it matters:** the learning-doc index must list Phase 93 so readers can discover the material, in the exact `| [Title](./NN-slug.md) | NN | description … Links the [NN design](./roadmap/NN-slug.md) + [task](./roadmap/tasks/NN-slug-tasks.md) docs |` shape used by the surrounding rows.

**Acceptance:**
- [ ] A row `| [Dynamic C Runtime](./93-dynamic-c-runtime.md) | 93 | … | Links the [93 design](./roadmap/93-dynamic-c-runtime.md) + [task](./roadmap/tasks/93-dynamic-c-runtime-tasks.md) docs |` is added in phase order.

### F.4 — Update the roadmap README Phase 93 row + status

**File:** `docs/roadmap/README.md` (the Phase 93 summary row, L479)
**Symbol:** the Phase 93 row's Tasks column + Status column
**Why it matters:** the Tasks column previously said "Deferred until implementation planning"; with the task doc authored it must be a `[Tasks](...)` link, and the Status flips from `Planned` as the phase progresses. **The Tasks-link half was applied together with this task doc**; the Status flip happens at phase completion.

**Acceptance:**
- [x] The Tasks column reads `[Tasks](./tasks/93-dynamic-c-runtime-tasks.md)`.
- [ ] Status reflects reality (`Planned` → `In Progress` → `Complete`) as the phase advances.

### F.5 — Update the AGENTS.md "Dynamic linking" capability bullet

**File:** `AGENTS.md` (the **Dynamic linking** capability bullet)
**Symbol:** the `**Dynamic linking**: ...` bullet
**Why it matters:** AGENTS.md's maintenance policy permits rewriting an existing capability bullet when the capability class changes. Today it correctly describes a loader *without* a libc; post-`libc.so` it must mention the real musl `libc.so`, dynamic programs (dynamic `python3`, `lib-dynload`, `ctypes`), and the copy-reloc/IFUNC/TLS maturity — while preserving the reloc/versioning/W^X detail. *(Applied only when the phase actually lands.)*

**Acceptance:**
- [ ] The bullet describes both the loader and a real musl `libc.so` + dynamic C/Python with `lib-dynload` + `ctypes`, preserving the existing technical detail, and is edited only at phase completion.

### F.6 — Flip the Phase 93 deferral pointers in the Python docs

**Files:** `docs/python-roadmap.md` (the ctypes/dlopen deferral, ~L498 + the Stage-2 dynamic-extension assumptions), `docs/roadmap/85c-python.md` (the Phase 93 deferral text, L31/41/81/84)
**Symbol:** the "deferred to Phase 93 (dynamic linking)" ctypes/dlopen pointers
**Why it matters:** when Phase 93 lands, those deferrals flip from "deferred" to "delivered." The historical "why 85c shipped static" explanation stays (it teaches *why* the loader had no libc to bind against); a callout notes that Phase 93 lifted the constraint.

**Acceptance:**
- [ ] Both docs note Phase 93 delivered the dynamic path (`ctypes`/`dlopen` live); the historical static rationale is preserved, not deleted.

### F.7 — Bump the kernel version (`0.92.5` → `0.93.0`)

**Files:** `kernel/Cargo.toml` (the `version` field, L3), `AGENTS.md` (the "kernel **v0.92.5**" reference in the Project Overview)
**Symbol:** `version = "0.92.5"`
**Why it matters:** every phase lands with a kernel version bump; the design doc's Implementation Outline (step 5) and *Related Documentation and Version Updates* both call for it, and AGENTS.md's maintenance policy explicitly permits bumping the version line when a phase lands.

**Acceptance:**
- [ ] `kernel/Cargo.toml` `version` is `0.93.0`.
- [ ] The AGENTS.md "kernel v0.xx.x" reference is updated to `v0.93.0` to match.

---

## Documentation Notes

- **What changed relative to Phase 85c.** Phase 85c shipped CPython **fully static** precisely because the Phase 76 loader had no `libc.so` to bind a dynamic interpreter against (`ldso: DT_NEEDED not found: libc.so`). Phase 93 ships that `libc.so`, extends the loader for the relocation/TLS shapes a real libc needs (the self-contained Phase 76 test libs never exercised them), and adds a **dynamic** `python3` variant **beside** — never replacing — the static one. The static `git` and static `python3` remain the conservative, always-green fallback.
- **What replaces an older implementation.** Nothing is removed. The loader's five-type relocation engine is **extended** (copy-relocs, IFUNC, TLS) rather than rewritten; `mremap` moves from the `ENOSYS` default arm to a real implementation; the static Python recipe is untouched and a parallel dynamic recipe is added.
- **The crux is TLS (B.3), not the file (A).** Shipping `libc.so` is mechanical; making m3OS's *foreign* Rust loader provide the per-thread TLS block + `__tls_get_addr` + TP setup that musl's own `ld.so` normally bootstraps is the genuinely novel work, and the most likely source of bring-up surprises.
- **Gate honesty.** `dynamic-hello-smoke` is CI-deterministic (no network/hardware); `dynamic-python-smoke`/`ctypes-smoke` are opt-in behind `M3OS_WITH_DYNAMIC_PYTHON` + `M3OS_DYNAMIC_C_REGRESSION` because they require the (large) dynamic Python image, mirroring the established `python-smoke`/`clang-smoke` opt-in pattern.
- **File/Symbol references are real-as-of-`0.92.5`.** Line numbers are approximate anchors against the current tree (`userspace/ld-musl-x86_64.so.1/`, `xtask/src/{main,port_build}.rs`, `kernel/src/arch/x86_64/syscall/mod.rs`); confirm with a fresh `grep` at implementation time, as surrounding code will have shifted.
- **Cross-links:**
  - Design doc: [Phase 93 — Dynamic C Runtime](../93-dynamic-c-runtime.md)
  - Predecessor that surfaced the gap: [Phase 85c — Python](../85c-python.md) (its static-vs-dynamic rationale) and the per-tool [Python roadmap](../../python-roadmap.md)
  - Loader this phase extends: [Phase 76 — Dynamic Linker](../76-dynamic-linker.md)
  - Learning doc to be created (F.1): `docs/93-dynamic-c-runtime.md`
  - Templates this doc conforms to: [doc templates](../../appendix/doc-templates.md) (phase-task-doc + aligned-learning-doc sections)
