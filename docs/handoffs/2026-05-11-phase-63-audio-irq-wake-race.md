---
status: open
branch: feat/phase-63-audio-stack-implementation
last-known-good-commit: f1573bd
date: 2026-05-11
component: userspace audio_server (Ac97Backend) / kernel device-host IRQ routing
related:
  - docs/handoffs/2026-04-25-scheduler-design-comparison.md
  - docs/63-audio-stack-implementation.md
  - docs/handoffs/2026-04-28-graphical-stack-startup.md
---

# Handoff — Phase 63 audio AC'97 IRQ pipeline

> **Doc title note**: filename still says `audio-irq-wake-race` from when
> that *was* the bug. The wake-race is fixed (`f1573bd`); the file is
> kept under the same name so existing references don't break, but the
> active investigation in this doc is now the AC'97 IRQ delivery path.

## ⚠ Status update (2026-05-11)

**The original spurious-wake symptom this doc was opened for is FIXED**
in `f1573bd` (Codex-authored, codex-rescue session
`019e15d7-5a1c-7c20-a50d-10b223b3084a`). Root cause was hypothesis C
from the original analysis: a stale ISR wake token in
`isr_wake_queue` survives `recv_msg_with_notif`'s fast-path drain and
is later converted into a wake on a task that has no message and no
pending bits, returning `u64::MAX` from the syscall.

The fix sits in `kernel/src/task/scheduler.rs` (the per-core
`isr_wake_queue` drain loop) and a new helper
`bound_pending_bits_for_task` in `kernel/src/ipc/notification.rs` —
when a queued wake target is `BlockedOnNotif` AND `pending_msg=None`
AND its bound notification has 0 pending bits, the wake is dropped
because that combination uniquely identifies a stale token. ~25 lines,
no new locks, no new allocations.

`audio-smoke` no longer logs `audio_server: recv failed`. **However,
`audio-smoke` still fails** — audio_server now correctly stays in
`BlockedOnNotif` waiting for an AC'97 IRQ, and that IRQ never arrives.
This is a fundamentally different bug (AC'97 controller programming /
QEMU interrupt routing / IRQ delivery to the bound notification) and
needs its own focused investigation. The "Concrete next-step plan"
section below is rewritten for this new bug; the original spurious-
wake plan is preserved at the end for historical context.

## TL;DR (post-fix state)

`cargo xtask audio-smoke` still times out. After `f1573bd` the failure
mode is now:

- audio_server claims AC'97 (`8086:2415` at `0000:00:05.0`)
- audio_server subscribes to legacy INTx line 10 → kernel allocates
  vector 0x62, routes via I/O APIC
- audio_server enters `run_io_loop` and parks in `BlockedOnNotif`
- audio-demo runs, sends `Open` → audio_server wakes, replies → idle
- audio-demo sends `SubmitFrames` (64 KiB PCM) → audio_server wakes,
  fills BDL, writes LVI, replies → idle
- The AC'97 controller (with `CR.RPBM | LVBIE | FEIE | IOCE` set and a
  populated BDL) **never raises an IRQ on slot completion**
- audio_server stays parked in `BlockedOnNotif` indefinitely
- `frames_consumed` stays at 0
- audio-demo's `drain` request also gets a reply but with stale stats,
  so AUDIO_DEMO never reaches `:PASS`

So the open question is: **why doesn't the AC'97 controller raise an
IOC interrupt after the BDL is programmed and LVI is advanced?**
Candidates listed in "Concrete next-step plan" below.

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

Five commits on `feat/phase-63-audio-stack-implementation` close every
known layer above the AC'97 IRQ-delivery question:

