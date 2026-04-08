//! Signal syscall handlers.

/// Handle signal-related syscalls.
///
/// Returns `Some(result)` if the syscall number belongs to this subsystem,
/// `None` otherwise.
#[inline(always)]
pub(super) fn handle_signal_syscall(number: u64, arg0: u64, arg1: u64, arg2: u64) -> Option<u64> {
    let result = match number {
        // rt_sigaction
        13 => super::sys_rt_sigaction(arg0, arg1, arg2),
        // rt_sigprocmask
        14 => super::sys_rt_sigprocmask(arg0, arg1, arg2),
        // kill
        62 => super::sys_kill(arg0, arg1),
        // sigaltstack
        131 => super::sys_sigaltstack(arg0, arg1),
        _ => return None,
    };
    Some(result)
}

/// Handle the divergent sigreturn syscall.
///
/// Returns `true` if the syscall number was handled (diverges, never returns).
/// Returns `false` if not this subsystem's syscall.
#[inline(always)]
pub(super) fn handle_divergent_signal_syscall(number: u64, user_rsp: u64) -> bool {
    match number {
        // sigreturn
        15 => super::sys_sigreturn(user_rsp),
        _ => false,
    }
}
