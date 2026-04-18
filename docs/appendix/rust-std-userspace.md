# Rust `std` in m3OS Userspace: Status, Gaps, and Recommended Approach

**Aligned Roadmap Phase:** Phase 44 — Rust Cross-Compilation
**Status:** Complete (Phase 44); residual gaps tracked here
**Source Ref:** phase-44
**Related:** [Phase 44 design doc](../roadmap/44-rust-cross-compilation.md), [Phase 44 task list](../roadmap/tasks/44-rust-cross-compilation-tasks.md), [`architecture-and-syscalls.md`](./architecture-and-syscalls.md)

## Overview

m3OS supports Rust `std` in userspace through a deliberate **Linux ABI shim**
strategy: Rust programs are cross-compiled on the host with
`--target x86_64-unknown-linux-musl`, statically linked against musl, and
delivered into the m3OS ramdisk. The kernel's Phase 12 Linux compatibility layer
translates the resulting Linux syscalls into m3OS operations. There is no
`std::sys::m3os` backend, no in-tree port of musl, and no runtime libc — every
binary brings its own statically linked musl. This document records the closure
state of that pipeline, the residual syscall and runtime gaps, and the
recommended approach for closing each one.

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

Phase 44 picked the second path. The kernel had already implemented most of the
Linux syscall surface for musl-linked C programs (Phase 12), `clone(CLONE_THREAD)`
and futex (Phase 40), TLS via `arch_prctl` (Phase 40), and a useful subset of
networking (Phase 23) and I/O multiplexing (Phase 37). Reusing that surface for
Rust `std` cost roughly *one xtask function plus five demo crates* rather than a
fork of the Rust compiler.

The trade-off is a permanent dependency on the Linux syscall numbering and
struct layouts. m3OS-native syscalls (kernel-extension space `0x1000+`,
including the IPC dispatch range `0x1100..=0x110F`) remain reachable from
`no_std` programs through `userspace/syscall-lib/`, but `std` programs cannot
see them.

## Two Compilation Paths in m3OS

| Path | Target | Library surface | Use when |
|---|---|---|---|
| musl `std` | `x86_64-unknown-linux-musl` | Full `std` (`std::fs`, `std::net`, `std::io`, `std::thread`, `std::process`) via musl libc | Porting existing Rust crates, writing new programs that benefit from the standard library, anything that needs threads/networking ergonomically |
| Native `no_std` | `x86_64-unknown-none` (default) or `x86_64-m3os.json` | `core` + `alloc` + `userspace/syscall-lib/` | Tiny binaries (50–200 KB), programs that need m3OS-native syscalls (IPC, framebuffer, debug print), kernel-adjacent code |

The two paths coexist in the same kernel build. They do **not** share crates,
because the workspace default target is `x86_64-unknown-none` and the musl
crates are non-workspace members (each musl crate's `Cargo.toml` starts with
`[workspace]` to detach it).

The custom `x86_64-m3os.json` target spec at the project root produces
functionally identical machine code to `x86_64-unknown-none`. Its only purpose
is to set `"os": "m3os"` so shared crates can branch on
`#[cfg(target_os = "m3os")]`. Migration is opt-in; nothing is forced to move.

## What Phase 44 Shipped

### The five demonstration crates

Located under `userspace/`, each as a non-workspace musl `std` crate:

| Crate | std surface | What it proves |
|---|---|---|
| `hello-rust` | `println!` | musl CRT + entry, `write` syscall, exit |
| `sysinfo-rust` | `std::fs::read_to_string` | tmpfs/procfs read path through musl |
| `httpd-rust` | `std::net::TcpListener` | socket + bind + listen + accept + send |
| `calc-rust` | `std::io::stdin().read_line()` | TTY line discipline through musl |
| `todo-rust` | `std::fs::write` round-trip | persistent ext2/FAT32 write path |

Each crate uses a release profile with `opt-level = "z"`, `lto = true`,
`strip = true`, `panic = "abort"` to control binary size.

### xtask integration

`xtask/src/main.rs:430` — `build_musl_rust_bins()` — owns the musl Rust build:

- Stages a zero-length placeholder under `target/generated-initrd/<name>` for
  each crate so the kernel's `include_bytes!` path always resolves even when a
  build fails or the target is not installed.
- Probes `rustup target list --installed` once, warns and bails if
  `x86_64-unknown-linux-musl` is missing, leaving placeholders behind.
- Invokes `cargo build --target x86_64-unknown-linux-musl --release` per crate
  via `--manifest-path`, with `RUSTFLAGS="-C relocation-model=static -C
  target-feature=+crt-static"`. The static relocation model produces ET_EXEC
  binaries (rather than ET_DYN/PIE) so the kernel's ELF loader does not collide
  with musl's self-relocating CRT startup.
