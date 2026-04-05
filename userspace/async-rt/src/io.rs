//! AsyncFd — pollable file descriptor futures for async I/O.

use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};

use crate::executor;

/// Wraps a raw file descriptor for async I/O readiness.
///
/// Must be used within a `block_on()` call (uses the global reactor).
pub struct AsyncFd {
    fd: i32,
}

impl AsyncFd {
    /// Wrap a raw file descriptor for async I/O.
    pub fn new(fd: i32) -> Self {
        Self { fd }
    }

    /// Returns a future that resolves when the FD is readable (POLLIN).
    pub fn readable(&self) -> ReadableFuture {
        ReadableFuture { fd: self.fd }
    }

    /// Returns a future that resolves when the FD is writable (POLLOUT).
    pub fn writable(&self) -> WritableFuture {
        WritableFuture { fd: self.fd }
    }

    /// Get the underlying file descriptor.
    pub fn as_raw_fd(&self) -> i32 {
        self.fd
    }
}

// ---------------------------------------------------------------------------
// Platform-specific non-blocking readiness checks
// ---------------------------------------------------------------------------

#[cfg(feature = "std")]
fn check_fd_readable(fd: i32) -> bool {
    let mut pfd = libc::pollfd {
        fd,
        events: libc::POLLIN as i16,
        revents: 0,
    };
    let ret = unsafe { libc::poll(&mut pfd, 1, 0) };
    ret > 0
        && (pfd.revents & (libc::POLLIN as i16 | libc::POLLHUP as i16 | libc::POLLERR as i16)) != 0
}

#[cfg(not(feature = "std"))]
fn check_fd_readable(fd: i32) -> bool {
    let mut pfd = syscall_lib::PollFd {
        fd,
        events: syscall_lib::POLLIN,
        revents: 0,
    };
    let ret = syscall_lib::poll(core::slice::from_mut(&mut pfd), 0);
    ret > 0
        && (pfd.revents & (syscall_lib::POLLIN | syscall_lib::POLLHUP | syscall_lib::POLLERR)) != 0
}

#[cfg(feature = "std")]
fn check_fd_writable(fd: i32) -> bool {
    let mut pfd = libc::pollfd {
        fd,
        events: libc::POLLOUT as i16,
        revents: 0,
    };
    let ret = unsafe { libc::poll(&mut pfd, 1, 0) };
    ret > 0 && (pfd.revents & libc::POLLOUT as i16) != 0
}

#[cfg(not(feature = "std"))]
fn check_fd_writable(fd: i32) -> bool {
    let mut pfd = syscall_lib::PollFd {
        fd,
        events: syscall_lib::POLLOUT,
        revents: 0,
    };
    let ret = syscall_lib::poll(core::slice::from_mut(&mut pfd), 0);
    ret > 0 && (pfd.revents & syscall_lib::POLLOUT) != 0
}

// ---------------------------------------------------------------------------
// ReadableFuture / WritableFuture
// ---------------------------------------------------------------------------

/// Future that resolves when a file descriptor becomes readable.
pub struct ReadableFuture {
    fd: i32,
}

impl Future for ReadableFuture {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if check_fd_readable(self.fd) {
            Poll::Ready(())
        } else {
            executor::reactor().register_read(self.fd, cx.waker().clone());
            if check_fd_readable(self.fd) {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        }
    }
}

/// Future that resolves when a file descriptor becomes writable.
pub struct WritableFuture {
    fd: i32,
}

