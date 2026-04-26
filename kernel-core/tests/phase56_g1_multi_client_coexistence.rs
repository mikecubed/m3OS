//! Phase 56 Track G.1 — Multi-client coexistence regression (DEFERRED).
//!
//! ## Why this file exists
//!
//! G.1's full acceptance contract is "two graphical clients connect to
//! `display_server`, each commits a distinct color, and a pixel-sampling
//! harness reads back the framebuffer to confirm both colors are
//! present." The harness path the spec calls for goes through the
//! control socket (E.4) and a hypothetical `read-back` test verb.
//!
//! ## What blocks runtime byte-flow today: the bulk-drain gap
//!
//! D.3 documented (and E.4 confirmed) that there is *no* userspace
//! helper for draining the reply-bulk of an `ipc_call_buf`. The kernel
//! transfers the bulk into the caller's `pending_bulk` slot but no
//! syscall reads it back. Three `// TODO(C.5-bulk-drain)` markers in
//! the codebase hold the swap point:
//!
//! * `userspace/m3ctl/src/main.rs` — uses `synthetic_reply_for(&cmd)`
//!   instead of decoding the kernel-staged reply bulk.
//! * `userspace/display_server/src/input.rs::lookup_with_backoff` —
//!   accepts the kernel-side staging but cannot observe it from the
//!   caller.
//! * `userspace/display_server/src/control.rs` — the subscription event
//!   path stages outbound bytes that the caller cannot drain.
//!
//! Until the kernel-side syscall lands, runtime byte-flow regressions
//! that depend on a client receiving server-sent bytes — including
//! G.1's pixel-sampling path — cannot be verified. Phase 56 G-track
//! delivers what *is* verifiable today (the pure-logic invariants the
//! production code enforces) and explicitly defers runtime byte-flow
//! to a follow-on Phase 56 closure task.
//!
//! ## What is covered by other host tests
//!
//! The pure-logic surface this test would exercise is already covered:
//!
//! * `kernel-core::display::compose` — damage arithmetic, layer
//!   ordering, clipping, multi-surface compose order. (Track C.4
//!   property test + recording-FB unit suite.)
//! * `kernel-core::input::dispatch` — focus routing, exclusive-keyboard
//!   gating, modifier tracking. (Track D.3 — 22 host tests.)
//! * `kernel-core::display::surface` — `SurfaceStateMachine`
//!   create / role / commit / destroy lifecycle.
//! * `kernel-core::display::layer` — anchor + margin geometry,
//!   exclusive-rect derivation, conflict tracker.
//!
//! Multi-client coexistence at the *protocol* layer (no shared
//! mutable state, separate surface-id allocations, distinct framebuffer
//! ownership rooted in `display_server`'s `KernelFramebufferOwner`) is
//! a corollary of those — the production code never branches on
//! "how many clients" in the layers G.1 cares about. The remaining
//! signal G.1 wants — "two distinct colors land in the framebuffer
//! and a test harness sees both" — is what bulk-drain unblocks.
//!
//! ## How to lift the deferral
//!
//! When `syscall_lib::ipc_take_pending_bulk` (or equivalent) lands:
//!
//! 1. Replace the `synthetic_reply_for` arm in `m3ctl` with a real
//!    `decode_event(reply_bulk)`.
//! 2. Replace `display_server::input::lookup_with_backoff`'s TODO with
//!    a real drain.
//! 3. Replace this file's `#[ignore]` test with a runnable QEMU smoke
//!    that drives two `gfx-demo`-flavoured clients with different
//!    fill colors, asks `m3ctl frame-stats` for the latest sample,
//!    and asserts the registry's `iter_compose` sequence places both
//!    surfaces — *and* that a pixel readback (via a new test-only
//!    control-socket verb) shows both colors at their layout-derived
//!    positions.
//!
//! Any commit lifting this deferral should also drop the matching
//! TODOs and update `.flow/artifacts/track-report-phase56-g.md`.

#![cfg(feature = "std")]

#[test]
#[ignore = "Phase 56 G.1 runtime byte-flow regression deferred behind \
            the userspace bulk-drain gap (C.5-bulk-drain); see file \
            header for the deferral rationale and lift-plan."]
fn multi_client_coexistence_deferred() {
    // Intentionally empty. The `#[ignore]` annotation prevents this
    // from running by default; `cargo test --ignored` surfaces it
    // (and the message above) so a future closure-task author cannot
    // miss the deferral.
    //
    // When the bulk-drain syscall lands, replace this file with a
    // runnable test per the lift plan in the file header.
    panic!(
        "G.1 multi-client coexistence regression is deferred behind \
         the userspace bulk-drain gap (TODO(C.5-bulk-drain)). See \
         this file's module-level docstring for the lift plan."
    );
}
