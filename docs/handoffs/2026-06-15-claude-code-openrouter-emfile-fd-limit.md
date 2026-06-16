---
status: IN PROGRESS — 3 of 4 issues FIXED + pushed; claude no longer crashes or freezes
  the machine, but `claude -p` does NOT yet COMPLETE a request (a deep, intermittent
  scheduler/IPC concurrency stall in claude's startup remains). Fixed + verified this
  session: (1) EMFILE lock-up — MAX_FDS 32→128 (3d92bb40); (2) kstack-overflow segfault
  in node clone/fork — heap-backed fd table (952da571, proven by controlled A/B +
  node-smoke); (3) SYSTEM-WIDE FREEZE (frozen login screen) running claude — `stat("/dev/tty")`
  hung the kernel VFS path → handled as a char device (36074826, verified via claude's
  own --debug log advancing past it). REMAINING (the only blocker to end-to-end): the
  `claude -p` round-trip stalls in claude's `setup()`/skill-discovery startup phase — a
  clean, intermittent, SYSTEM-STAYS-RESPONSIVE wake/reblock-loop or lost-wake inside the
  reply-block IPC primitive under claude's ~10-thread concurrent `vfs_server` load. NOT a
  crash/freeze. Needs a dedicated instrumented pass (see "Next steps"); do NOT hot-patch
  the scheduler/IPC primitives blind (regression risk to the 3 landed fixes).
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
| 4 | `claude -p` never finishes | intermittent wake/reblock-loop / lost-wake in the reply-block IPC primitive under concurrent multi-thread vfs_server load | — | ❌ OPEN (the only end-to-end blocker) |

**The crashes and the system freeze are gone.** With #1–#3, claude boots, loads, JITs,
spawns ripgrep, handles signals, and runs deep into its agent startup without wedging the
machine. #4 is what stops a prompt from completing — it is NOT a crash or freeze (the system
stays responsive); it is a deep scheduler/IPC concurrency stall.

Networking is fully exonerated (see "Ruled out"). The kstack is NOT undersized: m3OS already
gives 64 KiB per task, 4× Linux's 16 KiB `THREAD_SIZE` — #2 was the kernel putting 12 KiB
buffers on the stack, which the heap fix removed.

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

## #4 — the remaining blocker: `claude -p` does not complete

**Symptom:** a `claude -p '…'` round-trip (OpenRouter) never prints the answer; the
claude-smoke step times out. NOT a crash, NOT a freeze — the system stays responsive
(`vfs_server` keeps replying, other node threads keep churning, no fault/panic/kill).

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

## Next steps — to get claude working end-to-end (priority order)

1. **INSTRUMENT first; do not patch blind.** The reply-block primitive
   (`block_current_until` / `block_current_on_reply_v2` in `kernel/src/task/scheduler.rs`) and
   `endpoint::call_msg` (`kernel/src/ipc/endpoint.rs`) are the kernel's most delicate
   concurrency code (the 2026-06-14 SMP work hardened them). A speculative change risks
   regressing fixes #2/#3 and seeding new lost-wakes. Add a SAFE, read-only diagnostic that
   makes the exact stall state visible on the next run:
   - A **cumulative-blocked-time / wake-reblock-cycle counter** per task (not the single-block
     `blocked_since` the current watchdog uses, which resets each cycle). When a task exceeds
     a cumulative threshold in `BlockedOnReply` (or churns >N reblock cycles), dump: the task
     (pid/state), whether `pending_msg` is set (reply arrived but not consumed = lost-wake vs
     never arrived = vfs stuck), its `reply_waker` flag, the endpoint's `senders`/`receivers`
     queues, and the outstanding `Capability::Reply` holder.
   - This single run distinguishes the three live hypotheses: (a) lost-wake (reply delivered,
     waker flag set, task not rescheduled), (b) livelock in the v2 re-check loop (woken flag
     toggling), (c) `vfs_server` never replied to THIS caller (request stranded/misrouted
     under concurrency).
2. **Fix per (1)'s verdict.** Likely audits: `ipc_reply` → `deliver_message` + `wake_task_v2`
   (`scheduler.rs:4721`/`4747`) and the single-threaded `vfs_server` reply loop
   (`userspace/vfs_server/src/main.rs:~1970-2009`) under CONCURRENT callers; the
   `block_current_on_reply_v2` re-check (`scheduler.rs:3841`); and the `metacache` lock in
   `vfs_service_stat_path` (`syscall/mod.rs:8553`) under concurrent stats.
3. **Verify the fix two ways:** (a) the OpenRouter round-trip answers `579` (command below);
   (b) a multi-thread concurrent-VFS stress (claude is the real one; a `node -e` that fans out
   many concurrent `fs.stat` on `/usr` paths would be a cheaper regression guard — consider
   adding it to `node-smoke` or a new gate).
4. **Lower priority / orthogonal:** `copyfile`→EFAULT (secondary bug below); the libuv
   event-loop angle (timerfd/epoll_wait returning-immediately-in-a-loop) as an ALTERNATIVE to
   the lost-wake hypothesis if (1) shows the thread is spinning rather than parked.

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
  cargo xtask claude-smoke --timeout 2400
# PASS pattern "579" = claude completed the round-trip (currently STALLS — issue #4).
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

The networking/HTTP stack is fully working. Proven via `node -e` probes against the real
OpenRouter API with the user's key: raw `https.request` POST → 200; `fetch()` non-streaming
→ 200; `fetch()` streaming read to completion → 200/DONE; and `node-smoke M3OS_NODE_NET=1`
(live HTTPS + `npm install`) PASSES. claude's request never reaches OpenRouter because it
stalls in startup (#4) before issuing the API call — the debug log shows no `Request to …`
line.

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
  PKU W+X path even on the "jitless" node.
