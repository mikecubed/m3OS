# Phase 89 — Node.js: Task List

**Status:** Planned — authored ahead of implementation. Headline outcome: `cargo xtask node-smoke` PASSES end-to-end (NODE_HELLO_OK + NODE_FS_OK + NODE_TIMER_OK + NODE_EVENTLOOP_OK + NODE_HTTP_OK) and the opt-in `M3OS_NODE_NET=1` arm runs `npm install` over HTTPS; kernel bumps to `0.89.0`.
**Source Ref:** phase-89
**Depends on:** Phase 37 (I/O Multiplexing — epoll/`epoll_pwait`) ✅, Phase 40 (Threading — `clone(CLONE_THREAD)`/futex/PT_TLS) ✅, Phase 42 (Crypto Primitives) ✅, Phase 75 (W^X — `mprotect` RW→RX, the `wx-violation` gate) ✅, Phase 76 (Dynamic Linker) ✅, Phase 85 (Cross-Compiled Toolchains — `.m3pkg` substrate + offline `pkg`) ✅, Phase 86 (Networking and GitHub — CSPRNG/CA-trust/DNS + `git`-over-HTTPS, the Go runtime 86d that already cleared `mmap`/epoll/`SIGURG` for managed runtimes) ✅ — see [86d-go-runtime-tasks.md](./86d-go-runtime-tasks.md). Quality-gated by Phase 87 (VFS bulk-I/O) ✅ and Phase 88 (`stat` conformance) ✅, both called out as heavy-I/O / `stat`-dependent prerequisites for this phase.
**Goal:** Bring up a supported, statically-linked Node.js (22 LTS) runtime inside m3OS as a content-addressed `.m3pkg` — closing the only two libuv kernel gaps (`timerfd`, `signalfd4`), choosing a W^X-compliant V8 code-memory model (JIT via `mprotect` RW↔RX toggling, with `--jitless` as the documented fallback), validating the local runtime (fs/timers/console/process/event-loop) and a plaintext loopback HTTP path, then delivering the TLS/DNS/`npm install` package path the Phase 90 CLI-agent milestone depends on. Bump the kernel to `0.89.0` and ship the learning doc.

> **Authored ahead of implementation.** Every acceptance item below is intentionally unchecked `[ ]`; it records the planned, measurable result, not a delivered one. (Mirrors the [Phase 86d](./86d-go-runtime-tasks.md) / [Phase 87](./87-vfs-bulk-io-tasks.md) task-list style.) Where a task only *validates* substrate that already exists, the acceptance item says so and points at the existing symbol to reuse rather than reimplement.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Kernel substrate gaps for libuv (`timerfd`, `signalfd4`) + V8 JIT W^X validation | 86d, 75 | Planned |
| B | Node port build pipeline (`build_node`, host-clang C++ cross, V8/ICU/static config, `M3OS_WITH_NODE` bundle) | A, 85a | Planned |
| C | Local-runtime validation gate (`node-smoke`: fs/timers/console/process/event-loop + loopback HTTP) | B | Planned |
| D | Networked runtime + npm (TLS/DNS + `npm install`, opt-in `M3OS_NODE_NET` skip-with-reason) | C, 86a, 86c | Planned |
| E | Docs + release closeout (learning doc, revived standalone roadmap, README rows, version bump) | B, C, D | Planned |

---

## Track A — Kernel Substrate Gaps for libuv + V8 JIT W^X

> The runtime substrate is **mostly already present** — Phase 86d (Go) cleared `mmap` `MAP_FIXED`+`PROT_NONE` arena commit, edge-triggered `epoll`/`epoll_pwait`, `eventfd2`, `pipe2`, `clone(CLONE_THREAD)`, and `SIGURG`/`tgkill`; Phase 75/76 gave W^X `mprotect` RW→RX + per-page demand-fault `PROT_EXEC`; Phase 86a gave `getrandom`/`AT_RANDOM`. A grep of `kernel/` finds **only two** libuv primitives missing: `timerfd_*` and `signalfd4`. Track A closes those and proves the V8 JIT code-page path on the existing W^X machinery — it does **not** rebuild the substrate.

### A.1 — Implement `timerfd_create` / `timerfd_settime` / `timerfd_gettime`

**Files:**
- `kernel/src/timerfd.rs` (new — backing object, modeled on `kernel/src/eventfd.rs`)
- `kernel/src/arch/x86_64/syscall/mod.rs` (`nr` constants + dispatch + `sys_timerfd_*` handlers)

**Symbol:** `timerfd_create` (`TIMERFD_CREATE=283`), `timerfd_settime` (`TIMERFD_SETTIME=286`), `timerfd_gettime` (`TIMERFD_GETTIME=287`); modeled on `sys_eventfd2` (`mod.rs:21228`) + `kernel/src/eventfd.rs:eventfd_create:65`
**Why it matters:** libuv's Linux backend (`src/unix/linux.c`) arms its timer wheel with a `timerfd` registered in the epoll set; without it libuv falls back to `epoll_wait` timeouts only on the `!HAVE_TIMERFD` build path, and Node's `setTimeout`/`setInterval` accuracy and the event-loop "due timer" wakeup degrade. A real `timerfd` fd is the supported path and the cleanest validation surface.

