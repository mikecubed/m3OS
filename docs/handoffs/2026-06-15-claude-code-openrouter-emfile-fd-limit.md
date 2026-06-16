---
status: IN PROGRESS — SECOND ROOT CAUSE FOUND + FIXED (uncommitted, in working tree),
  pending a verification run. The MAX_FDS=32→128 raise (commit 3d92bb40) fixed the
  EMFILE lock-up but introduced a SECOND bug: with `MAX_FDS=128` the per-process fd
  table was an inline `[Option<FdEntry>; 128]` of a `VfsFileMeta`-bloated ~96-byte
  `FdEntry` ≈ **12 KiB**, living inline in `Process` AND returned by-value from
  `fd_table_snapshot()`. Node's `clone(CLONE_THREAD)` (libuv threadpool) / `fork`
  path stacks THREE of them at once (parent snapshot + `child_fds_copy` + the new
  `Process`'s inline copy) ≈ 36 KiB — which overflowed the 64 KiB per-task kernel
  stack the instant MAX_FDS went 32→128 (at 32 the three copies were only ~9 KiB).
  This is exactly the 06-15 "hypothesis #2 / kstack pressure" prediction, now
  CONFIRMED by the live `[int] DOUBLE FAULT = kstack overflow … attributable to pid
  43 [node]` (slot-math on `rsp=0xffff808000550df8` shows the FULL 64 KiB was
  consumed). The kernel's Track-D recovery killed node and survived, but node dying
  means claude can't run. **FIX (this session):** moved the fd table to the heap —
  `Process.fd_table` / `shared_fd_table` / `fd_table_snapshot()` / `new_fd_table()` /
  `spawn_process_with_cr3_and_fds()` are now `Vec<Option<FdEntry>>` (always length
  `MAX_FDS`), so only a 24-byte `Vec` header ever lands on the kstack. `cargo xtask
  check` passes. Networking remains fully exonerated. **OPEN:** verify node no longer
  overflows (re-run the OpenRouter round-trip / node-smoke); commit the fix.
branch: feat/phase-90b-claude-code
key-commits:
  - 9e09b67c  kernel/net: accept TCP keepalive socket options (fixes setsockopt ENOPROTOOPT)
  - 28e42a55  ports/claude-code: depend on ca-certificates (CA bundle for the launcher)
  - 3d92bb40  kernel/process: raise MAX_FDS 32 → 128 (fixes Claude Code EMFILE lock-up)
  - c7e443ca  kernel/smp/tlb: word ack-timeout diagnostic per regime (PR #247 review)
  - (uncommitted) kernel/process: heap-back the fd table (Vec<Option<FdEntry>>) —
    fixes the node clone/fork kstack overflow the MAX_FDS=128 raise introduced
date: 2026-06-15 (updated 2026-06-16)
component: kernel/process (fd table / MAX_FDS) + kernel/net (socket options) +
  ports/claude-code (DEPS) + xtask claude-smoke (OpenRouter/model harness plumbing,
  UNCOMMITTED in working tree)
artifacts:
  - m3os.log (project root) — the user's single-core run serial (kernel side)
  - debug1.log / debug2.log (project root) — claude's OWN debug logs (the smoking gun)
  - openrouter.sh (project root) — the user's OpenRouter env (key filled in locally; NOT committed)
---

## Goal

Get `@anthropic-ai/claude-code@2.1.112` (the pinned Node-runnable build, Phase 90b)
to actually complete a request on m3OS — first against OpenRouter (an
Anthropic-protocol proxy the user already runs on their laptop), as both a working
setup and a diagnostic vs the official `api.anthropic.com`.

## TL;DR — current status

- **ROOT CAUSE FOUND:** `MAX_FDS = 32` (kernel/src/process/mod.rs) is far too small for
  Node/Claude Code → **EMFILE** → lock-up. Fixed to 128 (commit `3d92bb40`, pushed).
- **Networking is NOT the problem** — proven exhaustively (see "Ruled out").
- **Immediate open item:** verify the MAX_FDS=128 fix lets claude complete a round-trip
  over OpenRouter (run was in flight when this doc was written — update the result).
- **Two tracked secondary bugs** (non-fatal, not yet fixed): `copyfile`→EFAULT, and the
  inline-fd-table design limiting MAX_FDS to 128 (heap-refactor needed for 1024).

## Root cause (the answer)

claude's own debug log (`/root/.claude/debug/*.txt`, pulled into `debug1.log`) ended with:

```
[DEBUG] rg error (signal=undefined, code=EMFILE, stderr: ), 0 results
```

right before the agent wedged. **EMFILE = too many open files.** The kernel caps every
process at 32 fds (`pub const MAX_FDS: usize = 32`), and Node/Claude Code blow past that
in one session:
- stdio (3) + libuv epoll/eventfd/timerfd/self-pipe (~6–8)
- the **custom undici agent** with the CA certs (claude builds its own dispatcher —
  `[DEBUG] TLS: Created undici agent with custom certificates`) → TLS sockets
- config / `.claude.json` / temp / lock / backup files
- **3 pipe fds per spawned `ripgrep`** (claude spawns rg repeatedly)

Once the 32-slot table fills, `fd_alloc()` returns None → the syscall returns `EMFILE`
(`NEG_EMFILE`), and claude locks up.

## Fixes landed (all on the branch)

1. **`9e09b67c` — TCP keepalive socket options.** `setsockopt(IPPROTO_TCP,
   TCP_KEEPIDLE/INTVL/CNT)` previously returned `ENOPROTOOPT`; libuv's
   `uv__tcp_keepalive` treats that as a fatal connect error → claude's first symptom
   ("Failed to connect … ENOPROTOOPT"). Now accepted+stored; the setsockopt catch-all is
   accept-and-log for best-effort options. Guarded by `connect-smoke` assertion 5
   (always-on, no network). The keepalive *prober* itself is deferred (see
   90b-claude-code.md "Deferred Until Later").
2. **`28e42a55` — ports/claude-code DEPS += ca-certificates.** The `/usr/bin/claude`
   launcher sets `NODE_EXTRA_CA_CERTS=/etc/ssl/certs/ca-certificates.crt`, but the package
   only `DEPS=node`, so that file was never installed ("Cannot open directory
   /etc/ssl/certs"). Now the solver installs the CA bundle dependency-first. (Confirmed
   working in claude's debug log: "Appended extra certificates from NODE_EXTRA_CA_CERTS".)
   NOTE: not the lock-up cause — Node falls back to built-in roots regardless.
3. **`3d92bb40` — MAX_FDS 32 → 128.** The actual lock-up fix. `getrlimit(RLIMIT_NOFILE)`
   already reports `MAX_FDS`, so it stays truthful. Bounded to 128 (not 1024) because
   `fd_table` is an inline `[Option<FdEntry>; MAX_FDS]` and `fd_table_snapshot()` returns
   it BY VALUE on the **16 KiB syscall stack** (`SYSCALL_STACK_SIZE`); at the 64-byte
   `FdEntry` class that is ≤8 KiB and fits. 128 is ~4× claude's observed ~30-fd ceiling.

## Verification status — hypothesis #2 CONFIRMED + FIXED (2026-06-16)

The prior stalls/lock-ups were the predicted **kstack pressure** (hypothesis #2 below),
now confirmed by a live crash and fixed by heap-backing the fd table.

**The confirming crash (2026-06-16 re-run):** node (`pid 43`) died with
```
[int] DOUBLE FAULT = kstack overflow (rsp=0xffff808000550df8) attributable to pid 43 — killing process; core recovers (no halt)
[WARN] [fault_kill] trampoline running for pid 43
```
Slot math nails it to a per-task kstack: area start `0xFFFF_8080_0000_0000`, slot vsize
`0x11000` (4 KiB guard + 64 KiB usable). `0x550df8 / 0x11000` = slot 80, whose guard page
is `0x550000..0x551000`; `rsp=0x550df8` sits 520 bytes INTO that guard — i.e. the entire
64 KiB usable stack was consumed and RSP marched past the bottom (a #DF because the
guard-page #PF couldn't push its own frame). Track-D recovery killed node and the core
survived, but a dead node means claude can't run.

**Why MAX_FDS=128 caused it.** `FdEntry` is ~96 bytes (Phase 88 embedded a `VfsFileMeta`
in `FdBackend::VfsService`), so `[Option<FdEntry>; 128]` ≈ **12 KiB**. It lived inline in
`Process` AND was returned by-value from `fd_table_snapshot()` AND passed by-value to
`spawn_process_with_cr3_and_fds()`. Node's `clone(CLONE_THREAD)` (libuv threadpool) and
`fork` paths stack THREE such copies simultaneously — `sys_clone_thread`: `parent_fds`
snapshot + `child_fds_copy = parent_fds.clone()` + the child `Process`'s inline copy ≈
36 KiB; `sys_fork`: `parent_fds` held in a tuple + the by-value arg to `spawn_*` + the
child `Process` ≈ 36 KiB. Plus the rest of the parent-state tuple (VmaTree clone, strings,
`[SignalAction; 32]`) and normal frames → > 64 KiB. At MAX_FDS=32 those three copies were
only ~9 KiB, which is why node started fine before the raise and overflowed right after.
(The handoff's "16 KiB SYSCALL_STACK" framing was imprecise — syscalls actually run on the
per-task **64 KiB** kstack via `set_per_core_syscall_stack_top(kstack_top)`; the overflow
math is against 64 KiB, not 16 KiB. Either way the inline 12 KiB ×3 was the killer.)

**The fix (this session, in the working tree — `cargo xtask check` passes):** moved the fd
table off the stack into the heap. `Process.fd_table` and `shared_fd_table` are now
`Vec<Option<FdEntry>>` (`Arc<IrqSafeMutex<Vec<…>>>` for the shared case); `new_fd_table()`,
`new_fd_table_pub()`, `fd_table_snapshot()` return `Vec`; `add_fd_refs()` takes `&[…]`;
`spawn_process_with_cr3_and_fds()` takes a `Vec`. The Vec is ALWAYS exactly `MAX_FDS` long
(every fd helper indexes `0..MAX_FDS`), built with `vec![None; MAX_FDS]` so even the
constructor never materializes an `[_; N]` stack temporary. Net: only a 24-byte `Vec`
header ever lands on the kstack; the clone/fork path's fd-table cost drops from ~36 KiB to
~100 bytes. This also makes raising `MAX_FDS` to Linux's 1024 a one-line change (heap cost
only, no stack budget) if claude ever needs more than 128 — kept at 128 for now (~4× the
observed ~30-fd ceiling).

Hypothesis #1 (libuv close-all-fds-to-RLIMIT loop) is NOT implicated by the crash but is
still a latent O(MAX_FDS) startup cost; `close_range(2)` would make it O(1). Track it
only if startup feels slow after the kstack fix lands.

**#1 NEXT-SESSION TASK — verify the heap fix:**
- Re-run the OpenRouter round-trip (command in "How to reproduce") and confirm node no
  longer hits `DOUBLE FAULT = kstack overflow … pid <node>` and claude answers `579`.
- Cheaper proxy: `M3OS_NODE_REGRESSION=1` `node-smoke` under `M3OS_KVM=1` — its egress arm
  exercises the same libuv `clone(CLONE_THREAD)` threadpool path that overflowed.
- Then commit the heap refactor (see key-commits).

## Secondary bugs (observed, NOT yet fixed — next-session candidates)

- **`copyfile` → EFAULT.** claude's config-backup (`fs.copyFile`) fails:
  `Failed to backup config: Error: EFAULT: bad address in system call argument, copyfile
  '/root/.claude.json' -> '/root/.claude/backups/...'`. m3OS's file-copy syscall
  (`copy_file_range`/`sendfile`) returns EFAULT instead of working or cleanly ENOSYS-ing
  so Node falls back. **Non-fatal** (claude proceeds via temp-write) but a real kernel bug
  — likely a bad arg-validation / unimplemented-but-not-ENOSYS handler. Find which syscall
  Node's copyFile uses and either implement it or return `-ENOSYS` so Node's userspace
  fallback engages.
- **MAX_FDS heap refactor — DONE (2026-06-16, uncommitted).** `Process.fd_table`,
  `shared_fd_table`, `fd_table_snapshot()`, `new_fd_table*()`, and
  `spawn_process_with_cr3_and_fds()` are now heap-backed `Vec<Option<FdEntry>>` (always
  length `MAX_FDS`), so neither construction nor the snapshot lands on the kstack. This
  was NOT just the "raise to 1024" nicety it was filed as — it was the FIX for the node
  clone/fork kstack overflow the MAX_FDS=128 raise introduced (see "Verification status").
  Raising to 1024 is now a one-line `MAX_FDS` change (heap cost only); left at 128.
- **SMP contention under heavy Node (multi-core).** The user's 4-core run showed
  `[sched] stale-ready` ×12, `spurious write-fault recovered` ×9, `vfs_server: ipc_reply
  failed` ×2, and one `virtio-blk request timeout`. The repo already pins `-smp 1` for
  heavy-Node/claude gates for this reason. EMFILE is core-independent, but for interactive
  claude prefer **`M3OS_SMP=1`**. The `vfs_server: ipc_reply failed` + virtio-blk timeout
  under SMP load are worth a separate look.
- **Unhandled syscalls = RED HERRINGS** (correctly ENOSYS, Node falls back): inotify_init1
  (294), io_uring_setup (425), mremap (25), clock_nanosleep (229), capget (125).

## What was ruled out (so the next session doesn't re-chase it)

The networking/HTTP stack is FULLY working on m3OS. Proven via `node -e` probes
(M3OS_CLAUDE_BASE_URL path, see harness below) against the real OpenRouter API with the
user's key:
- raw Node-core `https.request` POST → **200**
- `fetch()` (undici) non-streaming POST → **200**
- `fetch()` streaming POST read **to completion** → **200, 13 chunks, 2218 bytes, DONE**
- separately, `node-smoke M3OS_NODE_NET=1` (live HTTPS + `npm install`) **PASSES**

OpenRouter server-side I/O logging (user) confirmed: the user's tiny 16-tok probes appear
and succeed; claude's own request did **not** appear — because claude exhausts fds
(EMFILE) before/while doing its work, not because of any network failure.

## Uncommitted work in the tree (decide: commit cleanly or drop)

`xtask/src/main.rs` has OpenRouter/model harness plumbing (NOT committed):
- `M3OS_CLAUDE_BASE_URL` → exports `ANTHROPIC_BASE_URL` + empty `ANTHROPIC_API_KEY` +
  bearer `ANTHROPIC_AUTH_TOKEN` (from the staged 0600 key) before launching claude.
- `M3OS_CLAUDE_MODEL` → exports `ANTHROPIC_MODEL` + `ANTHROPIC_SMALL_FAST_MODEL`.
- the `base_url` arm of `claude_smoke_steps` currently runs the verification round-trip
  (echo env-check → `claude -p '…123 plus 456…'` → WaitPassOrFail on `579`).
This `M3OS_CLAUDE_BASE_URL`/`M3OS_CLAUDE_MODEL` support is genuinely useful (run Claude
Code against OpenRouter/any Anthropic-proxy). Worth committing a clean version once the
fix is verified; the round-trip/probe scaffolding is throwaway.

Also untracked: `openrouter.sh` (user's env, key filled in locally — do NOT commit),
`debug1.log`, `debug2.log`, `m3os.log`, `claude.png` (the original screenshot).

## How to reproduce / test

```
git pull   # get 3d92bb40 (MAX_FDS=128)
# Single-core jitless run against OpenRouter (key read from openrouter.sh, never printed):
M3OS_SMP=1 M3OS_CLAUDE_NET=1 \
  M3OS_CLAUDE_BASE_URL="https://openrouter.ai/api" \
  M3OS_CLAUDE_MODEL="anthropic/claude-haiku-4.5" \
  M3OS_CLAUDE_KEY="$(. ./openrouter.sh >/dev/null 2>&1; printf %s "$OPENROUTER_API_KEY")" \
  M3OS_KVM=1 M3OS_CLAUDE_FAST_ITER=1 \
  cargo xtask claude-smoke --timeout 2400
# PASS pattern "579" = claude completed a real round-trip over OpenRouter (fix works).
```
Interactive (visible screen, user's path): `M3OS_SMP=1 M3OS_WITH_CLAUDE=1 cargo xtask
run-gui`, then in the guest export the 4 ANTHROPIC_* vars + `claude --debug -p "hi"`.
Verify `echo $ANTHROPIC_BASE_URL` is non-empty (ion is the login shell, not POSIX sh —
`export VAR=value` should work but confirm). claude's debug logs are at
`/root/.claude/debug/`; pull them off the ext2 disk host-side with
`debugfs -R "rdump /root/.claude/debug /tmp/x" target/x86_64-unknown-none/release/disk.img`.

## Next steps (priority order)

1. **Verify the heap fd-table fix** (this session's change, uncommitted): re-run the
   OpenRouter round-trip (command below) and confirm (a) NO `DOUBLE FAULT = kstack
   overflow … attributable to pid <node>` appears, and (b) claude answers `579`. Cheaper
   proxy first: `M3OS_NODE_REGRESSION=1` `node-smoke` under `M3OS_KVM=1` (its egress arm
   drives the same libuv `clone(CLONE_THREAD)` path that overflowed).
2. **Commit the heap fd-table refactor** once verified (kernel/src/process/mod.rs +
   kernel/src/arch/x86_64/syscall/mod.rs + the slab.rs comment). `cargo xtask check`
   already passes. Suggested: also set `M3OS_NODE_REGRESSION=1` / `M3OS_SMP_REGRESSION=1`
   on the PR since the change touches the fork/clone fd path.
3. **Fix `copyfile`→EFAULT** (implement the copy syscall or return ENOSYS for fallback).
4. **Commit the clean `M3OS_CLAUDE_BASE_URL`/`M3OS_CLAUDE_MODEL` harness support**; drop the
   throwaway probe scaffolding.
5. Optional: revisit the SMP `vfs_server: ipc_reply failed` + virtio-blk timeout under
   heavy multi-core Node.

## Key environment facts

- m3OS login shell is **`/bin/ion`** (root:…:/bin/ion), NOT POSIX sh; it even rejects some
  `/bin/sh -c` forms. `export VAR=value` appears to work (gh/git gates use it) but verify.
- `KERNEL_STACK_SIZE` = 64 KiB; **syscalls run on a 16 KiB stack** — this bounds inline
  `[_; MAX_FDS]` by-value returns.
- `PROCESS_TABLE` is a heap `Vec<Process>` (no fixed slot); Node threads SHARE one fd table
  via `shared_fd_table: Arc<Mutex<…>>`, so per-process (not per-thread) fd cost is what matters.
- Default `qemu_smp_count()` = 4; override `M3OS_SMP=1`. `M3OS_KVM=1` for near-native + real PKU.
- `M3OS_CLAUDE_FAST_ITER=1` reuses the installed data disk (kernel still rebuilt → fix included).