| Commit | What it fixed | File(s) |
|---|---|---|
| `d3e88f3` | `cargo xtask run-gui` exited silently on Omarchy because `-audiodev pa,id=snd0` failed to connect to PulseAudio (Omarchy ships PipeWire without `pipewire-pulse`). | `xtask/src/main.rs` — added `gui_audiodev_flag()` runtime probe. |
| `3164cdf` | `audio_server` was always falling into stub mode because the kernel's `is_authorized_driver_process` gate (`kernel/src/syscall/device_host.rs:126`) requires the caller's `exec_path` to start with `/drivers/`, but audio_server lived at `/bin/audio_server`. | `kernel/src/fs/ramdisk.rs` (move ELF entry from BIN_ENTRIES to DRIVERS_ENTRIES); `kernel/initrd/etc/services.d/audio_server.conf` + `xtask/src/main.rs` audio_server.conf (`command=/drivers/audio_server`); `userspace/audio_server/src/lib.rs` test assertion. |
| `51002f8` | `SyscallBackend::recv` allocated a 1522 B bulk buffer (sized for net frames). audio's `SubmitFrames` carries up to 64 KiB of PCM; the kernel was silently truncating to 1522 B. | `userspace/lib/driver_runtime/src/ipc/mod.rs` — added `recv_with_capacity`; `userspace/audio_server/src/irq.rs` — calls it with `MAX_SUBMIT_BYTES + 256`. |
| `ada4f6a` | An earlier "log + continue" branch on recv error (in 51002f8) turned the kernel race into a tight hot loop that allocated 64 KiB / iter and starved stdin_feeder. Reverted to the original "exit 8 → init restart" behavior. | `userspace/audio_server/src/irq.rs` — reverted continue branch. |
| `f1573bd` | Stale ISR wake tokens left in `isr_wake_queue` after `recv_msg_with_notif`'s fast-path bit-drain were converted into wakes on tasks with no message and no bits, returning `u64::MAX`. Drop those tokens in the per-core drain loop. **Codex-authored** via codex-rescue. | `kernel/src/task/scheduler.rs` (drain-loop guard); `kernel/src/ipc/notification.rs` (new `bound_pending_bits_for_task` helper). |

**After f1573bd, audio_server is no longer crashing.** It correctly
parks in `BlockedOnNotif` waiting for an AC'97 IRQ. The remaining
failure is that **the AC'97 IRQ never fires**, so the wake never
arrives and `frames_consumed` stays at 0.

## What I tried that didn't work

Logging the failures here so the next session doesn't repeat them:

1. **"Tolerate transient recv errors with `continue`".** Created the
   tight hot loop described above. Reverted in `ada4f6a`. Lesson: the
   recv error is not transient — it fires on every call, so you can't
   simply ignore it. (Also obsoleted by `f1573bd` — the recv error no
   longer fires at all.)

2. **Increasing the recv buffer to 64 KiB.** Correct and shipped in
   `51002f8` — but the spurious-wake bug fired before any submit ever
   reached audio_server, so this fix alone wasn't observable until
   `f1573bd` landed.

3. **Investigating whether `MAX_BULK_LEN` (kernel side, 65536) caps
   the buf_len.** The kernel does NOT validate buf_len in
   `ipc_recv_msg`; the only cap is on the send path.

4. **Hypotheses A and B from the original analysis** (IRQ shim drains
   bits during wake / stale `woken` flag). Codex confirmed both
   wrong — the IRQ shim does not drain bits, and
   `block_current_on_notif_v2` constructs a fresh `AtomicBool` per
   call. The actual race (hypothesis C) was a stale ISR wake token in
   `isr_wake_queue` that survived the fast-path drain.

## Concrete next-step plan (AC'97 IRQ pipeline)

The spurious-wake bug is closed. The new question is: **why doesn't
the AC'97 controller raise an IOC interrupt after the BDL is
programmed and `LVI` is advanced?** Candidates in priority order:

### Hypothesis I — `CR.RPBM` is being cleared / never set on the right register

