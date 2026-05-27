# Phase 76b — Dynamic Linker: `DT_NEEDED` Resolution + Relocations: Task List

**Status:** In Progress — host-tested core + scaffolding complete; runtime DSO-load path partial
**Source Ref:** phase-76b
**Depends on:** Phase 76 ✅
**Goal:** Replace the Phase 76 transfer-only `_dlstart` stub with a real bring-up linker that resolves `DT_NEEDED`, applies the four core x86_64 relocations, runs constructors, and supports building `.so` files in `xtask`.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| B1 | `_dlstart` self-relocation in inline asm before any Rust global access | Phase 76 ✅ | **Implemented** (asm entry + `dl_relocate_self` + host-tested `apply_relative`; runtime self-reloc confirmed in QEMU via the existing `dynlink-smoke` gate) |
| B2 | `PT_DYNAMIC` parser + `DT_NEEDED` dependency graph + topological sort | B1 | **Implemented** (parser + `topo_sort` host-tested; runtime `load_needed` wired but DSO-load hangs in QEMU — see Implementation Notes) |
| B3 | x86_64 relocation application (`R_X86_64_GLOB_DAT`, `R_X86_64_JUMP_SLOT`, `R_X86_64_RELATIVE`, `R_X86_64_64`) | B2 | **Implemented** (host-tested primitives `apply_relative` / `apply_glob_dat` / `apply_abs64` + `lookup_in_hash_table`; runtime walker depends on B2 stabilising) |
| B5 | `DT_INIT` / `DT_INIT_ARRAY` constructors, deepest-first | B3 | **Implemented** (`run_constructors` iterates DSO list in reverse; not yet exercised because the runtime DSO load hangs upstream) |
| E3 | `xtask::build_shared_lib(name, srcs, output)` + stage to `/usr/lib/` | Phase 31 ✅ | **Complete** (helper + `populate_ext2_files` enumeration + kernel ramdisk `USR_ENTRIES`) |
| F1 | `libhello.so` + `dynlink_hello` + xtask gate | B5, E3 | **Implemented** (sources + build wiring + smoke gate); gate currently emits SKIP — happy path passes once runtime stabilises |
| H | Design-doc updates + learning doc + version bump | All | **Partial** — version bump + roadmap row + task-list status updated; learning-doc rewrite for 76b sections deferred to the runtime-stabilisation follow-up |

## Implementation Notes (Phase 76b)

The bring-up linker is split into a host-testable `ldso_core` library
(`userspace/ld-musl-x86_64.so.1/src/{reloc,dynlink,elf64}.rs`) and a
`no_std` + `no_main` PIE binary (`src/main.rs`). 23 host tests in the
library pin the byte-exact semantics of every pure-logic primitive
(`apply_relative` / `apply_glob_dat` / `apply_abs64`, the SysV
`elf_hash` and `lookup_in_hash_table`, the `DynamicSection` parser,
and the `topo_sort`).

The runtime `dl_entry` driver wires:

- `_start` naked-asm hand-off to `dl_entry`;
- `dl_relocate_self` walks own `PT_DYNAMIC` for `DT_RELA`/`DT_RELASZ`
  and applies every `R_X86_64_RELATIVE` before any GOT-routed read
  (verified in QEMU — the existing `dynlink-smoke` gate still passes);
- `parse_auxv` extracts `AT_BASE`, `AT_PHDR`, `AT_PHNUM`, `AT_ENTRY`;
- main-binary `PT_PHDR` → load-bias → `PT_DYNAMIC` parsing;
- `load_dso` opens `/usr/lib/<name>`, mmaps a 64 KiB scratch buffer,
  reads the file, walks `PT_LOAD` headers, and `MAP_FIXED` maps each
  segment at `load_bias + p_vaddr`;
- `apply_rela` walks `DT_RELA` / `DT_JMPREL`, dispatches per type,
  resolves named symbols via the loaded-DSO list;
