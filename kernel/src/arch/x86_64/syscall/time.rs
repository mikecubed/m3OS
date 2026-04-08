//! Timekeeping syscall handlers.

/// Handle timekeeping syscalls.
///
/// Returns `Some(result)` if the syscall number belongs to this subsystem,
/// `None` otherwise.
pub(super) fn handle_time_syscall(number: u64, arg0: u64, arg1: u64) -> Option<u64> {
    let result = match number {
        // nanosleep
        35 => super::sys_nanosleep(arg0),
        // gettimeofday
        96 => super::sys_gettimeofday(arg0),
        // times
        100 => super::sys_times(arg0),
        // clock_gettime
        228 => super::sys_clock_gettime(arg0, arg1),
        _ => return None,
    };
    Some(result)
}
