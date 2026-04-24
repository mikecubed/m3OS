# Post-mortem: ring-3 NIC ingress task starves PID 1 reap loop

**Incident:** The `serverization-fallback` regression began failing shortly after Phase 55c
Track E landed. `service stop vfs` never completed within its 30 s budget because PID 1's
reap loop was not running `check_control_commands` on `/run/init.cmd`. The newer `e1000-restart-crash`
regression, introduced in the same phase and ungated by Track I.4, never passed in-tree —
it either hit `exit 4` (subscribe_and_bind failure) at `ba96bdc` or stalled at step 2.5
(EAGAIN never observed) at `8b65fdc` and onward.
**Status:** Partially resolved 2026-04-24.
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

## Lessons

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
