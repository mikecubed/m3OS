# Phase 57d Graphical Boot — Debugging Handoff

**Status:** Root cause for the graphical-boot stall is fixed and validated by a
clean fork-trampoline pass. The follow-up Ion/userspace null page fault at
`rip=0x65e54b` is also fixed. The later shell-prompt investigation found and
fixed a VFS readiness race plus the last cooperative `waitpid` loop; the latest
work adds retryable display publishing plus a `term.prompt-ready` readiness gate
so non-critical boot daemons do not race the first graphical prompt with early
disk writes. The remaining open issue is the underlying virtio-blk write timeout
(`type=1`, sector 2072), which can still appear but no longer blocks the prompt
in the latest validation logs.

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
7. **Latest shell-liveness split:** `m3os-stat-trace.log` showed Ion entering
   `stat("/root/.local/share/ion/history")` and issuing VFS IPC while
   `vfs_server` had published the `vfs` service but had not yet entered
   `ipc_recv_msg`, because it wrote the `"registered, entering server loop"`
   banner to stdout after `ipc_register_service`. The working tree now sends
   that banner to serial instead, eliminating the registered-but-not-receiving
   window. `m3os-vfs-ready-fix.log` confirms VFS reaches the receive loop early
   and Ion gets as far as forking child pid 20.
