# Phase 57d Graphical Boot — Debugging Handoff

**Status:** Open. Multiple intermittent symptoms surfaced after Phase 57d's
voluntary-preemption landing; SHM surface transport built and working when
the underlying scheduler bug doesn't fire. The remaining work is hunting a
**run-queue / dispatch invariant violation on AP cores** that occasionally
strands a freshly-forked task and never schedules it.

**Branch:** `feat/57d-voluntary-preemption` (pushed). Ahead of `main` by 17
commits past the original 57d landing; the most recent 12 are diagnostic and
fix work landed during the post-merge debugging.

---

## TL;DR for next session

1. **Root cause hypothesis:** kernel scheduler / run-queue bug. A
   fork-child task assigned to a non-zero core (`target_core=1` reproduces
   most reliably) is sometimes enqueued via `enqueue_to_core` but never
   dispatched. `pid=2` (early service) goes first and runs fine; `pid=8` /
   `pid=19` arrive at core 1's queue later and are never picked up. No
   panic, no log, just silent stall.
2. **The diagnostic to look at:** in any failure log, search for
   `fork-task-spawn pid=N task_idx=I target_core=C` lines without a matching
   `fork-child pid=N trampoline-enter` line. Those are the stuck pids.
3. **Don't bother investigating SHM, term rendering, or input routing
   first** — those work correctly when the scheduler bug isn't firing. The
   intermittent visibility issues at the application layer are downstream
   effects of stuck fork-children (no shell on PTY → no echo → no
   PutGlyph → no compose).
4. **First thing to try:** read `kernel/src/task/scheduler.rs` looking at:
   - `enqueue_to_core` and what wakes the target core
   - the AP scheduler loop and how it scans its own run queue
   - whether IRQ-return preemption is actually firing on cores that
     have a long-running user task
   - `least_loaded_core` and whether it's picking a core that's
     actually pulling from its queue

---

## What's been built / committed

### Architectural changes (production fixes)

| Commit | Topic | Layer |
|---|---|---|
| `c745d45` | Initial framebuffer wipe + lazy `display.input-owner` service | userspace display + stdin_feeder |
| `7d27a3a` | Term emits `RenderCommand::Clear` at startup so surface attaches | userspace term |
| `73c25b6` | Term compose throttled to ~60 Hz | userspace term |
| `d98d136` | Term drains ALL pending events per iter (was: just one) | userspace term |
| `986aa37` | `MAX_BULK_LEN` 4 KiB → 64 KiB (interim fix, superseded by SHM) | kernel IPC |
| `f88aa80` | Mark surface dirty on Damage when buffer attached | userspace display_server |
| `9568693` | Snapshot SHM pixels into owned `Vec` per compose | userspace display_server |
| `673f400` | Key-repeat targets only the most-recently-pressed held key | kernel-core input |
| `7f6f6c4` | **Shared-memory surface transport** — replaces chunked-pixel | kernel mm + protocol + display + term |

### Diagnostic infrastructure

| Commit | What it logs |
|---|---|
| `968b579` | `kernel/Cargo.toml` `exec-trace` feature; logs at fork-child first userspace entry, every `close`/`dup2`/`execve` |
| `12bedb0` | Skip pid=1 close logging (drops init reap-loop spam) |
| `0decf62` | display_server compose stats every 60 frames |
| `0468d3f` | SHM `incref` / `map_user_frames` failure paths logged in kernel; `AttachSharedBuffer ok/fail shm_id va` in display_server |
| `cbcdeb3` | display_server compose first-5 + every-60 with result tag (`ok0`/`okN`/`err`) |
| `463030d` | Fork-trampoline entry logged BEFORE any panicking step |
| `ad59f3a` | `[exec-trace] fork-task-spawn pid task_idx target_core` at info-level |
| `73e5c1d` | term per-1000-iter stats: iter, events_pulled, composes, pty_bytes |

All diagnostic logs gated on `cfg(feature = "exec-trace")`. Enable with:
```bash
M3OS_KERNEL_FEATURES=exec-trace cargo xtask run-gui --fresh 2>&1 | tee m3os.log
```

The `M3OS_KERNEL_FEATURES` env var path is plumbed through `xtask/build_kernel`
in commit `968b579`.

---

## Open symptoms observed across many boots

The user has run ~30+ boots. Symptoms are **intermittent** and don't all
co-occur. Likely all downstream of one root cause.

### S1 — Forked task never enters userspace (the smoking gun)

`init: started 'X' pid=N` appears, kernel emits
`[INFO] [proc] fork: parent_pid=... child_pid=N`, `[exec-trace] fork-task-spawn
pid=N task_idx=I target_core=C` appears, but **no matching
`fork-child pid=N trampoline-enter` line.** The task is on the run queue
but is never dispatched.

**Reproduced for various pids across boots:**
- `pid=7` (display_server) → m3os-no-display.log ⇒ no display the whole boot
- `pid=8` (mouse_server), `pid=15` (session_manager), `pid=17` (term),
  `pid=19` (term-fork-child / ion) → various logs ⇒ partial functionality
