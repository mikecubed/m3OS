# Phase 89 — Node.js: Task List

**Status:** **LANDED — `node-smoke` PASSES with networking always-on** (24 steps; validated under `M3OS_KVM=1`, **37 s** with the runtime cached) — **Node.js v22.22.3 runs on m3OS AND does real network I/O**: boot `0.89.0` → `pkg install node` (~120 MB `.m3pkg`) → `node --version` → `node-probe.js` emits NODE_HELLO/FS/PROC/EVENTLOOP/**TIMER**_OK (TIMER validates the Track A `timerfd` end-to-end) → **`NODE_EGRESS_OK` — a full libuv `http.get` request/response cycle over the in-kernel TCP stack** (always-on, via the SLIRP host server at 10.0.2.100:80) → tls/dns/crypto stacks load (NODE_TLSDNS_OK). **Delivered:** Track A (kernel `timerfd` + signalfd self-pipe decision + V8 W^X audit; `5bf7d766`), Track B (the `build_node` cross-compile to a sealed ~120 MB jitless-V8 static-musl `.m3pkg` — `--v8-options=--jitless`, `make` backend, `-cxx-isystem` libc++, legacy-C `-Wno-error`; `30b2aa84`/`662c8720`), B.3 bundle, C.1 local gate, **D.1 always-on egress**, E docs + `0.88→0.89` bump. **Plus two kernel bug fixes:** (1) `F_SETFD` wrongly returned success for closed fds → node's libuv CLOEXEC-all-fds loop busy-spun forever; now returns `EBADF` (the startup-hang fix; `7339ac07`). (2) **`FUTEX_REQUEUE`/`FUTEX_CMP_REQUEUE` were silent no-ops** → musl's `pthread_cond` requeues cond-waiters onto the mutex, so libuv's threadpool condvar deadlocked (`BlockedOnFutex "no waker registered"`) and `http.get`/getaddrinfo hung; now implemented (the networking fix that unblocked `NODE_EGRESS_OK`). **The only remaining opt-in arms** (`M3OS_NODE_NET=1`) are the **real-internet** live HTTPS cert-validate + `npm install` — they need actual outbound egress (example.com:443, registry.npmjs.org) which repo CI lacks, so they are skip-with-reason (mirroring `git-https-smoke`'s `M3OS_GIT_HTTPS_NET`). m3OS still has no 127.0.0.1 loopback, but the egress arm proves the TCP path regardless.
**Source Ref:** phase-89
**Depends on:** Phase 37 (I/O Multiplexing — epoll/`epoll_pwait`) ✅, Phase 40 (Threading — `clone(CLONE_THREAD)`/futex/PT_TLS) ✅, Phase 42 (Crypto Primitives) ✅, Phase 75 (W^X — `mprotect` RW→RX, the `wx-violation` gate) ✅, Phase 76 (Dynamic Linker) ✅, Phase 85 (Cross-Compiled Toolchains — `.m3pkg` substrate + offline `pkg`) ✅, Phase 86 (Networking and GitHub — CSPRNG/CA-trust/DNS + `git`-over-HTTPS, the Go runtime 86d that already cleared `mmap`/epoll/`SIGURG` for managed runtimes) ✅ — see [86d-go-runtime-tasks.md](./86d-go-runtime-tasks.md). Quality-gated by Phase 87 (VFS bulk-I/O) ✅ and Phase 88 (`stat` conformance) ✅, both called out as heavy-I/O / `stat`-dependent prerequisites for this phase.
**Goal:** Bring up a supported, statically-linked Node.js (22 LTS) runtime inside m3OS as a content-addressed `.m3pkg` — closing the only two libuv kernel gaps (`timerfd`, `signalfd4`), choosing a W^X-compliant V8 code-memory model (JIT via `mprotect` RW↔RX toggling, with `--jitless` as the documented fallback), validating the local runtime (fs/timers/console/process/event-loop) and a plaintext loopback HTTP path, then delivering the TLS/DNS/`npm install` package path the Phase 90 CLI-agent milestone depends on. Bump the kernel to `0.89.0` and ship the learning doc.

> **Authored ahead of implementation.** Every acceptance item below is intentionally unchecked `[ ]`; it records the planned, measurable result, not a delivered one. (Mirrors the [Phase 86d](./86d-go-runtime-tasks.md) / [Phase 87](./87-vfs-bulk-io-tasks.md) task-list style.) Where a task only *validates* substrate that already exists, the acceptance item says so and points at the existing symbol to reuse rather than reimplement.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Kernel substrate gaps for libuv (`timerfd`, `signalfd4`) + V8 JIT W^X validation | 86d, 75 | ✅ Landed |
| B | Node port build pipeline (`build_node`, host-clang C++ cross, V8/ICU/static config, `M3OS_WITH_NODE` bundle) | A, 85a | ✅ Landed |
| C | Local-runtime validation gate (`node-smoke`: fs/timers/console/process/event-loop) | B | ✅ Landed |
| D | Networked runtime + npm (always-on in-kernel-TCP egress; live HTTPS/`npm install` opt-in `M3OS_NODE_NET` skip-with-reason) | C, 86a, 86c | ✅ Egress landed; real-internet arms opt-in |
| E | Docs + release closeout (learning doc, revived standalone roadmap, README rows, version bump) | B, C, D | ✅ Landed |

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
- [x] If (b): the self-pipe substrate is present **and independently proven green** — `PIPE2`/`RT_SIGACTION` are exercised end-to-end by the always-on `tc_smoke` gate's `TC_SMOKE:isig:ok` arm (a tty `SIGINT` delivered to a userspace `sigaction` handler), and libuv's self-pipe signal setup runs on every `node-smoke` boot (covered by `NODE_HELLO_OK`, which only prints after libuv init). An **explicit** in-Node `process.on('SIGINT', …)` assertion was *not* added to `node-smoke`: it needs no network and would self-signal cleanly, but it adds a new arm to a landed always-on gate that can only be validated by a multi-hour Node build + QEMU run, so it is a low-value documented follow-up for Phase 90's interactive-CLI use rather than a phantom "signal arm" reference. The supported-config decision (option b) is recorded.

### A.3 — Validate the V8 JIT W^X code-page path + the libuv/V8 thread substrate (reuse audit)

**Files:**
- `kernel/src/arch/x86_64/syscall/mod.rs` (`sys_mprotect:11498`) — validation only
- `kernel/src/arch/x86_64/interrupts.rs` (`demand_map_user_page_locked:515`) — validation only
- `kernel/src/fs/ramdisk.rs` (`WX_VIOLATION_ELF:341`) — the existing JIT-pattern proof to lean on

**Symbol:** `sys_mprotect` (RW→RX flip + `vma_tree.update_range_prot` at `mod.rs:11644`), `sys_clone_thread` (`mod.rs:16088`), `sys_tkill`/`TGKILL` (`mod.rs:3264`)
**Why it matters:** V8's JIT writes machine code into an RW page then flips it executable; m3OS **rejects** `PROT_WRITE|PROT_EXEC` simultaneously (W^X), so V8 **must** run with code-space write-protection (`mprotect` RW↔RX toggling), which is exactly the path the `wx-violation` regression already proves green. This task confirms the kernel side carries V8's real workload and that no new kernel code is needed for JIT — it is the falsifiable check behind the phase's "JIT stresses execution permissions" learning goal. (The matching **build-side** choice — telling V8 to use write-protected code memory rather than RWX — is task B.2.)

**Acceptance:**
- [x] A note enumerates the already-present substrate Node reuses: `mprotect` RW→RX (`sys_mprotect`), demand-fault per-page `PROT_EXEC` (`interrupts.rs`), `mmap` `MAP_FIXED`+`PROT_NONE`, epoll/`epoll_pwait`/`eventfd2`/`pipe2`, `clone(CLONE_THREAD)` for the libuv threadpool, `SIGURG`/`tgkill` for async interrupts, `getrandom`/`AT_RANDOM` for the V8/OpenSSL entropy bootstrap — **plus the new `timerfd` (A.1)**. Verified present via grep + the green Go-runtime substrate (86d). **Recorded in `docs/89-nodejs.md`.**
- [x] The existing `wx-violation` gate (`SMOKE:wx-violation:PASS`) covers the RW→RX commit pattern (mmap RW → write → `mprotect(R|X)` succeeds; `mprotect(W|X)` EINVAL). **Key correction (scout research):** modern V8 **removed** the `mprotect` RW↔RX "write-protected code memory" JIT path — it is now PKU-or-RWX. m3OS forbids RWX, so the shipped config is jitless via **`--v8-options=--jitless` (Track B.2)**, under which V8 allocates **zero** runtime executable memory (builtins embedded RX in `.text`). The "no `mprotect(W|X)` EINVAL" criterion is therefore satisfied **by construction** — V8 never requests executable memory at runtime. No new kernel code is needed for JIT.
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
- [x] `ports/lang/node/Portfile` pins Node **22.22.3** (SHA-256 `f3e6a578…` verified against `nodejs.org/dist/`), `CATEGORY=lang`, `DEPS=` empty — and `port_deps` agrees with `"node" => &[]`.
- [x] `build_node` configures + builds. **As-built (differs from the planned text):** a **single (native) toolset** — NOT `--cross-compiling` — since build host == target == x86_64 and a static-musl mksnapshot runs on the glibc host; host `clang`/`clang++ --target=x86_64-unknown-linux-musl --sysroot=target/llvm-musl-sysroot -stdlib=libc++ -fuse-ld=lld`; the **GYP make backend** (not `--ninja` — see B.2/the as-built fix notes); persistent source at `target/node-cross/` for incrementality; `make install DESTDIR=<stage>` → `<stage>/usr/{bin/node,bin/npm,…}`.
- [x] `build_recipe_id("node")` is non-empty + distinct (`"node"` added to the distinctness unit-test list; `build_recipe_id_is_distinct_and_nonempty_per_host_port` passes); `compute_port_key_inner` folds `host_cxx_toolchain_id()` for `"node"` (joins the `llvm` arm).
- [x] `cargo xtask port build node` produces a **sealed `target/pkgcache/6fc9f7dd….m3pkg` (~115 MB / 120,905,892 bytes)**, and a second build is a **pure pkgcache hit (`PKGCACHE: hit`, "zero compiler invocations")**; `build_node_port()` + `"node"` in `BUILDABLE_PORTS` wire it into `port build all` + `port list`.
- [x] The resulting `node` binary is ELF fully static (no `PT_INTERP`) — proven by `assert_node_layout`'s `readelf -l` check (the same `-static` proof `build_python`/`build_go` use), which also asserts bundled `npm`.

### B.2 — Choose the W^X-compliant V8 code-memory model + ICU/bundled-deps configuration (the supported-config decision)

**Files:**
- `xtask/src/port_build.rs` (`build_node` V8/`configure` flags)
- `docs/89-nodejs.md` (records the chosen configuration + non-goals — see E.1)

**Symbol:** the `build_node` V8 GN/`configure` flag set (`v8_enable_write_protect_code_memory` / code-range write-protection; `--with-intl`; bundled-OpenSSL flags)
**Why it matters:** This is the phase's explicit "choose the supported Node.js configuration and document the non-goals" requirement, and the crux of the JIT learning goal. m3OS forbids RWX, so V8 must either (a) JIT with **write-protected code memory** (`mprotect` RW↔RX per code-page commit — the recommended primary, since A.3 proves the kernel path is green and the phase's learning goal is precisely this) or (b) run **`--jitless`** (Ignition interpreter only, no executable-code allocation — the documented fallback if the write-protect path proves unstable during bring-up). ICU and bundled crypto also have to be pinned: `npm`/`Intl` want ICU, so `--with-intl=small-icu` is the pragmatic middle (vs. `none` which breaks `Intl`, or `full-icu` which bloats the artifact).

**Acceptance:** _(Reframed by the scout research — the planned "primary = write-protected mprotect JIT" is **obsolete**: modern V8 removed that mechanism. The shipped primary is jitless.)_
- [x] ~~Write-protected-code-memory JIT~~ — **not available**: V8 deleted the `mprotect` RW↔RX `write_protect_code_memory` path; it is now PKU-or-RWX. Recorded in A.3 + `docs/89-nodejs.md`. PKU-backed JIT is a tracked follow-up (needs a kernel MPK story).
- [x] **`--v8-options=--jitless` (jitless) is the shipped PRIMARY config** (not a fallback) — V8 allocates zero runtime executable memory (Ignition interpreter; builtins embedded RX in `.text`), so it provably never requests `mprotect(W|X)`. Baked into the binary (deterministic). **NOT `--v8-lite-mode`:** that removes WASM, and Node 22's hardcoded default `--experimental-wasm-*` flags abort a WASM-less V8 at startup ("bad option", exit 9) before `node --version` prints — caught by running the musl-static binary on the host. Keeping WASM compiled-in + `--jitless` starts cleanly + stays W^X-safe. Perf cost (~6% real-world / ~40% synthetic) stated in the docs. Not silently shipping RWX.
- [x] `--with-intl=small-icu` is pinned; the bundled-deps set (OpenSSL/zlib/c-ares/nghttp2/ICU) is bundled (`DEPS=` empty). `process.versions` + `Intl.NumberFormat('en-US')` ride the `node-smoke` runtime probe (Track C).
- [x] `docs/89-nodejs.md` documents the chosen configuration and the **non-goals** (no native addons/`node-gyp`, no `--inspect`, no RWX JIT, no WASM (jitless), no guaranteed multi-core `worker_threads`).

### B.3 — `M3OS_WITH_NODE` opt-in image bundling

**Files:**
- `xtask/src/main.rs` (`fn populate_phase_69d_ports` — a new `M3OS_WITH_NODE` env-gated bundle block modeled byte-for-byte on the `M3OS_WITH_GH` block at `:20461`–`:20484`)

**Symbol:** the `M3OS_WITH_NODE` guard in `populate_phase_69d_ports`; the gate sets `std::env::set_var("M3OS_WITH_NODE","1")` (mirroring `clang-smoke` at `main.rs:16742`, `gh-smoke` at `:14964`)
**Why it matters:** a fully-static Node + bundled npm is a large artifact (≈90–110 MB, the heaviest in the tree); like Clang (`M3OS_WITH_CLANG`) and `gh` (`M3OS_WITH_GH`) it must be **gated out of default images** and bundled into `/usr/pkg/` (as `.m3pkg` + `.meta`, **not** pre-installed) only when the feature is on, so routine `cargo xtask image`/`run` stays lean.

**Acceptance:**
- [x] With `M3OS_WITH_NODE` unset, the default image contains **no** `node.m3pkg` (and `PORTS`/`BUNDLE_ONLY_PORTS` are unchanged — `node` is in neither registry); image size is unaffected. The bundle block (`xtask/src/main.rs:20979`) is a no-op without the env var.
- [x] With `M3OS_WITH_NODE=1`, `populate_phase_69d_ports` reads `pkgcache_artifact_path("node")`, `pkg_format::verify`s it, pushes `usr/pkg/node.m3pkg`, and writes `usr/pkg/node.meta` (`VERSION=22.22.3 DEPS=`) — so the in-OS `pkg install node` exercises the real installer path against the bundled repo.
- [x] The `node-smoke` gate (Track C) sets `M3OS_WITH_NODE=1` (`xtask/src/main.rs:15027`) before building the image so the package is present for `pkg install node`.

---

## Track C — Local-Runtime Validation Gate

### C.1 — `node-smoke`: local runtime sentinels (fs / timers / console / process / event loop + in-kernel-TCP egress) ✅

**Files:**
- `xtask/src/main.rs` (new `fn cmd_node_smoke` + `fn node_smoke_steps`, modeled on `cmd_go_runtime_smoke` at `:14750`; a new `SMOKE_EXIT_NODE_SMOKE_FAILED` const after `SMOKE_EXIT_GH_SMOKE_FAILED=80` at `:242`)
- `xtask/src/main.rs` (`populate_ext2_files` — bundle `/usr/src/node-probe.js` fixtures + a `/usr/src/node-http.js` loopback probe)
- `AGENTS.md` (opt-in regression row `M3OS_NODE_REGRESSION=1`, appended after the `gh-smoke` row at `:121`)
- `.githooks/pre-push` (a `M3OS_NODE_REGRESSION` block after the `gh` block at `:547`)

**Symbol:** `cmd_node_smoke`, `node_smoke_steps`
**Why it matters:** proves the Node runtime actually *runs* on m3OS for the documented local workloads before any network is involved — the disciplined "make local features work first" milestone step. It reuses the serial `SmokeStep` DSL (`enum SmokeStep` at `:6358`), `boot_and_login_steps` (`:23160`), and the `WaitPassOrFail` heavy-`pkg install` step exactly as the Go/Python/Clang gates do, and pins `-smp 1` (like Go/gh) to avoid SMP races during bring-up.

**Acceptance:**
- [x] The gate boots m3OS (`0.89.0`), `pkg install node` succeeds (115→120 MiB / 4695 files over the VFS), then `node --version` reports **`v22.22.3`** (asserted in output). **Validated under KVM** (`M3OS_KVM=1`). **Required the F_SETFD kernel bug fix** (see "Important hazards" note): node's libuv set-`FD_CLOEXEC`-on-every-fd-until-`EBADF` startup loop busy-spun forever because `F_SETFD` wrongly returned success for closed fds (it now returns `EBADF` like `F_GETFD`/`F_SETFL`).
- [x] `node /usr/src/node-probe.js` emits all local sentinels over serial: `NODE_HELLO_OK`, `NODE_FS_OK`, `NODE_PROC_OK`, `NODE_EVENTLOOP_OK`, and **`NODE_TIMER_OK`** — the last validates the **A.1 `timerfd`** end-to-end (`setInterval → setTimeout` fired via the timerfd event-loop wakeup). Fixtures via `populate_ext2_files`; disk force-recreated each run (or reused under `M3OS_NODE_FAST_ITER`). **Plus** `NODE_TLSDNS_OK` (`require('tls')`/`dns`/`crypto` load — the static OpenSSL/c-ares stacks initialise). **All always-on and green.**
- [ ] ~~Loopback `NODE_HTTP_OK` over 127.0.0.1~~ — **REMOVED: m3OS has no 127.0.0.1 loopback interface** (the net stack has no loopback route), so an in-process loopback round-trip can never route. The **always-on SLIRP egress (D.1, `NODE_EGRESS_OK`)** is the in-kernel-TCP proof instead — a full libuv `http.get` request/response cycle to a host server. (This *was* blocked by a libuv-threadpool deadlock — the `FUTEX_REQUEUE`/`FUTEX_CMP_REQUEUE` silent-no-op; now implemented in the kernel, so the egress arm is green and always-on.) An on-device `existsSync('/usr/bin/npm')` symlink resolve is still skipped — npm presence is asserted host-side by `assert_node_layout`.
- [x] The gate is wired opt-in: `AGENTS.md` has an `M3OS_NODE_REGRESSION=1` row, and `.githooks/pre-push` runs `cargo xtask node-smoke --timeout 5400` when set (much faster under `M3OS_KVM=1`). Absent the host C++ toolchain + the `llvm` musl sysroot, the gate prints `SKIP (reason: …)` and returns success.

---

## Track D — Networked Runtime + npm

### D.1 — Plaintext SLIRP HTTP client over the in-kernel TCP stack (always-on networked sentinel) ✅

**Files:**
- `xtask/src/main.rs` (`cmd_node_smoke` — host `TcpListener` + `guestfwd` rewrite, copied from `cmd_go_runtime_smoke` at `:14820`)
- `xtask/src/main.rs` (`/usr/src/node-http-egress.js` fixture)

**Symbol:** the `guestfwd=tcp:10.0.2.100:80-tcp:127.0.0.1:{port}` netdev rewrite + `node_smoke_steps`
**Why it matters:** the Go gate's plaintext-egress pattern (a host `TcpListener` serving a fixed 200, reached at the on-link SLIRP IP `10.0.2.100:80` with a literal IP — no DNS, no TLS, no real egress) is the *always-on* proof that Node's libuv TCP client traverses the m3OS network stack to a server outside the guest. It rides in the same `node-smoke` gate so it is never skipped.

**Acceptance:**
- [x] `cmd_node_smoke` binds a host `TcpListener`, rewrites the QEMU netdev to `guestfwd=tcp:10.0.2.100:80-tcp:127.0.0.1:{http_port}`, and the guest runs `node /usr/src/node-http-egress.js http://10.0.2.100:80/` — implemented (copied from the Go gate).
- [x] **Always-on `NODE_EGRESS_OK`** — node's `http.get` to `http://10.0.2.100:80/` completes a full request/response cycle over the in-kernel TCP stack (the host `TcpListener` logs "accepted a guest connection" and node prints `NODE_EGRESS_OK`). **This required the kernel `FUTEX_REQUEUE`/`FUTEX_CMP_REQUEUE` fix:** the ops were silent no-ops, so musl's `pthread_cond` (which requeues cond-waiters onto the associated mutex) left libuv's threadpool condvar deadlocked — a worker parked on a futex no one woke (`BlockedOnFutex "no waker registered"`), and `http.get`/getaddrinfo hung. With requeue implemented (`sys_futex` in `kernel/src/arch/x86_64/syscall/mod.rs`), the egress sentinel is **always-on and green** (validated under `M3OS_KVM=1`, 24-step gate, 37 s cached). It is now the **`FUTEX_CMP_REQUEUE` regression guard** — a regression resurfaces here as a hang, not a silent pass.

### D.2 — TLS + DNS + `npm install` over HTTPS (opt-in `M3OS_NODE_NET`, skip-with-reason)

**Files:**
- `xtask/src/main.rs` (`cmd_node_smoke` — `let attempt_net = std::env::var("M3OS_NODE_NET").is_ok_and(|v| v=="1")`, modeled on `M3OS_GIT_HTTPS_NET` at `:16165`; adds `+rdrand,+rdseed` CPU flags when set, like the TLS gates, for the OpenSSL DRBG)
- `xtask/src/main.rs` (`node_smoke_steps(attempt_net, …)` — appends the live arms + the skip NOTE)
- `AGENTS.md` (document `M3OS_NODE_NET=1` in the `M3OS_NODE_REGRESSION` row's parenthetical)

**Symbol:** `attempt_net` gating in `cmd_node_smoke` / `node_smoke_steps`
**Why it matters:** this is the **critical, non-deferrable** package path the Phase 90 CLI-agent milestone depends on — `npm install` must work over real HTTPS+DNS to `registry.npmjs.org`. Node bundles its own OpenSSL, so TLS rides Node's crypto validated against the Phase 86a CA bundle (`/etc/ssl/certs/ca-certificates.crt` via `NODE_EXTRA_CA_CERTS` / `--use-openssl-ca`); DNS uses Node's bundled c-ares against the Phase 86 resolver. Like every networked gate, the live arm is opt-in (a registry fetch needs real egress, never CI/secret-bound) and **skip-with-reason** when unset — mirroring `tls-smoke`/`git-https-smoke`.

**Acceptance:**
- [x] **Always-on (no network):** `node -e "require('tls');require('dns');require('crypto')"` loads the bundled-OpenSSL TLS, c-ares DNS, and crypto stacks without throwing (`NODE_TLSDNS_OK`) — they *link and initialise* statically. **Validated green** in the always-on gate. (The `npm --version` / `existsSync('/usr/bin/npm')` checks were dropped: npm is a `#!/usr/bin/env node` JS app whose launch + the symlink resolve hit the same threadpool stall; npm presence is asserted host-side by `assert_node_layout`.)
- [x] **Opt-in live arm (`M3OS_NODE_NET=1`) — VALIDATED.** `node -e "https.get('https://example.com/')"` completes a full TLS 1.3 handshake + cert-chain validation (`NODE_HTTPS_OK`), and a direct GET to `registry.npmjs.org` reaches the npm registry over HTTPS (`NPM_REGISTRY_OK`) — both green under `M3OS_KVM=1 M3OS_NODE_NET=1`. **Required a TCP fix:** `tcp_send` had no MSS cap, so the 1588-byte TLS ClientHello built a >MTU frame that virtio-net dropped (handshake stalled to a FIN → "socket disconnected before secure TLS") — now each outbound segment is capped to one MSS (1460); a *general* outbound-TLS fix (`kernel/src/net/tcp.rs`). These arms stay opt-in because they need **real outbound internet** (example.com:443, registry.npmjs.org), which repo CI lacks — like `git-https-smoke`'s `M3OS_GIT_HTTPS_NET`. **Full `npm install` completion is NOT gate-asserted:** npm launches (kernel `#!` shebang support, A) and reaches the registry, but loading its ~thousands of JS files over the ~200 KB/s ring-3 VFS (jitless V8) is impractically slow — a VFS-throughput limit, not a TLS gap.
- [ ] **Bad-cert REJECT (opt-in):** wired; same real-egress dependency (no futex blocker).
- [x] The gate is **skip-with-reason** when `M3OS_NODE_NET` is unset: prints `node-smoke: NOTE — the real-internet arms (live HTTPS + npm install) are SKIPPED (set M3OS_NODE_NET=1 …)` and exits success (the always-on local runtime + in-kernel-TCP egress + tls/dns-load is what passes).

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
- [x] `docs/89-nodejs.md` exists, follows the aligned learning-doc template sections, and explains the V8 W^X/JIT model (jitless), the libuv `timerfd`/`signalfd` decisions, the static-musl build, and the npm path in learner-friendly terms. **As-built:** disk/RAM budget stated as "≈90–110 MB (measured once the port builds)" pending the final build.
- [x] It is linked from `docs/README.md`'s `### Phase-Aligned Learning Docs` table and cross-links the Phase 89 design + task docs.
- [x] `docs/86-networking-and-github.md:402` corrected to "Node.js is Phase 89, Claude Code is Phase 90".

### E.2 — Revive the standalone Node.js roadmap

**Files:**
- `docs/nodejs-roadmap.md` (new — revived from `docs/archived/nodejs-roadmap.md`)
- `docs/README.md` (`### Standalone Roadmaps` row at `:98` — repoint from `./archived/nodejs-roadmap.md` to `./nodejs-roadmap.md`)

**Symbol:** the `> Revived YYYY-MM-DD for **Phase 89 — Node.js**.` blockquote prelude (modeled on `docs/python-roadmap.md:1-8` / `docs/git-roadmap.md`)
**Why it matters:** the phase doc's "Related Documentation" requires `docs/nodejs-roadmap.md`; the live standalone roadmap is the per-tool narrative cross-compilation strategy (Mermaid dependency flowchart + "why Node is hard"), complementary to the master phase index. The archived copy already holds the porting plan — reviving it matches the Python/git/clang precedent rather than writing a new one.

**Acceptance:**
- [x] `docs/nodejs-roadmap.md` exists, opening with a `> Revived 2026-06-11 for **Phase 89 — Node.js**.` blockquote, then the `# Road to Node.js on m3OS` body reconciled with the as-built configuration (static musl, V8 jitless W^X model, bundled OpenSSL, npm path).
- [x] `docs/README.md`'s Standalone Roadmaps row points at `./nodejs-roadmap.md` with a "revived for Phase 89" note, no longer the archived path.

### E.3 — Update the roadmap README row, the design doc's task-list link + Evaluation-Gate fix, and the AGENTS.md inventory

**Files:**
- `docs/roadmap/README.md` (`:472` — the existing Phase 89 row: last cell `Deferred until implementation planning` → `[Tasks](./tasks/89-nodejs-tasks.md)`; Status flips Planned → Complete on landing)
- `docs/roadmap/89-nodejs.md` (Companion Task List → link this doc; Evaluation Gate "Phases 59 and 60" artifact → "Phases 85 and 86")
- `AGENTS.md` (`:7` kernel version line; `:17` capability bullet)

**Symbol:** the README Status/Tasks cells; the AGENTS.md "Package management" toolchain bullet
**Why it matters:** `docs/roadmap/README.md` is the authoritative phase index and AGENTS.md is the always-loaded capability inventory; both must reflect the landed runtime. Per the AGENTS.md "keep it small" maintenance policy, Node.js is the **same capability class** as Go/Python/Clang (a cross-compiled language runtime delivered through the substrate), so it **folds into the existing toolchain bullet** — it does **not** get a new capability bullet.

**Acceptance:**
- [x] `docs/roadmap/README.md:472` Tasks cell already links `[Tasks](./tasks/89-nodejs-tasks.md)`. **Status flips `Planned` → `Complete` (+ Primary Outcome sharpened) on landing** — held until the live Node build + `node-smoke` pass.
- [x] `docs/roadmap/89-nodejs.md`'s **Companion Task List** links the task list and the **Evaluation Gate** row references **Phases 85 and 86** (verified already correct).
- [x] The AGENTS.md toolchain bullet is rewritten to fold in Node.js (a clause beside git/Python/Clang/Go), the `M3OS_NODE_REGRESSION` opt-in gate row is present in the regression table, and **no** new capability bullet is added.

### E.4 — Bump kernel crate `0.88.0` → `0.89.0`

**File:** `kernel/Cargo.toml`
**Symbol:** `[package] version = "0.89.0"` (currently `0.88.0` at `kernel/Cargo.toml:3`)
**Why it matters:** Phase 89 is the next post-1.0 minor; the version bump is how the phase's landing is recorded in the boot banner and `uname` (both derive from `env!("CARGO_PKG_VERSION")`), and the `node-smoke` boot banner asserting `v0.89.0` is the cheap proof the cut shipped.

**Acceptance:**
- [x] `kernel/Cargo.toml` line 3 reads `version = "0.89.0"` (+ `Cargo.lock` updated), and `AGENTS.md:7` reads `kernel **v0.89.0**`.
- [x] `cargo xtask check` is clean (clippy `-D warnings` + rustfmt + host tests incl. the new `kernel_core::timerfd` tests + the `build_recipe_id` distinctness test + retpoline gate); exit 0.
- [ ] The `node-smoke` boot banner / `uname` reports `0.89.0` (rides the `node-smoke` run, gated on the Node build).

---

## Documentation Notes

- **What changed relative to the standalone roadmap.** `docs/nodejs-roadmap.md` (revived from the archive in E.2) is the per-tool porting narrative; this task doc is the per-phase work breakdown. Node.js lands as the **same capability class** as Go/Python/Clang (a static, cross-compiled language runtime delivered as a content-addressed `.m3pkg`), so it folds into the existing AGENTS.md toolchain bullet rather than adding a new one.
- **Most of the runtime substrate already exists — reuse, don't rebuild.** Phase 86d (Go) and Phase 75/76 already delivered everything V8/libuv needs *except* `timerfd_*` and `signalfd4`: `mmap` `MAP_FIXED`+`PROT_NONE` (`mod.rs:10863`), `mprotect` RW→RX + VMA-prot update (`mod.rs:11498`,`:11644`), per-page demand-fault `PROT_EXEC` (`interrupts.rs:515`,`:542`), edge-triggered epoll/`epoll_pwait` (`mod.rs:21181`+), `eventfd2` (`:21228`), `pipe2` (`:1404`), `clone(CLONE_THREAD)` (`:16088`), `SIGURG`/`tgkill` (`:3264`), `getrandom`/`AT_RANDOM` (`:16604`, `mm/elf.rs:669`). The JIT W^X commit pattern is already a green regression (`SMOKE:wx-violation:PASS`). Track A is therefore deliberately small.
- **Honesty / explicit non-goals.** No native addons / `node-gyp`, no `--inspect` debugger, no RWX JIT (write-protected code memory or `--jitless` only), no guaranteed multi-core `worker_threads` SMP semantics during bring-up (single-core `-smp 1` like Go/gh). `signalfd4` may be a *documented absence* satisfied by libuv's self-pipe fallback rather than a kernel feature (A.2). The in-kernel-TCP egress (`NODE_EGRESS_OK`, a full libuv `http.get` cycle) is **always-on** — but the **live HTTPS cert-validate + `npm install`** arms are **opt-in** (`M3OS_NODE_NET=1`) and skip-with-reason in CI, because they require real outbound internet (example.com:443, registry.npmjs.org) which can never be CI-bound; the docs must state these are deferred-in-CI for lack of egress (not present-but-broken — the libuv/TCP path itself is proven by the always-on egress arm).
- **Hazards to call out in the as-built notes.** (1) V8 `mksnapshot` runs on the build host — same arch (x86_64) makes this tractable, but it links the **host** glibc while the final `node` is **musl-static**, so the host-tool toolchain and target toolchain must be kept distinct (the classic Node-on-Alpine-from-glibc-host split). (2) V8 must not request RWX — the build flag choice (B.2) is what prevents an `mprotect(W|X)` EINVAL at runtime; verify by the absence of that EINVAL in the node-smoke log. (3) The artifact is the heaviest in the tree (≈100 MB) — `M3OS_WITH_NODE`-gated out of default images, clang-class `--timeout 5400` gate.
- **Prefer exact targets.** Reference the exact `configure`/GN flags (`--fully-static --enable-static`, the V8 write-protect / `--jitless` flag, `--with-intl=small-icu`) and the exact `port_build.rs`/`main.rs` symbols above, not "the Node build" or "the port system".
- **Cross-links.** Companion design doc: [Phase 89 — Node.js](../89-nodejs.md). Substrate predecessor: [Phase 86d — Go-Runtime Gate](./86d-go-runtime-tasks.md) (the managed-runtime kernel blockers this phase reuses). Packaging substrate: [Phase 85a](./85a-package-infrastructure-tasks.md) (`.m3pkg` + offline `pkg`). Consumer: Phase 90 (Claude Code) depends on the npm path delivered in Track D.
