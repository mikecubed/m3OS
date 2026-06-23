---
status: OPEN — crash LOCALIZED to `libc.so + 0x28188`; the original `CR2=0` is NOT
  fixed (CORRECTION below). `rustc` loads + runs LLVM (paged ~16 MiB) then NULL-derefs
  (`addr=0x0`, USER_MODE) at userspace rip `0x200a204188` = **`libc.so` vaddr
  `0x28188`** — the SAME rip the original diagnosis recorded (mis-attributed then to
  `librustc_driver.so`; with runtime bases `libc.so=0x200a1dc000` /
  `librustc_driver=0x2000010000`, the rip is in `libc.so`). CORRECTION: the loader
  init-array `(argc,argv,envp)` fix (`cdb48a11`, glibc-ABI) did NOT clear this crash;
  the earlier "cleared" claim was a MEASUREMENT ERROR — the rustc-smoke cargo log
  carries NO guest serial unless `M3OS_SMOKE_SERIAL_DUMP` is set, so the
  `process killed` line was invisible and the ~25-min CPU-bound time was the headless
  GUI servers busy-looping AFTER rustc died, not rustc running. The init-array fix is
  still a correct glibc-ABI improvement (keep it; rustc now reaches LLVM before
  crashing) but is necessary-not-sufficient. NEXT: disassemble `libc.so+0x28188`
  (`fs:` TLS access vs plain null) — in progress. ---OLD-STATUS-BELOW---
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

## UPDATE 2026-06-23 (latest): crash LOCALIZED to `libc.so+0x28188` — and the "CLEARED" claim was WRONG

**RETRACTION.** An interim update here claimed the init-array fix "cleared the
CR2=0". That was a **measurement error**, now corrected. The `rustc-smoke` cargo log
carries only the harness `[step N]` markers — it does **NOT** echo guest serial
unless `M3OS_SMOKE_SERIAL_DUMP=<path>` is set (see `run_smoke_script` in
`xtask/src/main.rs`; `dump_serial` writes the full history only at a terminal
point — timeout/error/end). So grepping the cargo log for `process killed` found
nothing because the guest serial was never there. And the "~25-min CPU-bound at
110–366%" was the **headless GUI servers busy-looping AFTER rustc died** (the prior
session's `READ_KBD_SCANCODE`/`FRAME_TICK_DRAIN` spin), not rustc running.

**How to actually see it.** Run with the serial dump and grep with `-a` (the dump
has control bytes):
```
cargo xtask clean
M3OS_SMOKE_SERIAL_DUMP=/tmp/s.txt M3OS_KERNEL_FEATURES=rustc-profile \
  M3OS_KVM=1 cargo xtask rustc-smoke --timeout 5400 &
# after install + a few min of rustc load, `pkill -TERM -x qemu-system-x86` to
# force dump_serial, then:
grep -a 'process killed\|\[pf\]\|loaded .* base=' /tmp/s.txt
```

**What the dump shows (clean disk, KVM):**
- `[int] userspace page fault: pid=43 addr=Ok(VirtAddr(0x0)) err=USER_MODE`
  `rip=0x200a204188 — process killed`.
- Loader bases (the loader DOES log them; they're just invisible without the dump):
  `librustc_driver-….so base=0x2000010000 len=0xa1bc000`, `libc.so base=0x200a1dc000
  len=0xb3000`. So **`rip 0x200a204188 = libc.so + 0x28188`** (NOT librustc_driver —
  the original diagnosis mis-attributed it; `0x200a204188 > driver_end 0x20081cc000`).
- The crash rip `0x200a204188` is **identical** to one the original diagnosis
  recorded → same crash, **unchanged by the init-array fix**.

**The `rustc-profile` kernel feature (added this pass; off by default, zero prod
residue)** confirms rustc *does* run before crashing — a `[pf]` heartbeat at the #PF
handler top (`kernel/src/arch/x86_64/interrupts.rs::rustc_prof`, every 32 faults)
shows: ~2300 anonymous faults at loader rip `0x244cc3` (the streaming PT_LOAD
read into anon, ~7 MiB), then `demand_pages` climbs 0→~4115 (~16 MiB file-backed) as
LLVM executes (rip `0x40002xxx`), THEN the libc.so null-deref. So rustc reaches LLVM
execution — this is a real null-deref ~16 MiB in, **not** a slow-load/throughput
problem (only ~16 MiB paged before the crash).

## ROOT CAUSE (disassembled + verified): `__libc.auxv` is NULL → mallocng first-malloc crash

Disassembly of `libc.so` (musl 1.2.5, `target/port-stage/musl/usr/lib/libc.so`) at
vaddr `0x28188`:
```
28175:  mov    0xaf8c8(%rip),%rdx     ; rdx = __libc.auxv   (the global; it is NULL)
28188:  mov    (%rdx),%rax            ; FAULT: deref __libc.auxv == NULL  (addr 0x0)
...     (loop scanning auxv for a_type==0x19 AT_RANDOM, 16-byte stride → memcpy 16 bytes)
```
- **NOT TLS** — no `fs:` prefix; a plain dereference of the global `__libc.auxv`
  (at libc.so vaddr `0xaf8c8`; neighbour `0xaf8c3` = `__libc.secure`).
- The faulting function is musl **mallocng's secret-init** (`get_random_secret` /
  `alloc_meta` path, `src/malloc/mallocng/{meta.c,glue.h}`), which runs on the
  **first `malloc`** and seeds the allocator from `AT_RANDOM`.
