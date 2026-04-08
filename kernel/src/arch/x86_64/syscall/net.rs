//! Network and socket syscall handlers.

/// Handle network/socket syscalls.
///
/// Returns `Some(result)` if the syscall number belongs to this subsystem,
/// `None` otherwise.
pub(super) fn handle_net_syscall(number: u64, arg0: u64, arg1: u64, arg2: u64) -> Option<u64> {
    let result = match number {
        // socket
        41 => super::sys_socket(arg0, arg1, arg2),
        // connect
        42 => super::sys_connect(arg0, arg1, arg2),
        // accept
        43 => super::sys_accept(arg0, arg1, arg2),
        // sendto
        44 => {
            let flags = super::per_core_syscall_arg3();
            let addr_ptr = crate::smp::per_core().syscall_user_r8;
            let addr_len = crate::smp::per_core().syscall_user_r9;
            super::sys_sendto(arg0, arg1, arg2, flags, addr_ptr, addr_len)
        }
        // recvfrom
        45 => {
            let flags = super::per_core_syscall_arg3();
            let addr_ptr = crate::smp::per_core().syscall_user_r8;
            let addr_len_ptr = crate::smp::per_core().syscall_user_r9;
            super::sys_recvfrom_socket(arg0, arg1, arg2, flags, addr_ptr, addr_len_ptr)
        }
        // shutdown
        48 => super::sys_shutdown_sock(arg0, arg1),
        // bind
        49 => super::sys_bind(arg0, arg1, arg2),
        // listen
        50 => super::sys_listen(arg0, arg1),
        // getsockname
        51 => super::sys_getsockname(arg0, arg1, arg2),
        // getpeername
        52 => super::sys_getpeername(arg0, arg1, arg2),
        // socketpair
        53 => {
            let sv_ptr = super::per_core_syscall_arg3();
            super::sys_socketpair(arg0, arg1, arg2, sv_ptr)
        }
        // setsockopt
        54 => {
            let optval_ptr = super::per_core_syscall_arg3();
            let optlen = crate::smp::per_core().syscall_user_r8;
            super::sys_setsockopt(arg0, arg1, arg2, optval_ptr, optlen)
        }
        // getsockopt
        55 => {
            let optval_ptr = super::per_core_syscall_arg3();
            let optlen_ptr = crate::smp::per_core().syscall_user_r8;
            super::sys_getsockopt(arg0, arg1, arg2, optval_ptr, optlen_ptr)
        }
        // accept4
        288 => {
            let flags = super::per_core_syscall_arg3();
            super::sys_accept4(arg0, arg1, arg2, flags)
        }
        _ => return None,
    };
    Some(result)
}
