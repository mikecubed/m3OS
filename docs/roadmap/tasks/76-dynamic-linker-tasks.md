# Phase 76 — Dynamic Linker / Shared Libraries: Task List

**Status:** Planned
**Source Ref:** phase-76
**Depends on:** Phase 11 (Process Model) ✅, Phase 12 (POSIX Compatibility) ✅, Phase 75 (W^X Enforcement) ✅, Phase 31 (TCC Compiler Bootstrap) ✅
**Goal:** Deliver dynamic linking by honoring `PT_INTERP` in the ELF loader, bringing up a musl-derived or fresh `ld.so`, implementing `dlopen`/`dlsym`/`dlclose`, and adding `.so` build support in `xtask`. All existing statically linked binaries must continue to work.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Kernel ELF loader `PT_INTERP` branch and aux vector | Phase 11 ✅, Phase 75 ✅ | Planned |
| B | `ld.so` bring-up: load, relocate, construct | A | Planned |
| C | `dlopen`/`dlsym`/`dlclose` runtime API | B | Planned |
| D | Symbol versioning (basic `DT_GNU_HASH`) | B | Planned |
| E | `xtask` `ld.so` and `.so` build pipeline and disk placement | Phase 31 ✅ | Planned |
| F | Test applications (`dynlink_hello`, `dlopen_test`) | B, C, E | Planned |
| G | Phase 11 and Phase 12 design doc updates | F | Planned |
| H | Documentation and Release | A–G | Planned |

---

## Track A — Kernel ELF Loader `PT_INTERP` Branch

### A.1 — Detect `PT_INTERP` and load the interpreter ELF

**File:** `kernel/src/elf/loader.rs`
**Symbol:** `load_elf_interp`
**Why it matters:** Without this branch, `exec` of a dynamically linked binary silently fails because the kernel tries to execute the main ELF without the linker having run.

**Acceptance:**
- [ ] `load_elf` scans PT segments; if `PT_INTERP` is present, it reads the interpreter path from segment content
- [ ] `load_elf_interp(path)` opens the interpreter from the VFS, parses its ELF header, and maps its PT_LOAD segments into the new process at a load bias computed to avoid collision with the main binary
- [ ] The interpreter's text segments are mapped `R-X`; its data segments are mapped `RW-|NX` (consistent with Phase 75)
- [ ] If the interpreter path does not exist in the VFS, `execve` returns `ENOENT` with a log line naming the missing path

### A.2 — Auxiliary vector construction

**File:** `kernel/src/elf/loader.rs`
**Symbol:** `build_auxv`
**Why it matters:** The dynamic linker reads the auxiliary vector to find the main binary's program headers, entry point, and page size; a missing or wrong `AT_BASE` causes the linker to compute wrong addresses for the main binary.

