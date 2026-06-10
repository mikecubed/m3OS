# Rust on m3OS: `std` Userspace, Dynamic libc, and a Native Toolchain

**Aligned Roadmap Phase:** Phase 44 — Rust Cross-Compilation (this doc), with
forward dependencies on Phase 93 (Dynamic C Runtime), Phase 94 (Rust-Cargo
Ports), and Phase 95 (Native Rust Toolchain)
**Status:** Phase 44 complete; re-evaluated against the current tree (post-85d /
86d). Residual gaps and the path to a native toolchain tracked here.
**Source Ref:** phase-44
**Related:** [Phase 44 design doc](../roadmap/44-rust-cross-compilation.md),
[Phase 93 — Dynamic C Runtime](../roadmap/93-dynamic-c-runtime.md),
[Phase 94 — Rust-Cargo Ports](../roadmap/94-rust-cargo-uutils.md),
[Phase 95 — Native Rust Toolchain](../roadmap/95-native-rust-toolchain.md),
[`architecture-and-syscalls.md`](./architecture-and-syscalls.md)

> **Note on citations.** This doc cites by **file + symbol** rather than line
> number. The original Phase-44 revision used precise line numbers and every one
> of them had drifted by the time of this re-evaluation; symbol-anchored
> citations survive refactors. Where a syscall landed in a specific later phase,
> the phase is named so the claim stays true regardless of where the code
> currently sits.

## Three Distinct Goals (read this first)

"Support Rust on m3OS" is really three separate problems at very different
maturities. Keeping them apart is the single most important thing this doc does:

| Goal | What it means | Status | Gating work |
|---|---|---|---|
| **A. Run Rust `std` *programs*** | A Rust binary compiled on the host runs in m3OS | ✅ **Done** (Phase 44), and far sturdier than the original doc claimed | This doc + Phase 94 extends it to cargo-cross ports |
| **B. Dynamic libc** | A real `libc.so` exists so dynamic objects (incl. proc-macro `.so`s) can bind | 🟡 Linker machinery done (Phase 76 → 76d); **no `libc.so` yet** | **Phase 93** (Planned) |
| **C. Run the Rust *toolchain* on-device** | `rustc`/`cargo` compile Rust *inside* m3OS | ❌ Not started | Phase 95 (needs B for proc-macros) |

The common confusion is to treat C as a small extension of "Go runs on m3OS"
(Phase 86d). It is not: Phase 86d only runs a *pre-built* Go binary — the Go
compiler never runs on the device. The correct precedent for C is **Clang/LLVM**
(Phase 85d), the project's only on-device native code generator. See
[Phase 95](../roadmap/95-native-rust-toolchain.md).

## Overview

m3OS supports Rust `std` in userspace through a deliberate **Linux ABI shim**
strategy: Rust programs are cross-compiled on the host with
`--target x86_64-unknown-linux-musl`, statically linked against musl, and
delivered into m3OS (the ramdisk for the Phase 44 demos; a `.m3pkg` for Phase 94
ports). The kernel's Phase 12 Linux compatibility layer translates the resulting
Linux syscalls into m3OS operations. There is no `std::sys::m3os` backend, no
in-tree port of musl as a shared library, and no runtime `libc.so` — every
static binary brings its own statically linked musl. This document records the
closure state of that pipeline, the residual syscall and runtime gaps (many now
closed by the C/Go/Python toolchain bring-ups), and the recommended approach for
closing what remains, up to a native on-device toolchain.

## Why a Shim Instead of a Native Backend

Two strategies were considered for Rust `std` support:

1. **Native std backend (`std::sys::m3os`).** Fork `rust-lang/rust`, write a
   target-specific backend module against the `syscall-lib` crate, and ship a
   custom sysroot. Maintenance burden is per-Rust-release: every stable bump
   requires re-vendoring `library/std`, re-implementing any new internal traits,
   and shipping a sysroot tarball.
2. **Linux ABI shim through musl.** Use the upstream
   `x86_64-unknown-linux-musl` target, which already has a maintained `std`
   backend. The kernel implements enough of the Linux x86_64 syscall ABI for
   musl's CRT and `std`'s common paths to run.

