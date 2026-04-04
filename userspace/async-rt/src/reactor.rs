//! Poll-based I/O readiness reactor.
//!
//! Owns a self-pipe and a list of FD interests. Calls `poll()` and wakes
//! the appropriate wakers when FDs become ready.

#[cfg(not(feature = "std"))]
use alloc::vec;
#[cfg(not(feature = "std"))]
use alloc::vec::Vec;

use core::task::Waker;

use crate::task::set_wake_pipe_fd;

/// An interest registration for a file descriptor.
pub struct Interest {
    pub fd: i32,
    pub read_waker: Option<Waker>,
    pub write_waker: Option<Waker>,
}

/// The reactor manages I/O readiness via `poll()`.
pub struct Reactor {
    pub(crate) wake_read_fd: i32,
    pub(crate) wake_write_fd: i32,
    pub(crate) interests: Vec<Interest>,
}

impl Reactor {
    /// Create a new reactor with a self-pipe for waker signalling.
    pub fn new() -> Self {
        let (read_fd, write_fd) = Self::create_pipe();
        Self::make_nonblocking(read_fd);
        Self::make_nonblocking(write_fd);
        set_wake_pipe_fd(write_fd);

        Reactor {
            wake_read_fd: read_fd,
            wake_write_fd: write_fd,
            interests: Vec::new(),
        }
    }

    /// Register a file descriptor for read readiness.
    pub fn register_read(&mut self, fd: i32, waker: Waker) {
        if let Some(interest) = self.interests.iter_mut().find(|i| i.fd == fd) {
            interest.read_waker = Some(waker);
        } else {
            self.interests.push(Interest {
                fd,
                read_waker: Some(waker),
                write_waker: None,
            });
        }
    }

    /// Register a file descriptor for write readiness.
    pub fn register_write(&mut self, fd: i32, waker: Waker) {
        if let Some(interest) = self.interests.iter_mut().find(|i| i.fd == fd) {
            interest.write_waker = Some(waker);
        } else {
            self.interests.push(Interest {
                fd,
                read_waker: None,
                write_waker: Some(waker),
            });
        }
    }

    /// Remove all interest registrations for a file descriptor.
    pub fn deregister(&mut self, fd: i32) {
        self.interests.retain(|i| i.fd != fd);
    }

    /// Poll for I/O readiness once. Returns the number of ready FDs (excluding
    /// the self-pipe). Wakes registered wakers for ready FDs.
    pub fn poll_once(&mut self, timeout_ms: i32) -> usize {
        // Build pollfd array: interests + self-pipe read end
        let n = self.interests.len();
        let mut pollfds = vec![Self::zero_pollfd(); n + 1];

        for (i, interest) in self.interests.iter().enumerate() {
            pollfds[i].fd = interest.fd;
            let mut events: i16 = 0;
            if interest.read_waker.is_some() {
                events |= Self::pollin();
            }
            if interest.write_waker.is_some() {
                events |= Self::pollout();
            }
            pollfds[i].events = events;
        }

        // Self-pipe entry
        pollfds[n].fd = self.wake_read_fd;
        pollfds[n].events = Self::pollin();

        let ret = Self::do_poll(&mut pollfds, timeout_ms);
        if ret < 0 {
            return 0;
        }

        let mut ready_count = 0;

        // Check interest FDs
        for i in 0..n {
            let revents = pollfds[i].revents;
            if revents == 0 {
                continue;
            }
            let interest = &self.interests[i];

            if (revents & (Self::pollin() | Self::pollhup() | Self::pollerr())) != 0 {
                if let Some(ref waker) = interest.read_waker {
                    waker.wake_by_ref();
                }
            }
            if (revents & Self::pollout()) != 0 {
                if let Some(ref waker) = interest.write_waker {
                    waker.wake_by_ref();
                }
            }
            ready_count += 1;
        }

        // Drain self-pipe
        if pollfds[n].revents != 0 {
            self.drain_wake_pipe();
        }

        ready_count
    }

