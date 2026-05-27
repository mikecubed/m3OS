# Phase 76 — Dynamic Linker / Shared Libraries: Task List

**Status:** In Progress
**Source Ref:** phase-76
**Depends on:** Phase 11 (Process Model) ✅, Phase 12 (POSIX Compatibility) ✅, Phase 75 (W^X Enforcement) ✅, Phase 31 (TCC Compiler Bootstrap) ✅
**Goal:** Land the kernel `PT_INTERP` branch, the auxv `AT_BASE`/`AT_ENTRY` extension, the `ld-musl-x86_64.so.1` PIE crate scaffold, and the `dynlink_smoke` end-to-end test that proves the kernel → ld.so stub → main binary handoff. Full dynamic-linker semantics ship in Phase 76b / 76c / 76d.

## Subphase Split (added during implementation)

Phase 76 turned out to be too large for one PR. It was split into four subphases. This task list documents only the **76 scaffolding** tracks. See:

- [`76b-dynamic-linker-bringup-tasks.md`](./76b-dynamic-linker-bringup-tasks.md) — DT_NEEDED + relocations + constructors + dynlink_hello
- [`76c-dlopen-tasks.md`](./76c-dlopen-tasks.md) — dlopen / dlsym / dlclose + dlopen_test
- [`76d-dynamic-linker-polish-tasks.md`](./76d-dynamic-linker-polish-tasks.md) — PLT lazy resolve + GNU hash + symbol versioning

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Kernel ELF loader `PT_INTERP` branch and aux vector | Phase 11 ✅, Phase 75 ✅ | In Progress |
| E | `xtask` `ld.so` crate scaffold, custom target spec, build + stage to `/lib/` | Phase 31 ✅ | Planned |
| B-stub | `_dlstart` transfer-only stub (no relocations, no DT_NEEDED) | A, E | Planned |
| F | `dynlink_smoke` musl-built dynamic ELF + smoke gate | B-stub, E | Planned |
| G | Phase 11 design-doc note on `PT_INTERP` extension | F | Planned |
| H | Docs (`docs/76-dynamic-linker.md`), kernel v0.76.0, AGENTS.md, roadmap README | A–G | Planned |

---

## Track A — Kernel ELF Loader `PT_INTERP` Branch

### A.1 — Detect `PT_INTERP` and load the interpreter ELF

**File:** `kernel/src/mm/elf.rs`
**Symbol:** `load_elf_into`, new helper `read_pt_interp_path`, new helper `map_interpreter`
**Why it matters:** Without this branch, `exec` of a dynamically linked binary silently fails because the kernel tries to execute the main ELF without the linker having run.

**Acceptance:**
- [ ] `load_elf_into` scans PT segments; if `PT_INTERP` is present, it reads the interpreter path from segment content (NUL-terminated string up to `p_filesz`)
- [ ] A new `read_interpreter` callback indirection (or `mm::elf::set_interpreter_reader` registry) lets the loader call into `crate::arch::x86_64::syscall::read_file_from_disk` without `mm::elf` taking a direct dependency on the syscall module
- [ ] The interpreter ELF is parsed and its `PT_LOAD` segments are mapped into the new process at `interp_load_bias`, computed as `max(INTERP_LOAD_BASE_HINT, main_binary_highest_vaddr_page_aligned + 0x10000)` so the interpreter never overlaps the main binary
- [ ] The interpreter's text segments are mapped `R-X`; its data segments are mapped `RW-|NX` (consistent with Phase 75)
- [ ] If the interpreter path does not exist on disk, `load_elf_into` returns `ElfError::MappingFailed("PT_INTERP not found")` and `execve` surfaces `NEG_ENOENT`
- [ ] A `log::info!("elf: PT_INTERP={} interp_bias={:#x}", path, interp_load_bias)` line appears in serial before the auxv is built

### A.2 — Auxiliary vector construction (full SysV-ABI)