Phase 44 picked the second path, reusing the Linux syscall surface already built
for musl-linked C programs (Phase 12), `clone(CLONE_THREAD)`/futex (Phase 40),
TLS via `arch_prctl` (Phase 40), networking (Phase 23), and I/O multiplexing
(Phase 37). The cost was roughly *one xtask function plus five demo crates*
rather than a fork of the Rust compiler.

The trade-off is a permanent dependency on Linux syscall numbering and struct
layouts. m3OS-native syscalls (kernel-extension space `0x1000+`, including the
IPC dispatch range) remain reachable from `no_std` programs through
`userspace/syscall-lib/`, but `std` programs cannot see them.

## Two Compilation Paths in m3OS

| Path | Target | Library surface | Use when |
|---|---|---|---|
| musl `std` | `x86_64-unknown-linux-musl` | Full `std` via musl libc (static) | Porting crates, threads/networking ergonomics, anything `std`-shaped |
| Native `no_std` | `x86_64-unknown-none` (default) or `x86_64-m3os.json` | `core` + `alloc` + `userspace/syscall-lib/` | Tiny binaries, m3OS-native syscalls (IPC, framebuffer), kernel-adjacent code |

The two paths coexist in the same kernel build and do **not** share crates: the
workspace default target is `x86_64-unknown-none` and the musl crates are
non-workspace members (each musl crate's `Cargo.toml` opens with `[workspace]`
to detach it).

`x86_64-m3os.json` at the project root is the **live Rust userspace
hardware-float target** as of Phase 86f. It carries `-mmx,+sse,+sse2,+aes`
(SSE/SSE2 + AES-NI; `+soft-float` removed), `disable-redzone: true`, and
`panic-strategy: abort`. It deliberately keeps `"os": "none"` (matching the
built-in `x86_64-unknown-none`) so `#[cfg(target_os = "none")]` stays correct
across both targets and shared crates (e.g. `driver_runtime`) keep their
existing `cfg` gates without a host-`std` fallback regression — there is **no**
`target_os = "m3os"`, so do not branch on one. `xtask`'s `build_userspace_bins`
points all three userspace `--target` invocations at it. The **kernel** stays on the built-in
`x86_64-unknown-none` (soft-float, `-sse`) — the two are deliberately
decoupled. (Phase 95 still needs a separate userspace *std* target spec for a
native on-device `rustc`; `x86_64-m3os.json` is `no_std`, not a `std` sysroot.)

## What Phase 44 Shipped (and where it is now)

### The five demonstration crates

Located under `userspace/`, each a non-workspace musl `std` crate:

| Crate | std surface | What it proves |
|---|---|---|
| `hello-rust` | `println!` | musl CRT + entry, `write` syscall, exit |
| `sysinfo-rust` | `std::fs::read_to_string` | tmpfs/procfs read path through musl |
| `httpd-rust` | `std::net::TcpListener` | socket + bind + listen + accept + send |
| `calc-rust` | `std::io::stdin().read_line()` | TTY line discipline through musl |
| `todo-rust` | `std::fs::write` round-trip | persistent ext2/FAT32 write path |

All five remain in the tree, unchanged, with `opt-level = "z"`, `lto`, `strip`,
`panic = "abort"` release profiles. No new `std` demo crates were added.

### xtask integration

`build_musl_rust_bins()` in `xtask/src/main.rs` owns the musl Rust build:

- Stages a zero-length placeholder under `target/generated-initrd/<name>` for
  each crate so the kernel's `include_bytes!` path always resolves.
- Probes `rustup target list --installed`; warns and bails if
  `x86_64-unknown-linux-musl` is missing, leaving placeholders behind.
- Invokes `cargo build --target x86_64-unknown-linux-musl --release` per crate,
  with `RUSTFLAGS="-C relocation-model=static -C target-feature=+crt-static"`.
  The static relocation model produces ET_EXEC binaries (not PIE) so the kernel
  ELF loader doesn't collide with musl's self-relocating CRT.
