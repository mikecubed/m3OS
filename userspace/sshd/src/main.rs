//! SSH server daemon for m3OS (Phase 43).
//!
//! Provides encrypted remote shell access using the sunset IO-less SSH library.
//! Architecture mirrors telnetd: accept loop → fork per connection → PTY + shell relay.
#![cfg_attr(not(test), no_std)]
#![cfg_attr(not(test), no_main)]
#![cfg_attr(not(test), feature(alloc_error_handler))]

extern crate alloc;
#[cfg(test)]
extern crate std;

mod auth;
mod getrandom_impl;
mod host_key;
mod session;

#[cfg(not(test))]
use core::alloc::Layout;
#[cfg(not(test))]
use syscall_lib::heap::BrkAllocator;
use syscall_lib::{
    AF_INET, NEG_EEXIST, POLLIN, PollFd, SO_REUSEADDR, SOCK_STREAM, SOL_SOCKET, STDOUT_FILENO,
    WNOHANG, accept, close, fork, getpid, listen, mkdir, poll, socket, waitpid, write_str,
    write_u64,
};

#[cfg(not(test))]
#[global_allocator]
static ALLOCATOR: BrkAllocator = BrkAllocator::new();

#[cfg(not(test))]
#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    syscall_lib::write(2, b"sshd: out of memory\n");
    syscall_lib::exit(1)
}

#[cfg(not(test))]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::write(2, b"sshd: PANIC\n");
    syscall_lib::exit(1)
}

const SSH_PORT: u16 = 22;
const LISTENER_POLL_TIMEOUT_MS: i32 = 1000;

#[cfg(not(test))]
syscall_lib::entry_point!(program_main);

fn program_main(_args: &[&str]) -> i32 {
    write_str(STDOUT_FILENO, "sshd: starting\n");

    // B.1: Ensure /etc/ssh/ directory exists.
    ensure_ssh_dir();

    // C.1: Bind and listen on port 22 immediately — defer host key
    // generation until the first connection so sshd doesn't compete
    // with the login shell for CPU during boot.
    let listen_fd = match setup_listener(SSH_PORT) {
        Ok(fd) => fd,
        Err(_) => {
            write_str(STDOUT_FILENO, "sshd: failed to bind port 22\n");
            return 1;
        }
    };

    write_str(STDOUT_FILENO, "sshd: listening on port 22\n");

    // Accept loop (host key generated lazily on first connection).
    accept_loop(listen_fd);
}

