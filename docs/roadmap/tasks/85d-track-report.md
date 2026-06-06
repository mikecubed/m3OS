# Phase 85d — Parallel-Impl Track Report

Durable batch/track report for the Phase 85d (Clang/LLVM/LLD + Release) implementation
run on branch `feat/phase-85d-clang-llvm`. Updated as tracks progress.

## Batch context

- **Integration branch:** `feat/phase-85d-clang-llvm` → PR to `main`.
- **Toolchain reality:** no musl C++ compiler in this environment (only the C-only
  `musl-tools` wrapper). Host **clang 18.1.3** is used as the cross-compiler
  (`--target=x86_64-linux-musl` + an assembled musl sysroot). LLVM pinned **18.1.8**.
- **Validation surface:** `cargo xtask check`; `cargo xtask port build llvm`;
  `cargo xtask <clang-smoke>` (new gate); pkgcache-hit assertion; in-OS C/C++ build.

## Tracks

### Track A — Clang + LLD cross-build
- **Owned tasks:** A.1–A.5
- **Owned files:** `ports/lang/llvm/Portfile`, `xtask/src/port_build.rs`
  (`build_llvm`, `build_llvm_port`, `build_recipe_id`, `port_deps`, dispatch),
  `xtask/src/main.rs` (`PORTS`/`BUNDLE_ONLY_PORTS`).
- **Validation:** `cargo xtask port build llvm` seals a `.m3pkg`; staged tree has
  `bin/clang`, `bin/lld`, `bin/clang++`, `lib/clang/<ver>/include`, libc++/abi/unwind,
  CRT + musl sysroot; resource dir relative to the binary.
- **State:** active (coordinator-driven, main tree).

### Track B — Opt-in packaging + validation
- **Owned tasks:** B.1, B.2
- **Owned files:** `xtask/src/main.rs` (`cmd_clang_smoke`, image-feature gate,
  `populate_ext2_files` fixtures), `AGENTS.md` (regression row — coordinator),
  data-disk fixtures `/usr/src/hello.c` + `/usr/src/hello.cpp`.
- **Validation:** in-OS `pkg install clang`; `clang -O2 hello.c -o hello && ./hello`;
  `clang++ hello.cpp`; `clang -fuse-ld=lld`; `clang -print-resource-dir` under `/usr`;
  second image build = pkgcache hit, zero compiler invocations.
- **State:** pending (depends on Track A artifact).

### Track C — Release closeout (docs)
- **Owned tasks:** C.1, C.2 (docs/README parts), C.3 partial
- **Owned files:** `docs/85-cross-compiled-toolchains.md` (new), `docs/README.md`,
  `docs/roadmap/README.md`. Coordinator handles `AGENTS.md` + `kernel/Cargo.toml`.
- **Worktree:** isolated worktree, separate agent, background.
- **State:** active (parallel agent).

## Progress

- **Track A — DONE + validated.** `cargo xtask port build llvm` cross-built static
  clang+lld (LLVM 18.1.8) and self-validated (the staged clang compiled+linked+ran
  C and C++ host-side). Sealed `clang.m3pkg` = **130,541,744 B (≈125 MB)**.
- **Track B / B.1 — DONE + validated.** Second build = `PKGCACHE: hit … zero
  compiler invocations` (the 85a payoff, on the heaviest artifact). Opt-in
  `M3OS_WITH_CLANG` bundled `clang.m3pkg` into `/usr/pkg`.
- **Track C — DONE + merged.** Learning doc (disk-delta corrected to measured
  125 MB + host-clang-cross note), README links, roadmap rows; AGENTS.md bullet +
  `v0.85.3` + gate row; kernel `0.85.3`; pre-push `M3OS_CLANG_REGRESSION`.
- **B.2 — in progress (rerun).** First `clang-smoke` run timed out mid-install
  (not a fault): the installer reads+SHA-verifies the 124 MiB `.m3pkg` (~10 min)
  then writes ~1500 files over the ~200 KB/s VFS (~25 min). Raised step ceilings +
  `--timeout 5400`; rerunning.

## BLOCKER — clang exec exceeds the kernel heap

