# Phase 76c — Dynamic Linker: `dlopen` / `dlsym` / `dlclose`: Task List

**Status:** Complete
**Source Ref:** phase-76c
**Depends on:** Phase 76 ✅, Phase 76b ✅
**Goal:** Ship a libdl-compatible `dlopen` / `dlsym` / `dlclose` / `dlerror` on top of the Phase 76b dependency-graph + relocation machinery, with `DT_FINI` / `DT_FINI_ARRAY` running on last-close.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| C1 | `dlopen(path, flags)` with `RTLD_LAZY` / `RTLD_NOW` / `RTLD_GLOBAL` / `RTLD_LOCAL` | Phase 76b | **Complete** |
| C2 | `dlsym(handle, name)` + `dlclose(handle)` with `DT_FINI` / `DT_FINI_ARRAY` | C1 | **Complete** |
| C3 | `dlerror()` process-global slot | C1 | **Complete** |
| F2 | `dlopen_test` binary + xtask gate | C2, C3 | **Complete** |
| H | Phase 12 doc closure + `docs/76-dynamic-linker.md` update + version bump | F2 | **Complete** |

---

## Track C1 — `dlopen`

### C1.1 — `DlState` + handle slab

**File:** `userspace/ld-musl-x86_64.so.1/src/handle.rs`
**Symbol:** `HandleTable`
**Why it matters:** `dlopen` returns an opaque handle that `dlclose` must validate; without a slab + generation counter, forged or already-freed handles produce undefined behavior on `dlclose`.

**Acceptance:**
- [x] `HandleTable::insert(dso_id) -> *mut c_void` returns an opaque pointer to a `Handle { dso_id, generation }` record.
- [x] `HandleTable::resolve(handle: *mut c_void) -> Result<DsoId, HandleError>` returns `Err` for forged handles, already-freed handles, or handles whose generation does not match the live DSO's generation.
- [x] Unit-tested under `#[cfg(test)]` with insert / resolve / remove / re-insert-bumps-generation fixtures.

### C1.2 — `dlopen` entry, path resolution, flag parsing

**File:** `userspace/ld-musl-x86_64.so.1/src/dl.rs`
**Symbol:** `dlopen`
**Why it matters:** This is the libdl entry point; flag parsing and search-path resolution must match the POSIX contract so existing libdl-using code works without modification.

**Acceptance:**
- [x] `extern "C" fn dlopen(path: *const c_char, flags: c_int) -> *mut c_void`.
- [x] `path = NULL` returns a handle to the main binary.
- [x] Bare-name path (no leading `/`) is resolved under `/usr/lib/` only in Phase 76c. The full `LD_LIBRARY_PATH` / `/lib` / `/usr/lib` / `/usr/local/lib` search chain is deferred to a follow-up phase; non-absolute inputs that include `/` (e.g. `./libx.so`) are still treated as bare basenames and prefixed with `/usr/lib/`.
- [x] Path whose first byte is `/` is treated as absolute and used as-is.
- [x] `RTLD_NOW` triggers `apply_jmprel_table` at open time; `RTLD_LAZY` is accepted but treated as `RTLD_NOW` in 76c (PLT lazy resolve is 76d).
- [x] `RTLD_GLOBAL` inserts the DSO into the process-global scope; `RTLD_LOCAL` (default) does not.

### C1.3 — Refcount-increment for repeat opens

**File:** `userspace/ld-musl-x86_64.so.1/src/dl.rs`
**Symbol:** `dlopen` (refcount path)
**Why it matters:** POSIX requires that repeat opens of the same `SONAME` return the same handle with an incremented refcount; without this, every plugin host that calls `dlopen` twice leaks memory.

**Acceptance:**
- [x] Repeat `dlopen` of the same resolved `SONAME` increments the existing handle's refcount and returns a fresh handle pointer that resolves to the same `DsoId`.
- [x] A new `dlopen` after every prior handle was `dlclose`d still re-maps the library (refcount semantics are per-DSO, not per-handle).

---

## Track C2 — `dlsym` + `dlclose`

### C2.1 — `dlsym(handle, name)` symbol lookup

**File:** `userspace/ld-musl-x86_64.so.1/src/dl.rs`
**Symbol:** `dlsym`
**Why it matters:** Every libdl-using consumer goes through `dlsym` to actually call into the loaded library; missing or wrong lookups break every consumer.

