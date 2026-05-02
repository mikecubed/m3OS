# Phase 57d Graphical Boot — Debugging Handoff

**Status:** Root cause for the graphical-boot stall is fixed in the current
working tree and validated by a clean fork-trampoline pass. The follow-up
Ion/userspace null page fault at `rip=0x65e54b` is also fixed in the current
working tree; one bounded post-fix run still showed `term` stuck at
`pty_bytes=0`, so treat shell-prompt/liveness as the next separate follow-up if
it reproduces.

**Branch:** `feat/57d-voluntary-preemption` (pushed). Ahead of `main` by 17
commits past the original 57d landing; the most recent 12 are diagnostic and
fix work landed during the post-merge debugging.

---

## TL;DR for next session

1. **The stall root cause was two-part.** First, `syscall_entry` masked IF
   for the whole syscall body and the syscall-return path did not consume a
   pending reschedule by switching through the full-frame preemption path.
   Second, virtio-blk serialized requests with `REQUEST_LOCK:
   IrqSafeMutex<()>` across `do_request()`, but `do_request()` parks in
   `block_current_until()`. That let one task sleep while holding an
   interrupt-masking spin lock, then a later block caller could spin with
   IF=0 and pin an AP.
2. **The fix is preemption/blocking based, not yield based.**
   `syscall_entry` now re-enables IF after saving the full user register
   set, disables IF again before the return tail touches user RSP, and calls
   `syscall_return_maybe_preempt`. That helper builds a `PreemptFrame` from
   the syscall save area plus the syscall return value and enters
   `preempt_frame_to_scheduler` when `reschedule ||
   preempt_resched_pending` is set. Virtio-blk now uses a scheduler-blocking
   single-flight request slot with static waiter flags, scoped per sector.
3. **Final validation:** `cargo xtask fmt --fix`, `cargo xtask check`, and
   two bounded GUI boots with `M3OS_KERNEL_FEATURES=exec-trace`
   (`m3os-slot-preempt-final.log` and `m3os-slot-preempt-postdoc.log`). The
   logs reached `AUDIO_SMOKE:server:READY`, `TERM_SMOKE:ready`,
   `display_server: AttachSharedBuffer`, and `session_manager:
   session.boot: state=running`. Fork diagnostic parse in the latest run:
   `spawned 19 trampolines 19 missing 0`.
4. **Do not confuse the current virtio-blk fix with reverted commit
   `ee73f3c`.** That earlier attempt regressed and was reverted by
   `711715d`. The current implementation differs in the important places:
   static waiter flags instead of stack `woken` pointers, per-sector slot
   scope instead of whole-call scope, scheduler parking through
   `block_current_until`, and wake-all on release. A wake-one refinement was
   tested in `m3os-slot-preempt-wakeone.log` and regressed with missing
   `pid=19 task_idx=24 target_core=2`.
5. **Ion null-page fault fix:** `rip=0x65e54b` maps to musl `__init_ssp`
   (`mov %fs:0,%rax`). The fault was caused by `%fs.base` being restored to 0
   after syscall-return preemption could switch away from
   `arch_prctl(ARCH_SET_FS)` before the task's saved `UserReturnState.fs_base`
   was refreshed. The ELF aux vector was also missing `AT_PHENT`, leaving musl
   with incomplete program-header metadata for TLS discovery. The current
   working tree refreshes saved FS.base in `preempt_frame_to_scheduler` and
   publishes `AT_PHENT` via typed `ElfAuxInfo`.
6. **Latest validation:** `m3os-ion-tls-fix.log` reached the graphical ready
   markers, fork parser reported `spawned 19 trampolines 19 missing 0`, and
   the log contains zero `userspace page fault` lines and no `rip=0x65e54b`.
   The same bounded run did not hit `PTY EOF; shell closed`, but `term` stayed
   at `pty_bytes=0` before timeout; debug that only as a separate shell/PTY
   liveness issue if it reproduces.

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

### Current working-tree fixes

| File | Fix | Validation |
|---|---|---|
| `kernel/src/arch/x86_64/syscall/mod.rs` | Enable IF during syscall bodies after saving user state, then convert pending syscall-return reschedules into a full `PreemptFrame` handoff. | `m3os-slot-preempt-postdoc.log` includes syscall-return preemption traces and no missing fork trampolines. |
| `kernel/src/task/scheduler.rs` | Factor `preempt_frame_to_scheduler` so IRQ-return and syscall-return preemption share the same full-frame scheduler transition. | `cargo xtask check`; final GUI run reached graphical ready state. |
| `kernel/src/blk/virtio_blk.rs` | Replace `REQUEST_LOCK` held across `do_request()` with a scheduler-blocking single-flight request slot using static waiter flags. | Final GUI run: `spawned 19 trampolines 19 missing 0`. |
| `kernel/src/task/scheduler.rs` | Refresh the current task's saved FS.base before publishing a full preemption frame, so syscall-return preemption after `ARCH_SET_FS` does not restore stale TLS state. | `m3os-ion-tls-fix.log`: `/bin/ion` execs with no `rip=0x65e54b` page fault. |
| `kernel/src/mm/elf.rs` | Add typed `ElfAuxInfo` and publish `AT_PHENT` alongside `AT_PHDR`/`AT_PHNUM` in the initial aux vector. | Musl can walk program headers with the correct entry size during TLS setup. |

