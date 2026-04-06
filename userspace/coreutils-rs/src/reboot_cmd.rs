//! reboot — restart the system (Phase 46).
//!
//! Signals init (PID 1) to stop all services, then invokes sys_reboot(RESTART).
#![no_std]
#![no_main]

use syscall_lib::{STDERR_FILENO, STDOUT_FILENO, write_str};

syscall_lib::entry_point!(main);

fn main(_args: &[&str]) -> i32 {
    if syscall_lib::getuid() != 0 {
        write_str(STDERR_FILENO, "reboot: must be root\n");
        return 1;
    }

    write_str(STDOUT_FILENO, "System is going down for reboot...\n");

    // Signal init to begin orderly shutdown.
    syscall_lib::kill(1, syscall_lib::SIGTERM);

    // Give init time to stop services.
    syscall_lib::nanosleep(3);

    let ret = syscall_lib::reboot(syscall_lib::REBOOT_CMD_RESTART);
    if ret < 0 {
        write_str(STDERR_FILENO, "reboot: syscall failed\n");
        return 1;
    }

    0
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::exit(101)
}
