---
status: open
branch: feat/phase-63-audio-stack-implementation
last-known-good-commit: ada4f6a
date: 2026-05-11
component: kernel/ipc (recv_msg_with_notif) / kernel/task/scheduler (block_current_on_notif_v2) / userspace audio_server
related:
  - docs/handoffs/2026-04-25-scheduler-design-comparison.md
  - docs/63-audio-stack-implementation.md
  - docs/handoffs/2026-04-28-graphical-stack-startup.md
---

# Handoff — Phase 63 audio IRQ / scheduler-v2 wake race

## TL;DR

`cargo xtask audio-smoke` and `cargo xtask run-gui` (with audio enabled)
both fail to play any audio because `audio_server`, once it claims the
AC'97 PCI device and subscribes to its IRQ, ends up in one of two
failure modes:

1. **`audio_server: recv failed`** every ~7s of boot, init's
   `restart=on-failure max_restart=3` policy burns the budget, then
   audio_server is permanently down → `frames_consumed` stays at 0.
2. **`task pid=16 name=fork-child state=BlockedOnNotif stuck-since=Nms
   (no waker registered)`** — audio_server is parked on a notification
   waiting for an IRQ that never fires.

The smoking gun is in
[`kernel/src/ipc/endpoint.rs:678-696`](../../kernel/src/ipc/endpoint.rs):
`recv_msg_with_notif` calls `block_current_on_notif_v2`, returns
`woken=true`, then both `take_message` and `drain_bits` are empty, so
the function returns `Message::new(u64::MAX)` (the "spurious wake"
debug-assert path). The kernel hands `u64::MAX` back to userspace and
audio_server's io loop treats that as a fatal error.

e1000 / nvme do **not** observe this race because their IRQs fire
frequently enough to mask it; audio_server has long idle periods and
hits the race on essentially every recv once the AC'97 IRQ binding is
active.

The four recent commits fix the surrounding plumbing so the bug is now
isolated to the kernel scheduler-v2 + bound-notification interaction
(see "Already done" below). What's left is a kernel-side investigation
of the wake-without-condition race.

## Reproduction

On any host:

```bash
git checkout feat/phase-63-audio-stack-implementation
git pull   # ada4f6a or later
cargo xtask audio-smoke 2>&1 | tee audio-smoke.log
```

Failure signature in the harness output:

```
[step 4] wait-pass-or-fail: guest/audio: audio-demo PASS sentinel (30s)
audio-smoke: FAILED
step 4 timed out: guest/audio: audio-demo PASS sentinel
expected pass pattern: "AUDIO_DEMO:PASS"
```

Followed by the trace-ring dump and:

```
[WARN] [sched] task pid=16 name=fork-child state=BlockedOnNotif
  stuck-since=30274ms (no waker registered)
```

For full kernel serial output, run with the diagnostic env var:

```bash
M3OS_SMOKE_SERIAL_DUMP=/tmp/serial.log cargo xtask audio-smoke
```

(Wired up in `xtask/src/main.rs:3680`; dumps the entire serial buffer
to the path on smoke failure. Do this **first** in the new session — the
default smoke output only prints the trace ring, not the kernel
messages.)

## Symptom — verbatim from a working repro

From a fresh `cargo xtask run-gui --kvm --fresh 2>&1 | tee m3os.log` on
Omarchy (PipeWire host, but PulseAudio probe falls back to `none` so
QEMU still uses `-audiodev none,id=snd0 -device AC97,addr=0x5`):

```
init: started 'audio_server' pid=16
audio_server: spawned
[INFO] [pci] claim: ring3-driver -> 8086:2415 00:05.0 (slot 2)
[INFO] device_host.claim pid=16 bdf=0000:00:05.0 cap_handle=0
[INFO] device_host.dma_alloc pid=16 size=4096 iova=0x31ad000 cap_handle=2
[INFO] device_host.dma_alloc pid=16 size=16384 iova=0x31b0000 cap_handle=3
[INFO] [pci-msi] 8086:2415: no MSI/MSI-X capability — fall back to INTx
[INFO] [apic] I/O APIC: PCI IRQ 10 → GSI 10 (pin 10) → vector 98 (level, active-low)
[INFO] device_host.irq_subscribe routed legacy INTx line 10 to vector 0x62
[INFO] device_host.irq_subscribe pid=16 bdf=0000:00:05.0 vector=0x62 notif=0 bit=0 cap_handle=4
AUDIO_SMOKE:server:READY                              ← entered run_io_loop
... ~7 seconds of normal boot activity ...
audio_server: recv failed                              ← recv returned u64::MAX
[INFO] device_host.dma_release pid=16 freed=2
[INFO] device_host.release pid=16 freed_claims=1 freed_mmio=0 freed_irqs=1
init: service 'audio_server' exited with error 8
init: restarting 'audio_server' after 1s delay (attempt 1/3)
... same pattern repeats for 3 restarts ...
```