**Acceptance:**
- [x] A `timerfd` is a pollable fd: it becomes readable when the timer expires, a `read(2)` returns the `u64` expiration count and resets it, and it is selectable by `epoll_ctl`/`epoll_wait` (level- **and** edge-triggered, reusing the `EpollInterest.last_ready` watermark). **As-built:** `kernel/src/timerfd.rs` (object table modeled on `eventfd.rs`) + lazy readiness (`timerfd_readable`) wired into `fd_poll_events`, the read path, and register/deregister. A blocked `poll`/`epoll_wait` is woken on expiry by **clamping its block deadline to the timer's next-expiry tick** (`clamp_block_deadline` + `nearest_timerfd_deadline_*`) — the scheduler's IRQ-safe `wake_deadline` scanner, since `WaitQueue::wake_all` cannot run from the timer ISR. Live end-to-end proof rides `node-smoke` `NODE_TIMER_OK` (Track C).
- [x] `CLOCK_MONOTONIC` and `CLOCK_REALTIME` are accepted (`CLOCK_BOOTTIME`→monotonic); `TFD_NONBLOCK`/`TFD_CLOEXEC` creation flags and `TFD_TIMER_ABSTIME` are honored; one-shot **and** interval (`it_interval`) rearm both work. **As-built:** `sys_timerfd_create/settime/gettime` in `mod.rs`; abstime resolved via `clock_now_nanos(clockid)`; `itimerspec` ns↔tick rounding through `kernel_core::timerfd::{ns_to_ticks_ceil,ticks_to_ns}`.
- [x] Host-tested expiry-accounting logic lives in `kernel-core` (`kernel_core::timerfd`) with unit tests for the expiration-count and rearm math, mirroring how `kernel_core::epoll::evaluate_interest` factors the epoll edge logic. **As-built:** 11 host tests (`expirations`/`remaining`/`ns_to_ticks_ceil`/`ticks_to_ns`, incl. a property test that re-basing to `next` returns 0 until the next period); pass under `cargo xtask check`.
- [x] The dispatch arms are added to the `mod nr {}` block and the dispatch match with the correct Linux syscall numbers (`TIMERFD_CREATE=283`, `TIMERFD_SETTIME=286`, `TIMERFD_GETTIME=287`; `settime`'s 4th arg read from `r10`). **As-built + `cargo xtask check` green** (clippy `-D warnings`, rustfmt, host tests, retpoline gate).

### A.2 — `signalfd4`: implement, or validate + force the libuv self-pipe fallback

**File:** `kernel/src/arch/x86_64/syscall/mod.rs` (`SIGNALFD4=289` dispatch + handler **or** the documented fallback decision)
**Symbol:** `sys_signalfd4`; fallback reuses `PIPE2` (`mod.rs:1404`) + `sys_rt_sigaction`
**Why it matters:** libuv uses `signalfd` to deliver process signals through the epoll loop; m3OS has no `signalfd*`. libuv's `!HAVE_SIGNALFD` path is a self-pipe written from a `sigaction` handler — and `pipe2` + `rt_sigaction` are **both already present** — so Node can run without `signalfd` if the build advertises the fallback. The choice (implement vs. fall back) must be explicit, not accidental.

**Acceptance:**
- [x] **Decision recorded — option (b), the libuv self-pipe fallback (no kernel `signalfd4`).** Rationale: `PIPE2` (293) and `RT_SIGACTION` (13, `sys_rt_sigaction`) are **both already present**, so libuv's `!HAVE_SIGNALFD` self-pipe path (a `sigaction` handler writing a byte into a `pipe2` fd registered in the epoll set) runs on the existing substrate. Implementing a kernel `signalfd4` would add a new pollable-fd surface (siginfo dequeue, masked-set tracking) for a primitive libuv can do without — not worth the kernel risk for Phase 89. The absence of `signalfd` is a **documented supported configuration**, recorded here and in `docs/89-nodejs.md`. (libuv detects `signalfd` at *its* configure via feature macros and selects the self-pipe path under musl by default.)
- [ ] _(option (a) not taken — no kernel `signalfd4`.)_
- [ ] If (b): `node -e` delivering `SIGINT`/`SIGTERM`/`SIGCHLD` to a handler still fires (`process.on('SIGINT', …)`) — **live proof deferred to the `node-smoke` signal arm (Track C), gated on the Node build.** The substrate (`pipe2`+`rt_sigaction`) is confirmed present; the supported-config decision is recorded.

### A.3 — Validate the V8 JIT W^X code-page path + the libuv/V8 thread substrate (reuse audit)

**Files:**
- `kernel/src/arch/x86_64/syscall/mod.rs` (`sys_mprotect:11498`) — validation only
- `kernel/src/arch/x86_64/interrupts.rs` (`demand_map_user_page_locked:515`) — validation only
- `kernel/src/fs/ramdisk.rs` (`WX_VIOLATION_ELF:341`) — the existing JIT-pattern proof to lean on

**Symbol:** `sys_mprotect` (RW→RX flip + `vma_tree.update_range_prot` at `mod.rs:11644`), `sys_clone_thread` (`mod.rs:16088`), `sys_tkill`/`TGKILL` (`mod.rs:3264`)
**Why it matters:** V8's JIT writes machine code into an RW page then flips it executable; m3OS **rejects** `PROT_WRITE|PROT_EXEC` simultaneously (W^X), so V8 **must** run with code-space write-protection (`mprotect` RW↔RX toggling), which is exactly the path the `wx-violation` regression already proves green. This task confirms the kernel side carries V8's real workload and that no new kernel code is needed for JIT — it is the falsifiable check behind the phase's "JIT stresses execution permissions" learning goal. (The matching **build-side** choice — telling V8 to use write-protected code memory rather than RWX — is task B.2.)

**Acceptance:**
- [x] A note enumerates the already-present substrate Node reuses: `mprotect` RW→RX (`sys_mprotect`), demand-fault per-page `PROT_EXEC` (`interrupts.rs`), `mmap` `MAP_FIXED`+`PROT_NONE`, epoll/`epoll_pwait`/`eventfd2`/`pipe2`, `clone(CLONE_THREAD)` for the libuv threadpool, `SIGURG`/`tgkill` for async interrupts, `getrandom`/`AT_RANDOM` for the V8/OpenSSL entropy bootstrap — **plus the new `timerfd` (A.1)**. Verified present via grep + the green Go-runtime substrate (86d). **Recorded in `docs/89-nodejs.md`.**
- [x] The existing `wx-violation` gate (`SMOKE:wx-violation:PASS`) covers the RW→RX commit pattern (mmap RW → write → `mprotect(R|X)` succeeds; `mprotect(W|X)` EINVAL). **Key correction (scout research):** modern V8 **removed** the `mprotect` RW↔RX "write-protected code memory" JIT path — it is now PKU-or-RWX. m3OS forbids RWX, so the shipped config is **`--v8-lite-mode` (jitless, Track B.2)**, under which V8 allocates **zero** runtime executable memory (builtins embedded RX in `.text`). The "no `mprotect(W|X)` EINVAL" criterion is therefore satisfied **by construction** — V8 never requests executable memory at runtime. No new kernel code is needed for JIT.
- [x] No `PROT_WRITE|PROT_EXEC` RWX mapping is requested by the (jitless) Node binary — guaranteed by the V8 Lite Mode build choice (B.2), not by chance. Live confirmation (absence of an `mprotect(W|X)` EINVAL) rides the `node-smoke` serial log (Track C). PKU-backed JIT is a **tracked follow-up** (needs a kernel MPK/`pkey_mprotect` story).

---

## Track B — Node Port Build Pipeline

### B.1 — `ports/lang/node/Portfile` + `build_node` (host-clang C++ cross, fully-static musl)

**Files:**
- `ports/lang/node/Portfile` (new — pinned Node 22 LTS version + SHA-256, `CATEGORY=lang`, `DEPS=`)
- `xtask/src/port_build.rs` (new `fn build_node`; early-return branch in `fn port_build` after the `gh` branch at `:1326`; `port_deps` arm `:752`; `compute_port_key_inner` toolchain arm `:861`; `build_recipe_id` arm `:339` + its distinctness-test list `:4942`; `BUILDABLE_PORTS` `:1047`; `pub fn build_node_port` wrapper `:~4628`)

**Symbol:** `build_node` (modeled on `build_llvm` at `port_build.rs:3441` — host `clang`/`clang++` as the C++ cross-compiler, *not* the musl-gcc C-port path)
**Why it matters:** Node is C++17 built by V8's GYP/ninja; `musl-tools` ships no C++ compiler, so — exactly like the Clang port (85d) — `build_node` must drive the **host clang/clang++** with `--target=x86_64-linux-musl --sysroot=<musl>` and `-fuse-ld=lld`, not `musl_toolchain()`. m3OS's `ld-musl` has no real `libc.so`, so the binary must be **fully static** (no `lib-dynload`/`dlopen`), the same loaderless contract as static CPython/Go/Clang. Because build-host and target are both x86_64, V8's `mksnapshot` runs natively on the host (no qemu-user) — the one thing that makes a Node cross-build tractable here.

**Acceptance:**
- [ ] `ports/lang/node/Portfile` pins Node 22 LTS (exact `VERSION` + SHA-256 verified against `nodejs.org/dist/`), `CATEGORY=lang`, `DEPS=` empty (Node bundles OpenSSL/zlib/c-ares/nghttp2/llhttp/ICU; the static binary has no runtime `.m3pkg` deps) — and `port_deps` agrees with `"node" => &[]` at `port_build.rs:752`.
- [ ] `build_node` configures with `./configure --fully-static --enable-static --dest-cpu=x64 --dest-os=linux --cross-compiling` (only if needed; same-arch may not require it) pointing `CC_host`/`CXX_host` at host glibc clang and `CC`/`CXX` at `clang --target=x86_64-linux-musl --sysroot=<assembled musl sysroot>` with `-fuse-ld=lld`, then `make` (ninja), installing `node` (+ bundled `npm`/`npx`) into `<stage>/usr` — following the persistent-out-of-tree-build-dir pattern `build_llvm` uses for incrementality.
- [ ] `build_recipe_id("node")` returns a non-empty, distinct arm (the configure flags not in the Portfile) and `"node"` is added to the distinctness unit test list (`port_build.rs:4942`) so `build_recipe_id_is_distinct_and_nonempty_per_host_port` still passes; `compute_port_key_inner` folds `host_cxx_toolchain_id()` for `"node"` (joining the `llvm` arm at `:861`) so two host-clang versions don't collide on one `.m3pkg`.
- [ ] `cargo xtask port build node` produces a sealed `target/pkgcache/<key>.m3pkg`, and a second build is a pure pkgcache hit (zero compiler invocations, `PKGCACHE: hit`); `build_node_port()` and `"node"` in `BUILDABLE_PORTS` wire it into `port build all` (topo order) + `port list`.
- [ ] The resulting `node` binary is ELF `EXEC`/`DYN` fully static (no `PT_INTERP`), proven the same way `build_python`/`build_go` prove `-static` (no dynamic-loader segment).

### B.2 — Choose the W^X-compliant V8 code-memory model + ICU/bundled-deps configuration (the supported-config decision)

**Files:**
- `xtask/src/port_build.rs` (`build_node` V8/`configure` flags)
- `docs/89-nodejs.md` (records the chosen configuration + non-goals — see E.1)

**Symbol:** the `build_node` V8 GN/`configure` flag set (`v8_enable_write_protect_code_memory` / code-range write-protection; `--with-intl`; bundled-OpenSSL flags)
**Why it matters:** This is the phase's explicit "choose the supported Node.js configuration and document the non-goals" requirement, and the crux of the JIT learning goal. m3OS forbids RWX, so V8 must either (a) JIT with **write-protected code memory** (`mprotect` RW↔RX per code-page commit — the recommended primary, since A.3 proves the kernel path is green and the phase's learning goal is precisely this) or (b) run **`--jitless`** (Ignition interpreter only, no executable-code allocation — the documented fallback if the write-protect path proves unstable during bring-up). ICU and bundled crypto also have to be pinned: `npm`/`Intl` want ICU, so `--with-intl=small-icu` is the pragmatic middle (vs. `none` which breaks `Intl`, or `full-icu` which bloats the artifact).