`Ac97Backend::init` (`userspace/audio_server/src/device.rs:721-767`)
calls `init_controller(&bus)` then `open_pcm_out_stream(&bus,
bdl_iova)`. The latter writes `CR.RPBM | LVBIE | FEIE | IOCE` to
`nabm::PCM_OUT_BASE + nabm::CR`. Verify by:

1. Add a debug write that reads back `CR` after the write completes
   and logs the value to STDOUT_FILENO. If `RPBM` is not set, the
   write never landed (PIO routing bug? wrong BAR? wrong base?).
2. Read `GLOB_STA` (NABM offset 0x30) after init and check the
   PCM-OUT half-empty / interrupt status bits — they should clear
   immediately after a successful start.

### Hypothesis II — BDL `IOC` flag isn't reaching the controller

`submit_buffer` in `Ac97Logic` (`device.rs:615-640`) writes
`flags: 0x8000` (IOC bit) into the per-slot BDL entry. Then
`submit_frames_inner` mirrors that descriptor into the DMA-backed
`bdl_dma[head]` (`device.rs:528`). Verify by:

1. After `submit_frames_inner` writes a slot, read back `bdl_dma[head]`
   and log `flags`. Should be `0x8000`.
2. Read `nabm::PCM_OUT_BASE + nabm::CIV` (current index value) from
   the controller — does it advance past the slot you posted? If yes,
   the controller IS consuming buffers but not raising IRQs (suggests
   IOC flag is being lost between userspace and the device — DMA-
   coherency or IOMMU translation bug). If no, the controller never
   sees the BDL update at all.

### Hypothesis III — IRQ delivered but bound notification not signalled

The kernel routes legacy INTx line 10 to vector 0x62 (visible in
m3os.log: `[apic] I/O APIC: PCI IRQ 10 → GSI 10 (pin 10) → vector
98 (level, active-low)`). The ISR shim for vector 0x62 should call
`signal_irq_bit` for the bound notification. Verify by:

1. Check kernel logs for any line mentioning vector 0x62 firing. With
   QEMU's `wav` audio backend, the controller should consume a slot
   every ~22 ms (one BDL slot at 48 kHz stereo S16Le).
2. Add a per-vector IRQ counter that gets dumped on serial
   periodically. If vector 0x62's count is 0 after audio-demo runs,
   the IRQ never fires (Hypothesis I or II). If non-zero but
   audio_server doesn't wake, the bound-notification signal path is
   dropping it (Hypothesis III).

### Hypothesis IV — QEMU's AC'97 device under `-audiodev wav` doesn't generate IRQs

QEMU's `none` audio backend famously does not pull samples from the
controller, so no IRQs fire. The `wav` backend writes samples to a
file but it's worth verifying that QEMU's `intel-hda` / `AC97`
emulation actually raises interrupts under `wav`. A quick experiment:

1. Run audio-smoke under `-audiodev wav,id=snd0,...` (current default).
2. Compare against a brief test under `-audiodev pa,id=snd0` (if a
   PulseAudio host is available).
3. If `pa` works and `wav` doesn't, switch the audio-smoke harness to
   `pa` (or add a `null,timer-period=...` fallback) and document.

### Step-by-step plan

1. Run `M3OS_SMOKE_SERIAL_DUMP=/tmp/serial.log cargo xtask audio-smoke`
   and grep the dump for any `[apic]` or `irq.subscribe` messages
   timed *after* `AUDIO_DEMO:opened`. If none, IRQ never fires →
   Hypothesis I / II / IV.
2. Add the per-vector IRQ counter dump (kernel/src/arch/x86_64/interrupts.rs).
3. Pick the matching hypothesis and patch.

### Backstop in audio_server (still open)

Even after the IRQ pipeline is fixed, `audio_server` should not crash
on the first recv error if a future regression brings the spurious-
wake class back. Consider adding a small consecutive-error counter
that tolerates up to N (~3) errors before exiting. Avoid the tight-
loop pitfall (no allocation in the error branch; yield between
retries). See the discussion in `ada4f6a`'s commit message.

