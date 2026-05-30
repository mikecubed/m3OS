---
status: resolved
resolution: "ROOT CAUSE PINNED + FIXED on feat/phase-77-pre-1-0-cleanup (2026-05-28). A PID-scoped /proc syscall trace (open path + read content + getdents count, gated on htop's pid so the boot-log flood is excluded) showed the real sequence: per PID htop opens /proc/<pid> (dir, OK), /proc/<pid>/statm (OK, reads '16 16 0 0 0 16 0' which parses to 7 fields fine), then opens /proc/<pid>/task/<pid>/stat — which returned ENOENT because m3OS never implemented the /proc/<pid>/task/ subtree. htop's readStatFile reads the MAIN THREAD's stat via task/<pid>/stat whenever scanMainThread is true (the default: threads-shown, not a kernel thread, top-level scan), so the ENOENT made readStatFile return false → goto errorReadingProcess → every process discarded. The earlier 'statm opens but stat never does' observation was correct but mis-attributed: htop never opens /proc/<pid>/stat at all in default config; it opens /proc/<pid>/task/<pid>/stat. FIX: implemented the /proc/<pid>/task/<tid>/ subtree (stat/statm/status/comm/cmdline/maps/io/fd/exe) in kernel/src/fs/procfs.rs. Verified: cargo xtask htop-render-probe now PASSES (480 changed band scanlines vs the 20 threshold; 0 before). Also fixed three adjacent correctness gaps surfaced during the dig: (1) /proc/<pid>/stat now emits real utime/stime from the scheduler's per-task tick accounting (was hard-coded 0 → flat 0% per-process CPU) and a corrected 52-field layout (was off-by-one before starttime); (2) /proc/stat now derives real busy/idle from per-task ticks (was hard-coded 10%/90% → htop CPU meter pinned ~10%); (3) /proc/loadavg + the /proc state column now read the scheduler's canonical TaskState instead of Process.state, which m3OS only ever sets to Ready (so load read a flat ~20 and every process showed state R). Regression gate added: M3OS_HTOP_REGRESSION=1 → htop-render-probe in .githooks/pre-push."
branch: feat/phase-77-pre-1-0-cleanup
last-known-good-commit: 435f0a1
date: 2026-05-20
component: /proc filesystem + htop port
related:
  - docs/handoffs/2026-04-28-graphical-stack-startup.md
  - docs/roadmap/72-compositor-tiling-workspaces.md
  - kernel/src/fs/procfs.rs
  - userspace/doom/dg_m3os.c  (not directly related — used as the other "Phase 72b client" reference)
  - ports/system/htop/Portfile
---

# Handoff — htop shows zero processes (even as root)

## ✅ RESOLVED (2026-05-28, feat/phase-77-pre-1-0-cleanup)

**Root cause:** htop's `LinuxProcessTable_readStatFile` reads the *main
thread's* stat via `task/<pid>/stat` (the `scanMainThread` path, which is
the default — threads shown, not a kernel thread, top-level scan). m3OS
never implemented the `/proc/<pid>/task/` subtree, so that `openat`
returned `ENOENT` → `readStatFile` returned false → `goto
errorReadingProcess` → **every** process discarded. The original
"statm opens but stat never does" note was a correct *observation* but
mis-attributed: in its default config htop never opens
`/proc/<pid>/stat` at all — it opens `/proc/<pid>/task/<pid>/stat`.

A PID-scoped `/proc` syscall trace (open path + read bytes + getdents
count, gated on htop's pid to dodge the boot-log flood that defeated the
first attempt) pinned it definitively:

```
open "/proc/1"             -> fd 4   (dir OK)
open "/proc/1/statm"       -> fd 5   ("16 16 0 0 0 16 0\n" — parses fine)
open "/proc/1/task/1/stat" -> -2     (ENOENT)  ← discards the process
```

**Fix (`kernel/src/fs/procfs.rs`):** implemented the
`/proc/<pid>/task/<tid>/…` subtree — `stat`, `statm`, `status`, `comm`,
`cmdline`, `maps`, `io`, `fd/`, `exe`, plus the `task/` dir listing — with
`<tid>` resolving to a thread snapshot (the main thread's `tid == pid`).

**Three adjacent correctness gaps fixed in the same pass** (surfaced by the
user noticing the header values were wrong):

1. `/proc/<pid>/stat` now emits **real `utime`/`stime`** from the
   scheduler's per-task tick accounting (`task_times_for_pid`, ms→USER_HZ
   `/10`); they were hard-coded `0`, so per-process CPU% was a flat 0%.
   Also corrected an off-by-one in the field layout (16 fields before
   `starttime` instead of 15) that misaligned `nice`/`starttime`/`processor`.
