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
//! Steps 3 and 5 require the userspace bulk-drain (cross-reference
//! `phase56_g1_multi_client_coexistence.rs` for the full deferral
//! rationale): the test client cannot observe the
//! `BindTriggered` event without a working drain, and the synthetic-
//! key-injection control-socket verb stages bytes the dispatcher
//! cannot read back. Phase 56 G-track therefore ships the *partial*
//! coverage: the pure-logic match path that production code uses to
//! decide whether a `KeyEvent` is swallowed or forwarded.
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
//! ## Bulk-drain deferral list (mirrors `phase56_g1_*`)
//!
//! Until the userspace bulk-drain syscall lands:
//!
//! * No runtime synthetic-key-injection regression.
//! * No `BindTriggered` event-stream regression.
//! * No "focused client receives no `KeyEvent`" cross-process check.
//!
//! These three together are what runtime G.2 deferral defers. The lift
//! plan is the same as G.1's: replace the deferred-runtime portion of
//! this file when bulk-drain ships.

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
            deferred behind the userspace bulk-drain gap \
            (TODO(C.5-bulk-drain)); see file header for the lift plan."]
fn runtime_grab_hook_synthetic_injection_deferred() {
    panic!(
        "G.2 runtime grab-hook regression is deferred behind the \
         userspace bulk-drain gap. See this file's module-level \
         docstring for the lift plan."
    );
}
