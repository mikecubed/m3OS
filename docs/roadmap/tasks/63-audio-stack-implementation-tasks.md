# Phase 63 — Audio Stack Implementation: Task List

**Status:** Complete
**Source Ref:** phase-63
**Depends on:** Phase 57 (Audio and Local Session) ✅, Phase 55a (IOMMU Substrate) ✅, Phase 55b (Ring-3 Driver Host) ✅, Phase 55c (Ring-3 Driver Correctness Closure) ✅
**Goal:** Make the Phase 57 audio stack actually emit PCM to hardware so a user can hear the BEL bell and `audio-demo` tone. Replace the `cfg(not(test)) Ac97Backend` accounting stub in `userspace/audio_server/src/device.rs:559-664` with a real backend that drives the existing Phase 57 register-poking helpers (`init_controller`, `open_pcm_out_stream`, `handle_pcm_out_irq`, `Ac97Logic`) over a new privileged PIO syscall path; switch the QEMU launchers to a real `audiodev` backend; and extend `cargo xtask audio-smoke` to assert frame consumption end-to-end via the existing `AudioControlCommand::GetStats` verb.

## Context: what Phase 57 already shipped

Track A.1 / A.3 / B.1 / B.2 / C.1 / C.2 / G.1 from earlier drafts of this plan are **not Phase 63 work** — Phase 57 D.2/D.3/D.4/D.5/G.6 already landed them. Reusing what exists is mandatory; do not add a parallel `FrameCounter`, `DebugFrameCounter`, or `kernel-core/audio/nabm.rs`.

| Already shipped (do not redo) | Location |
|---|---|
| NAM/NABM register-offset table, `sr_bits`, `cr_bits`, `BufferDescriptor`, `BDL_ENTRIES` | `userspace/audio_server/src/device.rs:99-203` |
| Pure register-write helpers (`init_controller`, `open_pcm_out_stream`, `close_pcm_out_stream`, `handle_pcm_out_irq`) over an `MmioOps` seam | `userspace/audio_server/src/device.rs:332-401` |
| BDL ring math + frames-consumed/underrun counters (`Ac97Logic::submit_buffer`, `observe_irq`) | `userspace/audio_server/src/device.rs:413-536` |
| Single-stream registry, BUSY-on-second-open, drain timeout | `userspace/audio_server/src/stream.rs` |
| Single-client admission (BUSY-on-second-connect) with rate-limited reject log + 13 host tests | `userspace/audio_server/src/client.rs` |
| Bound-notification IRQ loop + `apply_irq_event` translation to underrun stats | `userspace/audio_server/src/irq.rs:191-332` |
| Stats verb returning `frames_submitted`, `frames_consumed`, `underrun_count` | `kernel-core/src/audio/protocol.rs` (`AudioControlCommand::GetStats`, `AudioControlEvent::Stats`); wired in `irq.rs::encode_outcome` |
| BEL → `RenderCommand::Bell` → `Bell::ring` → `AudioClientBellSink` → `audio_client::submit_frames` | `userspace/term/src/screen.rs:185-188`, `userspace/term/src/main.rs:212`, `userspace/term/src/bell.rs:205-265` |
| Ring-buffer property tests | `kernel-core/src/audio/ring_proptest.rs` |
| Multi-client EBUSY contract host tests | `userspace/audio_server/src/client.rs::tests` |

## What Phase 63 actually has to do

1. Add a privileged PIO syscall family so userspace drivers can read/write I/O-space BARs (Track Z). AC'97's two BARs are I/O-space only (`kernel-core/src/device_host/audio_class.rs::AC97_BAR_LAYOUT.is_pio_only() == true`); the existing `sys_device_mmio_map` filters PIO BARs and rejects them.
2. Wire the production `Ac97Backend` to the existing pure helpers over PIO + DMA buffers (Track A).
3. Connect client-submitted PCM bytes to the DMA region and advance LVI per submission (Track B).
4. Switch QEMU `-audiodev` from `none` to a real backend so frames produce audible output and the smoke gate can record a WAV (Track C).
5. Extend `audio-smoke` to assert `frames_consumed` advances via `GetStats`, and (under WAV backend) that the recorded file is non-silent (Track D).
6. Verify the existing BEL path lights up end-to-end against the real backend (Track E).
7. Phase 57 closure notes + Phase 63 design/release docs (Tracks F, H).

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| Z | Privileged PIO syscall + `Pio<T>` driver_runtime wrapper + AC'97 PIO `MmioOps` impl | None | Complete |
| A | Replace `cfg(not(test)) Ac97Backend` stub with real BDL/PCM-ring DMA + register init | Z | Complete |
| B | `submit_frames` copies into PCM ring, posts BDL entry, advances LVI through PIO | A | Complete |
| C | QEMU `-audiodev` selection: PulseAudio for `run-gui`, WAV for `audio-smoke` | None | Complete |
| D | `audio-smoke` asserts `frames_consumed` advance via `GetStats`, plus non-silent WAV | A, B, C | Complete |
| E | End-to-end BEL/audio-demo verification against real backend | A, B, C | Complete |
| F | Phase 57 design + task doc closure notes | D, E | Complete |
| H | Phase 63 design + release wiring (kernel version bump, learning doc) | F | Complete |