2. `/proc/stat` now derives **real busy/idle** from per-task ticks
   (non-idle tasks = busy, per-core idle tasks = idle); it was a hard-coded
   10 %/90 % split, which pinned htop's CPU meter at ~10 %.
3. `/proc/loadavg` and the `/proc` **state column** now read the
   scheduler's canonical `TaskState` instead of `Process.state` — which
   m3OS only ever sets to `Ready`, so load read a flat ~20 and every
   process showed state `R`. New scheduler accessors:
   `runnable_task_count`, `global_cpu_times`, `task_state_for_pid`.

**Verification:** `cargo xtask htop-render-probe` now **PASSES** (≈480
changed process-table band scanlines vs. the 20 threshold; 0 before).
Trace confirms `task/<pid>/stat` resolves and returns real per-thread
state (`init`=S, `display_server`=R, `term`=S) and CPU times.

**Regression gate:** `M3OS_HTOP_REGRESSION=1` → `htop-render-probe` in
`.githooks/pre-push` (added to the AGENTS.md gate table).

The investigation notes below are retained for historical context; several
"likely root causes" they list were ruled out by the trace.

## Symptom

`/usr/local/bin/htop` launched from a SUPER+RETURN-spawned terminal
shows the header (CPU bars, memory bar, load average, uptime) but the
process list is empty. The user confirmed this happens **even when
running htop as root**, so the obvious "EUID filter" suspect does not
apply.

## Reproduction

1. `cargo xtask run-gui` (default graphical-only mode, greeter login).
2. Log in as any user (root or `user`).
3. SUPER+RETURN to spawn a term.
4. `htop` at the shell prompt.
5. Expected: process list rows under the header.
6. Actual: header renders, body is empty.

## Already ruled out

- **EUID-filter on `/proc` enumeration.** The Phase 72b commit
  `435f0a1` removed it so non-root users can also see all PIDs, but
  it was never the cause for the user's case — root already passed
  the filter via `caller_euid == 0` at
  `kernel/src/fs/procfs.rs:179` (pre-fix) and currently passes
  trivially because the filter is gone entirely. Useful side
  effect: `htop` / `ps` / `top` now work for unprivileged users
  whose tools previously only saw their own PID tree.
- **PID enumeration entirely missing.** `procfs::list_dir("/proc")`
  iterates `PROCESS_TABLE` and pushes one entry per PID
  (`kernel/src/fs/procfs.rs:177-186`). Not zero entries.
- **`process_snapshot` blanket-rejecting.** Same Phase 72b commit
  dropped the EUID gate at the per-PID file level; renders fire for
  every PID now.

## Likely root causes (ranked)

### 1. `getdents64` / `getdents` semantic mismatch
htop on Linux uses `readdir(3)` (libc) which under glibc calls
`getdents64`. m3OS's userspace ports are statically linked against
musl, and musl's `readdir` may also prefer `getdents64`. Worth
verifying the kernel implements **both** legacy and 64-bit variants
identically, including the d_off field and d_reclen alignment.

`kernel/src/arch/x86_64/syscall/mod.rs:13207-13225` is the
directory-entry build path; the syscall dispatch surrounding it
should be checked for `getdents` vs `getdents64` opcodes.

### 2. `/proc/<pid>/stat` field count or escaping
htop's scanner uses a fixed `sscanf` template against
`/proc/<pid>/stat`. The renderer at
`kernel/src/fs/procfs.rs:680-709` emits 52 whitespace-separated
fields with the canonical `comm` in parentheses. If the process
name contains `)` or `(` it will desync the parser. Most likely
candidate to break a row is an embedded paren or newline in
`proc.comm`. `proc_name` (line 596) **should** sanitise but worth
re-reading.

### 3. `/proc/<pid>/status` missing a field htop relies on
Linux's `/proc/<pid>/status` has dozens of fields. Our renderer
(`kernel/src/fs/procfs.rs:529-557`) emits a minimal set: Name,
State, Pid, PPid, Uid, Gid, Threads, VmSize, Cwd. htop may want
**Tgid**, **VmRSS**, **VmData**, **VmStk**, **State** in the exact
two-letter form (`R (running)` vs `R`). A missing or differently-
formatted field could cause htop to skip the row entirely.

### 4. `openat(AT_FDCWD, "/proc", O_DIRECTORY | O_RDONLY)` returns
something unexpected
Some htop builds open `/proc` once and `openat` per-PID under that
dirfd. If our VFS doesn't preserve dirfd semantics across
`openat(dirfd, "<pid>")`, every per-PID open fails and the scanner
sees zero processes despite enumerating PIDs successfully.