- Runs `strip` on each output before staging.

It is wired into `build_kernel()` (line ~1294), `image` (line ~1536), and `run`
(line ~1787) so every kernel artifact path picks up the Rust `std` binaries.

### Ramdisk embedding

`kernel/src/fs/ramdisk.rs:173–177` defines five static byte slices via the
`generated_initrd_asset!` macro pointing at `target/generated-initrd/`. The
`BIN_ENTRIES` table maps those slices to `/bin/<name>` so `execve` resolves the
binaries at runtime.

### Custom target spec

`x86_64-m3os.json` at the project root. Derived from `x86_64-unknown-none`,
preserves all kernel-critical settings (`disable-redzone: true`, SIMD off,
`panic-strategy: abort`, `code-model: kernel`), changes only `"os": "m3os"`.
`"has-thread-local": false` is intentional — `std::thread` is reached through
the musl path, not this target.

## The Linux ABI Boundary

A musl-linked Rust `std` binary, before `main()` runs, will issue roughly:
`set_tid_address`, `brk`, `mmap` (anonymous), `rt_sigaction`, `rt_sigprocmask`,
`arch_prctl(ARCH_SET_FS, ...)`, optionally `prlimit64`, `sigaltstack`,
`getrandom`. Then user code starts. Each of those must either succeed or fail
in a way musl tolerates. m3OS implements every member of that startup set
except `prlimit64` (returns `-ENOSYS`, see gap below), and musl handles the
`-ENOSYS` gracefully — the program continues without a configured rlimit.

After startup, the syscalls used by common `std` paths fall into these
families. The table is what musl emits, **not** what Rust source code calls.

| `std` API | musl-emitted syscalls | m3OS status |
|---|---|---|
| `std::fs::File::open` | `openat` + `fcntl(F_GETFD)` + `fcntl(F_SETFD, FD_CLOEXEC)` | Works |
| `std::fs::read_to_string` | `openat` + `fstat` + `read` (loop) + `close` | Works |
| `std::fs::write` | `openat(O_WRONLY|O_CREAT|O_TRUNC)` + `write` (loop) + `close` | Works on writable mounts |
| `std::fs::read_dir` + iteration | `openat(O_DIRECTORY)` + `getdents64` (loop) + `close` | Works |
| `std::fs::metadata` | `fstatat` (or `lstat`) | Works |
| `std::fs::set_permissions` | `fchmodat` / `chmod` | Works |
| `std::fs::rename` | `renameat2` (preferred) → `renameat` (fallback) | `renameat` works; `renameat2` flags partly honored |
| `TcpListener::bind` | `socket(AF_INET, SOCK_STREAM, 0)` + `setsockopt(SO_REUSEADDR)` + `bind` + `listen` | Works |
| `TcpListener::accept` | `accept4(SOCK_CLOEXEC)` (or `accept` fallback) | Works |
| `TcpStream::connect` | `socket` + `connect` (blocking) | Works |
| `TcpStream::set_nonblocking` | `fcntl(F_SETFL, O_NONBLOCK)` | Works |
| `UdpSocket` | `socket(SOCK_DGRAM)` + `sendto` / `recvfrom` | Works |
| `std::thread::spawn` | `mmap` (stack) + `clone(CLONE_THREAD|CLONE_VM|CLONE_FS|CLONE_FILES|CLONE_SIGHAND|CLONE_SETTLS|CLONE_PARENT_SETTID|CLONE_CHILD_CLEARTID)` + `set_tid_address` | Works (Phase 40) |
| `Mutex` / `Condvar` / `Once` | `futex(FUTEX_WAIT)` / `futex(FUTEX_WAKE)` | Works (`kernel/src/process/futex.rs`) |
| `std::process::Command` | `clone(SIGCHLD)` (musl posix\_spawn fallback) → `execve` → `waitpid` | Works (`sys_clone` accepts `SIGCHLD`, `0`, and the `CLONE_VM|CLONE_VFORK[|SIGCHLD]` posix\_spawn pattern at `kernel/src/arch/x86_64/syscall/mod.rs:11305–11311`) |
| `std::env::args`, `std::env::vars` | Read at startup from the user stack populated by `execve` | Works |
| `std::time::Instant`, `SystemTime` | `clock_gettime(CLOCK_MONOTONIC)`, `clock_gettime(CLOCK_REALTIME)` | Works |
| `std::io::stdin().read_line()` | `read(0, ...)` against the TTY in cooked mode | Works |
| Raw mode terminal | `ioctl(TCGETS)` / `ioctl(TCSETS)` | Works (Phase 22 termios) |