Note: the "7 seconds" interval is consistent across runs. Whatever
triggers the first spurious wake fires at a deterministic point in the
boot sequence.

## Root cause — current best hypothesis

`recv_msg_with_notif` (`kernel/src/ipc/endpoint.rs:560-700`) implements
the v2 bound-notification recv. The relevant tail is:

```rust
// 1. Try fast-paths (drain notification bits, pop pending sender).
//    [omitted — these work fine]

// 2. Block in BlockedOnNotif until either a notification fires
//    or a sender wakes us via deliver_message.
let _ = scheduler::block_current_on_notif_v2(receiver);
notification::unregister_recv_waiter(notif_id, receiver);

// 3. Re-check both wake conditions after returning.
if let Some(msg) = scheduler::take_message(receiver) {
    return (RECV_KIND_MESSAGE, msg);
}
let bits = notification::drain_bits(notif_id);
if bits != 0 {
    // ... return Notification ...
} else {
    debug_assert!(false, "[ipc] recv_msg_with_notif: spurious wake");
    (RECV_KIND_MESSAGE, Message::new(u64::MAX))   // ← fires constantly in release
}
```

Conditions for the bug:
- `block_current_on_notif_v2` returns true (or otherwise unblocks).
- `take_message(receiver)` returns `None`.
- `drain_bits(notif_id)` returns `0`.

Possible causes (ordered by likelihood):
1. **Race between IRQ shim drain and recv post-block drain.** The IRQ
   shim signals the notification → `register_reply_waker`'s woken flag
   flips → block_current returns → but the IRQ shim has already drained
   the bits as part of waking the task. By the time `drain_bits` runs in
   recv_msg_with_notif, bits are 0.
2. **Spurious wake from `register_reply_waker`.** The waker may be
   triggered without a corresponding state change (e.g. preemption,
   signal, deadline scanner). audio_server has no deadline registered.
3. **`block_current_on_notif_v2` early-return when the woken flag is
   already set at entry.** The pre-check at line 3403
   (`has_pending_message(receiver)`) returns false; `block_current_until`
   is then called and may return immediately if `woken` is already true
   from a stale signal.

Why other ring-3 drivers are unaffected:
- `e1000_driver`: the `net_server.handle_next` abstraction (`userspace/lib/driver_runtime/src/`) wraps `recv` and tolerates `Err`
  by looping. Spurious wakes are masked.
- `nvme_driver`: similar `block_server` abstraction, plus block I/O
  fires IRQs on every queued request — there's almost always real work
  to dispatch.
- `audio_server`: idle for seconds at a time, calls `recv` directly via
  `transport.recv_with_capacity`, treats `Err` as fatal (returns 8 →
  init restart cycle).

## Already done — current state of the branch

Four commits on `feat/phase-63-audio-stack-implementation` close every
layer above the kernel race:

