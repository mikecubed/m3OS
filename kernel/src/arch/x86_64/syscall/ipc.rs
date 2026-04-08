//! IPC syscall handlers.
//!
//! Userspace IPC syscalls use numbers `0x1100..=0x1109`, which are translated
//! to internal IPC dispatch numbers `1..=10`.  This avoids colliding with
//! Linux-compatible syscall numbers (1=write, 2=open, etc.) that are handled
//! by earlier dispatchers in the syscall chain.

/// First userspace syscall number reserved for IPC.
const IPC_SYSCALL_BASE: u64 = 0x1100;

/// Last userspace syscall number reserved for IPC (inclusive).
const IPC_SYSCALL_LAST: u64 = 0x1109;

/// Handle IPC syscalls (userspace numbers `0x1100..=0x1109`).
///
/// These external syscall numbers are translated back to the existing
/// internal IPC dispatch IDs (`1..=10`) before calling `crate::ipc::dispatch`,
/// preserving current kernel-side behavior while avoiding syscall-number
/// conflicts in the userspace ABI.
///
/// Returns `Some(result)` if the syscall number belongs to IPC,
/// `None` otherwise.
#[inline(always)]
pub(super) fn handle_ipc_syscall(number: u64, arg0: u64, arg1: u64, arg2: u64) -> Option<u64> {
    match number {
        IPC_SYSCALL_BASE..=IPC_SYSCALL_LAST => {
            let dispatch_number = (number - IPC_SYSCALL_BASE) + 1;
            Some(crate::ipc::dispatch(
                dispatch_number,
                arg0,
                arg1,
                arg2,
                0,
                0,
            ))
        }
        _ => None,
    }
}