---

## Track Z — Privileged PIO Syscall + `Pio<T>` Wrapper

### Z.1 — Add `SYS_DEVICE_PIO_READ` / `SYS_DEVICE_PIO_WRITE` syscall numbers

**File:** `kernel-core/src/device_host/syscalls.rs`
**Symbol:** `SYS_DEVICE_PIO_READ`, `SYS_DEVICE_PIO_WRITE`, `DEVICE_HOST_LAST`
**Why it matters:** The Phase 55b syscall block ends at `0x1124` (`SYS_DEVICE_IRQ_SUBSCRIBE`). New numbers must be appended without renumbering and `DEVICE_HOST_LAST` updated, per the comment at line 12 of that file.

**Acceptance:**
- [ ] `SYS_DEVICE_PIO_READ = 0x1125` and `SYS_DEVICE_PIO_WRITE = 0x1126` declared as `pub const`.
- [ ] `DEVICE_HOST_LAST` updated to `SYS_DEVICE_PIO_WRITE`.
- [ ] Existing constants pin-tests in `syscalls.rs::tests` extended to include the two new numbers without changing the prior values.
- [ ] `cargo test -p kernel-core` passes.

### Z.2 — Implement kernel-side `sys_device_pio_read` / `sys_device_pio_write`

**File:** `kernel/src/syscall/device_host.rs`
**Symbol:** `sys_device_pio_read`, `sys_device_pio_write`
**Why it matters:** Userspace cannot issue `inb`/`outb` (privileged). The kernel must validate the caller's `Capability::Device`, the BAR index points at a PIO BAR, and the offset+width is in range, then issue the port I/O on the caller's behalf.

**Acceptance:**
- [ ] `sys_device_pio_read(dev_cap, bar_index, offset, width) -> isize` returns the value zero-extended into the low bits, or a negative errno.
- [ ] `sys_device_pio_write(dev_cap, bar_index, offset, value, width) -> isize` returns 0 on success or a negative errno.
- [ ] `width` is one of `1`, `2`, `4` bytes; any other value returns `-EINVAL`.
- [ ] Returns `-EBADF` if `dev_cap` is not a `Capability::Device` owned by the caller; `-EINVAL` if the BAR is not PIO; `-ERANGE` if `offset + width > BAR_SIZE`.
- [ ] At least four kernel-core unit tests cover: valid 8/16/32-bit access, mismatched width, MMIO-BAR rejection, and out-of-range offset rejection.
- [ ] No allocation in the syscall path; no logging on the hot path.

### Z.3 — Add userspace `Pio<T>` wrapper in `driver_runtime`

**File:** `userspace/lib/driver_runtime/src/pio.rs` (new), `userspace/lib/driver_runtime/src/lib.rs`
**Symbol:** `Pio<T>`, `Pio::map`, `Pio::read_u{8,16,32}`, `Pio::write_u{8,16,32}`
**Why it matters:** The existing `MmioOps` trait in `userspace/audio_server/src/device.rs:217-230` declares the surface drivers consume. Phase 63 needs a production type that implements that surface against the new PIO syscalls; tests already use a `FakeMmio`.

**Acceptance:**
- [ ] `Pio<T>` is constructed via `Pio::<T>::map(&DeviceHandle, bar_index)` and stores the device-cap handle plus the BAR index.
- [ ] `read_u8/16/32(offset)` and `write_u8/16/32(offset, value)` route through the Z.2 syscalls.
- [ ] Re-exported as `pub use pio::Pio;` from `lib.rs`.
- [ ] `Drop` is a no-op (PIO has no MMIO mapping to release; the device-cap handle owns the lifetime).
- [ ] At least one host-test stub exercises the contract surface against a `PioContract` trait double, mirroring the `MmioContract` shape in `kernel_core::driver_runtime::contract`.

