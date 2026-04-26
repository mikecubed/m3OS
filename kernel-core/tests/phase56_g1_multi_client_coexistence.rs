//! Phase 56 Track G.1 — Multi-client coexistence regression (DEFERRED to QEMU).
//!
//! ## Why this file exists
//!
//! G.1's full acceptance contract is "two graphical clients connect to
//! `display_server`, each commits a distinct color, and a pixel-sampling
//! harness reads back the framebuffer to confirm both colors are
//! present." The harness path the spec calls for goes through the
//! control socket (E.4) and a hypothetical `read-back` test verb.
//!
//! ## Status: Phase 56 close-out
//!
//! The kernel-side **bulk-drain gap is closed** in the Phase 56 close-out:
//! `SYS_IPC_TAKE_PENDING_BULK = 0x1112` (`syscall_lib::ipc_take_pending_bulk`)
//! drains the caller's `pending_bulk` slot, and `SYS_IPC_TRY_RECV_MSG = 0x1113`
//! (`syscall_lib::ipc_try_recv_msg`) lets `display_server`'s main loop serve
//! the control endpoint without blocking. The three `// TODO(C.5-bulk-drain)`
//! markers from the original deferral are resolved:
//!
//! * `userspace/m3ctl/src/main.rs` — now decodes the kernel-staged reply bulk
//!   via `decode_event(reply_buf)` instead of the synthetic reply.
//! * `userspace/display_server/src/input.rs` — `KbdInputSource::poll_key` and
//!   `MouseInputSource::poll_pointer` drain the reply bulk into typed wire
//!   buffers (`KEY_EVENT_WIRE_SIZE = 19`, `POINTER_EVENT_WIRE_SIZE = 37`).
//! * `userspace/display_server/src/main.rs::serve_one_control_request` —
//!   the main loop multiplexes frame-tick driving and control-endpoint
//!   serving via the new try-recv.
//!
//! ## What still defers G.1 to a follow-on
//!
//! Two graphical clients running concurrently need a QEMU regression
//! harness similar to F.2's `display-server-crash-recovery`: a guest
//! binary that forks two child clients, drives them through their
//! `Hello → CreateSurface → AttachBuffer → CommitSurface` flows with
//! distinct colors, then reads back the framebuffer through a (still
//! TBD) test-only control-socket verb. That harness is bounded but
//! non-trivial and lives outside the kernel/userspace IPC fix landed
//! here. The pure-logic invariants the production code enforces are
//! already covered by:
//!
//! * `kernel-core::display::compose` — damage arithmetic, layer
//!   ordering, clipping, multi-surface compose order.
//! * `kernel-core::input::dispatch` — focus routing, exclusive-keyboard
//!   gating, modifier tracking. (Track D.3 — 22 host tests.)
//! * `kernel-core::display::surface` — `SurfaceStateMachine`
//!   create / role / commit / destroy lifecycle.
//! * `kernel-core::display::layer` — anchor + margin geometry,
//!   exclusive-rect derivation, conflict tracker.
//!
//! The remaining signal G.1 wants — "two distinct colors land in the
//! framebuffer and a test harness sees both" — needs the QEMU
//! integration test, not new kernel work.
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
//! ## How to lift the deferral (post-bulk-drain)
//!
//! 1. Add a guest binary `userspace/display-multi-client-smoke/`
//!    modelled on `userspace/display-server-crash-smoke/` that drives
//!    two `gfx-demo`-flavoured clients with different fill colors.
//! 2. Add a test-only `ControlCommand::ReadBackPixel { x, y }` verb
//!    gated by an env var (mirrors F.2's
//!    `M3OS_DISPLAY_SERVER_DEBUG_CRASH=1`) so production builds reject
//!    the verb but regression builds expose it.
//! 3. Add `cargo xtask regression --test multi-client-coexistence` that
//!    boots with the marker, runs the smoke, and asserts both colors
//!    appear at their expected positions.
//! 4. Replace this file's `#[ignore]` test with a thin host-side
//!    assertion that the protocol shapes compose for the harness, OR
//!    delete this file once the QEMU regression is the canonical
//!    signal.
//!
//! Any commit lifting this deferral should update
//! `.flow/artifacts/track-report-phase56-g.md` accordingly.

#![cfg(feature = "std")]

#[test]
#[ignore = "Phase 56 G.1 multi-client coexistence regression deferred to \
            QEMU integration. The kernel-side bulk-drain gap is closed \
            (SYS_IPC_TAKE_PENDING_BULK = 0x1112, SYS_IPC_TRY_RECV_MSG = \
            0x1113); only the QEMU harness remains. See file header for \
            the lift plan."]
fn multi_client_coexistence_deferred() {
    // Intentionally empty. The `#[ignore]` annotation prevents this
    // from running by default; `cargo test --ignored` surfaces it
    // (and the message above) so a future closure-task author cannot
    // miss the deferral.
    //
    // The bulk-drain gap that originally blocked this test is closed
    // in the Phase 56 close-out. The remaining lift is a QEMU
    // regression harness — see file header.
    panic!(
        "G.1 multi-client coexistence regression is deferred to QEMU \
         integration. The bulk-drain gap is closed; see this file's \
         module-level docstring for the lift plan."
    );
}
