# Phase 76c — Dynamic Linker: `dlopen` / `dlsym` / `dlclose`: Task List

**Status:** Planned
**Source Ref:** phase-76c
**Depends on:** Phase 76 ✅, Phase 76b
**Goal:** Ship a libdl-compatible `dlopen` / `dlsym` / `dlclose` / `dlerror` on top of the Phase 76b dependency-graph + relocation machinery, with `DT_FINI` / `DT_FINI_ARRAY` running on last-close.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| C1 | `dlopen(path, flags)` with `RTLD_LAZY` / `RTLD_NOW` / `RTLD_GLOBAL` / `RTLD_LOCAL` | Phase 76b | Planned |
| C2 | `dlsym(handle, name)` + `dlclose(handle)` with `DT_FINI` / `DT_FINI_ARRAY` | C1 | Planned |
| C3 | `dlerror()` process-global slot | C1 | Planned |
| F2 | `dlopen_test` binary + xtask gate | C2, C3 | Planned |
| H | Phase 12 doc closure + `docs/76-dynamic-linker.md` update + version bump | F2 | Planned |

---

## Track C1 — `dlopen`

### C1.1 — `DlState` + handle slab

**File:** `userspace/ld-musl-x86_64.so.1/src/handle.rs`
**Symbol:** `HandleTable`
**Why it matters:** `dlopen` returns an opaque handle that `dlclose` must validate; without a slab + generation counter, forged or already-freed handles produce undefined behavior on `dlclose`.

**Acceptance:**
- [ ] `HandleTable::insert(dso_id) -> *mut c_void` returns an opaque pointer to a `Handle { dso_id, generation }` record.
- [ ] `HandleTable::resolve(handle: *mut c_void) -> Option<DsoId>` returns `None` for forged handles, already-freed handles, or handles whose generation does not match the live DSO's generation.
- [ ] Unit-tested under `#[cfg(test)]` with insert / resolve / remove / re-insert-bumps-generation fixtures.

### C1.2 — `dlopen` entry, path resolution, flag parsing

**File:** `userspace/ld-musl-x86_64.so.1/src/dl.rs`
**Symbol:** `dlopen`
**Why it matters:** This is the libdl entry point; flag parsing and search-path resolution must match the POSIX contract so existing libdl-using code works without modification.

