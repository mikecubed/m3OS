//! Userspace console service for m3OS (Phase 52).
//!
//! Handles `CONSOLE_WRITE` IPC requests from other processes and renders
//! text to the framebuffer. This is the ring-3 replacement for the
//! kernel-resident `console_server_task`.
#![no_std]
#![no_main]

syscall_lib::entry_point!(program_main);

fn program_main(_args: &[&str]) -> i32 {
    syscall_lib::write_str(syscall_lib::STDOUT_FILENO, "console_server: starting\n");
    // Full implementation in Track C.
    0
}
