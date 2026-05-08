# Phase 63 — Audio Stack Implementation: Task List

**Status:** Planned
**Source Ref:** phase-63
**Depends on:** Phase 57 (Audio and Local Session) ✅, Phase 55a (IOMMU Substrate) ✅, Phase 55b (Ring-3 Driver Host) ✅, Phase 55c (Ring-3 Driver Correctness Closure) ✅
**Goal:** Replace the Phase 57 `Ac97Backend` accounting stub with real AC'97 NABM register writes so PCM frames reach hardware; extend `cargo xtask audio-smoke` to assert frame consumption via a hardware position counter; wire the BEL byte in `term` to an audible tone; preserve the Phase 57 single-client EBUSY policy throughout.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | AC'97 NABM register driver writes: BDL setup, LVI advance, CIV read, DMA start/stop | None | Planned |
| B | Ring buffer + DMA staging: copy client frames into DMA region, underrun handling | A | Planned |
| C | Timing + frame delivery: IRQ handler, counter wiring, latency check | B | Planned |
| D | `audio-smoke` gate: assert `FrameCounter` advancement end-to-end | C | Planned |
| E | Audible-bell smoke: `term` BEL byte → `audio_client::bell()` → PCM frames | D | Planned |
| F | Phase 57 design doc + task doc closure note | E | Planned |
| G | Multi-client EBUSY regression guard | A | Planned |

---

## Track A — AC'97 NABM Register Driver Writes

### A.1 — Audit BAR1 MMIO region and verify NABM register map

**File:** `userspace/audio_server/src/device.rs`
**Symbol:** `Ac97Backend::init`
**Why it matters:** The NABM PCM-out registers must be at the correct offsets within BAR1 before any BDL setup can be correct.

**Acceptance:**
- [ ] A compile-time constant table maps each used NABM register (BDBAR, CIV, LVI, SR, PICB, CR) to its byte offset within BAR1.
- [ ] `init` asserts at startup that the MMIO region size is at least 256 bytes (the full NABM block).
- [ ] At least two unit tests in `kernel-core/audio/nabm.rs` verify offset values against the AC'97 spec.

### A.2 — Implement BDL setup and DMA start

**File:** `userspace/audio_server/src/device.rs`
**Symbol:** `Ac97Backend::start_pcm_out`
**Why it matters:** Without writing BDBAR and asserting the RUN bit the AC'97 engine never fetches samples.

**Acceptance:**
- [ ] `start_pcm_out` writes the DMA page physical address to NABM `PCM_OUT_BDBAR`.
- [ ] 32 BDL entries are populated with correct sample-count and IOC bits before BDBAR is written.
- [ ] LVI is set to 31 (last valid index) and the CR RUN bit is asserted.
- [ ] `cargo xtask test --test audio_nabm_init` passes in QEMU.

### A.3 — Implement CIV read and LVI advance

**File:** `userspace/audio_server/src/device.rs`
**Symbol:** `Ac97Backend::retire_completed`, `Ac97Backend::advance_lvi`
**Why it matters:** Progress through the BDL requires the driver to read CIV and move LVI forward.

**Acceptance:**
- [ ] `retire_completed(civ)` computes how many descriptors were consumed since the last call and returns the count.
- [ ] `advance_lvi` writes the new LVI value back to the NABM register.
- [ ] A unit test covers the wraparound case (CIV wraps from 31 to 0).

---

## Track B — Ring Buffer and DMA Staging

### B.1 — Connect PCM ring buffer to DMA region

**File:** `userspace/audio_server/src/stream.rs`
**Symbol:** `PcmStream::submit_frames`
**Why it matters:** Client-submitted frames must be DMA-coherently copied into the descriptor ring before LVI is advanced.

**Acceptance:**
- [ ] `submit_frames` copies bytes from the client buffer into the DMA page at the correct BDL slot offset.
- [ ] The copy respects the 64-byte alignment required per BDL entry.
- [ ] At least three property tests in `kernel-core/audio/ring.rs` verify buffer-full, buffer-empty, and wraparound cases.

### B.2 — Implement underrun handling

**File:** `userspace/audio_server/src/stream.rs`
**Symbol:** `PcmStream::handle_underrun`
**Why it matters:** Without underrun recovery the DMA engine halts permanently on a missed deadline.

**Acceptance:**
- [ ] When status register shows BCIS and the software ring is empty, `handle_underrun` zero-fills one descriptor and re-asserts RUN.
- [ ] An underrun event increments a named counter in `FrameCounter`.
- [ ] A log event at WARN level is emitted naming the underrun and the current LVI/CIV values.

---

## Track C — Timing and Frame Delivery

### C.1 — Add `FrameCounter` to `kernel-core`

