# Claude Code

**Aligned Roadmap Phase:** Phase 90
**Status:** Complete
**Source Ref:** phase-90

## Overview

Phase 90b runs **Claude Code** — Anthropic's CLI coding agent — natively inside
m3OS as a content-addressed `.m3pkg`. **The delivered milestone is install +
launch + headless `claude -p` + the interactive primitives + the rendering
interactive TUI**, on *both* the CI-viable jitless Node (Phase 89) and the JIT
Node (Phase 90a). This is the post-1.0 developer platform's integration
capstone: a non-trivial modern Node application that needs HTTPS to a live API,
subprocess management, raw-mode terminal I/O, git, and — for the interactive TUI
— a JIT runtime and a WebAssembly layout engine, exercised together rather than
as isolated synthetic probes. The headline outcome is `cargo xtask
claude-smoke`, which **PASSES** on m3OS: an always-on offline core that `pkg
install claude-code` (solving `DEPS=node` dependency-first) → `claude --version`
(= `2.1.112`) → `claude --help` → the vendored static-pie `rg --version` → the
A.2 `NODE_SIGINT_OK`/`NODE_SPAWN_OK`/`NODE_RAWMODE_OK` interactive-substrate
probes + a `SEG_OK` `Intl.Segmenter` guard — **27/27 on the jitless full-install
gate** (the CI-viable default, no KVM/PKU needed) and on the **90a JIT node**
(`M3OS_CLAUDE_JIT=1`, KVM/PKU-gated), plus opt-in live arms — an authenticated
`claude -p` round-trip **verified working end-to-end** (a `<<<579>>>` answer over
a real TLS request/response, confirmed by a host-side packet capture) and a
real-filesystem agent workflow asserted by `cat`. **And the interactive `claude` TUI renders on the
90a JIT node** — proven by an automated QMP/PPM render arm
(`claude_tui_render_arm`): it launches `claude` in the graphical `term`,
screendumps, and asserts **592 changed band scanlines** vs the empty-prompt
baseline (threshold 20; a blank screen ≈ 0); the captured screenshot shows the
rendered "Welcome to Claude Code v2.1.112" welcome screen with the
yoga.wasm-laid-out logo splash, with no credential or network needed. Two fixes
made the TUI render: the W^X-v2 cross-thread PKU read-recovery kernel fix
(needed for `cli.js` to *launch*) and the node build's switch from
`--with-intl=small-icu` to **full-icu** (small-icu omits the ICU break-iterator
data `Intl.Segmenter` needs for grapheme segmentation, which had null-deref'd
V8's `JSSegments::Create` — see *The A.1 WASM/TUI decision* below). (The early
"24/24" figure was an `M3OS_CLAUDE_FAST_ITER` reuse-disk run.) The kernel bumps
`0.90.0` → `0.90.1` (Phase 90a took the `0.90.0` minor; this sub-phase takes the
patch, mirroring the 86a–f sub-phase pattern).

Phase 90b was **planned as "no kernel work"**, but the integration test falsified
that — running a real multi-threaded, network-driven Node application surfaced a
cluster of kernel gaps the earlier phases' single-process synthetic probes never
reached, turning the phase into a genuine kernel-hardening pass (~1.7k kernel
lines, every change driven by a reproduced Claude Code symptom and regression-gated).
The headline is a W^X-v2 cross-thread PKU read-recovery fix (the roadmap had
pre-flagged it as the "SMP-PKU follow-up"); it unblocked `cli.js` *launch* on both
node variants. Carrying it to a completed `claude -p` round-trip and a rendering
TUI then took per-address-space futex keys, an `EXT2_VOLUME` yielding lock, an
Enter-key-CR keymap fix, `MAX_FDS`/heap-fd-table/`/dev/tty`/TCP-keepalive
ABI fixes, and an SMP-hardening cluster behind the new `smp-smoke` gate — all
enumerated in *Kernel fixes the integration test surfaced* below. The rest of the
runtime substrate is delivered by Phase 89 (static Node 22, the `timerfd` event
loop, the libuv threadpool `FUTEX_CMP_REQUEUE` fix, always-on in-kernel-TCP egress)
and Phase 90a (PKU-backed W^X v2 + a JIT Node variant on which `yoga.wasm`
instantiates). This phase's deliverable is packaging, environment pinning,
credential handling, the bring-up kernel fixes, and — most importantly — an honest,
falsifiable supported-workflow boundary. The one Phase 89 leftover it closes is the
explicitly deferred in-Node interactive substrate (`NODE_SIGINT_OK` and the
`NODE_SPAWN_OK`/`NODE_RAWMODE_OK` raw-mode/spawn probes), now validated in the
`claude-smoke` always-on core.

## What This Doc Covers

- The **supported-workflow decision** and the **native-binary divergence** — why
  the pin is `@anthropic-ai/claude-code@2.1.112` and not `latest`.
- The **install path** — a pre-bundled `.m3pkg` (fetch + stage host-side, seal,
  install offline from `/usr/pkg/`) and why live `npm install -g` is *not* the
  supported path.
- The **runtime dependency chain** — `claude-code` (`DEPS=node`) → the bundled
  Node runtime (jitless by default; the 90a JIT variant for the TUI) → the
  `/usr/bin/claude` launcher → the Phase 86a CA bundle.
- **The kernel fixes the integration test surfaced** — the W^X-v2 cross-thread
  PKU read-recovery headline (what unblocked `cli.js` *launch* on both node
  variants), plus the per-address-space futex keys, `EXT2_VOLUME` yielding lock,
  Enter-key-CR, `MAX_FDS`/`/dev/tty`/TCP-keepalive ABI, and SMP-hardening fixes
  that carried it to a completed `claude -p` round-trip and a rendering TUI.