### Z.4 — Implement `MmioOps` for `Pio<()>` so AC'97 helpers compile against real hardware

**File:** `userspace/audio_server/src/device.rs`
**Symbol:** `impl device::MmioOps for driver_runtime::Pio<()>`, plus a small adapter that holds two `Pio<()>` (one per BAR) and dispatches by `bar` parameter
**Why it matters:** `init_controller<M: MmioOps>(mmio: &M)` and friends are already polymorphic over `MmioOps`. The adapter is the seam where the production `Ac97Backend` plugs in PIO without changing any pure helper.

**Acceptance:**
- [ ] A new `Ac97PioBus` type (or named adapter) holds `Pio<()>` for `BAR_NAM` and `BAR_NABM` and dispatches `MmioOps` calls by `bar` parameter to the right handle.
- [ ] `Ac97PioBus::new(&DeviceHandle)` performs both `Pio::map` calls.
- [ ] All existing `device.rs` tests still pass (the `FakeMmio` path is unchanged).
- [ ] The adapter exposes no shared state between BARs — each method dispatches strictly on `bar`.

---

## Track A — Real `Ac97Backend` Backed by Hardware

### A.1 — Replace stub fields with real DMA + PIO state

**File:** `userspace/audio_server/src/device.rs`
**Symbol:** `Ac97Backend` (the `cfg(not(test))` definition at lines 559–567)
**Why it matters:** Today the backend owns only `device: DeviceHandle` plus 4 accounting counters. None of the existing pure helpers can fire without DMA buffers and a PIO bus.

**Acceptance:**
- [ ] `Ac97Backend` owns: `device: DeviceHandle`, `bus: Ac97PioBus`, `bdl: DmaBuffer<[BufferDescriptor; BDL_ENTRIES]>`, `pcm_ring: DmaBuffer<[u8; DEFAULT_PCM_RING_BYTES]>`, `logic: Ac97Logic`, `stream_open: bool`.
- [ ] The four standalone accounting counters (`frames_submitted`, `frames_consumed`, `underrun_count`, `initialised`) are removed — `Ac97Logic` already owns them.
- [ ] `StatsSnapshot` is computed from `logic.frames_consumed()` / `logic.underrun_count()` plus a tracked `frames_submitted` that mirrors `Ac97Logic`.
- [ ] `cargo xtask check` passes.

### A.2 — Wire `Ac97Backend::init` to the real reset/open path

**File:** `userspace/audio_server/src/device.rs`
**Symbol:** `Ac97Backend::init`
**Why it matters:** The existing stub at line 579 just sets `initialised = true`. Without `init_controller` + `open_pcm_out_stream` the codec is mute and the BDL never receives BDBAR.

**Acceptance:**
- [ ] `Ac97Backend::init(device)` constructs the `Ac97PioBus`, allocates both `DmaBuffer`s through `sys_device_dma_alloc`, calls `init_controller(&bus)`, then `open_pcm_out_stream(&bus, bdl_iova)` using `bdl.iova()`.
- [ ] Failure at any step releases earlier-allocated DMA caps via `Drop` (no leaked caps on the error path).
- [ ] `AudioBackend::open_stream` no longer toggles a separate `stream_open` flag — the BDL state machine in `Ac97Logic` is the single source of truth.
- [ ] `cargo xtask test --test audio_init_real` (new QEMU test) boots far enough to reach the `AUDIO_SMOKE:server:READY` sentinel with `-device AC97` present.

### A.3 — Wire IRQ handler to read CIV, classify SR, advance counters

**File:** `userspace/audio_server/src/device.rs`
**Symbol:** `Ac97Backend::handle_irq`
**Why it matters:** The existing stub at line 660 returns `IrqEvent::None`. The pure logic exists (`handle_pcm_out_irq` + `Ac97Logic::observe_irq`); A.3 just plugs them in.

**Acceptance:**
- [ ] `handle_irq` reads `nabm::CIV` via `bus.read_u8`, calls `handle_pcm_out_irq(&bus, ring_was_empty)`, then `logic.observe_irq(sr, civ)`.
- [ ] `ring_was_empty` is computed from `logic.head == logic.tail` before the SR read.
- [ ] No allocation, no logging in the IRQ path (still tested by the existing `no_irq_wait_calls_in_audio_server_production_paths` discipline test).
- [ ] On `IrqEvent::FifoError` the function returns `Err(AudioError::Internal)` so the io loop surfaces the error to the open client.

