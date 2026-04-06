//! shutdown — halt the system (Phase 46).
//!
//! Signals init (PID 1) to stop all services, then invokes sys_reboot(HALT).
#![no_std]
#![no_main]

use syscall_lib::{STDERR_FILENO, STDOUT_FILENO, write_str};

syscall_lib::entry_point!(main);

fn main(args: &[&str]) -> i32 {
    // Only root can shut down.
    if syscall_lib::getuid() != 0 {
        write_str(STDERR_FILENO, "shutdown: must be root\n");
        return 1;
    }

    // Check for -h (halt, default), -r (reboot — use the reboot command instead).
    let halt = !(args.len() > 1 && args[1] == "-r");

    write_str(STDOUT_FILENO, "System is going down for halt...\n");

    // Signal init to begin orderly shutdown.
    syscall_lib::kill(1, syscall_lib::SIGTERM);

    // Give init time to stop services.
    syscall_lib::nanosleep(3);

    // Now invoke the reboot syscall.
    let cmd = if halt {
        syscall_lib::REBOOT_CMD_HALT
    } else {
        syscall_lib::REBOOT_CMD_RESTART
    };
    let ret = syscall_lib::reboot(cmd);
    if ret < 0 {
        write_str(STDERR_FILENO, "shutdown: reboot syscall failed\n");
        return 1;
    }

    // Should not reach here.
    0
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::exit(101)
}