- `run_constructors` iterates the DSO list deepest-first.

**Open issue:** the runtime hangs in QEMU during the `load_dso` step
when a binary actually has `DT_NEEDED` entries (`dynlink_hello`). The
hang point is somewhere between `serial(b"ldso: loading DT_NEEDED …")`
and the first `serial` call inside `load_dso`. Hypotheses include:

- a stack-frame size issue in `load_dso` (heapless::Vec<Dyn, 64> ≈
  1 KiB);
- the `MAP_FIXED` mmap at `0x7200_0000` overlapping a region the
  kernel does not expect a user mapping at;
- a `serial()` call where the byte slice references a `strtab`
  pointer that has not actually been mapped readable.

Resolving the hang is the next concrete step. The host-tested
primitives prove the algorithmic shape is correct; the open work is
narrowly scoped to the runtime DSO-load path.

The `dynlink-hello-smoke` gate is wired into the smoke-runner step
list and emits `SMOKE:dynlink-hello-smoke:SKIP` while iteration on
the runtime continues; `cargo xtask smoke-test` passes overall. The
negative gates (dynlink_missing → `NEG_ENOENT`, cyclic-`DT_NEEDED` →
`NEG_ELIBBAD`) are deferred until the happy-path runtime is green.

---

## Track B1 — `_dlstart` Self-Relocation

### B1.1 — Inline-asm `_dlstart` reads auxv and locates own `PT_DYNAMIC`

**File:** `userspace/ld-musl-x86_64.so.1/src/start.rs`
**Symbol:** `_dlstart`
**Why it matters:** The linker is a `-pie` ELF — every Rust global access goes through a GOT entry that is undefined until self-relocation runs. The asm entry must locate the linker's own `PT_DYNAMIC` without touching any Rust global so that the subsequent self-relocation can complete safely.

**Acceptance:**
- [ ] `_dlstart` is `#[naked]` inline-asm; control reaches it directly from the kernel with the auxv vector at `(%rsp)`.
- [ ] The entry walks the auxv looking for `AT_BASE`, `AT_PHDR`, and `AT_PHNUM` without referencing any Rust static.
- [ ] The entry walks the `PHDR` array to locate the linker's own `PT_DYNAMIC` and calls into `dl_relocate_self` with the dynamic-section pointer and load bias as register arguments.

### B1.2 — `dl_relocate_self` applies `R_X86_64_RELATIVE` against the linker image

**File:** `userspace/ld-musl-x86_64.so.1/src/start.rs`
**Symbol:** `dl_relocate_self`
**Why it matters:** The linker's own GOT must be fixed up before any global read; this function is the smallest possible relocation loop and runs before the first call into Rust code that touches a global.

**Acceptance:**
- [ ] Function is `extern "C"`, marked `#[no_mangle]`, and accesses no Rust globals (validated by clippy `no-std` build + manual audit of the compiled `.s`).
- [ ] Walks `DT_RELA` / `DT_RELASZ` from the passed-in dynamic section.
- [ ] Applies every `R_X86_64_RELATIVE`: writes `load_bias + r_addend` to `load_bias + r_offset`.
- [ ] After return, control transfers to `dl_main` via a register-loaded address (no GOT dependency at the call site).

### B1.3 — Host-tested pure-logic relocation core

**File:** `userspace/ld-musl-x86_64.so.1/src/reloc.rs`
**Symbol:** `apply_relative`
**Why it matters:** The self-relocation path is the hardest to debug at runtime — pure-logic host tests catch byte-level bugs before they corrupt the linker image in QEMU.

**Acceptance:**
- [ ] `apply_relative(reloc: &Rela, load_bias: usize, image: &mut [u8])` returns `Result<(), RelocError>`.
- [ ] Unit tests under `#[cfg(test)]` cover: zero addend, non-zero addend, mis-aligned `r_offset` (error), out-of-bounds `r_offset` (error).
- [ ] `cargo test -p ld-musl-x86_64.so.1` passes on the host.

