# Bug: `kernel::net::remote::tests` use the wrong header encoder for RX-path tests

**Status:** Open. Found during Phase 56 close-out (`cargo xtask test`); recorded
here so a follow-up session can pick it up without re-investigating.

**Related:**
- Phase 55c (PR #118) — introduced the failing tests
- Phase 56 close-out — see `docs/roadmap/56-phase-56-completion-gaps.md` § 5.2
- Branch where surfaced: `feat/phase-56-tracks-d-h`, commit `048c3d3`

## Symptom

Running `cargo xtask test` on `feat/phase-56-tracks-d-h` (and on `main`,
once `frame_allocator::contiguous_alloc_recovers_order0_hoarding` is
unblocked — see § 5.2) hits this panic:

```
kernel::net::remote::tests::drain_rx_queue_removes_malformed_frames_after_deferred_queueing
  [INFO]  [remote_nic] registered ring-3 NIC driver: endpoint=EndpointId(5) mac=00:00:00:00:00:00
  [WARN]  [remote_nic] RX: bad NET_RX_FRAME header at offset 0: InvalidFrame
  [failed]

Error: panicked at kernel/src/net/remote.rs:743:9:
assertion `left == right` failed
  left: 0
 right: 1
```

The `InvalidFrame` warn line is the smoking gun: the encoded payload is
being rejected by `decode_net_rx_notify` because it carries the wrong
`kind` label.

## Root cause

The protocol library at `kernel-core/src/driver_ipc/net.rs` defines
**two** encoders, one per direction:

```rust
// encode_header_with_kind always overwrites caller's `header.kind`:
fn encode_header_with_kind(kind: u16, header: NetFrameHeader) -> Vec<u8> {
    let mut out = vec![0u8; NET_FRAME_HEADER_SIZE];
    // Overwrite `kind` with the declared label; the caller is always the
    // authoritative side for which direction this header belongs to.
    out[0..2].copy_from_slice(&kind.to_le_bytes());
    ...
}

pub fn encode_net_send(header: NetFrameHeader) -> Vec<u8> {
    encode_header_with_kind(NET_SEND_FRAME, header)   // 0x5511
}

pub fn encode_net_rx_notify(header: NetFrameHeader) -> Vec<u8> {
    encode_header_with_kind(NET_RX_FRAME, header)     // 0x5512
}
```

`encode_net_send`'s docstring says: *"The encoder always stamps `kind =
NET_SEND_FRAME` regardless of what the caller supplied"*.

The decoders mirror that — `decode_net_rx_notify` rejects bytes whose
`kind` field is anything other than `NET_RX_FRAME`:

```rust
fn decode_header_with_kind(expected_kind: u16, bytes: &[u8])
    -> Result<NetFrameHeader, NetDriverError>
{
    ...
    let kind = u16::from_le_bytes([bytes[0], bytes[1]]);
    if kind != expected_kind {
        return Err(NetDriverError::InvalidFrame);
    }
    ...
}
```

The three RX-path tests in `kernel/src/net/remote.rs` (added in PR #118)
all do:

```rust
use kernel_core::driver_ipc::net::{NET_RX_FRAME, NetFrameHeader, encode_net_send};

let header = NetFrameHeader {
    kind: NET_RX_FRAME,         // <-- intent: RX
    frame_len: ...,
    flags: 0,
};
let mut payload = encode_net_send(header).to_vec();   // <-- bug: encodes as NET_SEND_FRAME
payload.extend_from_slice(&frame);

assert_eq!(RemoteNic::inject_rx_frame(&payload), 1);  // <-- inject_rx_frame's
                                                      //     decode_net_rx_notify rejects,
                                                      //     returns 0, assertion fails
```

The test imports `NET_RX_FRAME` and sets `header.kind = NET_RX_FRAME`,
but feeds the header through `encode_net_send` — which stamps
`NET_SEND_FRAME` (0x5511) over the supplied `NET_RX_FRAME` (0x5512). The
test author probably intended `encode_net_rx_notify` and copy-pasted
`encode_net_send` from the TX-path tests above without noticing the
encoder's "always-stamps" contract.

## Tests affected

All three RX-path tests added in PR #118 are at `kernel/src/net/remote.rs`:

| Line | Test | Expected | Observed |
|---|---|---|---|
| ~707 | `inject_rx_frame_queues_payload_for_deferred_dispatch` | inject returns 1, queue length 1 | inject returns 0, queue length 0 |
| ~727 | `drain_rx_queue_removes_malformed_frames_after_deferred_queueing` | inject returns 1, drain returns 0 (dropped because frame is structurally malformed in the protocol-stack-level sense, even though the header decode succeeds) | inject returns 0, never reaches drain assertion |
| ~748 | `inject_rx_frame_queues_each_record_in_a_multi_frame_batch` | inject returns 3, queue length 3 | inject returns 0 (first record's kind mismatch breaks the loop), queue length 0 |

The xtask harness stops at the first failing test, so only test #2 is
observable in the panic output. Tests #1 and #3 will start failing as
soon as test #2 is fixed — fix all three at once.

## Why CI didn't catch this in PR #118

Best guess: the `frame_allocator::contiguous_alloc_recovers_order0_hoarding`
failure (recorded in `docs/roadmap/56-phase-56-completion-gaps.md` § 5.2,
fixed in this PR) was already pre-existing on `main` at the time of PR
#118 and short-circuited the kernel test suite before reaching
`net::remote::tests::*`. PR #118's CI passed because xtask stops at the
first failure and the frame allocator test panicked first.

The frame_allocator fix landed in commit `048c3d3` of PR #124, which
unblocks the kernel test suite to actually reach the
`net::remote::tests::*` cluster — at which point this latent bug
surfaces.

## Reproduction

On any branch that contains the fix in `048c3d3` (frame_allocator
contiguous-reclaim symmetric-snapshot fix):

```bash
cargo xtask test 2>&1 | grep -A2 "drain_rx_queue_removes_malformed"
```

Should reproduce the same panic at `kernel/src/net/remote.rs:743:9`.

To diagnose without the frame_allocator fix, on `main`:

```bash
git stash  # save any local work
cargo xtask test 2>&1 | grep -E "drain_rx_queue|frame_allocator" | head
# Will show frame_allocator failing first; net::remote::tests::* won't run
```

## Fix

One-line change per test — replace the `encode_net_send` import and call
sites with `encode_net_rx_notify`. Concretely, in
`kernel/src/net/remote.rs`:

```diff
-        use kernel_core::driver_ipc::net::{NET_RX_FRAME, NetFrameHeader, encode_net_send};
+        use kernel_core::driver_ipc::net::{NET_RX_FRAME, NetFrameHeader, encode_net_rx_notify};
         ...
-        let mut payload = encode_net_send(header).to_vec();
+        let mut payload = encode_net_rx_notify(header).to_vec();
```

Apply this in all three `#[test_case]` functions and in the `bulk` loop
inside `inject_rx_frame_queues_each_record_in_a_multi_frame_batch`.

The `NET_RX_FRAME` import on each test is now redundant (the encoder
stamps it itself) but keeping it documents intent — leave it in.

After the fix, run:

```bash
cargo xtask test 2>&1 | grep "net::remote::tests"
```

All three should be `[ok]`. If any other failure surfaces afterward,
follow-up: file a similar bug doc for it.

## Estimated effort

- Fix: 5 minutes (one-line diff × 3 tests, plus an import line).
- Verification: 5 minutes (one `cargo xtask test` run).
- Commit + PR: 10 minutes.
- Total: ~20 minutes.

## Why this is out of scope for Phase 56

Phase 56 is the display + input architecture. `kernel/src/net/remote.rs`
is the ring-3 NIC driver host (Phase 55b) plus its correctness closure
(Phase 55c). No Phase 56 commit touches `net::remote` or its tests; the
bug is fully contained in PR #118's test scaffolding.

Phase 56 close-out captures this in `docs/roadmap/56-phase-56-completion-gaps.md`
§ 5.2 as a follow-up so the close doesn't get blocked on an unrelated
issue.

## When picking this up

A follow-up session should:

1. Branch from `main` after PR #124 lands: `git checkout -b fix/55c-net-remote-rx-test-encoder-mismatch main`.
2. Apply the diff above to all three RX-path tests.
3. Run `cargo xtask test` — expect either a clean run, or a *different*
   pre-existing failure to surface next.
4. If a new failure surfaces, file a sibling doc under
   `docs/roadmap/follow-ups/` and stop. Do not chain fixes.
5. Commit with message `fix(net::remote): use encode_net_rx_notify in RX-path tests`.
6. Open a small PR to `main`. The diff should be ~6 lines.

This is a small, scoped fix — bias toward fast turnaround.