- **The small-icu→full-icu node build fix** — why the interactive TUI needed it
  (small-icu lacked the ICU break-iterator data `Intl.Segmenter` needs →
  `JSSegments::Create` null-deref), and why the `mremap`/`io_uring` syscalls in
  the interim trace were red herrings.
- The **`/usr/bin/claude` `#!/usr/bin/env node` launcher** (a CJS wrapper that
  pins the supported env *in-process* and runs `cli.js` via dynamic `import()` —
  *not* a `#!/bin/sh` script, because m3OS's `/bin/sh` is `ion`), each pinned
  env var with one sentence of *why*.
- The **credential posture** — subscription-first 0600 OAuth-token/key seeding,
  headless onboarding, and the in-OS `/login` paste-flow.
- The **A.1 WASM/TUI decision** and the **ripgrep static-pie finding**.
- The **supported-workflow boundary** stated falsifiably, with the explicit
  non-goals matching the design doc's Deferred section.

## Core Implementation

### The supported-workflow decision and the native-binary divergence

The single most important decision in this phase is the **version pin**.
Claude Code's `latest` line (2.1.177 at authoring time) does **not** run on the
Phase 89/90a Node runtime at all: at version **2.1.113** Anthropic repackaged
claude-code into a **native Bun/JavaScriptCore single-file binary** — a
per-platform, ~500 MB native executable that the npm wrapper's `install.cjs`
copies over a stub. That model ships **no `cli.js` and does not use the Node
runtime**, so pinning `latest` would invalidate the entire `DEPS=node` +
Phase 89/90a JIT-Node dependency chain this phase is built on (running the native
Bun binary on m3OS would be a separate future port — a Bun runtime, not Node).

**`2.1.112` is the last version shipping the classic model** the phase assumes:

- `cli.js` — a 9.3 MB (gzipped; ~13.7 MB unpacked) JavaScript bundle that runs
  on `node`, with the **WebAssembly TUI layout engine (`yoga`) embedded inside it**
  (in 2.1.112; the 1.x/2.0 bundles shipped a standalone `yoga.wasm` file). The
  embedded WASM is the reason the JIT variant is required (see below).
- `vendor/ripgrep/` — a vendored platform `rg` for the file-search tool.

The pinned tarball (Track A host-side spike, 2026-06-13):

- **Version:** `@anthropic-ai/claude-code@2.1.112`
- **URL:** `https://registry.npmjs.org/@anthropic-ai/claude-code/-/claude-code-2.1.112.tgz`
- **SHA256:** `84379969ea53a0e5fd231a8f77debe4c7cb17dd971f4809d10d33f9aeca5de09`
  (~18.7 MB tarball, ~49 MB unpacked)

Pinning 2.1.112 is the faithful, correct way to deliver the phase as designed.
The host spike validated it: `node cli.js --version` → `2.1.112 (Claude Code)`
(exit 0, 0.26 s / 178 MB RSS under host node v24), and `--help` renders the full
CLI. Running the native Bun binary (2.1.113+) on m3OS is explicit future /
out-of-scope work.

### The install path: a pre-bundled `.m3pkg`, not live `npm install -g`

The supported, reproducible install path is the same one every heavy port uses:
**fetch + stage the npm tarball host-side, seal it as a content-addressed
`.m3pkg`, and install offline from `/usr/pkg/`** (Track B). `build_claude_code`
downloads the pinned registry tarball SHA-verified, stages the package payload
under `<stage>/usr/lib/claude-code/`, stages the `/usr/bin/claude` launcher, and
seals a `target/pkgcache/<key>.m3pkg` whose `DEPS=node` makes the in-OS solver
auto-install the Phase 89/90a runtime — the same dependency-first proof
`pkg install tmux`→`libevent` established in Phase 85a. No live network inside
the guest is needed to install.

Live `npm install -g @anthropic-ai/claude-code` is deliberately **not** the
supported path. Phase 89 D.2 proved npm *launches* and *reaches the registry*
over real HTTPS, but loading npm's ~thousands of tiny JS files over the
~200 KB/s ring-3 VFS made full `npm install` completion impractical — a per-file
`open`/`stat`/`write` round-trip-latency limit the Phase 87 bulk-I/O coalescing
does not remove (87's win is on large sequential files; npm's workload is the
opposite shape) — and repo CI has no outbound egress anyway. Live
`npm install -g` survives only as a **documented opt-in real-internet arm**, not
the supported path.

### The runtime dependency chain

```
claude-code (.m3pkg, DEPS=node)
  └─> node (.m3pkg)  ← jitless (Phase 89) by default; the 90a JIT variant under M3OS_CLAUDE_JIT=1
        └─> /usr/bin/claude launcher  (#!/usr/bin/env node CJS wrapper, in-process import())
              └─> /etc/ssl/certs/ca-certificates.crt  ← the Phase 86a Mozilla CA bundle
```

The default bundle is the **Phase 89 jitless node**. `cli.js`'s entire CLI
surface — `--version`, `--help`, headless `claude -p`, the vendored search tool,
the A.2 interactive primitives — runs on it: jitless V8 uses the Ignition
interpreter and allocates *zero* runtime executable memory, so it never touches
the PKU/W^X JIT path, and the always-on core is therefore **CI-viable under
plain TCG** (no KVM, no PKU). The **interactive TUI** needs runtime WASM
(`yoga.wasm`, embedded in `cli.js` in 2.1.112), which requires the **Phase 90a
JIT variant** (V8 JIT + WASM under PKU-backed W^X v2); jitless V8 cannot
instantiate WebAssembly. `M3OS_CLAUDE_JIT=1` selects that variant, and because it
requires PKU that arm is **KVM/PKU-gated exactly like `node-jit-smoke`**
(SKIP-with-reason without `M3OS_KVM=1` on a PKU host — on a no-PKU CPU the JIT
node aborts at its first code-space commit, it does not degrade to jitless). On
that JIT variant the **interactive TUI renders** (proven by the automated
QMP/PPM render arm — see *The A.1 WASM/TUI decision*), once the node build was
switched from `small-icu` to **full-icu** so `Intl.Segmenter` has the ICU
break-iterator data the TUI's grapheme segmentation needs. The
`M3OS_WITH_CLAUDE` image block bundles **both**
`.m3pkg`s (claude-code *and* the node variant being tested) so the offline
solver can resolve `DEPS=node` in-guest against the right runtime.

### The `/usr/bin/claude` launcher: a `#!/usr/bin/env node` CJS wrapper

The launcher is where the supported configuration is *pinned* rather than hoped
for — but it is emphatically **not** a `#!/bin/sh` shell script. The first gate
runs surfaced exactly why: m3OS's `/bin/sh` is **`ion`**, which — unlike POSIX
`sh` — intercepts `--version` and prints its own banner instead of running the
shebang script body (and the alternative built-in shell `sh0` ignores `argv`
entirely). A `#!/bin/sh` launcher that tried to `exec node cli.js "$@"` therefore
never reached `cli.js` for `claude --version`. **The one interpreter m3OS runs
correctly with flag arguments is node itself** — the `#!/usr/bin/env node` path
that npm's own bin shims ride.

So `/usr/bin/claude` is a **`#!/usr/bin/env node` CommonJS wrapper**: it pins the
supported environment **in-process** (`process.env.…`) and then runs `cli.js`
via a dynamic `import()` — a **single node process, no fork/exec of a second
node**. It relies on the Phase 89 `#!` shebang support to launch node, and on
node resolving its own argv with flags (the path npm rides). The install layout
is relocated under `/usr/lib/claude-code/`, and `cli.js` resolves the `vendor/`
tools relative to its own dir (the WASM TUI engine is embedded in `cli.js`).

| Pinned in the wrapper | Why |
|---|---|
| `process.env.NODE_EXTRA_CA_CERTS = '/etc/ssl/certs/ca-certificates.crt'` | Node's bundled OpenSSL validates `api.anthropic.com` against the Phase 86a Mozilla CA bundle — m3OS has no system trust-store discovery, so the path is pinned explicitly. Set in-process; node reads it lazily when the root store is first built, which covers the opt-in TLS arm. |
| `process.env.DISABLE_AUTOUPDATER = '1'` | The sealed `.m3pkg` is the only supported delivery; auto-update over the VFS is impractical and would version-drift the artifact away from the pinned, sealed content. |
| `process.env.CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC = '1'` | Suppresses telemetry / Statsig / Sentry egress attempts — dead weight and a confusing failure mode on a box with no default outbound egress. |
| `await import('/usr/lib/claude-code/cli.js')` | Runs the bundled `cli.js` **in the same node process** (no second fork/exec); the relocated `/usr/lib/claude-code/` layout keeps the `vendor/` tools resolvable (the WASM TUI engine is embedded in `cli.js`). |

`claude --version` on m3OS prints `2.1.112` *through this wrapper*, proving the
shebang→node→in-process-`import()` chain + relocated install layout end-to-end —
asserted in the Track D always-on core.

### Kernel fixes the integration test surfaced (the W^X-v2 PKU read-recovery headline + more)

Phase 90b was planned with **no kernel work**: the runtime was supposed to be
fully delivered by Phases 89 + 90a, leaving only packaging. The integration test
falsified that assumption in the best possible way — running a real multi-threaded,
network-driven Node application (`cli.js`) exercised kernel paths that Phase 89/90a's
synthetic probes never touched, and each gap it surfaced is exactly what an
integration phase exists to find. The headline is a W^X-v2 cross-thread PKU gap
the roadmap had even pre-flagged as the "SMP-PKU follow-up"; it is described in
full below, and the **other bring-up fixes are enumerated after it**. None of
these weaken an existing invariant — they are robustness/ABI-conformance fixes on
paths the earlier phases' single-process synthetic probes did not reach.

**The bug.** A real-world Node process (`cli.js`) allocates a write-deny
protection key for its V8 code space, then spawns worker/background threads
(`clone_thread` ×5). **PKRU is per-thread.** A sibling thread that was created
*before* the key existed inherits a PKRU in which that key is access-disabled, so
when it DATA-reads the now-pkey-tagged **executable** V8 code page, the CPU
raises a `PROTECTION_KEY` page fault:

```
userspace page fault: pid=… err=(PROTECTION_VIOLATION | USER_MODE | PROTECTION_KEY) rip=0x1b898b1 — process killed
```

m3OS kills the faulting process, so `claude --version` crashed before `cli.js`
finished booting. (The fault header was buried in the trace-ring dump; an
`M3OS_SERIAL_LOG` full-serial tee captured it.) This is *not* a JIT-only failure
— it reproduces identically on the jitless node and the JIT node, because both
run the real multi-threaded `cli.js`.

**The fix.** The W^X-v2 invariant only needs **writes** gated per-thread-window;
**read + execute of guarded code is process-wide** (every thread of a process
must be able to run its own code pages). So on a `PROTECTION_KEY` **read** fault
against a *present*, *executable* page carrying a non-zero protection key, the
page-fault handler now grants the faulting thread read access — it clears that
key's access-disable bit in the thread's *live* PKRU (the context-switch XSAVE
persists it across switches) and retries the instruction. Crucially:

- **Writes stay gated.** `CAUSED_BY_WRITE` faults are *excluded* from the grant,
  so the W^X-v2 write-deny window is intact — W^X is not weakened.
- **Data-isolation pages are never granted.** The grant requires the page to be
  **executable**; non-executable access-deny *data* pages (the PKU data
  isolation that `pku-smoke` exercises) fall through to the kill path unchanged.

This fix unblocks `cli.js` on **both** the jitless node (where it's the actual
blocker — jitless still spawns the same threads and tags the same code pages) and
the JIT node. See `kernel/src/arch/x86_64/interrupts.rs` (`page_fault_handler` +
a new `leaf_pte_flag_bits` helper) and `kernel/src/arch/x86_64/pkru.rs` (the new
`grant_read_access`).

#### The other bring-up fixes (the rest of the "no kernel work" correction)

The PKU read-recovery got `cli.js` to *launch*; carrying it from launch to a
rendering interactive TUI and a completed authenticated `claude -p` round-trip
surfaced a further set of kernel fixes. They are robustness/ABI-conformance
changes (no invariant relaxed), each landed with the symptom that found it:

- **`EXT2_VOLUME` → `YieldingMutex`** (`kernel/src/fs/ext2.rs`). `claude -p`'s
  concurrent demand-paged `exec(rg)`/`stat` startup storm wedged the single core:
  a task sleeping in virtio-blk I/O *while holding* the plain-spinlock ext2 volume
  lock could never be rescheduled to release it, because a second task busy-spun
  the only core on `EXT2_VOLUME.lock()`. The lock now `yield_now()`s on contention
  (uncontended boot-mount fast path unchanged). Same fix also corrects a Phase 57e
  deadline-IPC lost-wake (`recv/call_msg_with_deadline` registered the waker after
  the pending-message recheck).
- **Per-address-space futex keys** (`kernel/src/arch/x86_64/syscall/mod.rs`).
  PRIVATE futexes were keyed `(0, uaddr)` — a single global root — so two
  identical-layout `node` subprocesses (Claude spawns several) whose libuv
  threadpool condvars sit at the same virtual address aliased into one wait queue
  and stole each other's wakes. Now keyed by the active page-table root (CR3);
  `CLONE_THREAD` siblings share it, distinct processes don't. `node-smoke` runs
  one node process so it never collided — which is why this only surfaced here.
- **Enter key emits CR not LF** (`kernel-core/src/input/keymap.rs`). The graphical
  keymap mapped `KEY_ENTER` to LF; a raw-mode TUI (which clears `ICRNL` and watches
  for `\r`) never saw a submit, leaving a typed prompt stuck in the input box.
  `KEY_ENTER => '\r'` aligns the graphical path with the serial/`term`/DOOM paths;
  cooked-mode shell is unchanged (`ICRNL` still converts CR→LF).
- **`MAX_FDS` 32 → 128 + heap-backed fd table** (`kernel/src/process/mod.rs`).
  Claude Code opened enough fds to hit `EMFILE`; widening the table moved it
  onto the heap, which in turn fixed a node `clone`/`fork` kernel-stack overflow
  from the larger on-stack table (regression-guarded by `kstack-overflow-smoke`).
- **`/dev/tty` in the `stat` path** (`kernel/src/arch/x86_64/syscall/mod.rs`).
  A missing `/dev/tty` stat case froze the machine system-wide.
- **TCP keepalive socket options accepted/stored** (`kernel/src/net/mod.rs`).
  libuv treats *any* `setsockopt(SO_KEEPALIVE/TCP_KEEP*)` failure as fatal; the
  kernel now accepts and round-trips them (the *prober* is a documented future
  networking item — see Deferred). Guarded network-free by `connect-smoke`.
- **SMP hardening** (`kernel/src/smp/tlb.rs`, `scheduler.rs`, `serial.rs`,
  `arch/x86_64/interrupts.rs`). Running Claude on multiple cores surfaced a
  cluster of races — a TLB-shootdown ack-timeout panic (now a degrade + per-core
  NMI IST stack), a cross-core lost-wakeup, a CoW/mprotect spurious-write-fault
  wrongful-kill (with PKU faults excluded from the spurious-recovery path), a
  task-attributable kernel-stack-overflow now recovered instead of halting, and a
  COM1-RX-under-SMP byte-drop. All five are pinned by the new always-on
  **`smp-smoke`** regression gate.

The honest correction to the "no kernel work" framing: the integration test
turned Phase 90b into a real kernel-hardening pass (~1.7k kernel lines), every
change driven by a reproduced Claude Code symptom and backed by a regression gate.

### Credential posture: subscription-first, 0600, never on the wire

Subscription use is **first-class**, mirroring the Phase 86e `gh` `GH_TOKEN`
precedent exactly. A Pro/Max user mints a long-lived OAuth token **on the host**
with `claude setup-token` (the browser dance happens there, off m3OS), and the
gate seeds it via the **dedicated** `M3OS_CLAUDE_TOKEN` env var — never the
user's ambient variable, so it can't bake into a routine image build. The token
is written to a **mode-0600** file under `/root/.claude/` and exported in-guest
as `CLAUDE_CODE_OAUTH_TOKEN`. The token **value never crosses the serial
console** (only file paths do), never lands in any log, and never lives in
repo/CI — secret hygiene is by construction, not by review. An in-guest `ls -l`
asserts the credential file is `-rw-------` (the same assertion `gh-smoke` makes
on `hosts.yml`).

`M3OS_CLAUDE_KEY` → `ANTHROPIC_API_KEY` is the **API-billing alternative**
through the same seeding path; when both are set the gate prefers the token and
records which mode authenticated. Absence of both means **skip-with-reason**,
never failure.

`/root/.claude.json` is pre-seeded with completed-onboarding / trust state so a
headless `claude -p` invocation proceeds with **no interactive first-run
prompt**. The in-OS **`/login` paste-flow** (the TUI shows a URL; the user visits
it on any browser-equipped device and pastes the returned code back into the
m3OS terminal) is the documented **human** path on the rendering interactive TUI
— it cannot be the gate path because it is interactive by design. Multi-user /
enterprise credential stories are out of scope (see Deferred).

### The A.1 WASM/TUI decision

Claude Code's terminal UI ships `yoga.wasm`, a WebAssembly layout engine. The
Phase 89 jitless V8 config disallows runtime WASM code generation (it allocates
zero runtime executable memory), and m3OS forbids unguarded RWX. Rather than
settling for a print-mode floor, **Phase 90a delivers PKU-backed JIT** — the W^X
invariant is *strengthened* to v2 (W+X is permitted only via the PKU-guarded
`pkey_mprotect` path under a write-deny key), not relaxed — and a JIT Node
variant on which WASM works. Phase 90b consumes that variant under
`M3OS_CLAUDE_JIT=1`, and the **JIT/WASM runtime the TUI depends on is proven**:
90a's `node-jit-smoke` proves V8 TurboFan optimization and
`WebAssembly.Instance` run on the m3OS JIT node, the W^X-v2 cross-thread PKU fix
above unblocks `cli.js` *launch* there too, and the A.2 raw-mode/SIGINT/spawn
primitives the TUI lives on all pass. The jitless `claude -p` path is the
always-available automation floor and the CI-viable default — not a degraded
fallback, but a first-class part of the delivered scope.

**The interactive `claude` TUI renders on the 90a JIT node** — proven by the
automated `claude_tui_render_arm` (`M3OS_CLAUDE_JIT=1` + KVM/PKU-gated). It
launches `claude` in the graphical `term`, screendumps via QMP, and asserts
**592 changed band scanlines** vs the empty-prompt baseline (threshold 20; a
blank screen ≈ 0); the captured screenshot shows the rendered "Welcome to Claude
Code v2.1.112" welcome screen with the yoga.wasm-laid-out logo splash. No
credential or network is needed — the unauthenticated first-run onboarding
screen renders.

Getting there took two fixes — and a corrected diagnosis. An interim PR-audit
QMP/PPM test had the interactive launch get through onboarding (writes
`/root/.claude.json`), JIT-compile under the W^X-v2 PKU-guarded path (`[wx]
v2-guarded W+X mapping` logged — so the JIT/PKU substrate works) and spawn a
ripgrep subprocess, then crash with a userspace null-pointer dereference
(`addr=0x0`). The `unhandled syscall 25` (`mremap`), `425` (`io_uring_setup`)
and `125` (`capget`) lines in that trace were **red herrings**: each correctly
returns `-ENOSYS`, and well-behaved callers (V8, libuv) fall back — they were
not the cause. The **real root cause**, found and fixed during the PR audit
(2026-06-14), was the node build's `--with-intl=small-icu` (the Phase 89
default): small-icu omits the ICU break-iterator / segmentation data that
`Intl.Segmenter` needs. Claude Code's TUI calls
`Intl.Segmenter.prototype.segment()` for Unicode grapheme segmentation (terminal
string-width / wrapping); with small-icu the ICU break iterator was **NULL**, so
V8's `v8::internal::JSSegments::Create` null-dereferenced (`addr=0x0`, confirmed
via a symbolicated backtrace: `JSSegments::Create` ←
`Builtin_SegmenterPrototypeSegment` ← JS). The fix switches the node build to
`--with-intl=full-icu` (+ `--download=all`), which compiles the complete ICU
data (including `brkitr`) into the binary (~30 MB larger). This was **not** a
kernel/JIT/PKU gap and **not** a Phase 93 syscall gap. The other fix — the
W^X-v2 cross-thread PKU read-recovery page-fault handler change above — is still
needed for `cli.js` to *launch* on the JIT node; both fixes together make the
TUI render.

