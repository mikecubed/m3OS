//! Phase 57 Track G.3 — PTY pair host + shell spawn.
//!
//! `PtyHost` opens the primary/secondary PTY pair via the Phase 29
//! syscalls (`syscall_lib::openpty`), spawns the in-tree shell on the
//! secondary side with stdin/stdout/stderr wired up, and exposes an
//! `Option<i32>` shell exit-status surface so the binary's main loop
//! observes the shell quitting and shuts down cleanly.
//!
//! ## Why a trait?
//!
//! Production `term` runs against the real kernel: it forks, calls
//! `dup2` to wire the secondary side, then `execve`s the production
//! shell (`/bin/ion`, falling back to `/bin/sh0`). None of that
//! compiles on the host, so the [`PtyOps`] trait abstracts the
//! syscalls behind a seam. Host tests run [`PtyHost`] against
//! `MockPtyOps` to exercise the bring-up flow without touching the
//! kernel.
//!
//! ## Lifecycle (per the G.3 acceptance)
//!
//! 1. `PtyHost::new(ops)` constructs an unattached host.
//! 2. `PtyHost::open_and_spawn` opens the PTY pair, forks; child wires
//!    the secondary side as stdio and `execve`s the shell; parent
//!    records the child pid, closes the secondary fd, and keeps the
//!    primary for I/O.
//! 3. `PtyHost::poll_shell_exit` checks via `waitpid(WNOHANG)` whether
//!    the shell exited; returns `Ok(Some(status))` on exit, `Ok(None)`
//!    while still running.
//! 4. The binary's main loop calls `poll_shell_exit` between read /
//!    render passes; on `Some(_)` it closes the primary fd and exits
//!    zero so the supervisor restarts per `term.conf`.

use crate::TermError;

/// Decode the `(rc, raw_status)` pair returned by `waitpid(WNOHANG)`
/// into the [`PtyOps::try_wait`] contract. Pure logic so the
/// production [`crate::syscall_pty::SyscallPtyOps`] wrapping can stay
/// trivial while the bit-twiddling around POSIX wait macros gets
/// host-test coverage.
///
/// Mapping:
/// - `rc < 0`               → `Err(rc as i32)` — `waitpid` errno.
/// - `rc == 0` (`WNOHANG`)  → `Ok(None)` — child still running.
/// - `rc > 0` and `WIFEXITED(status)` (`status & 0x7F == 0`) → `Ok(Some((status >> 8) & 0xFF))`.
/// - `rc > 0` otherwise     → `Ok(Some(status))` — abnormal exit
///   (signal, etc.); caller logs and treats as exited.
pub fn decode_wait_status(rc: isize, raw_status: i32) -> Result<Option<i32>, i32> {
    if rc < 0 {
        return Err(rc as i32);
    }
    if rc == 0 {
        return Ok(None);
    }
    if (raw_status & 0x7F) == 0 {
        Ok(Some((raw_status >> 8) & 0xFF))
    } else {
        Ok(Some(raw_status))
    }
}

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
    fn from(err: PtyError) -> Self {
        match err {
            PtyError::OpenFailed(e) => TermError::PtyOpen(e),
            PtyError::ForkFailed(e) | PtyError::ExecFailed(e) => TermError::ShellSpawn(e),
        }
    }
}

/// Syscall seam for the PTY host. The production impl wraps
/// `syscall_lib::openpty`, `fork`, `dup2`, `execve`, and
/// `waitpid(WNOHANG)`. Host tests provide a recording mock.
pub trait PtyOps {
    /// Open a PTY pair. Returns `(primary_fd, secondary_fd)` on
    /// success; negative errno on failure.
    fn openpty(&mut self) -> Result<(i32, i32), i32>;

    /// Fork the calling process. Returns the child pid (>0) in the
    /// parent, 0 in the child, negative errno on failure.
    fn fork(&mut self) -> i32;

    /// Wire `secondary_fd` as stdin/stdout/stderr in the child and
    /// replace the process image with the shell. Only the child path
    /// reaches this method; the parent never calls it. The function
    /// does not return — on success the process image is replaced; on
    /// failure the implementation aborts the child.
    fn exec_shell(&mut self, secondary_fd: i32) -> !;

    /// Close a file descriptor.  Returns the underlying close errno.
    fn close(&mut self, fd: i32) -> i32;

    /// `waitpid(pid, WNOHANG)` — returns `Ok(Some(status))` if the
    /// child exited, `Ok(None)` if still running, `Err(errno)` on
    /// error.  Implementations decode the raw status into the exit
    /// code (or signal) before returning.
    fn try_wait(&mut self, pid: i32) -> Result<Option<i32>, i32>;
}

