# Post-mortem: ring-3 NIC ingress task starves PID 1 reap loop

**Incident:** The `serverization-fallback` regression began failing shortly after Phase 55c
Track E landed. `service stop vfs` never completed within its 30 s budget because PID 1's
reap loop was not running `check_control_commands` on `/run/init.cmd`. The newer `e1000-restart-crash`
regression, introduced in the same phase and ungated by Track I.4, never passed in-tree —
it either hit `exit 4` (subscribe_and_bind failure) at `ba96bdc` or stalled at step 2.5
(EAGAIN never observed) at `8b65fdc` and onward.
**Status:** Resolved 2026-04-25 (items 2 + 3 below). Item 1 (nanosleep busy-yield)
remains as a known scheduler hygiene issue that surfaces as `serverization-fallback`
flakiness; the resolution restores e1000-restart-crash to green and re-enables the
ring-3 NIC RX path without re-introducing PID 1 starvation.
**Severity:** High — PR #118 could not satisfy the pre-push regression gate, and the
Phase 55b/c ring-3 networking extraction silently lost its RX half.
**Owners:** Kernel scheduler, IPC primitives, `RemoteNic` facade, service manager.
**Workaround commit:** `d16a5a2` `fix(kernel,e1000): stop spawning blocked ingress task to unblock PID 1`.

## Summary

Phase 55c Track E wired a kernel-side receiver task (`remote_nic_ingress_task`) and a
companion `net.nic.ingress` endpoint so the ring-3 e1000 driver could forward RX frames
and link-state events into the kernel through synchronous IPC. Bisect against
`serverization-fallback` pinned `8b65fdc fix: close Phase 55c readiness blockers` as the
first bad commit for that regression: with the ingress task spawned, PID 1's reap loop
was starved on core 0 to the point that `/bin/service stop <name>`'s 30 s polling budget
expired before init ever opened `/run/init.cmd`.

The actual fault is a scheduling pathology: any time PID 1 is on the same core as a
second blocked-on-recv task and PID 1 is executing `sys_nanosleep`'s long-sleep branch
(a `while rdtsc < deadline: yield_now()` busy-yield loop), the yield loop hogs the core
and PID 1 never makes enough forward progress between sleeps to touch
`/run/init.cmd`. The scheduler emitted both
`cpu-hog: pid=1 name=userspace-init ... ran~500ms final_state=Running` and
`stale-ready: pid=0 name=serial-stdin core=0 stale~550ms` under that pattern.

Two follow-on symptoms stacked on top:

- `e1000-restart-crash` was ungated by Track I.4 (commit `ba96bdc`) so that
  `cargo xtask regression --test e1000-restart-crash` would always be runnable, but the
  test also needed the ingress transport to exist in order to latch `RESTART_SUSPECTED`
  after a SIGKILL on `e1000_driver`. Removing the ingress task to unblock
  `serverization-fallback` broke the only path the kernel had to notice the driver had
  died in time for the test's 5-second EAGAIN retry window.
