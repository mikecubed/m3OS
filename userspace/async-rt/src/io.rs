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
        ReadableFuture {
            fd: self.fd,
            registered: false,
        }
    }

    /// Returns a future that resolves when the FD is writable (POLLOUT).
    pub fn writable(&self) -> WritableFuture {
        WritableFuture {
            fd: self.fd,
            registered: false,
        }
    }

    /// Get the underlying file descriptor.
    pub fn as_raw_fd(&self) -> i32 {
        self.fd
    }
}

/// Future that resolves when a file descriptor becomes readable.
pub struct ReadableFuture {
    fd: i32,
    registered: bool,
}

impl Future for ReadableFuture {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.registered {
            Poll::Ready(())
        } else {
            self.registered = true;
            executor::reactor().register_read(self.fd, cx.waker().clone());
            Poll::Pending
        }
    }
}

/// Future that resolves when a file descriptor becomes writable.
pub struct WritableFuture {
    fd: i32,
    registered: bool,
}

impl Future for WritableFuture {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.registered {
            Poll::Ready(())
        } else {
            self.registered = true;
            executor::reactor().register_write(self.fd, cx.waker().clone());
            Poll::Pending
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::block_on;
    use crate::reactor::Reactor;
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
}