- `apply_musl_cargo_env()` now sets `CC_x86_64_unknown_linux_musl` /
  `AR_x86_64_unknown_linux_musl` from the detected musl cross-toolchain (with a
  host-`ar` fallback) — needed for any crate with a `cc`-built build-script dep.
  *(This wiring postdates the original Phase-44 doc.)*
- Runs `strip` on each output before staging.

It is wired into the kernel-build, `image`, and `run` paths so every artifact
picks up the Rust `std` binaries.

### Ramdisk embedding

`kernel/src/fs/ramdisk.rs` defines five static byte slices via the
`generated_initrd_asset!` macro pointing at `target/generated-initrd/`, and a
`BIN_ENTRIES` table maps them to `/bin/<name>` so `execve` resolves them.

### The newer Rust port path (Phase 94)

Phase 44's pipeline embeds binaries in the ramdisk. **Phase 94** (Planned) adds
the first Rust-*cargo* cross-build *port* class: `build_uutils()` in
`xtask/src/port_build.rs` cross-compiles a std crate for
`x86_64-unknown-linux-musl` (prebuilt-std, **self-contained** — `rust-lld` +
bundled musl, so a pure-Rust crate needs no external musl-gcc) and ships it as a
`.m3pkg` installed by `pkg install`. It is still **host** cross-compilation; it
does not compile on-device. It is the natural successor to the Phase-44 lineage
for new Rust userspace work.

## The Linux ABI Boundary

A musl-linked Rust `std` binary, before `main()`, issues roughly:
`set_tid_address`, `brk`, anonymous `mmap`, `rt_sigaction`, `rt_sigprocmask`,
`arch_prctl(ARCH_SET_FS, …)`, optionally `prlimit64`, `sigaltstack`,
`getrandom`. m3OS now implements all of that startup set (including
`prlimit64`, closed in Phase 85d — see below). After startup, common `std`
paths map to the families in the table below — what musl *emits*, not what Rust
source calls.

| `std` API | musl-emitted syscalls | m3OS status |
|---|---|---|
| `File::open` | `openat` + `fcntl(F_GETFD/F_SETFD)` | Works |
| `read_to_string` | `openat` + `fstat` + `read` loop + `close` | Works |
| `fs::write` | `openat(O_WRONLY\|O_CREAT\|O_TRUNC)` + `write` + `close` | Works on writable mounts |
| `read_dir` | `openat(O_DIRECTORY)` + `getdents64` loop + `close` | Works |
| `metadata` | `fstatat` / `newfstatat` | Works (`statx` absent; `std` uses `fstatat`) |
| `set_permissions` | `fchmodat` / `chmod` | Works |
| `rename` | `renameat2` → `renameat` | `renameat` works; `renameat2` flags partly honored |
| `TcpListener::bind` | `socket` + `setsockopt(SO_REUSEADDR)` + `bind` + `listen` | Works |
| `accept` | `accept4(SOCK_CLOEXEC)` / `accept` | Works |
| `TcpStream::connect` | `socket` + `connect` | Works (non-blocking `connect` added Phase 86b) |
| `UdpSocket` | `socket(SOCK_DGRAM)` + `sendto`/`recvfrom` | Works |
| `thread::spawn` | `mmap` stack + `clone(CLONE_THREAD\|…\|CLONE_SETTLS\|…)` + `set_tid_address` | Works (Phase 40) |
| `Mutex`/`Condvar`/`Once` | `futex(WAIT/WAKE)` | Works (`kernel/src/process/futex.rs`; CHILD_CLEARTID lost-wakeup fixed Phase 77) |
| `process::Command` | `clone(SIGCHLD)`/posix_spawn → `execve` → `waitpid` | Works |
| `env::args`/`vars` | startup user stack from `execve` | Works |
| `Instant`/`SystemTime` | `clock_gettime(MONOTONIC/REALTIME)` | Works |
| `stdin().read_line()` | `read(0, …)` against the TTY (cooked) | Works |
| Raw terminal | `ioctl(TCGETS/TCSETS)` | Works (Phase 22 / 69a termios) |
| File-backed `mmap` (e.g. `memmap2`) | `mmap(fd, …)` | **Now works** (Phase 47 eager-load) |