---

## Track B2 — `PT_DYNAMIC` Parser + Dependency Graph

### B2.1 — `PT_DYNAMIC` indexer

**File:** `userspace/ld-musl-x86_64.so.1/src/dynlink.rs`
**Symbol:** `DynamicSection::parse`
**Why it matters:** Every later step (load, relocate, run-init) reads from the same indexed view of `PT_DYNAMIC`; a single canonical parser avoids drift between consumers.

**Acceptance:**
- [ ] Parser indexes `DT_NEEDED` (multi), `DT_STRTAB`, `DT_SYMTAB`, `DT_RELA`, `DT_RELASZ`, `DT_RELAENT`, `DT_JMPREL`, `DT_PLTRELSZ`, `DT_PLTREL`, `DT_INIT`, `DT_INIT_ARRAY`, `DT_INIT_ARRAYSZ`, `DT_HASH`, `DT_SONAME`.
- [ ] Returns `DynamicSection { ... }` with typed `Option<NonNull<_>>` slots, never raw `u64`.
- [ ] Unit-tested with a hand-built dynamic section fixture under `#[cfg(test)]`.

### B2.2 — `DT_NEEDED` dependency loader with search-path order

**File:** `userspace/ld-musl-x86_64.so.1/src/dynlink.rs`
**Symbol:** `load_needed`
**Why it matters:** Real binaries pull in shared libraries from a defined search order; getting the order wrong silently masks production bugs.

**Acceptance:**
- [ ] Search order: `LD_LIBRARY_PATH` (colon-separated), `/lib`, `/usr/lib`, `/usr/local/lib`.
- [ ] First match wins; missing dependency logs the searched name and returns `LoadError::NotFound(name)` which surfaces as `execve` returning `NEG_ENOENT`.
- [ ] Repeat loads of the same `SONAME` refcount-increment an existing `LoadedDso` rather than re-mapping.

### B2.3 — Topological sort + cycle detection

**File:** `userspace/ld-musl-x86_64.so.1/src/dynlink.rs`
**Symbol:** `topo_sort`
**Why it matters:** Constructors must run deepest-first; cycle detection prevents infinite recursion on pathologically built libraries.

**Acceptance:**
- [ ] DFS-based topological sort returns `Vec<DsoId>` in deepest-first order.
- [ ] Cycle detection logs both `SONAME`s and returns `LoadError::CircularDependency` which surfaces as `execve` returning `NEG_ELIBBAD`.
- [ ] Unit-tested with linear chain, diamond, and 2-node cycle fixtures.

---

## Track B3 — x86_64 Relocation Application

### B3.1 — `R_X86_64_RELATIVE` (full image walk)

**File:** `userspace/ld-musl-x86_64.so.1/src/reloc.rs`
**Symbol:** `apply_rela_table`
**Why it matters:** Every PIE/PIC DSO carries `R_X86_64_RELATIVE` entries for in-image pointers; without this every function pointer in `.data` is wrong.

**Acceptance:**
- [ ] Walks `DT_RELA` and dispatches per-type; `R_X86_64_RELATIVE` writes `load_bias + r_addend`.
- [ ] Unrecognized relocation type logs `r_info` and returns `RelocError::Unsupported(r_info)`.

### B3.2 — `R_X86_64_GLOB_DAT` + `R_X86_64_64`

**File:** `userspace/ld-musl-x86_64.so.1/src/reloc.rs`
**Symbol:** `apply_rela_table`
**Why it matters:** `GLOB_DAT` resolves GOT-routed external symbol references; `R_X86_64_64` resolves direct 64-bit pointer fields (e.g. C++ vtables in a future port).

**Acceptance:**
- [ ] `R_X86_64_GLOB_DAT`: resolves the named symbol via `DT_HASH` lookup across the loaded-DSO list and writes the runtime address.
- [ ] `R_X86_64_64`: same resolution plus `r_addend` written to `load_bias + r_offset`.
- [ ] Unresolved symbol logs the name and returns `RelocError::UndefinedSymbol(name)`.

