# Dynamic Linker — Scaffolding (Phase 76) → Bring-up (76b) → libdl (76c) → Polish (76d)

**Aligned Roadmap Phase:** Phase 76 / 76b / 76c / 76d
**Status:** Complete (Phase 76 family shipped through 76d — PLT lazy resolve, GNU hash, symbol versioning, `LD_BIND_NOW`, end-to-end gates for all)
**Source Ref:** phase-76 / phase-76b / phase-76c / phase-76d
**Supersedes Legacy Doc:** new

## Overview

A *dynamic linker* (sometimes called the *runtime linker*, or
**`ld.so`** for short) is the userspace program that loads shared
libraries into a process at runtime. When a Linux binary is built
against `libc.so`, the linker is what resolves `printf` to a real
address before `main` runs. On every Unix-derived system since
Solaris 2.0 (1991), the linker is itself a normal user binary — the
kernel's only job is to know that the binary asks for one and to load
it before transferring control to the main program.

m3OS shipped without dynamic linking for the first 75 phases because
the kernel and userspace were small enough that every binary could be
statically linked. Phase 76 begins to undo that simplification, but in
a deliberately conservative shape: the kernel learns to honor the
`PT_INTERP` program header, the auxiliary vector grows the two slots
musl's `_dlstart` reads (`AT_BASE`, `AT_ENTRY`), and a new
`/lib/ld-musl-x86_64.so.1` interpreter binary lands. The interpreter
in this phase is intentionally a **transfer-only stub** — it walks the
auxv, finds `AT_ENTRY`, and `jmp`s. Real `DT_NEEDED` resolution,
relocation application, and `dlopen` ship in 76b / 76c / 76d.

## Why split the phase?

The original Phase 76 design tried to ship the full stack — kernel
`PT_INTERP` + `ld.so` bring-up + relocations + `dlopen` + PLT lazy
resolve + GNU hash — in one PR. After design review the scope was
estimated at multi-week work and was split into four subphases along
the `PT_INTERP` boundary:

| Subphase | What it ships | Smoke gate |
|---|---|---|
| **76** | Kernel `PT_INTERP` branch, full SysV-ABI auxv, ld.so PIE crate scaffold, transfer-only `_dlstart` | `dynlink_smoke` ✅ |
| **76b** | Real `_dlstart`, `PT_DYNAMIC` parse, `DT_NEEDED` dependency graph, x86_64 relocations, `DT_INIT_ARRAY` | `libhello.so` + `dynlink_hello` ✅ |
| **76c** | `dlopen` / `dlsym` / `dlclose` / `dlerror`, refcounted handle table, `DT_FINI_ARRAY` + `DT_FINI` destructors on last-close | `dlopen_test` + `libhello_fini.so` ✅ |
| **76d** | PLT lazy resolve (`_dl_runtime_resolve`), `DT_GNU_HASH` Bloom + bucket + chain, `DT_VERSYM` / `DT_VERNEED` / `DT_VERDEF`, `LD_BIND_NOW` env handling | `libhello_gnu.so` + `dynlink_hello_gnu` (lazy / eager / W^X), `libhello_versioned.so` + `dynlink_hello_versioned` (exact match), `dynlink_hello_versioned_mismatch` (mismatch-fallback + strict-mode) ✅ |

The split lets each PR land with a green smoke gate. 76's gate proves
the kernel → ld.so → main binary handoff in isolation; 76b adds real
linking on top; 76c adds the runtime plugin API; 76d adds the
performance / compatibility polish.

## What changes in 76

### Kernel side — `kernel/src/mm/elf.rs`

