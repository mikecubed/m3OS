# Phase 57 Track A.3 — Audio ABI Shape

**Status:** Decided
**Source Ref:** phase-57
**Scope:** A.3 (this memo) — informs B.3 (`ClientMessage` / `ServerMessage` codec), C.3 (kernel substrate), D.2/D.3 (audio_server backend + stream), and E.1 (`audio_client` library)
**Cross-links:**
- Phase 57 design doc — [`docs/roadmap/57-audio-and-local-session.md`](../roadmap/57-audio-and-local-session.md)
- Phase 57 audio target choice — [`docs/appendix/phase-57-audio-target-choice.md`](./phase-57-audio-target-choice.md)
- Phase 57 service topology — [`docs/57-audio-and-local-session.md`](../57-audio-and-local-session.md)
- Phase 56 display server (precedent for pure-userspace IPC) — [`docs/56-display-and-input-architecture.md`](../56-display-and-input-architecture.md)
- Phase 55b ring-3 driver host (precedent for kernel-mediated facade) — [`docs/55b-ring-3-driver-host.md`](../55b-ring-3-driver-host.md)
- Phase 55c net-send shape — [`docs/appendix/phase-55c-net-send-shape.md`](./phase-55c-net-send-shape.md)

---

## Decision

**Phase 57's audio surface is a pure-userspace IPC contract on `audio_server`'s AF_UNIX listening endpoint.** The kernel gains **no** new audio syscalls and **no** kernel-side facade analogous to `RemoteBlockDevice` or `RemoteNic`. Userspace clients link `userspace/lib/audio_client`, which speaks the wire format declared once in `kernel-core::audio::protocol` (B.3).

Rejected alternative: a `sys_audio_open` / `sys_audio_submit_frames` / `sys_audio_drain` / `sys_audio_close` syscall block mirroring `sys_block_*` and routed through a `RemoteAudio` kernel facade.

## Rationale

Two precedents are live in the codebase. The choice is **which one to follow** for audio, not whether to invent a third pattern.

| Precedent | Pattern | When it applies |
|---|---|---|
| Phase 56 `display_server` | Pure-userspace IPC: AF_UNIX listening socket; protocol declared once in `kernel-core::display::protocol`; client lib (`display_client`) speaks the protocol; kernel learns no display semantics; the only kernel surface is `sys_fb_acquire` + page-grant transport, both of which are general-purpose primitives that pre-existed | When the device is owned by **one** ring-3 service and clients talk to it through ordinary IPC. The kernel doesn't need to multiplex requests across legacy callers |
| Phase 55b `RemoteBlockDevice` / `RemoteNic` | Kernel-mediated facade: ring-3 driver registers an IPC endpoint with the kernel; the kernel keeps a struct (`RemoteBlockDevice`, `RemoteNic`) that legacy callers (the VFS, the TCP stack) talk to via existing kernel APIs (`block::read_blocks`, `net::send_frame`); the facade forwards to the userspace driver over IPC and routes errors through `*_error_to_neg_errno` helpers | When **legacy kernel callers** need to talk to a device that has moved out of ring 0 and the existing kernel API surface (`sys_read` on a block-backed file, `sys_sendto` on a UDP socket) must keep working unchanged |

Audio matches the **first** pattern. There are no legacy kernel callers for audio — Phase 57 is the first audio path the system has ever had. There is no `sys_audio_*` legacy ABI to preserve. There is no kernel subsystem (analogous to the VFS or the TCP stack) that needs to reach the device through the kernel. Every audio caller in Phase 57 — the `audio-demo` reference client, `term`'s bell, future media clients — is a userspace process that can link `audio_client` and speak IPC directly.

The pure-userspace shape buys three concrete things over a kernel-mediated shape:

1. **Smaller blast radius.** A kernel-mediated facade adds new syscall arms in `sys_dispatch` (per the existing pattern), new error-routing in `audio_error_to_neg_errno` callsites in the kernel, new restart-gate atomics if the userspace driver can be in mid-restart, and new tests against `kernel-core::driver_ipc::audio` of the kind the e1000 path needs (the `check_restart_gate` shape from Phase 55c). None of that adds value for audio because no kernel subsystem needs the facade.

2. **Consistency with Phase 56.** `term` (the Phase 57 graphical terminal) is already a `display_server` client speaking the Phase 56 wire protocol over AF_UNIX; making it a pure-IPC `audio_server` client too means the same connection model, the same length-prefixed framing convention, and the same single-threaded io-loop discipline applies to both endpoints. A kernel-mediated audio path would force `term` to mix two surface shapes.

3. **The driver restart story is local.** When `audio_server` crashes and the supervisor restarts it (D.6), the kernel is uninvolved in the recovery beyond reaping the dead PID and re-registering the new endpoint via the existing service registry. A kernel facade would force the kernel to track an `audio_server` registration state (per `RemoteNic::is_registered()` / `RemoteNic::check_restart_gate()`) and to gate every audio syscall on that state — adding one more atomic-flag pair to the syscall hot path with no clients that need it.