## Known Syscall Gaps

These are the residual gaps that musl-linked Rust `std` binaries can encounter.
None of them block the demo program set, but each one will surface eventually
when porting more substantial Rust crates.

### High-impact gaps (likely to be hit)

| Syscall | Status | Evidence | Failure mode |
|---|---|---|---|
| `prlimit64` (302) | Returns `-ENOSYS` | `kernel/src/arch/x86_64/syscall/mod.rs:1526` | musl tolerates; programs that explicitly probe rlimits get an error |
| `utimensat` on tmpfs | Returns `-ENOSYS` | `kernel/src/arch/x86_64/syscall/mod.rs:10166–10167` | `std::fs::set_modified` and tools like `touch` fail on tmpfs paths; works on ext2 |
| Unsupported `clone` flag combos | Returns `-ENOSYS` with warn-log | `kernel/src/arch/x86_64/syscall/mod.rs:11313–11315` | Unusual `clone` calls (e.g., user namespaces, exotic posix\_spawn variants) fail |

### Lower-impact gaps (rare to hit from `std`)

| Syscall family | Status |
|---|---|
| `splice`, `vmsplice`, `tee` | Not implemented. `std` does not use them; `tokio`-style high-perf I/O can fall back to `read`/`write`. |
| `prctl`, `personality` | Partial / stubs. Used by sandboxing libraries, not by `std` directly. |
| `sysinfo`, `ugetrlimit` | Partial. Used by some `nix`-based crates. |
| `io_uring_*` | Not implemented. Modern async runtimes will fall back to `epoll`. |
| `pidfd_*` | Not implemented. `std::process` does not require it; some tokio versions use it opportunistically. |
| `statx` | Partial; `std::fs::metadata` uses `fstatat` and works. |

### Catch-all behavior

Any syscall the dispatcher does not recognize falls through to the default arm
at `kernel/src/arch/x86_64/syscall/mod.rs:1559–1562`, which logs a warning
(`unhandled syscall N (args: ...)`) and returns `-ENOSYS`. This means an
unknown-syscall hit is observable at runtime over the serial log without
crashing the program. The recommended workflow when porting a new crate is to
run it once with serial logging on, grep for `unhandled syscall`, and decide
whether each one needs an implementation or a deliberate stub.

## Threading and TLS Coverage

Threading is complete enough that `std::sync::Mutex`, `std::sync::Condvar`,
`std::sync::OnceLock`, and `std::thread::spawn` work. The core mechanics:

- `clone(CLONE_THREAD | CLONE_VM | CLONE_SIGHAND | CLONE_SETTLS | ...)` creates
  a sibling thread that shares address space, signal handlers, and fd table
  with the parent. Implemented at
  `kernel/src/arch/x86_64/syscall/mod.rs:11290–11299` (`sys_clone_thread`).
- `futex` with real `FUTEX_WAIT` and `FUTEX_WAKE` semantics on anonymous user
  memory. Implementation in `kernel/src/process/futex.rs`.
- TLS via `arch_prctl(ARCH_SET_FS, addr)` (syscall 158). FS base is saved and
  restored per thread on context switch.
- `set_tid_address` (218) records `clear_child_tid` for thread-exit cleanup.
- `gettid` (224) returns the per-thread ID; `tkill` delivers signals to a
  specific thread.

Atomic operations work because the `x86_64` target spec sets
`max-atomic-width: 64` and the kernel runs on cores with native atomic support.

## Filesystem Coverage Notes

- **tmpfs timestamps.** Reads succeed; modification time is not tracked. Any
  `std` API that round-trips an mtime through tmpfs will see it reset to a
  default value, and `utimensat` returns `-ENOSYS` (see gap table).
- **`/proc`.** Backed by tmpfs entries populated by the kernel (`pid/stat`,
  `pid/fd`, `pid/cmdline`). `std::fs::read_to_string("/proc/...")` works for
  these entries; full procfs coverage is **not** a Phase 44 goal.
- **mmap-backed file I/O.** `std::fs` does not use file-backed `mmap` by
  default; the kernel currently supports anonymous `MAP_PRIVATE` only. A
  Rust crate using `memmap2` directly will fail with `-ENOSYS` or
  `-EINVAL` until file-backed `mmap` lands (tracked separately under
  `docs/appendix/file-backed-mmap.md`).
- **Path lookup.** `openat(AT_FDCWD, ...)` works; relative-to-dirfd lookups via
  arbitrary `dirfd` values work for opened directories.

## Networking Coverage Notes

