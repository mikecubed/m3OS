//! Phase 69d follow-up — `sendmsg-test`.
//!
//! Asserts the kernel's `sendmsg`/`recvmsg` with `SCM_RIGHTS` correctly
//! ferries an fd between two ends of a `socketpair(AF_UNIX, SOCK_STREAM)`:
//!
//! 1. Build a pipe.  Read end `R`, write end `W`.
//! 2. `socketpair` → two stream sockets `S0`, `S1`.
//! 3. `sendmsg(S0, ...)` carrying one inline byte plus an SCM_RIGHTS
//!    cmsg of `[R]`.
//! 4. `recvmsg(S1, ...)` recovers the inline byte and a *new* fd `R2`
//!    that points at the same pipe-read backend as `R`.
//! 5. Write into `W`, read from `R2`, assert the bytes match.
//! 6. Print `SENDMSG_SMOKE:scm-rights:ok`.

#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

use core::alloc::Layout;

use syscall_lib::heap::BrkAllocator;
use syscall_lib::{
    AF_UNIX, IoVec, MsgHdr, SOCK_STREAM, STDOUT_FILENO, close, exit, pipe, read, recvmsg, sendmsg,
    socketpair, write,
};

#[global_allocator]
static ALLOCATOR: BrkAllocator = BrkAllocator::new();

#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    let _ = write(STDOUT_FILENO, b"sendmsg-test: alloc error\n");
    exit(99)
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    let _ = write(STDOUT_FILENO, b"sendmsg-test: PANIC\n");
    exit(101)
}

// CMSG header layout on Linux x86_64.
const CMSGHDR_SIZE: usize = 16;
const SOL_SOCKET: i32 = 1;
const SCM_RIGHTS: i32 = 1;

fn fail(reason: &[u8]) -> ! {
    let _ = write(STDOUT_FILENO, b"SENDMSG_SMOKE:scm-rights:fail ");
    let _ = write(STDOUT_FILENO, reason);
    let _ = write(STDOUT_FILENO, b"\n");
    exit(2)
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    let _ = write(STDOUT_FILENO, b"SENDMSG_SMOKE:scm-rights:begin\n");

    // 1. Pipe.
    let mut pipe_fds = [0i32; 2];
    if pipe(&mut pipe_fds) < 0 {
        fail(b"pipe");
    }
    let pipe_r = pipe_fds[0];
    let pipe_w = pipe_fds[1];

    // 2. Stream socket pair.
    let mut sv = [0i32; 2];
    if socketpair(AF_UNIX as i32, SOCK_STREAM as i32, 0, &mut sv) < 0 {
        fail(b"socketpair");
    }
    let s0 = sv[0];
    let s1 = sv[1];

    // 3. Encode CMSG buffer manually: cmsghdr (16 bytes) + i32 fd (4) + 4-byte align tail.
    let mut cmsg_buf = [0u8; 24];
    let cmsg_len: u64 = (CMSGHDR_SIZE + 4) as u64; // 20
    cmsg_buf[0..8].copy_from_slice(&cmsg_len.to_le_bytes());
    cmsg_buf[8..12].copy_from_slice(&SOL_SOCKET.to_le_bytes());
    cmsg_buf[12..16].copy_from_slice(&SCM_RIGHTS.to_le_bytes());
    cmsg_buf[16..20].copy_from_slice(&pipe_r.to_le_bytes());

    let mut send_payload = [b'X'];
    let mut send_iov = IoVec {
        iov_base: send_payload.as_mut_ptr(),
        iov_len: send_payload.len(),
    };
    let send_msg = MsgHdr {
        msg_name: core::ptr::null_mut(),
        msg_namelen: 0,
        _pad0: 0,
        msg_iov: &mut send_iov as *mut IoVec,
        msg_iovlen: 1,
        msg_control: cmsg_buf.as_mut_ptr(),
        msg_controllen: cmsg_buf.len() as u64,
        msg_flags: 0,
        _pad1: 0,
    };
    let sent = sendmsg(s0, &send_msg, 0);
    if sent != 1 {
        fail(b"sendmsg");
    }

    // 4. Recv on s1 with a control buffer big enough for one fd.
    let mut recv_payload = [0u8; 1];
    let mut recv_iov = IoVec {
        iov_base: recv_payload.as_mut_ptr(),
        iov_len: recv_payload.len(),
    };
    let mut recv_control = [0u8; 24];
    let mut recv_msg = MsgHdr {
        msg_name: core::ptr::null_mut(),
        msg_namelen: 0,
        _pad0: 0,
        msg_iov: &mut recv_iov as *mut IoVec,
        msg_iovlen: 1,
        msg_control: recv_control.as_mut_ptr(),
        msg_controllen: recv_control.len() as u64,
        msg_flags: 0,
        _pad1: 0,
    };
    let got = recvmsg(s1, &mut recv_msg, 0);
    if got != 1 {
        fail(b"recvmsg-bytes");
    }
    if recv_payload[0] != b'X' {
        fail(b"recvmsg-payload");
    }
    if recv_msg.msg_controllen == 0 {
        fail(b"recvmsg-cmsg-missing");
    }

    // Decode the cmsg and pull out the new fd.
    let got_cmsg_len = u64::from_le_bytes([
        recv_control[0],
        recv_control[1],
        recv_control[2],
        recv_control[3],
        recv_control[4],
        recv_control[5],
        recv_control[6],
        recv_control[7],
    ]);
    let got_level = i32::from_le_bytes([
        recv_control[8],
        recv_control[9],
        recv_control[10],
        recv_control[11],
    ]);
    let got_type = i32::from_le_bytes([
        recv_control[12],
        recv_control[13],
        recv_control[14],
        recv_control[15],
    ]);
    if got_cmsg_len < (CMSGHDR_SIZE + 4) as u64 || got_level != SOL_SOCKET || got_type != SCM_RIGHTS
    {
        fail(b"cmsg-fields");
    }
    let new_fd = i32::from_le_bytes([
        recv_control[16],
        recv_control[17],
        recv_control[18],
        recv_control[19],
    ]);
    if new_fd < 3 {
        fail(b"new-fd-range");
    }

    // 5. Write to original W; read should arrive on the recovered fd.
    let probe = b"HELLO";
    let w = write(pipe_w, probe);
    if w != probe.len() as isize {
        fail(b"pipe-write");
    }
    let mut buf = [0u8; 8];
    let r = read(new_fd, &mut buf);
    if r != probe.len() as isize {
        fail(b"new-fd-read-len");
    }
    if &buf[..probe.len()] != probe {
        fail(b"new-fd-read-bytes");
    }

    // 6. Cleanup.
    let _ = close(new_fd);
    let _ = close(s0);
    let _ = close(s1);
    let _ = close(pipe_r);
    let _ = close(pipe_w);

    let _ = write(STDOUT_FILENO, b"SENDMSG_SMOKE:scm-rights:ok\n");
    exit(0)
}
