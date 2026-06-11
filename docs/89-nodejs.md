# Node.js

**Aligned Roadmap Phase:** Phase 89
**Status:** Planned
**Source Ref:** phase-89
**Supersedes Legacy Doc:** docs/archived/nodejs-roadmap.md (revived as docs/nodejs-roadmap.md)

## Overview

Phase 89 brings a statically-linked Node.js 22 LTS runtime inside m3OS as a
content-addressed `.m3pkg` — extending the post-1.0 developer platform into its
first JIT-capable managed runtime. The headline outcome is `cargo xtask
node-smoke` passing five local sentinels (NODE_HELLO_OK, NODE_FS_OK,
NODE_TIMER_OK, NODE_EVENTLOOP_OK, NODE_HTTP_OK) plus an opt-in `npm install`
arm (Track D, `M3OS_NODE_NET=1`), followed by a kernel bump to `0.89.0`.

The central lesson is how a JIT-heavy managed runtime stresses execution
permissions differently from a static CLI. CPython, Go, and Clang all load
once into RX pages and stay there. Node's V8 engine historically wrote machine
code into RW pages, flipped them executable via `mprotect`, and ran them hot.
Modern V8 removed that flip-based model — it now wants either PKU-backed
memory keys or RWX pages — and m3OS forbids RWX. Phase 89 resolves this by
building V8 in **jitless mode** (`--v8-lite-mode`): all JavaScript runs through
the Ignition interpreter, V8 builtins are embedded RX in the binary's `.text`
at link time, and zero executable memory is allocated at runtime. The W^X
property is satisfied by construction.

The second lesson is how libuv's event loop is layered on top of the epoll and
threading substrate that Phase 86d (Go) already cleared. Most of the kernel
prerequisites were delivered well before Phase 89; only `timerfd_*` was
genuinely missing.

## What This Doc Covers

- The V8 jitless W^X model and why modern V8 cannot use the old `mprotect`
  RW↔RX flip on a kernel without PKU.
- The libuv `timerfd` event-loop integration and the `signalfd` self-pipe
  fallback decision.
- The static-musl build strategy — host clang targeting musl, reusing the
  `build_llvm` sysroot, GYP host/target toolchain split.
- The ICU, bundled-deps, and npm configuration choices.
- The TLS/DNS/`npm install` package path and how it relates to Phase 90 (Claude
  Code).
- Non-goals: native addons, `--inspect` debugger, RWX JIT, WASM.

## Core Implementation

### The V8 JIT problem and the jitless solution

V8's JIT compiler (TurboFan) has two modes for committing executable code to
memory:

1. **PKU (Intel Memory Protection Keys):** allocates code pages with a
   hardware-enforced domain key, flips the key to RX on execution. Requires the
   kernel to expose `pkey_mprotect` — m3OS does not have MPK support in
   Phase 89.
2. **RWX fallback:** maps pages `PROT_READ|PROT_WRITE|PROT_EXEC` simultaneously
   and runs the JIT without any protection flip. m3OS rejects `PROT_WRITE|
   PROT_EXEC` at `sys_mprotect` (the `wx-violation` regression gate) — any
   attempt returns `EINVAL` and V8 aborts.

The older `write_protect_code_memory` flag (which staged RW→RX transitions via
plain `mprotect`) was **removed from V8** before Node 22 shipped. There is no
currently-supported V8 GN flag to restore it. This means a default-configured
Node binary on m3OS would segfault on the first JavaScript execution.