## Original plan — kept for historical context

The plan below was written for the spurious-wake bug, **which is now
fixed by `f1573bd`**. Preserved here in case the symptom resurfaces
under a different repro (e.g. a new ring-3 driver class with long
idle periods).

<details>
<summary>Original five-step plan (obsolete — keep collapsed)</summary>

The right starting point is **kernel-side instrumentation**, not more
userspace adjustments.

#### Step 1 — capture full serial output, not just the trace ring

Add `M3OS_SMOKE_SERIAL_DUMP=/tmp/serial.log` to your audio-smoke
invocation. The default smoke harness only prints the trace ring on
failure (last 256 events per core); the actual kernel serial output
(audio_server's "recv failed", per-IRQ logs, scheduler diagnostics) is
discarded. The wired-up env var lives in `xtask/src/main.rs:3680`.

#### Step 2 — instrument the spurious-wake path

Add a temporary log line at
`kernel/src/ipc/endpoint.rs:695` just before the
`debug_assert!(false, "[ipc] recv_msg_with_notif: spurious wake")`.
Capture: receiver TaskId, ep_id, notif_id, the value returned by
`block_current_on_notif_v2`, what the woken flag was at entry vs.
exit, whether `unregister_recv_waiter` returned anything useful.

#### Step 3 — verify with a unit test on `kernel-core`

`kernel-core/src/sched_model.rs` has the host-testable scheduler state
machine. Add a test that drives `BlockedOnNotif × wake` with the
condition (message + bits) both empty at the moment `block_current` is
called.

#### Step 4 — fix the race

Either a per-task spinlock around block/wake (Linux `pi_lock` model)
or a single state-word + condition recheck (Linux `try_to_wake_up`).
The 2026-04-25 doc recommends the second.

#### Step 5 — backstop in audio_server

See the "Backstop" item in the new plan above.

</details>

## Files to read first (in this order — for the AC'97 IRQ pipeline bug)

1. **This document.**
2. `userspace/audio_server/src/device.rs:120-260, 366-410, 700-870` —
   AC'97 register definitions (`nabm` module), `init_controller`,
   `open_pcm_out_stream`, `Ac97Backend::init`, and the trait impls
   for `submit_frames` / `handle_irq` / `close_stream`.
3. `userspace/audio_server/src/device.rs:474-534` —
   `submit_frames_inner`: the hot path that copies PCM into the
   DMA ring and mirrors BDL descriptors with `IOC=0x8000`.
4. `userspace/audio_server/src/irq.rs:174-260` — `subscribe_and_bind`
   and `run_io_loop` — what audio_server actually does on a wake.
5. `kernel/src/syscall/device_host.rs:1468-1815` —
   `sys_device_irq_subscribe`, vector allocation, `bind_irq_vector`,
   `release_irq_bindings_for_pid`. This is where the legacy INTx
   line gets wired into vector 0x62.
6. `kernel/src/arch/x86_64/apic.rs` (`route_pci_irq`) and
   `kernel/src/arch/x86_64/interrupts.rs` (the ISR shim that
   calls `signal_irq_bit`).
7. `kernel/src/ipc/notification.rs` (now updated by `f1573bd`) —
   `signal_irq_bit`, the per-core `isr_wake_queue`, and the new
   `bound_pending_bits_for_task` helper used by the f1573bd guard.

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
- `cargo xtask run-gui` boots cleanly; `audio_server` stays alive past
  30s of idle (already true post-`f1573bd`).
- `frames_consumed > 0` is observable via `audio-stats` from the
  shell. **(Currently 0 — this is the open work.)**
- The recorded WAV file (`target/audio-smoke/audio.wav`) has at least
  5% of samples with `|sample| > 100` (the existing audio-smoke
  acceptance criterion).
- AC'97 IRQ vector 0x62 fires at least once per second of playback
  (verifiable via the per-vector counter from "Step 2" of the new plan).