---

## Track B — Frame Submission Through PCM Ring

### B.1 — `submit_frames` copies into the PCM ring DMA region

**File:** `userspace/audio_server/src/device.rs`
**Symbol:** `Ac97Backend::submit_frames`
**Why it matters:** The existing stub at line 635 only advances `frames_submitted` by `bytes.len()` without touching DMA memory or the BDL.

**Acceptance:**
- [ ] `submit_frames(stream_id, bytes)`:
  1. Returns `Err(AudioError::WouldBlock)` if `Ac97Logic::submit_buffer` would reject (BDL full).
  2. Copies bytes into the next free PCM-ring slot at the BDL's head index × slot stride.
  3. Calls `logic.submit_buffer(bdl_iova_offset, slot_phys_addr, samples)` where `samples = bytes.len() / 2` (S16Le).
  4. Writes the new `logic.lvi()` value to `nabm::PCM_OUT_BASE + nabm::LVI` via the bus.
- [ ] The PCM ring is divided into `BDL_ENTRIES` equal slots; partial-slot submissions are not supported in Phase 63 (return `Err(AudioError::InvalidArgument)` with a clear message). A submission larger than one slot is split internally into multiple BDL entries; the function returns the total bytes copied.
- [ ] No allocation per call (asserted by the existing `submit_does_not_allocate_per_call` pattern in `stream.rs::tests`).
- [ ] Existing host tests in `stream.rs` continue to pass (the `FakeBackend` in those tests is unchanged).

### B.2 — Underrun: zero-fill + repost on `IrqEvent::Underrun`

**File:** `userspace/audio_server/src/irq.rs`
**Symbol:** `apply_irq_event` extension; new helper `repost_silence_after_underrun`
**Why it matters:** `apply_irq_event` already records the underrun stat. Without re-arming the BDL, the engine stays halted and the next `submit_frames` looks like a hang.

**Acceptance:**
- [ ] When `IrqEvent::Underrun` fires AND the software ring is empty, the io loop calls `backend.submit_frames(stream_id, &SILENCE_FRAME)` once (where `SILENCE_FRAME` is a const-zeroed buffer matching one BDL slot).
- [ ] The repost path increments `underrun_count` exactly once per underrun event (no double-count between `Ac97Logic::observe_irq` and `apply_irq_event`).
- [ ] One new host test in `irq.rs::tests` exercises: open → simulate `IrqEvent::Underrun` → assert `frames_submitted` advances by exactly one slot's worth of zero bytes.

---

## Track C — QEMU Audio Backend Selection

### C.1 — `run-gui` selects PulseAudio when audio is enabled

**File:** `xtask/src/main.rs`
**Symbol:** `AC97_QEMU_AUDIO_FLAGS`, `append_ac97_audio_flags`, `cmd_run_gui`
**Why it matters:** The current constant at line 50 is `["-audiodev","none,id=snd0", ...]`. With `none` the AC'97 device discards every frame; users hear nothing regardless of how correct the driver is.

**Acceptance:**
- [ ] A new constant `AC97_QEMU_AUDIO_FLAGS_GUI` selects `pa,id=snd0` (PulseAudio) on Linux hosts; the existing `none` constant is renamed to `AC97_QEMU_AUDIO_FLAGS_HEADLESS`.
- [ ] `cmd_run_gui` appends the GUI flags by default and exposes `--no-audio` to fall back to `none`.
- [ ] On non-Linux hosts (detected via `cfg!(target_os = ...)` at xtask compile time, so xtask itself can be cross-compiled for CI) the GUI flag set falls back to `none` with a printed warning so xtask never fails to launch.
- [ ] xtask unit tests pin both flag sets and confirm `cmd_run_gui` emits the GUI variant.

### C.2 — `audio-smoke` selects WAV output for deterministic CI

**File:** `xtask/src/main.rs`
**Symbol:** `audio_smoke_qemu_args`
**Why it matters:** A WAV file gives the smoke gate a second, hardware-independent way to verify "frames actually became audio" without depending on the host PulseAudio daemon.

**Acceptance:**
- [ ] `audio_smoke_qemu_args` emits `-audiodev wav,id=snd0,path=<smoke_dir>/audio.wav` instead of `none`.
- [ ] The smoke harness deletes any prior `audio.wav` before launch and refuses to run if the path is not writable.
- [ ] xtask unit test confirms the WAV path is wired in and the `id=snd0` reference matches `-device AC97`.

