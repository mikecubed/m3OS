//! Phase 57 Track F.4 — Recovery + text-mode fallback for `session_manager`.
//!
//! ## Roles in this module
//!
//! Per the engineering-discipline rule that pure logic lives in
//! `kernel-core` and IPC / hardware wiring lives in `userspace/`, this
//! module re-exports the host-tested policy + executor types from
//! [`kernel_core::session`] and supplies the syscall-backed
//! [`FramebufferRestorer`] implementation the executor consumes.
//!
//! 1. **Policy** types [`Recovery`] + [`RecoveryAction`] re-export
//!    [`kernel_core::session::Recovery`] / [`RecoveryAction`]. The
//!    pure-logic implementation lives in
//!    `kernel-core/src/session/recover.rs` and is host-tested via
//!    `kernel-core/tests/phase57_f4_recovery.rs`.
//!
//! 2. **Executor** [`run_text_fallback`] wraps
//!    [`kernel_core::session::recover::execute_text_fallback_rollback`]
//!    with a [`SyscallFramebufferRestorer`] impl that calls
//!    [`syscall_lib::framebuffer_release`]. The kernel-core function is
//!    host-tested via
//!    `kernel-core/tests/phase57_f4_text_fallback_executor.rs`; the
//!    syscall wrapper here adds only the typed errno mapping and the
//!    structured `session.recover.text_fallback` log line.
//!
//! Splitting these concerns means the policy + the rollback motion are
//! host-testable as pure logic, and the only side-effecting wiring
//! that lives in this binary is the framebuffer-release syscall.
//!
//! ## Why the executor calls `framebuffer_release`
//!
//! Per the F.4 acceptance:
//!
//! > On `text-fallback`: `session_manager` stops the graphical services
//! > in reverse start order, releases the framebuffer back to the
//! > kernel console (the existing Phase 47 `restore_console` path),
//! > and surfaces an admin shell on the serial console.
//!
//! `session_manager` does not own the raw framebuffer mapping — the
//! Phase 56 `display_server` daemon is the FB owner. On a
//! `display_server` death the kernel's process-exit path
//! (`kernel/src/arch/x86_64/syscall/mod.rs`) already calls
//! `crate::fb::restore_console()`. F.4 must guarantee that the same
//! restore happens when `session_manager` decides to abandon the
//! graphical session voluntarily — even if `display_server` is still
//! alive. Calling `framebuffer_release` from `session_manager`
//! returns `-EPERM` (the caller does not own the FB) under steady
//! state; this is the expected outcome and is logged as an
//! observability event rather than treated as an error. The
//! `SupervisorBackend::stop("display_server")` issued earlier in the
//! rollback is what actually triggers the kernel-side `restore_console`
//! via the death path.

extern crate alloc;

pub use kernel_core::session::recover::{
    FramebufferRestoreError, FramebufferRestorer, Recovery, RecoveryAction, TextFallbackOutcome,
    execute_text_fallback_rollback,
};

use kernel_core::session_supervisor::SupervisorBackend;
use syscall_lib::STDOUT_FILENO;

/// Errno returned by `framebuffer_release` when the caller does not
/// own the framebuffer. Phase 47 / Phase 56 contract: `session_manager`
/// is not the FB owner (the owner is `display_server`), so this is the
/// expected return on the F.4 belt-and-braces call.
const EPERM_NEG: isize = -1;

/// Errno returned by `framebuffer_release` when no FB mapping exists.
const ENOENT_NEG: isize = -2;

/// Production [`FramebufferRestorer`] that issues the
/// [`syscall_lib::framebuffer_release`] syscall. The expected steady-
/// state return for `session_manager` is `-EPERM` (the FB belongs to
/// `display_server`); the impl maps that and `-ENOENT` (no mapping at
/// all, e.g. after `display_server` already died) to `Ok(())` so the
/// caller sees the rollback as successful in the typical case. Genuine
/// transport failures surface as `Err(())`.
pub struct SyscallFramebufferRestorer;

impl FramebufferRestorer for SyscallFramebufferRestorer {
    fn restore_console(&mut self) -> Result<(), FramebufferRestoreError> {
        let rc = syscall_lib::framebuffer_release();
        match rc {
            0 => {
                syscall_lib::write_str(
                    STDOUT_FILENO,
                    "session_manager: session.recover.text_fallback: framebuffer released\n",
                );
                Ok(())
            }
            x if x == EPERM_NEG => {
                syscall_lib::write_str(
                    STDOUT_FILENO,
                    "session_manager: session.recover.text_fallback: fb_release=EPERM (display_server owns FB; stop will trigger kernel restore_console)\n",
                );
                Ok(())
            }
            x if x == ENOENT_NEG => {
                syscall_lib::write_str(
                    STDOUT_FILENO,
                    "session_manager: session.recover.text_fallback: fb_release=ENOENT (no FB mapping; kernel console already restored)\n",
                );
                Ok(())
            }
            _ => {
                syscall_lib::write_str(
                    STDOUT_FILENO,
                    "session_manager: session.recover.text_fallback: fb_release returned unexpected errno\n",
                );
                Err(FramebufferRestoreError::TransportFailure)
            }
        }
    }
}

/// Run the text-fallback rollback against the supplied supervisor
/// backend, using the production [`SyscallFramebufferRestorer`].
///
/// Emits the structured `session.recover.text_fallback` log lines
/// before and after the rollback so the boot transcript records the
/// motion. Returns the typed [`TextFallbackOutcome`] for the caller's
/// own logging if needed.
pub fn run_text_fallback<B: SupervisorBackend>(backend: &mut B) -> TextFallbackOutcome {
    syscall_lib::write_str(
        STDOUT_FILENO,
        "session_manager: session.recover.text_fallback: rolling back graphical session\n",
    );
    let mut restorer = SyscallFramebufferRestorer;
    let outcome = execute_text_fallback_rollback(backend, &mut restorer);
    syscall_lib::write_str(
        STDOUT_FILENO,
        "session_manager: session.recover.text_fallback: serial admin shell now available\n",
    );
    outcome
}