## Syscall Gaps — Re-evaluated

The original doc's gap list predates the Clang (85d), Go (86d), and earlier
memory (47) work that closed several entries. Current status:

### Now closed (were gaps in the Phase-44 doc)

| Syscall | Closed by | Note |
|---|---|---|
| `prlimit64` (302) / `getrlimit` (97) | **Phase 85d** | `sys_prlimit64` accepts-and-ignores `new_rlim`, writes generous defaults; LLVM-class tools probe rlimits |
| File-backed `mmap` (was anonymous-only) | **Phase 47** (Strategy A, eager-load) | `MAP_PRIVATE`/`MAP_SHARED` file-backed; demand-paged Strategy B still deferred |
| `pread64` (17) / `pwrite64` (18) | **Phase 85d** | Positional I/O — "THE compile blocker" for LLVM; `pwrite64` non-atomicity tracked in Phase 88 |
| `epoll_pwait` (281) | **Phase 86d** | Delegates to `sys_epoll_wait` (Go passes a nil sigmask) |
| `eventfd2` (290) | **Phase 86d** | Full `EFD_SEMAPHORE`/`EFD_NONBLOCK`/`EFD_CLOEXEC` + epoll/poll wakeup |
| `MAP_FIXED` arena commit + `PROT_NONE` reservations | **Phase 86d** | The Go runtime's reserve-then-commit arena contract |
| `tgkill` (234) / `SIGURG` async preempt | **Phase 86d** | `sys_tkill` delivery; Go's `doSigPreempt` |
| `SA_SIGINFO` ucontext | **Phase 86d** | Handler gets `RSI=&siginfo`, `RDX=&ucontext` so `gregs[RIP]` is readable |

*(The Phase 86d entries land with the Go-runtime branch; they are attributed by
phase here because they may not yet be on `main` when this doc is read.)*

### Still open (the original doc is correct)

| Syscall family | Status |
|---|---|
| `splice` / `vmsplice` / `tee` | Not implemented. `std` does not use them; high-perf I/O falls back to `read`/`write`. |
| `statx` | Not implemented. `std::fs::metadata` uses `fstatat` and works. |
| `io_uring_*` | Not implemented. Async runtimes fall back to `epoll`. |
| `pidfd_*` | Not implemented. `std::process` does not require it. |
| `prctl` / `personality` | Partial / stubs. Sandboxing libraries, not `std`. |
| `utimensat` on **tmpfs** | `-ENOSYS` on tmpfs; **works on ext2** (sets atime/mtime/ctime). See timestamps below. |
| `mremap` (25) | Not implemented. musl `realloc` of large mmap chunks falls back to map-copy-unmap; **explicitly in Phase 93 scope** (a dynamic libc exercises it). |

### Catch-all behavior

Unrecognized syscalls fall through to the default dispatcher arm, which logs
`unhandled syscall N (args: …)` and returns `-ENOSYS`. The recommended
crate-porting workflow is unchanged: run once with serial logging on, grep for
`unhandled syscall`, decide implement-vs-stub per number. This catch-all remains
the project's primary syscall-gap discovery mechanism.

## Threading and TLS Coverage

`std::sync::{Mutex, Condvar, OnceLock}` and `std::thread::spawn` work:

- `clone(CLONE_THREAD | CLONE_VM | CLONE_SIGHAND | CLONE_SETTLS | …)` creates a
  sibling thread sharing address space, signal handlers, and fd table.
- `futex` with real `FUTEX_WAIT`/`FUTEX_WAKE` on anonymous user memory
  (`kernel/src/process/futex.rs`); the Phase 77 `CHILD_CLEARTID` lost-wakeup fix
  is the load-bearing correctness patch for thread-exit.
- TLS via `arch_prctl(ARCH_SET_FS, addr)`; FS base saved/restored per thread.
- `set_tid_address` records `clear_child_tid`; `gettid` + `tkill`/`tgkill`
  target a specific thread.