**Acceptance:**
- [ ] **Primary path:** V8 is built so JIT'd code pages are committed RW then flipped RX via `mprotect` (write-protected code memory enabled; no RWX mapping) — confirmed by A.3's "no `mprotect(W|X)` EINVAL in the node-smoke log" check while real JS is JIT-compiled.
- [ ] **Fallback path documented:** if the write-protect path is deferred, the runtime ships configured for `--jitless` (or `NODE_OPTIONS=--jitless`), the perf cost is stated, and JIT is recorded as a tracked follow-up — *not* silently shipping RWX.
- [ ] `--with-intl=small-icu` (or an explicitly justified alternative) is pinned; `process.versions` reports the bundled OpenSSL/V8/ICU/uv/ares/nghttp2 versions and `node -e "new Intl.NumberFormat('en-US')"` does not throw.
- [ ] `docs/89-nodejs.md` documents the chosen configuration and the **non-goals**: no native addons / `node-gyp` (no on-device C++ toolchain dependency contract), no inspector/`--inspect` debugging, no RWX JIT, no `worker_threads` SharedArrayBuffer guarantees beyond what the static build provides — matching the phase doc's "Deferred Until Later".

### B.3 — `M3OS_WITH_NODE` opt-in image bundling

**Files:**
- `xtask/src/main.rs` (`fn populate_phase_69d_ports` — a new `M3OS_WITH_NODE` env-gated bundle block modeled byte-for-byte on the `M3OS_WITH_GH` block at `:20461`–`:20484`)