The cost of pure-userspace IPC is one well-understood: the audio client cannot reach `audio_server` through a libc-style POSIX wrapper that pretends audio is a file (`/dev/dsp`-style). Phase 57 deliberately is **not** a POSIX-audio compatibility milestone; the userspace `audio_client` library is the public surface, and the only workspace consumers that need audio in Phase 57 are `audio-demo` and `term`, both of which link `audio_client` directly. A future POSIX-audio shim is an additive userspace project that doesn't force a rewrite.

## Wire-format shape (codified in B.3)

The pure-userspace IPC contract is a length-prefixed binary framing on AF_UNIX, identical in framing convention to Phase 56's `display_server` client protocol. The four message families are declared once in `kernel-core::audio::protocol`:

| Family | Direction | Variants |
|---|---|---|
| `ClientMessage` | client → audio_server | `OpenStream { format, layout, rate }`, `SubmitFrames { stream_id, byte_count }` (followed by `byte_count` bytes), `Drain { stream_id }`, `CloseStream { stream_id }` |
| `ServerMessage` | audio_server → client | `OpenStreamReply { stream_id, OK or Err(AudioError) }`, `SubmitFramesReply { bytes_accepted, OK or Err }`, `DrainReply { OK or Err }`, `CloseStreamReply { OK or Err }`, `BufferReleased { stream_id, frame_count }` (event published when the device drains a chunk) |
| `AudioControlCommand` | control client → audio_server's control endpoint | `version`, `stream-stats`, `stop-stream { stream_id }` (admin verb) |
| `AudioControlEvent` | audio_server → subscribed control clients | `StreamOpened { stream_id }`, `StreamClosed { stream_id }`, `Underrun { stream_id, frame_count }` |

`SubmitFrames` is the only variant that carries a bulk payload. The recommendation in A.1 places the userspace API at `submit_frames(&[u8])`; the wire shape splits the call into one length-prefixed control frame (`SubmitFrames { stream_id, byte_count }`) followed by `byte_count` raw bytes on the same socket. This keeps the codec round-trip property (`decode(encode(msg)) == msg`) verifiable on the control frame alone — the bulk payload is opaque PCM and is not part of the codec's invariants.

### Maximum bulk size for a single `SubmitFrames`

**`MAX_SUBMIT_BYTES = 64 * 1024` (64 KiB).** This is the upper bound on the audio DMA ring per the resource-bounds rule in the task list (`Audio DMA ring size: at least 4 KiB and at most 64 KiB, recorded as named constants in kernel-core::audio`). Allowing a single submit larger than the ring would force `audio_server` to either fragment internally or block waiting for the device to drain — adding policy that belongs to the client, not to the server. A submit larger than 64 KiB returns `-EINVAL` immediately at the codec; the client must split into multiple `SubmitFrames` messages. Smaller submits are fully accepted; the server returns `bytes_accepted < byte_count` only when the ring is partially full, in which case the client retries with the remainder per the standard `WouldBlock`/`-EAGAIN` semantics.

The constant lives once in `kernel-core::audio::protocol` (`MAX_SUBMIT_BYTES`) so `audio_server` (D.3, server side), `audio_client` (E.1, client side), and the `audio-demo` reference (E.2) all consume the same value. A workspace-wide grep for `MAX_SUBMIT_BYTES` returns exactly one declaration site.

## Files affected (exact paths)

### Files added under the chosen shape

| File | Track | Purpose |
|---|---|---|
| `kernel-core/src/audio/protocol.rs` | B.3 | `ClientMessage`, `ServerMessage`, `AudioControlCommand`, `AudioControlEvent`, `encode`, `decode`, `ProtocolError`, `MAX_SUBMIT_BYTES` |
| `kernel-core/src/audio/format.rs` | B.1 | `PcmFormat`, `SampleRate`, `ChannelLayout`, `frame_size_bytes` |
| `kernel-core/src/audio/ring.rs` | B.2 | `AudioRingState`, `AudioSink` trait, `RingError` |
| `kernel-core/src/audio/errno.rs` | B.5 | `audio_error_to_neg_errno` |
| `kernel-core/src/audio/ring_proptest.rs` | B.4 | property tests |
| `kernel-core/src/audio/mod.rs` | B.1 | re-exports the public surface |
| `userspace/lib/audio_client/Cargo.toml` | E.1 | client library manifest |
| `userspace/lib/audio_client/src/lib.rs` | E.1 | `AudioClient::open` / `submit_frames` / `drain` / `close`; consumes `kernel-core::audio::protocol` |
| `userspace/audio_server/Cargo.toml` | D.1 | server crate manifest |
| `userspace/audio_server/src/main.rs` | D.1 | server entry point + io loop scaffold |
| `userspace/audio_server/src/device.rs` | D.2 | `AudioBackend` trait + `Ac97Backend` impl |
| `userspace/audio_server/src/stream.rs` | D.3 | single-stream `Stream` + `StreamRegistry` |
| `userspace/audio_server/src/irq.rs` | D.4 | `subscribe_and_bind`, `run_io_loop` (Phase 55c bound-notification multiplex) |
| `userspace/audio_server/src/client.rs` | D.5 | `ClientRegistry` (single-client policy + `-EBUSY`) |
| `etc/services.d/audio_server.conf` | D.1, D.6 | supervised driver manifest |
| `userspace/audio-demo/{Cargo.toml,src/main.rs}` | E.2 | reference client |

