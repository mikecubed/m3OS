//! Phase 57 Track G.3 — PTY pair host + shell spawn.
//!
//! Red commit: this file declares the trait + types so the tests
//! compile.  Every method body is `unimplemented!()` so the tests fail
//! at runtime.  The green commit (next) lands the real bodies.

use crate::TermError;

/// Errors observable on the PTY public surface. Variants are typed
/// (no string payloads) so callers can match and recover.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum PtyError {
    /// `openpty` returned a negative errno.
    OpenFailed(i32),
    /// `fork` returned a negative errno.
    ForkFailed(i32),
    /// `execve` of the shell returned a negative errno.
    ExecFailed(i32),
}

impl From<PtyError> for TermError {
    fn from(_err: PtyError) -> Self {
        unimplemented!("G.3 green commit lands this")
    }
}

/// Syscall seam for the PTY host.
pub trait PtyOps {
    fn openpty(&mut self) -> Result<(i32, i32), i32>;
    fn fork(&mut self) -> i32;
    fn exec_shell(&mut self, secondary_fd: i32) -> !;
    fn close(&mut self, fd: i32) -> i32;
    fn try_wait(&mut self, pid: i32) -> Result<Option<i32>, i32>;
}

/// Shell-process lifecycle owner.
pub struct PtyHost<O: PtyOps> {
    #[allow(dead_code)]
    ops: O,
}

impl<O: PtyOps> PtyHost<O> {
    pub fn new(_ops: O) -> Self {
        unimplemented!("G.3 green commit lands this")
    }

    pub fn open_and_spawn(&mut self) -> Result<i32, PtyError> {
        unimplemented!("G.3 green commit lands this")
    }

    pub fn primary_fd(&self) -> Option<i32> {
        unimplemented!("G.3 green commit lands this")
    }

    pub fn shell_pid(&self) -> Option<i32> {
        unimplemented!("G.3 green commit lands this")
    }

    pub fn poll_shell_exit(&mut self) -> Result<Option<i32>, i32> {
        unimplemented!("G.3 green commit lands this")
    }

    pub fn close_primary(&mut self) {
        unimplemented!("G.3 green commit lands this")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    struct MockPtyOps {
        next_openpty: Result<(i32, i32), i32>,
        next_fork_pid: i32,
        next_try_wait: Vec<Result<Option<i32>, i32>>,
        closed_fds: Vec<i32>,
        exec_called_with: Option<i32>,
    }

    impl MockPtyOps {
        fn new() -> Self {
            Self {
                next_openpty: Ok((10, 11)),
                next_fork_pid: 42,
                next_try_wait: Vec::new(),
                closed_fds: Vec::new(),
                exec_called_with: None,
            }
        }
    }

    impl PtyOps for MockPtyOps {
        fn openpty(&mut self) -> Result<(i32, i32), i32> {
            self.next_openpty
        }

        fn fork(&mut self) -> i32 {
            self.next_fork_pid
        }

        fn exec_shell(&mut self, secondary_fd: i32) -> ! {
            self.exec_called_with = Some(secondary_fd);
            panic!("MockPtyOps::exec_shell called from parent path");
        }

        fn close(&mut self, fd: i32) -> i32 {
            self.closed_fds.push(fd);
            0
        }

        fn try_wait(&mut self, _pid: i32) -> Result<Option<i32>, i32> {
            if self.next_try_wait.is_empty() {
                Ok(None)
            } else {
                self.next_try_wait.remove(0)
            }
        }
    }

    /// Phase 57 G.3 acceptance: parent path opens PTY, forks, closes
    /// the secondary fd, and records the shell pid.
    #[test]
    fn open_and_spawn_records_pid_and_closes_secondary() {
        let mut host = PtyHost::new(MockPtyOps::new());
        let pid = host.open_and_spawn().expect("happy path must succeed");
        assert_eq!(pid, 42);
        assert_eq!(host.primary_fd(), Some(10));
        assert_eq!(host.shell_pid(), Some(42));
    }

    #[test]
    fn open_failure_surfaces_typed_error() {
        let mut ops = MockPtyOps::new();
        ops.next_openpty = Err(-5);
        let mut host = PtyHost::new(ops);
        let err = host.open_and_spawn().expect_err("openpty err must surface");
        assert_eq!(err, PtyError::OpenFailed(-5));
        assert_eq!(host.primary_fd(), None);
        assert_eq!(host.shell_pid(), None);
    }

    #[test]
    fn fork_failure_rolls_back_open_fds() {
        let mut ops = MockPtyOps::new();
        ops.next_fork_pid = -12;
        let mut host = PtyHost::new(ops);
        let err = host.open_and_spawn().expect_err("fork err must surface");
        assert_eq!(err, PtyError::ForkFailed(-12));
        assert_eq!(host.primary_fd(), None);
        assert_eq!(host.shell_pid(), None);
    }

    #[test]
    fn poll_shell_exit_observes_running_then_exited() {
        let mut ops = MockPtyOps::new();
        ops.next_try_wait = alloc::vec![Ok(None), Ok(Some(0))];
        let mut host = PtyHost::new(ops);
        host.open_and_spawn().expect("spawn ok");
        assert_eq!(host.poll_shell_exit(), Ok(None));
        assert_eq!(host.poll_shell_exit(), Ok(Some(0)));
    }

    #[test]
    fn poll_shell_exit_without_spawn_returns_none() {
        let mut host = PtyHost::new(MockPtyOps::new());
        assert_eq!(host.poll_shell_exit(), Ok(None));
    }

    #[test]
    fn close_primary_is_idempotent() {
        let mut host = PtyHost::new(MockPtyOps::new());
        host.open_and_spawn().expect("spawn ok");
        host.close_primary();
        host.close_primary();
        assert_eq!(host.primary_fd(), None);
    }

    #[test]
    fn pty_error_lifts_into_term_error() {
        let from_open: TermError = PtyError::OpenFailed(-5).into();
        assert_eq!(from_open, TermError::PtyOpen(-5));
        let from_fork: TermError = PtyError::ForkFailed(-12).into();
        assert_eq!(from_fork, TermError::ShellSpawn(-12));
        let from_exec: TermError = PtyError::ExecFailed(-2).into();
        assert_eq!(from_exec, TermError::ShellSpawn(-2));
    }
}