`clang-smoke` rerun: **install succeeded** (`pkg install: clang: OK` — the timeout
fix worked), but the first `clang --version` failed to exec:
```
[exec] file too large (68103968 bytes > 33554432 limit): /usr/bin/clang-18
[execve] file not found or rejected: /usr/bin/clang-18
```
Root cause: `sys_execve` → `read_file_from_disk` loads the **whole** binary into one
kernel-heap `Vec`, capped at `max_page_backed_allocation_bytes()` = buddy
`MAX_ORDER`(13) → **32 MiB**. The clang-18 binary is **~65 MiB**, which is also
larger than the **entire** `HEAP_MAX_SIZE` = **64 MiB** kernel heap. So clang
cannot be exec'd without a kernel change. Fix options: (1) a **streaming ELF
loader** that reads PT_LOAD segments from disk straight into the mapped user pages
(no giant kernel buffer — the per-page copy in `map_load_segment` is already
page-by-page); (2) raise `HEAP_MAX_SIZE` + buddy `MAX_ORDER` so a 65 MiB read
buffer fits (simpler, but fragile — needs the buddy to form a 128 MiB block — and
wasteful); (3) land the completed work and defer in-OS execution to a kernel
follow-up. **Resolved — maintainer chose the streaming loader (option 1):**
- `kernel/src/mm/elf.rs` — an `ElfBytes` byte-source trait (the `&[u8]` impl is
  byte-identical to the pre-85d path); `map_load_segment` reads each page through
  it; `load_elf_streaming` loads a large **static ET_EXEC** by reading only the
  ELF header + phdr table, then streaming each PT_LOAD page-by-page into the user
  pages (no giant kernel buffer; rejects PT_INTERP/PT_DYNAMIC).
- `kernel/src/arch/x86_64/syscall/mod.rs` — `DiskElfSource` (a 1 MiB-windowed,
  ext2-backed `ElfBytes`); `open_exec_stream`; `sys_execve` streams when
  `read_file_from_disk` returns E2BIG, gated to large binaries so existing exec is
  untouched. `resolve_block` handles clang's double-indirect blocks.
- Kernel compiles clean (`-Zbuild-std`). clang-smoke rerun in progress to validate
  clang exec + in-OS compile end-to-end.

## Kernel enablers (beyond the original 85d scope)

Running a 65 MiB static clang in-OS surfaced several kernel gaps not anticipated by
the task list. Each was a real, general fix (not a clang hack):

1. **Streaming ELF exec loader** (`mm/elf.rs`, `syscall/mod.rs`) — clang-18 (65 MiB)
   exceeds the entire 64 MiB kernel heap, so `sys_execve`'s read-whole-binary path
   could not load it. Added an `ElfBytes` source + `load_elf_streaming` + a
   windowed ext2 `DiskElfSource`, gated to large binaries.
2. **PT_DYNAMIC tolerance in streamed static ET_EXEC** — clang/python/git static
   binaries carry a benign `.dynamic`; reject only PT_INTERP.
3. **`USER_VADDR_MIN` 4 MiB → 2 MiB** — LLD bases x86_64 `ET_EXEC` at 0x200000.
4. **`pread64` (17) + `pwrite64` (18)** — LLVM reads/writes files **positionally**;
   CPython uses sequential read, which is why python worked in-OS and clang didn't.
   This was THE compile blocker. `pread64` is race-free (offset-based per backend:
   `vfs_service_read` for `/usr` VFS files, `kernel_read_fd_at` otherwise).
5. **`getrlimit` (97) + `prlimit64` (302)** — were ENOSYS; report generous limits.
6. **VFS `fstat` inode identity** (`syscall/mod.rs`, `process/mod.rs`) — the
   *official* gate (fresh disk + real install) failed intermittently with clang
   `error: redefinition of 'main'`: `fstat`-by-fd returned `st_ino = 0` for
   `vfs_server`-backed files while `fstatat`-by-path returned the real inode, so
   clang's `(st_dev, st_ino)` file-dedup collapsed `<stdio.h>` onto the open main
   source → recursive self-include. Fixed by resolving the real ext2 inode at
   VFS open and reporting it as `st_ino`. Full root-cause + systemic findings:
   `docs/post-mortems/2026-06-06-vfs-fstat-inode-identity-and-ext2-dual-impl.md`;
   the systemic audit is tracked as **Phase 93**.

**Result — clang fully works in-OS, validated end-to-end.** Final validation: a
fresh-disk `pkg install clang` + **9 in-OS compiles** (`M3OS_CLANG_STRESS`) all
passed with **zero** recurrences — `clang --version`=18.1.8, `-print-resource-dir`
=`/usr/lib/clang/18`, C compile+link(lld)+run (`CLANG_C_OK`), C++ compile+link
(libc++)+run (`CLANG_CPP_OK`), `-fuse-ld=lld`, and 6 stress C compiles. The
inode fix is deterministic (the bug was ~1-in-2-3 before it; 9/9 after).

## Rescue history
- **clang-smoke timeout (install).** Trigger: step-14 install exceeded the 900 s
  ceiling while still writing files (verify ~10 min + bulk write ~15 min over the
  slow VFS). Action: raised install→2400 s, version/resource-dir→600 s,
  compiles→1500 s, gate `--timeout`→5400; rerun. Same-config rerun (no rebuild —
  pkgcache hit). No agent rescue; coordinator-driven fix.

## Outcome measures (filled at batch close)
- discovery-reuse: n/a (coordinator inline discovery)
- rescue-attempts: 0
- abandonment-events: 0
- re-review-loops: 0