Two always-on regression guards back this. (1) A `SEG_OK` step in the
`claude-smoke` core exercises `Intl.Segmenter` directly (always-on, no
PKU/network) — it catches a regression back to small-icu. (2) The automated
TUI-render arm (`M3OS_CLAUDE_JIT=1` + KVM/PKU-gated) is the falsifiable proof the
screen *shows* the UI — a serial sentinel is blind to TUI rendering, so only a
framebuffer screenshot suffices. The arm reuses the `htop-render-probe` QMP/PPM
harness (`xtask/src/qmp.rs` + `xtask/src/ppm.rs`): drive an interactive `claude`
launch, `screendump`, and assert the populated TUI (the 592-changed-scanline
band check vs the empty baseline).

### The ripgrep static-pie finding (no port needed)

Claude Code's file-search tool shells out to a *vendored platform ripgrep
binary* at `vendor/ripgrep/x64-linux/rg`. The Track A/B audit (`readelf -l`)
found it is **static-pie linked with NO `PT_INTERP`** (~6.5 MB). m3OS's ELF
loader supports `ET_DYN` static-PIE via its no-interpreter path
(`kernel/src/mm/elf.rs`), so the vendored `rg` runs **directly** — no separate
ripgrep port was needed. **This is now confirmed on-OS:** `rg --version` runs
under m3OS in the `claude-smoke` always-on core. The `build_ripgrep` static-musl
fallback stays a documented contingency, not built. The optional vendored native
bits (`audio-capture.node`, a dynamic addon; the `seccomp` helper, for which
m3OS has no seccomp) are pruned or degrade gracefully. Because `rg --version` is
asserted in the always-on core, a search-tool regression is a gate failure, not
a silent degradation.

