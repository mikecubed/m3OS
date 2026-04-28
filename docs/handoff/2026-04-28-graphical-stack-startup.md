---
status: open
branch: feat/phase-57-impl
last-known-good-commit: 3729a69
date: 2026-04-28
component: scheduler / display_server / serial-stdin / session_manager
related: docs/handoffs/2026-04-25-scheduler-design-comparison.md
---

# Handoff — Phase 57 graphical-stack startup regression

## ⚠ Read this companion doc first

**`docs/handoffs/2026-04-25-scheduler-design-comparison.md`** describes
a lost-wake bug class in m3OS's `block_current_unless_woken*` machinery
that almost certainly **is the same root cause** as the cursor-stuck-
at-(0, 0) symptom this doc catalogs. Specifically, that doc's signature:

> A task ends up in some Blocked-or-equivalent state that the scheduler
> never wakes, with no fault, no signal, no panic, no `cpu-hog`, no
> `stale-ready` warning.

…matches exactly what we see when `display_server` lands on an AP and
its `MouseInputSource::poll_pointer` calls `ipc_call(mouse_handle,
MOUSE_EVENT_PULL, 0)`. `ipc_call` is blocking → display_server enters
`BlockedOnReply` → if mouse_server's reply hits the `switching_out` /
`wake_after_switch` race window, display_server is stuck in
`BlockedOnReply` forever. The teal background + cursor at (0, 0)
visible to the user is consistent with exactly one compose pass having
completed (initial fill + cursor sprite at the initial `(0, 0)`)
before the next-iteration `poll_mouse` hung.

The eight remediation attempts catalogued in this doc were all
adjusting the *placement* that exposes the lost-wake. They couldn't
work — the underlying primitive itself is broken in a way that
manifests only when display_server is on a not-BSP core under
sufficient cross-core IPC pressure. **Fixing the lost-wake primitive
(per the recommendation in the 2026-04-25 doc) is almost certainly
the correct path forward**, and the 8 placement-tweak attempts in
this doc are essentially wasted motion that should not be repeated.

The 2026-04-25 doc proposed a "single state word + condition recheck
after state write" rewrite (Linux `try_to_wake_up` pattern, single
state CAS, no `switching_out` / `wake_after_switch` flags). That is
a substantial dedicated phase of work, but it is the smallest
viable fix that closes this entire bug class.

A smaller intermediate also suggested in the 2026-04-25 doc — a
**per-task spinlock** around the block/wake transition mirroring
Linux's `p->pi_lock` — would give most of the benefit without the
global rewrite. Worth considering as the next concrete step before
committing to a full state-machine rewrite.

## TL;DR

`feat/phase-57-impl` boots through to a `m3OS login:` prompt and the
serial console works, but the **framebuffer graphical session is
partially broken** in two flavours depending on which fork-task
placement the load balancer happens to pick:

- **At `3729a69` (current branch HEAD, last known "mostly working"
  baseline):** mouse cursor moves, term's surface eventually shows up,
  but **`kbd_server` is dead on core 3** because `serial-stdin` parks
  there (`enable_and_hlt` halt-loop, never yields). Symptom: framebuffer
  terminal is visible but you can't type into it.
- **After the parks_scheduler / term-retry / display-yield commits
  (since-reverted, see "Commit timeline" below):** kbd works on a live
  core, but the framebuffer ends up **teal background + cursor stuck at
  (0, 0) + no terminal surface visible**. Mouse motion is not delivered;
  term reaches `term: spawned` and never reaches `TERM_SMOKE:ready`.

The branch has been **force-reset to `3729a69`** so the user is back on
the "kbd dead, everything else works" baseline. The remote
(`origin/feat/phase-57-impl`) reflects that.

## What the user actually sees