impl Future for WritableFuture {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if check_fd_writable(self.fd) {
            Poll::Ready(())
        } else {
            executor::reactor().register_write(self.fd, cx.waker().clone());
            if check_fd_writable(self.fd) {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::{block_on, spawn};
    use crate::reactor::Reactor;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread;
    use std::time::Duration;

    fn make_pipe() -> (i32, i32) {
        let mut fds = [0i32; 2];
        unsafe { libc::pipe(fds.as_mut_ptr()) };
        (fds[0], fds[1])
    }

    // E.1: readable() resolves immediately when data is available
    #[test]
    fn test_readable_immediate() {
        let mut reactor = Reactor::new();
        let (read_fd, write_fd) = make_pipe();

        unsafe { libc::write(write_fd, [42u8].as_ptr() as *const _, 1) };

        let async_fd = AsyncFd::new(read_fd);
        block_on(&mut reactor, async_fd.readable());

        unsafe {
            libc::close(read_fd);
            libc::close(write_fd);
        }
    }

    // E.1: readable() resolves after data arrives
    #[test]
    fn test_readable_delayed() {
        let mut reactor = Reactor::new();
        let (read_fd, write_fd) = make_pipe();

        thread::spawn(move || {
            thread::sleep(Duration::from_millis(20));
            unsafe { libc::write(write_fd, [1u8].as_ptr() as *const _, 1) };
        });

        let async_fd = AsyncFd::new(read_fd);
        block_on(&mut reactor, async_fd.readable());

        unsafe { libc::close(read_fd) };
    }

    // E.2: writable() resolves immediately for a pipe with buffer space
    #[test]
    fn test_writable_immediate() {
        let mut reactor = Reactor::new();
        let (read_fd, write_fd) = make_pipe();

        let async_fd = AsyncFd::new(write_fd);
        block_on(&mut reactor, async_fd.writable());

        unsafe {
            libc::close(read_fd);
            libc::close(write_fd);
        }
    }

    // E.3: Bidirectional relay pattern
    #[test]
    fn test_bidirectional_relay() {
        let mut reactor = Reactor::new();
        let (r_a, w_a) = make_pipe();
        let (r_b, w_b) = make_pipe();

        let data = b"hello relay";
        unsafe { libc::write(w_a, data.as_ptr() as *const _, data.len()) };

        let async_r_a = AsyncFd::new(r_a);
        let async_w_b = AsyncFd::new(w_b);

        block_on(&mut reactor, async {
            async_r_a.readable().await;

            let mut buf = [0u8; 64];
            let n = unsafe { libc::read(r_a, buf.as_mut_ptr() as *mut _, buf.len()) };
            assert!(n > 0);

            async_w_b.writable().await;

            unsafe { libc::write(w_b, buf.as_ptr() as *const _, n as usize) };
        });

        let mut result = [0u8; 64];
        let n = unsafe { libc::read(r_b, result.as_mut_ptr() as *mut _, result.len()) };
        assert_eq!(&result[..n as usize], data);

        unsafe {
            libc::close(r_a);
            libc::close(w_a);
            libc::close(r_b);
            libc::close(w_b);
        }
    }

    // Spurious wakes must not cause ReadableFuture to resolve without data.
    #[test]
    fn test_readable_spurious_wake_stays_pending() {
        let mut reactor = Reactor::new();
        let (read_fd, write_fd) = make_pipe();

        let completed = Arc::new(AtomicBool::new(false));
        let completed2 = completed.clone();

        block_on(&mut reactor, async move {
            let handle = spawn(async move {
                AsyncFd::new(read_fd).readable().await;
                completed2.store(true, Ordering::Release);
            });

            // Yield several times — executor re-polls the spawned task each
            // time, simulating spurious wakes.  The old registered-flag impl
            // would resolve Ready on the second poll; the fixed version stays
            // Pending because the FD is not actually readable.
            for _ in 0..10 {
                core::future::poll_fn(|cx| {
                    cx.waker().wake_by_ref();
                    core::task::Poll::Ready(())
                })
                .await;
            }

            assert!(
                !completed.load(Ordering::Acquire),
                "ReadableFuture resolved without data (spurious wake bug)"
            );

            // Write data so the spawned task can complete.
            unsafe { libc::write(write_fd, [1u8].as_ptr() as *const _, 1) };
            handle.await.unwrap();
        });

        unsafe {
            libc::close(read_fd);
            libc::close(write_fd);
        }
    }

    // Spurious wakes must not cause WritableFuture to resolve prematurely.
    // (Writable on a pipe with space resolves immediately, so we test with a
    // full-pipe scenario where the FD is NOT writable.)
    #[test]
    fn test_writable_spurious_wake_stays_pending() {
        let mut reactor = Reactor::new();
        let (read_fd, write_fd) = make_pipe();

        // Make write_fd non-blocking so we can fill without blocking.
        unsafe {
            let flags = libc::fcntl(write_fd, libc::F_GETFL);
            libc::fcntl(write_fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
        }

        // Fill the pipe buffer until it would block.
        let buf = [0u8; 4096];
        loop {
            let n = unsafe { libc::write(write_fd, buf.as_ptr() as *const _, buf.len()) };
            if n <= 0 {
                break;
            }
        }

        let completed = Arc::new(AtomicBool::new(false));
        let completed2 = completed.clone();

        block_on(&mut reactor, async move {
            let handle = spawn(async move {
                AsyncFd::new(write_fd).writable().await;
                completed2.store(true, Ordering::Release);
            });

            for _ in 0..10 {
                core::future::poll_fn(|cx| {
                    cx.waker().wake_by_ref();
                    core::task::Poll::Ready(())
                })
                .await;
            }

            assert!(
                !completed.load(Ordering::Acquire),
                "WritableFuture resolved on a full pipe (spurious wake bug)"
            );

            // Drain some data so the pipe becomes writable again.
            let mut drain = [0u8; 4096];
            unsafe { libc::read(read_fd, drain.as_mut_ptr() as *mut _, drain.len()) };
            handle.await.unwrap();
        });

        unsafe {
            libc::close(read_fd);
            libc::close(write_fd);
        }
    }
}