### The interactive substrate (Phase 89 A.2, validated in `claude-smoke`)

An interactive CLI agent lives on three primitives Phase 89 deferred to this
phase: trapping Ctrl-C, putting the tty in raw mode, and spawning shell commands.
Each is a one-line always-on probe arm riding the `claude-smoke` always-on core
(the task list explicitly permits the A.2 arms to ride this gate, and they ride
`claude-smoke` — **not** `node-smoke`). The probes need no JIT, so they run on
the jitless default core, which is **CI-viable under plain TCG** (no KVM/PKU) —
and they **PASS**, closing the Phase 89 A.2 deferral:

- `NODE_SIGINT_OK` — `process.on('SIGINT')` fires on a self-signal, proving
  libuv's self-pipe signal path (`pipe2` + `rt_sigaction`) end-to-end — the
  explicit Phase 89 A.2 deferred item.
- `NODE_SPAWN_OK` — `child_process.spawn('/bin/sh', ['-c', 'echo spawned'])`
  captures stdout with a 0 exit code — the libuv fork/exec + pipe-capture path
  the shell tool uses.
- `NODE_RAWMODE_OK` — `process.stdin.setRawMode(true/false)` toggles termios
  `ICANON`/`ECHO` over the PTY stack without throwing.

### The `claude-smoke` gate

