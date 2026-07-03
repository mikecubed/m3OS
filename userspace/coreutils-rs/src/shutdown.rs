//! shutdown — power the system off (Phase 46; real ACPI S5 since Phase
//! 103 D.3).
//!
//! Signals init (PID 1) to stop all services, then invokes
//! `sys_reboot(POWER_OFF)` — kernel filesystem sync followed by the ACPI
//! S5 write acpid registered at boot (falls back to halt on platforms
//! with no `\_S5`). `-r` reboots instead; the `reboot` command is the
//! same path.
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

    // Check for -h (poweroff, default), -r (reboot — same as the reboot command).
    let poweroff = !(args.len() > 1 && args[1] == "-r");

    let message = if poweroff {
        "System is going down for poweroff...\n"
    } else {
        "System is going down for reboot...\n"
    };
    write_str(STDOUT_FILENO, message);

    // Signal init to begin orderly shutdown.
    syscall_lib::kill(1, syscall_lib::SIGTERM);

    // Give init time to stop services.
    syscall_lib::nanosleep(3);

    // Now invoke the reboot syscall.
    let cmd = if poweroff {
        syscall_lib::REBOOT_CMD_POWER_OFF
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