The ELF loader (`load_elf_into`) grows a new branch: if the binary
carries a `PT_INTERP` segment, the loader reads the interpreter path
from the segment content, calls the new `read_file_from_disk`
indirection (a closure threaded through from
`load_elf_into_with_interp`) to fetch the interpreter ELF, parses it,
and maps its `PT_LOAD` segments at a chosen `interp_load_bias` — far
enough above the main binary's highest mapped vaddr to guarantee no
collision (`INTERP_LOAD_BASE_HINT = 0x4000_0000`, plus a 64 KiB pad
above the main binary's top).

The returned `LoadedElf.entry` is now the **interpreter's** entry
point (not the main binary's), because that is where control
transfers. The main binary's entry travels through the new
`LoadedElf.aux_extras: Option<AuxExtras>` field so the auxiliary
vector can hand it to the interpreter via `AT_ENTRY`.

### Auxiliary vector — `kernel-core/src/elf/auxv.rs`

A new pure-logic kernel-core module owns the byte-exact auxv layout.
`build_layout(phdr, extras, at_random_ptr)` returns the list of
`AuxEntry { a_type, a_val }` slots the kernel writes onto the user
stack. When `extras` is `Some`, the auxv carries `AT_BASE` (interp
load bias) and `AT_ENTRY` (main binary entry); when `None` (static
binary), the auxv keeps its pre-Phase-76 six-entry shape so existing
binaries see no change. The module is host-testable; six tests pin
the layout's exact a_type ordering for both the static and the
dynamic case.

### Userspace `ld.so` — `userspace/ld-musl-x86_64.so.1/`

A new no_std PIE crate. The entry point is `_start` (aliased as
`_dlstart` for cross-reference to musl's naming convention). The
implementation does two things and nothing else:

1. Print `ldso: _dlstart entry=0x<hex>` to fd 2 (serial console) so
   the smoke gate is observable
2. Walk the SysV-ABI stack (argc → argv → NULL → envp → NULL → auxv)
   for `AT_ENTRY` and `jmp` to it

The linker uses inline-asm `syscall` instructions directly — it does
NOT link `syscall_lib` because `syscall_lib::BrkAllocator` would touch
the heap before the main binary has had a chance to initialize. Phase
76b will introduce a no-alloc subset of `syscall_lib` that the linker
can share.

### Build pipeline — `xtask/src/main.rs`

Two new helpers:

- `build_ldso()` compiles `userspace/ld-musl-x86_64.so.1/` with the
  standard `x86_64-unknown-none` target and stages the result to
  `target/generated-libs/ld-musl-x86_64.so.1`. The target's
  `position-independent-executables: true` setting (verified with
  `rustc --print target-spec-json`) is sufficient — no custom target
  spec is needed.
- `build_dynlink_smoke()` compiles `userspace/dynlink_smoke/dynlink-smoke.c`
  with `musl-gcc -nostdlib -nostartfiles -fPIC -Wl,-pie
  -Wl,-dynamic-linker=/lib/ld-musl-x86_64.so.1` so the resulting ELF
  carries `PT_INTERP` but has zero `DT_NEEDED` entries.

`populate_ext2_files` creates `/lib` on the ext2 disk and stages the
linker binary at `/lib/ld-musl-x86_64.so.1`. The same binary is also
embedded in the kernel ramdisk under `/lib/` so the kernel's
`PT_INTERP` reader can find it before ext2 is mounted (early boot).

## Key Files

| File | Role |
|---|---|
| `kernel/src/mm/elf.rs` | ELF loader; `PT_INTERP` branch; `setup_abi_stack_with_envp` extension |
| `kernel-core/src/elf/auxv.rs` | Pure-logic auxv layout (host-testable) |
| `kernel/src/arch/x86_64/syscall/mod.rs` | `execve` path passes the interpreter-reader closure |
| `kernel/src/fs/ramdisk.rs` | Embeds the linker at `/lib/ld-musl-x86_64.so.1` and the cycle libs / demo binaries |
| `userspace/ld-musl-x86_64.so.1/src/main.rs` | `_start` / `dl_relocate_self` / `dl_entry` runtime (76b) |
| `userspace/ld-musl-x86_64.so.1/src/lib.rs` | `ldso_core` library root (host-testable) |
| `userspace/ld-musl-x86_64.so.1/src/reloc.rs` | `apply_relative` / `apply_glob_dat` / `apply_abs64` (76b host-tested) |
| `userspace/ld-musl-x86_64.so.1/src/dynlink.rs` | `DynamicSection::parse` / `elf_hash` / `lookup_in_hash_table` / `topo_sort` (76b host-tested) |
| `userspace/ld-musl-x86_64.so.1/src/elf64.rs` | ELF64 type stubs + dynamic-tag / relocation-type constants |
| `userspace/dynlink_smoke/dynlink-smoke.c` | Smoke binary: `PT_INTERP` set, no `DT_NEEDED` |
| `userspace/lib/libhello/hello.{c,h}` | 76b demo shared library (`hello_str` returns sentinel) |
| `userspace/dynlink_hello/dynlink_hello.c` | 76b consumer linking `-lhello` via `R_X86_64_JUMP_SLOT` |
| `userspace/dynlink_missing/dynlink_missing.c` | 76b F1.4 missing-dep negative gate (`DT_NEEDED = libdoesnotexist.so` → exit 2) |
| `userspace/dynlink_cycle/dynlink_cycle.c` | 76b F1.4 cycle negative gate (links `libcyca.so` ↔ `libcycb.so` → exit 80) |
| `userspace/lib/libcyca/{cyca,cyca_stub}.c` | 76b cycle-test source (final + chicken-and-egg-break stub) |
| `userspace/ld-musl-x86_64.so.1/src/dl.rs` | 76c `DlState`, `dlopen` / `dlsym` / `dlclose` / `dlerror` |
| `userspace/ld-musl-x86_64.so.1/src/handle.rs` | 76c `HandleTable` slab + generation counter (host-tested) |
| `userspace/ld-musl-x86_64.so.1/build.rs` | 76c link-time flags (`--hash-style=sysv` / `--export-dynamic` / `-soname=ld-musl-x86_64.so.1`) |
| `userspace/lib/libdl/libdl.c` | 76c link-time stub library (real impls live in the linker) |
| `userspace/lib/libhello_fini/hello_fini.{c,h}` | 76c destructor demo (`__attribute__((destructor))` writes `LIBHELLO_FINI:RAN`) |
| `userspace/dlopen_test/dlopen_test.c` | 76c smoke binary: open / sym / call / close + four negative paths + DT_FINI_ARRAY assertion |
| `userspace/lib/libcycb/cycb.c` | 76b cycle-test source (other half of the cycle) |
| `userspace/smoke-runner/src/main.rs` | Drives `dynlink_*` and asserts sentinels + exit codes |
| `xtask/src/main.rs` | `build_ldso` + `build_shared_lib` (76d.F: `build_shared_lib_with_hash_style` variant) + `build_dynlink_{smoke,hello,missing,cycle}` + `build_cycle_libs` + 76d artifacts (`build_libhello_{gnu,versioned,versioned_v2_pair}` + `build_dynlink_hello_{gnu,versioned,versioned_mismatch}`) + `/lib/` and `/usr/lib/` staging |
| `userspace/ld-musl-x86_64.so.1/src/sym.rs` | 76d.S1.1 unified `sym::lookup(scope, name, version)` — dispatcher picks GNU over SysV per DSO; 76d.D2.2 version-aware path with `dso_version_matches`; 76d.D2.3 strict-mode error on mismatch under `LD_BIND_NOW` |
| `userspace/ld-musl-x86_64.so.1/src/gnu_hash.rs` | 76d.D1 pure-logic GNU hash primitives (`gnu_hash`, `bloom_probe`, `gnu_hash_lookup`) — 15 host tests with djb2 known-answer fixtures + hand-built tables |
| `userspace/ld-musl-x86_64.so.1/src/ver.rs` | 76d.D2.1 pure-logic versioning parser — `Verdef` / `Verdaux` / `Verneed` / `Vernaux` decoders + `VersionTable` with `version_index` / `defined_version_name` / `required_version_name{,_by_index}` query methods (4 host tests) |
| `userspace/ld-musl-x86_64.so.1/src/plt.rs` | 76d.B4 naked-asm `_dl_runtime_resolve` trampoline + `resolve_pltrel` Rust callback + `install_trampoline` (GOT[1]=link_map, GOT[2]=trampoline addr) + `apply_jmprel_lazy` (lazy-bind GOT rebase) + `BIND_NOW: AtomicBool` master flag |
| `userspace/lib/libhello_gnu/hello.{c,h}` | 76d.F demo lib built with `--hash-style=gnu` (DT_GNU_HASH present, DT_HASH absent) |
| `userspace/dynlink_hello_gnu/dynlink_hello_gnu.c` | 76d.F consumer that asserts `BIND_NOW:{0,1}` via GOT[3]-mutation check (PC-relative `lea _GLOBAL_OFFSET_TABLE_`) and `WX_CHECK:OK` via `/proc/self/maps` scan |
| `userspace/lib/libhello_versioned/{hello.{c,h},libhello_versioned.ver}` | 76d.G demo lib: `--version-script` exports `hello_str@LIBHELLO_1.0` |
| `userspace/dynlink_hello_versioned/dynlink_hello_versioned.c` | 76d.G consumer; `DT_VERNEED` requires `LIBHELLO_1.0` |
| `userspace/lib/libhello_versioned_v2/{hello.{c,h},libhello_versioned_v2.ver,v1_stub.{c,ver}}` | 76d.G.3 mismatch-test artifacts: real v2 lib defines `LIBHELLO_2.0` only; v1 stub (linked at build, NOT staged) lets the mismatch consumer carry `DT_VERNEED` for `LIBHELLO_1.0` |
| `userspace/dynlink_hello_versioned_mismatch/dynlink_hello_versioned_mismatch.c` | 76d.G.3 mismatch consumer — drives D2.2 fallback (default) + D2.3 strict-error (`LD_BIND_NOW=1`) |

## What you'll see on the serial console

```text
elf: PT_INTERP=/lib/ld-musl-x86_64.so.1 (binary=/bin/dynlink_smoke)
elf: mapped pid=12 binary=/lib/ld-musl-x86_64.so.1 p_vaddr=0x40000000 p_flags=r-x ...
elf: mapped pid=12 binary=/lib/ld-musl-x86_64.so.1 p_vaddr=0x40001000 p_flags=rw- ...
elf: interp loaded base=0x40000000 entry=0x40001250 main_entry=0x401000
ldso: _dlstart entry=401000
DYNLINK_SMOKE:PASS
SMOKE:dynlink-smoke:PASS
```

The three log layers tell the same story from three angles: the kernel
records that it honored `PT_INTERP` and reports the interpreter's load
bias and entry; the linker prints the `AT_ENTRY` value it pulled from
the auxv; and the test binary prints its `DYNLINK_SMOKE:PASS` sentinel
once `_start` runs.

## What changes in 76b

Phase 76b grows the transfer-only `_dlstart` stub into a real
bring-up linker. The kernel side is unchanged from Phase 76; the
work is entirely in `userspace/ld-musl-x86_64.so.1/` plus the
`xtask` build pipeline, the kernel ramdisk, and the smoke-runner.

### Architecture

The crate is split into a host-testable `ldso_core` library (no
`#[panic_handler]`, only `core::` imports) and the `no_std` +
`no_main` PIE binary that links it. The library carries:

- `reloc.rs` — `apply_relative` / `apply_glob_dat` / `apply_abs64`
  pure-logic relocation primitives with `RelocError` enum.
- `dynlink.rs` — `DynamicSection::parse`, SysV `elf_hash`,
  `lookup_in_hash_table` (bucket+chain walker with hops bound),
  `topo_sort` (heapless::Vec iterative DFS with cycle detection).
- `elf64.rs` — ELF64 type stubs (`Rela`, `Dyn`, `Sym`, `Phdr`) +
  dynamic-tag constants + x86_64 relocation-type constants.

23 host tests in the library pin the byte-exact semantics of every
primitive:

```bash
cargo test -p ld-musl-x86_64-so-1 --lib --target x86_64-unknown-linux-gnu
# 23 passed; 0 failed
```

### Runtime driver

`main.rs::dl_entry` is the bring-up driver, invoked from naked-asm
`_start`. It:

1. Walks the SysV-ABI initial stack for `AT_BASE`, `AT_PHDR`,
   `AT_PHNUM`, `AT_ENTRY`.
2. Runs `dl_relocate_self` against the linker's own image
   (`DT_RELA` → every `R_X86_64_RELATIVE`) BEFORE any GOT-routed
   read.
3. Computes the main binary's load bias from `PT_PHDR.p_vaddr`
   versus `AT_PHDR` and parses its `PT_DYNAMIC`.
4. Iteratively loads every `DT_NEEDED` (and each loaded DSO's
   transitive `DT_NEEDED`) from `/usr/lib/<name>` with SONAME-keyed
   deduplication. Builds a parallel adjacency-list dependency graph.
5. Runs `topo_sort` over the dependency graph. Detected cycles
   surface as `TopoError::CircularDependency` → `exit(80)` (ELIBBAD).
6. Calls `apply_rela` for the main binary's `DT_RELA` + `DT_JMPREL`
   and for every loaded DSO. The walker dispatches per `r_type`
   (RELATIVE/GLOB_DAT/JUMP_SLOT/R_X86_64_64) and resolves named
   symbols via the loaded-DSO chain (main binary first, then deps
   in load order — SysV global scope).
7. Calls `run_constructors` over the loaded-DSO list deepest-first
   (`DT_INIT` then `DT_INIT_ARRAY` entries as `extern "C" fn()`).
8. Returns `AT_ENTRY` to the naked-asm caller, which `jmp`s into
   the main binary's `_start` with the initial stack intact.

### DSO load strategy

The kernel's `sys_linux_mmap` (`kernel/src/arch/x86_64/syscall/mod.rs`)
ignores `addr_hint` for anonymous mappings — pages always come from
the per-process `mmap_next` allocator. So `load_dso` does NOT try to
place segments at a chosen address. Instead:

1. Open the file; mmap a 64 KiB scratch buffer and read the whole
   file in.
2. Walk `PT_LOAD` headers to compute the max `p_vaddr + p_memsz`.
3. mmap ONE anonymous region of that size as `PROT_READ|PROT_WRITE`.
   The kernel's chosen address becomes the DSO's load bias.
4. Copy each `PT_LOAD`'s `p_filesz` bytes from scratch to
   `load_bias + p_vaddr`. The `p_memsz - p_filesz` tail is already
   zero (mmap-zeroed).
5. For each segment with `PF_X` set, `mprotect` its page range to
   `PROT_READ | PROT_EXEC` (Phase 75 W^X requires the W and X
   mappings be separate).

### Negative gates (F1.4)

Two negative gates ride alongside `dynlink-hello-smoke`:

- `dynlink-missing-smoke` execs `/bin/dynlink_missing`, a binary
  with `DT_NEEDED = libdoesnotexist.so` and no on-disk copy of that
  lib. The linker hits ENOENT and `exit(2)`. Gate asserts
  `WEXITSTATUS == 2`.
- `dynlink-cycle-smoke` execs `/bin/dynlink_cycle`, which depends
  on `libcyca.so`, which depends on `libcycb.so`, which depends on
  `libcyca.so` (closed cycle). The linker's `topo_sort` detects the
  cycle and `exit(80)` (ELIBBAD). Gate asserts `WEXITSTATUS == 80`.

The cycle libs are built in three steps (see `xtask::build_cycle_libs`)
to break the chicken-and-egg link order: a `libcyca_stub.so` is
built first so `libcycb.so` can link against it; then the final
`libcyca.so` is built linking against `libcycb.so` to close the
cycle. Only the final pair is staged on disk.

## What changes in 76c

### libdl runtime — `userspace/ld-musl-x86_64.so.1/src/dl.rs`

The four POSIX libdl entry points (`dlopen`, `dlsym`, `dlclose`,
`dlerror`) live in the dynamic linker binary itself. The linker is
no longer just an exec-time stub — after `dl_entry` returns and the
asm caller `jmp`s to the main binary, the linker stays in memory
and answers libdl calls.

State persistence is a single `static DL_STATE: DlStateCell` wrapping
an `UnsafeCell<DlState>`. The cell is `Sync` only because Phase 76c
is single-threaded; the thread-safety upgrade is gated on TLS. The
`DlState` carries:

- `dsos: [LoadedDso; MAX_SLOTS]` — slot-indexed DSO table.
- `name_storage: [[u8; MAX_NAME_LEN]; MAX_SLOTS]` + `name_lens: [u8; MAX_SLOTS]` — linker-owned per-slot SONAME buffer (so dedup never aliases caller memory or about-to-be-unmapped DSO bytes). Names are interned via `DlState::intern_name` and read back via `DlState::name(idx)`.
- `dep_lists: [heapless::Vec<DsoId, MAX_SLOTS>; MAX_SLOTS]` — dependency edges for `dlsym`'s walk.
- `refcounts: [u32; MAX_SLOTS]` — `0` means free slot; `REFCOUNT_PERMANENT == u32::MAX` for main / linker / bring-up `DT_NEEDED` libs (`dlclose` never unmaps them).
- `in_global_scope: [bool; MAX_SLOTS]` — `RTLD_GLOBAL` visibility.
- `handles: HandleTable` — slab of `(dso_id, generation)` records so a forged or freed handle pointer is detected.
- `error: Option<&'static [u8]>` — last `dlerror()` message; read-and-clear semantics.

### Linker self-injection

`dl_entry` builds the bring-up DSO list as before, but with the
linker itself injected at slot 1 (right after the main binary).
That makes the linker's own DT_HASH the first place the SysV
symbol-search walker checks for `dlopen` / `dlsym` / `dlclose` /
`dlerror`, so the linker's real implementations resolve to those
names regardless of whether `libdl.so` defines them as stubs.

The linker's binary now ships with three new link-time flags
(`build.rs` emits them for the `x86_64-unknown-none` target):

- `--hash-style=sysv` — `DT_HASH` populated, `DT_GNU_HASH` absent (76d will switch).
- `--export-dynamic` — promotes `#[no_mangle] pub extern "C" fn` symbols into the dynamic symbol table.
- `-soname=ld-musl-x86_64.so.1` — `DT_SONAME` set so GNU ld can scan the linker as a shared library at link time.

### Destructor pipeline — `run_destructors_for(&LoadedDso)`

On `dlclose`'s last-close path, the linker walks `DT_FINI_ARRAY` in
reverse-array order then calls `DT_FINI` (if present). Destructors
are invoked through a register-loaded `extern "C" fn()` pointer —
not via a GOT slot — because the DSO's GOT is about to be unmapped;
a GOT-routed indirect call would page-fault on the next dispatch
inside `unmap_dso`. The unmap is a single
`munmap(load_bias, image_len)` (matched to the 76b whole-image
mmap) wrapped by the host-testable `ldso_core::dynlink::unmap_dso`.

### `dlopen_test` smoke gate

The new `dlopen_test` C binary exercises every libdl entry point —
positive open / sym / call / close on `libhello.so`, refcount path
(open twice, close twice), and four negative paths (missing
library, missing symbol, close-of-bogus-handle, double-close).
`libhello_fini.so` has a `__attribute__((destructor))` that writes
`LIBHELLO_FINI:RAN\n` directly via `write(1, …)`; the smoke gate
asserts the strict serial order
`DLOPEN_TEST:FINI_PENDING → LIBHELLO_FINI:RAN → DLOPEN_TEST:PASS`,
so a missing destructor invocation between the bracket sentinels is
a `:FAIL`.

### Caveats / known sharp edges

- **Thread safety:** `DL_STATE` is reachable through an
  `UnsafeCell` with an unsynchronised access wrapper. The current
  invariant is "single-threaded process"; concurrent libdl calls
  from different threads would race. The fix is gated on TLS.
- **`dlerror` is process-global:** likewise gated on TLS. POSIX
  requires thread-local `dlerror` storage; m3OS will switch when
  TLS lands.
- **`RTLD_LAZY` is treated as `RTLD_NOW`:** PLT lazy resolution
  ships in 76d.

## What changes in 76d

76d closes the Phase 76 family. Four major capability tracks ship,
plus a refactor track (S1) that lets the other three land without
re-touching every consumer.

### Symbol-lookup unification — `userspace/ld-musl-x86_64.so.1/src/sym.rs`

Phase 76b/c had three places that walked SysV `DT_HASH` directly:
the free-function `lookup_symbol` in `main.rs`, `lookup_in_dso` in
`dl.rs` (used by `dlsym`), and the inline path in `apply_rela`.
76d.S1 collapses them all behind `sym::lookup(scope, name, version)`.
The dispatcher picks a backend per DSO:

* `Backend::Gnu` when the DSO has `DT_GNU_HASH` (76d.D1) — Bloom
  filter short-circuits non-matches in O(1).
* `Backend::SysV` when the DSO has only `DT_HASH` (76b fallback).
* Skipped when the DSO has neither.

The `version` parameter is reserved for 76d.D2: `None` means
"unversioned lookup" (Phase 76b/c semantics, back-compat); `Some(v)`
means "exact version `v` required".

S1 also routes the three remaining `core::ptr::write_unaligned`
relocation sites through the host-tested `ldso_core::reloc` slice
helpers (`apply_relative`, `apply_glob_dat`) — eliminates the
divergence between what the host tests prove and what the runtime
actually executes.

### GNU hash — `userspace/ld-musl-x86_64.so.1/src/gnu_hash.rs`

`DT_GNU_HASH` is the modern format GNU `ld` emits with
`--hash-style=gnu` (or `both`). Its Bloom filter answers "is this
symbol definitely NOT here?" in O(1), short-circuiting the
expensive bucket-and-chain walk for symbols the DSO doesn't define.

The pure-logic primitives — `gnu_hash` (djb2 with seed 5381),
`bloom_probe`, `gnu_hash_lookup` — live in `ldso_core::gnu_hash`
with 15 host tests (known-answer fixtures: `printf=0x156B2BB8`,
`exit=0x7C967E3F`, `dlopen=0xF9040207`, plus hand-built bucket+chain
tables). The runtime walker in `sym::lookup_gnu` reads the four-word
header inline and bypasses the slice-construction step in the hot
path.

### PLT lazy resolve — `userspace/ld-musl-x86_64.so.1/src/plt.rs`

The 76b/c bring-up linker resolved every `R_X86_64_JUMP_SLOT`
eagerly at load time. That works for the demo but doesn't scale: a
program linking against a multi-MiB `libc.so` would pay the
resolution cost for hundreds of functions it never calls. 76d.B4
adds the classic SysV PLT lazy-resolve dance:

* **`_dl_runtime_resolve`** is a `#[naked]` asm trampoline. On
  first call to a PLT-routed function, the static-linker-generated
  `plt0` stub pushes the link-map (`GOT[1]`) and jumps through the
  resolver pointer (`GOT[2]`) — landing here. The trampoline saves
  all 9 caller-saved registers, loads link_map and reloc_index
  from the PLT-pushed stack slots into `rdi`/`rsi`, calls
  `resolve_pltrel`, overwrites the saved `reloc_index` slot with
  the resolved address, restores the 9 registers, discards the
  stale link_map slot, and `ret`s — which pops the resolved
  address into `rip`. The caller's return address survives one
  slot below so the resolved function's own `ret` works.
* **`resolve_pltrel`** is the Rust callback. It reads
  `DT_JMPREL[reloc_index]`, resolves the symbol via `sym::lookup`
  with the version constraint extracted from the consumer's
  `DT_VERSYM` + `DT_VERNEED`, writes the absolute address into the
  GOT slot at `load_bias + r.r_offset` (so subsequent calls bypass
  the trampoline), and returns the address.
* **`install_trampoline`** runs once per loaded DSO at the end of
  `dl_entry`, after DL_STATE publication and before constructors.
  Writes `link_map` (= `&DL_STATE.dsos[i]`, stable for program
  lifetime) at `GOT[1]` and `&_dl_runtime_resolve` at `GOT[2]`.
* **`apply_jmprel_lazy`** is the lazy-bind reloc applicator. For
  each `JUMP_SLOT` it leaves the static linker's image-relative
  offset alone and adds `load_bias` so the first call lands on
  the PLT's plt0 stub.

The `BIND_NOW: AtomicBool` master flag at the top of `plt.rs`
controls the eager / lazy split. POSIX default is **lazy** (false);
76d.E4's env walk in `dl_entry` flips it to true when
`LD_BIND_NOW=1` is in `envp`.

### Symbol versioning — `userspace/ld-musl-x86_64.so.1/src/ver.rs`

Real-world `.so` files (every glibc-built library since 1999)
record per-symbol version requirements and definitions in three
optional dynamic tags:

* `DT_VERSYM` — `u16` parallel array, one per dynsym entry; low 15
  bits index a `Verdef` (for symbols the DSO defines) or a
  `Vernaux` (for symbols the DSO requires). Bit 15 is a "hidden"
  flag.
* `DT_VERDEF` + `DT_VERDEFNUM` — linked list of `Verdef` records
  (one per version this DSO defines — e.g. `GLIBC_2.2.5`).
* `DT_VERNEED` + `DT_VERNEEDNUM` — linked list of `Verneed` records
  (one per `DT_NEEDED` dependency the DSO requires versions from).

76d.D2.1 ships the pure-logic decoders in `ldso_core::ver`. The
`VersionTable` struct provides:

* `version_index(symbol_idx) -> Option<u16>` — read VERSYM with the
  hidden bit masked.
* `defined_version_name(version_index) -> Option<&[u8]>` — walk
  VERDEF for matching `vd_ndx`, follow `vd_aux` to the `Verdaux`,
  return the name from STRTAB.
* `required_version_name{_by_index}` — walk VERNEED's Vernaux
  chains.

4 host tests pin the decoder behavior.

76d.D2.2 wires the runtime path. `apply_rela` and `resolve_pltrel`
read `versym[sym_idx]` on the consumer, walk `DT_VERNEED` for the
matching `vna_other`, and pass the resulting version-name to
`sym::lookup`. The dispatcher then matches against each candidate
provider's `dyn_.versym` + `dyn_.verdef` via `dso_version_matches`.

The matching rules follow standard glibc back-compat:

* Provider with no `DT_VERSYM` (unversioned) satisfies any version
  request — keeps Phase 76b/c binaries working.
* Provider with VERSYM index 0 (LOCAL) or 1 (GLOBAL) is the
  unversioned default export; matches any request.
* Provider with VERSYM ≥ 2 is versioned; matches only when the
  VERDEF lookup returns the requested name.

76d.D2.3 strict mode: when `BIND_NOW=true` (set by E4 on
`LD_BIND_NOW=1`), a version-mismatch returns `None` and emits a
serial error instead of falling back to unversioned lookup. The
caller (`apply_rela`) treats the resulting `0` address as an
undefined symbol and `exit`s.

### `LD_BIND_NOW` — `userspace/ld-musl-x86_64.so.1/src/main.rs::read_ld_bind_now`

76d.E4 walks `envp` at the very start of `dl_entry`, before any
relocation pass runs. When `LD_BIND_NOW` is present and its value
is non-empty AND non-zero, stores `true` to `plt::BIND_NOW`. The
flag controls two paths: 76d.B4.4's lazy-vs-eager `JUMP_SLOT`
applicator, and 76d.D2.3's strict-vs-fallback version-mismatch
handler.

### Gates

76d adds two new smoke gates with sub-phase coverage:

* `dynlink-hello-gnu-smoke` (76d.F) — runs `/bin/dynlink_hello_gnu`
  twice. Default env asserts `BIND_NOW:0` (GOT[3] mutated across
  first call = lazy resolution went through trampoline) +
  `HELLO_FROM_GNU_LIB:OK` + `WX_CHECK:OK`. `LD_BIND_NOW=1` asserts
  `BIND_NOW:1` (GOT[3] stable = pre-resolved at load).
* `dynlink-hello-versioned-smoke` (76d.G) — runs
  `/bin/dynlink_hello_versioned` twice (default + `LD_BIND_NOW=1`),
  both asserting `HELLO_FROM_VERSIONED_LIB:OK`. The exact-version
  match must succeed in both modes (strict only rejects mismatches).
* `dynlink-hello-versioned-mismatch-smoke` (76d.G.3) — runs
  `/bin/dynlink_hello_versioned_mismatch` twice. Default env: D2.2
  warn + fallback to unversioned `hello_str` →
  `HELLO_FROM_V2_FALLBACK:OK`. `LD_BIND_NOW=1`: D2.3 error + non-zero
  exit (the asm caller `jmp 0`s after `dl_entry` returns 0,
  triggering SIGSEGV).

## What is intentionally NOT in 76 / 76b / 76c / 76d

| Concern | Subphase |
|---|---|
| TLS blocks (`__thread`, `tls_get_addr`, TLS-aware reloc types) | Beyond 76d (gates Phase 77 TLS work) |
| `RTLD_NEXT` + interpose / preload semantics | Beyond 76d |
| `dlmopen` (namespaces / load groups) | Beyond 76d |
| IFUNC (indirect functions for CPU-specific dispatch) | Beyond 76d |
| `DT_FILTER` / `DT_AUXILIARY` (filter-only DSOs) | Beyond 76d |
| `dladdr` / `dlinfo` / `dlvsym` | Beyond 76d |
| `LD_BIND_NOT` (alternate lazy mode that resolves but doesn't patch) | Beyond 76d |
| Verdef-aux chain walk (multiple Verdaux per Verdef — for aliases) | Beyond 76d (we only read the first Verdaux per Verdef) |

## Related Roadmap Docs

- [Phase 76 roadmap doc](./roadmap/76-dynamic-linker.md)
- [Phase 76 task list](./roadmap/tasks/76-dynamic-linker-tasks.md)
- [Phase 76b roadmap doc](./roadmap/76b-dynamic-linker-bringup.md)
- [Phase 76c roadmap doc](./roadmap/76c-dlopen.md)
- [Phase 76d roadmap doc](./roadmap/76d-dynamic-linker-polish.md)
- [Phase 11 process model](./roadmap/11-process-model.md) — original ELF loader
- [Phase 75 W^X enforcement](./75-wx-enforcement.md) — guarantees the linker's text and GOT stay write-XOR-execute compliant