### 5. htop expects `/proc/cpuinfo` rows or `/proc/stat` cpu lines
in a specific shape and bails early
We emit both files (`kernel/src/fs/procfs.rs:377` and `:443`). The
formats are best-effort. If htop's per-CPU bar logic fails to
parse the global `/proc/stat`, the entire scanner pass may
short-circuit (depending on htop's main loop structure).

## Investigation plan

The fastest signal-to-noise path is to **instrument the kernel
procfs paths**, run htop in the guest, and watch the host serial log.

1. **Boot the guest with verbose logging on the procfs path.** Add
   one `log::info!` per call in `procfs::list_dir`,
   `procfs::read_file`, and `procfs::path_node` recording the path
   and (for `read_file`) the byte count returned. Build, run-gui,
   log in, spawn term, launch htop. Save the serial transcript.

2. **Diff against a Linux-host strace.** Run `strace -e
   trace=file,openat,read,getdents htop` on a Linux host briefly,
   capture which `/proc` paths htop opens in what order. Compare
   against step 1 to find the first divergence: a path htop opens
   on Linux but never opens on m3OS, OR an opened-but-empty read,
   OR a getdents that returns one or two entries vs many.

3. **If the divergence is at `getdents64`**, test directly: write a
   tiny userspace binary that `openat("/proc", O_DIRECTORY)` and
   loops `getdents64` printing each entry. Compare against running
   the same binary on a Linux host with `/proc` mounted.

4. **If the divergence is at a specific file**, dump that file's
   bytes from m3OS (`cat /proc/<pid>/stat | hexdump`) and compare
   to Linux. Most likely culprits are `Name:` escaping, `(comm)`
   parens, or whitespace-vs-tab in `/proc/<pid>/status`.

5. **Add a regression test once the root cause is fixed.** The
   current `cargo xtask tui-app-smoke` gate launches htop and
   asserts the header renders + `q` quits cleanly, but it does NOT
   assert that any process row appears. Extend the gate to grep the
   captured screen output for at least one canonical PID (1 for
   init, or the htop PID itself) so this regression cannot slip
   past CI again.

## Relevant files

- `kernel/src/fs/procfs.rs` — every `/proc` renderer + the
  `list_dir` / `read_file` / `path_node` entry points.
- `kernel/src/arch/x86_64/syscall/mod.rs:13195+` —
  `dirent_type_for_path`, `getdents`-style entry builder, the
  ext2/tmpfs/procfs dispatch for readdir.
- `kernel/src/process/mod.rs:796+` — `Process::cmdline` / `comm`
  storage that feeds `ProcessSnapshot`.
- `ports/system/htop/Portfile` and `xtask/src/port_build.rs` (the
  `build_htop` function) — version + configure flags. Currently
  pinned at htop 3.4.0 against ncurses 6.5.
- `xtask/src/main.rs::tui_app_smoke_steps` — the smoke gate that
  launches htop. Extending its assertions is the regression-test
  hook.

## What this PR (Phase 72b) accomplished related to /proc

- `435f0a1` removed the EUID filter on `/proc` enumeration and
  per-PID reads so non-root users see all processes (Linux-default
  behaviour). This is independently useful but did not fix the
  root-case empty list.

## Why this is parked rather than fixed in PR #183

Phase 72b's branch (`feat/phase-72-compositor-tiling`) has already
absorbed the full compositor close-out scope plus several
follow-ups; htop's process-list bug is a /proc / VFS issue with no
direct relationship to tiling. Investigating it well needs a
focused boot-and-instrument session that's better spent on its own
branch than smeared into the Phase 72 merge.

## Pickup checklist

- [x] Instrument the per-PID `/proc` path — done as a PID-scoped trace
      (open path + read bytes + getdents count) gated on htop's pid, so
      the boot-log flood that defeated the first attempt was excluded.
- [x] Capture serial log — via `cargo xtask htop-render-probe` (headless
      QMP/VNC), which drives htop in the graphical term.
- [x] Identify the first divergence — `/proc/<pid>/task/<pid>/stat`
      returned ENOENT (the `task/` subtree was unimplemented).
- [x] Fix the responsible path — implemented `/proc/<pid>/task/<tid>/…`
      in `kernel/src/fs/procfs.rs`.
- [x] Assert a populated table renders — `htop-render-probe` already does
      this (changed-band-scanline check); wired into pre-push as the
      `M3OS_HTOP_REGRESSION=1` gate.
- [x] Remove the instrumentation before merging — done (the trace was
      temporary; `grep htoptrace kernel/src` is clean).