**Acceptance:**
- [ ] `extern "C" fn dlopen(path: *const c_char, flags: c_int) -> *mut c_void`.
- [ ] `path = NULL` returns a handle to the main binary.
- [ ] Path with no `/` searches the standard load paths (matches 76b's `LD_LIBRARY_PATH` / `/lib` / `/usr/lib` / `/usr/local/lib` order).
- [ ] Path with `/` is treated as absolute (or relative-to-CWD; POSIX allows either — m3OS chooses absolute).
- [ ] `RTLD_NOW` triggers `apply_jmprel_table` at open time; `RTLD_LAZY` is accepted but treated as `RTLD_NOW` in 76c (PLT lazy resolve is 76d).
- [ ] `RTLD_GLOBAL` inserts the DSO into the process-global scope; `RTLD_LOCAL` (default) does not.

### C1.3 — Refcount-increment for repeat opens

**File:** `userspace/ld-musl-x86_64.so.1/src/dl.rs`
**Symbol:** `dlopen` (refcount path)
**Why it matters:** POSIX requires that repeat opens of the same `SONAME` return the same handle with an incremented refcount; without this, every plugin host that calls `dlopen` twice leaks memory.

**Acceptance:**
- [ ] Repeat `dlopen` of the same resolved `SONAME` increments the existing handle's refcount and returns a fresh handle pointer that resolves to the same `DsoId`.
- [ ] A new `dlopen` after every prior handle was `dlclose`d still re-maps the library (refcount semantics are per-DSO, not per-handle).

---

## Track C2 — `dlsym` + `dlclose`

### C2.1 — `dlsym(handle, name)` symbol lookup

**File:** `userspace/ld-musl-x86_64.so.1/src/dl.rs`
**Symbol:** `dlsym`
**Why it matters:** Every libdl-using consumer goes through `dlsym` to actually call into the loaded library; missing or wrong lookups break every consumer.

**Acceptance:**
- [ ] `extern "C" fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void`.
- [ ] Real-handle path: search the handle's DSO and its dependency chain via `DT_HASH`.
- [ ] `RTLD_DEFAULT` (`handle == NULL`): search the process-global scope.
- [ ] Not-found returns `NULL` and populates `dlerror()` with `"undefined symbol: <name>"`.

### C2.2 — `dlclose` refcount + destructor pipeline

**File:** `userspace/ld-musl-x86_64.so.1/src/dl.rs`
**Symbol:** `dlclose`
**Why it matters:** `dlclose` must run destructors before unmapping; the order (`DT_FINI_ARRAY` reverse then `DT_FINI`) is contractually fixed and ABI-visible.

**Acceptance:**
- [ ] `extern "C" fn dlclose(handle: *mut c_void) -> c_int`.
- [ ] Decrements the DSO's refcount; when refcount reaches zero, runs `DT_FINI_ARRAY` in reverse-array order then `DT_FINI` (if present), then removes the DSO from the global scope, then unmaps `PT_LOAD` segments.
- [ ] Forged or already-freed handle returns `-1` and populates `dlerror()`.
- [ ] Destructor invocation uses a register-loaded function pointer (not a GOT slot).

### C2.3 — DSO unmap path

**File:** `userspace/ld-musl-x86_64.so.1/src/dynlink.rs`
**Symbol:** `unmap_dso`
**Why it matters:** Without unmapping, refcounted close still leaks address space; the unmap must walk every `PT_LOAD` segment of the DSO and `munmap` each in turn.

**Acceptance:**
- [ ] `unmap_dso(dso: &LoadedDso) -> Result<(), DlError>` walks every `PT_LOAD` segment and calls `munmap` for the mapped range.
- [ ] After return, the DSO record is removed from the linker's load list and its handle generation is invalidated.

---

## Track C3 — `dlerror`

### C3.1 — Process-global error slot

**File:** `userspace/ld-musl-x86_64.so.1/src/dl.rs`
**Symbol:** `DlError` + `dlerror`
**Why it matters:** The libdl contract requires that error messages survive across libdl calls but are cleared by `dlerror()` itself; getting the read-and-clear ordering wrong breaks error-checking idioms.

**Acceptance:**
- [ ] `DlError` is a `Mutex<Option<&'static str>>` (or equivalent) accessed under the same `DlState` lock as the handle table.
- [ ] `dlerror()` reads the current message, clears the slot, returns the message (or `NULL` if there was none).
- [ ] Documented as not-yet-thread-safe in `docs/76-dynamic-linker.md` (the thread-local upgrade is gated on TLS).

---

## Track F2 — `dlopen_test` Demo

### F2.1 — `dlopen_test` C binary

**File:** `userspace/dlopen_test/dlopen_test.c`
**Symbol:** `main`
**Why it matters:** A real C consumer is the only way to validate that the libdl ABI works against existing libdl-using code shapes; a Rust-only test would mask C-ABI bugs.

**Acceptance:**
- [ ] Calls `dlopen("/usr/lib/libhello.so", RTLD_NOW)`; asserts non-NULL.
- [ ] Calls `dlsym(handle, "hello_str")`; asserts non-NULL.
- [ ] Calls the function through the resolved pointer; asserts the returned string equals `"HELLO_FROM_SHARED_LIB:OK"`.
- [ ] Calls `dlclose(handle)`; asserts return value is 0.
- [ ] Exercises the four negative paths: missing library, missing symbol, double-close, close-of-never-opened-handle. Asserts each populates `dlerror()` appropriately.

### F2.2 — `cargo xtask dlopen-test-smoke` gate

**File:** `xtask/src/main.rs`
**Symbol:** `dlopen_test_smoke`
**Why it matters:** Without the gate, the demo regresses silently the moment any of C1/C2/C3 is broken.

**Acceptance:**
- [ ] Subcommand boots QEMU, execs `/bin/dlopen_test`, asserts the test prints `DLOPEN_TEST:PASS` on serial.
- [ ] Smoke-runner emits `SMOKE:dlopen-test-smoke:PASS` / `:FAIL` and is wired into the standard `cargo xtask smoke-test` step list.

### F2.3 — Destructor-runs assertion in the gate

**File:** `userspace/dlopen_test/dlopen_test.c` (extended)
**Symbol:** `main` (extended)
**Why it matters:** `DT_FINI_ARRAY` is the most subtle part of `dlclose` and the easiest to silently skip; the gate must explicitly verify destructor invocation.

**Acceptance:**
- [ ] A second test library `libhello_fini.so` includes a `__attribute__((destructor))` function that prints `LIBHELLO_FINI:RAN` to stdout.
- [ ] `dlopen_test` opens, then closes `libhello_fini.so` and asserts the sentinel appears on serial between `dlopen_test` lifecycle messages.

---

## Track H — Documentation + Version Bump

### H.1 — Bump kernel version to `0.76.2`

**Files:**
- `kernel/Cargo.toml`
- `Cargo.lock` (regenerated)

**Symbol:** `package.version`
**Why it matters:** Phase 76c is the second 76 sub-phase; the patch bump keeps the running banner accurate.

**Acceptance:**
- [ ] `kernel/Cargo.toml` `version = "0.76.2"`.
- [ ] `Cargo.lock` regenerated and checked in.
- [ ] Boot banner prints `m3OS 0.76.2`.

### H.2 — Extend `docs/76-dynamic-linker.md` with the libdl sections

**File:** `docs/76-dynamic-linker.md`
**Symbol:** N/A (existing learning doc, extended)
**Why it matters:** Phase 76's learning doc must grow to cover the runtime plugin API once it ships, or it becomes misleading.

**Acceptance:**
- [ ] New "What changes in 76c" section describes `dlopen` / `dlsym` / `dlclose` / `dlerror` and the destructor pipeline.
- [ ] Key Files table extended with `userspace/ld-musl-x86_64.so.1/src/dl.rs`, `handle.rs`, `userspace/dlopen_test/`.
- [ ] Subphase table at the top of the doc updates 76c's row to reflect the gate is now wired.
- [ ] "Deferred Until Later" section in the learning doc clarifies that the `dlerror` slot is process-global until TLS lands.

### H.3 — Phase 12 doc closure: remove `dlopen` not-yet-implemented entry

**File:** `docs/12-posix-compatibility-layer.md`
**Symbol:** `dlopen` entry (whichever section lists POSIX gaps)
**Why it matters:** `docs/12-posix-compatibility-layer.md` currently lists `dlopen` / `dlsym` / `dlclose` as unimplemented; without an update, the POSIX-compat tracker contradicts the shipping behavior.

**Acceptance:**
- [ ] `dlopen` / `dlsym` / `dlclose` / `dlerror` are documented as implemented in 76c with a link to `docs/76-dynamic-linker.md`.
- [ ] Any "Deferred Until Later" entry in Phase 12 that pointed at `dlopen` is updated to point at the 76c row.

### H.4 — Update roadmap README row for Phase 76c

**File:** `docs/roadmap/README.md`
**Symbol:** Phase 76c table row
**Why it matters:** The roadmap README is the canonical phase index; missing or stale rows mean readers cannot navigate to the phase docs.

**Acceptance:**
- [ ] New row: `| 76c | Dynamic Linker: dlopen | dlopen / dlsym / dlclose / dlerror with DT_FINI_ARRAY destructors | Complete | phase-76c | [Phase 76c](./76c-dlopen.md) | [Tasks](./tasks/76c-dlopen-tasks.md) |`.
- [ ] Phase 76 and 76b rows remain `Complete` and are unaffected.

### H.5 — Update `AGENTS.md` project-overview paragraph

**File:** `AGENTS.md`
**Symbol:** Phase 76 paragraph (extended with Phase 76c clause)
**Why it matters:** The project-overview paragraph is the single most-read summary of the current state of m3OS.

**Acceptance:**
- [ ] Phase 76c clause added describing: `dlopen` / `dlsym` / `dlclose` / `dlerror`, refcounted handle table, `DT_FINI_ARRAY` + `DT_FINI` destructors on last-close, `dlopen_test` gate, kernel version `0.76.2`.
- [ ] Phase 76 paragraph's "Deferred Until Later" closure references are updated to remove 76c from the deferred list.

---

## Documentation Notes

- The original (pre-split) Phase 76 task list's C.1 / C.2 / F.2 acceptance items migrate here verbatim, restructured to match the per-track template.
- `dlerror()` storage uses a process-global `static mut` slot (wrapped in `Mutex`) until TLS lands; the deferred-TLS rationale is recorded in `docs/76-dynamic-linker.md`.
- 76c uses `DT_HASH` only — 76d switches `dlsym` to prefer `DT_GNU_HASH`. Libraries opened via `dlopen` in 76c must therefore be built with `--hash-style=sysv` (matching the 76b convention).
- 76c kernel version is `0.76.2` (patch); 76d will continue the patch sequence with `0.76.3`.
- The 76c learning content is added to the existing `docs/76-dynamic-linker.md` (created in Phase 76 and extended by 76b); no new learning doc is created.