- TCP and UDP work end-to-end via the BSD socket family (Phase 23). `std::net`
  programs do not need any flag-translation shims at the language level —
  musl's wrappers handle the option-name translation.
- `accept4(SOCK_CLOEXEC | SOCK_NONBLOCK)` is honored as a flag set on the new
  fd; bare `accept` works as well.
- `epoll_create1` / `epoll_ctl` / `epoll_wait` work for `std::net` non-blocking
  flows. `select` is also present.
- DNS is **not** in the kernel. Programs that resolve names need a userspace
  resolver or hardcoded IPs. `std::net::ToSocketAddrs` for `&str` will only
  work for literal IPs.
- TLS is not in `std`; programs needing HTTPS pull in `rustls`, which works
  with musl-linked `std` provided the entropy source (`getrandom` syscall) is
  available — and it is.

## Memory and Allocator Notes

- musl ships its own malloc; m3OS does not provide an allocator override.
  The malloc implementation in musl 1.2 is `mallocng` and is well-tested
  against Linux semantics. Any divergence between m3OS and Linux on `mmap`
  flags (e.g., `MAP_FIXED_NOREPLACE`) could cause subtle issues — none have
  been observed in the demo set.
- Heap growth uses `brk`. The kernel honors `brk` for the data segment.
- Stack overflow on a thread is detected by guard pages set up during
  `clone(CLONE_THREAD)`.
- Binary size: a stripped, LTO'd musl `std` "hello world" lands around
  300 KB. The full demo set (five binaries) adds roughly 1.5–2 MB to the
  initrd. The `opt-level = "z"` + `lto = true` profile in each crate's
  `Cargo.toml` is essential because the initrd is embedded in the kernel ELF.

## Recommended Approach for Closing Each Gap

The order below reflects expected impact on real-world Rust crates. None of
these are blockers for shipped demo programs.

### 1. Implement `prlimit64` (high value, low effort)

Implement `sys_prlimit64(pid, resource, new_limit, old_limit)` to write the
historical default limits into `*old_limit` and accept any reasonable
`new_limit` as a no-op for most resources. Two resources matter:
`RLIMIT_STACK` (used by Rust to size new thread stacks) and `RLIMIT_NOFILE`
(used by some crates to size fd tables). Implementing the read path with
sensible defaults is enough for most callers and removes a steady stream of
`unhandled syscall 302` warnings from the serial log.

Estimated effort: half a day. Touchpoint:
`kernel/src/arch/x86_64/syscall/mod.rs:1526`.

### 2. Track timestamps in tmpfs (medium value, medium effort)

Add `mtime`, `atime`, `ctime` fields to the tmpfs inode struct, update them on
`write`/`read`/`open` as appropriate, return them from `stat`, and accept
`utimensat` writes. This unblocks tools like `touch`, `make`, `cargo`, and
anything that uses file timestamps for incremental work. Recommended to do as
its own task because it touches every tmpfs operation site.

Estimated effort: 1–2 days. Touchpoint:
`kernel/src/arch/x86_64/syscall/mod.rs:10164–10168` and the tmpfs inode
definition.

### 3. Document the unhandled-syscall workflow (high value, trivial effort)

The serial-log catch-all at line 1559 is the project's primary mechanism for
discovering syscall gaps. Add a one-page note under `docs/appendix/` (or
extend this one) covering: how to enable serial-log capture, how to grep for
`unhandled syscall`, how to translate the number into a Linux syscall name,
and how to decide whether to implement, stub, or reject. This converts an
implicit workflow into an onboarding artifact.

Estimated effort: 1 hour.

### 4. Stress-test musl malloc (medium value, medium effort)

Write a long-running musl `std` program that allocates and frees aggressively
across multiple threads (e.g., a Rust port of the existing `mmap-leak-test`
crate, but using `Vec`/`Box` instead of raw `mmap`). Run it inside QEMU under
`cargo xtask run` for tens of minutes. Watch for memory leaks, fragmentation,
and crashes. The kernel's anonymous `mmap` and `brk` paths have not been
exercised under sustained musl-malloc load.

Estimated effort: 1 day.

### 5. Wire CI into the demo set (medium value, low effort)

Extend `cargo xtask test` (or a new `cargo xtask test --rust-std`
sub-command) to boot the kernel, invoke each of the five demo programs from a
seeded shell script, and assert their output. Today the validation is manual.
Mechanical CI removes the human step and catches regressions in the Linux
compat layer that the no-std test suite cannot see.

Estimated effort: 1 day. Touchpoint: `xtask/src/main.rs` test subcommand.

### 6. Decide policy on `splice`, `io_uring`, `pidfd` (low value, design effort)