**Symbol:** the `M3OS_WITH_NODE` guard in `populate_phase_69d_ports`; the gate sets `std::env::set_var("M3OS_WITH_NODE","1")` (mirroring `clang-smoke` at `main.rs:16742`, `gh-smoke` at `:14964`)
**Why it matters:** a fully-static Node + bundled npm is a large artifact (≈90–110 MB, the heaviest in the tree); like Clang (`M3OS_WITH_CLANG`) and `gh` (`M3OS_WITH_GH`) it must be **gated out of default images** and bundled into `/usr/pkg/` (as `.m3pkg` + `.meta`, **not** pre-installed) only when the feature is on, so routine `cargo xtask image`/`run` stays lean.

**Acceptance:**
- [ ] With `M3OS_WITH_NODE` unset, the default image contains **no** `node.m3pkg` (and `PORTS`/`BUNDLE_ONLY_PORTS` are unchanged); image size is unaffected.
- [ ] With `M3OS_WITH_NODE=1`, `populate_phase_69d_ports` reads `pkgcache_artifact_path("node")`, `pkg_format::verify`s it, pushes `usr/pkg/node.m3pkg`, and writes `usr/pkg/node.meta` (`VERSION=… DEPS=`) — so the in-OS `pkg install node` exercises the real installer path against the bundled repo.
- [ ] The `node-smoke` gate (Track C) sets `M3OS_WITH_NODE=1` before building the image so the package is present for `pkg install node`.

