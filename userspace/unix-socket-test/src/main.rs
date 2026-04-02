//! Unix domain socket test program (Phase 39).
//!
//! Tests socketpair, named stream sockets, and datagram sockets.

#![no_std]
#![no_main]

use syscall_lib::{
    AF_UNIX, SHUT_WR, SOCK_DGRAM, SOCK_STREAM, SockaddrUn, accept, bind_unix, close, connect_unix,
    exit, fork, listen, read, recvfrom_unix, sendto_unix, shutdown, socket, socketpair, unlink,
    write, write_str,
};

fn print(s: &[u8]) {
    let _ = write(1, s);
}

fn ok(name: &[u8]) {
    print(b"  PASS: ");
    print(name);
    print(b"\n");
}

fn fail(name: &[u8]) {
    print(b"  FAIL: ");
    print(name);
    print(b"\n");
}

/// Test 1: socketpair — create connected pair, write/read between them.
fn test_socketpair() -> bool {
    print(b"[socketpair test]\n");
    let mut sv = [0i32; 2];
    let ret = socketpair(AF_UNIX as i32, SOCK_STREAM as i32, 0, &mut sv);
    if ret < 0 {
        fail(b"socketpair() failed");
        return false;
    }
    ok(b"socketpair() created pair");

    let msg = b"hello unix!";
    let n = write(sv[0], msg);
    if n != msg.len() as isize {
        fail(b"write to sv[0] failed");
        close(sv[0]);
        close(sv[1]);
        return false;
    }
    ok(b"write to sv[0]");

    let mut buf = [0u8; 64];
    let n = read(sv[1], &mut buf);
    if n != msg.len() as isize {
        fail(b"read from sv[1] wrong size");
        close(sv[0]);
        close(sv[1]);
        return false;
    }
    if &buf[..n as usize] != msg {
        fail(b"data mismatch");
        close(sv[0]);
        close(sv[1]);
        return false;
    }
    ok(b"read from sv[1] matches");

    // Test bidirectional: write from sv[1], read from sv[0].
    let msg2 = b"reply";
    let _ = write(sv[1], msg2);
    let n2 = read(sv[0], &mut buf);
    if n2 == msg2.len() as isize && &buf[..n2 as usize] == msg2 {
        ok(b"bidirectional I/O");
    } else {
        fail(b"bidirectional I/O");
        close(sv[0]);
        close(sv[1]);
        return false;
    }

    close(sv[0]);
    close(sv[1]);
    ok(b"close pair");
    true
}

/// Test 2: socketpair with fork — parent writes, child reads.
fn test_socketpair_fork() -> bool {
    print(b"[socketpair fork test]\n");
    let mut sv = [0i32; 2];
    let ret = socketpair(AF_UNIX as i32, SOCK_STREAM as i32, 0, &mut sv);
    if ret < 0 {
        fail(b"socketpair() failed");
        return false;
    }

    let pid = fork();
    if pid < 0 {
        fail(b"fork() failed");
        close(sv[0]);
        close(sv[1]);
        return false;
    }

    if pid == 0 {
        // Child: read from sv[1], verify, exit.
        close(sv[0]);
        let mut buf = [0u8; 64];
        let n = read(sv[1], &mut buf);
        close(sv[1]);
        if n == 5 && &buf[..5] == b"hello" {
            exit(0);
        } else {
            exit(1);
        }
    }

    // Parent: write to sv[0], wait for child.
    close(sv[1]);
    let _ = write(sv[0], b"hello");
    close(sv[0]);

    let mut status: i32 = -1;
    let _ = syscall_lib::waitpid(pid as i32, &mut status, 0);
    let exit_code = (status >> 8) & 0xff;
    if exit_code == 0 {
        ok(b"child read correct data");
        true
    } else {
        fail(b"child got wrong data");
        false
    }
}

