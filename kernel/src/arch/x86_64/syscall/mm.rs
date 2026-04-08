//! Memory management syscall handlers.

/// Handle memory-management syscalls.
///
/// Returns `Some(result)` if the syscall number belongs to this subsystem,
/// `None` otherwise.
#[inline(always)]
pub(super) fn handle_mm_syscall(number: u64, arg0: u64, arg1: u64, arg2: u64) -> Option<u64> {
    let result = match number {
        // mmap
        9 => super::sys_linux_mmap(arg0, arg1, arg2),
        // mprotect
        10 => super::sys_mprotect(arg0, arg1, arg2),
        // munmap
        11 => super::sys_linux_munmap(arg0, arg1),
        // brk
        12 => super::sys_linux_brk(arg0),
        // framebuffer_mmap
        0x1006 => super::sys_framebuffer_mmap(),
        _ => return None,
    };
    Some(result)
}