**File:** `kernel-core/src/audio/counters.rs`
**Symbol:** `FrameCounter`
**Why it matters:** The `audio-smoke` gate needs a counter it can read between two time points to verify hardware consumption.

**Acceptance:**
- [ ] `FrameCounter` is a `no_std`-compatible struct with `frames_consumed: u64` and `underruns: u32`.
- [ ] Accessible through the `audio_server` debug IPC verb `DebugFrameCounter`.
- [ ] At least one unit test verifies atomic increment semantics under simulated concurrent access.

### C.2 — Wire IRQ handler to retire descriptors and increment `FrameCounter`

**File:** `userspace/audio_server/src/irq.rs`
**Symbol:** `AudioIrqHandler::handle`
**Why it matters:** Without IRQ-driven retirement the counter never advances and frames back up.

**Acceptance:**
- [ ] `handle` reads CIV, calls `retire_completed`, increments `FrameCounter::frames_consumed` by the count.
- [ ] `handle` clears the NABM status register BCIS bit to acknowledge the interrupt.
- [ ] Handler executes in bounded time: no allocation, no blocking.

---

## Track D — `audio-smoke` Gate Frame-Consumption Assertion

### D.1 — Extend `cargo xtask audio-smoke` to sample `FrameCounter`

**Files:**
- `xtask/src/main.rs`
- `userspace/audio_server/src/protocol.rs`

**Symbol:** `smoke_audio` (xtask), `DebugFrameCounter` (protocol verb)
**Why it matters:** The gate must fail when PCM frames do not reach hardware, not merely when `audio_server` starts.

**Acceptance:**
- [ ] `audio-smoke` sends a 500 ms 440 Hz test tone via `audio-demo`, then queries `DebugFrameCounter`.
- [ ] Gate fails with a clear error if `frames_consumed` delta is zero during the window.
- [ ] Gate passes against the real implementation and fails against the Phase 57 stub (confirmed by running both).

---

## Track E — Audible-Bell Smoke

### E.1 — Wire `term` BEL byte to `audio_client::bell()`

**File:** `userspace/term/src/ansi.rs`
**Symbol:** `AnsiParser::handle_bel`
**Why it matters:** The BEL character is the only user-visible audio path in `term`; if it is a no-op the audio stack has no in-session test vector.

**Acceptance:**
- [ ] `handle_bel` calls `audio_client::bell()` which submits a 440 Hz, 50 ms PCM tone.
- [ ] If `audio_client` returns `-EBUSY` or `-ENODEV`, `handle_bel` logs at DEBUG level and returns without error.
- [ ] `audio-smoke` sub-test sends `\x07` to `term`'s PTY and asserts `FrameCounter` advances within 200 ms.

---

## Track F — Phase 57 Documentation Closure

### F.1 — Update Phase 57 design doc with closure note

**File:** `docs/roadmap/57-audio-and-local-session.md`
**Symbol:** (document section)
**Why it matters:** The audit surfaced that Phase 57 was declared Complete with a stub audio path; the design doc must record the accurate state and the phase that closed the gap.

**Acceptance:**
- [ ] A `> **Phase 63 closure note:**` block is added to the `## Deferred Until Later` section stating that real NABM writes and frame-consumption verification were delivered by Phase 63.
- [ ] The `audio-smoke` gate description in the Phase 57 doc is updated to reference the new assertion.

### F.2 — Update Phase 57 task doc Track H acceptance items

**File:** `docs/roadmap/tasks/57-audio-and-local-session-tasks.md`
**Symbol:** Track H
**Why it matters:** H.1 (audio smoke) carried a false-passing acceptance criterion; it must be corrected with the Phase 63 closure reference.

**Acceptance:**
- [ ] H.1 acceptance item updated to note "extended in Phase 63 to assert `FrameCounter` advancement".
- [ ] No other Phase 57 acceptance items changed.

---

## Track G — Multi-Client EBUSY Regression Guard

### G.1 — Add regression test for second-client EBUSY

**File:** `userspace/audio_server/tests/multi_client.rs`
**Symbol:** `test_second_client_ebusy`
**Why it matters:** Phase 57 established single-client policy; real NABM writes must not accidentally remove the arbitration check.

**Acceptance:**
- [ ] Test opens one `audio_client` connection (succeeds), opens a second (expects `-EBUSY`).
- [ ] Test runs via `cargo xtask test --test audio_multi_client` in QEMU.

---

## Documentation Notes

- `kernel-core/src/audio/counters.rs` is a new file; place it alongside the existing `kernel-core/src/audio/` modules established in Phase 57.
- The `DebugFrameCounter` IPC verb is debug-only and should be feature-gated or gated on a kernel build flag rather than exposed in production builds.
- Phase 57 design and task docs receive closure notes only — no substantive content is removed or restructured.
