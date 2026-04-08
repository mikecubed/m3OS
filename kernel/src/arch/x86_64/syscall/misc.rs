//! Miscellaneous syscall handlers (ioctl, uname, arch_prctl, reboot, etc.).

/// Handle miscellaneous syscalls.
///
/// Returns `Some(result)` if the syscall number belongs to this subsystem,
/// `None` otherwise.
#[inline(always)]
pub(super) fn handle_misc_syscall(number: u64, arg0: u64, arg1: u64, arg2: u64) -> Option<u64> {
    let result = match number {
        // ioctl
        16 => super::sys_linux_ioctl(arg0, arg1, arg2),
        // nice
        34 => {
            let pid = crate::process::current_pid();
            let uid_val = {
                let table = crate::process::PROCESS_TABLE.lock();
                table.find(pid).map(|p| p.uid).unwrap_or(0)
            };
            crate::task::sys_nice(arg0 as i32, uid_val) as u64
        }
        // uname
        63 => super::sys_linux_uname(arg0),
        // arch_prctl
        158 => super::sys_linux_arch_prctl(arg0, arg1),
        // reboot
        169 => super::sys_reboot(arg0),
        // futex
        202 => {
            let val3 = crate::smp::per_core().syscall_user_r9;
            super::sys_futex(arg0, arg1, arg2, val3)
        }
        // set_robust_list — stub no-op
        273 => 0,
        // prlimit64 — return ENOSYS
        302 => super::NEG_ENOSYS,
        // getrandom
        318 => super::sys_getrandom(arg0, arg1, arg2),
        // debug_print
        0x1000 => super::sys_debug_print(arg0, arg1),
        // meminfo
        0x1001 => super::sys_meminfo(arg0, arg1),
        // ktrace
        #[cfg(feature = "trace")]
        0x1002 => super::sys_ktrace(arg0, arg1, arg2),
        // framebuffer_info
        0x1005 => super::sys_framebuffer_info(arg0, arg1),
        // read_scancode
        0x1007 => super::sys_read_scancode(),
        _ => return None,
    };
    Some(result)
}