These appear when porting performance-oriented async crates (tokio with
`io_uring`, modern HTTP servers using `splice`). The pragmatic choice for an
educational kernel is to **not** implement them and accept the fallback paths
in the userspace libraries. Document the decision and which crates are known
to need adjustment.

Estimated effort: 2 hours of writing.

### 7. (Deferred indefinitely) Native `std::sys::m3os` backend

Building a native backend would let `std` programs reach m3OS-only syscalls
(IPC dispatch, framebuffer mmap, ktrace) without the Linux numbering. The
maintenance burden — re-vendoring `library/std` per Rust release — is
disproportionate to the benefit while m3OS has fewer than a dozen
m3OS-specific syscalls. Revisit only if the m3OS-native syscall surface grows
substantially (e.g., a graphics or audio API that `std` programs would need).

## When to Choose Each Path

For new userspace work in m3OS:

- **Choose musl `std`** if any of the following is true:
  - The program uses crates from crates.io that depend on `std`
    (clap, serde, tokio, hyper, regex, rustls, …).
  - The program needs threads, networking, or rich filesystem APIs and would
    otherwise require re-implementing those bindings against `syscall-lib`.
  - Binary size is acceptable (300 KB minimum, often 1–5 MB for non-trivial
    crates after LTO).

- **Choose native `no_std`** (with `userspace/syscall-lib/`) if any of the
  following is true:
  - The program needs to call m3OS-native syscalls — IPC dispatch, framebuffer
    mmap, ktrace, raw scancode reads.
  - Binary size matters (can hit 50–200 KB).
  - The program is part of the kernel's trusted base (init, low-level
    servers, drivers).
  - You want to use `#[cfg(target_os = "m3os")]` to fork shared crate logic
    on the m3os identity (use `x86_64-m3os.json` rather than
    `x86_64-unknown-none`).

There is no shame in mixing both within one project — `userspace/init`,
`userspace/syscall-lib`, `userspace/coreutils` stay `no_std` while a new
networked daemon could be a musl `std` crate.

## Validation Recipe

To confirm the pipeline still works after kernel changes:

1. `rustup target add x86_64-unknown-linux-musl` (one-time, host-side).
2. `cargo xtask run` — boots the kernel with the five musl Rust binaries
   already staged in the initrd. Watch the serial log for warnings like
   `warning: x86_64-unknown-linux-musl target not installed` (means the host
   target is missing) or `warning: musl Rust build failed for <name>` (means
   one of the demo crates regressed).
3. From the m3OS shell, run `hello-rust`. Expect `Hello from Rust on m3OS!`.
4. Run `sysinfo-rust`, `calc-rust`, `todo-rust` to spot-check `std::fs` and
   `std::io`.
5. Start `httpd-rust` and use `nc` (or any external HTTP client against the
   QEMU port forward) to confirm `std::net` is alive.
6. After kernel changes that touch syscall numbers, also run the no-std
   surface: `cargo xtask test` and `cargo test -p kernel-core`.

## Future Work (Deferred from Phase 44)

These items are intentionally out of scope for Phase 44 and have no concrete
phase claim yet:

- Native `std::sys::m3os` backend (see "Deferred indefinitely" above).
- Running `cargo` and `rustc` inside the OS. The compiler binaries are 50 MB+
  and would require a vastly larger initrd or on-disk install plus a working
  package fetch path. Phase 31 covers the C compiler bootstrap; a Rust
  equivalent would be its own multi-phase effort.
- Crate registry access from inside the OS. Requires DNS, HTTPS, and a TLS
  trust store inside the OS — none of which are kernel features today.
- Remote debugging (gdb stub) for Rust programs.
- Dynamic linking. All Rust `std` programs are statically linked against musl
  by design and there is no plan to support `.so` loading.

## Related Docs

- [Phase 44 design doc](../roadmap/44-rust-cross-compilation.md) — milestone
  goal, learning goals, acceptance criteria.
- [Phase 44 task list](../roadmap/tasks/44-rust-cross-compilation-tasks.md) —
  per-track scope, files, acceptance items.
- [Phase 12 design doc](../roadmap/12-posix-compat.md) — the Linux ABI
  compatibility layer this approach depends on.
- [Phase 40 design doc](../roadmap/40-threading-primitives.md) — `clone`,
  futex, TLS via `arch_prctl` (shared with this approach).
- [`architecture-and-syscalls.md`](./architecture-and-syscalls.md) — overall
  syscall ABI reference.
- [`file-backed-mmap.md`](./file-backed-mmap.md) — separate gap covering one
  of the limitations called out under Filesystem Coverage Notes.
