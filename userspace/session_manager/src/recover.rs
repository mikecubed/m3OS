//! Phase 57 Track F.4 — Recovery + text-mode fallback for `session_manager`.
//!
//! ## Roles in this module
//!
//! Per the engineering-discipline rule that pure logic lives in
//! `kernel-core` and IPC / hardware wiring lives in `userspace/`, this
//! module re-exports the host-tested policy types from
//! [`kernel_core::session`] and exposes the side-effecting executor
//! that talks to the supervisor + the framebuffer release syscall.
//!
//! 1. The **policy** types [`Recovery`] + [`RecoveryAction`] re-export
//!    [`kernel_core::session::Recovery`] / [`RecoveryAction`]. The
//!    pure-logic implementation lives in
//!    `kernel-core/src/session/recover.rs` and is host-tested via
//!    `kernel-core/tests/phase57_f4_recovery.rs`.
//!
//! 2. The **executor** [`execute_text_fallback`] issues the rollback
//!    verbs that bring the graphical services down in reverse start
//!    order, releases the framebuffer back to the kernel console (so
//!    the serial admin shell becomes visible), and emits the structured
//!    `session.recover.text_fallback` log event.
//!
//! Splitting these concerns means the policy is host-testable as pure
//! logic and the executor is the only side-effecting code path — the
//! Phase 56 `display_server::control::dispatch_command` precedent uses
//! the same shape: pure dispatcher + side-effect-bearing main loop.

pub use kernel_core::session::{Recovery, RecoveryAction};