/// B.1: Create /etc/ssh/ with mode 0755 if it does not exist.
fn ensure_ssh_dir() {
    let ret = mkdir(b"/etc\0", 0o755);
    if ret < 0 && ret != NEG_EEXIST {
        write_str(STDOUT_FILENO, "sshd: warning: cannot create /etc\n");
    }
    let ret = mkdir(b"/etc/ssh\0", 0o755);
    if ret < 0 && ret != NEG_EEXIST {
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
/// Host key is generated lazily on first connection to avoid competing
/// with the login shell during boot.
fn accept_loop(listen_fd: i32) -> ! {
    let mut host_key: Option<host_key::HostKey> = None;
    let mut listener_ready_count = 0u64;
    let mut accepted_count = 0u64;

    loop {
        reap_finished_children();

        if !wait_for_listener(listen_fd, LISTENER_POLL_TIMEOUT_MS) {
            continue;
        }
        listener_ready_count = listener_ready_count.saturating_add(1);
        if listener_ready_count == 1 || listener_ready_count.is_multiple_of(100) {
            write_str(STDOUT_FILENO, "sshd: listener ready count=");
            write_u64(STDOUT_FILENO, listener_ready_count);
            write_str(STDOUT_FILENO, "\n");
        }

        let client_fd = accept(listen_fd, None);
        if client_fd < 0 {
            write_str(STDOUT_FILENO, "sshd: accept failed\n");
            continue;
        }
        let client_fd = client_fd as i32;
        accepted_count = accepted_count.saturating_add(1);
        write_str(STDOUT_FILENO, "sshd: accepted client fd=");
        write_u64(STDOUT_FILENO, client_fd as u64);
        write_str(STDOUT_FILENO, " count=");
        write_u64(STDOUT_FILENO, accepted_count);
        write_str(STDOUT_FILENO, "\n");

        // Generate host key on first connection.
        if host_key.is_none() {
            write_str(STDOUT_FILENO, "sshd: loading host key\n");
            match host_key::load_or_generate() {
                Ok(k) => {
                    write_str(STDOUT_FILENO, "sshd: host key ready\n");
                    host_key = Some(k)
                }
                Err(_) => {
                    write_str(STDOUT_FILENO, "sshd: failed to load/generate host key\n");
                    close(client_fd);
                    continue;
                }
            }
        }

        let pid = fork();
        if pid < 0 {
            write_str(STDOUT_FILENO, "sshd: fork failed\n");
            close(client_fd);
            continue;
        }
        if pid == 0 {
            // Child: handle the SSH session.
            write_str(STDOUT_FILENO, "sshd: session child pid=");
            write_u64(STDOUT_FILENO, getpid() as u64);
            write_str(STDOUT_FILENO, " sock_fd=");
            write_u64(STDOUT_FILENO, client_fd as u64);
            write_str(STDOUT_FILENO, "\n");
            close(listen_fd);
            let exit_code = session::run_session(client_fd, host_key.as_ref().unwrap());
            close(client_fd);
            syscall_lib::exit(exit_code);
        }
        // Parent: close client fd and continue accepting.
        write_str(STDOUT_FILENO, "sshd: parent forked child pid=");
        write_u64(STDOUT_FILENO, pid as u64);
        write_str(STDOUT_FILENO, " client_fd=");
        write_u64(STDOUT_FILENO, client_fd as u64);
        write_str(STDOUT_FILENO, "\n");
        close(client_fd);
    }
}

fn reap_finished_children() {
    let mut status: i32 = 0;
    loop {
        let pid = waitpid(-1, &mut status, WNOHANG);
        if pid <= 0 {
            break;
        }
        write_str(STDOUT_FILENO, "sshd: reaped child pid=");
        write_u64(STDOUT_FILENO, pid as u64);
        write_str(STDOUT_FILENO, " status=");
        write_u64(STDOUT_FILENO, status as u64);
        if (status & 0x7f) == 0 {
            write_str(STDOUT_FILENO, " exit_code=");
            write_u64(STDOUT_FILENO, ((status >> 8) & 0xff) as u64);
        } else {
            write_str(STDOUT_FILENO, " signal=");
            write_u64(STDOUT_FILENO, (status & 0x7f) as u64);
        }
        write_str(STDOUT_FILENO, "\n");
    }
}

fn wait_for_listener(_listen_fd: i32, _timeout_ms: i32) -> bool {
    let mut pfd = PollFd {
        fd: _listen_fd,
        events: POLLIN,
        revents: 0,
    };
    let ready = poll(core::slice::from_mut(&mut pfd), _timeout_ms);
    ready > 0 && (pfd.revents & POLLIN) != 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, TcpStream};
    use std::thread;
    use std::time::Duration;

    fn make_listener() -> i32 {
        let fd = socket(AF_INET as i32, SOCK_STREAM as i32, 0);
        assert!(fd >= 0);
        let fd = fd as i32;

        let one: i32 = 1;
        let optval = unsafe {
            core::slice::from_raw_parts(
                &one as *const i32 as *const u8,
                core::mem::size_of::<i32>(),
            )
        };
        assert_eq!(
            syscall_lib::setsockopt(fd, SOL_SOCKET as i32, SO_REUSEADDR as i32, optval),
            0
        );

        let addr = syscall_lib::SockaddrIn::new([127, 0, 0, 1], 0);
        assert_eq!(syscall_lib::bind(fd, &addr), 0);
        assert_eq!(listen(fd, 1), 0);
        fd
    }

    #[test]
    fn wait_for_listener_times_out_without_connections() {
        let fd = make_listener();
        assert!(!wait_for_listener(fd, 20));
        close(fd);
    }

    #[test]
    fn wait_for_listener_reports_ready_socket() {
        let fd = make_listener();
        let mut addr = syscall_lib::SockaddrIn::new([0, 0, 0, 0], 0);
        assert_eq!(syscall_lib::getsockname(fd, &mut addr), 0);
        let port = addr.port();

        let connector = thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
        });

        assert!(wait_for_listener(fd, 500));
        let client_fd = accept(fd, None);
        assert!(client_fd >= 0);
        close(client_fd as i32);
        close(fd);
        connector.join().unwrap();
    }
}
