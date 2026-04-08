//! IPC syscall handlers.

/// Handle IPC syscalls (numbers 1–10).
///
/// Returns `Some(result)` if the syscall number belongs to IPC,
/// `None` otherwise.
#[inline(always)]
pub(super) fn handle_ipc_syscall(number: u64, arg0: u64, arg1: u64, arg2: u64) -> Option<u64> {
    match number {
        1..=10 => Some(crate::ipc::dispatch(number, arg0, arg1, arg2, 0, 0)),
        _ => None,
    }
}
