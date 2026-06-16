---
status: RESOLVED (2026-06-16) — **`claude -p` completes a full authenticated round-trip
  end-to-end on m3OS over OpenRouter.** VERIFIED collision-proof: claude's actual stdout
  printed `<<<579>>>` on serial, `claude-smoke` reported `serial core PASSED (33 steps)`,
  exit 0. The whole pipeline works: cold cli.js load → ion env export → TLS 1.3 handshake to
  OpenRouter → full ~103 KB request (system prompt + tools) → authenticated 200 response →
  claude prints the answer. Confirm via the user's OpenRouter request logs (the ground-truth
  external check — this run WILL appear, unlike the earlier false-positive run).
  THE REAL ENABLING FIXES (both committed, load-bearing):
   1. EXT2_VOLUME single-core spin-deadlock (RIP-confirmed via host-side QMP `info
      registers`: constant spin RIP at `path_node_nofollow`'s `EXT2_VOLUME.lock()`
      cmpxchg/pause loop; whole machine wedged at 100% CPU, no watchdog). Fix: EXT2_VOLUME
      `spin::Mutex` → `YieldingMutex` (yield, not busy-spin, on contention). Committed
      `fcd78100`.
   2. Cross-process PRIVATE-futex collision. `sys_futex` keyed private futexes as
      `(0, uaddr)` — a single GLOBAL root. Claude spawns multiple identical-layout `node`
      subprocesses whose musl/libuv pthread threadpool condvars sit at the same uaddr; all
      aliased into one wait queue, so one process's `FUTEX_WAKE` woke/absorbed another
      process's waiter. Fix: key futexes per-address-space (CR3 == caller's pml4; `is_private`
      folded into bit 0). Committed `1bada591`. This is what carried claude from 0 TCP
      connections (hung in startup) to a full request/response exchange with OpenRouter.
  THE FINAL-STRETCH "HANG" WAS NOT A CODE BUG — it was a STALE TEST CREDENTIAL:
   `M3OS_CLAUDE_FAST_ITER` reuses an installed disk to skip the slow in-OS install, but the
   seeded 0600 credential lives ON that disk and was NOT refreshed between runs. A reused
   disk held an old **Anthropic** key (`sk-ant-…`, 104 B) while the run intended the
   **OpenRouter** key (`sk-or-…`, 74 B), so claude sent the wrong key and OpenRouter returned
   `401 {"error":{"message":"Missing Authentication header","code":401}}` (and OpenRouter does
   not log unauthenticated requests → the empty-logs "ground truth" that looked like "no
   request arrived"). A host-side pcap (`M3OS_CLAUDE_PCAP=1`, new) was decisive: it showed the
   FULL TLS handshake completing, the entire 103 KB request leaving the guest and being ACKed,
   and OpenRouter's 1159 B response arriving and being read — i.e. the kernel network/TLS path
   was working the whole time. THE "residual single-process futex lost-wake" (#4c in the prior
   draft) WAS A RED HERRING: those `BlockedOnFutex "no waker registered"` lines are IDLE libuv
   threadpool workers (normal for an idle node), not the blocker.
  CORRECTION HISTORY (kept for the record): an earlier draft + commit `fcd78100` once claimed
   "claude -p COMPLETES (answers 579)" — that WAS a false positive (the bare `579` matched a
   kernel watchdog `stuck-since=32579ms` timestamp while claude was hung). The pass pattern is
   now the collision-proof `<<<579>>>`; THIS resolution is verified against that token in
   claude's real stdout.
fixes (this session):
  - kernel/fs/ext2: EXT2_VOLUME spin::Mutex → YieldingMutex (committed fcd78100) — REAL, load-bearing
  - kernel/arch/syscall sys_futex: private futex key (0,uaddr) → per-CR3 (committed 1bada591) — REAL, load-bearing
  - kernel/ipc/endpoint + task/scheduler: deadline-path Bug #8.1 waker registration (committed) — real but orthogonal
  - xtask/claude-smoke: 579 → <<<579>>> collision-proof check; small cwd; M3OS_CLAUDE_MONITOR HMP socket;
    M3OS_CLAUDE_PCAP guest-net capture (NEW); FAST_ITER credential re-stamp (NEW — fixes the stale-cred footgun)
  - xtask/node-smoke: M3OS_NODE_VFS_STRESS arm
  - REVERTED (red-herring diagnostics): the [futexcensus] scheduler dump (the futex stall was idle workers)
branch: feat/phase-90b-claude-code
key-commits:
  - 9e09b67c  kernel/net: accept TCP keepalive socket options (fixes setsockopt ENOPROTOOPT)
  - 28e42a55  ports/claude-code: depend on ca-certificates (CA bundle for the launcher)
  - 3d92bb40  kernel/process: raise MAX_FDS 32 → 128 (fixes Claude Code EMFILE lock-up)
  - c7e443ca  kernel/smp/tlb: word ack-timeout diagnostic per regime (PR #247 review)
  - 952da571  kernel/process: heap-back the fd table (Vec<Option<FdEntry>>) — fixes the
    node clone/fork kstack-overflow segfault the MAX_FDS=128 raise introduced (proven)
  - b567e304  xtask/claude-smoke: M3OS_CLAUDE_BASE_URL/MODEL + OpenRouter round-trip arm
  - 6f02d18b  kernel/interrupts: kstack-overflow #DF backtrace diagnostic (pinned #2)
  - 36074826  kernel/syscall: handle /dev/tty in stat path (fixes the system-wide FREEZE)
  - ed54ce1d  kernel/interrupts: PKU read-recovery diagnostic (ruled PKU out of the stall)
  - (docs)    0ba11f5e, 98b6d4f4, 2e449377 — running findings/analysis in this handoff
date: 2026-06-15 (last updated 2026-06-16)
component: kernel/process (fd table / MAX_FDS), kernel/syscall (stat /dev/tty),
  kernel/net (socket options), kernel/interrupts (diagnostics), ports/claude-code (DEPS),
  xtask/claude-smoke (OpenRouter harness). REMAINING work is in kernel/task/scheduler +
  kernel/ipc (reply-block primitive) and userspace/vfs_server (reply loop).
artifacts (project root, gitignored — do NOT commit):
  - openrouter.sh — the user's OpenRouter env; key filled in locally. The repro command
    reads OPENROUTER_API_KEY from it; the value never crosses serial or the repo.
  - m3os.log — a user-supplied serial capture from a freeze repro.
  - claude.png — original screenshot.
  - host-side serial captures from this session live under /tmp/m3os-*.log (transient).
---

## Goal

Get `@anthropic-ai/claude-code@2.1.112` (the pinned Node-runnable build, Phase 90b) to
**complete a request end-to-end on m3OS** — driven against OpenRouter (an Anthropic-protocol
proxy the user runs on their laptop; key in `openrouter.sh`), as both a working setup and a
diagnostic vs the official `api.anthropic.com`.

## Current status (TL;DR)

claude went through FOUR distinct m3OS bugs, hit one after another as each was fixed:

| # | Symptom | Root cause | Fix | State |
|---|---------|-----------|-----|-------|
| 1 | EMFILE lock-up | per-process fd cap of 32 | `3d92bb40` MAX_FDS 32→128 | ✅ fixed+pushed |
| 2 | Segfault (node killed) | node clone/fork stacked ~36 KiB of inline 12 KiB fd-tables → 64 KiB kstack overflow | `952da571` heap-backed fd table | ✅ fixed+pushed, proven |
| 3 | **Frozen login screen** | `stat("/dev/tty")` hung the kernel VFS path, wedging the shared VFS system-wide | `36074826` `/dev/tty`→char device | ✅ fixed+pushed, verified |
| 4 | `claude -p` never finishes | Layered. (a) EXT2_VOLUME single-core spin-deadlock (held across virtio-blk I/O; a 2nd task busy-spins forever). (b) Cross-process PRIVATE-futex collision: private futexes keyed `(0,uaddr)` globally, so claude's multiple identical-layout node subprocesses aliased their pthread threadpool condvars and stole each other's wakes. (c) **NOT A BUG** — the final "hang" was a STALE TEST CREDENTIAL (FAST_ITER reused a disk with an old Anthropic key → OpenRouter 401). | (a) `EXT2_VOLUME`→`YieldingMutex` (`fcd78100`). (b) futex key `(0,uaddr)`→per-CR3 (`1bada591`). (c) re-stamp the credential under FAST_ITER (xtask). | ✅ RESOLVED — `claude -p` answers `<<<579>>>` end-to-end over OpenRouter (smoke 33/33 PASS, exit 0) |

**`claude -p` COMPLETES** (verified collision-proof against `<<<579>>>` in claude's real
stdout — NOT the earlier false positive). The two load-bearing kernel fixes were (a) the
EXT2_VOLUME yielding lock and (b) the per-address-space futex key; together they carried
claude from "hangs in startup (0 TCP)" to a full request/response exchange with OpenRouter.
The remaining "hang" after that was **not** a kernel bug: `M3OS_CLAUDE_FAST_ITER` reused a
disk whose seeded credential was a STALE Anthropic `sk-ant-…` key (104 B) rather than the
intended OpenRouter `sk-or-…` key (74 B), so OpenRouter returned `401 "Missing Authentication
header"` and (since it does not log unauthenticated requests) the request logs stayed empty —
which had looked like "no request ever arrived". A host-side pcap (`M3OS_CLAUDE_PCAP=1`)
disproved that: it captured the full TLS 1.3 handshake, the entire ~103 KB request being sent
+ ACKed, and OpenRouter's response coming back and being read. The `BlockedOnFutex "no waker
registered"` lines were IDLE libuv threadpool workers (a red herring), not a lost-wake.

### What the pcap showed (the decisive evidence)

`M3OS_CLAUDE_PCAP=1` adds `-object filter-dump,netdev=net0,file=/tmp/claude-net.pcap` to the
claude-smoke QEMU launch. For each connection to `openrouter.ai:443`: SYN/SYN-ACK/ACK →
ClientHello (1605 B, ALPN `http/1.1`, TLS 1.2+1.3 offered) → server flight 1 (3933 B, ACKed)
→ client Finished → **full ~103 KB application request** sent and ACKed segment-by-segment →
server's **1159 B response** arrives and is ACKed → claude reads it and prints
`Failed to authenticate. API Error: 401 {"error":{"message":"Missing Authentication
header","code":401}}`. That 401 (OpenRouter's error JSON shape, not Anthropic's) is proof the
request reached OpenRouter's app and was rejected purely on auth — the network/TLS path was
never the problem.

### How #4 was actually found (the reply-block hypothesis was wrong)

The previous handoff blamed an intermittent lost-wake in the reply-block IPC primitive. That
was a red herring. The real failure is a **deterministic whole-machine wedge**: single-core,
100 % CPU, **no watchdog** (the spin is kernel-mode; IRQs fire but preemption cannot unwind a
spinlock). Method that cracked it: an opt-in QEMU HMP monitor (`M3OS_CLAUDE_MONITOR=1`) +
a host-side `info registers` poller showed a **constant spin RIP** that `addr2line`'d to
`path_node_nofollow`'s `EXT2_VOLUME.lock()` cmpxchg/pause loop. The kernel ext2 volume
(`kernel/src/fs/ext2.rs`) is a plain `spin::Mutex` deliberately held across `read_inode`/
`resolve_path` → `block_current_until` (virtio-blk I/O); on one core, when task A sleeps in
that I/O holding the lock and task B acquires it, B busy-spins and denies A the only CPU, so A
never releases — hard deadlock. This is the **same bug class as #3** (`/dev/tty`): a kernel FS
lock held across a block, wedging the shared VFS. **Fix:** `EXT2_VOLUME` is now a
`YieldingMutex` whose `lock()` `try_lock`s and, only on contention, `yield_now()`s so A is
rescheduled to release; uncontended (boot) acquisition is unchanged.

Orthogonal real bug fixed the same session: the deadline IPC block sites
(`recv_msg_with_deadline`/`call_msg_with_deadline`) used `block_current_until` with an
unregistered local wake flag — **Phase 57e Bug #8.1 on the deadline path** — so a sender
racing the block was lost-woken until the 5 s flush deadline. Fixed with
`block_current_on_recv_v2_deadline`/`block_current_on_reply_v2_deadline` (register the waker
before the pending-message recheck). Not load-bearing for claude (the `EXT2_VOLUME` wedge was
the blocker) but a genuine latency bug.

Networking is fully exonerated (see "Ruled out") — claude never reached the API; it wedged in
startup. The kstack is NOT undersized; #2 was the kernel putting 12 KiB buffers on the stack.

## The three fixed bugs (detail)

### #1 — EMFILE: MAX_FDS 32 → 128 (`3d92bb40`)
claude's own debug log ended with `rg error (code=EMFILE)` before wedging. The kernel capped
every process at 32 fds; Node + claude (stdio + libuv epoll/eventfd/timerfd/self-pipe + the
custom undici TLS agent + config/lock/backup files + 3 pipe fds per spawned ripgrep) blow
past 32 in one session → `fd_alloc()` → `EMFILE` → lock-up. Raised to 128 (~4× the observed
~30-fd ceiling). `getrlimit(RLIMIT_NOFILE)` reports MAX_FDS so it stays truthful.
(Pre-reqs that unblocked earlier symptoms: `9e09b67c` TCP keepalive setsockopt; `28e42a55`
ca-certificates DEPS so `/etc/ssl/certs/ca-certificates.crt` is installed.)

### #2 — kstack-overflow segfault: heap-backed fd table (`952da571`)
The MAX_FDS 32→128 raise introduced this. `FdEntry` is ~96 bytes (Phase 88 embedded a
`VfsFileMeta` in `FdBackend::VfsService`), so `[Option<FdEntry>; 128]` ≈ **12 KiB**. It lived
inline in `Process`, was returned by-value from `fd_table_snapshot()`, and passed by-value to
`spawn_process_with_cr3_and_fds()`. node's `clone(CLONE_THREAD)` (libuv threadpool) and `fork`
paths stack THREE such copies at once (parent snapshot + `child_fds_copy` + the child
`Process`'s inline copy) ≈ 36 KiB → overflowed the 64 KiB per-task kstack the instant MAX_FDS
went to 128 (at 32 the three copies were only ~9 KiB). Surfaced as
`#DF = kstack overflow attributable to pid <node>`; Track-D recovery killed node (the segfault).
**Fix:** `Process.fd_table` / `shared_fd_table` / `fd_table_snapshot()` / `new_fd_table*()` /
`spawn_process_with_cr3_and_fds()` are now heap `Vec<Option<FdEntry>>` (always length MAX_FDS,
built `vec![None; MAX_FDS]` so no `[_; N]` stack temporary). Per-frame fd-table cost: ~12 KiB
→ 24-byte Vec header. **Proven:** a controlled A/B revert (pre-fix inline array → overflow
single-core on the first node launch; heap Vec → no overflow), plus `M3OS_KVM=1 cargo xtask
node-smoke` PASSED (exercises the same `clone(CLONE_THREAD)` egress path) with 0 overflow
lines. The `6f02d18b` #DF backtrace diagnostic pinned the mechanism (only ~66 `.text` frames
on the stack, deepest are `memcpy`/`memset` of ~12 KiB = the inline fd-table, not recursion).
MAX_FDS can now go to Linux's 1024 with a one-line change (heap cost only); left at 128.

### #3 — system-wide FREEZE: `/dev/tty` stat (`36074826`)
The user's actual reported symptom: running `claude` over SSH froze the **login screen**
(whole machine). node/claude `readlink` their TTY (→ `/dev/tty`, 8 chars) then `stat("/dev/tty")`
at startup. `/dev/tty` was MISSING from the char-device special-case lists in BOTH
`path_node_nofollow` and `path_filemeta` (every other `/dev/*` device — null/zero/urandom/
random/full/ptmx/pts — was handled, and `path_metadata` already listed `/dev/tty`). So the
stat fell through to the kernel ext2/`EXT2_VOLUME.lock` path and **hung in-kernel**; because it
blocked holding the shared VFS, every other process blocked on file I/O → frozen login screen.
**Fix:** report `/dev/tty` as a char device (S_IFCHR 0o666) in both functions, matching Linux
(`stat("/dev/tty")` never touches the FS) and the other `/dev/*` devices. **Verified:** claude's
own `--debug` log (pulled off the disk via `debugfs`) now advances from the old hang at
`[STARTUP] Loading MCP configs...` through `Running setup()...` to skill discovery, and node
stats `/dev/tty` repeatedly + proceeds. The system no longer freezes.

## #4 — RESOLVED: `claude -p` completes (the layered chain + the credential footgun)

> **RESOLUTION:** `claude -p` now completes end-to-end over OpenRouter (`<<<579>>>`, smoke
> 33/33 PASS). The load-bearing fixes were the EXT2_VOLUME yielding lock (`fcd78100`) and the
> per-address-space futex key (`1bada591`). The investigation notes below (written while the
> bug was open) are kept for the record — BUT note two were superseded: (1) the "reply-block
> IPC stall" hypothesis was wrong (it was the EXT2 wedge), and (2) the "residual single-process
> futex lost-wake" was a RED HERRING (idle libuv threadpool workers). The actual final blocker
> was a **stale `FAST_ITER` credential** (a reused disk's old Anthropic key → OpenRouter 401),
> now fixed by re-stamping the credential into the reused disk (xtask `restamp_claude_credential`).

**Historical symptom (while open):** a `claude -p '…'` round-trip (OpenRouter) never printed
the answer; the claude-smoke step timed out. NOT a crash, NOT a freeze — the system stayed
responsive (`vfs_server` kept replying, other node threads kept churning, no fault/panic/kill).

**Where it stalls (from claude's --debug log + node syscall traces):** in claude's `setup()` /
skill-discovery startup phase (after CA-bundle load + the undici TLS agent; `Loading skills
from: managed=/etc/claude-code/.claude/skills, user=/root/.claude/skills`). The blocked node
thread is in a VFS `stat` (`syscall=4`), i.e. inside `endpoint::call_msg` →
`block_current_on_reply_v2` → `BlockedOnReply`.

**Why it is invisible to current diagnostics (and what that tells us):**
- The `[sched] … no waker registered` watchdog does NOT fire — but `BlockedOnReply` ALWAYS
  has a `reply_waker` registered, and the watchdog's `StuckNoWaker` verdict is for
  no-deadline blocks past 30 000 ticks; a node trace showed node strace-SILENT for ~90 s
  (no syscall boundary), yet no watchdog line. Reconciliation: the thread is most likely
  **wake/reblock-looping INSIDE `block_current_until`** (no syscall boundary → strace-silent;
  `blocked_since` resets each cycle → watchdog blind), OR a genuine lost-wake the watchdog
  excludes because a waker is present.
- The spurious-wake diagnostics (`reply_v2:*`, `call_msg:no_reply_message`) do NOT fire
  either — so it is not a full spurious wake that returns to `call_msg` with no message.
- **Trigger is CONCURRENCY:** claude runs ~10 threads hammering the single-threaded
  `vfs_server`; `node-smoke` (which passes) is not concurrent. So a reply delivered to caller
  A while caller B is mid-`call_msg` enqueue, or a reply-cap resolution race, or a
  re-check-loop in the v2 block primitive, could strand a caller.
- Intermittent: some runs clear the startup window and reach ripgrep; some stall earlier
  (right after V8 `[wx] v2-guarded W+X mapping (pkey=1)` WASM codegen). Happens on BOTH
  `-smp 1` and `-smp 4`.

**RULED OUT (do not re-chase):**
- cwd / `rg --files` walking `/` — disproved: an empty cwd (`/tmp/cw`) still stalled. (Note:
  once PAST the stall, `rg --files` at a large cwd like `/` IS pathologically slow over the
  ~200 KB/s ring-3 VFS — cwd-mitigable, not a bug. Run claude in a small project dir.)
- single-core `EXT2_VOLUME.lock` deadlock — `-smp 4` stalls too.
- `vfs_server` lost/failed reply — no `ipc_reply failed` / `request missing reply cap` logged.
- anonymous-`mmap` kernel hang — that path is a non-blocking linear `mmap_next` bump + VMA
  insert; cannot block.
- PKU read-fault spin-loop — the new `PKU_READ_RECOVERIES` counter (`ed54ce1d`) shows the
  Phase-90b cross-thread read-recovery fires a BOUNDED ~11 times across DIFFERENT worker tids
  at the SAME V8 rip with DIFFERENT addrs (each thread reads the pkey-1 WASM code space ONCE
  and proceeds) — healthy, not looping. PKU is unlikely.

## Remaining follow-ups (claude -p itself is DONE)

1. **Official `api.anthropic.com` arm.** The same harness runs against the real Anthropic API
   when `M3OS_CLAUDE_NET=1` + a credential is seeded via `M3OS_CLAUDE_TOKEN` (subscription
   OAuth) or `M3OS_CLAUDE_KEY` (API key), WITHOUT `M3OS_CLAUDE_BASE_URL` (so it hits
   `api.anthropic.com` and sends the native `x-api-key`/Bearer). The user plans to SSH into a
   test machine to authenticate (`claude login`) and try it; the credential path is the same
   0600 `/root/.claude/{oauth_token,api_key}` file, now also refreshed under FAST_ITER.
2. **Multi-core (`M3OS_CLAUDE_MULTICORE=1`).** claude-smoke pins `-smp 1` by default. The
   multicore arm validates the 2026-06-14 SMP TLB-shootdown survivability fixes against the
   real claude workload; pair with `M3OS_CLAUDE_JIT=1` + `M3OS_KVM=1`. Independent of the
   `claude -p` completion (which is done single-core).
3. **Interactive TUI** already verified (the `claude_tui_render_arm` QMP/PPM render check —
   the yoga.wasm "Welcome to Claude Code" splash paints under `M3OS_CLAUDE_JIT=1` + KVM).
4. **Orthogonal cleanups (non-blocking):** `copyfile`→EFAULT (secondary bug below); the stale
   `FAST_ITER` credential footgun is now fixed in xtask but consider extending the same
   re-stamp to `node-smoke`/`gh-smoke` if they ever reuse disks with changing secrets.

## How to reproduce / test

Pull the branch first (xtask always recompiles the kernel from source, so a fresh build picks
up all fixes). Run from a **small cwd** to avoid the orthogonal `rg --files`-at-`/` slowness.

```
git pull
# Single-core jitless run against OpenRouter (key read from openrouter.sh, never printed):
M3OS_SMP=1 M3OS_CLAUDE_NET=1 \
  M3OS_CLAUDE_BASE_URL="https://openrouter.ai/api" \
  M3OS_CLAUDE_MODEL="anthropic/claude-haiku-4.5" \
  M3OS_CLAUDE_KEY="$(. ./openrouter.sh >/dev/null 2>&1; printf %s "$OPENROUTER_API_KEY")" \
  M3OS_KVM=1 M3OS_CLAUDE_FAST_ITER=1 \
  M3OS_SERIAL_LOG=/tmp/m3os-claude.log \
  cargo xtask claude-smoke --timeout 900
# PASS = claude's stdout prints the collision-proof `<<<579>>>` (smoke "serial core PASSED").
# This is the VERIFIED working command (33/33 PASS, ~69 s under KVM+FAST_ITER).
# Add M3OS_CLAUDE_PCAP=1 to capture all guest net traffic to /tmp/claude-net.pcap (decode with
# `tcpdump -nr /tmp/claude-net.pcap 'port 443'`) if a future network regression needs proof of
# what actually leaves/returns at the wire.
```

- `M3OS_SERIAL_LOG=<path>` tees the COMPLETE raw guest serial to a file (the in-memory
  harness buffer only keeps a drained tail) — essential for post-mortem; the claude-smoke
  harness does not echo full serial otherwise.
- `M3OS_STRACE_COMM=<comm-prefix>` is a BUILD-TIME (`option_env!`) per-comm syscall trace.
  `M3OS_STRACE_COMM=node` traces all node threads; `=vfs_server` traces the VFS server.
  Touch `kernel/src/arch/x86_64/syscall/mod.rs` to force the rebuild when changing it.
  Caveat: heavy trace volume can perturb timing (heisenbug) and truncate the serial-log tail.
- Pull claude's OWN `--debug` log off the disk (the smoking gun for the claude-level step):
  `dd if=target/x86_64-unknown-none/release/disk.img of=/tmp/ext2.img bs=512 skip=2048` then
  `debugfs -R "rdump /root/.claude/debug /tmp/cdebug" /tmp/ext2.img`. (The disk has an MBR
  partition table; the ext2 root is at LBA 2048, so debugfs the partition, not the whole image.)
  Add `--debug` to the round-trip's `claude -p` in `claude_smoke_steps` (xtask) to make claude
  write that log; it lands at `/root/.claude/debug/<session>.txt`.
- Interactive (visible screen): `M3OS_SMP=1 M3OS_WITH_CLAUDE=1 cargo xtask run-gui`, then in
  the guest export the 4 ANTHROPIC_* vars and `claude --debug -p "hi"`. ion is the login
  shell (not POSIX sh); `export VAR=value` works.

## Diagnostics available (committed, permanent)

- `[int] kstack-bt: …` — on a kstack-overflow #DF, scans the exhausted stack and prints the
  recurring return address / verdict (RECURSION vs LARGE-FRAME) + deepest frames (`6f02d18b`).
- `[pf] pku read-recovery: …` — rate-limited per W^X-v2 cross-thread PKU read-recovery; a
  single repeating pid/rip/addr would indicate a PKU spin-loop (`ed54ce1d`).
- `M3OS_STRACE_COMM` per-comm syscall trace (above); the `[sched] … no waker registered`
  watchdog (does NOT catch #4 — see "Why it is invisible").

## Ruled out (network is NOT the problem)

The networking/HTTP stack is fully working — now proven THREE ways: (1) `node -e` probes
against the real OpenRouter API (raw `https.request` POST → 200; `fetch()` non-streaming and
streaming → 200/DONE); (2) `node-smoke M3OS_NODE_NET=1` (live HTTPS + `npm install`) PASSES;
and (3) decisively, the `M3OS_CLAUDE_PCAP` capture of an actual `claude -p` run, which shows
claude's full TLS 1.3 handshake + ~103 KB request + OpenRouter's response on the wire. The
final verified run authenticates and answers `<<<579>>>`. (Historical note: while the bug was
open it LOOKED like "the request never reaches OpenRouter" because the OpenRouter request logs
were empty — but that was the 401-on-a-stale-key path; OpenRouter does not log unauthenticated
requests, so empty logs meant "rejected at auth", not "never arrived".)

## Secondary bugs (observed, not yet fixed)

- **`copyfile` → EFAULT.** claude's config-backup (`fs.copyFile '/root/.claude.json' →
  '/root/.claude/backups/…'`) fails with EFAULT. Non-fatal (claude proceeds via temp-write)
  but a real kernel bug — m3OS's copy syscall (`copy_file_range`/`sendfile`) returns EFAULT
  instead of working or cleanly `-ENOSYS`-ing for Node's fallback. Find which syscall Node's
  `copyFile` uses; implement it or return `-ENOSYS`.
- **SMP contention under heavy node (multi-core).** Earlier 4-core runs showed `[sched]
  stale-ready`, `spurious write-fault recovered`, `vfs_server: ipc_reply failed`, and a
  `virtio-blk request timeout`. Repo gates pin `-smp 1` for heavy-node/claude. Likely related
  to #4's concurrency surface; worth a look alongside it.
- **Unhandled syscalls = RED HERRINGS** (correctly `-ENOSYS`, Node falls back): inotify_init1
  (294), io_uring_setup (425), mremap (25), clock_nanosleep (229), capget (125).

## Key environment facts

- m3OS login shell is **`/bin/ion`** (not POSIX sh); `export VAR=value` works.
- **`KERNEL_STACK_SIZE` = 64 KiB**, and syscalls run on the per-task 64 KiB kstack (set via
  `set_per_core_syscall_stack_top(kstack_top)` on switch) — NOT the 16 KiB static
  `SYSCALL_STACK` (that is only the initial RSP0). The earlier "16 KiB syscall stack" framing
  was wrong; #2's overflow math is against 64 KiB.
- `MAX_FDS` is now 128 and the fd table is **heap-backed** (`Vec`), so raising it is a
  one-line, heap-cost-only change.
- `PROCESS_TABLE` is a heap `Vec<Process>`; node threads SHARE one fd table via
  `shared_fd_table: Arc<Mutex<…>>`, so per-process (not per-thread) fd cost is what matters.
- Default `qemu_smp_count()` = 4; override `M3OS_SMP=1`. `M3OS_KVM=1` for near-native + real
  PKU (claude's V8 WASM codegen needs PKU for its W+X mappings; TCG has no PKU).
- `M3OS_CLAUDE_FAST_ITER=1` reuses the installed data disk (kernel still rebuilt → fixes
  included). The default bundled node is JITLESS-but-`wasm-in-jitless` (it DOES compile WASM
  to native code → the `[wx] v2-guarded W+X mapping (pkey=1)` lines), so claude exercises the
  PKU W+X path even on the "jitless" node. **The seeded credential is now RE-STAMPED into the
  reused disk on every FAST_ITER run** (`restamp_claude_credential`, via `debugfs -w
  "disk.img?offset=1048576"`), so a key change between runs always takes effect — without this,
  a reused disk silently kept its old credential (the 2026-06-16 footgun: an old Anthropic key
  401'd against OpenRouter despite a fully working network path, costing a long false-trail
  debugging detour). Requires host `debugfs` (e2fsprogs ≥ 1.45); a missing debugfs warns and
  proceeds with the disk's existing credential.