`cargo xtask claude-smoke` (Track D) has an **always-on offline core** that
proves the entire setup story with zero network and zero secrets, and it
**PASSES on m3OS**: `M3OS_WITH_CLAUDE=1` bundles both `.m3pkg`s, the in-OS solver
resolves `DEPS=node` dependency-first, the launcher chain (`/usr/bin/claude` →
shebang → node → in-process `import()` of `cli.js`) runs to `claude --version` =
`2.1.112` and `claude --help` exits 0, the vendored static-pie `rg --version`
prints, and the A.2 `NODE_SIGINT_OK`/`NODE_SPAWN_OK`/`NODE_RAWMODE_OK` probes
pass — **27/27 on the jitless full-install gate** (the early "24/24" figure was
an `M3OS_CLAUDE_FAST_ITER` reuse-disk run).

The `claude-smoke` core also runs a `SEG_OK` `Intl.Segmenter` step (always-on,
no PKU/network) — the regression guard that catches a node build reverting to
small-icu (which would re-break the TUI's grapheme segmentation).

**The default bundle is the jitless node, so this core is CI-viable under plain
TCG** — no KVM, no PKU. Jitless V8 never touches the W^X/PKU JIT path, so the
gate is *not* KVM-gated by default (KVM is only a speed knob). Setting
`M3OS_CLAUDE_JIT=1` selects the **90a JIT node** (the runtime-WASM variant the
interactive TUI needs); *that* arm IS KVM/PKU-gated exactly like `node-jit-smoke`
(SKIP-with-reason without `M3OS_KVM=1` on a PKU host, since the JIT node requires
real PKU), and on it the `claude-smoke` serial core PASSES (the
`--version`/`--help`/`-p` + `rg` + A.2 + `SEG_OK` set — the same step count as
the jitless full-install gate) **and the automated interactive-TUI render arm
PASSES** (`claude_tui_render_arm`, 592 changed band scanlines). The gate
runs at
`--timeout 5400` (the ~130 MB install + cold `cli.js` parse over the slow VFS —
far faster under KVM). Absent the build prerequisites (host C++ toolchain for the
node dep) it prints `SKIP (reason: …)` and returns success.

The **opt-in live arms** (`M3OS_CLAUDE_NET=1` + a `M3OS_CLAUDE_TOKEN` /
`M3OS_CLAUDE_KEY`) are the actual milestone — and a full authenticated `claude -p`
round-trip is **verified working end-to-end on m3OS**, not merely implemented:

- **Verified round-trip (OpenRouter / any Anthropic-protocol endpoint).** With
  `M3OS_CLAUDE_BASE_URL` + `M3OS_CLAUDE_MODEL` set (the seeded key becomes the
  `ANTHROPIC_AUTH_TOKEN` bearer), `claude -p 'What is 123 plus 456? … <<<NUMBER>>>'`
  completes a full TLS 1.3 handshake + the request/response exchange and prints
  **`<<<579>>>`** over serial — the model genuinely computed and returned the
  answer. This was confirmed collision-proof (a host-side `M3OS_CLAUDE_PCAP` of
  `net0` captured the entire TLS handshake + the ~103 KB request ACKed + the
  response read back; the gate reports `serial core PASSED`, exit 0). The unique
  `<<<579>>>` delimiter is used precisely because a bare `579` false-matched a
  kernel watchdog timestamp — so the pass means a real API answer, never a hang.
  Getting here is what drove the per-address-space-futex-key and
  `EXT2_VOLUME`-yielding-lock kernel fixes above (and the Enter-key-CR fix for the
  interactive TUI round-trip).
- **`CLAUDE_API_OK` (official `api.anthropic.com` path).** Without a base-URL
  override the arm runs `claude -p 'Reply with exactly CLAUDE_API_OK …'` against
  `api.anthropic.com` (Node's bundled OpenSSL + the 86a CA bundle + c-ares DNS),
  subscription-OAuth-token mode preferred. The code path is identical to the
  verified OpenRouter arm; it runs whenever the user supplies an Anthropic
  credential (a real Anthropic secret can never be CI-bound, so the dev-run
  verification above used an Anthropic-protocol proxy instead).
- **Real-filesystem agent workflow** — `claude -p '… Use the Write tool to create
  /root/claude-work.txt containing WORKFLOW_FILE_OK' --allowedTools Write`, then
  the gate asserts the content **outside** the agent via `cat` (`WORKFLOW_FILE_OK`)
  — trusting the filesystem, never the model's own claim. (The delivered proof is
  the agent's Write tool touching the real FS; a broader shell + `git log --oneline`
  commit assertion is a documented future extension, not in the as-built arm.)

Skip-with-reason when unconfigured (real egress + a secret can never be CI-bound)
— the always-on core is what CI sees, mirroring `gh-smoke` / `git-https-smoke`.

**The full interactive `claude` TUI renders on the 90a JIT node, proven
automatically by the QMP/PPM render arm.** Everything the TUI depends on is
proven: the JIT/WASM runtime (90a's `node-jit-smoke` — TurboFan optimization +
`WebAssembly.Instance`), the W^X-v2 PKU fix above (unblocking `cli.js` *launch*
on the JIT node), the A.2 raw-mode/SIGINT/spawn primitives the TUI lives on (all
passing in the `claude-smoke` core), and — after the small-icu→full-icu build
switch — `Intl.Segmenter`'s grapheme segmentation (the `SEG_OK` guard). The
render arm (`claude_tui_render_arm`, `M3OS_CLAUDE_JIT=1` + KVM/PKU-gated) is the
falsifiable visual proof: an interactive `claude` launch driven in the graphical
`term`, captured via `screendump`, with the PPM analysis asserting a populated
TUI — **592 changed band scanlines** vs the empty-prompt baseline (threshold 20;
a blank screen ≈ 0), the rendered "Welcome to Claude Code v2.1.112" onboarding
splash. It reuses the existing `less-render-probe`/`htop-render-probe` QMP/PPM
harness (`xtask/src/qmp.rs` + `xtask/src/ppm.rs`). The serial sentinel is blind
to TUI rendering, so the framebuffer screenshot is the falsifiable evidence the
screen *shows* the UI.

## Key Files

| File | Purpose |
|---|---|
| `ports/util/claude-code/Portfile` | Pinned `@anthropic-ai/claude-code@2.1.112` + registry-tarball SHA-256, `CATEGORY=util`, `DEPS=node` |
| `xtask/src/port_build.rs` | `fn build_claude_code` — fetch-and-stage the pinned npm tarball, stage `/usr/lib/claude-code/` + the `/usr/bin/claude` launcher, seal the `.m3pkg`; the vendored-`rg` `readelf -l` static-pie audit |
| `xtask/src/main.rs` | `fn cmd_claude_smoke` / `fn claude_smoke_steps` — serial DSL gate; the `M3OS_WITH_CLAUDE` bundle block in `populate_phase_69d_ports`; the `M3OS_CLAUDE_TOKEN`/`M3OS_CLAUDE_KEY` 0600 credential seeding |
| `xtask/src/main.rs` (`claude_tui_render_arm`) | The **active automated interactive-TUI render arm** (`M3OS_CLAUDE_JIT=1` + KVM/PKU-gated): launches `claude` in the graphical `term`, screendumps via QMP, and asserts 592 changed band scanlines vs the empty-prompt baseline (the rendered "Welcome to Claude Code v2.1.112" onboarding splash) |
| `xtask/src/qmp.rs`, `xtask/src/ppm.rs` | The headless-framebuffer harness (`QmpClient::screendump` + PPM band-occupancy / change analysis) reused by `htop-render-probe` and by the active `claude_tui_render_arm` TUI render proof above |
| `xtask/src/port_build.rs` (`build_node`) | The node build switched from `--with-intl=small-icu` to **full-icu** (+ `--download=all`) so `Intl.Segmenter` has the ICU break-iterator data the TUI's grapheme segmentation needs (small-icu's missing `brkitr` had null-deref'd V8's `JSSegments::Create`); ~30 MB larger binary |
| `kernel/src/arch/x86_64/interrupts.rs` | `page_fault_handler` + the new `leaf_pte_flag_bits` helper — the W^X-v2 cross-thread PKU read-recovery fix (grant read access on a `PROTECTION_KEY` read fault against a present executable pkey-tagged page; writes stay gated, non-executable data pages excluded) — needed for `cli.js` to launch on the JIT node |
| `kernel/src/arch/x86_64/pkru.rs` | The new `grant_read_access` — clears the key's access-disable bit in the faulting thread's live PKRU (context-switch XSAVE persists it) |
| `kernel/src/mm/elf.rs` | The `ET_DYN` static-PIE (no `PT_INTERP`) loader path that runs the vendored static-pie `rg` directly |
| `kernel/Cargo.toml` | `version = "0.90.1"` (Phase 90a took `0.90.0`; this sub-phase takes the patch) |
| `docs/claude-code-roadmap.md` | Standalone per-tool narrative (revived from archive in E.2) |
| `docs/roadmap/90b-claude-code.md` | Phase design doc |
| `docs/roadmap/tasks/90b-claude-code-tasks.md` | Per-track task list with acceptance items |

