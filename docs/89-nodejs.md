# Node.js

**Aligned Roadmap Phase:** Phase 89
**Status:** Complete
**Source Ref:** phase-89
**Supersedes Legacy Doc:** docs/archived/nodejs-roadmap.md (revived as docs/nodejs-roadmap.md)

## Overview

Phase 89 brings a statically-linked Node.js 22 LTS runtime inside m3OS as a
content-addressed `.m3pkg` — extending the post-1.0 developer platform into its
first JIT-capable managed runtime. The headline outcome is `cargo xtask
node-smoke` passing the always-on sentinels (NODE_HELLO_OK, NODE_FS_OK,
NODE_PROC_OK, NODE_EVENTLOOP_OK, NODE_TIMER_OK, **NODE_EGRESS_OK** — a full
libuv `http.get` cycle over the in-kernel TCP stack — and NODE_TLSDNS_OK) plus
opt-in live-HTTPS + `npm install` arms (Track D, `M3OS_NODE_NET=1`, real
internet only), followed by a kernel bump to `0.89.0`.

The central lesson is how a JIT-heavy managed runtime stresses execution
permissions differently from a static CLI. CPython, Go, and Clang all load
once into RX pages and stay there. Node's V8 engine historically wrote machine
code into RW pages, flipped them executable via `mprotect`, and ran them hot.
Modern V8 removed that flip-based model — it now wants either PKU-backed
memory keys or RWX pages — and m3OS forbids RWX. Phase 89 resolves this by
building V8 in **jitless mode** (`--v8-options=--jitless`): all JavaScript runs through
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

The resolution is **`--v8-options=--jitless`** (Node's `./configure` flag bakes
`--jitless` as a default V8 option). Under jitless mode:
- V8 uses the **Ignition interpreter** exclusively for all JS execution.
- TurboFan and Maglev (the optimizing JITs) never allocate executable memory.
- V8 builtins (the built-in JavaScript functions and the interpreter dispatch
  table) are embedded as **read-execute data in the binary's `.text` segment**
  during the build's `mksnapshot` step — not allocated at runtime.
- WebAssembly is unavailable at runtime (WASM requires a JIT), though the WASM
  support is still *compiled into* V8 (see the as-built note below).

> **As-built note — why `--v8-options=--jitless`, not `--v8-lite-mode`.** The
> two are equivalent for W^X (both make V8 allocate zero runtime executable
> memory), and `--v8-lite-mode` was the first choice. But `--v8-lite-mode` also
> sets `v8_enable_webassembly=false`, compiling WASM *out* of V8 — and Node 22
> unconditionally passes its default `--experimental-wasm-imported-strings` /
> `-memory64` / `-exnref` V8 flags at startup. A WASM-less V8 rejects those as a
> fatal "bad option" (exit 9) *before `node --version` prints*. Keeping WASM
> compiled in (the default, no lite-mode) makes V8 recognise the flags, and
> `--jitless` then renders WASM inert — so node starts cleanly **and** stays
> W^X-safe. This was found by running the musl-static binary on the build host.

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
| `--v8-options=--jitless` | Jitless V8 (zero runtime executable memory); W^X safe; the primary W^X config (see above). NOT `--v8-lite-mode` — that removes WASM, which aborts Node 22 startup |
| `--enable-static` | Builds the static `libnode.a` alongside the binary (harmless; the static-musl link consumes the objects) |
| `--with-intl=small-icu` | Bundles en-US ICU data into the binary; `Intl.NumberFormat('en-US')` works |
| `--without-inspector` | Drops the `--inspect`/DevTools C++ surface (documented non-goal) |
| `--openssl-no-asm` | Avoids perlasm paths that break under clang+lld+musl |
| `--without-corepack` | Drops the corepack shim; npm is enough |
| `--without-node-snapshot` | Skips the host-`node_mksnapshot`→target startup-snapshot blob (a host-glibc tool writing a musl-target blob); the snapshot is regenerated at first run — small startup cost, robust cross-build |

> **NB — `--cross-compiling` is deliberately NOT passed.** Build host and target
> are both x86_64 and a fully-static musl `mksnapshot`/`torque` runs natively on
> the glibc host, so the `CC_host`/`CXX_host` env split is honored without it.
> Passing `--cross-compiling` forces V8's `want_separate_host_toolset=1`, which
> emits both host- and target-toolset `v8_inspector_headers` rules writing the
> same arch-independent `js_protocol.stamp` → a fatal "multiple rules generate"
> error. A single native toolset generates it once.

All other major dependencies (OpenSSL, zlib, c-ares, nghttp2, llhttp, brotli,
ngtcp2, simdjson, simdutf, ICU data) are **bundled** — `DEPS=` is empty and no
`--shared-*` flags are used. The `node` `.m3pkg` is self-contained with no
runtime package dependencies.

The resulting `node` binary is ELF `EXEC`/`DYN` fully static — no `PT_INTERP`
segment — proven by `readelf -l` (the same check `build_python`/`build_go` use).
The sealed `node.m3pkg` measures **~120 MB** (126,327,183 bytes — larger than a
lite-mode build because WASM stays compiled in; see the jitless note above).

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

It also asserts `NODE_TLSDNS_OK` — `require('tls')`/`dns`/`crypto` load the
static OpenSSL/c-ares/crypto stacks without throwing. **This always-on core
PASSES** (validated under `M3OS_KVM=1`, ~30 s once booted).

> **The startup-hang fix (a real kernel bug).** Getting node to even print
> `--version` required fixing `F_SETFD` in the kernel: it wrongly returned
> *success* for closed/out-of-range fds (unlike its sibling `F_GETFD`/`F_SETFL`,
> which return `EBADF`). Node 22's libuv sets `FD_CLOEXEC` on every fd in a loop
> that stops at the first `EBADF` — so on m3OS that loop busy-spun to millions of
> fds and never returned. `F_SETFD` now returns `EBADF` for invalid fds.