- `nanosleep`'s long-sleep branch was a documented approximation from the DOOM port
  ("yield costs ~10 ms at the AP timer granularity, which is acceptable for multi-millisecond
  sleeps"). On a quiet idle system the aggregate CPU share this gives init is fine; on a
  contended single core with extra Ready/Blocked bookkeeping it is not.

The landed workaround trades the RX half of the ring-3 e1000 driver for PID 1 responsiveness:
the kernel no longer registers `net.nic.ingress` or spawns the ingress receiver, and the
userspace driver now treats the ingress endpoint as optional (RX publish silently drops,
TX stays fully functional). `remote_nic_ingress_task` itself is kept intact under
`#[allow(dead_code)]` so the follow-up can re-enable it once the underlying scheduler /
nanosleep bug is addressed.

## What we lost by removing the ingress task

The ingress path was the kernel-side receiver in the textbook microkernel driver-extraction
pattern: ring-3 driver ↔ IPC rendezvous ↔ kernel-resident receiver. Turning it off is not a
neutral change — it ablates half of what Phase 55b/c delivered:

- **Ring-3 e1000 RX is dropped on the floor.** The driver still captures frames off the wire,
  but `NetServer::publish_rx_frame` returns `DeviceHostError::NotClaimed` without an ingress
  endpoint, so nothing flows from the wire back into the kernel net stack. Any protocol
  that needs a reply (TCP establishment, SSH handshake, ICMP reply, UDP receive, HTTP
  serving) does not work through `-device e1000` on real hardware. virtio-net is untouched
  because it uses a separate kernel-side ISR path.
- **Link-state updates do not reach the kernel.** The driver would normally publish
  `NET_LINK_STATE` through the ingress endpoint; without a receiver those publishes drop.
  In practice this is masked because most stacks just try to send and discover the link is
  down from the TX error path.
- **Phase 55b/c invariants are silently half-broken.** The whole point of the phase was
  "extract drivers to ring 3." The extraction is preserved for TX but the RX half is a
  stub. Regression coverage does not notice because no test pings something that replies.

## Workaround

`d16a5a2` applies three coupled edits:

- `kernel/src/main.rs` — removes both the `net.nic.ingress` endpoint creation/registration
  and the `task::spawn(remote_nic_ingress_task, "net-ingress")` call from `init_task`.
  The function is marked `#[allow(dead_code)]` so a future patch can re-enable it without
  a re-port.
- `userspace/drivers/e1000/src/main.rs` — makes the ingress service lookup optional:
  if the kernel has not published `net.nic.ingress`, the driver emits
  `e1000_driver: ingress service absent, RX publish disabled` and continues in TX-only mode.
- `userspace/drivers/e1000/src/io.rs` — `run_io_loop` now takes
  `ingress_endpoint: Option<EndpointCap>` and constructs the `NetServer` with or without
  `.with_ingress_endpoint(ep)` accordingly.

With this, `cargo xtask regression` goes from 9/11 to 10/11 (individual test runs). The
remaining failure, `e1000-restart-crash`, is a direct downstream consequence: without the
async ingress transport the kernel never learns the driver has died, so
`drain_tx_queue`'s `on_ipc_error` path — which is what sets `RESTART_SUSPECTED` and
surfaces `NEG_EAGAIN` to userspace — never fires.

## What the real fix would look like

The scheduler/nanosleep interaction is the root cause. A clean resolution lands
`remote_nic_ingress_task` back in the scheduler and drops the optional-ingress branch
from the driver. One pass each of the following would be sufficient:

1. **Replace `sys_nanosleep`'s long-sleep TSC-yield loop with
   `block_current_unless_woken_until`.** The primitive already exists in
   `kernel/src/task/scheduler.rs` (net_task uses it). Drops the sleeping task out of the
   run queue entirely; scan_expired_wake_deadlines wakes it on deadline.
   **Gotcha we hit:** a naive replacement regressed init's `stop_service` path. With the
   workaround reverted and the new block path wired in, PID 1 woke correctly from the
   first nanosleep (`service stop vfs completed` appeared for the first time ever), but
   a later 1-second sleep between SIGTERM and SIGKILL in `stop_service` silently failed
   to wake, so the test stalled at the second stop (`service stop net_udp`). The primitive
   works for short kbd_server sleeps and for net_task, but some user-context callers are
   not being rescheduled after the deadline fires. A follow-up needs to audit
   `scan_expired_wake_deadlines` and the dispatch path for PID-1-specific behavior — the
   initial block-and-wake cycle works; subsequent cycles on the same task do not.
2. **Tie `RESTART_SUSPECTED` to driver process-exit.** Currently the kernel only notices
   a ring-3 NIC driver is gone when `drain_tx_queue` fails to `send_buf` into the closed
   command endpoint. That coupling is what we just broke. Cleanly: hook `close_owned_by`
   (or the `cleanup_task_ipc` path) to call `RemoteNic::on_ipc_error` when the task that
   owned `net.nic` exits. Then EAGAIN-on-restart is independent of the ingress transport.
3. **Fold the ingress receiver into `net_task`.** `net_task` already blocks on
   `NIC_WOKEN` and drains both virtio-net and `RemoteNic` TX. It could also drain the
   ingress endpoint's pending-sender queue each iteration. This removes the extra
   scheduler task entirely, which — even once item 1 is solved — is the cheapest place
   for the RX dispatcher to live. The current receive model requires a dedicated
   `ipc::endpoint::recv_msg` thread because recv is synchronous; either add a
   non-blocking `recv_msg_nowait` or have the net_task's wake flag be set by the driver's
   send_buf and do the peek-and-drain inline.

Any one of 1, 2, or 3 restores the ingress task cleanly. Doing 1+2 gives us a principled
fix for both the regression and the architectural gap; 3 is the longer-term simplification.

## Resolution (2026-04-25): items 2 + 3 landed; item 1 deferred

The follow-up landed items 2 and 3 from the section above — **both** are required, and
they compose cleanly:

### Item 3 — fold the ingress receiver into `net_task`

The Redox `event:` / `O_NONBLOCK` precedent (cf. `smolnetd` reading from
`/scheme/<adapter>` with `O_NONBLOCK` and dispatching readiness through an event
queue) maps directly onto our model: the kernel does **not** need a dedicated
receiver task per ring-3 driver. The pieces:

- **`ipc::endpoint::recv_msg_nowait`** — new non-blocking variant that mirrors
  `recv_msg`'s sender-found path (reply cap insertion, bulk transfer, sender wake)
  but returns `None` instead of enqueueing the receiver when no sender is queued.
- **Per-endpoint pending-send hook** — `Endpoint::on_pending_send: Option<fn()>`
  installed on the ingress endpoint so that when the driver's `ipc_send_buf` enqueues
  a sender with no receiver waiting, the hook fires `wake_net_for_ingress` which
  sets an edge-triggered `INGRESS_HAS_WORK` flag and wakes `net_task`.
- **`net_task` ingress drain** — on each iteration, if `INGRESS_HAS_WORK` was set,
  call `recv_msg_nowait` in a loop until it returns `None`, dispatching `NET_RX_FRAME`
  through `RemoteNic::inject_rx_frame` and `NET_LINK_STATE` through
  `RemoteNic::handle_link_state`. The edge-triggered gate is critical: an
  unconditional `recv_msg_nowait` per net_task wake reliably amplifies PID 1
  starvation (see "Open issue" below).

`remote_nic_ingress_task` is deleted entirely — there is no kernel-resident receiver
on the run queue. The ingress endpoint stays a normal IPC rendezvous point; the
driver's `send_buf` semantics are unchanged.

### Item 2 — driver-death detection independent of TX traffic

The kernel learns of driver exit through the IPC cleanup path rather than through TX
drain failure. Two pieces:

- **`EndpointRegistry::endpoints_owned_by(task_id)`** captures the dying task's owned
  endpoint IDs *before* `close_owned_by` clears the `owner` field, so the dispatch
  loop after the lock release knows which IDs to notify.
- **`RemoteNic::on_endpoint_closed` and `blk::remote::on_endpoint_closed`** are
  invoked from `cleanup_task_ipc` for each owned endpoint. Both have lock-free
  fast-paths that early-return when no driver is registered (most cleanups) so the
  cleanup hot path stays a single atomic load.

For `RemoteNic` specifically, `on_endpoint_closed` sets `RESTART_SUSPECTED` but
intentionally **does not** clear `REMOTE_NIC_REGISTERED`. The host-testable
`sendto_restart_errno` returns EAGAIN exactly when `is_registered && is_restarting`
— clearing the registered flag here would suppress EAGAIN until the driver
re-registered, which is the opposite of what `e1000-restart-crash`'s H.1 assertion
exercises.

### Companion changes that compose with items 2 + 3

- **`sendto_restart_ret` is now consume-on-observe.** Once a userspace caller
  observes the EAGAIN signal, the latch is cleared so subsequent sends proceed.
  Without this, the latch could survive arbitrarily long (especially with item 3
  draining link-state events), causing every sendto post-restart to return EAGAIN
  forever.
- **`ensure_link_event_entry` no longer auto-clears `RESTART_SUSPECTED`.** With
  item 3 draining link-state events, the post-restart driver's bootstrap link state
  raced with userspace observation: a fast restart re-published link state before
  the test's retry loop caught the EAGAIN window. The latch is now cleared only via
  the consume-on-observe path in `sendto_restart_ret`.

### Item 1 (nanosleep busy-yield) — still open

`sys_nanosleep`'s long-sleep `while rdtsc < deadline: yield_now()` busy-yield loop
remains. With items 2 + 3 in place there is no extra blocked task on the run queue
to interact with, but the underlying scheduler hygiene is still suboptimal:
`serverization-fallback` continues to be flaky (~40% pass rate on the base; my
changes do not fix this). Fixing requires re-attempting the
`block_current_unless_woken_until` substitution and resolving the second-wake bug
described above. Tracked separately.

### Files touched

- `kernel/src/ipc/endpoint.rs` — `recv_msg_nowait`, `set_endpoint_pending_send_hook`,
  `endpoints_owned_by`, `Endpoint::on_pending_send` field, hook invocation in
  `send`/`call_msg`/`send_with_cap`.
- `kernel/src/ipc/cleanup.rs` — capture `owned_ep_ids` and dispatch driver-facade
  hooks after lock release.
- `kernel/src/net/remote.rs` — `on_endpoint_closed`, consume-on-observe in
  `sendto_restart_ret`, removed `RESTART_SUSPECTED` clear from
  `ensure_link_event_entry`.
- `kernel/src/blk/remote.rs` — `on_endpoint_closed`, lock-free
  `REMOTE_BLOCK_REGISTERED` mirror.
- `kernel/src/net/mod.rs` — `INGRESS_HAS_WORK` edge-trigger flag.
- `kernel/src/main.rs` — restored `net.nic.ingress` endpoint creation in `init_task`,
  installed wake hook, folded ingress drain into `net_task` via
  `drain_remote_nic_ingress`. Removed `remote_nic_ingress_task`.
- `kernel/src/syscall/device_host.rs` — drive-by fix to a pre-existing test-only
  build break (`install_irq_binding` missed the `legacy_irq_line` argument added to
  `bind_irq_vector`).

## Timeline

- **2026-04-22** `8b65fdc fix: close Phase 55c readiness blockers` adds
  `remote_nic_ingress_task` + `net.nic.ingress` endpoint.
- **2026-04-22** `ba96bdc fix Track I regression gates` ungates `e1000-restart-crash` so
  `cargo xtask regression` always includes it. `serverization-fallback` has already
  regressed; nobody notices yet because the pre-push gate only runs
  `cargo xtask regression --timeout 90`, which at this point does not hit
  `serverization-fallback` (the test was still within budget) but *does* start failing
  `e1000-restart-crash`. (The test is new-and-never-passing, not "was passing and broke.")
- **2026-04-24** PR #118 attempt to merge uncovers the full breakage: 0/11 on first run,
  then `0..9/11` across attempts. Bisect isolates `8b65fdc`.
- **2026-04-24** Workaround commit `d16a5a2` lands, 10/11 green on individual runs.
- **2026-04-24** Attempt to wire `block_current_unless_woken_until` into `sys_nanosleep`
  surfaces a second bug: PID 1 wakes from the first deadline but a follow-up sleep
  silently stalls. Reverted.
- **2026-04-25** Items 2 + 3 implemented: `RemoteNic`/`RemoteBlockDevice` close-hook
  in `cleanup_task_ipc`, `recv_msg_nowait` non-blocking IPC primitive,
  edge-triggered ingress drain in `net_task`, consume-on-observe semantics for
  `sendto_restart_ret`, removal of `RESTART_SUSPECTED` auto-clear from
  `ensure_link_event_entry`. `e1000-restart-crash` passes deterministically;
  ring-3 e1000 RX path restored without re-introducing PID 1 starvation. Item 1
  (nanosleep) deferred — `serverization-fallback` flakiness remains at the
  pre-existing baseline.

## Lessons

- **Microkernel-style ingress doesn't need a dedicated kernel receiver.** Redox's
  `event:` scheme + `O_NONBLOCK` reads from per-fd queues is the textbook design;
  m3OS now matches it (driver does fire-and-forget IPC; a per-endpoint wake hook
  flips a flag; the existing net_task does a non-blocking drain via
  `recv_msg_nowait`). The fundamental insight: **the kernel does not need a task
  blocked on `recv_msg` per ring-3 driver — a non-blocking primitive plus an
  edge-triggered wake suffices**.
- **Driver-death detection should not depend on TX traffic.** Hooking
  `cleanup_task_ipc` to call `RemoteNic::on_endpoint_closed` is the symmetric
  counterpart to `drain_tx_queue`'s `on_ipc_error`. Both paths now feed into the
  same `RESTART_SUSPECTED` latch; userspace observes EAGAIN within microseconds of
  the kill rather than waiting for the next TX retry.
- **Consume-on-observe latches survive concurrent producers.** When the
  driver-restart path re-publishes link state asynchronously, a "set on death,
  clear on link-up" latch raced with userspace observation. Switching to "set on
  death, clear on first sendto observation" makes the contract robust to whatever
  arrival order the post-restart bootstrap produces.
- **Adding a kernel task on the shared run queue is not free.** Even a blocked-on-recv
  task interacts with the scheduler enough that busy-yield paths on the same core get
  starved. The nanosleep TSC-yield loop was load-bearing on "no other task exists" and
  nobody noticed.
- **Test gating matters.** `e1000-restart-crash` was added, ungated in the same phase,
  and never observed to pass green. It has been listed as required coverage without ever
  having been required-passing. The pre-push gate prevents that from riding in silently,
  which is working as designed; but the regression catalog itself should enforce that a
  test lands green at least once before it's ungated.
- **`cargo xtask regression` is not test-isolated.** Individual-test runs and full-suite
  runs produce different pass counts because QEMU resource contention causes spurious
  timeouts in the harness. Each test ideally would get a fresh QEMU or a cooldown between
  runs.