---

## Track D — `audio-smoke` Frame-Consumption Assertion

### D.1 — Run `audio-demo` inside the smoke and wait for `AUDIO_DEMO:PASS`

**File:** `xtask/src/main.rs`
**Symbol:** `audio_smoke_steps`
**Why it matters:** Today (line 5653) the smoke only checks `init: loaded service 'audio_server'`. It never invokes the existing `audio-demo` reference client and so cannot detect a regression that breaks the open/submit/drain/close path.

**Acceptance:**
- [ ] After the existing `init: loaded service 'audio_server'` step, `audio_smoke_steps` writes `audio-demo\n` to the kernel's serial console (init's shell can spawn it) and waits for the `AUDIO_DEMO:PASS` sentinel within 30 s.
- [ ] On `AUDIO_DEMO:FAIL stage=...` the smoke exits with `SMOKE_EXIT_AUDIO_DEMO_FAILED` and surfaces the failing stage in the error message.

### D.2 — Assert `frames_consumed > 0` via `GetStats` after the demo runs

**File:** `xtask/src/main.rs`, `userspace/audio-demo/src/main.rs`
**Symbol:** `audio_smoke_steps`, `program_main` (audio-demo)
**Why it matters:** `audio-demo` currently exits 0 without proving the device consumed anything. The existing `AudioControlCommand::GetStats` verb returns `frames_consumed`; the demo can read it just before exit.

**Acceptance:**
- [ ] `audio-demo` issues a `GetStats` request after `drain` succeeds and prints `AUDIO_DEMO:stats consumed=<N> underruns=<M>` before exiting.
- [ ] `audio_smoke_steps` adds a final wait for a regex matching `AUDIO_DEMO:stats consumed=[1-9]\d*` (i.e., non-zero).
- [ ] Acceptance verified by reverting Track A.2 to the stub on a scratch branch and confirming the gate fails with `consumed=0`.

### D.3 — WAV smoke: assert recorded file is non-silent

**File:** `xtask/src/main.rs`
**Symbol:** `cmd_audio_smoke`, new helper `assert_wav_non_silent`
**Why it matters:** A non-zero `frames_consumed` only proves the driver believes it shipped frames. The WAV check proves QEMU's audio backend received them.

**Acceptance:**
- [ ] After QEMU exits, `cmd_audio_smoke` opens the WAV file written under C.2.
- [ ] The helper parses the RIFF/WAVE header, walks the data chunk, and asserts that at least 5% of the samples have `|sample| > 100` (well above WAV-silence noise floor).
- [ ] Failure prints the percentage of non-silent samples observed alongside the `consumed=` count from D.2.

---

## Track E — End-to-end Audio Verification

### E.1 — Verify the existing BEL → bell path emits audible output

**File:** `userspace/term/src/main.rs`, `xtask/src/main.rs`
**Symbol:** `ring_bell`, `cmd_session_smoke` extension
**Why it matters:** The bell wiring (`screen.rs:185-188` → `main.rs:212` → `bell.rs::AudioClientBellSink`) is already complete from Phase 57 G.6. Phase 63 just needs to verify the path advances `frames_consumed` once the real backend is live — no code change in `term` is expected.

**Acceptance:**
- [ ] A new `bell-smoke` xtask step writes `printf '\\x07'\n` to the kernel serial console after `term` is registered, then issues `GetStats` against `audio.cmd` after 200 ms and asserts `frames_consumed > 0`.
- [ ] If `Ac97Backend` ever regresses, the smoke fails with `consumed=0` (verified the same way as D.2: scratch revert).
- [ ] No edits to `userspace/term/src/screen.rs`, `userspace/term/src/bell.rs`, or `userspace/term/src/main.rs` are required by Phase 63. If a change is needed it is a bug; document it in the PR.

### E.2 — Verify `audio-demo` produces audible output under `run-gui`

**File:** `docs/63-audio-stack-implementation.md`
**Symbol:** Manual-smoke checklist section
**Why it matters:** The headless smoke gates the WAV check; the GUI manual smoke is the human-audible confirmation.

**Acceptance:**
- [ ] The Phase 63 design doc records a manual-smoke step: `cargo xtask run-gui` → wait for `term` prompt → run `/bin/audio-demo` → confirm an audible 1-second 440 Hz tone on the host audio device.
- [ ] The same checklist records the BEL test: type `printf '\\x07'` in `term` → confirm a short audible beep.

---

## Track F — Phase 57 Documentation Closure