- `pid=18` (login) on the same boot DID dispatch. Difference: `pid=18`
  was assigned `target_core=0`; the stuck pids were `target_core=1` (most
  recent log) or whatever core was busy.

**Most recent failure log:** `m3os.log` (May 1 20:34) — pid=8 and pid=19
both stuck on `target_core=1`. pid=2 on core=1 went first and ran fine.
Hypothesis: **once a long-running user task pins core 1, subsequent
ready tasks enqueued there are never picked up.**

### S2 — Symptoms downstream of S1

- **No shell prompt / commands don't work:** ion's fork-child is the
  pid most likely to get stuck (it's late in the boot). Without ion,
  PTY slave has no reader.
- **Mouse cursor doesn't move:** mouse_server (pid=8) sometimes stuck.
- **"Outbound queue full" overflow:** display_server's per-client
  outbound queue fills up because term isn't draining (ion's fork-child
  hangs term in some way — possibly nonblocking write to PTY master
  isn't actually nonblocking, or some other secondary effect).
- **Last-line-only-shows-first-char visibility bug:** *unconfirmed
  whether it's still real after the SHM fix.* Reported only in earlier
  logs from the chunked-path era.

### S3 — Ion crashes at `rip=0x4c1e10` (separate, intermittent)

Same RIP that the H.3 acceptance gate claimed to fix via per-task
`fxsave64`/`fxrstor64`. Recurs occasionally. Phase 57d's commit `142e420`
added FPU save/restore in scheduler dispatch but the crash returns
roughly 1-in-N boots. **Might or might not be related to S1.**

### S4 — Held-key repeat noise (cosmetic)

`kbd_server: warn: held-key table overflow; oldest key dropped` appears
when 9+ keys are held. Defensive cap at 8; message text is now slightly
misleading after `673f400` (we only repeat the highest-age key, but the
table still tracks all held keys for de-dup). Cosmetic only.

---

## Where to investigate next

### Highest-value lead: scheduler run-queue invariants

Read `kernel/src/task/scheduler.rs` with these questions:

1. **`enqueue_to_core(target_core, idx)`** at line ~1188 — does it post
   any cross-core notification (IPI / wakeup) so the target core
   actually re-scans its run queue? Or does it just push to the queue
   and rely on the target core checking on its next scheduler decision?

2. **AP scheduler loop** — `pub fn run() -> !` loop (line ~3458). Each
   core's scheduler picks a task, dispatches via `switch_context`. When
   the task yields / blocks / preempts, the core re-runs the dispatch
   loop. **If the task NEVER yields and IRQ-return preemption doesn't
   fire** (preempt_count > 0 the whole time, or `from_user == false`,
   or `reschedule == false`), the core never picks up newly-enqueued
   tasks.

3. **Does the AP timer-IRQ pipeline actually exist?** BSP runs on
   APIC ID 0. APs were brought up via the trampoline. Each AP should
   receive its own LAPIC timer interrupts. Verify via boot log:
   ```
   [INFO] [smp] AP core_id=N (APIC ID=N) is online
   ```
   appears for cores 1, 2, 3. But is the timer ARMED on each AP?

4. **`least_loaded_core` (line ~1144)** — does it pick a core that has
   capacity to actually run the task? Pid=8 was assigned core=1 even
   though pid=2 was already running there. A truly least-loaded
   selection should consider whether the core's current task is
   blocking-vs-running; a "ready"-state task on a core whose current
   user task never preempts is effectively unrunnable.

5. **`PENDING_REENQUEUE` machinery** (around line 1274 + 1833 + 1895
   + 2277 + 2511 + 3598) — this is the post-`switch_context` epilogue
   that commits the previous task's `saved_rsp` and re-enqueues it.
   Phase 57d added the preempt path here. Verify: when a task is
   newly *enqueued* (not re-enqueued from preemption), does the
   target core's dispatch loop actually scan its queue?

### Concrete repro recipe

```bash
M3OS_KERNEL_FEATURES=exec-trace cargo xtask run-gui --fresh 2>&1 | tee m3os.log
```

Reproduces in 3–10 boots. Failure pattern: search for
`fork-task-spawn pid=N task_idx=I target_core=C` in the log, then check
whether the matching `fork-child pid=N trampoline-enter` line exists.
If a stuck pid is found, the core it was assigned to is the broken one.

### If a focused look at scheduler doesn't reveal the bug

Add `sched-trace` (already a Cargo feature in `kernel/Cargo.toml`) plus
an on-demand `dump_sched_trace_rings()` invocation. Currently the rings
are only dumped on panic; if we add a syscall or a control-socket
verb to dump them on demand, we can see *exactly* what's happening on
each core's scheduler loop right when display_server starts logging
"outbound queue full" messages.

The function exists at `kernel/src/task/sched_trace.rs::dump_sched_trace_rings`
(see commit `cbcdeb3` for context). It just needs a syscall wrapper.

---

## Where NOT to look (already verified working)

- **SHM transport.** When the scheduler bug doesn't fire, term's SHM
  buffer mapping works end-to-end. `m3os3.log` from May 1 19:42 showed
  `display_server: AttachSharedBuffer ok shm_id=1 va=0x20003e8000`
  followed by 8+ seconds of healthy compose with growing write counts.
- **Term initial paint.** Commit `7d27a3a`'s startup `RenderCommand::Clear`
  paints the surface as soon as compose runs.
- **Outbound-queue overflow.** Commit `d98d136` made term drain ALL
  events per iter; when term is alive, queue stays empty.
- **MAX_BULK_LEN bump.** `986aa37` was an interim fix; once SHM lands,
  it's no longer load-bearing for term, but it's preserved as the
  per-IPC bulk ceiling for any remaining clients on the chunked path.
- **Background fill.** `c745d45` paints teal at framebuffer acquire
  and on first compose-pass fallback.
- **Lazy `display.input-owner`.** `c745d45` defers the service
  registration so `stdin_feeder` keeps PS/2 ownership until a
  graphical client maps a Toplevel.

---

## Files of interest (with last-touched commits)

| File | Recent commits | Why look |
|---|---|---|
| `kernel/src/task/scheduler.rs` | `73e5c1d`, `463030d`, `ad59f3a`, `142e420` | All scheduler dispatch / run-queue / FPU save logic |
| `kernel/src/process/mod.rs` | `463030d`, `ad59f3a`, `968b579` | `fork_child_trampoline`, `sys_fork`, FPU reset |
| `kernel/src/arch/x86_64/syscall/mod.rs` | `0468d3f`, `968b579` | SHM syscalls, exec-trace logs |
| `kernel/src/mm/shm.rs` | `7f6f6c4` | New refcounted shared-region registry (created by SHM rebuild) |
| `kernel/src/arch/x86_64/interrupts.rs` | `8f46411`, `5f4338c`, `f5c64ce`, `e0a842b` | Timer-IRQ preempt path; AP IRQ routing |
| `userspace/term/src/main.rs` | `73e5c1d`, `73c25b6`, `d98d136`, `7d27a3a` | Main loop with new diagnostics |
| `userspace/term/src/display.rs` | `7f6f6c4` | SHM-backed `DisplayClient` |
| `userspace/display_server/src/main.rs` | `cbcdeb3`, `0decf62`, `c745d45` | Compose loop diagnostic counters |
| `userspace/display_server/src/surface.rs` | `0468d3f`, `f88aa80`, `7f6f6c4` | `BufferStorage::Shared`, `AttachSharedBuffer` handler |
| `kernel-core/src/display/protocol.rs` | `7f6f6c4` | `ClientMessage::AttachSharedBuffer` |
| `kernel-core/src/input/keymap.rs` | `673f400` | KeyRepeatScheduler, only-newest-key behaviour |

---

## Logs in the workspace root (most recent first)

| File | Description |
|---|---|
| `m3os.log` (May 1 20:34) | Failure: pid=8 and pid=19 both stuck on `target_core=1` |
| `m3os-no-terminal.log` (May 1 20:04) | Failure: term + session_manager stuck, login (pid=18) ran |
| `m3os-no-display.log` (May 1 20:03) | Failure: display_server's pid=7 stuck, full text-fallback |
| `m3os3.log` (May 1 19:42) | Working boot: 8+ seconds of healthy compose |
| `m3os2.log` (May 1 19:41) | Partial: display worked, terminal partial |
| `m3os-fail-1.log` | Earlier failure pattern (May 1 17:26) |

The user has been clearing / overwriting old logs across iterations.
Keep the May 1 20:34 `m3os.log` as the canonical "smoking gun" because
it has both the new `fork-task-spawn` and `trampoline-enter` diagnostics
showing the exact stuck pids.

---

## Engineering-discipline notes

- **Don't add more diagnostic logging** until you've read the
  scheduler. We've added enough; the data is sufficient to point at
  the layer.
- **All diagnostic logs are still in the tree.** Once the bug is
  rooted, do a one-shot `chore: remove Phase 57d-followup diagnostic
  instrumentation` commit that reverts every `[exec-trace]` log line,
  the term per-iter stats, the display_server compose stats, and the
  `M3OS_KERNEL_FEATURES` xtask plumbing. Optionally keep the
  `exec-trace` feature flag itself behind `cfg(feature)` for future
  diagnoses, but its emit sites should be empty in the default build.
- **The SHM rebuild is real work** that should not be rolled back. It
  unlocks zero-copy pixel transport and the kernel SHM module is
  reusable beyond display.
- **The Phase 57d roadmap doc** (`docs/roadmap/tasks/57d-voluntary-preemption-tasks.md`)
  marks I.2 / I.3 / H.3 / H.4 as procedurally pending. Until the
  scheduler bug is fixed, none of those gates can be ticked, because
  intermittent boot failures preclude soak testing.