/// Test 3: Named stream socket — server binds/listens, client connects.
fn test_named_stream() -> bool {
    print(b"[named stream socket test]\n");

    let server_fd = socket(AF_UNIX as i32, SOCK_STREAM as i32, 0);
    if server_fd < 0 {
        fail(b"socket() failed");
        return false;
    }

    // Unlink stale socket from prior run.
    let _ = unlink(b"/tmp/test.sock\0");
    let addr = SockaddrUn::new("/tmp/test.sock");
    let ret = bind_unix(server_fd as i32, &addr);
    if ret < 0 {
        fail(b"bind() failed");
        close(server_fd as i32);
        return false;
    }
    ok(b"bind to /tmp/test.sock");

    let ret = listen(server_fd as i32, 5);
    if ret < 0 {
        fail(b"listen() failed");
        close(server_fd as i32);
        return false;
    }
    ok(b"listen()");

    let pid = fork();
    if pid < 0 {
        fail(b"fork() failed");
        close(server_fd as i32);
        return false;
    }

    if pid == 0 {
        // Client: connect, send, read echo, exit.
        close(server_fd as i32);
        let client_fd = socket(AF_UNIX as i32, SOCK_STREAM as i32, 0);
        if client_fd < 0 {
            exit(1);
        }
        let addr = SockaddrUn::new("/tmp/test.sock");
        let ret = connect_unix(client_fd as i32, &addr);
        if ret < 0 {
            exit(2);
        }
        let msg = b"stream msg";
        let _ = write(client_fd as i32, msg);
        // Shutdown write so server sees EOF.
        shutdown(client_fd as i32, SHUT_WR);
        let mut buf = [0u8; 64];
        let n = read(client_fd as i32, &mut buf);
        close(client_fd as i32);
        if n == msg.len() as isize && &buf[..n as usize] == msg {
            exit(0);
        } else {
            exit(3);
        }
    }

    // Server: accept, read, echo back, close.
    let conn_fd = accept(server_fd as i32, None);
    if conn_fd < 0 {
        fail(b"accept() failed");
        close(server_fd as i32);
        return false;
    }
    ok(b"accept()");

    let mut buf = [0u8; 64];
    let n = read(conn_fd as i32, &mut buf);
    if n <= 0 {
        fail(b"server read failed");
        close(conn_fd as i32);
        close(server_fd as i32);
        return false;
    }
    ok(b"server read");

    // Echo back.
    let _ = write(conn_fd as i32, &buf[..n as usize]);
    close(conn_fd as i32);
    close(server_fd as i32);

    let mut status: i32 = -1;
    let _ = syscall_lib::waitpid(pid as i32, &mut status, 0);
    let exit_code = (status >> 8) & 0xff;
    if exit_code == 0 {
        ok(b"client verified echo");
        true
    } else {
        fail(b"client failed");
        false
    }
}

/// Test 4: Datagram socket — send two separate datagrams, receive as two separate messages.
fn test_datagram() -> bool {
    print(b"[datagram socket test]\n");

    let recv_fd = socket(AF_UNIX as i32, SOCK_DGRAM as i32, 0);
    if recv_fd < 0 {
        fail(b"socket(DGRAM) failed");
        return false;
    }

    // Unlink stale socket from prior run.
    let _ = unlink(b"/tmp/dgram.sock\0");
    let recv_addr = SockaddrUn::new("/tmp/dgram.sock");
    let ret = bind_unix(recv_fd as i32, &recv_addr);
    if ret < 0 {
        fail(b"bind(dgram) failed");
        close(recv_fd as i32);
        return false;
    }
    ok(b"bind dgram receiver");

    let send_fd = socket(AF_UNIX as i32, SOCK_DGRAM as i32, 0);
    if send_fd < 0 {
        fail(b"socket(DGRAM sender) failed");
        close(recv_fd as i32);
        return false;
    }

    // Send two datagrams of different sizes.
    let msg1 = b"short";
    let msg2 = b"a longer message here";
    let n1 = sendto_unix(send_fd as i32, msg1, 0, &recv_addr);
    let n2 = sendto_unix(send_fd as i32, msg2, 0, &recv_addr);
    if n1 != msg1.len() as isize || n2 != msg2.len() as isize {
        fail(b"sendto failed");
        close(send_fd as i32);
        close(recv_fd as i32);
        return false;
    }
    ok(b"sent two datagrams");

    // Receive first datagram.
    let mut buf = [0u8; 128];
    let mut sender = SockaddrUn::new("");
    let r1 = recvfrom_unix(recv_fd as i32, &mut buf, 0, &mut sender);
    if r1 != msg1.len() as isize || &buf[..r1 as usize] != msg1 {
        fail(b"first recvfrom mismatch");
        close(send_fd as i32);
        close(recv_fd as i32);
        return false;
    }
    ok(b"first datagram correct");

    // Receive second datagram.
    let r2 = recvfrom_unix(recv_fd as i32, &mut buf, 0, &mut sender);
    if r2 != msg2.len() as isize || &buf[..r2 as usize] != msg2 {
        fail(b"second recvfrom mismatch");
        close(send_fd as i32);
        close(recv_fd as i32);
        return false;
    }
    ok(b"second datagram correct (boundary preserved)");

    close(send_fd as i32);
    close(recv_fd as i32);
    true
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    write_str(1, "=== Unix Domain Socket Tests ===\n");

    let mut pass = 0;
    let mut total = 0;

    total += 1;
    if test_socketpair() {
        pass += 1;
    }

    total += 1;
    if test_socketpair_fork() {
        pass += 1;
    }

    total += 1;
    if test_named_stream() {
        pass += 1;
    }

    total += 1;
    if test_datagram() {
        pass += 1;
    }

    if pass == total {
        write_str(1, "\nAll tests passed!\n");
        exit(0);
    } else {
        write_str(1, "\nSome tests FAILED!\n");
        exit(1);
    }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    write_str(2, "unix-socket-test: PANIC\n");
    exit(101)
}