### F.1 — Phase 57 design doc closure note

**File:** `docs/roadmap/57-audio-and-local-session.md`
**Symbol:** (document section)
**Why it matters:** Phase 57 was declared Complete with an `Ac97Backend` accounting stub; the design doc must record the accurate state and the phase that closed the gap.

**Acceptance:**
- [ ] A `> **Phase 63 closure note:**` block is added to the `## Deferred Until Later` section (or the nearest existing closure-notes section) stating that real PIO + DMA register writes and the WAV/`GetStats` smoke assertion were delivered by Phase 63.
- [ ] The `audio-smoke` gate description (originally pinned at H.1) is updated to reference the new Phase 63 frame-consumption assertion.

### F.2 — Phase 57 task doc Track H acceptance update

**File:** `docs/roadmap/tasks/57-audio-and-local-session-tasks.md`
**Symbol:** Track H (audio-smoke + run-gui audio)
**Why it matters:** H.1 in Phase 57 carried a deliberately false-passing acceptance ("conf-loaded check"); the closure must record where the real assertion now lives.

**Acceptance:**
- [ ] H.1 acceptance items annotated with "extended in Phase 63 to assert `frames_consumed` via `GetStats` plus non-silent WAV output".
- [ ] No other Phase 57 acceptance items changed.

---

## Track H — Documentation and Release

### H.1 — Aligned legacy learning doc

**File:** `docs/63-audio-stack-implementation.md`
**Symbol:** (new document)
**Why it matters:** Learners need a standalone reference that explains real PCM emission via AC'97 PIO without mixing in Phase 57 stub context or Phase 64+ HDA detail.

**Acceptance:**
- [ ] `docs/63-audio-stack-implementation.md` exists with all template fields populated (`**Aligned Roadmap Phase:** Phase 63`, `**Status:** Planned`, `**Source Ref:** phase-63`, `**Supersedes Legacy Doc:** new`).
- [ ] Overview is one learner-friendly paragraph explaining what changed from Phase 57's stub to real PCM emission, with explicit mention of the new PIO syscall and the WAV/`GetStats` smoke checks.
- [ ] Key Files table cites: `kernel/src/syscall/device_host.rs`, `kernel-core/src/device_host/syscalls.rs`, `userspace/lib/driver_runtime/src/pio.rs`, `userspace/audio_server/src/device.rs`, `xtask/src/main.rs`.
- [ ] Related Roadmap Docs links `docs/roadmap/63-audio-stack-implementation.md` and `docs/roadmap/tasks/63-audio-stack-implementation-tasks.md`.
- [ ] A "Manual smoke checklist" section captures Track E.2.

### H.2 — Bump kernel version to 0.63.0

**Files:** `kernel/Cargo.toml`, `Cargo.lock`, `AGENTS.md`, `docs/roadmap/README.md`
**Symbol:** `version` in `kernel/Cargo.toml` `[package]`
**Why it matters:** Project convention is one minor-bump per shipped phase; keeping the version cursor accurate ensures `AGENTS.md` and the README reflect the real state of the kernel at any given phase.

**Acceptance:**
- [ ] `kernel/Cargo.toml` `version = "0.63.0"`
- [ ] `Cargo.lock` regenerated (run `cargo check` or `cargo xtask check` to trigger).
- [ ] `AGENTS.md` "Kernel v0.X.0" reference updated to `v0.63.0`.
- [ ] `cargo xtask check` passes after the bump.
- [ ] Git tag `v0.63.0` recommended at phase merge.

---

## Documentation Notes

- The new PIO syscall is privileged: only processes holding a `Capability::Device` for a BAR may issue `inb`/`outb` on that BAR. This preserves the Phase 55b ring-3 driver-host invariant (no userspace can issue arbitrary port I/O).
- No new `kernel-core/audio/counters.rs` module is created. The existing `Ac97Logic` counters plus `AudioControlEvent::Stats` cover the smoke gate's needs; adding a parallel `FrameCounter` would duplicate state across two modules and split the host-test surface.
- The `DebugFrameCounter` IPC verb proposed in earlier drafts is not introduced. The existing `AudioControlCommand::GetStats` returns the same data and is already wired through `dispatch_message` / `encode_outcome`.
- Phase 57 design and task docs receive closure notes only — no substantive content is removed or restructured.
- The Phase 57 `bell.rs` already emits an 880 Hz / 30 ms square wave (not 440 Hz / 50 ms as earlier drafts of this plan proposed); Phase 63 does not change the tone shape.