### Virtio-blk request-slot history

| Commit | Outcome | Notes |
|---|---|---|
| `ee73f3c` | Regressed; reverted | Replaced the legacy virtio-blk `REQUEST_LOCK: IrqSafeMutex<()>` with `REQUEST_IN_FLIGHT` plus a scheduler-blocking waiter list. Intended to avoid holding IRQ/preempt masking across `block_current_until()`, but user testing showed graphical boot failures appeared immediately and more frequently than baseline. |
| `711715d` | Restores baseline | Reverts `ee73f3c`. Keep this revert in history; do not re-apply the exact `ee73f3c` waiter/yield shape. |
| Current working tree | Validated | Reintroduces the request-slot idea with the correctness fixes that `ee73f3c` lacked: static waiter flags, per-sector slot scope, no stack wait-flag pointers, no yield loop, and wake-all release semantics. |

Observed after `ee73f3c`:

- `m3os-mouse-sticky.log`: mouse deltas appeared to move the cursor briefly,
  then the cursor snapped back toward the original position. The log had 19
  fork spawns and 18 trampoline entries; missing `pid=19 task_idx=24
  target_core=1`.
- `m3os-no-text.log`: graphical boot appeared mostly alive, but terminal /
  fallback echo did not print typed characters. The log had 19 fork spawns
  and 17 trampoline entries; missing `pid=6 task_idx=13 target_core=2` and
  `pid=19 task_idx=24 target_core=1`. `term` stayed alive but reported
  `events=0 composes=1 pty_bytes=0` for many iterations.
- `m3os-ds-pv.log`: all 19 fork spawns reached trampoline, but later logs
  showed `display_server: client protocol violation; dropping message` and
  `term: display verb ipc_call_buf failed: CommitSurface`. Mouse and key echo
  appeared to work in this run.

Conclusion from `ee73f3c`: the rough request-slot shape was not enough and
the implementation made timing worse. Conclusion from the current working
tree: virtio-blk serialization really was part of the stall, but only when
fixed as a scheduler-blocking gate that never parks while holding an
interrupt-masking spin lock. Keep the current no-yield shape.

Wake-one note: a local refinement that woke one waiter instead of all waiters
regressed the final fork-child check (`m3os-slot-preempt-wakeone.log`:
missing `pid=19 task_idx=24 target_core=2`). The current wake-all release is
intentional; losing waiters re-check `REQUEST_IN_FLIGHT` and re-park.

---

## Symptoms observed across many boots

The user has run ~30+ boots. The original symptoms were intermittent and did
not all co-occur. The final validation run shows the fork-dispatch stall is
fixed in the current working tree; the Ion crash remains open.

### S1 — Forked task never enters userspace (fixed in current tree)

`init: started 'X' pid=N` appears, kernel emits
`[INFO] [proc] fork: parent_pid=... child_pid=N`, `[exec-trace] fork-task-spawn
pid=N task_idx=I target_core=C` appears, but **no matching
`fork-child pid=N trampoline-enter` line.** The task is on the run queue
but is never dispatched.

This was the smoking gun for the graphical-boot stall. The final validation
log (`m3os-slot-preempt-final.log`) has `spawned 19 trampolines 19 missing 0`,
so this symptom is fixed by the syscall-return preemption plus virtio-blk
single-flight request-slot changes.

**Reproduced for various pids across boots:**
- `pid=7` (display_server) → m3os-no-display.log ⇒ no display the whole boot
- `pid=8` (mouse_server), `pid=15` (session_manager), `pid=17` (term),
  `pid=19` (term-fork-child / ion) → various logs ⇒ partial functionality
- `pid=18` (login) on the same boot DID dispatch. Difference: `pid=18`
  was assigned `target_core=0`; the stuck pids were `target_core=1` (most
  recent log) or whatever core was busy.

**Canonical failure log:** `m3os.log` (May 1 20:34) — pid=8 and pid=19
both stuck on `target_core=1`. pid=2 on core=1 went first and ran fine.
The corrected diagnosis is that the AP could be pinned either by syscall
work with IF masked until return, or by a block caller spinning with IF=0
behind a sleeping virtio-blk request holder.

