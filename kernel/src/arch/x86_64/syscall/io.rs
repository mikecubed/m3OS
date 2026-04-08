//! I/O multiplexing syscall handlers (poll, select, epoll, pipe).

/// Handle I/O multiplexing syscalls.
///
/// Returns `Some(result)` if the syscall number belongs to this subsystem,
/// `None` otherwise.
#[inline(always)]
pub(super) fn handle_io_syscall(number: u64, arg0: u64, arg1: u64, arg2: u64) -> Option<u64> {
    let result = match number {
        // poll
        7 => super::sys_poll(arg0, arg1, arg2),
        // pipe
        22 => super::sys_pipe_with_flags(arg0, false),
        // select
        23 => {
            let exceptfds = super::per_core_syscall_arg3();
            let timeout_ptr = crate::smp::per_core().syscall_user_r8;
            super::sys_select(arg0, arg1, arg2, exceptfds, timeout_ptr)
        }
        // epoll_wait
        232 => {
            let timeout = super::per_core_syscall_arg3();
            super::sys_epoll_wait(arg0, arg1, arg2, timeout)
        }
        // epoll_ctl
        233 => {
            let event_ptr = super::per_core_syscall_arg3();
            super::sys_epoll_ctl(arg0, arg1, arg2, event_ptr)
        }
        // pselect6
        270 => {
            let exceptfds = super::per_core_syscall_arg3();
            let timeout_ptr = crate::smp::per_core().syscall_user_r8;
            super::sys_pselect6(arg0, arg1, arg2, exceptfds, timeout_ptr)
        }
        // epoll_create1
        291 => super::sys_epoll_create1(arg0),
        // pipe2
        293 => {
            let cloexec = arg1 & 0x80000 != 0;
            super::sys_pipe_with_flags(arg0, cloexec)
        }
        _ => return None,
    };
    Some(result)
}
