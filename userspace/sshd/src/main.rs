//! SSH server daemon for m3OS (Phase 43).
//!
//! Provides encrypted remote shell access using the sunset IO-less SSH library.
//! Architecture mirrors telnetd: accept loop → fork per connection → PTY + shell relay.
#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

mod auth;
mod getrandom_impl;
mod host_key;
mod session;

use core::alloc::Layout;
use syscall_lib::heap::BrkAllocator;
use syscall_lib::{
    AF_INET, SO_REUSEADDR, SOCK_STREAM, SOL_SOCKET, STDOUT_FILENO, WNOHANG, accept, close, fork,
    listen, mkdir, socket, waitpid, write_str,
};

#[global_allocator]
static ALLOCATOR: BrkAllocator = BrkAllocator::new();

#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    syscall_lib::write(2, b"sshd: out of memory\n");
    syscall_lib::exit(1)
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::write(2, b"sshd: PANIC\n");
    syscall_lib::exit(1)
}

const SSH_PORT: u16 = 22;

syscall_lib::entry_point!(main);

fn main(_args: &[&str]) -> i32 {
    write_str(STDOUT_FILENO, "sshd: starting\n");

    // B.1: Ensure /etc/ssh/ directory exists.
    ensure_ssh_dir();

    // B.2/B.3: Load or generate the Ed25519 host key.
    let host_key = match host_key::load_or_generate() {
        Ok(k) => k,
        Err(_) => {
            write_str(STDOUT_FILENO, "sshd: failed to load/generate host key\n");
            return 1;
        }
    };

    // C.1: Bind and listen on port 22.
    let listen_fd = match setup_listener(SSH_PORT) {
        Ok(fd) => fd,
        Err(_) => {
            write_str(STDOUT_FILENO, "sshd: failed to bind port 22\n");
            return 1;
        }
    };

    write_str(STDOUT_FILENO, "sshd: listening on port 22\n");

    // Accept loop.
    accept_loop(listen_fd, &host_key);
}

/// B.1: Create /etc/ssh/ with mode 0755 if it does not exist.
fn ensure_ssh_dir() {
    let ret = mkdir(b"/etc\0", 0o755);
    // Ignore EEXIST (-17).
    if ret < 0 && ret != -17 {
        write_str(STDOUT_FILENO, "sshd: warning: cannot create /etc\n");
    }
    let ret = mkdir(b"/etc/ssh\0", 0o755);
    if ret < 0 && ret != -17 {
        write_str(STDOUT_FILENO, "sshd: warning: cannot create /etc/ssh\n");
    }
}

/// C.1: Create, bind, and listen on a TCP socket.
fn setup_listener(port: u16) -> Result<i32, ()> {
    let fd = socket(AF_INET as i32, SOCK_STREAM as i32, 0);
    if fd < 0 {
        return Err(());
    }
    let fd = fd as i32;

    // Set SO_REUSEADDR.
    let one: i32 = 1;
    let optval = unsafe {
        core::slice::from_raw_parts(&one as *const i32 as *const u8, core::mem::size_of::<i32>())
    };
    syscall_lib::setsockopt(fd, SOL_SOCKET as i32, SO_REUSEADDR as i32, optval);

    let addr = syscall_lib::SockaddrIn::new([0, 0, 0, 0], port);
    let ret = syscall_lib::bind(fd, &addr);
    if ret < 0 {
        close(fd);
        return Err(());
    }

    let ret = listen(fd, 5);
    if ret < 0 {
        close(fd);
        return Err(());
    }

    Ok(fd)
}

/// F.1: Accept connections in a loop, fork a child for each.
fn accept_loop(listen_fd: i32, host_key: &host_key::HostKey) -> ! {
    loop {
        // Reap finished children.
        let mut status: i32 = 0;
        while waitpid(-1, &mut status, WNOHANG) > 0 {}

        let client_fd = accept(listen_fd, None);
        if client_fd < 0 {
            syscall_lib::nanosleep(1);
            continue;
        }
        let client_fd = client_fd as i32;

        let pid = fork();
        if pid < 0 {
            write_str(STDOUT_FILENO, "sshd: fork failed\n");
            close(client_fd);
            continue;
        }
        if pid == 0 {
            // Child: handle the SSH session.
            close(listen_fd);
            let exit_code = session::run_session(client_fd, host_key);
            close(client_fd);
            syscall_lib::exit(exit_code);
        }
        // Parent: close client fd and continue accepting.
        close(client_fd);
    }
}