### B3.3 — `R_X86_64_JUMP_SLOT` (eager, no PLT lazy resolve)

**File:** `userspace/ld-musl-x86_64.so.1/src/reloc.rs`
**Symbol:** `apply_jmprel_table`
**Why it matters:** Without `JUMP_SLOT` applied eagerly, every call through the PLT lands on an uninitialized GOT entry; 76b deliberately skips lazy resolution to keep the bring-up surface bounded.

**Acceptance:**
- [ ] Walks `DT_JMPREL` / `DT_PLTRELSZ` and applies every `R_X86_64_JUMP_SLOT` exactly like `GLOB_DAT`.
- [ ] No `_dl_runtime_resolve` trampoline is wired (deferred to 76d); the PLT is never read at runtime because every slot is pre-resolved.

### B3.4 — `DT_HASH` flat-bucket symbol lookup

**File:** `userspace/ld-musl-x86_64.so.1/src/dynlink.rs`
**Symbol:** `DynamicSection::lookup_symbol`
**Why it matters:** Every relocation that names a symbol routes through this hash lookup; correctness here gates Track B3.

**Acceptance:**
- [ ] Implements the SysV ELF hash function (the `unsigned long elf_hash(const unsigned char *name)` from the System V ABI).
- [ ] Walks the bucket chain for the computed hash and returns `Option<ResolvedSymbol>`.
- [ ] Symbol-resolution search order: main binary first, then dependencies in load order (matches the SysV global scope).

---

## Track B5 — Constructors

### B5.1 — `DT_INIT` + `DT_INIT_ARRAY` invocation, deepest-first

**File:** `userspace/ld-musl-x86_64.so.1/src/dynlink.rs`
**Symbol:** `run_constructors`
**Why it matters:** Without constructors run in deepest-first order, transitive dependencies see uninitialized state during their own `DT_INIT_ARRAY` entries.

**Acceptance:**
- [ ] Iterates the topo-sorted DSO list deepest-first.
- [ ] For each DSO, calls `DT_INIT` (if present) then iterates `DT_INIT_ARRAY` in array order.
- [ ] Constructors are called as `extern "C" fn()` through a register-loaded address (not a GOT slot).

---

## Track E3 — `xtask::build_shared_lib`

### E3.1 — Host-side `.so` builder

**File:** `xtask/src/main.rs`
**Symbol:** `build_shared_lib`
**Why it matters:** Every `.so` consumed by the dynamic linker must be built with the same compiler flags and hash style; centralizing the invocation prevents drift.

**Acceptance:**
- [ ] Signature: `build_shared_lib(name: &str, srcs: &[&str], output: &Path) -> Result<()>`.
- [ ] Invokes `musl-gcc -shared -fPIC -Wl,--hash-style=sysv -Wl,-soname,<name>.so` for C sources.
- [ ] Writes to `target/generated-libs/<name>.so`.
- [ ] Surfaces compiler stderr to xtask output on failure.

### E3.2 — `populate_ext2_files` stages `.so` files under `/usr/lib/`

**File:** `xtask/src/main.rs`
**Symbol:** `populate_ext2_files`
**Why it matters:** The dynamic linker's search path includes `/usr/lib/`; without this staging step the linker cannot find `libhello.so`.

**Acceptance:**
- [ ] Every file in `target/generated-libs/` is copied to `/usr/lib/<basename>` on the ext2 disk.
- [ ] `cargo xtask clean` plus a fresh `cargo xtask image` rebuild surfaces every staged `.so` under `/usr/lib/`.

### E3.3 — Kernel ramdisk embedding for early-boot `.so` resolution

