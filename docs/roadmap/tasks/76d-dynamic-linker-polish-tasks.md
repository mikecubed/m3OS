# Phase 76d — Dynamic Linker: PLT Lazy + GNU Hash + Versioning: Task List

**Status:** In Progress
**Source Ref:** phase-76d
**Depends on:** Phase 76 ✅, Phase 76b ✅, Phase 76c ✅
**Goal:** Add PLT lazy resolution (`_dl_runtime_resolve`), `DT_GNU_HASH` lookup, and graceful `DT_VERSYM` / `DT_VERNEED` handling. Complete the Phase 76 dynamic-linker theme.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| S1 | `sym.rs` refactor — unified `lookup(scope, name, version)` API over the 76b `DT_HASH` backend + route 76b runtime reloc write-sites through `ldso_core::reloc` slice helpers | Phase 76b, 76c | ✅ Complete |
| D1 | `DT_GNU_HASH` Bloom + bucket + chain lookup; dispatcher prefers GNU over SysV | S1 ✅ | ✅ Complete (end-to-end gate ships with F) |
| D2 | `DT_VERSYM` / `DT_VERNEED` / `DT_VERDEF` graceful handling | D1 | Planned |
| B4 | `_dl_runtime_resolve` asm trampoline + GOT slot rewrite + lazy `JUMP_SLOT` deferral | S1 ✅ | ✅ Complete |
| E4 | `LD_BIND_NOW` environment variable honored | B4 ✅ | In Progress |
| F | New gate variant: a `.so` built with `-Wl,--hash-style=gnu` runs end-to-end | B4, D1 | Planned |
| G | New gate variant: a versioned `.so` (with `DT_VERSYM` / `DT_VERNEED`) loads end-to-end, with mismatch-fallback and `LD_BIND_NOW` strict-mode coverage | D2, E4 | Planned |
| H | docs/76-dynamic-linker.md polish-pass + kernel version bump + mark Phase 76 family Complete | All | Planned |

---

## Track S1 — Symbol-Lookup Refactor

### S1.1 — Unified `sym::lookup` API

**File:** `userspace/ld-musl-x86_64.so.1/src/sym.rs`
**Symbol:** `lookup`
**Why it matters:** D1, D2, and B4 all need to call into the same lookup surface; without a unified API, each track diverges and the GOT-rewrite path silently bypasses versioning.

**Acceptance:**
- [ ] `fn lookup(scope: &Scope, name: &str, version: Option<&str>) -> Option<ResolvedSymbol>`.
- [ ] Internally dispatches to a `Backend::SysV` (existing `DT_HASH`) implementation; the dispatch lookup is a single match.
- [ ] Behavior is byte-for-byte unchanged vs. 76b's `DynamicSection::lookup_symbol` (validated by a regression run of the 76b smoke gate after the refactor).

### S1.2 — Reroute 76b and 76c consumers through `sym::lookup`

**Files:**
- `userspace/ld-musl-x86_64.so.1/src/reloc.rs`
- `userspace/ld-musl-x86_64.so.1/src/dl.rs`

**Symbol:** Updated call sites in `apply_rela_table`, `apply_jmprel_table`, `dlsym`.
**Why it matters:** D1's GNU-hash backend, D2's version table, and B4's lazy resolver only deliver their benefit if every existing consumer routes through `sym::lookup`.

**Acceptance:**
- [ ] `apply_rela_table` and `apply_jmprel_table` call `sym::lookup` instead of `DynamicSection::lookup_symbol` directly.
- [ ] `dlsym` calls `sym::lookup`.
- [ ] All 76b and 76c smoke gates pass after the refactor.

### S1.3 — Route runtime relocation write-sites through `ldso_core::reloc` slice helpers

**Files:**
- `userspace/ld-musl-x86_64.so.1/src/main.rs`

**Symbols:** `dl_relocate_self`, `apply_rela` (R_X86_64_RELATIVE / R_X86_64_GLOB_DAT / R_X86_64_JUMP_SLOT arms).
**Why it matters:** Phase 76b's host-tested `apply_relative` / `apply_glob_dat` / `apply_abs64` primitives in `ldso_core::reloc` take a `&mut [u8]` image slice and perform their own alignment + bounds check; the runtime currently bypasses three of those four paths and writes via `core::ptr::write_unaligned` directly (only the `R_X86_64_64` arm at `main.rs:885` already routes through `apply_abs64`). Routing the remaining three write-sites through the slice helpers eliminates the divergence between what the host tests prove and what the runtime actually executes, and lets a future malformed-input regression be caught at the same byte-level the host tests pin. Declined in PR #194 review thread `PRRT_kwDORTRVIM6FBzyo` as a 76c/76d follow-up; pinned here so it is not lost.