---

## Track C — Local-Runtime Validation Gate

### C.1 — `node-smoke`: local runtime sentinels (fs / timers / console / process / event loop + loopback HTTP)

**Files:**
- `xtask/src/main.rs` (new `fn cmd_node_smoke` + `fn node_smoke_steps`, modeled on `cmd_go_runtime_smoke` at `:14750`; a new `SMOKE_EXIT_NODE_SMOKE_FAILED` const after `SMOKE_EXIT_GH_SMOKE_FAILED=80` at `:242`)
- `xtask/src/main.rs` (`populate_ext2_files` — bundle `/usr/src/node-probe.js` fixtures + a `/usr/src/node-http.js` loopback probe)
- `AGENTS.md` (opt-in regression row `M3OS_NODE_REGRESSION=1`, appended after the `gh-smoke` row at `:121`)
- `.githooks/pre-push` (a `M3OS_NODE_REGRESSION` block after the `gh` block at `:547`)

**Symbol:** `cmd_node_smoke`, `node_smoke_steps`
**Why it matters:** proves the Node runtime actually *runs* on m3OS for the documented local workloads before any network is involved — the disciplined "make local features work first" milestone step. It reuses the serial `SmokeStep` DSL (`enum SmokeStep` at `:6358`), `boot_and_login_steps` (`:23160`), and the `WaitPassOrFail` heavy-`pkg install` step exactly as the Go/Python/Clang gates do, and pins `-smp 1` (like Go/gh) to avoid SMP races during bring-up.

**Acceptance:**
- [ ] The gate boots m3OS, `pkg install node` succeeds (`WaitPassOrFail` pass=`pkg install: node: OK`, fail=`pkg install: cannot`), then `node --version` reports the pinned 22.x version (asserted in **output**, not the echoed command, per the scrollback-clearing `Send` convention).
- [ ] `node /usr/src/node-probe.js` emits, over serial: `NODE_HELLO_OK` (interpreter + V8 start, `AT_RANDOM`/`getrandom` bootstrap), `NODE_FS_OK` (`fs.writeFileSync`/`readFileSync` round-trip a `/tmp` file with byte-compare), `NODE_TIMER_OK` (`setTimeout` + `setInterval` + `setImmediate` fire in order — exercising the A.1 `timerfd`/event-loop wakeup), `NODE_PROC_OK` (`process.argv`/`env`/`platform==='linux'`/`pid`/`versions`), and `NODE_EVENTLOOP_OK` (a `Promise`/microtask + `queueMicrotask` + nextTick ordering check). The `.js` fixtures are written via `populate_ext2_files`; the gate force-recreates the data disk each run.
- [ ] `node /usr/src/node-http.js` runs an in-process `http.createServer` + `http.get` over `127.0.0.1` (loopback through the in-kernel TCP stack — no SLIRP, no DNS, no TLS) and prints `NODE_HTTP_OK` on a 200 round-trip, proving libuv's TCP + event-loop integration end-to-end.
- [ ] The gate is wired opt-in: `AGENTS.md` has an `M3OS_NODE_REGRESSION=1` row (full parenthetical describing build/boot/install/sentinels/timeout/skip-with-reason), and `.githooks/pre-push` runs `cargo xtask node-smoke --timeout 5400` when `M3OS_NODE_REGRESSION=1` (clang-class timeout — the ~100 MB install + cold static-binary load over the ring-3 VFS take tens of minutes). Absent a host C++ toolchain (clang/cmake/ninja), `build_node_port()` errors and the gate prints `SKIP (reason: …)` and returns success — mirroring the `clang-smoke`/`python-smoke` build-precondition SKIP.

---

## Track D — Networked Runtime + npm

### D.1 — Plaintext SLIRP HTTP client over the in-kernel TCP stack (always-on networked sentinel)