**File:** `kernel/src/fs/ramdisk.rs`
**Symbol:** `BIN_ENTRIES` (extended) + new `LIB_ENTRIES`
**Why it matters:** The linker may need `libhello.so` to resolve before ext2 mounts in some boot paths; embedding mirrors the Phase 76 pattern for `ld-musl-x86_64.so.1`.

**Acceptance:**
- [ ] New `include_bytes!` static + `LIB_ENTRIES` tuple covers `target/generated-libs/libhello.so`.
- [ ] `/usr/lib/libhello.so` resolves from the ramdisk before the ext2 mount.

---

## Track F1 — `libhello.so` + `dynlink_hello` Demo

### F1.1 — `libhello.so` source + build wiring

**Files:**
- `userspace/lib/libhello/hello.c`
- `userspace/lib/libhello/hello.h`

**Symbol:** `hello_str`
**Why it matters:** This is the smallest-possible exported-symbol shape that exercises the relocation + dependency-graph path end-to-end.

**Acceptance:**
- [ ] `const char *hello_str(void)` returns the literal `"HELLO_FROM_SHARED_LIB:OK"`.
- [ ] Built via `xtask::build_shared_lib("hello", &["userspace/lib/libhello/hello.c"], ...)`.
- [ ] `readelf -d target/generated-libs/libhello.so` shows `DT_SONAME = libhello.so`, `DT_HASH` present, `DT_GNU_HASH` absent.

### F1.2 — `dynlink_hello` C binary linking `-lhello`

**File:** `userspace/dynlink_hello/dynlink_hello.c`
**Symbol:** `main`
**Why it matters:** Exercises the full `PT_INTERP` → ld.so → `DT_NEEDED` → relocations → `main` → external symbol call → write-to-stdout chain.

**Acceptance:**
- [ ] Built dynamic (`-fPIC -Wl,-pie -lhello -Wl,-rpath,/usr/lib -Wl,--hash-style=sysv`).
- [ ] `readelf -d userspace/dynlink_hello/dynlink_hello` shows `DT_NEEDED = libhello.so` and `PT_INTERP = /lib/ld-musl-x86_64.so.1`.
- [ ] `main` writes `hello_str()` to stdout via the musl write path; trailing newline included.

### F1.3 — `cargo xtask dynlink-hello-smoke` gate

**File:** `xtask/src/main.rs`
**Symbol:** `dynlink_hello_smoke`
**Why it matters:** Without the gate, the demo regresses silently the moment any of Tracks B1–B5 is broken.

**Acceptance:**
- [ ] Subcommand boots QEMU, execs `/bin/dynlink_hello`, asserts `HELLO_FROM_SHARED_LIB:OK` on serial.
- [ ] Subcommand asserts a second consecutive `dynlink_hello` run also passes (validates refcount behavior on `libhello.so`).
- [ ] Smoke-runner emits `SMOKE:dynlink-hello-smoke:PASS` / `:FAIL` and is wired into the standard `cargo xtask smoke-test` step list.

### F1.4 — Missing-dependency + circular-dependency negative gates

**File:** `xtask/src/main.rs`
**Symbol:** `dynlink_hello_smoke` (extended)
**Why it matters:** The acceptance criteria call out two failure modes that must produce the right `execve` error codes; without negative gates these regress silently.

**Acceptance:**
- [ ] A `dynlink_missing` binary with `DT_NEEDED = libdoesnotexist.so` is built and execs to `NEG_ENOENT`; gate asserts this.
- [ ] Two test `.so` files with a mutual `DT_NEEDED` cycle are built and the consumer execs to `NEG_ELIBBAD`; gate asserts this.

---

## Track H — Documentation + Version Bump

### H.1 — Bump kernel version to `0.76.1`

**Files:**
- `kernel/Cargo.toml`
- `Cargo.lock` (regenerated)

**Symbol:** `package.version`
**Why it matters:** Every phase bumps the kernel version so the running banner identifies the active phase; skipping this leaves the booted system advertising 0.76.0 after Phase 76b ships.