## How This Phase Differs From Later and Real Agent Work

- Phase 90b pins **2.1.112** (the last `cli.js` + `yoga.wasm` + `vendor/ripgrep/`
  Node-runtime version). The 2.1.113+ **native Bun/JavaScriptCore single-file
  binary** distribution is a separate future port (a Bun runtime, not Node) and
  is out of scope here.
- The supported install is a **pre-bundled `.m3pkg`**, not live `npm install -g`.
  Live npm survives only as an opt-in real-internet arm — a VFS-throughput limit
  on npm's thousands-of-tiny-files workload, not a TLS or registry gap.
- The agent runs on **both** node variants. The jitless node (Phase 89) runs the
  full CLI — `--version`/`--help`/`-p`, search, the A.2 primitives — so the
  always-on core is **CI-viable under plain TCG**. The interactive TUI needs
  `yoga.wasm`, which needs the **Phase 90a JIT variant** (`M3OS_CLAUDE_JIT=1`);
  on a no-PKU CPU the JIT node aborts (it does not degrade to jitless), so *that
  arm* — not the whole gate — is KVM/PKU-gated. The JIT/WASM runtime is proven,
  and the **interactive TUI renders on the JIT node** (the automated
  `claude_tui_render_arm` — 592 changed band scanlines), enabled by the W^X-v2
  PKU read-recovery kernel fix (so `cli.js` launches) and the node build's
  small-icu→full-icu switch (so `Intl.Segmenter` has the ICU break-iterator data
  the TUI needs — the earlier `mremap`/`io_uring` syscalls were red herrings).