The resolution is `--v8-lite-mode` (Node's `./configure` flag), which sets
`v8_enable_lite_mode=true`. Under Lite Mode:
- V8 uses the **Ignition interpreter** exclusively for all JS execution.
- TurboFan and Maglev (the optimizing JITs) are compiled out.
- V8 builtins (the built-in JavaScript functions and the interpreter dispatch
  table) are embedded as **read-execute data in the binary's `.text` segment**
  during the build's `mksnapshot` step — not allocated at runtime.
- WebAssembly is disabled (`v8_enable_webassembly=false`), since WASM requires
  a JIT to emit machine code.

The result: `node` starts, interprets JavaScript, and never calls
`mprotect(PROT_EXEC)` on a page it controls. The W^X property holds without
any new kernel machinery. PKU-backed JIT is a documented follow-up (needs a
kernel `pkey_mprotect`/MPK story).

Performance cost: roughly 40% on synthetic benchmarks like Speedometer, but
real-world Node server workloads (the m3OS use case) are much closer to 6%.
For a system whose primary Node use case is the Phase 90 CLI agent — an
I/O-bound, API-call-driven workflow — the interpreter throughput is sufficient.

### libuv and the timerfd event loop

libuv is the async I/O library that underlies Node's event loop. On Linux,
libuv builds its timer wheel on top of a `timerfd` file descriptor registered
in the epoll set: instead of passing a timeout to `epoll_wait`, libuv creates
one `timerfd`, arms it to the next due timer, and waits on an unbounded
`epoll_wait`. When the timer fires, the fd becomes readable — the epoll wakes,
libuv reads the expiration count, and fires the corresponding `setTimeout` or
`setInterval` callbacks.

Phase 89 Track A implemented `timerfd_create` (syscall 283), `timerfd_settime`
(286), and `timerfd_gettime` (287) in `kernel/src/timerfd.rs`, modeled on
`eventfd.rs`. The backing object tracks the next-expiry tick and expiration
count; the scheduler's deadline scanner clamps blocked `epoll_wait` calls to
the nearest outstanding `timerfd` expiry rather than waking from a timer ISR
(which would require allocation in interrupt context). `CLOCK_MONOTONIC` and
`CLOCK_REALTIME` are both supported; one-shot and interval rearms both work.

The `timerfd` implementation has 11 host-side unit tests in `kernel_core::timerfd`
covering expiration accounting, rearm math, and tick↔nanosecond conversion.

### signalfd: the self-pipe fallback

libuv uses `signalfd` to route process signals (SIGINT, SIGTERM, SIGCHLD)
through the epoll loop, but `signalfd4` (syscall 289) is not implemented in
m3OS. libuv's `!HAVE_SIGNALFD` fallback — a `pipe2` fd pair with a `sigaction`
handler that writes one byte into the write end when a signal arrives, and the
read end registered in the epoll set — is a documented, supported libuv
configuration. Both `pipe2` (293) and `rt_sigaction` (13) are present. Under
the musl cross-build, libuv detects `signalfd` absence via feature macros and
selects the self-pipe path automatically. No kernel `signalfd4` is needed.

### Static-musl build: the host-clang cross model

Node.js is C++17 (V8 + libuv + the Node core). `musl-tools` ships no C++
compiler, so — exactly like the Phase 85d Clang port — `build_node` drives the
**host `clang++`** with `--target=x86_64-unknown-linux-musl` and
`-fuse-ld=lld`, not the musl-gcc path. It reuses the `target/llvm-musl-sysroot`
built by `build_llvm` (the musl headers + `libc.a` + the Stage-B
`libc++.a`/`libc++abi.a`/`libunwind.a` + compiler-rt builtins).

The key cross-build complication is that Node builds some tools — `mksnapshot`,
`torque` — on the host machine and runs them during the build. These must link
the host glibc, not musl. Node's GYP build system supports a host/target split
via `CC_host`/`CXX_host` environment variables (for `toolset=host` targets) and
`CC`/`CXX` for the final `node` binary (`toolset=target`). Because build host
and target are both x86_64, `mksnapshot` runs natively on the host without
`qemu-user` — the single property that makes a Node cross-build tractable here.

Key `./configure` flags:

| Flag | Reason |
|---|---|
| `--fully-static` | No `PT_INTERP`; disables `dlopen`; loaderless contract like CPython/Go/Clang |
| `--v8-lite-mode` | Jitless V8; W^X safe; the primary W^X config (see above) |
| `--with-intl=small-icu` | Bundles en-US ICU data into the binary; `Intl.NumberFormat('en-US')` works |
| `--cross-compiling` | GYP honors the `CC_host`/`CXX_host` split even on same-arch |
| `--without-inspector` | Drops the `--inspect`/DevTools C++ surface (documented non-goal) |
| `--openssl-no-asm` | Avoids perlasm paths that break under clang+lld+musl |
| `--without-corepack` | Drops the corepack shim; npm is enough |

All other major dependencies (OpenSSL, zlib, c-ares, nghttp2, llhttp, brotli,
ngtcp2, simdjson, simdutf, ICU data) are **bundled** — `DEPS=` is empty and no
`--shared-*` flags are used. The `node` `.m3pkg` is self-contained with no
runtime package dependencies.

The resulting `node` binary is ELF `EXEC`/`DYN` fully static — no `PT_INTERP`
segment — proven by `readelf -l` (the same check `build_python`/`build_go` use).
Installed size is approximately 90–110 MB (measured once the port builds).

### npm path: TLS, DNS, and the Phase 90 dependency

npm and npx are JavaScript files installed into `/usr/lib/node_modules/npm/`
with `#!/usr/bin/env node` shebangs — they run on the same static `node` binary
and are included automatically (no `--without-npm` is passed). The `npm install`
over HTTPS path is Track D (`M3OS_NODE_NET=1`, opt-in).

TLS for npm rides Node's **bundled OpenSSL** (not the Phase 86c mbedTLS/curl
chain). The CA bundle path is configured via `NODE_EXTRA_CA_CERTS` pointing at
the Phase 86a `/etc/ssl/certs/ca-certificates.crt`. DNS uses Node's bundled
`c-ares` library against the Phase 86 kernel resolver. This means npm is
entirely independent of the dropbear/mbedTLS/curl/git TLS stack — a second TLS
path, validated separately. Phase 90 (Claude Code) depends on `npm install -g
@anthropic-ai/claude-code` working over real HTTPS; Track D is the non-deferrable
prerequisite.