- `__libc.auxv`'s ONLY writer is `__init_libc` (`0x1df89: mov %rax,0xaf8c8(%rip)`),
  called ONLY from `__libc_start_main` (`0x1e1bc`).

**Why NULL on m3OS:** for a dynamic program, `__init_libc` is the **ld.so's**
responsibility (musl's own `__dls2`/`__dls3`), and even via `__libc_start_main` it
runs only at app entry — but the m3OS Rust loader **replaces the ld.so and never
calls `__init_libc`**, and it runs the DSO constructors (`run_constructors`) BEFORE
the app's `__libc_start_main`. librustc_driver's LLVM static ctors `malloc` during
`run_constructors` → mallocng secret-init → walk NULL `__libc.auxv` → fault.
**Same foreign-loader-NULL-global class already patched twice** (`main_ctor_queue`
patch `0001`, `__init_tp` un-hide patch `0002`); `__libc.auxv` is the third.

**THE FIX (loader must do the ld.so's `__init_libc` job before constructors):**
The loader already has `argc/argv/envp` (the init-array fix stashed them in
`STARTUP_ARGV`/`STARTUP_ENVP`). Two options, ranked:
1. **Un-hide + call `__init_libc(envp, argv[0])`** (preferred — it's the canonical
   function; sets `auxv` + `page_size` + `secure` + `environ` + `hwcap`). Add
   `ports/lib/musl/patches/0003-export-init-libc.patch` mirroring `0002` (drop
   `hidden` from `__init_libc`'s decl in `src/internal/libc.h`), then in
   `userspace/ld-musl-x86_64.so.1/src/main.rs` `sym::lookup(b"__init_libc")` and call
   it **after `__init_tp` / `setup_static_tls`, BEFORE `run_constructors`**. RISK:
   `__init_libc` calls `__init_tls(aux)` — verify that resolves to the **no-op weak
   stub** in this shared libc.so (the handoff already relies on `static_init_tls`
   being a no-op stub, so this is expected safe); if it instead redoes TLS and
   conflicts with the loader's multi-module `setup_static_tls`, fall back to option 2.
2. **Minimal exported setter** (lower risk): patch musl to add an exported
   `void __m3os_set_auxv(size_t *auxv)` that sets `libc.auxv = auxv` + derives
   `libc.page_size` from `AT_PAGESZ` (mallocng needs both), `__attribute__((visibility
   ("default")))` so it survives `-fvisibility=hidden`; loader calls it with the auxv
   pointer (`= envp` walked to NULL, +1). No TLS interaction.

**Confirming insight — why `dynamic-hello-smoke` passes despite `malloc`ing:** its
malloc is in `main`, which runs AFTER the app's `crt1 _start → __libc_start_main →
__init_libc` (auxv set). rustc crashes because its **first** malloc is in an LLVM
**static constructor**, which the loader runs (`run_constructors`) BEFORE the app's
`__libc_start_main`. So the fix MUST set `__libc.auxv` before `run_constructors`.
This also means option 1 would run `__init_libc` **twice** (loader + the app's
`__libc_start_main`); the auxv/environ/page_size sets are idempotent, but the
`__init_tls(aux)` inside `__init_libc` is NOT obviously safe to run twice (verify it
resolves to the no-op weak TLS stub this libc.so uses; if not, **prefer option 2**,
the auxv+page_size-only setter, which is idempotent and TLS-free).

**Loader insertion point:** `userspace/ld-musl-x86_64.so.1/src/main.rs`, in
`dl_entry` between the `store_startup_args(...)` block (~line 2894) and
`run_constructors(&dsos)` (~line 2905). Use `sym::lookup(dsos.as_slice(), b"__init_libc"
/* or the setter */, None)` (mirrors the `__init_tp` lookup at ~line 2356), transmute
to `extern "C" fn(*const *const u8, *const u8)` (envp, argv[0]) / the setter's sig,
and call it; skip gracefully if the symbol is absent (un-patched libc — no regression).

**Validation:** musl rebuild (`cargo xtask port build musl`) → `M3OS_WITH_RUST` image
→ `cargo xtask clean` → `M3OS_SMOKE_SERIAL_DUMP=/tmp/s.txt M3OS_KERNEL_FEATURES=rustc-profile
M3OS_KVM=1 cargo xtask rustc-smoke --timeout 5400`; after the install + a few min of
rustc load, `pkill -TERM -x qemu-system-x86` to force the dump, then `grep -a` for
`process killed` (should be GONE), a higher `[pf] demand_pages` (rustc gets further),
and ideally `rustc 1.96.0`. Use a CLEAN disk (NOT `M3OS_RUST_FAST_ITER`, which
recursive-faults on a reused disk). The 95c throughput levers are NOT on the
`RUSTC_OK` critical path — this correctness crash is the blocker.

(A kernel-mode fault was also seen once — `InterruptStackFrame ip=0x10000b53f61
rpl:Ring0` reading `0x0` — likely a secondary effect of the user crash or a
`rustc-profile` interaction; revisit only if it recurs after the `__libc.auxv` fix.)

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