| Commit | What it fixed | File(s) |
|---|---|---|
| `d3e88f3` | `cargo xtask run-gui` exited silently on Omarchy because `-audiodev pa,id=snd0` failed to connect to PulseAudio (Omarchy ships PipeWire without `pipewire-pulse`). | `xtask/src/main.rs` — added `gui_audiodev_flag()` runtime probe. |
| `3164cdf` | `audio_server` was always falling into stub mode because the kernel's `is_authorized_driver_process` gate (`kernel/src/syscall/device_host.rs:126`) requires the caller's `exec_path` to start with `/drivers/`, but audio_server lived at `/bin/audio_server`. | `kernel/src/fs/ramdisk.rs` (move ELF entry from BIN_ENTRIES to DRIVERS_ENTRIES); `kernel/initrd/etc/services.d/audio_server.conf` + `xtask/src/main.rs` audio_server.conf (`command=/drivers/audio_server`); `userspace/audio_server/src/lib.rs` test assertion. |
| `51002f8` | `SyscallBackend::recv` allocated a 1522 B bulk buffer (sized for net frames). audio's `SubmitFrames` carries up to 64 KiB of PCM; the kernel was silently truncating to 1522 B. | `userspace/lib/driver_runtime/src/ipc/mod.rs` — added `recv_with_capacity`; `userspace/audio_server/src/irq.rs` — calls it with `MAX_SUBMIT_BYTES + 256`. |
| `ada4f6a` | An earlier "log + continue" branch on recv error (in 51002f8) turned the kernel race into a tight hot loop that allocated 64 KiB / iter and starved stdin_feeder. Reverted to the original "exit 8 → init restart" behavior. | `userspace/audio_server/src/irq.rs` — reverted continue branch. |

After these four commits, the **only remaining failure** is the kernel
scheduler-v2 + bound-notification race that returns `u64::MAX` from
`ipc_recv_msg`.

## What I tried that didn't work

Logging the failures here so the next session doesn't repeat them:

1. **"Tolerate transient recv errors with `continue`".** Created the
   tight hot loop described above. Reverted in `ada4f6a`. Lesson: the
   recv error is not transient — it fires on every call, so you can't
   simply ignore it.

2. **Increasing the recv buffer to 64 KiB.** This is correct (audio
   needs it) but does not fix the wake race — recv still returns
   `u64::MAX` immediately.

3. **Investigating whether `MAX_BULK_LEN` (kernel side, 65536) caps the
   buf_len.** The kernel does NOT validate buf_len in
   `ipc_recv_msg`; the only cap is on the send path. So buffer size is
   not the source of the immediate `u64::MAX`.

4. **Asking whether the AC'97 IRQ ever fires.** I never got past the
   recv-error to verify — but the symptom is the same in both
   `audio-smoke` (where audio-demo runs and submits frames) and
   `run-gui` (where no client ever runs), so the wake bug is
   independent of any actual IRQ activity. It's likely triggered by
   the very *act* of binding the notification.

## Concrete next-step plan

The right starting point is **kernel-side instrumentation**, not more
userspace adjustments.

### Step 1 — capture full serial output, not just the trace ring