> **Important distinction for dynamic linking:** this is *static-binary* TLS
> (the kernel sets `FS` for a statically-linked musl thread). It is **not**
> dynamic-loader TLS — the from-scratch `ld-musl` loader has **no** TLS-block /
> `TPOFF`/`DTPMOD` handling. That gap is a Phase 93 prerequisite and the
> dominant risk there (see Dynamic Linking below).

Atomics work: the target spec sets `max-atomic-width: 64` and cores have native
atomics.

## Filesystem Coverage Notes

- **tmpfs timestamps — still unimplemented.** The tmpfs node structs
  (`kernel-core/src/fs/tmpfs.rs`: `FileData`/`DirData`/`SymlinkData` and
  `TmpfsStat`) carry **no** `atime`/`mtime`/`ctime` fields, consistent with
  `utimensat` returning `-ENOSYS` on tmpfs. Timestamps that round-trip through
  tmpfs reset to a default. ext2 tracks and honors them.
- **`/proc`.** Backed by tmpfs entries the kernel populates (`pid/stat`,
  `pid/fd`, `pid/cmdline`, and the `pid/task` subtree that Phase 77 added for
  `htop`). `read_to_string("/proc/…")` works for these; full procfs is not a
  goal.
- **`stat` identity.** Phase 85d fixed the acute `fstat`-by-fd `st_ino=0` bug
  (recursive-`#include` dedup collapse). The systemic pass — one canonical
  `fill_stat()` serializer, same `(st_dev, st_ino)` by path or fd, kernel-ext2
  vs `vfs_server` reconciliation — is **Phase 88** (and matters for `cargo`'s
  build-graph correctness, hence a Phase 95 dependency).
- **Path lookup.** `openat(AT_FDCWD, …)` and relative-to-dirfd lookups work.

## Networking Coverage Notes

- TCP/UDP work end-to-end via the BSD socket family (Phase 23). `std::net` needs
  no language-level shims; musl wraps the option-name translation.
- `accept4(SOCK_CLOEXEC | SOCK_NONBLOCK)` honored; bare `accept` works.
- `epoll_create1`/`epoll_ctl`/`epoll_wait` (+ `epoll_pwait`, Phase 86d) and
  `select` work for non-blocking flows. Non-blocking `connect`
  (`EINPROGRESS`/`POLLOUT`/`getsockopt(SO_ERROR)`) landed in Phase 86b.
- **DNS is userspace, not a kernel feature.** Resolution runs through musl's
  stub resolver (`getaddrinfo` → `socket(AF_INET, SOCK_DGRAM)` → kernel UDP →
  the `/etc/resolv.conf` nameserver, e.g. the QEMU SLIRP virtual DNS). Two kernel
  *enablers* postdate the original doc: `recvmsg` on AF_INET (musl's
  `__res_msend` drains replies with it) and a wildcard ephemeral-UDP bind fix
  (Phase 86b) so a second consecutive `getaddrinfo` no longer hits `EADDRINUSE`.
  `ToSocketAddrs` on a literal IP needs none of this.
- **TLS now exists — in userspace, not `std`.** Phase 86c shipped a static
  mbedTLS 3.6.2 + libcurl chain (SIMD-off-safe C crypto) used by `git` over
  HTTPS. Pure-Rust `rustls` was deferred but is plausible on musl-`std`: the
  entropy (`getrandom`) and socket substrate it needs are all present. The point
  the original doc made — "TLS not in `std`" — is still literally true, but TLS
  is no longer absent from the OS.

## Memory and Allocator Notes

- musl ships `mallocng`; m3OS does not override the allocator. Heap growth uses
  `brk` (honored); thread stacks get guard pages during `clone`.
- No `mmap`-flag divergence has bitten the demo/port set, but a real dynamic
  libc will exercise `mremap` and tighter `MAP_*`/`PROT_*` flag coverage
  (Phase 93).
- Binary size: a stripped/LTO'd musl `std` hello-world is ~300 KB; the five
  demos add ~1.5–2 MB to the initrd. (Phase 94 ports ship as on-disk `.m3pkg`s,
  not initrd, so size pressure there is the install/VFS cost, not the kernel ELF.)

## Dynamic Linking and a Real `libc.so` (Phase 93)