**Acceptance:**
- [x] `extern "C" fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void`.
- [x] Real-handle path: search the handle's DSO and its dependency chain via `DT_HASH`.
- [x] `RTLD_DEFAULT` (`handle == NULL`): search the process-global scope.
- [x] Not-found returns `NULL` and populates `dlerror()` with `"undefined symbol: <name>"`.

### C2.2 — `dlclose` refcount + destructor pipeline

**File:** `userspace/ld-musl-x86_64.so.1/src/dl.rs`
**Symbol:** `dlclose`
**Why it matters:** `dlclose` must run destructors before unmapping; the order (`DT_FINI_ARRAY` reverse then `DT_FINI`) is contractually fixed and ABI-visible. The destructor must be invoked via a register-loaded function pointer (not a GOT slot) because the DSO's GOT is about to be unmapped — a GOT-routed indirect call would page-fault on the very next instruction after the unmap.

**Acceptance:**
- [x] `extern "C" fn dlclose(handle: *mut c_void) -> c_int`.
- [x] Decrements the DSO's refcount; when refcount reaches zero, runs `DT_FINI_ARRAY` in reverse-array order then `DT_FINI` (if present), then removes the DSO from the global scope, then unmaps the DSO's image via `unmap_dso` (C2.3).
- [x] Forged or already-freed handle returns `-1` and populates `dlerror()`.
- [x] Destructor invocation uses a register-loaded function pointer (not a GOT slot) — rationale above.

### C2.3 — DSO unmap path

**File:** `userspace/ld-musl-x86_64.so.1/src/dynlink.rs`
**Symbol:** `unmap_dso`
**Why it matters:** Without unmapping, refcounted close still leaks address space. Phase 76b's `load_dso` issues a single anonymous `mmap` covering the whole image extent (the kernel ignores `MAP_FIXED`, so the kernel-chosen base becomes `load_bias`) and then copies each `PT_LOAD` in and `mprotect`s the executable segments — the matching unmap is therefore one `munmap` of the same whole-image range, not a per-`PT_LOAD` walk (the inter-segment gaps belong to the same allocation).

**Acceptance:**
- [x] `LoadedDso` carries the `(load_bias, image_len)` pair captured at load time (image_len = `p_vaddr + p_memsz` of the highest `PT_LOAD`, page-aligned up).
- [x] `unmap_dso(dso: &LoadedDso) -> Result<(), DlError>` issues a single `munmap(load_bias, image_len)` matching the 76b whole-image mmap shape.
- [x] After return, the DSO record is removed from the linker's load list and its handle generation is invalidated so subsequent `dlsym`/`dlclose` against a stale handle returns the forged-handle error.

---

## Track C3 — `dlerror`

### C3.1 — Process-global error slot

**File:** `userspace/ld-musl-x86_64.so.1/src/dl.rs`
**Symbol:** `DlError` + `dlerror`
**Why it matters:** The libdl contract requires that error messages survive across libdl calls but are cleared by `dlerror()` itself; getting the read-and-clear ordering wrong breaks error-checking idioms.

**Acceptance:**
- [x] Error state lives on `DlState` itself: an `error: Option<&'static [u8]>` slot for the static message bank (`ERR_LIBRARY_NOT_FOUND`, …) plus a `error_buf: [u8; MAX_ERR_LEN]` formatting buffer for the per-symbol `"undefined symbol: <name>"` shape. `DlState` is held in a process-global `UnsafeCell` (no `Mutex`) — Phase 76c is single-threaded and a `Mutex` upgrade is gated on TLS.
- [x] `dlerror()` reads the current message, clears the slot, returns the message (or `NULL` if there was none).
- [x] Documented as not-yet-thread-safe in `docs/76-dynamic-linker.md` (the thread-local upgrade is gated on TLS).

---

## Track F2 — `dlopen_test` Demo

### F2.1 — `dlopen_test` C binary

**File:** `userspace/dlopen_test/dlopen_test.c`
**Symbol:** `main`
**Why it matters:** A real C consumer is the only way to validate that the libdl ABI works against existing libdl-using code shapes; a Rust-only test would mask C-ABI bugs. Per AGENTS.md the binary also needs the full four-place wiring (xtask build + ramdisk embedding) or `execve` returns ENOENT at smoke time.