Test rig: real hardware (the user's "test machine"), `cargo xtask
run-gui --fresh`. QEMU on host is not used for the framebuffer
checks — headless tests cannot reproduce the visual symptoms.

| Commit                       | Mouse moves | Terminal visible | Can type | Login on serial |
|------------------------------|-------------|------------------|----------|-----------------|
| `3729a69` (baseline, HEAD)   | yes         | yes              | **no**   | yes             |
| `dd570bb` (pin serial-stdin) | no          | no               | n/a      | yes             |
| `5306f93` (yield in feeder)  | no          | no               | n/a      | yes             |
| `e1eb5e7` (parks_scheduler)  | **no**      | **no**           | n/a      | yes             |
| `3c65bb3` (display yield)    | no          | no               | n/a      | yes             |

The user explicitly described the broken framebuffer as: **"teal with
a mouse cursor in top left corner. Mouse cursor doesn't respond to
mouse movement."**

Critical observation: the cursor is at **(0, 0)** — the initial
`pointer_position` value. That means **zero** `PointerEvent`s have
been delivered to display_server, not just delayed ones. Even one
event would have moved the cursor from origin. The mouse_server →
display_server IPC path is failing *binary*, not gradually.

## Architecture quick-ref

The Phase 57 graphical stack:

```
                            ┌─────────────────┐
                            │ kernel PS/2 IRQ │   IRQ 1 (kbd)
                            │ + scancode ring │   IRQ 12 (mouse)
                            └────────┬────────┘
                                     │ syscall_lib::read_*_packet
        ┌──────────────────┐         │
        │   kbd_server     │◄────────┘
        │   mouse_server   │
        └────────┬─────────┘
                 │ ipc_call(MOUSE_EVENT_PULL / KBD_EVENT_PULL)
                 ▼
        ┌──────────────────┐         ┌──────────────────┐
        │  display_server  │◄────────│       term       │
        │  (compositor)    │  ipc_call│  (graphical TTY) │
        │  owns FB         │  Hello,  └──────────────────┘
        │                  │  CreateSurface,                ▲
        │  poll_pointer ───┘  PixelsChunk                   │
        │  poll_key   ─────┐                                │ PTY
        │                  │  ServerMessage::Pointer/Key    │
        │  outbound queue ─┴─►per-client event queue       │
        │                     (term drains via              │
        │                      LABEL_CLIENT_EVENT_PULL)     │
        └──────────────────┘                                │
                 ▲                                          │
                 │                                  ┌───────┴───────┐
                 │ frame_tick (60 Hz, BSP-only)     │      ion       │
                 │                                  │  (term's child)│
        ┌────────┴─────────┐                        └────────────────┘
        │  session_manager │  boot ordering: display, kbd, mouse,
        │  (text-fallback) │  audio, term — each w/ 5s × 3 retry
        └──────────────────┘  (currently fires text-fallback always
                               because audio_server exits cleanly with
                               no AC97 — but the F.4 stop() calls are
                               LOGGING-ONLY stubs, no actual teardown)
```

Service layout that init creates (boot order, all forked from PID 1):

| pid | name              | binary                  | responsible for           |
|-----|-------------------|-------------------------|---------------------------|
| 1   | userspace-init    | /sbin/init              | service supervisor        |
| 2   | syslogd           | /bin/syslogd            | log collector             |
| 3   | sshd              | /bin/sshd               | SSH daemon                |
| 4   | crond             | /bin/crond              | cron                      |
| 5   | console_server    | /bin/console_server     | text console IPC          |
| 6   | kbd_server        | /bin/kbd_server         | keyboard ring → IPC       |
| 7   | display_server    | /bin/display_server     | compositor + FB owner     |
| 8   | mouse_server      | /bin/mouse_server       | PS/2 mouse → IPC          |
| 9   | stdin_feeder      | /bin/stdin_feeder       | kbd events → stdin buffer |
| 10  | fat_server        | /bin/fat_server         | FAT32 server              |
| 11  | vfs_server        | /bin/vfs_server         | VFS server                |
| 12  | net_udp           | /bin/net_server         | UDP networking            |
| 13  | nvme_driver       | /drivers/nvme           | NVMe (no HW → exits)      |
| 14  | e1000_driver      | /drivers/e1000          | e1000 (no HW → exits)     |
| 15  | session_manager   | /bin/session_manager    | boot ordering for GUI     |
| 16  | audio_server      | /bin/audio_server       | AC'97 (no HW → exits)     |
| 17  | term              | /bin/term               | graphical terminal        |
| 18  | (login or sup.)   | /bin/login              | serial login              |
| 19  | ion (term child)  | /bin/ion                | shell inside term         |

All "fork-task-spawn" calls use `least_loaded_core` for placement. Order
of service spawns matters for placement — see "Why core 3 is special"
below.

## Symptoms in detail

### At `3729a69` baseline (current HEAD)

Boot completes. From the user's run-gui log on real hardware:

```
init: starting 'kbd'
[INFO] [sched] fork-task-spawn pid=6 task_idx=13 target_core=3 ...
init: started 'kbd' pid=6
                                     ← kbd_server NEVER reaches its main loop
                                     ← no "kbd_server: ready" log
                                     ← no `display_server: kbd service connected`
init: started 'fat' pid=10           target_core=3   ← also dead
init: started 'e1000_driver' pid=14  target_core=3   ← never runs
display_server: starts on core 0 (BSP) — works, composes
mouse_server: starts on core 1 — works, replies with PointerEvents
term: starts on core 2 — works, reaches TERM_SMOKE:ready, draws to FB
```

User-visible:
- **Framebuffer terminal IS visible** (term's surface composited)
- **Mouse cursor IS visible AND MOVES** when user moves the mouse
- **Keyboard input does not work** — anything typed into the framebuffer
  terminal is lost (kbd_server is in core 3's run queue forever, never
  dispatched, so display_server's `KbdInputSource::poll_key` never gets
  a `KBD_EVENT_PULL` reply)
- Serial console login WORKS (user can `ssh` in or use the
  `getty`-equivalent on COM1)
- `session_manager` declares `text-fallback` because `audio_server`
  exits cleanly (no AC'97 hardware) — but the `stop(...)` calls in
  `recover.rs::run_text_fallback_rollback` are F.4 logging-only stubs.
  Display_server / kbd_server / etc. are NOT actually killed. This is
  a red herring for the visible symptoms.

### After the recent commits (now reverted)

With `parks_scheduler` reserving core 3 for serial-stdin, the placement
re-shuffles:

```
[INFO] fork-task-spawn pid=6  task_idx=13 target_core=0  ← kbd works!
[INFO] fork-task-spawn pid=7  task_idx=14 target_core=1  ← display moved off BSP
[INFO] fork-task-spawn pid=8  task_idx=15 target_core=2  ← mouse moved off core 1
[INFO] fork-task-spawn pid=10 task_idx=17 target_core=1  ← fat works
[INFO] fork-task-spawn pid=17 task_idx=24 target_core=2  ← term on core 2
```

User-visible (real hw):
- Mouse cursor visible at **(0, 0)**, never moves
- Framebuffer is teal (display_server's `BG_PIXEL = 0x002B_5A4B`) — so
  display_server IS composing
- term reaches `term: spawned` but not `TERM_SMOKE:ready`
- No terminal surface composited

This is the bug we couldn't crack. Three plausible causes (none yet
verified):

1. **display_server on AP doesn't get frame_tick subdivided properly**
   — `kernel/src/time/mod.rs::on_timer_tick_isr` is only called from
   the BSP timer ISR (`kernel/src/arch/x86_64/interrupts.rs:807`). All
   cores increment a global atomic, so reading it from an AP works,
   but the *production rate* depends on the BSP. If BSP is heavily
   contested, frame_ticks stop accumulating, compose pauses, cursor
   doesn't update.
2. **mouse_server's ipc_call to deliver PointerEvent is timing out
   /returning MOUSE_EVENT_NONE**, perhaps because mouse_server is
   starved on core 2 by `sshd`'s ~480 ms cpu-hog, so the PS/2 ring
   overflows and packets are dropped before `read_mouse_packet`
   consumes them.
3. **A real bug in display_server's `MouseInputSource::poll_pointer`
   lazy-reconnect** when display_server registers BEFORE mouse_server
   has registered as `"mouse"`. The first `lookup_with_backoff` call
   in `MouseInputSource::lookup_with_backoff` may permanently set
   `handle = None`, and the lazy reconnect path may not be retrying
   correctly. See `userspace/display_server/src/input.rs:188-224` for
   the lazy reconnect logic — investigate whether
   `display_server: mouse service connected (lazy)` actually fires in
   the broken case.

The user's "cursor at (0, 0), zero motion" symptom is best explained
by hypothesis 3 (binary failure) rather than 1 or 2 (which would yield
*delayed* but non-zero motion).

## Why core 3 is special

Service spawn order in `userspace/init/src/main.rs` and the
`least_loaded_core` algorithm in
`kernel/src/task/scheduler.rs:589-617` interact such that
`serial-stdin` (a kernel-spawned task created in `init_task` via
`task::spawn`) consistently lands on core 3:

1. BSP starts, runs `init_task` (kernel-side, before userspace init).
2. `init_task` calls `task::spawn(console_server_task, ...)` — least
   loaded is AP1 (core 1). console queued there.
3. `task::spawn(net_task, ...)` — AP2 picks (core 2).
4. `task::spawn(serial_stdin_feeder_task, ...)` — AP3 picks (core 3).
5. `task::spawn_userspace_init()` — fork to least loaded. Core 0 has
   only init currently running (queue empty), other cores each have one.
   Core 0 wins. Userspace init lands on core 0.

Then `serial_stdin_feeder_task` (defined in `kernel/src/main.rs:486`)
does this in a loop — note it is **kernel code**, not userspace:

```rust
loop {
    interrupts::disable();
    SERIAL_RX_PENDING.store(false, ...);
    if let Some(b) = serial_rx_pop() {
        interrupts::enable();
        break b;
    }
    interrupts::enable_and_hlt();   // ← halts forever waiting for IRQ
}
```

The COM1 RX IRQ is delivered only to the BSP (the IO-APIC redirection
entry routes to LAPIC ID 0). When the feeder is dispatched on core 3,
it `enable_and_hlt`s, the CPU halts, and **no serial IRQ ever arrives
on core 3** to wake it. Other IRQs (timer at 1 kHz) DO wake it, but
the buffer is empty so it halts again.

**Crucially, the feeder never returns to the scheduler between halts.**
There is no `yield_now` call anywhere in the loop. Once the scheduler
on core 3 dispatches the feeder, **the scheduler on core 3 is
parked** — it cannot pick another task because `switch_context` to the
scheduler never happens. Anything queued to core 3 (kbd_server,
fat_server, e1000_driver) waits forever.

## Commit timeline (in chronological order on the working branch)

```
3729a69 (HEAD - baseline) DEBUG: log ap_idle_task post-hlt per core
4eda03b DEBUG: log yield_now enter + handoff per core
a2d932f DEBUG: log dispatch + resume around switch_context per core
61e044d Revert "Fix: spawn_fork_task pins fresh fork-children to spawning core"
597b99d Fix: spawn_fork_task pins fresh fork-children to spawning core
03ec6c9 DEBUG: log first 4 reschedule-IPIs received per core
7bd9bce DEBUG: log first 4 scheduler-loop wakes per core
e33d40b Fix: AP boot timeouts no longer leave phantom scheduler slots
39db4f7 Fix: scheduler.least_loaded_core skips offline cores
```

All commits *above* `3729a69` (i.e. attempts during this debug session)
were force-pushed away. They are visible in the agent's
conversation history; do NOT cherry-pick them blindly — they all
introduced regressions. Brief notes on each, in attempt order:

| Attempted commit | Approach                                              | Result |
|------------------|-------------------------------------------------------|--------|
| `dd570bb`        | `task::spawn_pinned_to_core(serial-stdin, BSP)`       | Core 0 starves: feeder owns BSP CPU between IRQs, init can't reap, framebuffer goes blank, mouse stops |
| `e62c6f5`        | `display_server` idle: 1 ms → 6 ms (yielding sleep)   | Did not fix the dd570bb regression |
| `bdb767f`        | Revert `e62c6f5`                                      | (cleanup) |
| `5306f93`        | `yield_now()` after `enable_and_hlt` in feeder        | "Didn't fix it" — placement shifted, framebuffer still broken |
| `8e78bd0`        | Revert `5306f93`                                      | (cleanup) |
| `e1eb5e7`        | `Task::parks_scheduler` flag + `core_load → MAX`      | Kbd works! But framebuffer breaks: display_server moved to core 1, term to same area, IPC chain failed |
| `0697c52`        | term `LOOKUP_MAX_ATTEMPTS` 8→100, backoff 5→50 ms     | Term still doesn't reach READY |
| `3c65bb3`        | display_server idle 1 ms → 6 ms (re-applied)          | No improvement; cursor still stuck at (0, 0) |
| **(reset)**      | `git reset --hard 3729a69 && git push --force`        | Back to known-good baseline |

## Code references

### Kernel
- **`kernel/src/main.rs:486`** — `serial_stdin_feeder_task`. The
  halt-loop that parks core 3. The fix candidate is to insert
  `crate::task::yield_now()` after `enable_and_hlt`. Caveat: in the
  past this caused `display_server` to migrate off BSP and break
  rendering — investigate why before re-trying.
- **`kernel/src/main.rs:205-274`** — service-task spawn order in
  `init_task`. Determines which core serial-stdin lands on (currently
  AP3 by least-loaded tiebreak).
- **`kernel/src/task/scheduler.rs:589-617`** — `core_load`. Needs
  awareness of "this core's current task will never yield". Approaches
  tried: explicit `parks_scheduler` flag (worked for kbd, broke
  display); time-based hog detection (couldn't distinguish a busy
  fork-issuing parent from a stuck halt-looper).
- **`kernel/src/task/scheduler.rs:660-676`** — `least_loaded_core`.
- **`kernel/src/task/scheduler.rs:1899`** — `task.start_tick = now` is
  the only site that updates start_tick (dispatch path).
- **`kernel/src/task/scheduler.rs:2168-2186`** — cpu-hog detection.
  **Has a stale comment-vs-impl bug:** `ran_ticks * 10` in the log
  message assumes 10 ms/tick (100 Hz), but `TICKS_PER_SEC = 1000`
  (`kernel/src/arch/x86_64/syscall/mod.rs:12224`). Reported "ran~Xms"
  values are 10× the actual elapsed time.
- **`kernel/src/arch/x86_64/syscall/mod.rs:14638-14786`** — `sys_poll`.
  Has its own 10×-multiplier bug at line 14647: `(timeout_i as
  u64).div_ceil(10)` assumes 10 ms/tick. So `poll(2000)` actually
  times out at 200 ticks = 200 ms. May be relevant for syslogd's
  cpu-hog pattern.
- **`kernel/src/arch/x86_64/syscall/mod.rs:3162-3232`** — `sys_nanosleep`.
  The `< 5 ms` branch is a TSC busy-spin that does **not** call
  `yield_now`. The `≥ 5 ms` branch yields between TSC checks. This
  is why a userspace daemon doing `nanosleep_for(0, 1_000_000)`
  saturates its core.

### Userspace
- **`userspace/display_server/src/main.rs:710`** — idle sleep
  `nanosleep_for(0, 1_000_000)` (1 ms). Falls into the kernel's
  busy-spin branch.
- **`userspace/display_server/src/main.rs:255-280`** — kbd / mouse
  service-lookup-with-backoff at startup.
- **`userspace/display_server/src/input.rs:200-224`** — `KbdInputSource::try_lazy_reconnect`.
- **`userspace/display_server/src/input.rs:288-312`** — `MouseInputSource::try_lazy_reconnect`.
  **Suspected hot zone:** if mouse registers AFTER display_server's
  initial lookup, the lazy reconnect fires `display_server: mouse
  service connected (lazy)`. If that log line is missing in a broken
  run, the reconnect logic itself is failing.
- **`userspace/display_server/src/input.rs:319-342`** — `MouseInputSource::poll_pointer`.
  `ipc_call(handle, MOUSE_EVENT_PULL, 0)`. Returns None on any non-success
  reply label; the dispatcher loop then produces no `CursorMoved` effect
  and `pointer_position` stays at its initial `(0, 0)`.
- **`userspace/term/src/display.rs:50-54`** — `LOOKUP_BACKOFF_NS = 5 ms`,
  `LOOKUP_MAX_ATTEMPTS = 8`. Total budget: 40 ms. Insufficient when
  display_server is delayed; raising to 5 s was tried (`0697c52`,
  reverted) but did not solve the underlying mouse-events-not-arriving
  problem.
- **`userspace/syslogd/src/main.rs:141-189`** — `main_loop`. Calls
  `poll(sock_fd, POLL_TIMEOUT_MS = 2000ms)` then `drain_kmsg`. Observed
  cpu-hog of ~500-760 ms at a stretch even though poll *should* yield;
  haven't pinned down whether this is a `sys_poll` bug or just very
  long iterations of `drain_kmsg`.
- **`userspace/audio_server/src/main.rs:67-76`** — exits cleanly (return
  0) when no AC'97 device, BEFORE registering `audio.cmd`. This is what
  triggers `session_manager`'s text-fallback (audio_server is one of 5
  required boot steps).
- **`userspace/session_manager/src/main.rs:271-280`** — `ipc_service_name`
  maps boot step name → registered service name. Note `"audio_server"`
  → `"audio.cmd"` — and audio_server never registers `audio.cmd` when
  hardware is absent.
- **`userspace/session_manager/src/main.rs:344` and `recover.rs::run_text_fallback_rollback`**
  — the F.4 `stop(...)` calls are stubs that print a log line and
  return `Ack`. They do NOT issue `init.cmd` writes yet. So the
  text-fallback teardown does not actually kill display_server / kbd /
  mouse / term. Don't be fooled by the alarming
  `session.recover.text_fallback: rolling back graphical session` log
  line — it's a future-track marker.

## Hypotheses ranked

1. **(Highest confidence — see `docs/handoffs/2026-04-25-scheduler-design-comparison.md`)
   The cursor-at-(0, 0) symptom is the lost-wake bug class identified
   in the 2026-04-25 doc.** `display_server.poll_mouse` calls
   `ipc_call(mouse_handle, MOUSE_EVENT_PULL, 0)`, which blocks
   display_server in `BlockedOnReply`. When mouse_server's reply
   races with display_server's switch-out under the
   `switching_out` / `wake_after_switch` protocol, the wake is lost
   and display_server is stuck in Blocked forever. The teal +
   stuck-cursor framebuffer is consistent with exactly one compose
   pass having run (initial BG_PIXEL fill + cursor sprite at the
   initial `(0, 0)`) before the next-iteration `poll_mouse` hung.
   The bug is masked at `3729a69` because display_server happens to
   land on BSP and the race window is much narrower there; the
   `parks_scheduler` change exposed it by shifting display_server to
   an AP under more cross-core IPC pressure.

   Resolution: per the 2026-04-25 doc's recommendation, rewrite the
   block/wake protocol to Linux's "single state word + condition
   recheck after state write" pattern, deleting `switching_out` and
   `wake_after_switch`. Or, as an intermediate, add a per-task
   spinlock around the block/wake transition.

2. **(Lower than I previously thought) The frame_tick BSP-only
   producer issue** — frame_tick is a global atomic readable from
   any core, so display_server-on-AP can drain it the same as on
   BSP. The earlier ranking that made this a top suspect was wrong;
   the lost-wake is a much better fit for the binary-failure shape.

3. **(Lower than I previously thought) `MouseInputSource::try_lazy_reconnect`
   state-machine bug** — possible but less likely than the lost-wake.
   Worth a one-line log to rule out, but don't spend a session on it
   before testing the lost-wake hypothesis.

4. **(Real bug, but secondary)** Even if mouse events were delivered,
   the syslogd cpu-hog (~500 ms / sshd ~480 ms / userspace-init
   ~590 ms) would make framebuffer updates stutter visibly. Not the
   cause of the cursor-doesn't-move-AT-ALL issue.

## What I would NOT do

- **Don't pin `serial-stdin` to a specific core.** Tried at `dd570bb`,
  it starves whichever core it's pinned to.
- **Don't add `yield_now()` to the feeder halt-loop without
  understanding the placement shift.** Tried at `5306f93`. The user
  reported it broke the framebuffer; we never figured out exactly why.
- **Don't add a generic time-based "hog detector" to `core_load`.**
  Tried with `last_yield_tick` and bare `start_tick`; both produce
  false positives on `userspace-init` during the boot fork burst,
  which legitimately runs for ~590 ms straight without yielding while
  it spawns 17 services.
- **Don't try to "fix" `session_manager`'s text-fallback as the
  primary lead.** It fires at `3729a69` too, the framebuffer still
  works there, and the `stop(...)` calls are stubs. Audio_server's
  clean exit IS a separate latent issue (it should register a stub
  `audio.cmd` even with no hardware so session_manager doesn't burn
  its retry budget), but fix it AFTER the kbd-and-mouse story
  converges.

## Recommended next session opening move

1. **Read `docs/handoffs/2026-04-25-scheduler-design-comparison.md`
   first.** The lost-wake bug class it documents is the top-ranked
   hypothesis for this issue. Skipping that doc means re-discovering
   the protocol bug on the back of yet another shotgun debugging
   session.

2. **Verify branch state.** `git log -1 --format=%H` should print
   `3729a69...`. Run `cargo xtask run-gui --fresh` on the test
   machine and confirm: terminal visible, mouse moves, can't type —
   that's the baseline. If it doesn't match, stop and figure out why
   before any code change.

3. **Falsify the lost-wake hypothesis** before committing to a
   protocol rewrite. Add one log inside
   `kernel/src/task/scheduler.rs::wake_task` printing "wake-task pid
   state switching_out wake_after_switch" on every wake. Then in
   `block_current_unless_woken_inner` (and the `_until` variant),
   log on entry and on the post-switch_context resume. With those
   two logs, a broken-state run will reveal:
   - whether display_server actually enters BlockedOnReply (yes →
     wake-side issue; no → IPC send-side issue)
   - whether the wake fires (yes → handler issue; no → wake-side
     code path didn't run, mouse_server's reply is somehow not
     reaching display_server's endpoint)
   - whether `switching_out=true` was observed at wake time (yes →
     classic lost-wake; the `wake_after_switch=true` deferred-enqueue
     handshake is the bug)

4. **If the lost-wake hypothesis is confirmed**, the recommendation
   in the 2026-04-25 doc is to rewrite the block/wake protocol to the
   Linux pattern. The intermediate suggested there — a per-task
   spinlock around the block/wake transition — is a smaller, lower-
   risk change and is worth attempting first.

5. **If the lost-wake hypothesis is falsified**, fall back to
   investigating `MouseInputSource::try_lazy_reconnect` (hypothesis
   3 in the ranked list above). Add a one-shot log on first
   successful `ipc_lookup_service("mouse")` in the lazy-reconnect
   branch.

6. **Hypothesis-test on real hardware**, not headless QEMU. The
   regression is only visible with a real display + real PS/2 mouse
   movement. Headless cannot reproduce it.

7. **One change at a time.** This bug ate two whole sessions of
   shotgun fixes. The branch already has 8 attempts that didn't
   converge. Make one hypothesis, write one log, run one test,
   read the result, and only then consider a code change.

## Open issues NOT directly on the path

These are real bugs surfaced during the investigation but not on the
critical path for the kbd-and-mouse-and-term story (which is now
believed to be the lost-wake protocol bug per the 2026-04-25 doc).
File them for later phases:

- **`audio_server` exits without registering `audio.cmd`**
  when no AC'97 hardware. session_manager's boot sequence treats it as
  required. Fix: have audio_server register a no-op `audio.cmd` stub
  even with no hardware, OR teach session_manager that audio is
  optional. (`userspace/audio_server/src/main.rs:67`,
  `userspace/session_manager/src/main.rs:271`)

- **`sys_poll` and the `cpu-hog` log message both have a 10×
  multiplier bug.** They assume 10 ms/tick (100 Hz timer) but the
  actual timer is 1 kHz (`TICKS_PER_SEC = 1000`).
  - `kernel/src/arch/x86_64/syscall/mod.rs:14647` — `(timeout_i as
    u64).div_ceil(10)` should be `(timeout_i as u64)` directly.
    Currently `poll(2000)` returns after 200 ms, not 2 s.
  - `kernel/src/task/scheduler.rs:2191` — `ran_ticks * 10` should be
    `ran_ticks` directly. All cpu-hog log values are 10× the truth.

- **`syslogd` cpu-hogs core 1 for ~500 ms at a stretch** even though
  it uses `poll`. Either the poll busy-yields are silently consuming
  the full 200 ms timeout window without giving cohabitants slices,
  or `drain_kmsg` is doing very long uninterrupted work.
  Investigation needed: `userspace/syslogd/src/main.rs:141-216`.

- **The serial-stdin feeder design is fundamentally fragile.**
  The kernel has IRQ12 (PS/2 mouse) → wake-task scaffolding for
  cross-core wakeups via the ISR wakeup queue
  (`kernel/src/smp/mod.rs::IsrWakeQueue`), but the COM1 IRQ4 path is
  still the legacy "ISR sets a flag, task halt-loops checking the
  flag" pattern. If the feeder were ported to the modern
  `signal_irq`-based notification scheme (like `net_task` is at
  `kernel/src/main.rs:598`), it would block on a notification rather
  than halting in user-userland. Then it would not park its core's
  scheduler regardless of which core the load balancer picks.

## Glossary / acronyms

- **BSP** = Bootstrap Processor (CPU 0)
- **AP** = Application Processor (CPUs 1..n)
- **IPI** = Inter-Processor Interrupt
- **F.x** in session_manager = Phase 57 task track F.x; F.4 was
  scheduled to write `/run/init.cmd` to issue real service stops but
  is currently stubbed.
- **`parks_scheduler`** = the `Task` flag from `e1eb5e7` (now reverted).
  Kept as conceptual shorthand for "this task may park its core
  indefinitely between IRQs".
