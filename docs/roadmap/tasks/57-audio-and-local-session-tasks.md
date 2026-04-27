# Phase 57 — Audio and Local Session: Task List

**Status:** Planned
**Source Ref:** phase-57
**Depends on:** Phase 22b (ANSI Escape) ✅, Phase 27 (User Accounts) ✅, Phase 29 (PTY Subsystem) ✅, Phase 47 (DOOM) ✅, Phase 55 (Hardware Substrate) ✅, Phase 55a (IOMMU Substrate) ✅, Phase 55b (Ring-3 Driver Host) ✅, Phase 55c (Ring-3 Driver Correctness Closure) ✅, Phase 56 (Display and Input Architecture) ✅
**Goal:** Turn the Phase 56 graphical architecture into a coherent local-system milestone by adding (1) the first supported PCM audio output path on a chosen target, exposed as a typed userspace contract; (2) a defined graphical session entry flow with an explicit recovery path back to a text-mode administration shell; and (3) at least one genuinely useful graphical client — a `term` emulator wired to the existing PTY subsystem — so the local session feels like a system, not a demo. Kernel version bumps to `v0.57.0` once the phase lands.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Architecture: audio target choice, session topology, capability map, ABI design memos | None | Done |
| B | `kernel-core` audio pure-logic: PCM format types, ring-buffer state model, protocol codec, property tests | A | Done |
| C | Kernel substrate: audio device claim path through `device_host`, IOMMU BAR coverage for the audio device, vsync-equivalent buffer-empty notification | A, B | Planned |
| D | `audio_server` ring-3 driver: chosen-target controller init, PCM stream submission, IRQ multiplexing via Phase 55c bound notifications, single-client arbitration, service manifest | C | Planned |
| E | Audio client surface: `userspace/lib/audio_client` library, `audio-demo` reference client (plays a documented test tone) | D | Planned |
| F | `session_manager` daemon: graphical session startup ordering, fallback-to-text-mode administration on failure, supervisor integration | A.4 (consumes contract); parallel to B–E in build order; D.6 manifest order is consumed at runtime by F.2; Phase 56 outputs at runtime | Planned |
| G | `term` graphical terminal emulator: display-server client, bitmap font renderer, PTY connection, ANSI parser reuse, service manifest | D.6, E.1, F.2 (and Phase 22b / 29 / 56 outputs) | Planned |
| H | Validation: audio smoke, session entry smoke, recovery smoke, multi-client audio policy, xtask plumbing, manual `run-gui` checklist | D, E, F, G | Planned |
| I | Documentation + version: learning doc, subsystem doc updates, evaluation doc updates, roadmap README + status flip, version bump to `0.57.0` | H | Planned |

---

## Engineering Discipline and Test Pyramid

These are preconditions for every code-producing task in this phase. A task cannot be marked complete if it violates any of them. Where a later task re-states a rule for emphasis, the rule here is authoritative.

### Test-first ordering (TDD)

- Tests for every code-producing task commit **before** the implementation that makes them pass. Git history for the touched files must show failing-test commits preceding green-test commits. "Tests follow" is not acceptable.
- Acceptance lists that say "at least N tests cover ..." name *minimums*. If the implementation reveals a new case, add the test before closing the task.
- Red-Green-Refactor: the third step is not optional. Once a task's tests pass, do one explicit refactor pass on the touched code (extract helpers, collapse duplication, tighten visibility) and re-run the same tests before opening the PR.
- A task is not complete until every test it names runs green via `cargo test -p kernel-core --target x86_64-unknown-linux-gnu`, `cargo test -p audio_client --target x86_64-unknown-linux-gnu`, or `cargo xtask test`.

### Test pyramid

| Layer | Location | Runs via | Covers |
|---|---|---|---|
| Unit | `kernel-core/src/audio/` and `kernel-core/src/session/` | `cargo test -p kernel-core --target x86_64-unknown-linux-gnu` | Pure logic: PCM format math, ring-buffer state transitions, audio protocol codec, session startup ordering, font glyph lookup |
| Contract | `kernel-core` shared harness | Same | Traits with ≥2 implementations (`AudioSink`, `SessionStep`, `FontProvider`) pass the same behavioral test suite against every impl |
| Property | `kernel-core` with `proptest` (available from Phase 43c) | Same | Codec round-trip (`decode(encode(x)) == x`) for every audio-protocol message variant; ring-buffer invariants under arbitrary write/consume interleavings; session-step ordering invariants |
| Integration | `userspace/audio_server/tests/`, `userspace/session_manager/tests/`, `userspace/term/tests/` | `cargo xtask test` (QEMU) | End-to-end: client connect → PCM submit → audio-out observed; boot → graphical session → terminal usable; display-server crash → text-mode fallback observed |
| Smoke | `cargo xtask audio-smoke`, `cargo xtask session-smoke` | xtask harness (QEMU) | A scripted tone is audible (or its driver-level "frames consumed" counter advances under `-audiodev none,id=`); a scripted session boot reaches `term` and accepts a keypress |

Pure logic belongs in `kernel-core`. Hardware and IPC wiring belongs in `kernel/` or `userspace/`. Tasks that straddle the boundary split their code along it so the pure part is host-testable; no task may defer this split to "later".

### SOLID and module boundaries

