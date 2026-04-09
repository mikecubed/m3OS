//! Userspace keyboard service for m3OS (Phase 52).
//!
//! Handles `KBD_READ` IPC requests, draining scancodes from a
//! kernel-provided buffer after IRQ1 notification signals. This is the
//! ring-3 replacement for the kernel-resident `kbd_server_task`.
#![no_std]
#![no_main]

syscall_lib::entry_point!(program_main);

fn program_main(_args: &[&str]) -> i32 {
    syscall_lib::write_str(syscall_lib::STDOUT_FILENO, "kbd_server: starting\n");
    // Full implementation in Track D.
    0
}
