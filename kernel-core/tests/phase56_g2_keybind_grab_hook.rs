//! Phase 56 Track G.2 — Keybind grab-hook regression (PARTIAL).
//!
//! ## What G.2 wants end-to-end
//!
//! The full G.2 acceptance path:
//!
//! 1. A test client gains focus.
//! 2. `m3ctl register-bind MOD_SUPER+q` registers a grab.
//! 3. A synthetic `KeyDown` for `SUPER+q` is injected through the input
//!    path (via a test-only input-injection control-socket verb).
//! 4. The focused client receives **no** `KeyEvent`.
//! 5. A `BindTriggered` event is observed on the subscribed control
//!    stream.
//! 6. A subsequent `KeyDown` for `q` (no modifier) is delivered
//!    normally to the focused client.
//!
//! Phase 56 close-out: the userspace bulk-drain syscall has landed
//! (`ipc_take_pending_bulk` / 0x1112) and the `InjectKey` control-
//! socket verb is wired, so the runtime grab-hook regression is
//! testable end-to-end — but only inside QEMU. The runtime
//! verification ships as a separate guest binary
//! (`userspace/grab-hook-smoke`, version 0.56.0) which the
//! supervisor launches against a live `display_server`. The
//! host-process `cargo test` cannot orchestrate the multi-process
//! flow (synthetic key injection through the dispatcher + cross-
//! process focus state + control-socket subscription readback), so
//! this file pins the *pure-logic* invariants the runtime path
//! depends on, and the QEMU smoke pins the cross-process behavior.
//!
//! ## What this file *does* cover
//!
//! The two key invariants the runtime path relies on, against the
//! production `BindTable`:
//!
//! * `BindTable::register(MOD_SUPER + 'q')` followed by
//!   `BindTable::match_bind(MOD_SUPER, 'q')` returns the registered
//!   `BindId` (the key would be swallowed).
//! * `BindTable::match_bind(0, 'q')` (same keycode, no modifier) returns
//!   `None` (the key would be forwarded normally).
//!
//! These are the same predicates `display_server::input::process_event`
//! evaluates per keystroke. A regression in either invariant would
//! show up as either swallowed normal keys or non-swallowed grabs —
//! i.e. the G.2 acceptance contract collapses.
//!
//! `kernel-core::input::bind_table` already exercises the same
//! invariants in its inline `#[cfg(test)]` mod; this file pins them
//! at the integration-test level (out-of-process `cargo test`) so a
//! reviewer running `cargo test -p kernel-core --test
//! phase56_g2_keybind_grab_hook` sees a named G.2 signal in the
//! output.
//!
//! ## QEMU-only coverage (Phase 56 close-out)
//!
//! Three checks that require a live multi-process compositor + input
//! pipeline are exercised by `userspace/grab-hook-smoke` and not by
//! this file:
//!
//! * synthetic key injection through the dispatcher;
//! * `BindTriggered` event observation on the subscribed control
//!   stream;
//! * "focused client receives no `KeyEvent`" cross-process check.
//!
//! These all use the bulk-drain syscall (`ipc_take_pending_bulk`)
//! and the `InjectKey` control verb — both of which landed in this
//! PR — and verify behavior that pure-logic host tests cannot reach.

#![cfg(feature = "std")]

use kernel_core::input::bind_table::{BindKey, BindTable};
use kernel_core::input::events::{MOD_CTRL, MOD_SHIFT, MOD_SUPER};

fn key(modifier_mask: u16, keycode: u32) -> BindKey {
    BindKey {
        modifier_mask,
        keycode,
    }
}

#[test]
fn registered_super_q_bind_is_matched_when_super_held() {
    // G.2 acceptance bullet 2 (partial — no runtime injection):
    // registering `MOD_SUPER + q` is recorded in the `BindTable` so
    // the dispatcher's match-on-down path will swallow it.
    let mut t = BindTable::new();
    let id = t
        .register(key(MOD_SUPER, b'q' as u32))
        .expect("first registration should succeed");
    assert_eq!(
        t.match_bind(MOD_SUPER, b'q' as u32),
        Some(id),
        "production dispatcher would observe a bind hit for SUPER+q",
    );
}

#[test]
fn unmodified_q_does_not_match_super_q_bind() {
    // G.2 acceptance bullet 6 (partial — no runtime delivery): a
    // subsequent `KeyDown` for plain `q` (no modifier) does not
    // match the SUPER+q bind, so production dispatcher would forward
    // it to the focused client.
    let mut t = BindTable::new();
    let _id = t
        .register(key(MOD_SUPER, b'q' as u32))
        .expect("registration ok");
    assert_eq!(
        t.match_bind(0, b'q' as u32),
        None,
        "no bind matches when no modifier is held; production would forward q to focused client",
    );
}

#[test]
fn additional_modifiers_do_not_match_exact_bind() {
    // The dispatcher uses an exact-modifier-mask match; SUPER+SHIFT+q
    // does not match a SUPER+q bind. This pins the rule that adding a
    // co-modifier does not accidentally trigger a different bind.
    let mut t = BindTable::new();
    let _id = t
        .register(key(MOD_SUPER, b'q' as u32))
        .expect("registration ok");
    assert_eq!(
        t.match_bind(MOD_SUPER | MOD_SHIFT, b'q' as u32),
        None,
        "SUPER+SHIFT+q should not match the SUPER+q bind under exact-mask matching",
    );
}

#[test]
fn unrelated_modifier_combo_does_not_match() {
    // CTRL+q does not match a SUPER+q bind — keystrokes intended for
    // a hypothetical app shortcut do not trigger the compositor's
    // grab table.
    let mut t = BindTable::new();
    let _id = t
        .register(key(MOD_SUPER, b'q' as u32))
        .expect("registration ok");
    assert_eq!(
        t.match_bind(MOD_CTRL, b'q' as u32),
        None,
        "unrelated modifier combination should not match a SUPER+q bind",
    );
}

#[test]
#[ignore = "Phase 56 G.2 runtime synthetic-key-injection regression \
            ships as a QEMU smoke (userspace/grab-hook-smoke, \
            version 0.56.0); the host-process `cargo test` cannot \
            orchestrate the multi-process compositor + input-injection \
            flow. See file header."]
fn runtime_grab_hook_synthetic_injection_belongs_in_qemu_smoke() {
    panic!(
        "G.2 runtime grab-hook regression belongs in a QEMU smoke. \
         The supporting transport (ipc_take_pending_bulk + InjectKey \
         control verb) landed in this PR; the cross-process \
         verification is owned by `userspace/grab-hook-smoke`."
    );
}