- **Single Responsibility.** Modules under `userspace/audio_server/src/` each own one concern: `device.rs` → MMIO + DMA setup, `stream.rs` → PCM stream state machine, `client.rs` → per-client protocol state, `irq.rs` → Phase 55c bound-notification multiplex loop. Modules under `userspace/session_manager/src/` separate `boot.rs` (startup ordering), `recover.rs` (fallback policy), and `control.rs` (signals from supervisor). Modules under `userspace/term/src/` separate `font.rs`, `pty.rs`, `render.rs`, and `input.rs`. No module accesses another's internal state directly; cross-module data flows only through typed function calls or trait objects.
- **Open / Closed and Dependency Inversion.** Public extension seams are named traits — `AudioSink` (B.2), `SessionStep` (F.1), `FontProvider` (G.2), `AudioBackend` (D.2) — and consumers depend on the trait, not the concrete type. Adding a second audio backend (e.g., HDA after AC'97), a second session step, or a second font lands by implementing the trait, not by editing callers.
- **Interface Segregation.** The audio-client surface exposes PCM submit + drain only. Device claim, DMA buffer plumbing, and IRQ binding are not visible to clients. Session control verbs (graceful stop, force-restart) live on a separate control endpoint, not on the audio or display protocol.
- **Liskov Substitution.** Every impl of a trait defined here passes the shared contract-test suite for that trait. Impls that need escape hatches document the exact invariants they relax in module-level docs.

### DRY

- PCM format types (`PcmFormat`, `SampleRate`, `ChannelLayout`), audio-protocol message types, and audio errno values live **once** in `kernel-core::audio`. `audio_server`, `audio_client`, `audio-demo`, `term` (for terminal bell), and any future media client consume the same definitions; a workspace-wide grep for any of these symbols must return exactly one declaration site.
- Session-step ordering (display → input → audio → term) lives **once** in `kernel-core::session::startup` as a typed sequence; `session_manager` consumes it. Service-manifest authors do not redeclare ordering separately.
- Bitmap glyph data is centralized in `kernel-core::session::font` (or a small data crate it owns) for every Phase 57 client (`term`, future graphical clients). The existing `kernel/src/fb/mod.rs` framebuffer-console font (IBM CP437 8×16, ASCII 0x20–0x7E) stays in place for Phase 57 — migrating the kernel framebuffer console to consume the new `kernel-core` font is an explicitly deferred follow-up so this phase does not gain a kernel refactor it does not need. New duplication introduced during Phase 57 is consolidated in the same PR, not deferred.
- `*_server` startup boilerplate (endpoint creation + registry registration + IRQ-notification + bind-to-endpoint via Phase 55c + standard panic handler) reuses the existing `syscall-lib` helpers. New duplication crossing two sites is factored out.

### YAGNI

- No syscall, struct field, capability bit, or service interface is added speculatively.
- Audio is **single-client** in Phase 57 (per the design doc's allowance). Multi-client mixing, sample-rate conversion, format conversion, capture (record), and audio routing are explicitly deferred. The chosen audio backend supports exactly one open stream at a time; a second client receives `-EBUSY`.
- Session manager handles exactly one session in Phase 57. Multiple TTYs, fast user switching, lock/unlock, and idle timeout are deferred.
- `term` ships one bitmap font at one size and one color scheme. Configurable fonts, scrollback beyond a fixed buffer, transparency, and tab support beyond ANSI HT semantics are deferred.

### Boy Scout Rule

- Leave every file you touch cleaner than you found it: fix a stale comment, remove a dead import, or clarify an opaque variable name. Keep changes scoped to the task at hand; do not open unrelated refactors.
- When a task reveals a lint suppression (`#[allow(...)]`) without a documented justification, either add the justification or remove the allow.
- Dead test stubs that are no longer accurate must be updated or removed before the task closes — do not leave misleading `#[ignore]` annotations without an inline comment naming the exact blocker.

### Error discipline

- Non-test code contains no `.unwrap()`, `.expect()`, `panic!()`, `todo!()`, or `unreachable!()` outside of documented fail-fast initialization points. Every such site carries an inline comment naming the audited reason it is safe; `grep`-level review must be able to find and justify every occurrence.
- Every module boundary returns `Result<T, NamedError>` with a named error enum per subsystem (`AudioError`, `SessionError`, `TermError`, `FontError`). Error variants are data, not stringly-typed; callers can match and recover.
- No silent fallbacks: every fallback path emits a structured log event naming the original error.
- `audio_server` returns `-EBUSY` on second-client open, `-EAGAIN` on transient ring-buffer-full, `-ENODEV` if the device claim has not completed, and `-EPIPE` if the client has been disconnected. Other `AudioError` values map through a single `audio_error_to_neg_errno` helper that lives once in `kernel-core::audio`.

### Observability

- `audio_server`, `session_manager`, and `term` emit structured log events keyed by subsystem (`audio.device`, `audio.stream`, `audio.client`, `session.boot`, `session.recover`, `session.control`, `term.input`, `term.render`, `term.pty`). No ad-hoc `println!` or raw stderr writes outside of test-only debug paths.
- `audio_server` exposes a control verb that returns the last N `(stream_id, frames_submitted, frames_consumed, underrun_count)` samples — same shape as Phase 56's `frame-stats` — so regressions in playback pacing are observable without an audio-out fixture.
- `session_manager` records every state transition (`booting`, `running`, `recovering`, `text-fallback`) on a control socket; the `m3ctl` client (Phase 56 E.4) gains a `session-state` verb that prints the current state.

### Capability safety

- `audio_server` claims its device through `sys_device_claim` (Phase 55b). The kernel verifies IOMMU BAR identity coverage (Phase 55c R2) before the claim succeeds; a coverage failure is a hard error, not a warning.
- The audio client capability is a send-cap to `audio_server`'s endpoint. Clients cannot forge or copy raw audio cap values; transfer goes through `sys_cap_grant` per the existing capability discipline.
- `session_manager` holds capabilities to start, stop, and signal the services it supervises; no other process gains those caps. The control socket gates verbs by capability — the connecting peer must hold `session_manager`'s control-socket cap, granted only to `m3ctl` at session-manager startup. Phase 57 introduces no new UID-based access control; consistency with the Phase 56 m3ctl precedent is the explicit design choice (see F.5 acceptance).

### Concurrency and IRQ safety

- `audio_server` runs a **single-threaded event loop** that multiplexes the audio IRQ (via Phase 55c `bind_to_endpoint`), the client listening endpoint, and per-client endpoints through the same `RecvResult` machinery the e1000 driver uses. No worker threads in Phase 57; future moves to threads are deliberate and tracked as later tasks.
- Audio DMA buffers are sized once at stream-open and pinned for the stream's lifetime. The IRQ path performs O(1) work: read status, advance ring tail, write a `Notification` bit; no allocation, no IPC.
- `session_manager` is also single-threaded; it consumes service-supervisor events through one endpoint and never blocks under a lock held across a `recv`.

### Resource bounds

- Audio stream count: **1** (single-client per the design doc). Second client connect closes with a named `-EBUSY`.
- Audio DMA ring size: at least 4 KiB and at most 64 KiB, recorded as named constants in `kernel-core::audio` and revisited only in a follow-up phase. Underrun is observable through the stats verb.
- `term` scrollback: fixed at 1000 lines. Exceeding the line cap drops the oldest line; the count is observable via a `term-stats` control-socket verb.
- Session-restart attempts: capped at 3 per service per minute by the supervisor (consistent with Phase 56 `restart=on-failure max_restart=5` patterns); exceeding the cap escalates the session to `text-fallback`.

---

## Track A — Architecture and Design Memos

### A.1 — Choose the first supported audio target

**File:** `docs/appendix/phase-57-audio-target-choice.md` (new — short design memo)
**Symbol:** N/A
**Why it matters:** Three reasonable audio targets exist (Intel HDA, AC'97, virtio-sound). Each has different MMIO complexity, IRQ semantics, and QEMU + real-hardware coverage. The phase needs one chosen target before any kernel-core or driver code is written; the rejected alternatives' tradeoffs must be recorded so a later phase can revisit without re-litigating.

**Acceptance:**
- [x] Memo names the chosen target verbatim and the rejected alternatives.
- [x] Memo records (a) QEMU device argument(s) needed, (b) real-hardware coverage on commodity x86_64, (c) approximate register/MMIO surface size, (d) DMA model (CORB/RIRB vs BDL vs vring), (e) IRQ shape.
- [x] Memo names the userspace API the target implies (open / submit / drain / close) so B.3 can codify it.
- [x] Memo cross-links the design doc and Phase 55b's ring-3 driver host pattern as the supervision precedent.
- [x] Memo records which `cargo xtask run-gui` flags need to change (e.g., `-audiodev` plus `-machine` device add) and the smallest-impact way to add them.

### A.2 — Service topology and capability map

**File:** `docs/57-audio-and-local-session.md` (learning doc, drafted in I.1; placeholder stub acceptable for A.2 completion)
**Symbol:** `Service topology` (new section)
**Why it matters:** A graphical local session that never names its processes, endpoints, and capabilities cannot be supervised or audited. Pinning the topology before implementation prevents "one big GUI blob" and prevents the kernel from quietly regaining presentation or audio responsibility later.

**Acceptance:**
- [x] `audio_server` is named as the sole userspace owner of the chosen audio device and the only arbiter of the single PCM stream.
- [x] `session_manager` is named as the orchestrator of the graphical session lifecycle; it does not own the framebuffer, input devices, or audio device — those belong to Phase 56's display/input services and to `audio_server` respectively.
- [x] `term` is named as a regular display-server client plus an `audio_client` consumer (for the bell); it holds no privileged capabilities.
- [x] The document records which capability each service holds (`audio_server` → audio device claim + IRQ notification + send-cap to its own listening endpoint; `session_manager` → service-supervisor caps + control-socket cap; `term` → display-server send-cap + audio-server send-cap + PTY fd).
- [x] A process-level Mermaid diagram shows data flow: audio_client → audio_server → device for output; session_manager → display_server / kbd_server / mouse_server / audio_server / term for lifecycle; term → display_server for surfaces and input, term → audio_server for bell, term ↔ PTY for shell I/O.

### A.3 — Audio ABI shape

**File:** `docs/appendix/phase-57-audio-abi.md` (new — short ABI memo)
**Symbol:** N/A
**Why it matters:** The audio surface can be a kernel-mediated set of syscalls (`sys_audio_*`, mirroring `sys_block_*`) or a pure-IPC contract on `audio_server`'s endpoint (mirroring Phase 56 `display_server`). The two shapes have different ABI costs and supervision implications; A.3 picks one and records the rationale before B.3 codifies the wire format.

**Acceptance:**
- [x] Memo names the chosen shape (kernel syscall block vs userspace-only IPC) and the rationale.
- [x] Memo references the existing precedent that justifies the choice: Phase 56's `display_server` (userspace-only IPC) or Phase 55b's `RemoteBlockDevice` / `RemoteNic` (kernel-mediated facade).
- [x] Memo lists the exact files that change under the chosen shape (e.g., `kernel/src/audio/remote.rs` if a kernel facade is added; `userspace/lib/audio_client/src/lib.rs` either way).
- [x] Memo records the maximum bulk size for a single PCM submit and the rationale.

### A.4 — Session-entry contract

**File:** `docs/appendix/phase-57-session-entry.md` (new — short design memo)
**Symbol:** N/A
**Why it matters:** "How a user reaches the local graphical session" is the load-bearing local-system question of Phase 57. Without an explicit contract, the graphical-session story is just "init starts a few daemons and hopes."

**Acceptance:**
- [x] Memo names the entry trigger: a fixed boot sequence ordered by `session_manager` (the default Phase 57 path, since m3OS has no "console session" UID concept yet), OR the memo proposes an explicit alternative trigger and the new concepts that would have to land first. Document the chosen trigger and the rejected alternative; if the alternative requires concepts not yet in the codebase (e.g., a console-session UID), name the prerequisite phase or memo that would deliver them.
- [x] Memo names the explicit ordered startup steps `session_manager` runs and the failure handling for each.
- [x] Memo names the failure-recovery contract: which failures escalate to `text-fallback`, which to a single restart attempt, and where the restart cap lives (per F.4).
- [x] Memo records how a developer reaches the text-mode admin path from the graphical session (input keychord owned by Phase 56's grab hook, or a `m3ctl session-stop` verb, or both — pick one; record the rejected alternative).

### A.5 — Adopt Phase 55b ring-3 driver-host pattern for the audio device

**File:** `docs/roadmap/57-audio-and-local-session.md`
**Symbol:** `Driver hosting and supervision` (new subsection)
**Why it matters:** The kernel must not regain a custom audio driver in ring 0. Phase 55b's ring-3 driver-host pattern is the precedent; A.5 records that Phase 57 adopts it explicitly so a later phase cannot quietly move audio back into the kernel.

**Acceptance:**
- [x] The Phase 57 design doc gains a `Driver hosting and supervision` subsection naming `audio_server` as a Phase 55b-style ring-3 supervised driver, claiming its device through `sys_device_claim`.
- [x] The subsection records that audio uses Phase 55c bound notifications + `RecvResult` for IRQ multiplexing (D.4 wires it; A.5 declares the contract).
- [x] The subsection cross-links Phase 55b, 55c, and the chosen-target memo (A.1).
- [x] The subsection records what is **not** changed in the kernel: the kernel does not learn audio; it only learns "device claim covers the audio BAR(s)."

---

## Track B — `kernel-core` Audio Pure-Logic

### B.1 — PCM format types

**File:** `kernel-core/src/audio/format.rs` (new); `kernel-core/src/audio/mod.rs` (new)
**Symbol:** `PcmFormat`, `SampleRate`, `ChannelLayout`, `frame_size_bytes`
**Why it matters:** The PCM contract (sample width, rate, channel count, endianness) is shared by `audio_server`, `audio_client`, `audio-demo`, and `term`'s bell path. Declaring it once in `kernel-core` is the DRY discipline for the phase and lets every consumer compute frame size and buffer math against the same source of truth.

**Acceptance:**
- [x] Tests commit first (failing) and pass after implementation lands; `git log --follow kernel-core/src/audio/format.rs` shows red-before-green.
- [x] `PcmFormat` enumerates exactly the formats the chosen target supports in Phase 57 (per A.1) — no speculative variants.
- [x] `SampleRate` and `ChannelLayout` are typed enums with `pub fn as_hz()` / `pub fn channel_count()` accessors.
- [x] `frame_size_bytes(format, layout) -> usize` is total-function and panic-free; an exhaustive unit-test matrix covers every (format, layout) pair.
- [x] Visibility is tight: `kernel-core::audio` is the only public surface; private internals stay private.
- [x] No new external crate dependencies.

### B.2 — Audio ring-buffer state model

**File:** `kernel-core/src/audio/ring.rs` (new)
**Symbol:** `AudioRingState`, `AudioSink` (trait), `RingError`
**Why it matters:** PCM playback hinges on a single producer (the client) and a single consumer (the device's DMA engine). Locking the ring state machine in pure logic — head, tail, capacity, underrun, fill-level — proves correctness before any DMA-adjacent unsafe code lands.

**Acceptance:**
- [x] Failing tests commit first: `write_advances_head`, `consume_advances_tail`, `write_into_full_returns_wouldblock`, `consume_from_empty_returns_underrun`, `wrap_around_preserves_byte_order`, `fill_level_is_consistent_with_head_tail`.
- [x] `AudioSink` trait abstracts the consumer side; a `RecordingAudioSink` test double records every byte consumed and the contract-test suite exercises both impls (the recording double + the kernel-core in-memory ring) with the same harness.
- [x] `RingError` is a typed enum with `Underrun`, `WouldBlock`, `BufferTooSmall`; no stringly-typed errors.
- [x] At least 6 unit tests + 1 contract test commit red before any `AudioRingState` implementation.
- [x] The state model carries no allocation; it operates on a caller-supplied byte buffer.

### B.3 — Audio protocol codec

**File:** `kernel-core/src/audio/protocol.rs` (new)
**Symbol:** `ClientMessage`, `ServerMessage`, `AudioControlCommand`, `AudioControlEvent`, `encode`, `decode`, `ProtocolError`
**Why it matters:** Following Phase 56's `kernel-core::display::protocol` pattern, the audio wire format lives once and is consumed by both `audio_server` and every client. Declaring it in `kernel-core` is the DRY discipline and makes the codec host-testable in isolation.

**Acceptance:**
- [x] Failing tests commit first; encode/decode are pure functions with no allocation on the hot path.
- [x] `encode` writes into a caller-supplied `&mut [u8]` and returns bytes-written; `decode` consumes from `&[u8]` and returns `Result<(Message, bytes_consumed), ProtocolError>`.
- [x] Per-variant unit round-trip tests cover every message type.
- [x] At least one `proptest`-based round-trip test per family (client, server, control-command, control-event) proves `decode(encode(msg)) == msg` for arbitrary valid messages with at least 1024 cases.
- [x] A corrupted-framing property test feeds arbitrary `&[u8]` into `decode` and asserts the decoder returns a typed `ProtocolError` without panicking, looping unboundedly, or allocating unboundedly.
- [x] No declaration of any audio-protocol type appears anywhere else in the workspace; a repo-wide grep confirms exactly one definition site.

### B.4 — Property tests for ring + protocol interaction

**File:** `kernel-core/src/audio/ring_proptest.rs` (new)
**Symbol:** `ring_proptest_invariants`
**Why it matters:** B.2's unit tests cover named cases; the property tests prove the ring stays consistent under arbitrary write/consume interleavings, which is how production playback actually exercises the model.

**Acceptance:**
- [x] Given an arbitrary sequence of `write(bytes)`, `consume(n)`, and `reset()` operations, the ring's reported `fill_level` always equals `head - tail (mod capacity)`.
- [x] No sequence produces negative fill, fill > capacity, or out-of-bounds index access.
- [x] `proptest` configured with at least 1024 cases; runs under `cargo test -p kernel-core --release --target x86_64-unknown-linux-gnu` in default CI.

### B.5 — `audio_error_to_neg_errno` helper

**File:** `kernel-core/src/audio/errno.rs` (new)
**Symbol:** `audio_error_to_neg_errno`
**Why it matters:** Every kernel-side or userspace-side audio path that surfaces a POSIX-style errno (e.g., a syscall, an IPC reply, a smoke binary's exit code) must agree on the mapping. Phase 55c established this pattern with `net_error_to_neg_errno`; Phase 57 follows it.

**Acceptance:**
- [x] Failing tests commit first.
- [x] Every `AudioError` variant maps to a stable negative-errno value; the mapping is total.
- [x] Unit tests cover every variant.
- [x] No other file in the workspace performs `AudioError → errno` translation; a workspace-wide grep proves a single call site for each variant's mapping.

---

## Track C — Kernel Substrate

### C.1 — Audio device claim path through `device_host`

**File:** `kernel/src/device_host/mod.rs`
**Symbol:** `DeviceHost::claim_audio_device` (or extension to existing claim path; choose the smallest delta consistent with A.5)
**Why it matters:** Phase 55b already exposes a `sys_device_claim` syscall for ring-3 drivers. Phase 57 adds the audio device class to its acceptance list (with the chosen-target's PCI IDs from A.1) without inventing a new syscall. The claim path performs IOMMU BAR identity-mapping per Phase 55c R2 — failure is a hard error.

**Acceptance:**
- [ ] Failing kernel-core tests commit first against a fake `DeviceHost` to lock the audio claim contract.
- [ ] Audio device's PCI vendor/device ID(s) are recognized by the claim path; mismatch returns the existing `DeviceHostError` variants (no new variants).
- [ ] Phase 55c `BarCoverage::assert_bar_identity_mapped` runs as part of the claim. A coverage failure surfaces `DeviceHostError::Internal` with a structured `iommu.missing_bar_coverage` log event (subsystem `audio.device`).
- [ ] Existing NVMe + e1000 device-claim tests stay green.
- [ ] No new syscall is introduced for the claim path itself.

### C.2 — Audio DMA buffer sizing + IOMMU coverage

**Files:**
- `kernel/src/iommu/intel.rs`
- `kernel/src/iommu/amd.rs` (parity)

**Symbol:** existing `install_bar_identity_maps` extended to cover the audio device's BARs
**Why it matters:** The audio device's MMIO and DMA regions must be visible to the ring-3 driver under both VT-d and AMD-Vi. Phase 55c R2 already extends the IOMMU domain on claim; C.2 verifies the audio device exercises the same path and that DMA buffer pages allocated by `audio_server` for stream submission are reachable through the device's IOMMU domain.

**Acceptance:**
- [ ] Integration test: `cargo xtask device-smoke --device audio --iommu` passes end-to-end on a VT-d-capable QEMU machine type.
- [ ] AMD-Vi parity test passes where supported by the local QEMU machine type; otherwise documented as conditional in the test's doc comment with a follow-up tracking the gap.
- [ ] DMA buffer pages allocated through the existing `DmaBuffer<T>` (Phase 55a) are reachable from the audio device with no new identity-mapping helper.
- [ ] `cargo xtask check` passes.

### C.3 — Buffer-empty notification surface

**File:** `kernel/src/audio/mod.rs` (new module — minimal kernel-side glue, MAY collapse to zero new files if A.3 chooses pure-userspace IPC)
**Symbol:** N/A or `AudioRemoteFacade` depending on A.3's choice
**Why it matters:** When the device's DMA ring drains, the userspace driver must wake without polling. Phase 55c bound notifications + `RecvResult` are the existing mechanism; C.3 wires the audio device's IRQ vector to a kernel-side `Notification` slot the userspace driver can bind to.

**Acceptance:**
- [ ] Whatever A.3 decided, the path between an audio-device IRQ and the userspace driver is a Phase 55c bound notification with a documented `WakeKind::Notification(bits)` payload.
- [ ] The IRQ handler does the minimum (read status, signal notification, EOI) — no allocation, no IPC, no blocking. Module-level docs in the touched file list the new IRQ source under the existing ISR-safety invariants.
- [ ] If A.3 chose a kernel facade (`RemoteAudio`), it follows `RemoteNic`'s shape and routes errors through `audio_error_to_neg_errno` (B.5). If A.3 chose pure-userspace IPC, no new kernel surface lands.
- [ ] Existing IRQ + notification tests stay green.

---

## Track D — `audio_server` Ring-3 Driver

### D.1 — `audio_server` scaffold

**Files:**
- `userspace/audio_server/Cargo.toml` (new)
- `userspace/audio_server/src/main.rs` (new)
- `Cargo.toml` workspace `members` update
- `xtask/src/main.rs` `bins` array entry (`needs_alloc = true`)
- `kernel/src/fs/ramdisk.rs` `BIN_ENTRIES` entry
- `etc/services.d/audio_server.conf` (new) and the `populate_ext2_files` + `KNOWN_CONFIGS` updates

**Symbol:** `userspace/audio_server` crate
**Why it matters:** Adding a userspace binary in m3OS requires updates in **four** places (workspace member, xtask `bins`, ramdisk `BIN_ENTRIES`, service config). D.1 lands all four so subsequent driver tasks can build incrementally.

**Acceptance:**
- [ ] All four convention points updated; running `cargo xtask check && cargo xtask run` boots the empty `audio_server` and the supervisor logs its start.
- [ ] `audio_server.conf` records `restart=on-failure max_restart=3 depends=display_server` (audio depends on display only insofar as the session boot order requires display first; D.6 may revise after F.2 is concrete).
- [ ] `cargo xtask clean` recreates the data disk so the new conf reaches userspace.
- [ ] The crate uses `syscall_lib::heap::BrkAllocator` per the four-step convention.

### D.2 — Chosen-target controller init

**File:** `userspace/audio_server/src/device.rs` (new)
**Symbol:** `AudioBackend` (trait), `<ChosenTarget>Backend` (concrete impl named after A.1's choice)
**Why it matters:** This is the first chunk of MMIO + DMA work touching real audio hardware. Splitting the trait from the concrete implementation now lets a later phase add a second backend (e.g., HDA after AC'97) by adding a file rather than editing callers.

**Acceptance:**
- [ ] Failing host tests commit first against a `FakeMmio` (the same shape `userspace/drivers/e1000` uses) covering: reset → status reads → DMA buffer programming.
- [ ] `AudioBackend` trait declares `init`, `open_stream`, `submit_frames`, `drain`, `close_stream`, `handle_irq`. Each method has a typed error from `kernel-core::audio::AudioError`.
- [ ] `<ChosenTarget>Backend` implements every method; methods that block on hardware advance use Phase 55c bound notifications via the parent `irq.rs` loop, never `irq.wait()` directly.
- [ ] Unit tests verify register-write ordering (e.g., reset before stream-base programming) on the `FakeMmio` double.
- [ ] No `unsafe` outside MMIO read/write helpers; every `unsafe` block has an inline justification comment.

### D.3 — PCM stream submission path

**File:** `userspace/audio_server/src/stream.rs` (new)
**Symbol:** `Stream`, `StreamRegistry` (single-stream variant)
**Why it matters:** The submission path is where the protocol codec, the ring-buffer state model, and the backend trait meet. Single-stream-only per YAGNI; multiple-stream support is explicitly deferred to a later phase.

**Acceptance:**
- [ ] Failing tests commit first against an in-memory `RecordingAudioSink` (B.2) and a fake `AudioBackend`.
- [ ] `Stream::open` returns `-EBUSY` if a stream is already open; the registry guarantees at-most-one-stream.
- [ ] `Stream::submit_frames(bytes)` advances the ring head; underflow on the device side is observable via the stats verb and never panics.
- [ ] Drain semantics match the chosen-target memo (A.1): graceful drain blocks on the next IRQ until `frames_consumed >= frames_submitted`, with a documented timeout and a typed error path on timeout.

### D.4 — IRQ multiplex via Phase 55c bound notifications

**File:** `userspace/audio_server/src/irq.rs` (new)
**Symbol:** `subscribe_and_bind`, `run_io_loop`
**Why it matters:** The same loop shape Phase 55c added to e1000 now applies to audio. The dispatch is single-threaded; the loop multiplexes one IRQ source and the client endpoint through `RecvResult`.

**Acceptance:**
- [ ] Failing integration test commits first; mirrors `userspace/drivers/e1000/tests/bound_notif_smoke.rs`.
- [ ] `subscribe_and_bind` calls Phase 55c `IrqNotification::bind_to_endpoint`; arms the device's IRQ; returns a typed error on bind-failure.
- [ ] `run_io_loop` blocks only on `endpoint.recv_multi(&irq_notif)`. On `RecvResult::Notification { bits }` the loop calls the backend's IRQ handler; on `RecvResult::Message(req)` it dispatches to the protocol codec.
- [ ] `grep "irq.wait" userspace/audio_server/src/` returns no hits in the io loop.

### D.5 — Single-client policy + queueing

**File:** `userspace/audio_server/src/client.rs` (new)
**Symbol:** `ClientRegistry`, `ClientState`
**Why it matters:** Single-client audio per YAGNI is a deliberate Phase 57 boundary. A second client must observe a clear, typed `-EBUSY`; closing the first client must release the slot.

**Acceptance:**
- [ ] Failing tests commit first.
- [ ] First connect is admitted; second connect is rejected with `-EBUSY` and the rejection is logged once per second-attempt (rate-limited per the observability rules).
- [ ] Client disconnect (graceful or fault) releases the stream slot synchronously; the next connect is admitted on the next dispatch tick.
- [ ] No allocation per dispatch.
- [ ] Coverage boundary: D.5 covers single-process unit semantics around `ClientRegistry`; cross-process integration coverage (two real client processes against a live `audio_server`) lives in H.4. The two tasks deliberately overlap on the observable `-EBUSY` so a regression in either layer fails CI.

### D.6 — Service manifest + supervision wiring

**Files:**
- `etc/services.d/audio_server.conf`
- `xtask/src/main.rs` (`populate_ext2_files`)
- `userspace/init/src/main.rs` (`KNOWN_CONFIGS`)

**Symbol:** N/A
**Why it matters:** Audio is a Phase 55b-style ring-3 supervised driver. Crash recovery must be observable; the manifest is the load-bearing contract for that.

**Acceptance:**
- [ ] Manifest records `restart=on-failure max_restart=3 on-restart=audio_server.restart` consistent with the Phase 56 F.1 supervisor `on-restart` precedent.
- [ ] On driver restart the prior `audio_server` process exits, the supervisor releases its caps (including the `sys_device_claim` handle), and the new process re-runs `sys_device_claim` and Phase 55c `IrqNotification::bind_to_endpoint` during `init`. **No kernel-side claim persistence across restart is introduced**; the kernel surface is unchanged from Phase 55b.
- [ ] Manifest's `depends=` reflects the F.2 / A.4 ordering decision exactly (must include every service that has to be ready before `audio_server` accepts clients — at minimum `display_server` and the input services if A.4 places audio downstream of input).
- [ ] `cargo xtask clean && cargo xtask run` boots the supervisor and `audio_server`; killing `audio_server` from the supervisor's debug verb causes a documented restart and a single `audio.device.claim` re-acquire log line.

---

## Track E — Audio Client Surface

### E.1 — `userspace/lib/audio_client` library

**Files:**
- `userspace/lib/audio_client/Cargo.toml` (new)
- `userspace/lib/audio_client/src/lib.rs` (new)

**Symbol:** `AudioClient`, `AudioClient::open`, `AudioClient::submit_frames`, `AudioClient::drain`, `AudioClient::close`
**Why it matters:** Every userspace consumer of audio (the demo, `term`'s bell, future media clients) goes through this library. Carrying the protocol concerns once here keeps consumer crates small and keeps the wire format private to `kernel-core::audio`.

**Acceptance:**
- [ ] Failing host tests commit first against a mock IPC backend.
- [ ] The library's public surface is exactly: `open`, `submit_frames`, `drain`, `close`, plus a typed `AudioClientError`. No protocol bytes are exposed.
- [ ] The library reuses `kernel-core::audio::protocol` for encode/decode — no parallel definitions.
- [ ] Cargo features: `alloc` (default off — the library is `#![no_std]`); enabling `alloc` unlocks the convenience helpers that allocate buffers for callers.
- [ ] Unit tests cover open → submit → drain → close happy path, every error variant, and the second-open `EBUSY` path.

### E.2 — `userspace/audio-demo` reference client

**Files:**
- `userspace/audio-demo/Cargo.toml` (new)
- `userspace/audio-demo/src/main.rs` (new)
- workspace `members`, xtask `bins`, ramdisk `BIN_ENTRIES`, optional `audio-demo.conf`

**Symbol:** `userspace/audio-demo`
**Why it matters:** A protocol-reference demo is the only credible way to show "audio works." It also doubles as the audio smoke harness for H.1.

**Acceptance:**
- [ ] All four-step new-binary convention points updated.
- [ ] On run, the demo opens a stream, submits a known sine-wave PCM buffer (frequency, sample rate, and duration recorded in source as named constants), drains, closes, and exits zero.
- [ ] On failure (any `AudioClientError`), the demo prints a structured log line with the variant name and exits non-zero.
- [ ] Source includes a comment explaining how the test tone was generated (so a reader can regenerate it without reading binary blobs).
- [ ] No external crate dependencies beyond the workspace.

---

## Track F — `session_manager` and Entry Flow

### F.1 — `kernel-core` session-step state model

**Files:**
- `kernel-core/src/session/mod.rs` (new)
- `kernel-core/src/session/startup.rs` (new)

**Symbol:** `SessionStep` (trait), `StartupSequence`, `SessionState`
**Why it matters:** The session lifecycle (booting → running → recovering → text-fallback) is a state machine. Locking it in pure logic before wiring service calls catches ordering bugs (e.g., starting `term` before `display_server` is ready) before any process is spawned.

**Acceptance:**
- [x] Failing tests commit first; `SessionState` transitions are total and exercised by a contract suite that runs against a recording double and a fake-supervisor double.
- [x] `SessionStep` trait declares `name`, `start`, `stop`, `is_ready`. Each method returns a typed `SessionError`.
- [x] `StartupSequence` runs steps in declared order; a step's `start` failure escalates per A.4's contract.
- [x] No allocation in steady-state; `proptest` covers arbitrary step-success / step-failure interleavings.

### F.2 — `session_manager` daemon scaffold

**Files:**
- `userspace/session_manager/Cargo.toml` (new)
- `userspace/session_manager/src/main.rs` (new)
- workspace `members`, xtask `bins`, ramdisk `BIN_ENTRIES`, `etc/services.d/session_manager.conf`

**Symbol:** `userspace/session_manager` crate
**Why it matters:** The session-entry contract from A.4 needs a single supervised daemon that owns it. Without one, ordering decisions live scattered across service manifests and drift over time.

**Acceptance:**
- [x] All four-step new-binary convention points updated.
- [x] On boot, `session_manager` consumes `kernel-core::session::startup::StartupSequence` and runs the declared graphical session steps in order: `display_server` → `kbd_server` → `mouse_server` → `audio_server` → `term` (exact order matches A.4). The order is declared once in `kernel-core::session_supervisor::DECLARED_SESSION_STEP_NAMES` (DRY rule).
- [x] `session_manager` is a Phase 56-style single-threaded event loop multiplexing supervisor events and a control socket. `userspace/session_manager/src/main.rs` after the boot sequence enters a steady-state loop polling `control::poll_control_once` (F.5 stub) + idle sleep.
- [x] `cargo xtask check` passes (release builds clean; clippy `-D warnings` clean for the new crate).

Phase 57 transitional note (per worktree spec): `audio_server` and `term` userspace binaries land in Tracks D and G respectively. Until they exist, `InitSupervisorBackend::start` returns `SupervisorReply::Error(UnknownService)` for those names. The F.1 sequencer counts each as a step failure; after 3 attempts the session escalates cleanly to `SessionState::TextFallback` with reverse rollback. Once D and G land, the same boot path reaches `SessionState::Running` without changing this binary. Documented in `userspace/session_manager/src/main.rs` module-level docs.

### F.3 — Service-supervisor integration

**File:** `userspace/init/src/main.rs` (touch only the supervision API surface) + `kernel-core::session_supervisor` (new pure-logic codec)
**Symbol:** existing `supervisor` API extended with the smallest interface `session_manager` needs
**Why it matters:** `session_manager` cannot supervise from outside the supervisor; F.3 surfaces only the verbs `session_manager` actually needs (start, stop, await-ready, on-exit), and no others.

**Acceptance:**
- [x] Failing tests commit first. `kernel-core/tests/phase57_f3_session_supervisor.rs` (17 tests) committed red, then green.
- [x] The new supervision verbs are visible only to processes holding the `session_manager` capability — no broad public surface. `kernel_core::session_supervisor::SupervisorCap` has a single named constructor `granted_for_session_manager_only`; `dispatch_authenticated` gates on `Option<&SupervisorCap>` and returns `CapabilityMissing` without invoking the backend when absent.
- [x] Existing service supervision tests stay green. `kernel-core/src/service.rs` is untouched and `kernel-core/src/session/` is untouched (F.1 owns it). Full kernel-core test suite (1264 + integration tests) remains green.
- [x] No new syscall is added for supervision; `session_manager` consumes existing IPC + capabilities. The transport is init's existing root-only `/run/init.cmd` control channel plus reads of `/run/services.status` for readiness/exit observation. F.4 will issue the first writes; F.2's adapter probes the IPC service registry as the readiness signal.

### F.4 — Recovery + text-mode fallback

**File:** `userspace/session_manager/src/recover.rs` (new)
**Symbol:** `Recovery`, `Recovery::on_step_failure`
**Why it matters:** "Returns to a recoverable administration path if the session fails" is in the design doc's Critical Items. Without an explicit recovery path, a single `display_server` crash bricks the local UX.

**Acceptance:**
- [ ] Failing tests commit first against a fake-supervisor double.
- [ ] On a step's `start` failure: the recovery state machine retries up to the documented per-service cap (3 by default per the resource-bounds rule); exceeding the cap escalates to `text-fallback`.
- [ ] On `text-fallback`: `session_manager` stops the graphical services in reverse start order, releases the framebuffer back to the kernel console (the existing Phase 47 `restore_console` path), and surfaces an admin shell on the serial console.
- [ ] Smoke test (H.3) verifies the kill-display-server → text-fallback path.

### F.5 — Control socket: `session-state` and `session-stop`

**File:** `userspace/session_manager/src/control.rs` (new)
**Symbol:** `ControlServer`
**Why it matters:** Without an out-of-band control verb the local user has no documented way to leave the graphical session for administration. The `m3ctl` client (Phase 56 E.4) gains the verbs in the Phase 56 client; F.5 adds the server side.

**Acceptance:**
- [ ] Failing tests commit first.
- [ ] Control socket lives on a separate AF_UNIX path consistent with the Phase 56 control-socket precedent.
- [ ] Verbs: `session-state` (returns the current `SessionState`), `session-stop` (graceful shutdown, falls through to `text-fallback`), `session-restart` (graceful stop + start).
- [ ] Access control follows the Phase 56 m3ctl precedent: capability-based — the connecting peer must hold the `session_manager` control-socket cap, granted to `m3ctl` at session-manager startup and to no other process. **No UID-based access control is introduced in Phase 57**; a future "console session UID" concept is a deferred design item with its own memo.
- [ ] F.5 closes with the server side and an integration test that drives the verbs through a raw `nc -U`-equivalent (or the existing IPC test harness). The typed `m3ctl` client-side commands ship in I.2; the F.5 → I.2 client deferral is explicit and intentional.

---

## Track G — `term` Graphical Terminal

### G.1 — `userspace/term` scaffold

**Files:**
- `userspace/term/Cargo.toml` (new)
- `userspace/term/src/main.rs` (new)
- workspace `members`, xtask `bins`, ramdisk `BIN_ENTRIES`, `etc/services.d/term.conf`

**Symbol:** `userspace/term` crate
**Why it matters:** "At least one useful graphical client" per the Critical Items list. A terminal emulator is the most generally useful client and the lowest-risk first one because every other userspace component is reused (PTY, ANSI parser, display protocol, audio client for the bell).

**Acceptance:**
- [ ] All four-step new-binary convention points updated.
- [ ] `term.conf` records `restart=on-failure max_restart=3 depends=display_server,kbd_server,session_manager`.
- [ ] Boot-time integration test verifies `term` reaches its event loop and registers a surface with `display_server`.

### G.2 — Bitmap font provider

**Files:**
- `kernel-core/src/session/font.rs` (new)
- `kernel-core/src/session/font_data.rs` (new — generated/embedded glyph data)

**Symbol:** `FontProvider` (trait), `BasicBitmapFont`, `Glyph`
**Why it matters:** Without a font, `term` renders nothing. Putting the font behind a trait keeps the door open for a future TrueType path without forcing one in Phase 57. YAGNI: one bitmap font, one size, one color.

**Acceptance:**
- [ ] Failing tests commit first.
- [ ] `FontProvider::glyph(codepoint) -> Option<Glyph>` covers ASCII printable + space + DEL; non-ASCII returns `None`.
- [ ] `Glyph::render_into(&mut [u32], stride, fg, bg)` writes BGRA8888 pixels into the caller's buffer; out-of-bounds is a typed error.
- [ ] Glyph data is statically embedded; no runtime file I/O.
- [ ] Contract test exercises the trait with at least the bundled font and one mock font.

### G.3 — PTY connection

**File:** `userspace/term/src/pty.rs` (new)
**Symbol:** `PtyHost`
**Why it matters:** `term` is useless without a shell behind it. Phase 29 already exposes the PTY subsystem and Phase 22b already parses ANSI; G.3 wires `term` to a PTY pair and spawns the existing shell on the secondary side.

**Acceptance:**
- [ ] Failing tests commit first against a mock PTY.
- [ ] `term` opens a PTY pair via existing Phase 29 syscalls; spawns `sh0` (or the existing default shell) with the secondary as stdio; reads from the primary into the ANSI parser.
- [ ] Shell exit causes `term` to close its surface and exit zero; the supervisor restarts `term` per `term.conf`.

### G.4 — ANSI parser reuse and screen state

**File:** `userspace/term/src/screen.rs` (new)
**Symbol:** `Screen`
**Why it matters:** Reuse Phase 22b's parser rather than re-implementing escape handling. The screen state machine is small but real (cursor, scrollback, color attrs, BEL → audio).

**Acceptance:**
- [ ] Failing tests commit first; the screen consumes parser output and produces a typed sequence of render commands (`PutGlyph`, `Scroll`, `SetColor`, `Bell`, …).
- [ ] No allocation per character; the screen owns a fixed-size cell buffer.
- [ ] Scrollback is fixed at 1000 lines; exceeding the cap drops the oldest line.
- [ ] BEL maps to a single `audio_client` submission of a documented short tone (or a no-op when `audio_server` is unavailable; the unavailable path emits a single warn log).
- [ ] Property test exercises arbitrary ANSI byte sequences and verifies the screen state stays consistent (no panic, no out-of-bounds, no negative cursor).

### G.5 — Display-server client wiring

**Files:**
- `userspace/term/src/render.rs` (new)
- `userspace/term/src/input.rs` (new)

**Symbol:** `Renderer`, `InputHandler`
**Why it matters:** `term` is a Phase 56 display-server client. The render path consumes screen-render commands and writes to the surface buffer; the input path receives `KeyEvent`s from `display_server` and forwards them to the PTY.

**Acceptance:**
- [ ] Failing tests commit first against a `RecordingFramebufferOwner`-style double.
- [ ] Renderer batches dirty cells per frame-tick; `compose` runs only when the screen has damage.
- [ ] Input handler consumes `KeyEvent`s, applies the keymap (Phase 56 D.1 outputs), and writes shell-relevant byte sequences (e.g., `0x03` for Ctrl-C) to the PTY.
- [ ] No worker threads in `term`.

### G.6 — `term` bell via `audio_client`

**File:** `userspace/term/src/bell.rs` (new)
**Symbol:** `Bell`
**Why it matters:** Without this, the BEL escape disappears silently. With it, audio is exercised end-to-end by a real graphical client, which is exactly the milestone the design doc names.

**Acceptance:**
- [ ] Failing tests commit first against a mock `AudioClient`.
- [ ] On `Screen::Bell`, the bell opens a short stream, submits the documented tone, drains, and closes — never blocking the render loop for more than a documented timeout (∼50 ms).
- [ ] If `audio_server` is unavailable, the bell emits one warn log and otherwise no-ops; subsequent bells within a documented coalescing window are silently dropped.

---

## Track H — Validation

### H.1 — Audio smoke test (`cargo xtask audio-smoke`)

**File:** `xtask/src/main.rs` — new `cmd_audio_smoke`
**Symbol:** `cmd_audio_smoke`
**Why it matters:** A regression in any of A.1, B.*, C.*, D.*, or E.* is observable here before it reaches the field. The smoke harness does not require a real audio fixture: it asserts the driver-level `frames_consumed` counter advances under `-audiodev none,id=…`, which is a deterministic, scriptable signal.

**Acceptance:**
- [ ] Boots `cargo xtask run` headless; runs `audio-demo`; reads the `audio.stream` stats verb.
- [ ] Asserts `frames_consumed >= frames_submitted` within a documented timeout.
- [ ] Fails with a distinct exit code if any assertion does not hold.
- [ ] Mirrors the `scripts/ssh_e1000_banner_check.sh` reference shape.

### H.2 — Session entry smoke test (`cargo xtask session-smoke`)

**File:** `xtask/src/main.rs` — new `cmd_session_smoke`
**Symbol:** `cmd_session_smoke`
**Why it matters:** Without an end-to-end session-entry test, regressions in F.* / G.* land silently. The smoke harness boots, waits for `term` to become render-ready, simulates a keypress through `kbd_server`, and asserts the keypress reaches the PTY's secondary side.

**Acceptance:**
- [ ] Boots `cargo xtask run-gui` (headless or display, configurable).
- [ ] Asserts `session_manager` reaches `SessionState::Running` within a documented timeout.
- [ ] Simulates a keypress; asserts the PTY receives the corresponding byte; asserts `term` damages and recomposes its surface.
- [ ] Fails with a distinct exit code if any assertion does not hold.

### H.3 — Recovery / text-mode fallback smoke

**File:** `xtask/src/main.rs` — extends H.2 or new `cmd_session_recover_smoke`
**Symbol:** `cmd_session_recover_smoke`
**Why it matters:** F.4's text-fallback path is recovery-of-last-resort. If it regresses silently the local-system milestone is broken: a `display_server` crash bricks the UX with no path to administration.

**Acceptance:**
- [ ] Boots the graphical session per H.2; asserts `Running`.
- [ ] Kills `display_server` from the supervisor's debug verb.
- [ ] Asserts `session_manager` retries up to the cap, then escalates to `text-fallback`.
- [ ] Asserts the kernel framebuffer console is restored (Phase 47 `restore_console`) and the serial admin shell is reachable.
- [ ] Fails with a distinct exit code on any assertion failure.

### H.4 — Multi-client audio policy

**File:** `userspace/audio_server/tests/multi_client.rs` (new)
**Symbol:** `second_client_returns_ebusy`
**Why it matters:** Single-client audio is a YAGNI boundary, not an accident. A second-client request must observe a clear `-EBUSY` and the first client's stream must remain undisturbed.

**Acceptance:**
- [ ] Two `audio_client` instances connect; the first is admitted; the second receives `-EBUSY` and a structured log event.
- [ ] First client's `frames_consumed` continues advancing across the second-client attempt.
- [ ] Test runs under `cargo xtask test`.

### H.5 — `cargo xtask run-gui` plumbing

**File:** `xtask/src/main.rs`
**Symbol:** `run_gui`
**Why it matters:** Without `-audiodev` / `-device <chosen-target>` flags wired into `run-gui`, no developer can validate audio interactively.

**Acceptance:**
- [ ] `cargo xtask run-gui` adds the chosen-target's QEMU `-audiodev` and `-device` flags by default, alongside the existing `pcspk-audiodev=noaudio` PC-speaker binding (which stays unchanged — the new audio backend is added, not substituted).
- [ ] An opt-out flag (`--no-audio`) skips the new flags but preserves the existing `pcspk-audiodev=noaudio` binding so non-audio runs match today's behavior byte-for-byte.
- [ ] `cargo xtask run-gui --fresh` continues to recreate the data disk.
- [ ] The "documented contract" is A.1's audio-target memo, and the exact `-audiodev` / `-device` snippet from A.1 is transcribed verbatim into a single named constant in `xtask/src/qemu_audio.rs` (or equivalent existing module). The xtask unit test pins the constructed QEMU command line against that constant.
- [ ] `cargo xtask device-smoke --device audio [--iommu]` is wired into the same `--device` whitelist as `nvme` and `e1000` (see C.2's IOMMU integration test); a sub-acceptance here covers the xtask command-line surface change so C.2's test can actually run.

### H.6 — Manual smoke checklist

**File:** `docs/57-audio-and-local-session.md` — `Manual smoke checklist` section (drafted in I.1)
**Symbol:** N/A
**Why it matters:** Some failure modes only show up on real hardware or a developer's QEMU with a real audio fixture. The manual checklist is the durable "did anyone try this lately?" record.

**Acceptance:**
- [ ] Checklist items: (1) `audio-demo` produces audible tone on a host with working audio; (2) `term` BEL produces audible bell; (3) graphical session reaches `Running` from cold boot; (4) keypress reaches a shell prompt visibly in `term`; (5) `Ctrl-C` from `term` reaches the shell as `0x03`; (6) killing `display_server` falls back to text-mode admin within the documented cap.
- [ ] Each item lists the exact command(s) to run and the expected observable outcome.

---

## Track I — Documentation and Version

### I.1 — Phase 57 learning doc

**File:** `docs/57-audio-and-local-session.md` (new)
**Symbol:** N/A
**Why it matters:** Every completed phase requires a learning doc — the design doc says so explicitly. Future audio-driver authors and roadmap contributors need a structured explanation of the audio contract, session flow, and terminal baseline. Without it, the roadmap has a gap exactly where later media or desktop-shell work would want to reuse what Phase 57 established.

**Acceptance:**
- [ ] Doc follows the **aligned legacy learning doc** template from `docs/appendix/doc-templates.md`: `Aligned Roadmap Phase`, `Status`, `Source Ref`, `Supersedes Legacy Doc` (N/A), `Overview`, `What This Doc Covers`, `Core Implementation`, `Key Files`, `How This Phase Differs From Later Work`, `Related Roadmap Docs`, `Deferred or Later-Phase Topics`.
- [ ] **What This Doc Covers** lists exactly: (1) the chosen audio target and PCM contract; (2) the graphical session-entry flow and recovery contract; (3) the `term` graphical-client baseline and how it composes Phase 22b/29/56 outputs; (4) the deferred boundary between Phase 57 and a later media/desktop phase.
- [ ] **Key Files** table names at minimum: `kernel-core/src/audio/format.rs`, `kernel-core/src/audio/ring.rs`, `kernel-core/src/audio/protocol.rs`, `kernel-core/src/session/startup.rs`, `kernel-core/src/session/font.rs`, `userspace/audio_server/src/main.rs`, `userspace/lib/audio_client/src/lib.rs`, `userspace/session_manager/src/main.rs`, `userspace/term/src/main.rs`, `etc/services.d/audio_server.conf`, `etc/services.d/session_manager.conf`, `etc/services.d/term.conf`.
- [ ] **How This Phase Differs From Later Work** notes that Phase 57 ships single-client audio, single-session, and a one-font terminal; multi-stream mixing, sample-rate conversion, capture, multi-session, and richer shell (notifications, settings panels, app ecosystem) are deferred.
- [ ] Doc records the resource-bound defaults from the Engineering Discipline section above (audio ring size, scrollback cap, restart cap).
- [ ] Doc cross-links the four design memos (A.1, A.3, A.4, A.5).
- [ ] Doc added to `docs/README.md` index.
- [ ] `docs/roadmap/57-audio-and-local-session.md` **Companion Task List** section updated to include a link to this learning doc.
- [ ] **Phase 57 cannot close without this learning doc in tree** — I.5 must not land first.

### I.2 — Subsystem and reference doc updates

**Files:**
- `docs/29-pty-subsystem.md`
- `docs/27-user-accounts.md` (session-entry / local-login angle)
- `docs/roadmap/55-hardware-substrate.md` (audio target row)
- `docs/README.md`
- `AGENTS.md` (project-overview line)
- `userspace/m3ctl/src/main.rs` for the new `session-state` / `session-stop` / `session-restart` verbs (closes the F.5 → I.2 client-side deferral)

**Symbol:** N/A
**Why it matters:** Reference documentation drifts silently. The design doc explicitly asks for hardware/audio support docs and session-startup/local-login documentation to be updated; without those updates the new audio target and session entry are effectively undocumented for any reader who lands first on the related subsystem doc rather than the Phase 57 learning doc.

**Acceptance:**
- [ ] `docs/29-pty-subsystem.md` gains a paragraph naming `term` as the first graphical PTY consumer and the existing shell as the secondary-side default.
- [ ] `docs/27-user-accounts.md` gains a paragraph naming the local-graphical-session entry trigger from A.4 and explicitly recording that Phase 57 introduces no new UID concepts (matching F.5's capability-based access decision).
- [ ] `docs/roadmap/55-hardware-substrate.md` gains a row in its hardware-target table for the chosen Phase 57 audio target (PCI vendor/device IDs, QEMU device argument, IOMMU coverage status).
- [ ] `docs/README.md` lists the Phase 57 learning doc.
- [ ] `AGENTS.md` project-overview line names audio, `session_manager`, and `term` alongside the existing capability list.
- [ ] `m3ctl` ships the three new client-side verbs documented in F.5; client-side smoke covers each verb against a live `session_manager`.

### I.3 — Evaluation doc updates

**Files:**
- `docs/evaluation/usability-roadmap.md`
- `docs/evaluation/gui-strategy.md`
- `docs/evaluation/roadmap/R09-display-and-input-architecture.md`

**Symbol:** N/A
**Why it matters:** The evaluation track is how "is m3OS a believable system yet?" gets answered. Phase 57 changes the answer materially; the evaluation docs must reflect the new baseline.

**Acceptance:**
- [ ] Each doc gains a Phase 57 entry summarizing what changed (audio path, session entry, terminal baseline, recovery contract).
- [ ] Each doc records what is now in scope for evaluation that was not before: end-to-end session entry, audible bell, recovery to text-mode, multi-client audio policy.
- [ ] R09's "next-step" section moves Phase 56's items to Done where Phase 57 closes them and names the remaining items as Phase 58+ candidates.

### I.4 — Roadmap README cross-links (status flip deferred)

**Files:**
- `docs/roadmap/README.md`
- `docs/roadmap/57-audio-and-local-session.md` (Companion Task List link only — Status stays as-is)

**Symbol:** N/A
**Why it matters:** The roadmap README must point at this task doc and the design doc must point at this task doc before I.5 can sensibly flip everything to Complete. I.4 lands the cross-links; I.5 lands the atomic status flip + version bump in one PR so there is never a window where docs say "Complete" but `kernel/Cargo.toml` still reads `0.56.0`.

**Acceptance:**
- [ ] `docs/roadmap/README.md` Phase 57 row links to this task doc and the design doc; Status column remains as the actual current value (no premature Complete flip).
- [ ] Phase 57 design doc's `Companion Task List` section links to this task doc.
- [ ] **Status flips for the design doc, this task doc, and the README row are explicitly deferred to I.5** so they ship atomically with the version bump in a single PR.

### I.5 — Status flip + version bump to `v0.57.0`

**Files:**
- `kernel/Cargo.toml`
- `AGENTS.md`
- `README.md`
- `docs/roadmap/README.md` (Phase 57 row Status → Complete)
- `docs/roadmap/57-audio-and-local-session.md` (Status → Complete)
- `docs/roadmap/tasks/57-audio-and-local-session-tasks.md` (this doc — Status → Complete; Track Layout table marks every track Done)

**Symbol:** N/A
**Why it matters:** The repo's declared version must stay in sync with the completed phase, and the roadmap status must flip in the same commit so there is never a window where docs claim Complete but the version still reads `0.56.0`. This task lands last and bundles the version bump with every Status flip.

**Acceptance:**
- [ ] `kernel/Cargo.toml` `version = "0.57.0"`.
- [ ] `AGENTS.md` project-overview line updated to `Kernel v0.57.0`.
- [ ] `README.md` version line updated.
- [ ] `docs/roadmap/README.md` Phase 57 row Status flipped to `Complete`.
- [ ] Phase 57 design doc Status flipped to `Complete`.
- [ ] This task doc's Status flipped to `Complete`; Track Layout table at the top marks every track `Done`.
- [ ] All Status flips and the version bump land in the same commit (or the same PR with a single squash-merge) — no intermediate commits leave the repo in a "Complete with old version" state.
- [ ] `cargo xtask check` passes.
- [ ] `cargo xtask test` passes.
- [ ] `cargo xtask smoke-test`, `cargo xtask audio-smoke`, and `cargo xtask session-smoke` all pass on the same commit.

---

## Rollback Plan

### Audio rollback (Tracks B–E)

If the chosen audio target's driver introduces a regression that a small patch cannot contain:

1. Revert Tracks D and E commits. `audio-demo` and `term`'s bell stop functioning; the kernel surface is dormant; no other path regresses.
2. Keep Tracks A, B, C landed — they are opt-in surfaces and contract-tested in pure logic. No driver besides `audio_server` consumes them yet.
3. Re-open Phase 57 for a second pass, optionally choosing a different target via a new A.1 memo revision.

### Session rollback (Track F)

If `session_manager` introduces a regression that bricks boot:

1. Revert Track F commits. The system reverts to the Phase 56 boot path: services start in their pre-existing manifest order, no graphical session orchestrator exists, and `term` may or may not start (G ships independently).
2. Keep Tracks A.4 (memo) landed. The contract is documented even if the implementation is rolled back.
3. The text-mode admin path is unchanged; serial console + existing shell remain reachable.

### Terminal rollback (Track G)

If `term` introduces a regression in display-server client behavior or PTY interaction:

1. Revert Track G commits. The graphical session reaches `Running` but has no terminal client; the local-system milestone is incomplete but no other path regresses.
2. Phase 56's `gfx-demo` continues to render its 16×16 surface as a sanity check.

### Version-bump rollback (I.5)

If post-bump validation surfaces a blocker:

1. Revert I.5 only — keep I.1, I.2, I.3, and I.4 cross-links landed.
2. Reverting I.5 returns the version string to `0.56.0` **and** rolls back the Status flips on the design doc, this task doc, and the README row (because I.5 lands all four atomically). The cross-links from I.4 stay in place.
3. Subsequent fix commits land a fresh I.5 once green.

---

## Documentation Notes

- **Learning doc is mandatory.** I.1 must be complete before the phase is marked Complete. The **aligned legacy learning doc** template from `docs/appendix/doc-templates.md` is the required shape — do not merge the phase-complete commit (I.5) without the learning doc in tree.
- **Engineering Discipline section is authoritative.** Where a later task re-states a TDD, SOLID, DRY, YAGNI, or related rule for emphasis, the rule in the Engineering Discipline section above is the canonical version.
- **What changed vs Phase 56.** Phase 56 added the userspace display-and-input architecture. Phase 57 turns that architecture into a coherent local-system milestone by adding (a) the first audio path, (b) a session-entry orchestrator, and (c) a useful graphical client. The kernel surface grows by exactly one device-claim entry (audio device) and one IRQ-notification slot — no new syscalls if A.3 chooses the userspace-IPC ABI.
- **Prefer exact files over directories.** Every task's **File** / **Files** entry names a concrete path, not a directory. If a file is renamed or split during implementation, update this doc before closing the task.
- **Prefer exact symbols over generic descriptions.** Every task's **Symbol** entry names the specific function, type, trait, or constant — not the module or crate. Generic descriptions like "audio module" are not acceptable.
- **Phase 55c bound notifications are exercised here.** D.4 is the second consumer of the `RecvResult` + `IrqNotification::bind_to_endpoint` pattern after e1000. Any divergence between e1000's loop shape and `audio_server`'s loop shape gets reconciled in `userspace/lib/driver_runtime` rather than fork-and-edit.
- **Single-client audio is a deliberate boundary.** Multi-client mixing, sample-rate conversion, capture, and routing are deferred per the design doc and the YAGNI rule. The "How Real OS Implementations Differ" section of the design doc is the canonical record of that boundary.
- **The four-step new-binary convention is non-negotiable.** Adding `audio_server`, `audio-demo`, `session_manager`, and `term` each requires updates in (1) workspace `members`, (2) xtask `bins`, (3) ramdisk `BIN_ENTRIES`, (4) service config + `KNOWN_CONFIGS`. Skipping any one of the four causes the binary to either not be built, not be embedded, or not be found at runtime — the resulting failure is silent at compile time.
- **Status flip + version bump land atomically.** I.4 lands cross-links only (Status columns stay as-is). I.5 lands the version bump together with every Status flip (design doc, this task doc, README row) in a single PR so the repo never enters a "Complete with old version" state. I.5 must not merge until I.1 (learning doc), I.2 (subsystem updates), I.3 (evaluation updates), and I.4 (cross-links) have all merged.