**Acceptance:**
- [ ] `dl_relocate_self`'s `R_X86_64_RELATIVE` write (`main.rs:301`) calls `ldso_core::reloc::apply_relative` against a `&mut [u8]` view of the linker's own image instead of `core::ptr::write_unaligned`.
- [ ] `apply_rela`'s `R_X86_64_RELATIVE` arm (`main.rs:849`) calls `apply_relative` against a `&mut [u8]` view of the loaded DSO's image.
- [ ] `apply_rela`'s `R_X86_64_GLOB_DAT` / `R_X86_64_JUMP_SLOT` arm (`main.rs:865`) calls `apply_glob_dat` against the same slice view.
- [ ] The `&mut [u8]` view is constructed from `load_bias` + `image_len` (which `LoadedDso` already carries since PR #194's bounds-check fix); no new field is needed.
- [ ] All 76b smoke gates (`dynlink-hello-smoke`, `dynlink-missing-smoke`, `dynlink-cycle-smoke`) and the 76c `dlopen-test-smoke` gate pass after the refactor.

---

## Track D1 — `DT_GNU_HASH`

### D1.1 — GNU hash function + Bloom filter probe

**File:** `userspace/ld-musl-x86_64.so.1/src/sym.rs`
**Symbol:** `gnu_hash`, `bloom_probe`
**Why it matters:** Most symbol lookups will short-circuit at the Bloom filter; getting the filter wrong silently mis-resolves symbols (returns the wrong address, not "not-found").

**Acceptance:**
- [ ] `gnu_hash(name: &[u8]) -> u32` implements the djb2 variant from the GNU ABI (`h = h * 33 + c`).
- [ ] `bloom_probe(hash: u32, bloom: &[u64], shift: u32) -> bool` returns `true` if both indexed bits are set.
- [ ] Unit-tested with: known-hash fixtures (the GNU ABI spec lists test values); a synthetic library whose `DT_GNU_HASH` table is constructed by hand.

### D1.2 — Bucket + chain walk

**File:** `userspace/ld-musl-x86_64.so.1/src/sym.rs`
**Symbol:** `gnu_hash_lookup`
**Why it matters:** This is the actual symbol-table walk; correctness gates every relocation against a GNU-hashed `.so`.

**Acceptance:**
- [ ] Reads `nbuckets`, `symoffset`, `bloom_size`, `bloom_shift` from the `DT_GNU_HASH` header.
- [ ] Walks the bucket array indexed by `hash % nbuckets` to find the chain start.
- [ ] Iterates the chain; stops at the first chain entry whose top bit is set (the chain-end marker).
- [ ] Returns `Option<ResolvedSymbol>`.

### D1.3 — Dispatcher prefers GNU over SysV

**File:** `userspace/ld-musl-x86_64.so.1/src/sym.rs`
**Symbol:** `lookup` (extended)
**Why it matters:** Libraries built with `--hash-style=both` carry both tables; the dispatcher must pick GNU first to benefit from the Bloom-filter short-circuit.

**Acceptance:**
- [ ] When both `DT_GNU_HASH` and `DT_HASH` are present, GNU is used.
- [ ] When only `DT_HASH` is present, SysV is used (76b fallback).
- [ ] When only `DT_GNU_HASH` is present, GNU is required (no SysV fallback).
- [ ] A library with neither logs a warning and the load returns `LoadError::NoHashTable`.

---

## Track D2 — Symbol Versioning

### D2.1 — `DT_VERSYM` / `DT_VERNEED` / `DT_VERDEF` parser

**File:** `userspace/ld-musl-x86_64.so.1/src/ver.rs`
**Symbol:** `VersionTable::parse`
**Why it matters:** Every glibc-built `.so` carries versioning data; without a parser, the resolver cannot know which version of a symbol to bind.

**Acceptance:**
- [ ] `VersionTable` carries: per-defined-symbol version index (from `DT_VERSYM`), per-version-name string (from `DT_VERDEF`), per-required-symbol version constraint (from `DT_VERNEED`).
- [ ] Unit-tested with a hand-constructed minimal versioned-symbol fixture.

### D2.2 — Versioned symbol lookup

**File:** `userspace/ld-musl-x86_64.so.1/src/sym.rs`
**Symbol:** `lookup` (version-aware path)
**Why it matters:** Without version-aware lookup, every versioned-symbol consumer either fails to load or binds to the wrong implementation.

**Acceptance:**
- [ ] When the consumer's `DT_VERNEED` specifies a version for a symbol, `sym::lookup` matches against the provider's `DT_VERSYM` / `DT_VERDEF`.
- [ ] Exact-version match returns the matched symbol.
- [ ] No exact-version match falls back to an unversioned lookup and emits a `log::warn!` recording the version-name and library.
- [ ] No symbol-name match returns `None` (existing not-found behavior).

### D2.3 — Strict mode under `LD_BIND_NOW`

**File:** `userspace/ld-musl-x86_64.so.1/src/sym.rs`
**Symbol:** `lookup` (strict path)
**Why it matters:** Diagnostic builds need to surface version mismatches as hard errors; warn-only is the wrong default for production debug builds. End-to-end gated by **G.3**.

**Acceptance:**
- [ ] When `LD_BIND_NOW=1`, a version mismatch returns `None` instead of falling back to unversioned lookup, and the log line is `log::error!` rather than `log::warn!`.

---

## Track B4 — PLT Lazy Resolve

### B4.1 — `_dl_runtime_resolve` asm trampoline

**File:** `userspace/ld-musl-x86_64.so.1/src/plt.rs`
**Symbol:** `_dl_runtime_resolve`
**Why it matters:** The PLT calls this with two stack arguments and a strict ABI contract — every clobbered register that isn't explicitly saved corrupts the caller. There is no way to debug this incrementally; it has to be right the first time.

**Acceptance:**
- [ ] `#[naked] extern "C" fn _dl_runtime_resolve()`.
- [ ] Saves all caller-saved registers (`rax`, `rcx`, `rdx`, `rsi`, `rdi`, `r8`, `r9`, `r10`, `r11`) on the stack.
- [ ] Pops the two PLT-pushed arguments (link-map, reloc-index) into `rdi` and `rsi`.
- [ ] Calls `plt::resolve_pltrel` (which returns the resolved address in `rax`).
- [ ] Stores the resolved address in a scratch register before restoring caller-saved registers.
- [ ] Restores the caller-saved registers.
- [ ] Adjusts `rsp` to discard the two PLT-pushed arguments.
- [ ] Jumps to the resolved address.

### B4.2 — `plt::resolve_pltrel` Rust callback

**File:** `userspace/ld-musl-x86_64.so.1/src/plt.rs`
**Symbol:** `resolve_pltrel`
**Why it matters:** The Rust side is where the actual symbol lookup happens; it must also write the resolved address into the GOT so subsequent calls bypass the trampoline.

**Acceptance:**
- [ ] `extern "C" fn resolve_pltrel(link_map: *const LinkMap, reloc_index: usize) -> usize`.
- [ ] Reads the `DT_JMPREL` entry at index `reloc_index` to get the symbol index and GOT offset.
- [ ] Calls `sym::lookup` with the symbol name and (if present) version.
- [ ] Writes the resolved address into the GOT slot.
- [ ] Returns the resolved address to the asm side.

### B4.3 — Install trampoline at `GOT[2]` + link-map at `GOT[1]`

**File:** `userspace/ld-musl-x86_64.so.1/src/dynlink.rs`
**Symbol:** `setup_lazy_resolution`
**Why it matters:** The PLT's plt0 stub jumps through `GOT[2]` and expects the link-map in `GOT[1]`; without correct setup, the first PLT call jumps to garbage.

**Acceptance:**
- [ ] At load time, after relocations are applied but before constructors run, write `&_dl_runtime_resolve` to `GOT[2]` and `&LinkMap` to `GOT[1]` for each DSO.
- [ ] The GOT region is mapped `RW-` (writable, not executable).
- [ ] `_dl_runtime_resolve` and the function bodies it targets are mapped `R-X` (read+exec, not writable).

### B4.4 — Switch eager `JUMP_SLOT` to lazy when permitted

**File:** `userspace/ld-musl-x86_64.so.1/src/reloc.rs`
**Symbol:** `apply_jmprel_table` (extended)
**Why it matters:** Without this switch, the trampoline is installed but never invoked — every PLT entry is still pre-resolved.

**Acceptance:**
- [ ] When `RTLD_NOW` is set on the DSO's open or `LD_BIND_NOW=1` is in the environment, every `JUMP_SLOT` is resolved eagerly (76b/76c behavior).
- [ ] Otherwise, `JUMP_SLOT` resolution is deferred: the GOT slot is rebased by `load_bias` so that the PLT's plt0 stub is reached on first call.

---

## Track E4 — `LD_BIND_NOW`

### E4.1 — Environment-variable plumbing

**File:** `userspace/ld-musl-x86_64.so.1/src/dynlink.rs`
**Symbol:** `read_env_flag`
**Why it matters:** Without `LD_BIND_NOW`, diagnostic builds cannot surface missing-symbol errors at load time, which makes debugging dynamic-link failures painful.

**Acceptance:**
- [ ] At linker startup, walk `envp` for `LD_BIND_NOW`; if present and non-empty / non-zero, set the global `BIND_NOW` flag.
- [ ] `BIND_NOW` is consulted by Track B4 (skip lazy) and Track D2 (strict version handling).
- [ ] An `LD_BIND_NOW=1 dlopen_test` run resolves every symbol at load time (validated by the gate).

---

## Track F — `--hash-style=gnu` Gate Variant

### F.1 — `libhello_gnu.so` + `dynlink_hello_gnu` artifacts

**Files:**
- `userspace/lib/libhello_gnu/hello.c` (or reuses `userspace/lib/libhello/hello.c` with different build flags)
- `userspace/dynlink_hello_gnu/dynlink_hello_gnu.c`

**Symbol:** `hello_str`, `main`
**Why it matters:** The whole point of 76d is to load GNU-hashed libraries; without a GNU-hashed test artifact, the gate is meaningless.

**Acceptance:**
- [ ] `libhello_gnu.so` is built with `-Wl,--hash-style=gnu` (no `DT_HASH`, only `DT_GNU_HASH`).
- [ ] `dynlink_hello_gnu` links against `libhello_gnu.so` and is itself built with `--hash-style=gnu`.
- [ ] `readelf -d target/generated-libs/libhello_gnu.so` shows `DT_GNU_HASH` present and `DT_HASH` absent.

### F.2 — `cargo xtask dynlink-hello-gnu-smoke` gate

**File:** `xtask/src/main.rs`
**Symbol:** `dynlink_hello_gnu_smoke`
**Why it matters:** Without the gate, the GNU-hash path regresses silently the moment any of D1/B4 is broken.

**Acceptance:**
- [ ] Subcommand boots QEMU, execs `/bin/dynlink_hello_gnu`, asserts `HELLO_FROM_GNU_LIB:OK` on serial.
- [ ] Subcommand asserts that the first call to `hello_str` went through `_dl_runtime_resolve` (by reading the GOT slot before and after the first call from a small inline test).
- [ ] Smoke-runner emits `SMOKE:dynlink-hello-gnu-smoke:PASS` / `:FAIL` and is wired into the standard `cargo xtask smoke-test` step list.

### F.3 — `LD_BIND_NOW=1` regression gate

**File:** `xtask/src/main.rs`
**Symbol:** `dynlink_hello_gnu_smoke` (extended)
**Why it matters:** Without an `LD_BIND_NOW` regression, the strict-mode path silently regresses.

**Acceptance:**
- [ ] Subcommand boots QEMU with `LD_BIND_NOW=1` in the environment of `dynlink_hello_gnu`; asserts that PLT entries are resolved at load time (verified by reading the GOT slot before the first call and asserting it already holds the function address).

### F.4 — W^X assertion at GOT-rewrite boundary

**File:** `xtask/src/main.rs`
**Symbol:** `dynlink_hello_gnu_smoke` (extended)
**Why it matters:** The W^X invariant at the trampoline boundary is the most subtle correctness gate in the Phase 76 family and must be enforced by the smoke gate.

**Acceptance:**
- [ ] An inline test in `dynlink_hello_gnu` reads `/proc/self/maps` (or the m3OS equivalent) and asserts that the GOT region is `rw-` and the `.text` of `libhello_gnu.so` is `r-x`.
- [ ] Gate asserts the sentinel `WX_CHECK:OK` on serial.

---

## Track G — Symbol-Versioning Gate Variant

### G.1 — `libhello_versioned.so` + `dynlink_hello_versioned` artifacts

**Files:**
- `userspace/lib/libhello_versioned/hello.c` (or reuses `userspace/lib/libhello/hello.c` with a version script)
- `userspace/lib/libhello_versioned/libhello_versioned.ver` (linker version script declaring `LIBHELLO_1.0`)
- `userspace/dynlink_hello_versioned/dynlink_hello_versioned.c`

**Symbols:** `hello_str@LIBHELLO_1.0`, `main`
**Why it matters:** Without a versioned-symbol test artifact, D2's correctness (parser + version-aware lookup) is unverifiable end-to-end and regresses silently between releases.

**Acceptance:**
- [ ] `libhello_versioned.so` is built with `-Wl,--version-script=libhello_versioned.ver,--hash-style=gnu` and exports `hello_str` under version `LIBHELLO_1.0`.
- [ ] `dynlink_hello_versioned` links against `libhello_versioned.so` and carries a `DT_VERNEED` referring to `libhello_versioned.so` / `LIBHELLO_1.0`.
- [ ] `readelf -V target/generated-libs/libhello_versioned.so` shows `DT_VERSYM` + `DT_VERDEF` populated with `LIBHELLO_1.0` under "Version definitions".
- [ ] `readelf -V target/generated-bins/dynlink_hello_versioned` shows `Version needs` referencing `libhello_versioned.so` / `LIBHELLO_1.0`.

### G.2 — `cargo xtask dynlink-hello-versioned-smoke` exact-match gate

**File:** `xtask/src/main.rs`
**Symbol:** `dynlink_hello_versioned_smoke`
**Why it matters:** Without the gate, D2.2's exact-version match path regresses silently the moment any of D2.1 / D2.2 breaks.

**Acceptance:**
- [ ] Subcommand boots QEMU, execs `/bin/dynlink_hello_versioned`, asserts `HELLO_FROM_VERSIONED_LIB:OK` on serial (proves `hello_str@LIBHELLO_1.0` resolved through the version-aware path).
- [ ] The smoke gate also asserts that NO `log::warn!` line referencing a version mismatch appears on serial during the run (negative assertion: the positive path is silent).
- [ ] Smoke-runner emits `SMOKE:dynlink-hello-versioned-smoke:PASS` / `:FAIL` and is wired into the standard `cargo xtask smoke-test` step list.

### G.3 — Mismatch-fallback and `LD_BIND_NOW` strict-mode gates

**Files:**
- `userspace/lib/libhello_versioned_v2/hello.c` + `libhello_versioned_v2.ver` declaring `LIBHELLO_2.0` (exports `hello_str@LIBHELLO_2.0` plus an unversioned `hello_str` so the fallback target exists)
- `userspace/dynlink_hello_versioned_mismatch/dynlink_hello_versioned_mismatch.c` linked against `libhello_versioned.so` (requiring `LIBHELLO_1.0`) at build time, then at boot the consumer is started with `libhello_versioned_v2.so` resolved via an `LD_LIBRARY_PATH`-shaped redirect (or by staging `libhello_versioned_v2.so` at `/usr/lib/libhello_versioned.so.1` so the `DT_NEEDED` SONAME match picks up the v2 library).

**Symbol:** `dynlink_hello_versioned_smoke` (extended)
**Why it matters:** D2.2's "no exact-version match → unversioned fallback + `log::warn!`" path is the policy that lets m3OS load real-world glibc-built `.so` files whose versions have no m3OS equivalent. D2.3's "under `LD_BIND_NOW`, version mismatch is a hard error" path is the diagnostic counterpart. Both regress silently without a gate.

**Acceptance:**
- [ ] Default-env (`LD_BIND_NOW` unset) run of `dynlink_hello_versioned_mismatch`: linker emits a `log::warn!` containing both the unmatched version name (`LIBHELLO_1.0`) and the providing library SONAME (`libhello_versioned.so.1`), falls back to the unversioned `hello_str`, and the binary still prints `HELLO_FROM_VERSIONED_LIB:OK`. Gate greps serial for both the warn-line substrings AND the success sentinel.
- [ ] `LD_BIND_NOW=1` run of the same binary: linker emits `log::error!` for the version mismatch and the binary exits non-zero (no fallback). Gate asserts the error-line substrings present and the exit code is non-zero (via `run_command_expect_exit`).

---

## Track H — Documentation + Version Bump + Phase 76 Family Closure

### H.1 — Bump kernel version to `0.76.3`

**Files:**
- `kernel/Cargo.toml`
- `Cargo.lock` (regenerated)

**Symbol:** `package.version`
**Why it matters:** Phase 76d is the third 76 sub-phase; the patch bump keeps the running banner accurate.

**Acceptance:**
- [ ] `kernel/Cargo.toml` `version = "0.76.3"`.
- [ ] `Cargo.lock` regenerated and checked in.
- [ ] Boot banner prints `m3OS 0.76.3`.

### H.2 — Extend `docs/76-dynamic-linker.md` with 76d sections + final polish

**File:** `docs/76-dynamic-linker.md`
**Symbol:** N/A (existing learning doc, finalized)
**Why it matters:** With 76d shipping, the learning doc must describe the lazy-resolve trampoline, the GNU-hash lookup, the version-table handling, and the `LD_BIND_NOW` knob. After 76d, the doc covers the dynamic linker as a complete subsystem.

**Acceptance:**
- [ ] New "What changes in 76d" section describes `_dl_runtime_resolve`, `DT_GNU_HASH`, `DT_VERSYM` / `DT_VERNEED`, and `LD_BIND_NOW`.
- [ ] Key Files table extended with `userspace/ld-musl-x86_64.so.1/src/plt.rs`, `sym.rs`, `ver.rs`.
- [ ] Subphase table at the top of the doc updates 76d's row to reflect the gate is now wired.
- [ ] Front-matter `Status` updated from "Implemented (scaffolding only — 76b/76c/76d ship the rest)" to "Complete (Phase 76 family shipped through 76d)".
- [ ] "Deferred Until Later" section in the learning doc lists the remaining gaps: TLS, IFUNC, `DT_FILTER` / `DT_AUXILIARY`, `dlmopen`, `LD_BIND_NOT`.

### H.3 — Mark Phase 76 family Complete in roadmap README

**File:** `docs/roadmap/README.md`
**Symbol:** Phase 76 / 76b / 76c / 76d table rows
**Why it matters:** The roadmap README is the canonical phase index; without an update, the index lies about the state of the dynamic-linker theme.

**Acceptance:**
- [ ] The existing Phase 76d row (currently `Status = Planned`, Primary Outcome lists `_dl_runtime_resolve` + `DT_GNU_HASH` + graceful `DT_VERSYM`/`DT_VERNEED` only) is updated to `Status = Complete` and its Primary Outcome column is extended to include `LD_BIND_NOW` strict-mode coverage and the new gates (`dynlink-hello-gnu-smoke`, `dynlink-hello-versioned-smoke`).
- [ ] All four Phase 76 family rows (76, 76b, 76c, 76d) carry Status `Complete` after this task lands.

### H.4 — Update `AGENTS.md` project-overview paragraph

**File:** `AGENTS.md`
**Symbol:** Phase 76 paragraph (extended with Phase 76d clause)
**Why it matters:** The project-overview paragraph is the single most-read summary of the current state of m3OS.

**Acceptance:**
- [ ] Phase 76d clause added describing: `_dl_runtime_resolve` asm trampoline + lazy `JUMP_SLOT`, `DT_GNU_HASH` Bloom + bucket + chain, `DT_VERSYM` / `DT_VERNEED` graceful handling, `LD_BIND_NOW` strict mode, `dynlink-hello-gnu-smoke` + `dynlink-hello-versioned-smoke` gates, kernel version `0.76.3`.
- [ ] The "Phase 76b/76c/76d are tracked as separate roadmap phases" sentence from the Phase 76 paragraph is updated to reflect that all three have shipped.

---

## Documentation Notes

- The original (pre-split) Phase 76 task list's B.4 / D.1 / D.2 acceptance items migrate here verbatim, restructured to match the per-track template.
- The Phase 75 W^X invariant for PLT trampolines (GOT in `RW-`, resolved target in `R-X`) is reaffirmed as the acceptance gate for B4.3 and F.4.
- 76d kernel version is `0.76.3` (patch). The dynamic-linker theme is intentionally not promoted to `0.77.0` — Phase 77 will be a separate theme.
- The 76d learning content is added to the existing `docs/76-dynamic-linker.md` (extended by 76b and 76c); no new learning doc is created.
- The `sym.rs` refactor in Track S1 is intentionally the first task: every subsequent track depends on its unified `lookup` surface, and shipping it first lets D1 / D2 / B4 land independently without re-touching every consumer.