### S2 — Symptoms downstream of S1 (resolved by S1 fix)

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
- **Mouse sticky / snap-back after `ee73f3c`:** `m3os-mouse-sticky.log`
  showed pointer movement that appeared to be applied transiently and then
  reset. This was observed only while the reverted virtio-blk request-slot
  attempt was present.
- **Display protocol violations after `ee73f3c`:** `m3os-ds-pv.log`
  showed `display_server: client protocol violation; dropping message`
  paired with `term: display verb ipc_call_buf failed: CommitSurface`.
  Because the same failed commit also reintroduced fork-trampoline gaps in
  other logs, treat this as downstream instability until proven otherwise.

### S3 — Ion/userspace page fault (fixed in current tree)

Older runs saw Ion crashes around `rip=0x4c1e10`, the same RIP that the H.3
acceptance gate claimed to fix via per-task `fxsave64`/`fxrstor64`.
Phase 57d's commit `142e420` added FPU save/restore in scheduler dispatch,
but the crash returned roughly 1-in-N boots.

The final fixed-stall run reached the graphical session and then hit:
`userspace page fault: pid=19 addr=0 rip=0x65e54b`, followed by `term: PTY
EOF; shell closed`. Because all fork children reached trampoline first, treat
this as a separate Ion/userspace debugging track, not as evidence that S1 is
still present.

Follow-up debugging mapped `rip=0x65e54b` to musl `__init_ssp`, where startup
reads `%fs:0` to find the thread pointer before storing the stack canary. The
root cause was stale FS.base restoration across syscall-return preemption after
`arch_prctl(ARCH_SET_FS)`, with a secondary ELF aux-vector correctness gap:
`AT_PHENT` was not published. `m3os-ion-tls-fix.log` validates the fix with no
userspace page faults.

### S4 — Held-key repeat noise (cosmetic)

`kbd_server: warn: held-key table overflow; oldest key dropped` appears
when 9+ keys are held. Defensive cap at 8; message text is now slightly
misleading after `673f400` (we only repeat the highest-age key, but the
table still tracks all held keys for de-dup). Cosmetic only.

---

## Where to investigate next

### Highest-value lead: shell/PTY liveness if it reproduces

The Ion null fault is no longer the highest-value lead. In
`m3os-ion-tls-fix.log`, graphical boot is complete, `/bin/ion` execs, and no
userspace page fault occurs:

```text
AUDIO_SMOKE:server:READY
TERM_SMOKE:ready
display_server: AttachSharedBuffer ok shm_id=1 va=0x20003e8000
session_manager: session.boot: state=running
[INFO] [exec-trace] pid=19 execve OK path="/bin/ion" entry=0x4101c2 rsp=...
```

If the shell still appears silent, start from term/PTY liveness rather than
from Ion fault mapping: the post-fix bounded run showed repeated
`term: iter=... pty_bytes=0` and `pid=17 ... BlockedOnReply` warnings before
timeout. Keep the fork-trampoline parser and `userspace page fault` grep only
as regression guards.

### If the fork-dispatch stall returns

Re-check the two fixed layers before broadening the search:

1. **Syscall-return preemption path** —
   `kernel/src/arch/x86_64/syscall/mod.rs` should re-enable IF only after
   saving the full user register set, disable IF again before the return
   tail touches user RSP, and convert a pending reschedule into
   `preempt_frame_to_scheduler` before restoring registers for `sysretq`.
   Do not replace this with `yield_now()` or userspace sleeps; the goal is a
   real preemption boundary.

2. **Virtio-blk request serialization** — `kernel/src/blk/virtio_blk.rs`
   must not hold `IrqSafeMutex` or any interrupt-masking spin lock across
   `do_request()` / `block_current_until()`. The single-flight slot may
   serialize descriptor/scratch/DMA ownership, but waiters have to park
   through the scheduler.

3. **Scheduler run-queue invariants** — `enqueue_to_core` should set the
   target core's `reschedule` flag and send `IPI_RESCHEDULE`, and
   `dequeue_local` should prefer fresh fork children because they run at
   priority 19 while normal daemons requeue at priority 20. If a future log
   again shows `fork-task-spawn` without `trampoline-enter`, inspect the
   target core's reschedule flag, IPI path, and `PENDING_REENQUEUE`
   epilogue.

4. **AP timer/IRQ pipeline** — verify that each AP reports online and has an
   armed LAPIC timer. Syscall IF handling and virtio-blk no longer mask IRQs
   across long blocking paths, so an AP that still fails to preempt should be
   treated as a timer/IPI routing problem.

### Concrete regression recipe

```bash
M3OS_KERNEL_FEATURES=exec-trace cargo xtask run-gui --fresh 2>&1 | tee m3os.log
```