**File:** `kernel/src/mm/elf.rs`
**Symbol:** `setup_abi_stack_with_envp`, new struct `AuxExtras`
**Why it matters:** The dynamic linker reads the auxiliary vector to find the main binary's program headers, entry point, and its own load bias; a missing or wrong `AT_BASE` causes the linker to compute wrong addresses.

**Acceptance:**
- [ ] `setup_abi_stack_with_envp` takes a new `aux_extras: Option<AuxExtras>` argument carrying `at_base` (interpreter load bias) and `at_entry` (main binary entry). The two fields travel together — both Some or both None — because `AT_ENTRY` always points to the main binary while `AT_BASE` is only meaningful when an interpreter was loaded.
- [ ] When `aux_extras` is `Some`, the auxv emits these entries (in this exact order, from low addresses upward after `envp NULL`): `AT_PHDR`, `AT_PHENT`, `AT_PHNUM`, `AT_PAGESZ`, `AT_BASE`, `AT_ENTRY`, `AT_RANDOM`, `AT_NULL`
- [ ] When `aux_extras` is `None` (static-only path), the auxv stays at its current 6-entry shape (`AT_PHDR`, `AT_PHENT`, `AT_PHNUM`, `AT_PAGESZ`, `AT_RANDOM`, `AT_NULL`) so existing static binaries are unaffected
- [ ] Initial `rsp` is 16-byte aligned at the point control transfers to the interpreter (SysV-ABI requirement for `_dlstart`)
- [ ] A new `kernel-core::elf::auxv` pure-logic module exposes `compute_layout(argv, envp, aux_extras) -> AuxvLayout` so the byte-exact layout is host-testable; `mm::elf` calls it and then writes the bytes via the existing physical-offset trick

---

## Track E — `xtask` `ld.so` Build Pipeline

### E.1 — `userspace/ld-musl-x86_64.so.1/` crate scaffold

**Files:**
- `userspace/ld-musl-x86_64.so.1/Cargo.toml`
- `userspace/ld-musl-x86_64.so.1/src/main.rs`
- `userspace/ld-musl-x86_64.so.1/x86_64-m3os-ldso.json` (custom target spec)
- `userspace/ld-musl-x86_64.so.1/.cargo/config.toml` (per-crate build-std + linker flags)
- `Cargo.toml` (workspace `members`)

**Symbol:** `_dlstart`, `dlstart_rust`
**Why it matters:** The dynamic linker is the one userspace binary that must be a `-pie` `no_std` ELF with its own `_start` (`_dlstart`); none of the existing userspace binaries are built this way, so the build pipeline must grow a new code path.

**Acceptance:**
- [ ] `userspace/ld-musl-x86_64.so.1/` exists as a workspace crate with `crate-type = ["bin"]`, `edition = "2024"`, `#![no_std]`, `#![no_main]`
- [ ] Crate is built with `relocation-model = "pic"` and `position-independent-executables = true` via the custom target spec
- [ ] `_dlstart` is an inline-asm entry point that preserves the initial `rsp`, calls `dlstart_rust(rsp)`, and `jmp`s to the returned `AT_ENTRY` value
- [ ] `dlstart_rust` walks the SysV-ABI stack (argc, argv[], NULL, envp[], NULL, auxv[]) looking for `AT_ENTRY` and returns it
- [ ] An early serial-write via `sys_write(2, ...)` prints `ldso: _dlstart entry=0x{:x}` before the `jmp` so the handoff is observable
- [ ] `readelf -h` on the built binary reports `Type: DYN (Shared object file)`

### E.2 — `xtask::build_ldso` and stage to `/lib/`

**Files:**
- `xtask/src/main.rs`

**Symbol:** `build_ldso`, `populate_ext2_files`
**Why it matters:** Without a build-system integration and the on-disk path, the kernel cannot find the interpreter and every dynamically linked binary fails with `ENOENT`.