Add `M3OS_SMOKE_SERIAL_DUMP=/tmp/serial.log` to your audio-smoke
invocation. The default smoke harness only prints the trace ring on
failure (last 256 events per core); the actual kernel serial output
(audio_server's "recv failed", per-IRQ logs, scheduler diagnostics) is
discarded. The wired-up env var lives in `xtask/src/main.rs:3680`.

### Step 2 — instrument the spurious-wake path

Add a temporary log line at
[`kernel/src/ipc/endpoint.rs:695`](../../kernel/src/ipc/endpoint.rs)
just before the `debug_assert!(false, "[ipc] recv_msg_with_notif:
spurious wake")`. Capture: receiver TaskId, ep_id, notif_id, the value
returned by `block_current_on_notif_v2`, what the woken flag was at
entry vs. exit, whether `unregister_recv_waiter` returned anything
useful.

```rust
log::warn!(
    "[ipc] recv_msg_with_notif: spurious wake — task={:?} ep={:?} notif={:?} \
     post-block bits={} pending_msg={}",
    receiver, ep_id, notif_id, bits,
    scheduler::has_pending_message(receiver),
);
```

This will tell you whether the wake is coming from:
- A real IRQ that drained too eagerly (bits would be 0 only after a
  successful drain elsewhere)
- A scheduler-side spurious wake (no IRQ fired but `woken` was true at
  block entry)

### Step 3 — verify with a unit test on `kernel-core`

`kernel-core/src/sched_model.rs` has the host-testable scheduler state
machine. Add a test that drives `BlockedOnNotif × wake` with the
condition (message + bits) both empty at the moment `block_current` is
called. Should reproduce the symptom on the host without QEMU.

### Step 4 — fix the race

Two candidate strategies (the
[2026-04-25 scheduler-design-comparison handoff](2026-04-25-scheduler-design-comparison.md)
discusses both at length):

- **Per-task spinlock around block/wake** (Linux `pi_lock` model). Smallest
  fix; makes the block + condition-check + wake transition atomic.
- **Single state-word + condition recheck after state write**
  (Linux `try_to_wake_up`). Larger rewrite; closes the entire
  lost-wake / spurious-wake bug class.

The 2026-04-25 doc strongly recommends the second; the 2026-04-28
handoff provides additional context on why intermediate fixes don't
hold.

### Step 5 — backstop in audio_server

Even after the kernel race is fixed, `audio_server` should not crash
on the first recv error. The current bounded "exit 8 → init restart 3x"
behavior is fine as a backstop, but consider adding a counter:
tolerate up to N consecutive errors before exiting. Avoid the tight-
loop pitfall (no allocation in the error branch; yield between
retries). See the discussion in the body of `ada4f6a`'s commit message.

## Files to read first (in this order)

1. **This document.**
2. `kernel/src/ipc/endpoint.rs:560-700` — the `recv_msg_with_notif`
   function that returns `u64::MAX` on the spurious-wake path.
3. `kernel/src/task/scheduler.rs:3395-3414` — `block_current_on_notif_v2`,
   the v2 blocking primitive recv_msg_with_notif depends on.
4. `kernel/src/ipc/notification.rs` — `drain_bits`,
   `register_recv_waiter`, `unregister_recv_waiter`, `signal_irq_bit`.
5. `userspace/audio_server/src/irq.rs:191-260` — audio_server's
   `run_io_loop`, the consumer that hits the race.
6. `userspace/lib/driver_runtime/src/ipc/mod.rs:195-300` — the
   `SyscallBackend::recv` / `recv_with_capacity` path; trace through
   how `u64::MAX` from the syscall becomes a `DriverRuntimeError`.
7. `docs/handoffs/2026-04-25-scheduler-design-comparison.md` —
   discusses the same lost-wake bug class for the v1 scheduler;
   recommendations apply directly.

## Key constants and IDs (for triage)

| Symbol | Value | Source |
|---|---|---|
| AC'97 PCI ID | `8086:2415` (Intel 82801AA) | `userspace/audio_server/src/lib.rs:4` |
| AC'97 PCI BDF | `0000:00:05.0` | `xtask/src/main.rs` AC97_QEMU_AUDIO_DEVICE_FLAGS — `addr=0x5` |
| `SENTINEL_BUS` / `SENTINEL_DEVICE` / `SENTINEL_FUNCTION` | `0x00` / `0x05` / `0x00` | `userspace/audio_server/src/lib.rs:65-67` |
| AC'97 INTx line (legacy) | 10 | observed in m3os.log: `PCI IRQ 10 → GSI 10 (pin 10)` |
| Allocated vector | 0x62 (98) | observed; not pinned |
| `MAX_SUBMIT_BYTES` | 64 KiB | `kernel-core/src/audio/protocol.rs:57` |
| `MAX_BULK_LEN` (kernel) | 65536 | `kernel/src/ipc/mod.rs:656` |
| audio_server pid | 16 | `docs/handoffs/2026-04-28-graphical-stack-startup.md:160` |

## Out of scope

- The PipeWire socket probe in `xtask/src/main.rs::detect_gui_audio_driver`
  currently only checks `$XDG_RUNTIME_DIR/pipewire-0`. On Omarchy
  (which ships PipeWire) this socket exists at the standard path but
  the probe was returning `none` in one repro — investigate whether
  Omarchy uses a different socket name (`pipewire-0-manager`?) or
  whether `$XDG_RUNTIME_DIR` was unset in the build environment. This
  is a UX issue (audio is silent rather than audible) but does not
  affect the kernel race.
- The Phase 63 audio-smoke gate is not in PR CI (`cargo xtask check`
  only). After fixing the kernel race, consider wiring `audio-smoke`
  into the pre-push hook so this regression class is caught.

## Done-when

- `cargo xtask audio-smoke` passes — `AUDIO_DEMO:PASS` sentinel is
  observed within the 30s timeout.
- `cargo xtask run-gui` boots cleanly with no `audio_server: recv
  failed` lines on serial; `audio_server` stays alive past 30s of idle.
- `frames_consumed > 0` is observable via `audio-stats` from the
  shell.
- The recorded WAV file (`target/audio-smoke/audio.wav`) has at least
  5% of samples with `|sample| > 100` (the existing audio-smoke
  acceptance criterion).