/// Shell-process lifecycle owner.
///
/// `PtyHost` is the only owner of the primary fd and the shell pid;
/// the binary's main loop borrows them through the public accessors.
/// The primary fd is *not* closed automatically — `PtyHost` does not
/// implement `Drop` because the close path is part of the
/// `PtyOps`-visible lifecycle (the syscall seam owns the fd table).
/// Callers must invoke [`PtyHost::close_primary`] explicitly when
/// shutting down (the binary's main loop calls it on shell exit).
/// The shell process itself is not killed by `PtyHost`; the
/// supervisor handles the lifecycle of the `term` process.
pub struct PtyHost<O: PtyOps> {
    ops: O,
    primary_fd: Option<i32>,
    shell_pid: Option<i32>,
}

impl<O: PtyOps> PtyHost<O> {
    /// Wrap a fresh `PtyOps` with no PTY open and no shell spawned.
    pub fn new(ops: O) -> Self {
        Self {
            ops,
            primary_fd: None,
            shell_pid: None,
        }
    }

    /// Open the PTY pair and spawn the shell. On success returns the
    /// shell pid; both `primary_fd` and `shell_pid` are now `Some(_)`.
    /// On failure the partial state (e.g. open primary fd) is
    /// rolled back so the caller observes a clean, unattached host.
    pub fn open_and_spawn(&mut self) -> Result<i32, PtyError> {
        let (primary_fd, secondary_fd) = self.ops.openpty().map_err(PtyError::OpenFailed)?;
        self.primary_fd = Some(primary_fd);
        let pid = self.ops.fork();
        if pid < 0 {
            // Roll back the open PTY pair so the caller sees no
            // partial state.
            self.ops.close(primary_fd);
            self.ops.close(secondary_fd);
            self.primary_fd = None;
            return Err(PtyError::ForkFailed(pid));
        }
        if pid == 0 {
            // Child path: never returns.  The implementation `dup2`s
            // the secondary fd onto stdin/stdout/stderr and `execve`s
            // the shell; failure aborts the child.  Close the primary
            // fd before `exec_shell` so it is not inherited by the
            // shell process — the primary belongs to the parent.
            self.ops.close(primary_fd);
            self.ops.exec_shell(secondary_fd);
        }
        // Parent path: close the secondary side and record the pid.
        self.ops.close(secondary_fd);
        self.shell_pid = Some(pid);
        Ok(pid)
    }

    /// Primary fd. `None` until [`open_and_spawn`] returns `Ok(_)`.
    pub fn primary_fd(&self) -> Option<i32> {
        self.primary_fd
    }

    /// Shell pid. `None` until [`open_and_spawn`] returns `Ok(_)`.
    pub fn shell_pid(&self) -> Option<i32> {
        self.shell_pid
    }

    /// Poll for shell exit. Returns:
    /// - `Ok(Some(status))` on shell exit (status decoded by the
    ///   `PtyOps` implementation).
    /// - `Ok(None)` while the shell is still running.
    /// - `Err(errno)` on a `waitpid` error (caller must decide whether
    ///   to retry or shut down).
    ///
    /// Returns `Ok(None)` if no shell has been spawned yet — the
    /// host's main loop calls this unconditionally and only acts on
    /// `Some(_)`.
    pub fn poll_shell_exit(&mut self) -> Result<Option<i32>, i32> {
        match self.shell_pid {
            None => Ok(None),
            Some(pid) => self.ops.try_wait(pid),
        }
    }