**Files:**
- `xtask/src/main.rs` (`cmd_node_smoke` — host `TcpListener` + `guestfwd` rewrite, copied from `cmd_go_runtime_smoke` at `:14820`)
- `xtask/src/main.rs` (`/usr/src/node-http-egress.js` fixture)

**Symbol:** the `guestfwd=tcp:10.0.2.100:80-tcp:127.0.0.1:{port}` netdev rewrite + `node_smoke_steps`
**Why it matters:** the Go gate's plaintext-egress pattern (a host `TcpListener` serving a fixed 200, reached at the on-link SLIRP IP `10.0.2.100:80` with a literal IP — no DNS, no TLS, no real egress) is the *always-on* proof that Node's libuv TCP client traverses the m3OS network stack to a server outside the guest. It rides in the same `node-smoke` gate so it is never skipped.

**Acceptance:**
- [ ] `cmd_node_smoke` binds a host `TcpListener` on `127.0.0.1:0` serving `HTTP/1.1 200 OK` + a fixed body, rewrites the QEMU netdev to `guestfwd=tcp:10.0.2.100:80-tcp:127.0.0.1:{http_port}`, and the guest runs `node /usr/src/node-http-egress.js http://10.0.2.100:80/`.
- [ ] The probe prints `NODE_EGRESS_OK` on receiving the fixed body over the in-kernel TCP stack (literal IP → no DNS, no TLS) — the always-on networked sentinel, asserted in the same run as the Track C local sentinels.

### D.2 — TLS + DNS + `npm install` over HTTPS (opt-in `M3OS_NODE_NET`, skip-with-reason)