**Acceptance:**
- [ ] `build_ldso` invokes `cargo build --release --bin ld-musl-x86_64.so.1 --target <crate>/x86_64-m3os-ldso.json -Zbuild-std=core,compiler_builtins -Zbuild-std-features=compiler-builtins-mem` and stages the binary to `target/generated-libs/ld-musl-x86_64.so.1`
- [ ] `build_ldso` is called from the userspace build flow before `populate_ext2_files`
- [ ] `populate_ext2_files` creates `/lib` (`mkdir lib` + `sif lib mode 0x41ED uid 0 gid 0`) if absent
- [ ] `populate_ext2_files` stages `target/generated-libs/ld-musl-x86_64.so.1` to `/lib/ld-musl-x86_64.so.1` with `mode 0x81ED uid 0 gid 0` (rwxr-xr-x)
- [ ] `cargo xtask clean && cargo xtask run` produces a disk that contains `/lib/ld-musl-x86_64.so.1`

---

## Track F — Test Application

### F.1 — `dynlink_smoke` musl-built dynamic ELF

**Files:**
- `userspace/dynlink_smoke/dynlink-smoke.c`
- `xtask/src/main.rs` (entry in `build_musl_bins` table)
- `kernel/src/fs/ramdisk.rs` (`include_bytes!` + `BIN_ENTRIES` row)

**Symbol:** `_start` (no `main` — the binary is built `-nostdlib -nostartfiles` so it has no `crt1.o` and is its own entry point)
**Why it matters:** This is the minimum end-to-end proof that the kernel → ld.so → main binary handoff works. Without this binary there is no smoke gate for Phase 76.

**Acceptance:**
- [ ] `userspace/dynlink_smoke/dynlink-smoke.c` is a single-file C program built by `musl-gcc` (or host `gcc` as fallback) with `-nostdlib -nostartfiles -fPIC -Wl,-pie -Wl,-dynamic-linker=/lib/ld-musl-x86_64.so.1` so the resulting ELF carries `PT_INTERP = /lib/ld-musl-x86_64.so.1` and zero `DT_NEEDED` entries (`-nostdlib` excludes libc.so; the binary uses inline-asm `syscall` directly)
- [ ] `_start` writes `DYNLINK_SMOKE:PASS\n` to fd 2 (stderr / serial console) via inline-asm `syscall` and then exits with `sys_exit(0)` (also via inline-asm `syscall`). fd 2 is chosen over fd 1 so the smoke runner's serial-log pattern match works even when the binary runs under a shell whose stdout is redirected
- [ ] `cargo xtask build` produces `target/generated-initrd/dynlink_smoke` whose `readelf -d` shows `PT_INTERP` set and **no** `DT_NEEDED` entries (the `-nostdlib` build is what guarantees this — `libc.so` resolution is a 76b problem, not a 76 problem)
- [ ] The binary appears in the ramdisk and is callable as `/bin/dynlink_smoke`

### F.2 — `dynlink-smoke` xtask gate

**Files:**
- `xtask/src/main.rs` (new `dynlink_smoke` subcommand + smoke-runner driver)
- `userspace/smoke-runner/src/main.rs` (per-mode case branch)
- `.githooks/pre-push` (optional new env-gated entry)

**Symbol:** `cmd_dynlink_smoke`
**Why it matters:** Without an automated regression, the kernel `PT_INTERP` branch will silently break on the next ELF-loader refactor.

**Acceptance:**
- [ ] `cargo xtask dynlink-smoke` builds the disk, boots QEMU, and asserts the serial log contains `DYNLINK_SMOKE:PASS`
- [ ] The same gate is wired into `cargo xtask smoke-test` (always-on) so the existing pre-push hook covers it
- [ ] Failure modes: missing `DYNLINK_SMOKE:PASS` ⇒ exit 70, QEMU launch failure ⇒ exit 71

---

## Track G — Design Doc Updates

### G.1 — Update Phase 11 design doc

**File:** `docs/roadmap/11-process-model.md`
**Symbol:** N/A
**Why it matters:** Phase 11's doc defers dynamic linking; the `PT_INTERP` branch landing in 76 should be cross-referenced so future readers don't repeat the audit.