    /// Close the primary fd, if open. Idempotent: a second call after
    /// the fd has been taken is a no-op.
    pub fn close_primary(&mut self) {
        if let Some(fd) = self.primary_fd.take() {
            self.ops.close(fd);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    /// Recording mock that emulates a successful PTY pair allocation
    /// and a parent-side `fork`.  Tests configure `next_*` to inject
    /// failures.
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
            // Record that we got here, then "exit" the child path by
            // panicking.  In tests we never take the child path
            // because `next_fork_pid` is non-zero.
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
        // Secondary fd (11) must have been closed by the parent.
        assert!(host.ops.closed_fds.contains(&11));
        // Primary fd (10) is still open.
        assert!(!host.ops.closed_fds.contains(&10));
    }

    /// Phase 57 G.3 acceptance: openpty failure surfaces as
    /// `PtyError::OpenFailed` and leaves no partial state.
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

    /// Child path closes the primary fd before `exec_shell` so the
    /// shell process does not inherit it. The mock's `exec_shell`
    /// panics, so we observe the close order via `closed_fds` and
    /// catch the unwind here in the test thread.
    #[test]
    fn child_path_closes_primary_before_exec_shell() {
        use std::panic::{AssertUnwindSafe, catch_unwind};
        let mut ops = MockPtyOps::new();
        ops.next_fork_pid = 0;
        let mut host = PtyHost::new(ops);
        let _ = catch_unwind(AssertUnwindSafe(|| {
            let _ = host.open_and_spawn();
        }));
        // Primary fd (10) was closed before exec_shell.
        assert_eq!(host.ops.closed_fds, alloc::vec![10]);
        // exec_shell saw the secondary fd (11), and the secondary fd
        // was *not* closed by the parent before exec (the child owns
        // the secondary side now).
        assert_eq!(host.ops.exec_called_with, Some(11));
        assert!(!host.ops.closed_fds.contains(&11));
    }

    /// Phase 57 G.3 acceptance: fork failure rolls back the PTY pair.
    #[test]
    fn fork_failure_rolls_back_open_fds() {
        let mut ops = MockPtyOps::new();
        ops.next_fork_pid = -12;
        let mut host = PtyHost::new(ops);
        let err = host.open_and_spawn().expect_err("fork err must surface");
        assert_eq!(err, PtyError::ForkFailed(-12));
        // Both fds must have been closed.
        assert!(host.ops.closed_fds.contains(&10));
        assert!(host.ops.closed_fds.contains(&11));
        assert_eq!(host.primary_fd(), None);
        assert_eq!(host.shell_pid(), None);
    }

    /// Phase 57 G.3 acceptance: poll_shell_exit returns None before
    /// the shell exits and Some(status) after.
    #[test]
    fn poll_shell_exit_observes_running_then_exited() {
        let mut ops = MockPtyOps::new();
        ops.next_try_wait = alloc::vec![Ok(None), Ok(Some(0))];
        let mut host = PtyHost::new(ops);
        host.open_and_spawn().expect("spawn ok");
        assert_eq!(host.poll_shell_exit(), Ok(None));
        assert_eq!(host.poll_shell_exit(), Ok(Some(0)));
    }

    /// Phase 57 G.3 acceptance: poll_shell_exit returns None when no
    /// shell has been spawned yet.
    #[test]
    fn poll_shell_exit_without_spawn_returns_none() {
        let mut host = PtyHost::new(MockPtyOps::new());
        assert_eq!(host.poll_shell_exit(), Ok(None));
    }

    /// Close-primary is idempotent.
    #[test]
    fn close_primary_is_idempotent() {
        let mut host = PtyHost::new(MockPtyOps::new());
        host.open_and_spawn().expect("spawn ok");
        host.close_primary();
        // Calling again should not call close on a stale fd.
        let close_count_before = host.ops.closed_fds.len();
        host.close_primary();
        assert_eq!(host.ops.closed_fds.len(), close_count_before);
        assert_eq!(host.primary_fd(), None);
    }

    /// `decode_wait_status` rejects negative `rc` as the errno path.
    #[test]
    fn decode_wait_status_negative_rc_is_err() {
        assert_eq!(decode_wait_status(-1, 0), Err(-1));
        assert_eq!(decode_wait_status(-12, 0), Err(-12));
    }

    /// `rc == 0` → still running (WNOHANG).
    #[test]
    fn decode_wait_status_zero_rc_means_running() {
        assert_eq!(decode_wait_status(0, 0), Ok(None));
        // `raw_status` is irrelevant when `rc == 0` (the kernel only
        // populates status on a state change).
        assert_eq!(decode_wait_status(0, 0xDEAD_BEEFu32 as i32), Ok(None));
    }

    /// Standard normal exit: `WIFEXITED(status)` is `(status & 0x7F)
    /// == 0`; the exit code lives in `(status >> 8) & 0xFF`.
    #[test]
    fn decode_wait_status_normal_exit_extracts_exit_code() {
        assert_eq!(decode_wait_status(42, 0x0000), Ok(Some(0)));
        assert_eq!(decode_wait_status(42, 0x0100), Ok(Some(1)));
        assert_eq!(decode_wait_status(42, 0x6500), Ok(Some(0x65)));
        // High byte beyond 0xFF gets masked away (POSIX convention).
        assert_eq!(
            decode_wait_status(42, 0x12_00FF_00u32 as i32),
            Ok(Some(0xFF))
        );
    }

    /// Signal-killed children fall through to the raw-status path —
    /// callers log and treat as abnormal exit. Phase 57 does not yet
    /// distinguish signal-kill from a normal exit code.
    #[test]
    fn decode_wait_status_signal_killed_returns_raw_status() {
        // SIGKILL = 9; raw status low byte = 0x09 (no core dump).
        assert_eq!(decode_wait_status(42, 0x09), Ok(Some(0x09)));
    }

    /// `PtyError` lifts cleanly into `TermError` for the binary's
    /// top-level error surface.
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
