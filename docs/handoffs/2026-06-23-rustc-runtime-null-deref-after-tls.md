---
status: OPEN — root cause NOT yet confirmed, but a NEW high-confidence CANDIDATE FIX
  has landed and needs a `rustc-smoke` run to confirm. The on-device `rustc`
  (Phase 95b RUSTC_OK milestone) cold-loads + relocates + executes
  `librustc_driver.so` but NULL-derefs (`CR2=0`) early in startup. The multi-module
  static-TLS loader gap (commit `59cd0c00`) was ruled OUT — rustc crashed identically
  with it fixed. A **separate loader ABI gap** was then found by static analysis: the
  from-scratch ld-musl ran shared-object `DT_INIT`/`DT_INIT_ARRAY` constructors with
  NO arguments (matching musl's `do_init_fini`), but **glibc's loader passes
  `(argc, argv, envp)` to every init_array entry including shared libraries**, and
  Rust's `std` captures `argv`/`envp` via exactly such an `.init_array` entry. A
  dynamic (`prefer-dynamic`) rustc keeps `std` in a shared `libstd-*.so`, so its
  argv-capture ctor was run with no args → null/garbage argv → `std::env::args()`
  derefs null → `CR2=0`. **Fix landed (commit `cdb48a11`): the loader now passes
  `(argc, argv, envp)` to all constructors, glibc-style.** Regression-guarded by
  `dynamic-hello-smoke` (existing C ctors ignore the extra register args — harmless
  on the SysV ABI). **NOT yet confirmed against rustc** — a full `rustc-smoke`
  (~90 min, KVM) is required. If rustc still `CR2=0`s, the remaining candidate is a
  TLS-residual / different null deref — use the offline disassembly path below
  (`rip - base = offset`, check for an `fs:` prefix to distinguish TLS from a plain
  null).
owner: unassigned (follow-up to the Phase 95c VFS-perf + loader-TLS work)
---

# rustc runtime NULL-deref (`CR2=0`) after the loader-TLS gap was closed

## TL;DR

`pkg install rust` now **completes** and `rustc --version` **cold-loads, relocates,
and starts executing** the 162 MiB `librustc_driver.so` — the two original Phase 95b
blockers (install timeout + load timeout) are **cleared** by the Phase 95c work
(zero-copy SHM read-window, larger VFS caps, ext2 block-cache LRU, 3 GiB data disk).
rustc then **NULL-derefs**: a userspace page fault, `addr=0x0` / `CR2=0`, read
(`err=USER_MODE` only), at a **fixed offset inside `librustc_driver.so`** (the
absolute `rip` varies per boot with the DSO load base; observed `0x200a204188` and
`0x10000b50a51` on different boots).

The leading hypothesis was the **dynamic-TLS loader gap** (the from-scratch ld-musl
deferred `DTPMOD64`/`TPOFF64` relocs and only set up the main exe's TLS, so a
DT_NEEDED DSO carrying thread-local storage got no TLS block). That gap is now
**closed** — multi-module static TLS landed in `59cd0c00`, is regression-clean
(`dynamic-hello-smoke` + `cargo xtask check` pass), and the boot log confirms it sets
up `librustc_driver.so`'s `0x5ff0` TLS segment as module 1 and resolves its relocs
(`ldso: tls modules=1 total_off=0x5ff0`). **rustc still crashes at the identical
spot.** So the crash is **NOT** the DSO-TLS gap.

## What is already done (committed + pushed, branch `feat/phase-95b-on-device-rustc`)

| Commit | What |
|---|---|
| `857a7818`, `01a14c4a` | Phase 95c A.2 — VFS read/write caps 64 KiB→256 KiB, `MAX_BULK_LEN`→512 KiB, `MAX_COPY_LEN`→576 KiB (~4× fewer sequential-read IPC round-trips) |
| `b4bc6986` | Phase 95c A.1 — zero-copy SHM read-window demand-fill (no IPC-bulk copy; `vfs_service_handle_for_fd` resolves the real vfs_server handle; legacy fallback for non-VfsService fds) |
| `38a92459` | Phase 95c C — `kernel_core::fs::lru_cache::LruBlockCache`, vfs_server ext2 block cache → LRU (kills fill-and-hold thrash) |
| `f1f731ee` | `M3OS_DATA_DISK_GB` (clamped 1..=64); rustc-smoke uses **3 GiB** (1 GiB ENOSPC'd mid-install after ~330 MiB) |
| `59cd0c00` | **Multi-module static TLS** in ld-musl (DT_NEEDED DSO TLS: `DsoTls`, `assign_tls_modules`, multi-module block + DTV, `TPOFF64`/`DTPMOD64`/`DTPOFF64` resolution). Regression-clean. Does NOT fix rustc. |
| `cdb48a11` | **Loader init-array ABI fix (CANDIDATE FIX for this CR2=0).** ld-musl now passes `(argc, argv, envp)` to every `DT_INIT`/`DT_INIT_ARRAY` constructor (`run_constructors_for`), matching glibc's `dl-init.c::call_init`. Adds `InitFn`, `startup_args_from_stack`, a process-lifetime arg stash (for dlopen-time ctors), + a host unit test. `cargo xtask check` clean, independently reviewed (APPROVE on all ABI/safety axes). NOT yet confirmed against rustc — needs a `rustc-smoke` run. |

Net effect proven on-device: install completes, the 162 MiB DSO demand-pages in and
executes. The runtime NULL-deref is the last thing between here and `RUSTC_OK`;
`cdb48a11` is the leading candidate fix for it.

## The new leading candidate: the init-array ABI gap (commit `cdb48a11`)

**The mechanism.** Rust's `std` registers an `.init_array` entry — `ARGV_INIT_ARRAY`
/ `init_wrapper(argc, argv, envp)` in `std::sys::pal::unix::args` — that captures
`argv`/`envp` into module-level statics so `std::env::args()` works. On the
`x86_64-*-linux-*` targets (musl included, since `target_os = "linux"`) this is the
**only** argv source for a shared `std`. The capture **requires the loader to invoke
init_array entries with `(argc, argv, envp)`**:

- **glibc** does this — `dl-init.c::call_init` calls every init_array entry
  (executable AND shared library) as `void (*)(int, char**, char**)`.
- **musl** does NOT — `do_init_fini` calls library ctors as `void (*)(void)`. A
  static musl Rust binary is unaffected because the *main executable's* init_array
  is run by `__libc_start_main`/`__libc_start_init` with the three args; only the
  exe's std matters there.
- m3OS's from-scratch ld-musl matched **musl** (no args). For a **dynamic
  `prefer-dynamic` rustc**, `std` lives in a shared `libstd-*.so` whose argv-capture
  init_array is run by **the loader**, not `__libc_start_main` — so it got no args →
  the captured `argv`/`envp` statics are null/garbage → the first `std::env::args()`
  (rustc reads its args immediately) derefs null → `CR2=0`, a raw page fault (NOT a
  Rust panic), early in `librustc_driver.so` startup — exactly the observed symptom.

**The fix.** `run_constructors_for` now calls each `DT_INIT`/`DT_INIT_ARRAY` entry as
`InitFn = extern "C" fn(i32, *const *const u8, *const *const u8)` with the real
`(argc, argv, envp)` (derived from the kernel-built SysV startup stack in `dl_entry`,
stashed for dlopen-time ctors). Passing three register args to a `void(void)` C ctor
is harmless on the x86-64 SysV ABI (the callee ignores `rdi`/`rsi`/`rdx`), so every
existing C/musl DSO keeps working — `dynamic-hello-smoke` is the regression guard.

**Confidence + caveat.** This is a textbook glibc-vs-musl loader divergence and the
symptom (raw null deref, no panic, fixed offset in a Rust shared object, early
startup) fits.

## UPDATE 2026-06-23 (later): rustc-smoke RAN — the CR2=0 crash is CLEARED; new wall is cold-load PERF

A full `M3OS_KVM=1 cargo xtask rustc-smoke --timeout 5400` was run on this branch
(`97ea3e89`). Result: **`pkg install rust` PASSED** (~8 min — the 95c VFS work +
KVM; far under the old ~40-min estimate), and **`rustc --version` no longer
crashes**: across the full 1500s step-16 window the guest emitted **zero** crash
markers (no `page fault`, no `addr=0x0`, no `process killed`, no `CR2`, no `PANIC`)
and QEMU stayed **CPU-bound at ~110%** the entire time. Pre-fix, rustc died in
*seconds* (early-startup null deref) and the box idled; post-fix it executes for
25 min straight. **The init-array `(argc,argv,envp)` fix (cdb48a11) cleared the
CR2=0** — strong evidence (sustained execution past the early-crash point + no
fault markers), though not a positive `version 1.96.0` print.

`rustc-smoke` still FAILS — but the failure mode changed from a **crash** to a
**timeout**: `rustc --version` did not finish printing `rustc 1.96.0` within the
1500s budget. The binding constraint is now the **cold-load / relocation /
LLVM-static-init cost of the 162 MB `librustc_driver.so`** (CPU- + page-fault-bound),
not a correctness bug. The wall moved from a hard blocker to a performance problem.

**Next steps (perf, not correctness):**
1. **Confirm slow-but-correct vs. pathology**: re-run with a much larger step-16
   timeout (e.g. 3600s) — does `rustc --version` EVER complete? If yes → purely
   perf; if no even at 60 min → a load-time pathology (O(n²) reloc loop, demand-page
   thrash, or a livelock) to localize.
2. **Localize the cost**: the `ldso` `serial()` is release-suppressed — build the
   loader with logging (or a debug profile) to time relocation vs. demand-paging vs.
   LLVM init; or re-enable the timer-ISR RIP sampler the prior session used.
3. **Levers** (see `docs/roadmap/tasks/95c-vfs-block-io-perf-tasks.md`): **Track B
   kernel file-backed page cache** (zero-IPC re-fault / second-invocation / shared
   `rust`+`rust-lld` pages — the milestone's biggest lever), loader relocation
   throughput (`R_X86_64_RELATIVE` batching / `DT_RELR`), and a larger demand-fault
   readahead cluster for the linear reloc sweep.

(If a future run DOES regress to a hard crash, the original CR2=0 caveat applies —
fall back to the offline-disasm path below; `fs:` prefix ⇒ TLS, plain `mov` ⇒ a
different non-arg null.)

## Hypotheses for the remaining `CR2=0` (ranked)

1. **A non-TLS null pointer in rustc's runtime init** that the TLS-reloc history
   misled us toward. `CR2=0` (exactly 0) is the signature of dereferencing a null
   *pointer-typed value*, which can itself come from an uninitialized/zero TLS
   variable (`mov rax, fs:[X]` loading a pointer that is 0, then `mov rbx, [rax]`).
   Could be an environment expectation rustc has that m3OS does not satisfy
   (an env var, a `/proc` read, a syscall returning 0 that rustc assumes non-null).
2. **A cached `__tls_get_addr` pointer** — if rustc's runtime caches a TLS-block
   pointer computed *before* the static block/DTV is fully wired, it would read the
   wrong (zero) slot. The multi-module DTV is built in `setup_static_tls`; verify the
   ordering vs. first `__tls_get_addr` use.
3. **A subtle multi-module TLS offset/DTV error** that the single TLS-module case
   (`modules=1`) doesn't exercise correctly (e.g. `dtv[1]` vs `dtv[0]` indexing,
   DTP_OFFSET, or the `TPOFF = st_value - tls_offset` sign on variant II). Less
   likely (the math is host-tested and dynamic-hello passes), but `modules=1` with a
   non-main module-1 is a path dynamic-hello does NOT cover.

## The next diagnostic — SAFE and mostly OFFLINE

A loader load-base log is now wired in `userspace/ld-musl-x86_64.so.1/src/main.rs`
(in the DT_NEEDED load loop): each loaded DSO prints
`ldso: loaded <soname> base=<load_bias> len=<image_len>`. So:

1. Run a full `rustc-smoke` (see infra notes), capture serial, and read both the
   crash `rip` AND the `ldso: loaded librustc_driver... base=…` line.
2. **Offline:** `offset = rip - base`. Disassemble the on-host
   `librustc_driver-*.so` (in the rust port build output / the staged `.m3pkg`) at
   that offset: `objdump -d --start-address=<offset> … | head`, or
   `gdb -batch -ex "disas <offset>,<offset>+0x40"`.
   - If the faulting instruction carries an **`fs:` override (opcode prefix `0x64`)**
     → it IS a TLS access → hypothesis 2/3; inspect the TLS reloc value + DTV.
   - If it's a **plain `mov reg,[reg]`** with no fs prefix → a non-TLS null
     (hypothesis 1); trace which register is 0 back through the basic block (and map
     the offset to a symbol with `addr2line`/`objdump -d` to see which rustc/std
     function is in `_start`/runtime init).

This avoids the trap that wasted the last run: an **in-kernel "dump bytes at rip"**
probe in the page-fault handler is UNSAFE — calling `get_mapper()`/`translate_addr`
or reading user memory from that context recurse-faulted the kernel
(`RECURSIVE KERNEL PAGE FAULT cr2=0x8`) and destroyed the signal. Do NOT re-add an
in-kernel instruction read; use the offline disassembly above. (If an in-OS read is
truly needed, do it from a *userspace* helper that mmaps the `.so`, not the kernel.)

## Test-infra gotchas (these cost hours last time)

- **`cargo xtask clean` between rustc runs.** A failed rustc run leaves m3OS idling
  at the shell with the ext2 disk mounted **rw**; the smoke harness does NOT kill
  that QEMU. Killing the orphan (`kill <pid>`) mid-mount **corrupts `disk.img`**, and
  the next boot is **firmware-only** (`=3h=3h…`, no kernel banner). `cargo xtask
  clean` deletes `disk.img` and recovers it (confirmed: dynamic-hello booted clean
  afterward).
- **Do NOT set `M3OS_SERIAL_LOG` for these runs.** The serial tee races the harness's
  step-1 "kernel first message" detection → spurious `QEMU exited while waiting for
  step 1` even though the kernel booted fine (you'll find the orphan QEMU alive). Run
  *without* it and rely on the harness's own trace-ring dump — BUT note the next bug:
- **dhcpv6 retransmit spam floods the failure buffer.** The guest's IPv6 stack
  retransmits an Information-Request ~1×/s forever (no DHCPv6 server), and after a
  userspace crash the kernel stays alive emitting it, so the harness's "last serial
  output" is all `[dhcpv6] retransmit` and the crash line scrolls off. To capture the
  crash reliably, either (a) grep the full serial (accepting the step-1 flake risk +
  retrying), or (b) quiet the dhcpv6 retransmits for the diagnostic, or (c) the
  offline-disassembly path above, which only needs the `rip` + `base` (both printed
  once, near the crash).
- **`M3OS_RUST_FAST_ITER=1` reuses `disk.img`** (skips the ~30 min install) — only
  valid when a *prior* run left a COMPLETE install (the install step passed). A
  half-installed or corrupted disk → `ion: command not found: rustc`.

## Reproduction

```
cargo xtask clean
M3OS_KVM=1 cargo xtask rustc-smoke --timeout 5400   # full: build + 3 GiB disk + install + cold-load
```
Expect: `pkg install: rust: OK` (install completes), then `rustc --version` crashes
(`userspace page fault … addr=0x0 … rip=0x… — process killed`). The
`ldso: loaded librustc_driver… base=…` line is in the same boot's serial.

## Relevant code

- `userspace/ld-musl-x86_64.so.1/src/main.rs` — `setup_static_tls` (multi-module
  block + DTV), `assign_tls_modules`, the `TPOFF64`/`DTPMOD64`/`DTPOFF64` reloc arms,
  `tls_module_for_reloc`, and the new `ldso: loaded …` log.
- `userspace/ld-musl-x86_64.so.1/src/tls.rs` — `next_tls_offset` (variant-II), host
  tests.
- `userspace/ld-musl-x86_64.so.1/src/sym.rs` — `lookup_tls`.
- `xtask/src/main.rs` — `cmd_rustc_smoke`, `create_data_disk` (`M3OS_DATA_DISK_GB`).
- The rust port: `ports/lang/rust/`, `build_rust` in `xtask/src/port_build.rs`.