> **Networking works — the futex fix (a second real kernel bug).** node's
> `http.get` now completes a full request/response cycle over the in-kernel TCP
> stack: the always-on `NODE_EGRESS_OK` arm GETs `http://10.0.2.100:80/` (a SLIRP
> host server) and the gate's host `TcpListener` logs the guest connection. This
> required implementing **`FUTEX_REQUEUE` / `FUTEX_CMP_REQUEUE`** in `sys_futex`:
> they were silent no-ops, but musl's `pthread_cond_signal`/`broadcast` *requeues*
> cond-waiters from the condition's futex onto the associated mutex's futex. With
> the op a no-op, libuv's threadpool condvar deadlocked — a worker parked on a
> futex no one ever woke (`BlockedOnFutex "no waker registered"`), so `http.get`
> /`getaddrinfo` hung indefinitely. Now the requeue moves the waiters correctly
> (and the `FUTEX_WAIT` return path dequeues robustly from *whichever* queue a
> waiter was requeued to). So the **in-kernel-TCP egress is always-on** and is the
> `FUTEX_CMP_REQUEUE` regression guard. The remaining `M3OS_NODE_NET`-gated arms
> are only the ones that need **real outbound internet** — a live HTTPS
> cert-validate against `example.com` and `npm install` from `registry.npmjs.org`
> — which repo CI can't reach (mirroring `git-https-smoke`'s `M3OS_GIT_HTTPS_NET`).
> Separately, m3OS still has **no 127.0.0.1 loopback** interface, but the egress
> arm exercises the TCP path regardless.

> **Real outbound HTTPS works now — the MSS fix (a third real kernel bug).**
> Under `M3OS_NODE_NET=1`, node's `https.get('https://example.com/')` completes a
> full TLS 1.3 handshake and validates the cert chain (`NODE_HTTPS_OK`), and a
> direct GET to `registry.npmjs.org` reaches the npm registry over the same TLS
> path (`NPM_REGISTRY_OK`). This required a TCP fix: `tcp_send` sized each
> outbound segment to the 8 KiB send window with **no MSS/MTU cap**, so a
> >1460-byte write — node/OpenSSL's 1588-byte TLS ClientHello — built a ~1642-byte
> Ethernet frame that virtio-net (1514 MTU + 10-byte vnet hdr = 1524) **silently
> dropped**; every retransmit dropped the same way, the handshake stalled to a
> peer FIN, and userspace saw "socket disconnected before secure TLS connection
> was established". Capping each outbound TCP segment to one MSS (1460) keeps every
> frame within the MTU (the ClientHello ships as 1460+128); RX of full-MTU inbound
> segments already fit the 1524-byte buffer. On-link egress only ever sent tiny
> frames, so the bug surfaced solely on the first real >MTU transmit — a *general*
> outbound-TLS fix (git/curl benefited too), not Node-specific. **Still opt-in
> (`M3OS_NODE_NET=1`):** these need real outbound internet, which repo CI lacks
> (mirroring `git-https-smoke`). Full `npm install` *completion* is not gate-
> asserted — npm launches and reaches the registry, but loading its ~thousands of
> JS files over the ~200 KB/s ring-3 VFS (jitless V8) is impractically slow; that
> is a VFS-throughput limit, not a TLS gap.

> **`#!` shebang exec + `/usr/bin/env` (the npm launcher chain).** Three pieces
> make npm's `#!/usr/bin/env node` wrapper launchable:
> 1. **Kernel `binfmt_script`.** `execve` now re-execs a `#!interp [arg]` script's
>    interpreter with argv rewritten to `[interp, arg?, script_path, original
>    argv[1..]]`, looping (bounded by `ELOOP`) so an interpreter that is itself a
>    script also resolves. Without it, any script returned `ENOEXEC`.
> 2. **`/usr/bin/env` staged.** The `env` coreutil is copied to `/usr/bin/env` in
>    the base image (it was only at `/bin/env`; the builder has no symlink op, and
>    the pkg installer's `mkdir` ignores `EEXIST` so pre-creating `/usr/bin` is
>    safe). `#!/usr/bin/env <interp>` now resolves the path.
> 3. **`env` runs the command.** m3OS's `env` previously only *printed* the
>    environment — it ignored `env COMMAND [args]`. It now implements that form:
>    apply leading `NAME=VALUE`, then PATH-search and exec the command with the
>    inherited environment (the `exec_with_path_search` helper, shared with
>    `xargs`). So `/usr/bin/env node …` actually finds + runs `node`.
> Proven always-on by the `node-smoke` `SHEBANG__OK` (a `#!/bin/echo` script
> re-execs `/bin/echo`) and `ENVCATMARKER_OK` (a `#!/usr/bin/env cat` script:
> staged `/usr/bin/env` PATH-finds `cat` and runs it) arms.

The gate is wired opt-in via `M3OS_NODE_REGRESSION=1` at `--timeout 5400`
(much faster under `M3OS_KVM=1`, which the gate honors; `M3OS_NODE_FAST_ITER`
reuses an installed disk). When the host C++ toolchain or the `llvm` musl
sysroot is absent, the gate prints `SKIP (reason: …)` and exits success.

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
  `--jitless`). A WASM engine for m3OS would need either a different
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
- **WebAssembly** — compiled into V8 but inert at runtime under `--jitless`
  (WASM needs a JIT to emit machine code); needs PKU-backed V8 or a standalone
  WASM runtime to actually run.
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
