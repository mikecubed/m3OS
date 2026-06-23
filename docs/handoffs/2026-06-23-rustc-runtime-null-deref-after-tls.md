---
status: OPEN — diagnosis narrowed, root cause NOT yet found. The on-device `rustc`
  (Phase 95b RUSTC_OK milestone) cold-loads + relocates + executes
  `librustc_driver.so` but NULL-derefs (`CR2=0`) early in startup. The multi-module
  static-TLS loader gap that was the leading hypothesis is now CLOSED (commit
  `59cd0c00`) and ruled OUT — rustc crashes identically with the TLS gap fixed. The
  remaining blocker is a separate, non-TLS crash in rustc's runtime init. A safe,
  offline next-diagnostic is wired up (a loader load-base log + disassembly); no
  more risky in-kernel probes.
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

Net effect proven on-device: install completes, the 162 MiB DSO demand-pages in and
executes. Only the runtime NULL-deref remains between here and `RUSTC_OK`.

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
