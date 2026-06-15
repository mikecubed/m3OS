---
status: IN PROGRESS — ROOT-CAUSED + first fix landed, verification run in flight.
  Claude Code locks up on m3OS. Root cause: the per-process fd table was capped at
  MAX_FDS=32, which Node/Claude Code exhaust in a single session → the next
  open/socket/pipe returns EMFILE and the agent wedges (observed `rg error
  (code=EMFILE)` immediately before the lock-up, in claude's own debug log). Fix:
  raised MAX_FDS 32→128 (commit 3d92bb40). The ENTIRE networking stack is exonerated
  (a raw authed streaming POST to the API returns 200/DONE from m3OS) — this was
  never a network bug. **VERIFICATION INCONCLUSIVE / NEW CONCERN:** the post-fix
  verification runs got STUCK at `claude --version` (single-core ~22 min, multi-core
  ~10 min, both frozen) — yet `claude --version` was FAST (~tens of seconds) in every
  pre-fix run (MAX_FDS=32). Strongly suggests raising MAX_FDS slowed node startup, BUT
  it is confounded (FAST_ITER reused-disk bloat after ~10 runs + long-session host
  load + single-core VFS serialization), so a CLEAN controlled re-test is the #1 open
  item before trusting 128. See "Verification status".
branch: feat/phase-90b-claude-code
key-commits:
  - 9e09b67c  kernel/net: accept TCP keepalive socket options (fixes setsockopt ENOPROTOOPT)
  - 28e42a55  ports/claude-code: depend on ca-certificates (CA bundle for the launcher)
  - 3d92bb40  kernel/process: raise MAX_FDS 32 → 128 (fixes Claude Code EMFILE lock-up)
  - c7e443ca  kernel/smp/tlb: word ack-timeout diagnostic per regime (PR #247 review)
date: 2026-06-15
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

## Verification status (2026-06-15) — INCONCLUSIVE, must re-test cleanly

After committing `3d92bb40` (MAX_FDS=128), two verification runs of `claude -p` over
OpenRouter (Haiku 4.5) both **stalled at `claude --version`** (step 14):
- single-core (`M3OS_SMP=1`): ~22 min, frozen at step 14 (never produced `2.1.112`).
- multi-core (default 4): ~10 min, frozen at step 14.

This is suspicious because `claude --version` was FAST (reached step 30 within ~120 s)
in EVERY pre-fix run (MAX_FDS=32). The only kernel delta is MAX_FDS 32→128, so the raise
is the prime suspect for slowing/breaking node startup. **But the data is confounded** —
both runs used `M3OS_CLAUDE_FAST_ITER=1`, reusing a data disk that ~10 prior runs bloated
with `.claude.json` + many `backups/` (claude reads/backs-up config at startup), plus
host load from a very long session, plus single-core's lack of node↔vfs_server I/O
parallelism. So we cannot yet conclude 128 itself is the cause.

Leading hypotheses for a real MAX_FDS-driven slowdown (investigate in order):
1. **libuv "close all fds to RLIMIT_NOFILE" startup loop.** `getrlimit(RLIMIT_NOFILE)`
   now reports 128, so libuv iterates fd=3..128 (vs 3..32) calling close/F_SETFD on each.
   Phase 89 already fixed an F_SETFD busy-spin here (`F_SETFD→EBADF`); confirm it still
   holds across the larger range and isn't re-triggering a spin. If m3OS has no
   `close_range(2)`, libuv falls back to the per-fd loop — implementing `close_range`
   would make this O(1) regardless of MAX_FDS.
2. **kstack pressure.** `fd_table_snapshot()`/`new_fd_table*()` materialize a
   `[Option<FdEntry>; 128]` (~8 KiB) BY VALUE on the 16 KiB syscall stack during
   fork/exec. Confirm this doesn't overflow/near-overflow under node's exec/clone call
   chain (login/sh0 exec fine at 128, so simple exec is OK — but node's deeper path may
   not be). If implicated, move the fd table to the heap (see secondary bug below).

**#1 NEXT-SESSION TASK — clean controlled re-test (isolate MAX_FDS):**
- `cargo xtask clean` (fresh data disk — removes the bloated `.claude` state), then run
  WITHOUT `M3OS_CLAUDE_FAST_ITER`, multi-core, and **time `claude --version`** at
  MAX_FDS=128 vs a quick build at MAX_FDS=32. If 128 is materially slower → confirmed;
  pursue hypothesis 1/2. If comparable → 128 is fine and the stalls were disk-bloat/load.
- A lighter isolation: boot a fresh image at 128 and run plain `node --version` (the
  `node-smoke` path) — if node itself is slow to start at 128, it's MAX_FDS, not claude.

## Secondary bugs (observed, NOT yet fixed — next-session candidates)

- **`copyfile` → EFAULT.** claude's config-backup (`fs.copyFile`) fails:
  `Failed to backup config: Error: EFAULT: bad address in system call argument, copyfile
  '/root/.claude.json' -> '/root/.claude/backups/...'`. m3OS's file-copy syscall
  (`copy_file_range`/`sendfile`) returns EFAULT instead of working or cleanly ENOSYS-ing
  so Node falls back. **Non-fatal** (claude proceeds via temp-write) but a real kernel bug
  — likely a bad arg-validation / unimplemented-but-not-ENOSYS handler. Find which syscall
  Node's copyFile uses and either implement it or return `-ENOSYS` so Node's userspace
  fallback engages.
- **MAX_FDS heap refactor (raise 128 → 1024).** To reach Linux's standard 1024 the fd
  table must move off the kstack: change `Process.fd_table` and `shared_fd_table` from
  inline `[Option<FdEntry>; MAX_FDS]` to a heap `Vec<Option<FdEntry>>` (or `Box<[_]>`), and
  make `new_fd_table*()` + `fd_table_snapshot()` heap-construct/return so neither lands on
  the 16 KiB syscall stack. Then 1024 is safe. Only needed if 128 proves tight under
  heavier agent use.
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

1. **Clean controlled re-test of MAX_FDS=128** (see "Verification status"): `cargo xtask
   clean`, fresh non-FAST_ITER multi-core boot, time `claude --version` at 128 vs 32 to
   isolate whether the raise slowed node startup. Then confirm the OpenRouter round-trip
   answers `579`. If 128 slows startup → fix the fd-close scaling (close_range / verify
   the libuv F_SETFD loop) BEFORE relying on the raise. If startup is fine → 128 stands;
   confirm the round-trip and we're done for the lock-up.
2. If 128 is tight under real agent use, or implicated in the kstack pressure →
   **heap-refactor the fd table** (`Vec`/`Box<[_]>`) so neither construction nor the
   snapshot lands on the 16 KiB syscall stack, enabling MAX_FDS=1024 safely.
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