**Files:**
- `xtask/src/main.rs` (`cmd_node_smoke` — `let attempt_net = std::env::var("M3OS_NODE_NET").is_ok_and(|v| v=="1")`, modeled on `M3OS_GIT_HTTPS_NET` at `:16165`; adds `+rdrand,+rdseed` CPU flags when set, like the TLS gates, for the OpenSSL DRBG)
- `xtask/src/main.rs` (`node_smoke_steps(attempt_net, …)` — appends the live arms + the skip NOTE)
- `AGENTS.md` (document `M3OS_NODE_NET=1` in the `M3OS_NODE_REGRESSION` row's parenthetical)

**Symbol:** `attempt_net` gating in `cmd_node_smoke` / `node_smoke_steps`
**Why it matters:** this is the **critical, non-deferrable** package path the Phase 90 CLI-agent milestone depends on — `npm install` must work over real HTTPS+DNS to `registry.npmjs.org`. Node bundles its own OpenSSL, so TLS rides Node's crypto validated against the Phase 86a CA bundle (`/etc/ssl/certs/ca-certificates.crt` via `NODE_EXTRA_CA_CERTS` / `--use-openssl-ca`); DNS uses Node's bundled c-ares against the Phase 86 resolver. Like every networked gate, the live arm is opt-in (a registry fetch needs real egress, never CI/secret-bound) and **skip-with-reason** when unset — mirroring `tls-smoke`/`git-https-smoke`.

**Acceptance:**
- [ ] **Always-on (no network):** the gate asserts `npm --version` reports the bundled npm version and `node -e` constructs a `tls`/`crypto` object + `dns` module load without throwing (the TLS/DNS stacks *link and initialize*), even when `M3OS_NODE_NET` is unset.
- [ ] **Opt-in live arm (`M3OS_NODE_NET=1`):** `node -e "https.get('https://…')"` validates a real TLS 1.3 cert chain + hostname against the CA bundle (a known-good host), and `npm install <small dependency-free package>` (e.g. `is-number`) into a `/tmp` project succeeds and `require()`s the installed module — proving DNS → TLS → registry fetch → tarball unpack → `node_modules` resolution end-to-end over the in-kernel stack. *(These bullets stay `[ ]` even once implemented: the live result is `M3OS_NODE_NET`-gated → SKIP without real egress.)*
- [ ] **Bad-cert REJECT (opt-in):** an HTTPS GET to a self-signed/expired host fails closed (TLS rejects before any body), proving Node's cert verification is on by default — mirroring the `git-https-smoke` bad-cert arm.
- [ ] The gate is **skip-with-reason** when `M3OS_NODE_NET` is unset: it prints `node-smoke: NOTE — the TLS/DNS/npm-install network arms are SKIPPED (set M3OS_NODE_NET=1 …)` and exits success. The npm registry / `NODE_EXTRA_CA_CERTS` / `cafile` configuration is documented; PAT/private-registry auth is documented but not exercised by the anonymous positive arm.

---

## Track E — Documentation + Release Closeout

### E.1 — Create the Phase 89 learning doc + fix the stale Phase-87 Node claim

**Files:**
- `docs/89-nodejs.md` (new — aligned learning-doc template, modeled on `docs/86-networking-and-github.md`)
- `docs/README.md` (link it in the `### Phase-Aligned Learning Docs` table after the Phase 86 row at `:72`)
- `docs/86-networking-and-github.md` (`:402` — fix the stale `Node.js is Phase 87, Claude Code is Phase 88` bullet to `Phase 89` / `Phase 90`)

**Symbol:** a learning doc following the aligned-legacy template (`docs/appendix/doc-templates.md:167`–`214`); header block per `docs/86-networking-and-github.md:1-6` (`**Aligned Roadmap Phase:** Phase 89` / `**Status:** … / **Source Ref:** phase-89`)
**Why it matters:** every phase ships a learning doc (the roadmap's "Required Documentation for Every Phase" rule); this one teaches how a JIT-heavy managed runtime stresses W^X/execution-permissions differently from static CLIs, how libuv builds on the epoll/threading/`timerfd` substrate, the chosen Node configuration + non-goals, and the TLS/DNS/npm package path — the four learning goals named in the phase doc.

**Acceptance:**
- [ ] `docs/89-nodejs.md` exists, follows the aligned learning-doc template sections (`## Overview` → `## What This Doc Covers` → `## Core Implementation` → `## Key Files` table → `## How This Phase Differs From Later Runtime Work` → `## Related Roadmap Docs` → `## Deferred or Later-Phase Topics`), and explains the V8 W^X/JIT model, the libuv `timerfd`/`signalfd` decisions, the static-musl build, and the npm path in learner-friendly terms with the disk/RAM budget (the Node `.m3pkg` size is the **measured** value once built).
- [ ] It is linked from `docs/README.md`'s `### Phase-Aligned Learning Docs` table (a `| [Node.js](./89-nodejs.md) | 89 | … |` row after `:72`) and cross-links the Phase 89 design + task docs.
- [ ] `docs/86-networking-and-github.md:402` no longer claims "Node.js is Phase 87, Claude Code is Phase 88" (corrected to Phase 89 / Phase 90).

### E.2 — Revive the standalone Node.js roadmap

**Files:**
- `docs/nodejs-roadmap.md` (new — revived from `docs/archived/nodejs-roadmap.md`)
- `docs/README.md` (`### Standalone Roadmaps` row at `:98` — repoint from `./archived/nodejs-roadmap.md` to `./nodejs-roadmap.md`)

**Symbol:** the `> Revived YYYY-MM-DD for **Phase 89 — Node.js**.` blockquote prelude (modeled on `docs/python-roadmap.md:1-8` / `docs/git-roadmap.md`)
**Why it matters:** the phase doc's "Related Documentation" requires `docs/nodejs-roadmap.md`; the live standalone roadmap is the per-tool narrative cross-compilation strategy (Mermaid dependency flowchart + "why Node is hard"), complementary to the master phase index. The archived copy already holds the porting plan — reviving it matches the Python/git/clang precedent rather than writing a new one.

**Acceptance:**
- [ ] `docs/nodejs-roadmap.md` exists, opening with a `> Revived … for **Phase 89 — Node.js**.` blockquote pointing at the live phase + task docs, then the `# Road to Node.js on m3OS` body carried from the archived copy and reconciled with the as-built configuration (static musl, V8 W^X model, bundled OpenSSL, npm path).
- [ ] `docs/README.md`'s Standalone Roadmaps row points at `./nodejs-roadmap.md` with a "revived for Phase 89" note (mirroring the Python/git rows), no longer the archived path.

### E.3 — Update the roadmap README row, the design doc's task-list link + Evaluation-Gate fix, and the AGENTS.md inventory

**Files:**
- `docs/roadmap/README.md` (`:472` — the existing Phase 89 row: last cell `Deferred until implementation planning` → `[Tasks](./tasks/89-nodejs-tasks.md)`; Status flips Planned → Complete on landing)
- `docs/roadmap/89-nodejs.md` (Companion Task List → link this doc; Evaluation Gate "Phases 59 and 60" artifact → "Phases 85 and 86")
- `AGENTS.md` (`:7` kernel version line; `:17` capability bullet)

**Symbol:** the README Status/Tasks cells; the AGENTS.md "Package management" toolchain bullet
**Why it matters:** `docs/roadmap/README.md` is the authoritative phase index and AGENTS.md is the always-loaded capability inventory; both must reflect the landed runtime. Per the AGENTS.md "keep it small" maintenance policy, Node.js is the **same capability class** as Go/Python/Clang (a cross-compiled language runtime delivered through the substrate), so it **folds into the existing toolchain bullet** — it does **not** get a new capability bullet.

**Acceptance:**
- [ ] `docs/roadmap/README.md:472` Tasks cell links `[Tasks](./tasks/89-nodejs-tasks.md)`; Status reads `Complete` (and Primary Outcome is sharpened to name the npm/TLS path) when the phase lands.
- [ ] `docs/roadmap/89-nodejs.md`'s **Companion Task List** section links `[Phase 89 Task List](./tasks/89-nodejs-tasks.md)` (replacing "defer until implementation planning begins"), and the **Evaluation Gate** "Toolchain and network baseline" row references **Phases 85 and 86** (not the copy-paste "Phases 59 and 60").
- [ ] The AGENTS.md `:17` toolchain bullet is rewritten to fold in Node.js (a clause beside git/Python/Clang/Go), the `M3OS_NODE_REGRESSION` opt-in gate row is present in the regression table, and **no** new capability bullet is added (per "prefer rewriting an existing bullet").

### E.4 — Bump kernel crate `0.88.0` → `0.89.0`

**File:** `kernel/Cargo.toml`
**Symbol:** `[package] version = "0.89.0"` (currently `0.88.0` at `kernel/Cargo.toml:3`)
**Why it matters:** Phase 89 is the next post-1.0 minor; the version bump is how the phase's landing is recorded in the boot banner and `uname` (both derive from `env!("CARGO_PKG_VERSION")`), and the `node-smoke` boot banner asserting `v0.89.0` is the cheap proof the cut shipped.

**Acceptance:**
- [ ] `kernel/Cargo.toml` line 3 reads `version = "0.89.0"` (+ `Cargo.lock` updated), and `AGENTS.md:7` reads `kernel **v0.89.0**`.
- [ ] `cargo xtask check` is clean (clippy `-D warnings` + rustfmt + host tests incl. the new `kernel_core::timerfd` tests + the `build_recipe_id` distinctness test + retpoline gate); exit 0.
- [ ] The `node-smoke` boot banner / `uname` reports `0.89.0`.

---

## Documentation Notes

- **What changed relative to the standalone roadmap.** `docs/nodejs-roadmap.md` (revived from the archive in E.2) is the per-tool porting narrative; this task doc is the per-phase work breakdown. Node.js lands as the **same capability class** as Go/Python/Clang (a static, cross-compiled language runtime delivered as a content-addressed `.m3pkg`), so it folds into the existing AGENTS.md toolchain bullet rather than adding a new one.
- **Most of the runtime substrate already exists — reuse, don't rebuild.** Phase 86d (Go) and Phase 75/76 already delivered everything V8/libuv needs *except* `timerfd_*` and `signalfd4`: `mmap` `MAP_FIXED`+`PROT_NONE` (`mod.rs:10863`), `mprotect` RW→RX + VMA-prot update (`mod.rs:11498`,`:11644`), per-page demand-fault `PROT_EXEC` (`interrupts.rs:515`,`:542`), edge-triggered epoll/`epoll_pwait` (`mod.rs:21181`+), `eventfd2` (`:21228`), `pipe2` (`:1404`), `clone(CLONE_THREAD)` (`:16088`), `SIGURG`/`tgkill` (`:3264`), `getrandom`/`AT_RANDOM` (`:16604`, `mm/elf.rs:669`). The JIT W^X commit pattern is already a green regression (`SMOKE:wx-violation:PASS`). Track A is therefore deliberately small.
- **Honesty / explicit non-goals.** No native addons / `node-gyp`, no `--inspect` debugger, no RWX JIT (write-protected code memory or `--jitless` only), no guaranteed multi-core `worker_threads` SMP semantics during bring-up (single-core `-smp 1` like Go/gh). `signalfd4` may be a *documented absence* satisfied by libuv's self-pipe fallback rather than a kernel feature (A.2). The TLS/DNS/`npm install` arms are real but **opt-in** (`M3OS_NODE_NET=1`) and skip-with-reason in CI — they require real egress, which can never be CI-bound; the docs must state these are deferred-in-CI, not present-but-broken.
- **Hazards to call out in the as-built notes.** (1) V8 `mksnapshot` runs on the build host — same arch (x86_64) makes this tractable, but it links the **host** glibc while the final `node` is **musl-static**, so the host-tool toolchain and target toolchain must be kept distinct (the classic Node-on-Alpine-from-glibc-host split). (2) V8 must not request RWX — the build flag choice (B.2) is what prevents an `mprotect(W|X)` EINVAL at runtime; verify by the absence of that EINVAL in the node-smoke log. (3) The artifact is the heaviest in the tree (≈100 MB) — `M3OS_WITH_NODE`-gated out of default images, clang-class `--timeout 5400` gate.
- **Prefer exact targets.** Reference the exact `configure`/GN flags (`--fully-static --enable-static`, the V8 write-protect / `--jitless` flag, `--with-intl=small-icu`) and the exact `port_build.rs`/`main.rs` symbols above, not "the Node build" or "the port system".
- **Cross-links.** Companion design doc: [Phase 89 — Node.js](../89-nodejs.md). Substrate predecessor: [Phase 86d — Go-Runtime Gate](./86d-go-runtime-tasks.md) (the managed-runtime kernel blockers this phase reuses). Packaging substrate: [Phase 85a](./85a-package-infrastructure-tasks.md) (`.m3pkg` + offline `pkg`). Consumer: Phase 90 (Claude Code) depends on the npm path delivered in Track D.
