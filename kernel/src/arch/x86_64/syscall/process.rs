//! Process and identity syscall handlers.

/// Handle process and identity syscalls.
///
/// Returns `Some(result)` if the syscall number belongs to this subsystem,
/// `None` otherwise.
pub(super) fn handle_process_syscall(
    number: u64,
    arg0: u64,
    arg1: u64,
    arg2: u64,
    user_rip: u64,
    user_rsp: u64,
) -> Option<u64> {
    let result = match number {
        // getpid
        39 => super::sys_getpid(),
        // clone
        56 => {
            let child_tidptr = super::per_core_syscall_arg3(); // r10
            let tls = crate::smp::per_core().syscall_user_r8;
            super::sys_clone(arg0, arg1, arg2, child_tidptr, tls, user_rip, user_rsp)
        }
        // fork
        57 => super::sys_fork(user_rip, user_rsp),
        // execve
        59 => super::sys_execve(arg0, arg1, arg2),
        // waitpid
        61 => super::sys_waitpid(arg0, arg1, arg2),
        // umask
        95 => super::sys_umask(arg0),
        // getuid
        102 => super::sys_linux_getuid(),
        // getgid
        104 => super::sys_linux_getgid(),
        // setuid
        105 => super::sys_linux_setuid(arg0),
        // setgid
        106 => super::sys_linux_setgid(arg0),
        // geteuid
        107 => super::sys_linux_geteuid(),
        // getegid
        108 => super::sys_linux_getegid(),
        // setpgid
        109 => super::sys_setpgid(arg0, arg1),
        // getppid
        110 => super::sys_getppid(),
        // getpgrp — equivalent to getpgid(0)
        111 => super::sys_getpgid(0),
        // setsid
        112 => super::sys_setsid(),
        // setreuid
        113 => super::sys_linux_setreuid(arg0, arg1),
        // setregid
        114 => super::sys_linux_setregid(arg0, arg1),
        // getpgid
        121 => super::sys_getpgid(arg0),
        // getsid
        124 => super::sys_getsid(arg0),
        // gettid
        186 => super::sys_gettid(),
        // tkill
        200 => super::sys_tkill(arg0, arg1),
        // sched_setaffinity
        203 => {
            if arg2 == 0 {
                return Some(super::NEG_EFAULT);
            }
            if arg1 < 8 {
                return Some(super::NEG_EINVAL);
            }
            let mask = {
                let mut buf = [0u8; 8];
                if crate::mm::user_mem::copy_from_user(&mut buf, arg2).is_err() {
                    return Some(super::NEG_EFAULT);
                }
                u64::from_ne_bytes(buf)
            };
            crate::task::sys_sched_setaffinity(arg0 as u32, mask) as u64
        }
        // sched_getaffinity
        204 => {
            let mask = crate::task::sys_sched_getaffinity(arg0 as u32);
            if mask < 0 {
                mask as u64
            } else if arg2 != 0 && arg1 >= 8 {
                let bytes = (mask as u64).to_ne_bytes();
                if crate::mm::user_mem::copy_to_user(arg2, &bytes).is_err() {
                    return Some(super::NEG_EFAULT);
                }
                8 // return bytes written
            } else {
                super::NEG_EINVAL
            }
        }
        // set_tid_address
        218 => super::sys_linux_set_tid_address(arg0),
        _ => return None,
    };
    Some(result)
}

/// Handle divergent process syscalls (exit, exit_group).
///
/// Returns `true` if the syscall number was handled (diverges, never returns).
/// Returns `false` if the syscall number does not belong here.
pub(super) fn handle_divergent_syscall(number: u64, arg0: u64) -> bool {
    match number {
        // exit
        60 => super::sys_exit(arg0 as i32),
        // exit_group
        231 => super::sys_exit_group(arg0 as i32),
        _ => false,
    }
}