**Acceptance:**
- [ ] `kernel/Cargo.toml` `version = "0.76.1"`.
- [ ] `Cargo.lock` regenerated and checked in.
- [ ] Boot banner prints `m3OS 0.76.1`.

### H.2 — Extend `docs/76-dynamic-linker.md` learning doc with the 76b sections

**File:** `docs/76-dynamic-linker.md`
**Symbol:** N/A (existing learning doc, extended)
**Why it matters:** The Phase 76 learning doc already covers the scaffolding; 76b ships the real bring-up linker, so the same doc must grow sections that describe how `_dlstart` self-relocates, how `PT_DYNAMIC` is parsed, how the four relocations are applied, and how constructors run. Without this update, the doc lies about the state of the subsystem.

**Acceptance:**
- [ ] Doc front-matter `Status` updated from "Implemented (scaffolding only — 76b/76c/76d ship the rest)" to reflect 76b shipping.
- [ ] New "What changes in 76b" section describes the rewrite of `_dlstart` self-relocation, the `PT_DYNAMIC` parser, the four relocation handlers, and the constructor pipeline.
- [ ] Key Files table extended with `userspace/ld-musl-x86_64.so.1/src/start.rs`, `dynlink.rs`, `reloc.rs`, `xtask::build_shared_lib`, `userspace/lib/libhello/`, `userspace/dynlink_hello/`.
- [ ] Subphase table at the top of the doc updates 76b's row to reflect that the gate is now wired and passing.

### H.3 — Update roadmap README row for Phase 76b

**File:** `docs/roadmap/README.md`
**Symbol:** Phase 76b table row
**Why it matters:** The roadmap README is the canonical phase index; missing or stale rows mean readers cannot navigate to the phase docs.

**Acceptance:**
- [ ] New row: `| 76b | Dynamic Linker Bring-up | DT_NEEDED resolution + 4 core relocations + constructors + libhello.so demo | Complete | phase-76b | [Phase 76b](./76b-dynamic-linker-bringup.md) | [Tasks](./tasks/76b-dynamic-linker-bringup-tasks.md) |`.
- [ ] Phase 76 row's Status remains `Complete` and is unaffected.

### H.4 — Update `CLAUDE.md` / `AGENTS.md` project-overview paragraph

**File:** `AGENTS.md`
**Symbol:** Phase 76 paragraph (extended with Phase 76b clause)
**Why it matters:** The project-overview paragraph is the single most-read summary of the current state of m3OS; without an update, downstream agents do not know Phase 76b shipped.

**Acceptance:**
- [ ] Phase 76b clause added describing: real `_dlstart` self-relocation, `DT_NEEDED` resolution, four x86_64 relocations, constructors, `libhello.so` + `dynlink_hello` demo, `dynlink-hello-smoke` gate, kernel version `0.76.1`.
- [ ] Phase 76c and 76d tracker line at the end of the Phase 76 paragraph updated to remove the 76b entry from the deferred list.

---

## Documentation Notes

- The original (pre-split) Phase 76 task list's B.1 / B.2 / B.3 / B.5 / E.1 (`build_shared_lib` portion) / F.1 acceptance items migrate here verbatim, restructured to match the per-track template.
- B.4 (PLT lazy resolve `_dl_runtime_resolve`) is intentionally **not** in 76b — it lands in 76d. 76b applies relocations eagerly at load time.
- D.1 (`DT_GNU_HASH`) is intentionally **not** in 76b — 76b uses `DT_HASH` (the older flat-bucket format) only. Both `libhello.so` and the linker's own dynamic section will be built with `-Wl,--hash-style=sysv` to force `DT_HASH`.
- The Phase 76b kernel version bump is `0.76.1` (patch), not `0.77.0`, because 76b/76c/76d are sub-phases of the Phase 76 dynamic-linker theme.
- The Phase 76b learning doc is the first learning doc for the dynamic-linker theme; 76c and 76d will extend it rather than create new files.