**Acceptance:**
- [ ] Phase 11 "Deferred Until Later" entry for `PT_INTERP` / dynamic linking updated to "Kernel `PT_INTERP` branch + auxv `AT_BASE` delivered in Phase 76; full dynamic-linker semantics in Phase 76b–76d"
- [ ] Phase 11 "Feature Scope" or "How This Builds on Earlier Phases" notes that `load_elf_into` is extended in Phase 76

---

## Track H — Documentation and Release

### H.1 — Create the aligned legacy learning doc

**File:** `docs/76-dynamic-linker.md`
**Symbol:** N/A
**Why it matters:** Dynamic linking is architecturally new ground; a learner-friendly doc that walks through `PT_INTERP`, the auxv layout, and the `_dlstart` handoff prevents readers from having to reconstruct it from the ELF spec.

**Acceptance:**
- [ ] File exists at `docs/76-dynamic-linker.md`
- [ ] All required template fields populated: `**Aligned Roadmap Phase:** Phase 76`, `**Status:** Implemented`, `**Source Ref:** phase-76`, `**Supersedes Legacy Doc:** new`
- [ ] Overview is learner-friendly (explains why dynamic linking exists, what `PT_INTERP` does, and how the auxv carries `AT_BASE`/`AT_ENTRY` to the linker)
- [ ] Key Files table cites real files this phase touches: `kernel/src/mm/elf.rs`, `kernel-core/src/elf/auxv.rs`, `userspace/ld-musl-x86_64.so.1/src/main.rs`, `xtask/src/main.rs`, `userspace/dynlink_smoke/dynlink-smoke.c`
- [ ] Related Roadmap Docs links `docs/roadmap/76-dynamic-linker.md` and `docs/roadmap/tasks/76-dynamic-linker-tasks.md`
- [ ] Closing section explicitly forward-references Phase 76b–76d for the missing dynamic-linker semantics

### H.2 — Bump kernel version to 0.76.0

**Files:**
- `kernel/Cargo.toml`
- `Cargo.lock`
- `AGENTS.md`
- `docs/roadmap/README.md`

**Symbol:** `version` in `kernel/Cargo.toml` `[package]`
**Why it matters:** Project convention is one minor-version bump per shipped phase; even with the subphase split, the kernel changes shipped in 76 (the `PT_INTERP` branch) warrant the bump.

**Acceptance:**
- [ ] `kernel/Cargo.toml` `version = "0.76.0"`
- [ ] `Cargo.lock` regenerated (run `cargo check` or `cargo xtask check` to trigger it)
- [ ] `AGENTS.md` "Kernel v0.76.0" updated with a short summary of the 76 scope (PT_INTERP + auxv + ld.so scaffold)
- [ ] `docs/roadmap/README.md` Phase 76 row Status updated to "In Progress (scaffolding)" and new rows added for 76b/76c/76d as "Planned"
- [ ] `cargo xtask check` passes

---

## Documentation Notes

- Track A's auxiliary vector layout must exactly match what musl `_dlstart` would expect at process start — even though our 76 stub does not run any musl code, future 76b ldso bring-up code (and any future port to musl's actual `dynlink.c`) reads the same byte layout. The musl source's `arch/x86_64/crt_arch.h` is the authoritative reference.
- Track B-stub is intentionally trivial: it does NOT apply relocations, does NOT walk `DT_NEEDED`, does NOT run constructors. This is by design — those concerns belong to Phase 76b and bundling them here would push the PR past reviewable size.
- Track F's `dynlink_smoke` deliberately avoids any `DT_NEEDED` resolution by using `musl-gcc -nostdlib` and a hand-written `_start` that calls `write(2)` directly via `int 0x80` / `syscall`. If a libc.so symbol resolution were required, the test would fail in 76 and only pass in 76b — which would block the 76 scaffolding gate.
- Track H.1's learner doc should be authored after Track F so it can cite the actual serial output as a concrete example of a successful kernel→ld.so→main handoff.
- The original (pre-split) task list moved its B.1–B.5 / C.1–C.2 / D.1–D.2 / E.3 (build_shared_lib) / F.1–F.2 (libhello.so + dynlink_hello + dlopen_test) acceptance items into the 76b / 76c / 76d task docs.