    fn drain_wake_pipe(&self) {
        let mut buf = [0u8; 64];
        loop {
            let n = Self::do_read(self.wake_read_fd, &mut buf);
            if n <= 0 {
                break;
            }
        }
    }

    // Platform-specific helpers

    #[cfg(feature = "std")]
    fn create_pipe() -> (i32, i32) {
        let mut fds = [0i32; 2];
        unsafe { libc::pipe(fds.as_mut_ptr()) };
        (fds[0], fds[1])
    }

    #[cfg(not(feature = "std"))]
    fn create_pipe() -> (i32, i32) {
        let mut fds = [0i32; 2];
        syscall_lib::pipe(&mut fds);
        (fds[0], fds[1])
    }

    #[cfg(feature = "std")]
    fn make_nonblocking(fd: i32) {
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFL);
            libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
    }

    #[cfg(not(feature = "std"))]
    fn make_nonblocking(fd: i32) {
        syscall_lib::set_nonblocking(fd);
    }

    // PollFd abstraction — use libc under std, syscall-lib under no_std

    #[cfg(feature = "std")]
    fn zero_pollfd() -> libc::pollfd {
        libc::pollfd {
            fd: -1,
            events: 0,
            revents: 0,
        }
    }

    #[cfg(not(feature = "std"))]
    fn zero_pollfd() -> syscall_lib::PollFd {
        syscall_lib::PollFd {
            fd: -1,
            events: 0,
            revents: 0,
        }
    }

    #[cfg(feature = "std")]
    fn pollin() -> i16 {
        libc::POLLIN as i16
    }
    #[cfg(not(feature = "std"))]
    fn pollin() -> i16 {
        syscall_lib::POLLIN
    }

    #[cfg(feature = "std")]
    fn pollout() -> i16 {
        libc::POLLOUT as i16
    }
    #[cfg(not(feature = "std"))]
    fn pollout() -> i16 {
        syscall_lib::POLLOUT
    }

    #[cfg(feature = "std")]
    fn pollhup() -> i16 {
        libc::POLLHUP as i16
    }
    #[cfg(not(feature = "std"))]
    fn pollhup() -> i16 {
        syscall_lib::POLLHUP
    }

    #[cfg(feature = "std")]
    fn pollerr() -> i16 {
        libc::POLLERR as i16
    }
    #[cfg(not(feature = "std"))]
    fn pollerr() -> i16 {
        syscall_lib::POLLERR
    }

    #[cfg(feature = "std")]
    fn do_poll(fds: &mut [libc::pollfd], timeout_ms: i32) -> isize {
        unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout_ms) as isize }
    }

    #[cfg(not(feature = "std"))]
    fn do_poll(fds: &mut [syscall_lib::PollFd], timeout_ms: i32) -> isize {
        syscall_lib::poll(fds, timeout_ms)
    }

    #[cfg(feature = "std")]
    fn do_read(fd: i32, buf: &mut [u8]) -> isize {
        unsafe { libc::read(fd, buf.as_mut_ptr() as *mut _, buf.len()) }
    }

    #[cfg(not(feature = "std"))]
    fn do_read(fd: i32, buf: &mut [u8]) -> isize {
        syscall_lib::read(fd, buf)
    }
}