### Files modified under the chosen shape

| File | Track | Reason |
|---|---|---|
| `Cargo.toml` (workspace) | D.1, E.1, E.2 | add new crates to `members` |
| `xtask/src/main.rs` (`bins` array) | D.1, E.2 | build `audio_server` and `audio-demo`; both `needs_alloc = true` |
| `xtask/src/main.rs` (`populate_ext2_files`) | D.1 | drop `audio_server.conf` into the data disk |
| `xtask/src/main.rs` (`qemu_args_with_devices_resolved`) | D.1 (or H) | emit `-device AC97,audiodev=snd0` when `DeviceSet::audio` is set, per A.1 |
| `kernel/src/fs/ramdisk.rs` (`BIN_ENTRIES`) | D.1, E.2 | embed the `audio_server` and `audio-demo` ELF binaries |
| `userspace/init/src/main.rs` (`KNOWN_CONFIGS`) | D.1 | recognize `audio_server.conf` |
| `kernel/src/device_host/mod.rs` | C.1 | recognize `0x8086:0x2415` (Intel AC'97) as a valid claim target — extension to existing claim path, no new syscall |

### Files NOT changed under the chosen shape (and would have been changed under the rejected one)

| File | Why this stays unchanged |
|---|---|
| `kernel/src/audio/mod.rs` | **Does not exist** under the chosen shape. C.3's task wording explicitly allows this module to collapse to zero new files when A.3 chooses pure-userspace IPC. The rejected shape would have created `kernel/src/audio/mod.rs` and `kernel/src/audio/remote.rs` for a `RemoteAudio` facade |
| `kernel/src/arch/x86_64/syscall/mod.rs` | No new `sys_audio_*` syscall arms. No new `check_restart_gate` calls. Existing syscall dispatch is byte-identical |
| `kernel-core/src/driver_ipc/` | No new `kernel-core::driver_ipc::audio` module mirroring `kernel-core::driver_ipc::net`. The codec lives in `kernel-core::audio::protocol`, not in `driver_ipc` — userspace consumes it directly |
| `userspace/syscall-lib/src/` | No new `sys_audio_*` wrappers — `audio_client` speaks IPC, not syscalls |

## Consistency with Phase 56 and Phase 55c precedents

- **Phase 56 precedent.** `display_server` ships exactly this shape: AF_UNIX listening socket, length-prefixed framing declared in `kernel-core::display::protocol`, no new kernel syscalls beyond `sys_fb_acquire` + `sys_fb_release` (which existed for the framebuffer itself). A.3 follows the precedent verbatim. The only structural difference is that audio uses `sys_device_claim` (Phase 55b) rather than `sys_fb_acquire` to obtain its hardware — a difference that lives entirely in the device-acquisition step, not in the wire format or the io-loop shape.

- **Phase 55c precedent.** Phase 55c added `RemoteNic::check_restart_gate()` because legacy kernel callers (the `sys_sendto` UDP and ICMP branches) had to observe `EAGAIN` while the e1000 driver was mid-restart. There is no analogous legacy kernel caller for audio. If a future phase adds one (e.g., a kernel-side beep that wants to ride `audio_server`), that phase introduces the kernel facade then — A.3's decision is forward-compatible because adding a `RemoteAudio` later is purely additive (a new kernel module with the same `RemoteNic` shape) and does not change the userspace contract.

- **Phase 55b ring-3 driver host pattern.** The chosen shape uses `sys_device_claim`, `sys_device_mmio_map`, `sys_device_dma_alloc`, `sys_device_irq_subscribe` exactly as `userspace/drivers/e1000` and `userspace/drivers/nvme` do. The `audio_server` process is a Phase 55b-style supervised driver on the kernel side, and a Phase 56-style userspace IPC owner on the client-facing side. Those two roles compose cleanly because the syscalls are about hardware acquisition and the IPC is about client coordination — different concerns, different surfaces.

## Acceptance trace

This memo discharges A.3's acceptance:

- [x] Names the chosen shape (pure-userspace IPC) and the rationale (smaller blast radius, consistency with Phase 56 `display_server`, no legacy kernel callers).
- [x] References the existing precedent: Phase 56 `display_server` for the chosen shape; Phase 55b `RemoteBlockDevice` / `RemoteNic` for the rejected shape.
- [x] Lists the exact files that change under the chosen shape (table above) and the files that would have changed under the rejected shape (`kernel/src/audio/mod.rs`, `kernel/src/audio/remote.rs`, syscall dispatch, `kernel-core::driver_ipc::audio`).
- [x] Records the maximum bulk size for a single `SubmitFrames` — `MAX_SUBMIT_BYTES = 64 * 1024` (64 KiB), matching the audio DMA ring upper bound in the task list — and the rationale (a single submit larger than the ring forces server-side fragmentation policy that belongs to the client).