**Acceptance:**
- [x] Source at `userspace/dlopen_test/dlopen_test.c`; built as a musl dynamic ELF with `PT_INTERP=/lib/ld-musl-x86_64.so.1` (mirrors the 76b `dynlink_hello` shape) — no Rust workspace member is added.
- [x] xtask build pipeline (`xtask/src/main.rs`) invokes musl-gcc for the binary and stages it to `target/generated-initrd/dlopen_test`; `populate_ext2_files` writes it to `/bin/dlopen_test` on the data disk.
- [x] `kernel/src/fs/ramdisk.rs` `BIN_ENTRIES` gains a `dlopen_test` row with the matching `include_bytes!` static so the binary is available before ext2 mount.
- [x] Calls `dlopen("/usr/lib/libhello.so", RTLD_NOW)`; asserts non-NULL.
- [x] Calls `dlsym(handle, "hello_str")`; asserts non-NULL.
- [x] Calls the function through the resolved pointer; asserts the returned string equals `"HELLO_FROM_SHARED_LIB:OK"`.
- [x] Calls `dlclose(handle)`; asserts return value is 0.
- [x] Exercises the four negative paths: missing library, missing symbol, double-close, close-of-never-opened-handle. Asserts each populates `dlerror()` appropriately.
- [x] Prints `DLOPEN_TEST:PASS` on serial after all positive and negative cases pass.

### F2.2 — `cargo xtask dlopen-test-smoke` gate

**File:** `xtask/src/main.rs`
**Symbol:** `dlopen_test_smoke`
**Why it matters:** Without the gate, the demo regresses silently the moment any of C1/C2/C3 is broken.

**Acceptance:**
- [x] Subcommand boots QEMU, execs `/bin/dlopen_test`, asserts the test prints `DLOPEN_TEST:PASS` on serial.
- [x] Smoke-runner emits `SMOKE:dlopen-test-smoke:PASS` / `:FAIL` and is wired into the standard `cargo xtask smoke-test` step list.

### F2.3 — Destructor-runs assertion in the gate

**Files:**
- `userspace/lib/libhello_fini/hello_fini.h`
- `userspace/lib/libhello_fini/hello_fini.c`
- `userspace/dlopen_test/dlopen_test.c` (extended)

**Symbol:** `__hello_fini_dtor` (the `__attribute__((destructor))` function) + `main` (extended)
**Why it matters:** `DT_FINI_ARRAY` is the most subtle part of `dlclose` and the easiest to silently skip; the gate must explicitly verify destructor invocation with a serial ordering pin that cannot be satisfied by a no-op.

**Acceptance:**
- [x] `userspace/lib/libhello_fini/hello_fini.{h,c}` source mirrors the 76b `libhello` shape (`DT_SONAME=libhello_fini.so`, built with `--hash-style=sysv` per Documentation Notes); a `__attribute__((destructor))` function writes `LIBHELLO_FINI:RAN\n` directly to **stdout (fd 1) via `write(2)`** (not stderr; not `printf` — avoids dependence on stdio flush on DSO unmap). Writing to fd 1 keeps the destructor sentinel in the same capture stream as the bracket sentinels because m3OS's `dup2` does not share the file description between fd 1 and fd 2.
- [x] xtask wiring calls `build_shared_lib("libhello_fini", &["userspace/lib/libhello_fini/hello_fini.c"], "target/generated-libs/libhello_fini.so")`; `populate_ext2_files` writes it to `/usr/lib/libhello_fini.so`; kernel ramdisk `USR_LIB_ENTRIES` gains the matching row.
- [x] `dlopen_test` prints `DLOPEN_TEST:FINI_PENDING` *before* its `dlclose(libhello_fini)` call and `DLOPEN_TEST:PASS` *after* every assertion.
- [x] Smoke-runner asserts the serial substring order `DLOPEN_TEST:FINI_PENDING` → `LIBHELLO_FINI:RAN` → `DLOPEN_TEST:PASS` strictly (a missing `LIBHELLO_FINI:RAN` between the two bracket sentinels is a `:FAIL`).

---

## Track H — Documentation + Version Bump

### H.1 — Bump kernel version to `0.76.2`

**Files:**
- `kernel/Cargo.toml`
- `Cargo.lock` (regenerated)

**Symbol:** `package.version`
**Why it matters:** Phase 76c is the second 76 sub-phase; the patch bump keeps the running banner accurate.

**Acceptance:**
- [x] `kernel/Cargo.toml` `version = "0.76.2"`.
- [x] `Cargo.lock` regenerated and checked in.
- [x] Boot banner prints `m3OS 0.76.2`.

### H.2 — Extend `docs/76-dynamic-linker.md` with the libdl sections

**File:** `docs/76-dynamic-linker.md`
**Symbol:** N/A (existing learning doc, extended)
**Why it matters:** Phase 76's learning doc must grow to cover the runtime plugin API once it ships, or it becomes misleading.