**Acceptance:**
- [ ] `build_auxv` constructs an `AT_NULL`-terminated auxiliary vector on the user stack after the environment strings
- [ ] Entries present: `AT_PHDR`, `AT_PHNUM`, `AT_ENTRY` (main binary entry), `AT_BASE` (interpreter load bias), `AT_PAGESZ` (4096), `AT_RANDOM` (16 bytes from the kernel's CSPRNG), `AT_NULL`
- [ ] The initial `rsp` points to the conventional SysV-ABI layout that musl `_dlstart` expects, in this exact order from low addresses upward: `argc: u64` → `argv[0..argc]: *const u8` → `argv_terminator: *const u8 = NULL` → `envp[0..]: *const u8` → `envp_terminator: *const u8 = NULL` → `auxv[0..]: AuxEnt { a_type: u64, a_val: u64 }` → `AT_NULL` sentinel `{ 0, 0 }` → string region holding the argv / envp / AT_RANDOM byte strings (string region must sit above the auxv so the pointers remain valid)
- [ ] Initial `rsp` is 16-byte aligned at the point control transfers to the interpreter (SysV-ABI requirement; misalignment crashes any later `xmm` save/restore the linker performs)
- [ ] A static-binary boot (no `PT_INTERP`) produces a minimal `AT_NULL`-only auxiliary vector and is unaffected

---

## Track B — `ld.so` Bring-Up

### B.1 — `ld.so` self-relocation and bootstrap

**File:** `userspace/ld-musl-x86_64.so.1/src/dynlink.rs` (or `dynlink.c` if porting musl)
**Symbol:** `_dlstart`, `_dl_start`
**Why it matters:** The dynamic linker is itself a shared object and must relocate itself before it can call any global functions.

**Acceptance:**
- [ ] `_dlstart` applies `RELATIVE` relocations to the linker's own GOT before any Rust or C global-variable access occurs
- [ ] Self-relocation is verified by a serial log line printed immediately after `_dlstart` completes
- [ ] The linker runs under QEMU without a page fault during its own startup

### B.2 — `DT_NEEDED` resolution and dependency graph

**File:** `userspace/ld-musl-x86_64.so.1/src/dynlink.rs`
**Symbol:** `load_dependency_graph`
**Why it matters:** `DT_NEEDED` entries are the mechanism by which binaries declare their shared library requirements; resolving them in topological order ensures constructors run in the correct sequence.

**Acceptance:**
- [ ] `load_dependency_graph(main_dso)` walks `PT_DYNAMIC`, finds all `DT_NEEDED` entries, and loads each from `LD_LIBRARY_PATH` then `/lib` then `/usr/lib` then `/usr/local/lib`
- [ ] The dependency graph is topologically sorted; deepest dependencies' constructors run first
- [ ] A circular dependency is detected and logged; `execve` returns `ELIBBAD`
- [ ] A missing `DT_NEEDED` library logs the name and returns `ENOENT` from `execve`

### B.3 — x86_64 relocation application

**File:** `userspace/ld-musl-x86_64.so.1/src/reloc.rs`
**Symbol:** `apply_relocations`
**Why it matters:** Relocations patch GOT and PLT entries with the resolved symbol addresses; incorrect relocation application causes silent wrong-address jumps or page faults.

**Acceptance:**
- [ ] `apply_relocations(dso)` processes `SHT_RELA` entries for `R_X86_64_GLOB_DAT`, `R_X86_64_JUMP_SLOT`, `R_X86_64_RELATIVE`, and `R_X86_64_64`
- [ ] `R_X86_64_RELATIVE` entries are applied with the load bias and require no symbol lookup
- [ ] `R_X86_64_GLOB_DAT` and `R_X86_64_JUMP_SLOT` entries look up the symbol in the global scope
- [ ] An unknown relocation type logs a warning and is skipped (not a fatal error in this phase)

### B.4 — PLT lazy-resolution trampoline

**File:** `userspace/ld-musl-x86_64.so.1/src/plt.rs`
**Symbol:** `_dl_runtime_resolve`
**Why it matters:** `RTLD_LAZY` PLT resolution is the default mode; without the trampoline, first calls to dynamically linked functions crash.

**Acceptance:**
- [ ] `_dl_runtime_resolve` is an x86_64 assembly stub that saves all caller-saved registers, calls into the linker's symbol resolution path, writes the resolved address into the GOT slot, and jumps to the function
- [ ] After the first call through a PLT entry, the GOT slot holds the resolved function address; subsequent calls skip the trampoline
- [ ] The resolved GOT address is in a `R-X`-mapped text region; the GOT slot itself is in a `RW-` region (W^X compliant)

### B.5 — `DT_INIT` and `DT_INIT_ARRAY` constructors

**File:** `userspace/ld-musl-x86_64.so.1/src/dynlink.rs`
**Symbol:** `run_constructors`
**Why it matters:** Many shared libraries (including musl itself) use constructors to initialize global state; skipping them causes `NULL` dereferences.

**Acceptance:**
- [ ] `run_constructors(dso_load_order)` calls each DSO's `DT_INIT` function pointer (if non-null) and each entry in `DT_INIT_ARRAY`
- [ ] Constructors run in dependency-first order (deepest dependency first)
- [ ] A constructor that calls `exit()` terminates the process cleanly; the linker does not re-run remaining constructors

---

## Track C — `dlopen`/`dlsym`/`dlclose`

### C.1 — `dlopen` implementation

**File:** `userspace/ld-musl-x86_64.so.1/src/dl.rs`
**Symbol:** `dlopen`
**Why it matters:** `dlopen` is the primary runtime plugin-loading mechanism; it is required for Node.js native modules (Phase 87) and for any application that loads backend implementations at runtime.

**Acceptance:**
- [ ] `dlopen(path, RTLD_LAZY)` loads and relocates the named shared object if not already loaded; returns a non-null opaque handle on success
- [ ] `dlopen(path, RTLD_NOW)` resolves all PLT entries at load time
- [ ] `dlopen(path, RTLD_GLOBAL)` adds the DSO's symbols to the global lookup scope
- [ ] A second `dlopen` of an already-loaded DSO increments the reference count and returns the same handle
- [ ] `dlopen` failure sets `dlerror()` and returns `NULL`

### C.2 — `dlsym` and `dlclose`

**File:** `userspace/ld-musl-x86_64.so.1/src/dl.rs`
**Symbol:** `dlsym`, `dlclose`
**Why it matters:** Without `dlsym`, a loaded library is inaccessible; without `dlclose`, loaded libraries accumulate and waste memory.

**Acceptance:**
- [ ] `dlsym(handle, "name")` searches the DSO's exported symbol hash table and returns the symbol's address
- [ ] `dlsym(RTLD_DEFAULT, "name")` searches the global scope (load order)
- [ ] `dlsym` returns `NULL` and sets `dlerror()` if the symbol is not found
- [ ] `dlclose(handle)` decrements the reference count; when it reaches zero, `DT_FINI`/`DT_FINI_ARRAY` run and the DSO is unmapped
- [ ] `dlclose` on a handle that was never opened returns an error via `dlerror()`

---

## Track D — Symbol Versioning (Basic)

### D.1 — `DT_GNU_HASH` lookup table

**File:** `userspace/ld-musl-x86_64.so.1/src/sym.rs`
**Symbol:** `gnu_hash_lookup`
**Why it matters:** musl-built shared objects use `DT_GNU_HASH` as the primary symbol lookup table; falling back to `DT_HASH` (older format) works but is slower and may not be present.

**Acceptance:**
- [ ] `gnu_hash_lookup(dso, name)` implements the Bloom filter + bucket + chain lookup defined by the GNU hash table format
- [ ] Falls back to `DT_HASH` if `DT_GNU_HASH` is absent in the DSO
- [ ] Returns `NULL` rather than crashing when neither hash table is present

### D.2 — `DT_VERNEED` / `DT_VERSYM` graceful handling

**File:** `userspace/ld-musl-x86_64.so.1/src/sym.rs`
**Symbol:** `resolve_versioned_symbol`
**Why it matters:** A linker that crashes on versioned symbols cannot load any glibc-built shared object; graceful fallback allows loading musl-built `.so` files which use limited versioning.

**Acceptance:**
- [ ] If `DT_VERSYM` is present, `resolve_versioned_symbol` uses it for exact version matching
- [ ] If exact version matching fails, it falls back to unversioned (baseline) symbol lookup
- [ ] A DSO with versioned symbols that the linker cannot resolve logs a warning but does not abort the load in this phase

---

## Track E — `xtask` `ld.so` and `.so` Build Pipeline

### E.1 — `build_shared_lib` helper in xtask

**File:** `xtask/src/main.rs`
**Symbol:** `build_shared_lib`
**Why it matters:** Without a build-system integration, `.so` files must be produced by hand and cannot be part of the normal `cargo xtask run` flow.

**Acceptance:**
- [ ] `build_shared_lib(name, srcs, output)` calls `tcc -shared -fPIC` (Phase 31 TCC) for C sources and `rustc --crate-type cdylib` for Rust sources
- [ ] Output `.so` files are staged to `target/generated-libs/`
- [ ] `populate_ext2_files` copies all files from `target/generated-libs/` to `/usr/lib/` on the ext2 data disk
- [ ] `cargo xtask run` builds and embeds `.so` files as part of the normal flow

### E.2 — `ld.so` placement on the data disk

**File:** `xtask/src/main.rs`
**Symbol:** `populate_ext2_files`
**Why it matters:** The kernel loads the interpreter from the VFS path recorded in `PT_INTERP`; if the path does not exist on disk, every dynamically linked binary fails with `ENOENT`.

**Acceptance:**
- [ ] `ld.so` binary is copied to `/lib/ld-musl-x86_64.so.1` on the ext2 data disk
- [ ] The directory `/lib` is created by `populate_ext2_files` if absent
- [ ] `cargo xtask clean && cargo xtask run` produces a disk that contains `/lib/ld-musl-x86_64.so.1`

### E.3 — Build `ld.so` as a position-independent ELF

**Files:**
- `userspace/ld-musl-x86_64.so.1/Cargo.toml`
- `userspace/ld-musl-x86_64.so.1/build.rs` (if linker-script wiring is needed)
- `userspace/ld-musl-x86_64.so.1/x86_64-m3os-ldso.json` (custom target spec)
- `xtask/src/main.rs`

**Symbol:** `build_ldso`
**Why it matters:** The dynamic linker is the one userspace binary that must be a `-pie` (position-independent executable) `no_std` ELF with its own `_start` (`_dlstart`); none of the existing userspace binaries are built this way, so the build pipeline must grow a new code path. Without an explicit build task, the linker never gets compiled and E.2 has nothing to stage.

**Acceptance:**
- [ ] `userspace/ld-musl-x86_64.so.1/` exists as a workspace crate with `crate-type = ["bin"]` (or `["staticlib"]` linked via a custom linker script — implementer's choice; document the chosen route in the crate's `lib.rs` / `main.rs` top-of-file comment)
- [ ] Crate is `no_std`, uses `BrkAllocator` from `syscall-lib`, and is built with `-fPIC` / `-Crelocation-model=pic` so the linker can self-relocate; output is a `-pie` ELF whose `e_type == ET_DYN`
- [ ] `xtask::build_ldso` invokes the Rust toolchain (via the existing `build_userspace` plumbing) with the linker's target spec and stages the resulting ELF to `target/generated-libs/ld-musl-x86_64.so.1`
- [ ] The crate is added to `Cargo.toml` `members` and to the `bins` array in `xtask/src/main.rs` (`build_userspace`); `needs_alloc = true`
- [ ] `readelf -h target/generated-libs/ld-musl-x86_64.so.1` reports `Type: DYN (Shared object file)` (i.e., `ET_DYN`, not `ET_EXEC`)
- [ ] `cargo xtask check` and `cargo xtask run` both succeed with the new crate in the workspace

---

## Track F — Test Applications

### F.1 — `libhello.so` and `dynlink_hello` test binary

**Files:**
- `userspace/tests/libhello/src/lib.rs`
- `userspace/tests/dynlink_hello/src/main.rs`

**Symbol:** `hello_str` (export), `main`
**Why it matters:** This is the minimal end-to-end proof that dynamic linking works: a binary with `DT_NEEDED` finds and calls a function from a shared library.

**Acceptance:**
- [ ] `libhello.so` exports `hello_str() -> *const u8` returning a null-terminated UTF-8 string
- [ ] `dynlink_hello` is built with `DT_NEEDED = libhello.so` (via `tcc` or `rustc --extern`)
- [ ] Running `dynlink_hello` under QEMU prints the expected string to stdout
- [ ] The binary is registered as a QEMU test under `cargo xtask test --test dynlink_hello`

### F.2 — `dlopen_test` binary

**Files:**
- `userspace/tests/dlopen_test/src/main.rs`

**Symbol:** `main`
**Why it matters:** `dlopen`/`dlsym` is the API that Node.js native modules and plugin architectures depend on; it must be validated independently of link-time `DT_NEEDED`.

**Acceptance:**
- [ ] `dlopen_test` calls `dlopen("/usr/lib/libhello.so", RTLD_LAZY)` and asserts a non-null handle
- [ ] It calls `dlsym(handle, "hello_str")` and asserts a non-null function pointer
- [ ] It calls the function pointer and asserts the returned string equals the expected value
- [ ] It calls `dlclose(handle)` and asserts success (return 0)
- [ ] The binary is registered as a QEMU test under `cargo xtask test --test dlopen_test`

---

## Track G — Design Doc Updates

### G.1 — Update Phase 11 design doc

**File:** `docs/roadmap/11-process-model.md`
**Symbol:** N/A
**Why it matters:** Phase 11's doc defers dynamic linking; that deferral must be closed with a Phase 76 reference.

**Acceptance:**
- [ ] Phase 11 "Deferred Until Later" entry for `PT_INTERP` / dynamic linking updated to "Delivered in Phase 76"
- [ ] Phase 11 "Feature Scope" or "How This Builds on Earlier Phases" notes that the ELF loader is extended in Phase 76

### G.2 — Update Phase 12 design doc

**File:** `docs/roadmap/12-posix-compat.md`
**Symbol:** N/A
**Why it matters:** Phase 12 includes `dlopen`/`dlsym` in the POSIX compatibility surface; the implementation landing here should be documented.

**Acceptance:**
- [ ] Phase 12 "Deferred Until Later" entry for `dlopen`/`dlsym` updated to "Delivered in Phase 76"
- [ ] No entry in Phase 12 claims `dlopen` is "not yet implemented"

---

## Track H — Documentation and Release

### H.1 — Create the aligned legacy learning doc

**File:** `docs/76-dynamic-linker.md`
**Symbol:** N/A
**Why it matters:** Dynamic linking is architecturally new ground for m3OS; a learner-friendly doc that walks through `PT_INTERP`, auxiliary vector, `ld.so` bring-up, GOT/PLT, and `dlopen`/`dlsym` in one place prevents readers from having to reconstruct the full picture from ELF spec sections and musl source comments.

**Acceptance:**
- [ ] File exists at `docs/76-dynamic-linker.md`
- [ ] All required template fields populated: `**Aligned Roadmap Phase:** Phase 76`, `**Status:** Planned`, `**Source Ref:** phase-76`, `**Supersedes Legacy Doc:** new`
- [ ] Overview is learner-friendly (explains why dynamic linking exists and what `PT_INTERP` means before describing implementation details)
- [ ] Key Files table cites real files this phase touches: `kernel/src/elf/loader.rs`, `userspace/ld-musl-x86_64.so.1/src/dynlink.rs`, `userspace/ld-musl-x86_64.so.1/src/reloc.rs`, `userspace/ld-musl-x86_64.so.1/src/dl.rs`, `xtask/src/main.rs`, `userspace/tests/dynlink_hello/src/main.rs`, `userspace/tests/dlopen_test/src/main.rs`
- [ ] Related Roadmap Docs links `docs/roadmap/76-dynamic-linker.md` and `docs/roadmap/tasks/76-dynamic-linker-tasks.md`

### H.2 — Bump kernel version to 0.76.0

**Files:**
- `kernel/Cargo.toml`
- `Cargo.lock`
- `AGENTS.md`
- `docs/roadmap/README.md`

**Symbol:** `version` in `kernel/Cargo.toml` `[package]`
**Why it matters:** Project convention is one minor-version bump per shipped phase; the 2026-05-08 audit found `AGENTS.md` stale and discipline in version tracking signals a complete, shippable phase.

**Acceptance:**
- [ ] `kernel/Cargo.toml` `version = "0.76.0"`
- [ ] `Cargo.lock` regenerated (run `cargo check` or `cargo xtask check` to trigger it)
- [ ] `AGENTS.md` "Kernel v0.76.0" updated
- [ ] `docs/roadmap/README.md` Phase 76 row Status updated to "Complete" at merge time
- [ ] `cargo xtask check` passes
- [ ] Git tag `v0.76.0` recommended at phase merge

---

## Documentation Notes

- Track A's auxiliary vector layout must exactly match what musl `_dlstart` expects at process start; the musl source's `arch/x86_64/crt_arch.h` and `ldso/dynlink.c:_dlstart` are the authoritative references.
- Track B.1's self-relocation must complete before any call to a Rust `#[no_mangle]` function in the linker itself; this is the hardest bootstrap correctness constraint.
- Track C's `dlerror()` storage: TLS is explicitly deferred in this phase (see design doc Deferred Until Later). Until per-thread TLS lands, `dlerror()` stores its message in a single process-global `static mut` (or `spin::Mutex<Option<&'static str>>`) string slot — m3OS userspace is effectively single-threaded per process at phase entry, so a process-global slot is correct. Track 76+ TLS work will replace this with `__thread`-qualified storage.
- Track B.4's PLT trampoline is x86_64 assembly; it must preserve all caller-saved registers (`rax`, `rcx`, `rdx`, `rsi`, `rdi`, `r8`, `r9`, `r10`, `r11`, `xmm0–xmm7`) before calling the Rust symbol-resolution code.
- Track E.2 must run `cargo xtask clean` in the PR CI step to ensure the disk is recreated with `/lib/ld-musl-x86_64.so.1`; otherwise the disk from a previous build is reused and the test binary cannot find the interpreter.
- Track F's two test binaries are the acceptance gate; they replace any manual QEMU session as the reproducible proof of dynamic linking correctness.
- Track H.1 learning doc should be authored after Track F so it can cite the actual test binary serial output as concrete examples of a successful dynamic link.
