# Dynamic Linker — Scaffolding (Phase 76) + Bring-up (Phase 76b)

**Aligned Roadmap Phase:** Phase 76 / Phase 76b
**Status:** 76 Implemented (scaffolding); 76b Implemented (bring-up — DT_NEEDED + 4 relocations + constructors + negative gates)
**Source Ref:** phase-76 / phase-76b
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
| **76** (this) | Kernel `PT_INTERP` branch, full SysV-ABI auxv, ld.so PIE crate scaffold, transfer-only `_dlstart` | `dynlink_smoke` — a no-DT_NEEDED dynamic ELF that prints `DYNLINK_SMOKE:PASS` |
| 76b | Real `_dlstart`, `PT_DYNAMIC` parse, `DT_NEEDED` dependency graph, x86_64 relocations, `DT_INIT_ARRAY` | `libhello.so` + `dynlink_hello` |
| 76c | `dlopen` / `dlsym` / `dlclose`, `DT_FINI_ARRAY` | `dlopen_test` |
| 76d | PLT lazy resolve (`_dl_runtime_resolve`), `DT_GNU_HASH`, basic symbol versioning | A `.so` built with `--hash-style=gnu` |

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
| `userspace/lib/libcycb/cycb.c` | 76b cycle-test source (other half of the cycle) |
| `userspace/smoke-runner/src/main.rs` | Drives `dynlink_*` and asserts sentinels + exit codes |
| `xtask/src/main.rs` | `build_ldso` + `build_shared_lib` + `build_dynlink_{smoke,hello,missing,cycle}` + `build_cycle_libs` + `/lib/` and `/usr/lib/` staging |

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

## What is intentionally NOT in 76 / 76b

| Concern | Subphase |
|---|---|
| `dlopen(path, flags)`, `dlsym(handle, name)`, `dlclose(handle)`, `dlerror()` | 76c |
| `DT_FINI` / `DT_FINI_ARRAY` destructor running on `dlclose` | 76c |
| PLT lazy resolution (`_dl_runtime_resolve` asm trampoline + first-call GOT slot rewrite) | 76d |
| `DT_GNU_HASH` Bloom-filter symbol lookup | 76d |
| `DT_VERSYM` / `DT_VERNEED` graceful handling | 76d |
| TLS blocks, `RTLD_NEXT`, namespaces (`dlmopen`), IFUNC | Beyond 76d |

## Related Roadmap Docs

- [Phase 76 roadmap doc](./roadmap/76-dynamic-linker.md)
- [Phase 76 task list](./roadmap/tasks/76-dynamic-linker-tasks.md)
- [Phase 76b roadmap doc](./roadmap/76b-dynamic-linker-bringup.md)
- [Phase 76c roadmap doc](./roadmap/76c-dlopen.md)
- [Phase 76d roadmap doc](./roadmap/76d-dynamic-linker-polish.md)
- [Phase 11 process model](./roadmap/11-process-model.md) — original ELF loader
- [Phase 75 W^X enforcement](./75-wx-enforcement.md) — guarantees the linker's text and GOT stay write-XOR-execute compliant