### The node-smoke gate

`cargo xtask node-smoke` (Track C) boots m3OS with `M3OS_WITH_NODE=1` bundling
the `.m3pkg`, installs Node via `pkg install node`, and asserts five local
sentinels from `node /usr/src/node-probe.js`:

- `NODE_HELLO_OK` — interpreter and V8 start; `AT_RANDOM`/`getrandom` entropy
  bootstrap succeeds.
- `NODE_FS_OK` — `fs.writeFileSync`/`readFileSync` round-trip a `/tmp` file
  with byte-compare.
- `NODE_TIMER_OK` — `setTimeout`, `setInterval`, and `setImmediate` fire in
  order, exercising the `timerfd` event-loop wakeup.
- `NODE_PROC_OK` — `process.argv`, `process.platform === 'linux'`, `process.pid`,
  and `process.versions` report correctly.
- `NODE_EVENTLOOP_OK` — `Promise`/microtask + `queueMicrotask` + `nextTick`
  ordering check.

A loopback `node /usr/src/node-http.js` probe additionally asserts
`NODE_HTTP_OK` (an in-process `http.createServer` + `http.get` over 127.0.0.1).
The egress probe (`NODE_EGRESS_OK`) runs a plaintext HTTP GET to the SLIRP
host at `10.0.2.100:80` — the same pattern as the Go gate (Track D.1, always-on).

The gate is wired opt-in via `M3OS_NODE_REGRESSION=1` at `--timeout 5400`
(clang-class timeout — the ~100 MB install over the ~200 KB/s ring-3 VFS takes
tens of minutes). When `clang`/`cmake`/`ninja` are absent on the host,
`build_node_port()` prints `SKIP (reason: …)` and exits success.

## Key Files