**Acceptance:**
- [x] New "What changes in 76c" section describes `dlopen` / `dlsym` / `dlclose` / `dlerror` and the destructor pipeline.
- [x] Key Files table extended with `userspace/ld-musl-x86_64.so.1/src/dl.rs`, `handle.rs`, `userspace/dlopen_test/`.
- [x] Subphase table at the top of the doc updates 76c's row to reflect the gate is now wired.
- [x] "Deferred Until Later" section in the learning doc clarifies that the `dlerror` slot is process-global until TLS lands.

### H.3 — Phase 12 doc closure: remove `dlopen` not-yet-implemented entry

**File:** `docs/12-posix-compatibility-layer.md`
**Symbol:** `dlopen` entry (whichever section lists POSIX gaps)
**Why it matters:** `docs/12-posix-compatibility-layer.md` currently lists `dlopen` / `dlsym` / `dlclose` as unimplemented; without an update, the POSIX-compat tracker contradicts the shipping behavior.

**Acceptance:**
- [x] `dlopen` / `dlsym` / `dlclose` / `dlerror` are documented as implemented in 76c with a link to `docs/76-dynamic-linker.md`.
- [x] Any "Deferred Until Later" entry in Phase 12 that pointed at `dlopen` is updated to point at the 76c row.

### H.4 — Update roadmap README row for Phase 76c

**File:** `docs/roadmap/README.md`
**Symbol:** Phase 76c table row
**Why it matters:** The roadmap README is the canonical phase index; missing or stale rows mean readers cannot navigate to the phase docs.

**Acceptance:**
- [x] New row: `| 76c | Dynamic Linker: dlopen | dlopen / dlsym / dlclose / dlerror with DT_FINI_ARRAY destructors | Complete | phase-76c | [Phase 76c](./76c-dlopen.md) | [Tasks](./tasks/76c-dlopen-tasks.md) |`.
- [x] Phase 76 and 76b rows remain `Complete` and are unaffected.

### H.5 — Update `AGENTS.md` project-overview paragraph

**File:** `AGENTS.md`
**Symbol:** Phase 76 paragraph (extended with Phase 76c clause)
**Why it matters:** The project-overview paragraph is the single most-read summary of the current state of m3OS.

**Acceptance:**
- [x] Phase 76c clause added describing: `dlopen` / `dlsym` / `dlclose` / `dlerror`, refcounted handle table, `DT_FINI_ARRAY` + `DT_FINI` destructors on last-close, `dlopen_test` gate, kernel version `0.76.2`.
- [x] Phase 76 paragraph's "Deferred Until Later" closure references are updated to remove 76c from the deferred list.

---

## Documentation Notes

- The original (pre-split) Phase 76 task list's C.1 / C.2 / F.2 acceptance items migrate here verbatim, restructured to match the per-track template.
- `dlerror()` storage uses a process-global `UnsafeCell<DlState>` slot (no `Mutex` — Phase 76c is single-threaded) until TLS lands; the deferred-TLS rationale is recorded in `docs/76-dynamic-linker.md`.
- 76c uses `DT_HASH` only — 76d switches `dlsym` to prefer `DT_GNU_HASH`. Libraries opened via `dlopen` in 76c must therefore be built with `--hash-style=sysv` (matching the 76b convention).
- 76c kernel version is `0.76.2` (patch); 76d will continue the patch sequence with `0.76.3`.
- The 76c learning content is added to the existing `docs/76-dynamic-linker.md` (created in Phase 76 and extended by 76b); no new learning doc is created.
- 76c does not implement `RTLD_NEXT` (search starting from the DSO *after* the caller's). The lookup-chain plumbing is a non-trivial extension of the global-scope walker and is deferred beyond Phase 76d; see `76c-dlopen.md` "Deferred Until Later" for the rationale.
- The C2.3 unmap path is a *single* `munmap(load_bias, image_len)` — not a per-`PT_LOAD` walk — because Phase 76b's `load_dso` issues one anonymous mmap for the whole image (the kernel ignores `MAP_FIXED`, so the kernel-chosen base becomes `load_bias` and the inter-segment gaps belong to that single allocation). The pair `(load_bias, image_len)` must be captured into `LoadedDso` at load time for unmap to recover it.
- `libhello_fini.so` follows the 76b `libhello` source-and-build convention (source under `userspace/lib/<libname>/`, built via `xtask::build_shared_lib`, staged to `/usr/lib/<basename>`, ramdisk row in `USR_LIB_ENTRIES`) so the 76c destructor demo composes with the existing 76b shared-library plumbing without introducing a new build path.
