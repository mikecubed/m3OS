//! Fork test: parent forks a child that exits(42), then waits and verifies.
//!
//! Validation: P11-T021 — fork child exits 42; waitpid returns 42.
#![no_std]
#![no_main]

use syscall_lib::{exit, serial_print, syscall0, syscall2, SYS_FORK, SYS_WAITPID};

#[no_mangle]
pub extern "C" fn _start() -> ! {
    let child_pid = unsafe { syscall0(SYS_FORK) };

    if child_pid == 0 {
        // Child path: exit with 42.
        exit(42)
    } else if child_pid == u64::MAX {
        serial_print("fork-test: fork() failed\n");
        exit(1)
    } else {
        // Parent path: wait for child.
        let mut wstatus: i32 = -1;
        let waited = unsafe { syscall2(SYS_WAITPID, child_pid, &mut wstatus as *mut i32 as u64) };
        if waited == u64::MAX {
            serial_print("fork-test: waitpid failed\n");
            exit(2)
        }
        // Linux wstatus encoding: exit code is in bits 15:8.
        let exit_code = (wstatus >> 8) & 0xff;
        if exit_code == 42 {
            serial_print("fork-test: PASS — child exited 42\n");
            exit(0)
        } else {
            serial_print("fork-test: FAIL — unexpected exit code\n");
            exit(3)
        }
    }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    exit(100)
}