8. **Prompt-helper classification:** `m3os-ion-parent-strace.log` shows the
   helper path is not stuck after the VFS fix: Ion forks pid 20, pid 20 execs
   `/bin/PROMPT`, Ion reaps it via `wait4`, reads 28 bytes from the helper pipe,
   writes 158 bytes to fd 1, then blocks in `read(0, ...)` waiting for input.
   The `pty_bytes=158` plateau is therefore a prompt-ready steady state unless
   a visual run proves the prompt is not rendered. Final clean validation
   `m3os-waitpid-reregister-final2.log` reached `AUDIO_SMOKE:server:READY`,
   `TERM_SMOKE:ready`, `session.boot: state=running`, early VFS readiness, Ion
   child pid 20 fork, and stable `pty_bytes=158`, with no userspace page fault.
   A follow-up 3-run fresh GUI retry (`m3os-repeat-{1,2,3}.log`) reproduced the
    same prompt-ready signature in all runs: VFS ready, session running,
    `/bin/ion` exec, `/bin/PROMPT` exec, stable `pty_bytes=158`, and no page
    fault / PTY EOF / panic. The display-side compose totals also advanced after
    `/bin/PROMPT` exec (`total=39` before the helper, then `total=59` or `79` by
    compose#600), so the prompt bytes are reaching the renderer/display path; if
    a human still sees a blank window, debug surface damage/composition next.
9. **Latest graphical-visibility / reply-wake split:** a user-visible blank
   terminal run showed `TERM_SMOKE:ready` before the first shared-buffer attach.
   `term` now forces an initial `renderer.compose()` before emitting the ready
   sentinel, so `display_server: AttachSharedBuffer ok` precedes readiness. A
   separate IPC reply lost-wake window was fixed by registering a per-task reply
   waker before parking in `BlockedOnReply`; replies now set that flag before
   `wake_task_v2`, closing the "reply arrived just before park" race without
   yielding. `term` no longer polls private `vfs` readiness; it blocks in
   `ipc_wait_service("vfs", 0)`, a readiness-only syscall that never grants a
   callable endpoint capability. Normal GUI validation
   (`m3os-no-exec-trace-fix.log`) reached `/bin/ion`, `AttachSharedBuffer ok`
   before `TERM_SMOKE:ready`, and stable `pty_bytes=158`; exec-trace builds can
   perturb this path enough to stall Ion startup and should not be treated as the
   prompt-readiness oracle by themselves.
10. **Blocking service-readiness primitive:** `ipc_wait_service` adds a
    scheduler-blocking service wait path (`BlockedOnService`) for userspace
    readiness dependencies. The wait path avoids `SERVICE_WAITERS`/`REGISTRY`
    lock overlap, and service registration defers wake delivery to the waiter's
    assigned-core scheduler loop so the registering task does not spin in
    cross-core `wake_task_v2` while the waiter is still publishing its blocked
    stack. `block_current_until` now disables preemption from the blocked-state
    write through the actual `switch_context`, closing the preemptive
    mark-blocked-but-not-switched window. `m3os-ipc-wait-service6.log` reached
    VFS registration, `/bin/ion`, `AttachSharedBuffer ok`, `TERM_SMOKE:ready`,
     and eventually stable `pty_bytes=158`; Ion still sometimes delays before the
     prompt helper fork and remains the next liveness target.
11. **Latest no-yield block/PTY work:** Follow-up runs showed two separate
    timing-dependent stalls: VFS could start but never register because a
    virtio-blk completion/wake was missed, and Ion could exec but sit behind an
    early disk owner while term stayed at `pty_bytes=0`. The current fix keeps
    the non-yield path: virtio-blk request and slot waits use bounded
    `block_current_until` deadlines to poll completions and re-notify the queue,
    used-ring wake delivery now happens after dropping the driver lock, and PTY
    master/slave blocking reads park on PTY wait queues instead of calling
    `yield_now()`. `term` also execs Ion with `-i` so the graphical PTY always
    requests an interactive prompt even if TTY detection races early boot. This
    improves the split but does **not** fully close the intermittent prompt miss:
    `m3os-final-block-pty-1.log` still reproduced VFS ready + `/bin/ion` exec +
    `TERM_SMOKE:ready` with `pty_bytes=0`. Its timeout diagnostics show an early
    active virtio write request (`type=1`, sector 2072, owner pid 3) before VFS
    registration; the next target is the legacy single-flight write request path
    or deferring boot-time write-heavy daemons so shell reads cannot sit behind a
    stuck log/host-key write.
12. **Prompt-readiness gating and display retry:** The latest failure set split
    into two classes. `m3os2.log` had `/bin/ion` but no `/bin/PROMPT`, with PTY
    bytes stuck at 0/8 behind a virtio write timeout (`owner_pid=2`, `type=1`,
    sector 4352). `m3os3.log` did reach `/bin/PROMPT` and `pty_bytes=158`, but
    a transient `CommitSurface` protocol failure dropped the publish, making it
    a display-visibility failure rather than an Ion-spawn failure. `term` now
    keeps failed display submits pending and retries them without replaying draw
    operations, then registers `term.prompt-ready` after enough PTY bytes prove
    the prompt path is alive. `syslogd` and `sshd` block on that service before
    boot-time persistent log / `/etc/ssh` setup, using the scheduler-blocking
    service wait primitive rather than yield loops. `m3os-prompt-ready-fix2.log`
    and `m3os-prompt-ready-fix3.log` both reached `/bin/PROMPT`,
    `TERM_SMOKE:prompt-ready`, syslogd/sshd gate-open logs, and stable
    `pty_bytes=158`. Both still showed one recoverable virtio timeout for
    `owner_pid=19 type=1 sector=2072`, so the timeout root cause is not closed.
13. **Latest visual terminal validation:** User testing now confirms the
    graphical prompt appears reliably. The current graphical boot path still
    bypasses the text `login` interaction and drops directly into Ion as root;
    treat that as a session/login policy issue, not as a prompt-readiness
    regression. The terminal renderer had a confirmed stale-cell bug:
    `ConsoleCmd::Backspace` only moved the cursor, and `EraseLine` (`ESC[K`)
    was ignored, so deleted characters and shell line-redraw clears were not
    painted over. The working tree now repaints blanks for backspace and
    erase-line modes 0/1/2 with host tests. The same interactive log still
    shows burst-time `CommitSurface` protocol violations after PTY output grows
    into the tens of KiB; keep that as a separate display-protocol lead if
    visual glitches remain after the stale-cell fix.
14. **Latest interactive `ls` / no-prompt liveness split:** `m3os2.log` showed first `ls`
    completing and Ion forking the next `/bin/PROMPT`, then a second `ls`
    reaching `execve OK` but never logging its final `close fd=3`. The matching
    IPC root cause was that `vfs_server` still replied through hardcoded cap
    slot `1` even though `recv_msg` now publishes the actual reply cap in
    `msg.data[3]`. If slot `1` was not the current caller's reply cap, the VFS
    reply failed and the `ls` caller remained blocked in the directory IPC.
    The follow-up no-prompt `m3os.log` exposed the matching kernel-side gap:
    when `call_msg` delivered directly to an already-waiting server, it inserted
    a reply cap but did not encode that handle into `data[3]`, so
    `vfs_server` logged `request missing reply cap` and pid 19 stayed
     `BlockedOnReply`. `kernel-core::ipc::Message::with_reply_cap_handle()` now
     centralizes the encoding and `call_msg`, `recv_msg`, `recv_msg_nowait`, and
     `recv_msg_with_notif` all use it before delivery.
15. **Latest prompt-success log-spam split:** the newest user GUI log reaches
    `/bin/ion`, `/bin/PROMPT`, `TERM_SMOKE:prompt-ready`, and rising
    `pty_bytes`, so the reply-cap/no-prompt bug is not reproducing there. The
    remaining spam is now bounded and classified: display-server fatal protocol
    replies carry a `reason`, `label`, `bulk_len`, and decoded frame
    `body_len/opcode` for the first few occurrences, term display-verb failure
    logs are rate-limited, and the scheduler's stale run-queue
    `dequeue-drop` diagnostic budget is reduced. If `CommitSurface` failures
    recur, use the new `display_server: client protocol violation reason=...`
    line as the next root-cause discriminator.
16. **Latest bottom-row terminal rendering target:** user testing now confirms a
    successful boot with prompt and command execution, but the current line at
    the bottom of the terminal can visually show only its first glyph until the
    next scroll replays queued glyph operations. The working tree adds a
    host-tested compose policy that keeps the normal frame throttle away from
    the bottom row, but immediately publishes damaged PTY output once the cursor
    is on the last terminal row. This remains preemption-compatible: no
    cooperative yield/poll loop was added.

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
| `userspace/vfs_server/src/main.rs` | Avoid stdout writes after `ipc_register_service("vfs")`; the readiness banner now goes to serial so clients cannot observe `vfs` registered while the server is blocked on terminal output before its first `ipc_recv_msg`. | `m3os-vfs-ready-fix.log`: `vfs_server: registered, entering server loop` appears at line 708 and Ion progresses beyond startup to `parent_pid=19 ... child_pid=20`. |
| `kernel/src/process/mod.rs`, `kernel/src/arch/x86_64/syscall/mod.rs`, `kernel/src/task/{mod.rs,scheduler.rs}` | Replace `waitpid`'s cooperative `yield_now()` polling with a child-exit wait queue and `BlockedOnWait` scheduler state. Child exit wakes parents from `send_sigchld_to_parent`. | `cargo xtask check`; `m3os-ion-parent-strace.log` shows Ion reaps `/bin/PROMPT`, writes the prompt, and blocks waiting for input. `m3os-waitpid-reregister-final2.log` confirms the clean build reaches the same `pty_bytes=158` steady state after the missed-wakeup review fix; `m3os-repeat-{1,2,3}.log` repeated that signature 3/3 times. |
| `userspace/term/src/main.rs` | Force the initial clear-frame compose before `TERM_SMOKE:ready`, and block on `ipc_wait_service("vfs", 0)` before spawning Ion. | `m3os-ipc-wait-service6.log`: `display_server: AttachSharedBuffer ok` precedes `TERM_SMOKE:ready`; `/bin/ion` launches and term stabilizes at `pty_bytes=158`. |
| `kernel/src/task/{mod.rs,scheduler.rs}` | Register a reply wait flag on the task before `BlockedOnReply` parking; `deliver_message` / `try_deliver_message` set it before `wake_task_v2` to close the reply-before-park lost-wake race. | `cargo xtask check`; the latest normal GUI run did not strand term in `BlockedOnReply`. |
| `kernel/src/ipc/mod.rs` | Let `ipc_service_exists` report private-service presence while keeping `ipc_lookup_service` denied, so clients can wait for readiness without receiving a callable capability. | Term can gate Ion startup on `vfs` readiness without exposing the private VFS endpoint. |
| `kernel/src/ipc/{mod.rs,registry.rs}`, `userspace/syscall-lib/src/lib.rs`, `kernel/src/task/{mod.rs,scheduler.rs}` | Add `ipc_wait_service` (`0x1115`) and `BlockedOnService`: service waiters park in the scheduler; registration marks matching waiters ready and defers wake delivery to the waiter's assigned-core scheduler loop. | `cargo xtask check`; `m3os-ipc-wait-service6.log` shows VFS registration completing while term is blocked on readiness, then term wakes and launches Ion. |
| `kernel/src/task/scheduler.rs` | Make the `block_current_until` park transition non-preemptible from the blocked-state write until `switch_context` returns on wake, preventing `on_cpu=true` from being left visible without a saved blocked stack. | `cargo xtask check`; GUI validation no longer hangs VFS inside immediate service registration wake when the deferred service-wake path is active. |
| `kernel/src/blk/virtio_blk.rs` | Add bounded no-yield timeout recovery for the single-flight virtio-blk path: request/slot waiters stay parked, timeout paths drain the used ring, re-notify the queue, and deliver wakes after dropping the driver lock so `wake_task_v2` cannot spin while holding the virtio lock. Timeout logs include owner PID, request type, sector, and whether a completion was drained. | `cargo xtask check`; `m3os-blk-type-probe.log` shows VFS registration, `/bin/ion`, `TERM_SMOKE:ready`, and stable `pty_bytes=158` after timeout recovery. `m3os-final-block-pty-1.log` still reproduced a later prompt miss behind an early stuck write owner, so this is not yet a full closure. |
| `kernel/src/arch/x86_64/syscall/mod.rs` | Replace PTY master/slave blocking-read `yield_now()` loops with PTY wait-queue registration plus `block_current_until`. | `cargo xtask check`; Ion reaches the normal `BlockedOnRecv` steady state after writing prompt bytes. |
| `userspace/term/src/{lib.rs,syscall_pty.rs}` | Pass `-i` when graphical term execs `/bin/ion`, with a host test for the argv shape. | `cargo test -p term --target x86_64-unknown-linux-gnu ion_argv_forces_interactive_mode --quiet`. |
| `userspace/term/src/{render.rs,display.rs}` | Make display publish retryable: `FramebufferOwner::submit()` reports success/failure, and `Renderer` keeps a failed submit pending so a transient `CommitSurface` failure cannot drop the prompt frame. | `cargo test -p term --target x86_64-unknown-linux-gnu failed_submit_keeps_frame_pending_without_replaying_ops --quiet`. |
| `userspace/term/src/{lib.rs,main.rs}` | Register `term.prompt-ready` after PTY output reaches the prompt-readiness threshold. | `m3os-prompt-ready-fix2.log` and `m3os-prompt-ready-fix3.log` reached `TERM_SMOKE:prompt-ready` and stable `pty_bytes=158`. |
| `userspace/syslogd/src/main.rs` | Bind `/dev/log`, then wait for `term.prompt-ready` before creating/opening persistent log files; replace zero-duration cooperative backpressure sleeps with timed parking. | `cargo xtask check`; GUI logs show `syslogd: prompt-ready gate open` only after the prompt marker. |
| `userspace/sshd/src/main.rs` | Wait for `term.prompt-ready` before `/etc/ssh` setup and listener startup, so SSH directory writes do not race the local graphical prompt. | `cargo test -p sshd --target x86_64-unknown-linux-gnu --quiet`; GUI logs show `sshd: prompt-ready gate open` only after the prompt marker. |
| `userspace/term/src/screen.rs` | Repaint blank cells for backspace and ANSI erase-line modes so shell editing/redraw clears stale glyphs on the graphical terminal. | `cargo test -p term --target x86_64-unknown-linux-gnu --quiet`: 68 tests pass. |
| `userspace/syscall-lib/src/lib.rs`, `userspace/vfs_server/src/main.rs` | Use the reply capability handle delivered in `IpcMessage::data[3]` instead of hardcoding cap slot `1`, so VFS replies cannot miss the caller when the cap table layout changes. | `cargo test -p syscall-lib --target x86_64-unknown-linux-gnu ipc_message_reply_cap --quiet`; `cargo check -p vfs_server --target x86_64-unknown-linux-gnu --quiet`. |
| `kernel-core/src/ipc/message.rs`, `kernel/src/ipc/endpoint.rs` | Encode the inserted reply-cap handle into `Message::data[3]` for direct `call_msg` delivery as well as queued receive paths. | `cargo test -p kernel-core --target x86_64-unknown-linux-gnu with_reply_cap_handle_sets_data3 --quiet`; `cargo check -p kernel --quiet`. |

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

### S5 — Prompt-ready regressions after the first liveness fixes (mitigated)

The post-waitpid/VFS failures had two different signatures:

- **Ion/PTY liveness:** `/bin/ion` execs but `/bin/PROMPT` never execs or PTY
  bytes remain near zero. This correlated with early virtio write timeouts from
  non-critical daemons (`syslogd` and then `sshd`) before the local shell prompt
  had proven ready.
- **Display publish loss:** `/bin/PROMPT` execs and PTY bytes reach the normal
  158-byte prompt plateau, but `CommitSurface` fails once and the old renderer
  had already drained the frame operations, so no later compose retried the
  publish.

The mitigation keeps to the no-yield policy: term publishes a scheduler-visible
`term.prompt-ready` service only after PTY output proves the prompt path is
alive, syslogd/sshd block on that service before write-heavy setup, and the
renderer retains failed display submits for retry. This improves startup
ordering but does not eliminate the lower-level virtio timeout itself.

### S6 — Terminal stale glyphs and burst-time commit failures (partially fixed)

The first successful user-visible prompt run exposed terminal-rendering bugs
instead of prompt-liveness bugs:

- Backspace moved the cursor but did not repaint the erased cell, so deleted
  characters stayed visible until later text overwrote them.
- ANSI `EraseLine` (`ESC[K`, `ESC[1K`, `ESC[2K`) was ignored, so shell
  prompt redraws and line editing left stale glyphs behind.
- After sustained interactive output, `m3os.log` still shows repeated
  `display_server: client protocol violation; dropping message` paired with
  `term: display verb ipc_call_buf failed: CommitSurface`. The screen fix does
  not claim to root-cause that protocol violation; it only fixes the stale-cell
  redraw path.

The current working tree fixes the first two bullets with host tests. If the
"only first char on the last line" symptom survives, prioritize the burst-time
`CommitSurface` protocol failure next.

---

## Where to investigate next

### Highest-value lead: burst-time display commit protocol failures

The latest interactive `m3os.log` reaches prompt readiness and handles user
input, but once PTY output grows under sustained typing/commands it logs many:

```text
display_server: client protocol violation; dropping message
term: display verb ipc_call_buf failed: CommitSurface
```

This is no longer an Ion/VFS/waitpid issue. The next concrete target is to log
the exact `ProtocolError`/bulk length/opcode in `display_server::client` and
determine why `DamageSurface` succeeds while the following small
`CommitSurface` frame is decoded as fatal under output bursts.

### Secondary lead: remaining virtio write timeout

The current prompt-ready mitigation intentionally avoids making early boot
write-heavy daemons compete with the first shell prompt, but the underlying
single-flight virtio-blk timeout still appears. In the latest successful
prompt-ready validations (`m3os-prompt-ready-fix2.log` and
`m3os-prompt-ready-fix3.log`), it appeared as:

```text
[virtio-blk] completion poll + queue notify after request timeout owner_pid=19 type=1 sector=2072 completed=false
```

Because the prompt still reached `/bin/PROMPT`, `TERM_SMOKE:prompt-ready`, and
stable `pty_bytes=158`, treat this as the next root-cause target rather than as
a current graphical boot blocker. Good next questions: which Ion filesystem
operation writes sector 2072, why the completion is not observed before the
deadline, and whether the timeout recovery path is missing a final wake or
whether QEMU/virtqueue notification timing is simply delayed.

### Tertiary lead: visual prompt confirmation

The Ion null fault and the VFS registered-but-not-receiving window are no longer
the highest-value leads. In `m3os-vfs-ready-fix.log`, graphical boot is
complete, `vfs_server` is receiving early, `/bin/ion` starts, and Ion forks its
first helper:

```text
AUDIO_SMOKE:server:READY
TERM_SMOKE:ready
session_manager: session.boot: state=running
[INFO] [userspace] vfs_server: registered, entering server loop
[INFO] [proc] fork: parent_pid=19 parent_exec=/bin/ion child_pid=20
term: iter=3000 events=0 composes=2 pty_bytes=158
```

`m3os-ion-parent-strace.log` classifies the `pty_bytes=158` plateau:

```text
pid=19 fork() -> child pid 20
pid=20 execve path="/bin/PROMPT"
pid=19 wait4(..., WUNTRACED) -> 20
pid=19 read(fd=3, ...) -> 28
pid=19 write(fd=1, ..., 158) -> 158
pid=19 read(fd=0, ...)   # blocks waiting for input
term: ... pty_bytes=158
```

That is the expected prompt-ready state from the kernel/PTY perspective. If a
future GUI run still looks blank, debug display rendering or visual surface
damage next, not Ion exec/TLS, VFS readiness, waitpid, or the original
missing-trampoline stall. The renderer now retries failed submits, so if
`CommitSurface` protocol violations continue, inspect the display server client
state machine and surface-generation expectations. Keep the fork-trampoline
parser and `userspace page fault` grep only as regression guards.

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
| `userspace/term/src/main.rs` | current working tree, `73e5c1d`, `73c25b6`, `d98d136`, `7d27a3a` | Main loop with prompt-ready marker and diagnostics |
| `userspace/term/src/render.rs` | current working tree | Renderer pending-submit retry for transient display publish failures |
| `userspace/term/src/display.rs` | current working tree, `7f6f6c4` | SHM-backed `DisplayClient`; submit success/failure reporting |
| `userspace/syslogd/src/main.rs` | current working tree | Prompt-ready gate before persistent log file setup |
| `userspace/sshd/src/main.rs` | current working tree | Prompt-ready gate before `/etc/ssh` setup and listener startup |
| `userspace/display_server/src/main.rs` | `cbcdeb3`, `0decf62`, `c745d45` | Compose loop diagnostic counters |
| `userspace/display_server/src/surface.rs` | `0468d3f`, `f88aa80`, `7f6f6c4` | `BufferStorage::Shared`, `AttachSharedBuffer` handler |
| `kernel-core/src/display/protocol.rs` | `7f6f6c4` | `ClientMessage::AttachSharedBuffer` |
| `kernel-core/src/input/keymap.rs` | `673f400` | KeyRepeatScheduler, only-newest-key behaviour |

---

## Logs in the workspace root (most recent first)

| File | Description |
|---|---|
| `m3os-prompt-ready-fix3.log` (May 2) | Latest bounded GUI validation after prompt-ready gating: `/bin/PROMPT`, `TERM_SMOKE:prompt-ready`, syslogd/sshd gate-open logs, stable `pty_bytes=158`; one recoverable Ion-owned virtio write timeout remains |
| `m3os-prompt-ready-fix2.log` (May 2) | First successful GUI validation after adding the sshd prompt-ready gate; same prompt-ready signature as fix3 |
| `m3os-prompt-ready-fix.log` (May 2) | Failed syslog-only gate experiment: stuck write owner moved to pid 3 (`sshd`), refuting syslogd as the sole boot-write cause |
| `m3os-slot-preempt-postdoc.log` (May 1) | Latest post-edit validation: all 19 fork children reached trampoline, graphical stack reached ready markers, then separate Ion null fault at `rip=0x65e54b` |
| `m3os-ion-tls-fix.log` (May 2) | Latest Ion/TLS fix validation: all 19 fork children reached trampoline, graphical stack reached ready markers, `/bin/ion` exec succeeded, and no userspace page fault occurred; term stayed at `pty_bytes=0` before timeout |
| `m3os-slot-preempt-final.log` (May 1) | Earlier current-tree validation: all 19 fork children reached trampoline, graphical stack reached ready markers, then separate Ion null fault at `rip=0x65e54b` |
| `m3os-slot-preempt-wakeone.log` (May 1) | Wake-one experiment: graphical markers appeared, but fork parser showed missing `pid=19 task_idx=24 target_core=2`; reverted to wake-all |
| `m3os-slot-preempt.log` (May 1) | Earlier wake-all run: all 19 fork children reached trampoline and same later Ion fault appeared |
| `m3os-ds-pv.log` (May 1 22:20) | After failed `ee73f3c`: all fork children dispatched, but display protocol violations and `CommitSurface` IPC failures appeared |
| `m3os-no-text.log` (May 1 22:20) | After failed `ee73f3c`: login/TERM_SMOKE present, but no typed chars; pid=6 and pid=19 missing trampoline |
| `m3os-mouse-sticky.log` (May 1 22:20) | After failed `ee73f3c`: mouse cursor moved then snapped back; pid=19 missing trampoline |
| `m3os.log` (May 2, overwritten by user) | Prompt failure class: partial PTY bytes plus display `CommitSurface` failures; compare with `m3os-prompt.log` |
| `m3os2.log` (May 2, overwritten by user) | Prompt failure class: `/bin/ion` without `/bin/PROMPT`, PTY stuck near zero behind virtio write timeout owner pid 2 |
| `m3os3.log` (May 2, overwritten by user) | Display publish class: `/bin/PROMPT` and `pty_bytes=158` reached, but display protocol violation dropped a commit |
| historical `m3os.log` (May 1 20:34) | Failure: pid=8 and pid=19 both stuck on `target_core=1` |
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
- **Diagnostic instrumentation is intentionally retained behind feature
  flags.** The kernel-side `[exec-trace]` log lines (fork-task-spawn,
  trampoline-enter, syscall-return-preempt, dup2/execve/close) are
  gated on `#[cfg(feature = "exec-trace")]` so default builds compile
  them out entirely. The `[TRACE] [sched]` ring is gated on
  `sched-trace` the same way. Both are documented in
  [`README.md` § Debugging](../../README.md#debugging) along with the
  full emit list and the `M3OS_KERNEL_FEATURES=...` toggle path.
  Userspace bring-up sentinels (`TERM_SMOKE:ready`,
  `TERM_SMOKE:prompt-ready`, `AUDIO_SMOKE:server:READY`,
  `session_manager: session.boot: state=...`), the term per-1000-iter
  liveness counter, the `display_server: compose#N` stats, the
  `display_server: AttachSharedBuffer ok|fail` outcome, and the
  `[virtio-blk] completion poll + queue notify after request timeout
  ...` recovery line are unconditional — they are the canonical
  always-on operational signals and are also documented in the
  README's Debugging section. Do not delete or further-gate any of
  these without updating the README and the relevant `Cargo.toml`
  comments at the same time.
- **The historical "remove Phase 57d-followup diagnostic
  instrumentation" cleanup is deferred.** Earlier revisions of this
  handoff called for a one-shot revert commit. That cleanup is
  postponed: with the boot stable but the burst-time `CommitSurface`
  protocol-violation lead and the sector-2072 virtio write timeout
  still open, the ability to flip `exec-trace` back on without a
  scaffolding rebuild has measurable value. Revisit the cleanup once
  both leads are root-caused. The default-build cost is already zero
  (everything is `cfg`-gated) so retention has no production impact.
- **The SHM rebuild is real work** that should not be rolled back. It
  unlocks zero-copy pixel transport and the kernel SHM module is
  reusable beyond display.
- **The Phase 57d roadmap doc** (`docs/roadmap/tasks/57d-voluntary-preemption-tasks.md`)
  marks I.2 / I.3 / H.3 / H.4 as procedurally pending. With the
  fork-dispatch stall fixed, the next gate is deciding whether the remaining
  Ion fault blocks those acceptance items or belongs to a separate follow-up.