The old failure reproduced in 3-10 boots. Failure pattern: search for
`fork-task-spawn pid=N task_idx=I target_core=C` in the log, then check
whether the matching `fork-child pid=N trampoline-enter` line exists. The
fixed run has all fork children entering trampoline; any future missing pid
is a regression.

### If a focused look at scheduler does not reveal a regression

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

- **SHM transport.** Term's SHM buffer mapping works end-to-end once the
  scheduler stall is out of the way. `m3os3.log` from May 1 19:42 showed
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
- **Yield-based virtio-blk wait variants.** A local-only variant that looped
  through `scheduler::yield_now()` was rejected because it moved the
  contention path back toward cooperative scheduling. The current
  scheduler-blocking single-flight slot is the correct shape; do not replace
  it with cooperative polling.

---

## Files of interest (with last-touched commits)

| File | Recent commits | Why look |
|---|---|---|
| `kernel/src/task/scheduler.rs` | current working tree, `73e5c1d`, `463030d`, `ad59f3a`, `142e420` | Shared `preempt_frame_to_scheduler`, scheduler dispatch / run-queue / FPU save logic |
| `kernel/src/process/mod.rs` | `463030d`, `ad59f3a`, `968b579` | `fork_child_trampoline`, `sys_fork`, FPU reset |
| `kernel/src/arch/x86_64/syscall/mod.rs` | current working tree, `0468d3f`, `968b579` | Syscall IF handling, syscall-return preemption, SHM syscalls, exec-trace logs |
| `kernel/src/blk/virtio_blk.rs` | current working tree | Scheduler-blocking single-flight request slot; must not park while holding IRQ-masking locks |
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
| `m3os-slot-preempt-postdoc.log` (May 1) | Latest post-edit validation: all 19 fork children reached trampoline, graphical stack reached ready markers, then separate Ion null fault at `rip=0x65e54b` |
| `m3os-ion-tls-fix.log` (May 2) | Latest Ion/TLS fix validation: all 19 fork children reached trampoline, graphical stack reached ready markers, `/bin/ion` exec succeeded, and no userspace page fault occurred; term stayed at `pty_bytes=0` before timeout |
| `m3os-slot-preempt-final.log` (May 1) | Earlier current-tree validation: all 19 fork children reached trampoline, graphical stack reached ready markers, then separate Ion null fault at `rip=0x65e54b` |
| `m3os-slot-preempt-wakeone.log` (May 1) | Wake-one experiment: graphical markers appeared, but fork parser showed missing `pid=19 task_idx=24 target_core=2`; reverted to wake-all |
| `m3os-slot-preempt.log` (May 1) | Earlier wake-all run: all 19 fork children reached trampoline and same later Ion fault appeared |
| `m3os-ds-pv.log` (May 1 22:20) | After failed `ee73f3c`: all fork children dispatched, but display protocol violations and `CommitSurface` IPC failures appeared |
| `m3os-no-text.log` (May 1 22:20) | After failed `ee73f3c`: login/TERM_SMOKE present, but no typed chars; pid=6 and pid=19 missing trampoline |
| `m3os-mouse-sticky.log` (May 1 22:20) | After failed `ee73f3c`: mouse cursor moved then snapped back; pid=19 missing trampoline |
| `m3os.log` (May 1 20:34) | Failure: pid=8 and pid=19 both stuck on `target_core=1` |
| `m3os-no-terminal.log` (May 1 20:04) | Failure: term + session_manager stuck, login (pid=18) ran |
| `m3os-no-display.log` (May 1 20:03) | Failure: display_server's pid=7 stuck, full text-fallback |
| `m3os3.log` (May 1 19:42) | Working boot: 8+ seconds of healthy compose |
| `m3os2.log` (May 1 19:41) | Partial: display worked, terminal partial |
| `m3os-fail-1.log` | Earlier failure pattern (May 1 17:26) |

The user has been clearing / overwriting old logs across iterations. Keep
the May 1 20:34 `m3os.log` as the canonical historical "smoking gun" because
it has both the new `fork-task-spawn` and `trampoline-enter` diagnostics
showing the exact stuck pids. Keep `m3os-slot-preempt-postdoc.log` as the
canonical fixed-stall validation.

---

## Engineering-discipline notes

- **Don't add more scheduler diagnostic logging by default.** The data was
  sufficient to root the fork-dispatch stall, and the final validation log
  covers the regression guard. Add targeted Ion/userspace diagnostics only if
  needed for S3.
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
  marks I.2 / I.3 / H.3 / H.4 as procedurally pending. With the
  fork-dispatch stall fixed, the next gate is deciding whether the remaining
  Ion fault blocks those acceptance items or belongs to a separate follow-up.