- Credentials are the **seeded host-minted OAuth token / API key** plus the
  in-OS `/login` paste-flow whose browser step happens on another device. There
  is no in-OS browser, no MCP / IDE integration, and no multi-user credential
  story — mature hosted-agent ecosystems support far broader integrations than
  m3OS should assume. The real value here is a platform-integration proof point,
  not a claim that m3OS is a full hosted AI workspace.

### The supported-workflow boundary (falsifiable)

With the documented steps a user can reproduce, on m3OS:

1. `M3OS_WITH_CLAUDE=1 cargo xtask image` bundles `claude-code` + the node
   variant under test (jitless by default; the 90a JIT node under
   `M3OS_CLAUDE_JIT=1`).
2. Boot `0.90.1`; `pkg install claude-code` auto-installs `node` first
   (dependency-first solver order, asserted in the gate output).
3. `claude --version` prints `2.1.112`, `claude --help` exits 0, the vendored
   `rg --version` runs, and the A.2 SIGINT/spawn/raw-mode probes pass — fully
   offline, on plain TCG (no KVM/PKU needed for the jitless core;
   27/27 full-install).
4. On a KVM/PKU host with `M3OS_CLAUDE_JIT=1`, the same serial core PASSES on the
   JIT node — runtime WASM + the W^X-v2 PKU read-recovery fix unblock
   `cli.js` *launch* on the JIT variant.