impl Drop for Reactor {
    fn drop(&mut self) {
        set_wake_pipe_fd(-1);
        #[cfg(feature = "std")]
        unsafe {
            libc::close(self.wake_read_fd);
            libc::close(self.wake_write_fd);
        }
        #[cfg(not(feature = "std"))]
        {
            syscall_lib::close(self.wake_read_fd);
            syscall_lib::close(self.wake_write_fd);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::{Duration, Instant};

    fn make_pipe() -> (i32, i32) {
        let mut fds = [0i32; 2];
        unsafe { libc::pipe(fds.as_mut_ptr()) };
        (fds[0], fds[1])
    }

    // C.1: Reactor::new() succeeds, self-pipe FDs are valid
    #[test]
    fn test_reactor_new() {
        let reactor = Reactor::new();
        assert!(reactor.wake_read_fd >= 0);
        assert!(reactor.wake_write_fd >= 0);
        assert!(reactor.interests.is_empty());
    }

    // C.2: Register read-end for POLLIN, write to write-end, poll_once — waker fires
    #[test]
    fn test_register_and_poll_wakeup() {
        let mut reactor = Reactor::new();
        let (read_fd, write_fd) = make_pipe();

        let task = crate::task::Task::new(Box::pin(async {}));
        task.clear_woken();
        let waker = crate::task::waker_for_task(&task);

        reactor.register_read(read_fd, waker);

        // Write a byte to make read end readable
        unsafe { libc::write(write_fd, [42u8].as_ptr() as *const _, 1) };

        let ready = reactor.poll_once(100);
        assert_eq!(ready, 1);
        assert!(task.is_woken());

        unsafe {
            libc::close(read_fd);
            libc::close(write_fd);
        }
    }

    // C.2: Register two pipes, write to only one, verify only that pipe's waker fires
    #[test]
    fn test_selective_wakeup() {
        let mut reactor = Reactor::new();
        let (r1, w1) = make_pipe();
        let (r2, w2) = make_pipe();

        let task1 = crate::task::Task::new(Box::pin(async {}));
        task1.clear_woken();
        let waker1 = crate::task::waker_for_task(&task1);

        let task2 = crate::task::Task::new(Box::pin(async {}));
        task2.clear_woken();
        let waker2 = crate::task::waker_for_task(&task2);

        reactor.register_read(r1, waker1);
        reactor.register_read(r2, waker2);

        // Only write to pipe 1
        unsafe { libc::write(w1, [1u8].as_ptr() as *const _, 1) };

        reactor.poll_once(100);
        assert!(task1.is_woken());
        assert!(!task2.is_woken());

        unsafe {
            libc::close(r1);
            libc::close(w1);
            libc::close(r2);
            libc::close(w2);
        }
    }

    // C.3: No ready FDs — poll times out, no wakers called
    #[test]
    fn test_poll_timeout() {
        let mut reactor = Reactor::new();
        let (read_fd, write_fd) = make_pipe();

        let task = crate::task::Task::new(Box::pin(async {}));
        task.clear_woken();
        let waker = crate::task::waker_for_task(&task);
        reactor.register_read(read_fd, waker);

        let start = Instant::now();
        let ready = reactor.poll_once(50);
        let elapsed = start.elapsed();

        assert_eq!(ready, 0);
        assert!(!task.is_woken());
        assert!(elapsed >= Duration::from_millis(30)); // allow some slack

        unsafe {
            libc::close(read_fd);
            libc::close(write_fd);
        }
    }

    // C.4: Self-pipe wakeup interrupts poll
    #[test]
    fn test_self_pipe_wakeup_interrupts_poll() {
        let mut reactor = Reactor::new();
        let wake_write = reactor.wake_write_fd;

        // Spawn a thread that writes to the self-pipe after 10ms
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(10));
            unsafe {
                libc::write(wake_write, [1u8].as_ptr() as *const _, 1);
            }
        });

        let start = Instant::now();
        reactor.poll_once(5000); // would block 5s without self-pipe
        let elapsed = start.elapsed();

        // Should return much sooner than 5s
        assert!(elapsed < Duration::from_millis(1000));
    }

    // C.5: Deregister removes FD from interests
    #[test]
    fn test_deregister() {
        let mut reactor = Reactor::new();
        let (read_fd, write_fd) = make_pipe();

        let task = crate::task::Task::new(Box::pin(async {}));
        task.clear_woken();
        let waker = crate::task::waker_for_task(&task);

        reactor.register_read(read_fd, waker);
        reactor.deregister(read_fd);

        // Write data — but FD is no longer registered
        unsafe { libc::write(write_fd, [1u8].as_ptr() as *const _, 1) };

        let ready = reactor.poll_once(10);
        assert_eq!(ready, 0);
        assert!(!task.is_woken());

        unsafe {
            libc::close(read_fd);
            libc::close(write_fd);
        }
    }
}