The original doc said: *"All Rust `std` programs are statically linked against
musl by design and there is no plan to support `.so` loading."* **Both halves
are now out of date.**

**The dynamic-linker machinery is done** (Phase 76 → 76d). The from-scratch Rust
loader `ld-musl-x86_64.so.1` (`userspace/ld-musl-x86_64.so.1/`, library
`ldso_core`) implements: PT_INTERP handoff, `DT_NEEDED` topo-resolution with
cycle detection, the four x86-64 reloc types (`RELATIVE`/`GLOB_DAT`/`JUMP_SLOT`/`64`),
`dlopen`/`dlsym`/`dlclose`/`dlerror`, PLT lazy resolve via `_dl_runtime_resolve`,
`DT_GNU_HASH`, symbol versioning, and `LD_BIND_NOW`. It enforces W^X (text `R-X`,
GOT `RW-`).

**But there is no `libc.so` for it to bind against.** Phase 85c discovered this
by running CPython: the loader parses `DT_NEEDED libc.so`, opens `/usr/lib/libc.so`,
and gets `-ENOENT`. m3OS *split* the loader from the C library (on real musl they
are one file); the loader exports only `_start`/`_dlstart`, so even symlinking
`libc.so` to it leaves every `malloc`/`memcpy`/`__errno_location` undefined.
Every on-device toolchain — `git`, Python, Clang, Go — is shipped **fully
static** precisely to dodge this. The synthetic test `.so`s (`libhello.so`, the
cyclic pair, the GNU-hash/versioned demos) reference *no* libc symbols, which is
why the gap stayed hidden until CPython.

**Phase 93 (Dynamic C Runtime, Planned)** closes it:
- **Area A** — ship a real upstream musl `libc.so` at `/usr/lib/libc.so`
  (recommended: make `/lib/ld-musl-x86_64.so.1` *be* upstream musl).
- **Area B** — close the syscalls a dynamic libc invokes at startup; `mremap`
  is the known-missing one.
- **Area C** — re-enable dynamic CPython + `lib-dynload` + `ctypes`/`libffi`.

The hard part is not the file — it is the **loader extensions a real libc
forces** that the synthetic libs never exercised: **TLS** (general-dynamic,
`TPOFF`/`DTPMOD`, loader-side `arch_prctl`), **copy relocations**, and
**IFUNC**. TLS is the dominant risk: unstarted, no owning phase, required by both
`__libc_start_main` and all of Rust `std`. Effort: shipping the libc + `mremap`
is low–medium; TLS is high; copy-relocs/IFUNC medium-high.

**Why this is on the Rust critical path:** standard Rust **proc-macros**
(`serde_derive`, `thiserror`, …) are `cdylib` `.so`s that `rustc` **`dlopen`s at
compile time**. With no `libc.so` in scope, every external relocation in that
`.so` is undefined and the load fails. So a native on-device `rustc` could
compile *proc-macro-free* crates without Phase 93, but the mainstream ecosystem
needs Phase 93's `libc.so` + loader TLS first. This is the same wall clang,
Python, and Go all hit — harder here, because proc-macros are pervasive.

## Native On-Device Toolchain (Phase 95)

Running `rustc`/`cargo` **on m3OS** (as opposed to cross-compiling on the host)
is its own phase. The full design is in
[Phase 95](../roadmap/95-native-rust-toolchain.md); the essentials:

- **Precedent is Clang (85d), not Go (86d).** rustc is an LLVM-based code
  generator; Go 86d only runs a pre-built binary.