5. With a seeded `M3OS_CLAUDE_TOKEN` (or `M3OS_CLAUDE_KEY`) and
   `M3OS_CLAUDE_NET=1`, `claude -p` completes a full authenticated round-trip —
   **verified end-to-end** (a `<<<579>>>` answer over a real TLS request/response,
   confirmed collision-proof by an `M3OS_CLAUDE_PCAP` capture). Adding
   `M3OS_CLAUDE_BASE_URL`/`M3OS_CLAUDE_MODEL` runs it against an Anthropic-protocol
   endpoint (the path the dev-run verification used); without them it runs against
   `api.anthropic.com` (`CLAUDE_API_OK`) on a user-supplied Anthropic credential —
   identical code. A scripted agent workflow then uses the Write tool to create
   `/root/claude-work.txt` with known content — proven *outside* the agent by
   `cat` (`WORKFLOW_FILE_OK`), not the model's own claim. (The as-built arm
   asserts the file write; a shell-command / `git commit` + `git log` assertion is
   a straightforward extension of the same opt-in arm, not yet wired.)
6. On that same KVM/PKU + `M3OS_CLAUDE_JIT=1` host, the **interactive `claude`
   TUI renders** — the automated `claude_tui_render_arm` launches `claude` in the
   graphical `term`, screendumps, and asserts 592 changed band scanlines vs the
   empty-prompt baseline (the rendered "Welcome to Claude Code v2.1.112"
   onboarding splash). It was delivered by the W^X-v2 PKU read-recovery fix (so
   `cli.js` launches) and the node build's small-icu→full-icu switch (so
   `Intl.Segmenter`'s grapheme segmentation has the ICU break-iterator data the
   TUI needs); the earlier `mremap`/`io_uring` syscalls were red herrings.

Everything outside that list is a non-goal (below).

## Related Roadmap Docs

- [Phase 90b design doc](./roadmap/90b-claude-code.md)
- [Phase 90b task list](./roadmap/tasks/90b-claude-code-tasks.md)
- [Phase 90a — Memory Protection Keys (PKU)](./roadmap/90a-memory-protection-keys.md) — the W^X v2 / JIT Node substrate the interactive TUI depends on
- [Phase 89 — Node.js](./89-nodejs.md) — the static Node runtime, the `npm`/TLS path, and the deferred interactive-substrate items closed here
- [Claude Code standalone roadmap](./claude-code-roadmap.md) — per-tool porting narrative with Mermaid dependency diagrams (revived for Phases 90a/90b)

## Deferred or Later-Phase Topics

- **Auto-update** — `DISABLE_AUTOUPDATER=1`; the sealed `.m3pkg` is the delivery.
- **Non-essential telemetry** — disabled via
  `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1`; no Statsig/Sentry egress.
- **In-OS browser** — none; auth is the seeded OAuth token / API key plus the
  `/login` paste-flow whose browser step happens on another device.
- **MCP servers / IDE integrations / optional protocol extensions** — out of
  scope (the surrounding ecosystem grows faster than the OS).
- **Multi-user / enterprise credential-management stories** — out of scope;
  credentials live only in 0600 files under `/root/.claude/`.
- **Rich GUI integration** for the agent — out of scope.
- **Offline / local-model alternatives** beyond the documented cloud-backed
  path — out of scope.
- **The native Bun-binary distribution (2.1.113+)** — a separate future port (a
  Bun runtime, not Node); out of scope for this phase.
- **Live `npm install -g`** — implemented as an opt-in real-internet arm only;
  the sealed `.m3pkg` is the supported install path (a VFS-throughput limit, not
  a network gap).
- **TCP keepalive *prober*** — bring-up made `setsockopt(SO_KEEPALIVE/TCP_KEEP*)`
  ABI-conformant (accepted + stored + round-tripped, so libuv's `fetch` path
  connects), but the kernel does not yet *send* keepalive probes. Not required
  here (an active API/SSE stream keeps data flowing so the idle timer never fires;
  a dead idle pooled socket is caught on reuse by undici's retry path); scheduled
  for a future networking phase. See the design doc's *Deferred Until Later*.