| File | Purpose |
|---|---|
| `kernel/src/timerfd.rs` | `timerfd` backing object: expiration count, `it_interval` rearm, poll readiness, `read(2)` drain |
| `kernel/src/arch/x86_64/syscall/mod.rs` | `sys_timerfd_create` / `sys_timerfd_settime` / `sys_timerfd_gettime` dispatch + `timerfd_readable` wired into `fd_poll_events` |
| `kernel-core/src/timerfd.rs` | Host-testable expiration accounting and `ns_to_ticks_ceil` / `ticks_to_ns` helpers (11 unit tests) |
| `ports/lang/node/Portfile` | Pinned `VERSION=22.22.3`, `SHA256=f3e6a578db1ab335a4a72785c1e87ad18a2cf6d2fc25747a1d741fb34af0bd0f`, `CATEGORY=lang`, `DEPS=` empty |
| `xtask/src/port_build.rs` | `fn build_node` — host-clang C++ cross, GYP host/target split, `assemble_musl_sysroot` reuse, `musl_extra_ldflags_joined` for the static link probe |
| `xtask/src/main.rs` | `fn cmd_node_smoke` / `fn node_smoke_steps` — serial DSL gate; `M3OS_WITH_NODE` bundle block in `populate_phase_69d_ports` |
| `docs/nodejs-roadmap.md` | Standalone per-tool porting narrative (revived from archive) |
| `docs/roadmap/89-nodejs.md` | Phase design doc |
| `docs/roadmap/tasks/89-nodejs-tasks.md` | Per-track task list with acceptance items |

## How This Phase Differs From Later Runtime Work

- Phase 89 uses **jitless V8** (Ignition interpreter only, no TurboFan). A
  future phase adding PKU (`pkey_mprotect` / Intel MPK) would allow opt-in JIT
  with hardware-enforced W^X — the design is tracked but deferred.
- Phase 89 uses Node's **bundled OpenSSL** for TLS. A future phase could unify
  the TLS stack with the Phase 86c mbedTLS/curl chain, but the two stacks are
  independent by design.
- Phase 89 runs `node-smoke` at `-smp 1` (single core, like Go/gh) to avoid
  cross-core SMP races during bring-up. Multi-core `worker_threads` semantics
  are a follow-up.
- WASM is explicitly disabled by `v8_enable_webassembly=false` (a side-effect of
  `--v8-lite-mode`). A WASM engine for m3OS would need either a different
  runtime (wasmtime, wasmer) or PKU-backed JIT in V8.
- Phase 90 (Claude Code) is the consumer of the npm path delivered in Track D.
  Phase 89 proves the path works; Phase 90 exercises it end-to-end with a real
  Claude API agent.

## Related Roadmap Docs

- [Phase 89 design doc](./roadmap/89-nodejs.md)
- [Phase 89 task list](./roadmap/tasks/89-nodejs-tasks.md)
- [Phase 86 task list — Go runtime (86d)](./roadmap/tasks/86d-go-runtime-tasks.md) — cleared the managed-runtime kernel blockers this phase reuses
- [Phase 85 task list — Cross-compiled toolchains](./roadmap/tasks/85a-package-infrastructure-tasks.md) — `.m3pkg` + offline `pkg` substrate
- [Node.js standalone roadmap](./nodejs-roadmap.md) — per-tool porting narrative with Mermaid dependency diagrams

## Deferred or Later-Phase Topics

- **PKU-backed JIT** — V8 TurboFan with `pkey_mprotect` / Intel MPK; requires
  a kernel `pkey_alloc`/`pkey_mprotect` story. The jitless config is the shipped
  deliverable; JIT is the tracked follow-up.
- **WebAssembly** — disabled by `--v8-lite-mode` (`v8_enable_webassembly=false`);
  needs either a standalone WASM runtime or PKU-backed V8.
- **Native addons / `node-gyp`** — requires on-device C++ compilation and
  `dlopen`; `--fully-static` disables `dlopen` by design.
- **`--inspect` / Chrome DevTools protocol** — dropped via `--without-inspector`;
  not needed for the Phase 90 CLI-agent use case.
- **`npm install` over HTTPS (live)** — Track D is implemented but opt-in
  (`M3OS_NODE_NET=1`); real registry egress cannot be CI-bound.
- **Multi-core `worker_threads`** — bring-up runs `-smp 1`; SMP validation is a
  follow-up once the single-core gate is stable.
- **Python TLS/DNS/`pip`** — independent of Phase 89; tracked within Phase 86 /
  Phase 91 (`ctypes`/`dlopen`).