- **Already solved by the clang bring-up** (free for rustc): huge-static-binary
  exec (streaming ELF loader + 512 MiB cap), positional I/O, `prlimit64`,
  `fstat` identity, the `.m3pkg` delivery + `M3OS_WITH_*` opt-in, and a working
  on-device **LLD** (rustc's default `rust-lld`).
- **New / hard:** a fully-static musl `rustc` bootstrap; a **userspace** target
  spec + prebuilt std sysroot (the kernel `x86_64-m3os.json` is unusable for
  userspace); proc-macro support (gated on Phase 93); cargo registry + `build.rs`;
  and absorbing a 200–500 MB artifact through the ~200 KB/s ring-3 VFS
  (mitigated by Phase 87).
- **Smaller alternative:** `mrustc` (a C++ Rust-subset compiler emitting C, no
  LLVM, no proc-macro `dlopen`) is a legitimate first cut that does not need
  Phase 93, at the cost of language coverage.

## Recommended Sequencing

Dependency-ordered path from "Rust programs run" to "Rust compiles on-device":

1. **Keep this doc current** (done — this revision).
2. **Phase 94** — the cargo-musl port class + uutils. No kernel work; proves the
   Rust-port machinery. Do regardless of the toolchain ambition.
3. **Phase 93** — the dynamic `libc.so` keystone. Unblocks dynamic linking
   generally, dynamic Python/`ctypes`, *and* proc-macros. Budget heavily for
   loader **TLS**.
4. **Phase 95** — native rustc. Clang-class port: static bootstrap, bundled LLD,
   userspace target + prebuilt std, `M3OS_WITH_RUST`. First milestone:
   `rustc hello.rs && ./hello` (proc-macro-free). `cargo` + derive macros ride
   Phase 93.

## When to Choose Each Path (for new userspace work today)

- **Choose musl `std`** if the program uses `std`-dependent crates (clap, serde,
  tokio, hyper, regex, rustls), needs threads/networking/rich-FS ergonomically,
  or binary size (300 KB–5 MB) is acceptable. New ports should follow the
  Phase 94 cargo path.
- **Choose native `no_std`** (`userspace/syscall-lib/`) if the program needs
  m3OS-native syscalls (IPC, framebuffer, ktrace, raw scancodes), size matters
  (50–200 KB), it is part of the trusted base (init, low-level servers,
  drivers), or you want `#[cfg(target_os = "m3os")]` to fork shared-crate logic
  (use `x86_64-m3os.json`).

Mixing within a project is fine — `init`/`syscall-lib`/`coreutils` stay `no_std`
while a networked daemon can be a musl `std` crate.

## Validation Recipe

To confirm the `std` pipeline still works after kernel changes:

1. `rustup target add x86_64-unknown-linux-musl` (one-time, host-side).
2. `cargo xtask run` — boots with the five musl Rust binaries staged. Watch for
   `x86_64-unknown-linux-musl target not installed` or `musl Rust build failed
   for <name>`.
3. From the m3OS shell, run `hello-rust` → `Hello from Rust on m3OS!`.
4. Run `sysinfo-rust`, `calc-rust`, `todo-rust` to spot-check `std::fs`/`std::io`.
5. Start `httpd-rust` and hit it via the QEMU port-forward to confirm `std::net`.
6. After syscall-number changes, also run the no-std surface: `cargo xtask test`
   and `cargo test -p kernel-core`.

## Related Docs

- [Phase 44 design doc](../roadmap/44-rust-cross-compilation.md) — the original
  cross-compilation milestone.
- [Phase 94 — Rust-Cargo Ports & uutils](../roadmap/94-rust-cargo-uutils.md) —
  the cargo-cross port class that succeeds Phase 44's ramdisk pipeline.
- [Phase 93 — Dynamic C Runtime](../roadmap/93-dynamic-c-runtime.md) — the
  `libc.so` keystone (and the proc-macro prerequisite).
- [Phase 95 — Native Rust Toolchain](../roadmap/95-native-rust-toolchain.md) —
  on-device `rustc`/`cargo`.
- [Phase 12 design doc](../roadmap/12-posix-compat.md) — the Linux ABI layer.
- [Phase 40 design doc](../roadmap/40-threading-primitives.md) — `clone`, futex,
  TLS via `arch_prctl`.
- [Phase 85d — Clang/LLVM/LLD](../roadmap/85d-clang-llvm.md) — the on-device
  native-codegen precedent and the kernel fixes Phase 95 reuses.
- [`architecture-and-syscalls.md`](./architecture-and-syscalls.md) — the syscall
  ABI reference.
- [`file-backed-mmap.md`](./file-backed-mmap.md) — the (now partially closed)
  file-backed mmap gap.
