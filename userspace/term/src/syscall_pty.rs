//! Phase 57 Track G.3 close-out — production [`PtyOps`] backed by
//! `syscall_lib`.
//!
//! `PtyHost` operates against the abstract [`PtyOps`] trait so its
//! lifecycle can be exercised by host tests against `MockPtyOps`. The
//! production binary needs a real implementation that calls into the
//! kernel: this module supplies it. Gated behind
//! `cfg(all(not(test), feature = "os-binary"))` so host tests
//! continue to build and the kernel-target build picks the production
//! path automatically.
//!
//! ## Behaviour
//!
//! - `openpty` calls [`syscall_lib::openpty`], which opens `/dev/ptmx`,
//!   unlocks the slot via the `TIOCSPTLCK` ioctl, queries the
//!   slave-fd path via `TIOCGPTN`, and opens the matching `/dev/pts/N`.
//!   The returned `(primary, secondary)` pair are inheritable file
//!   descriptors.
//! - `fork` calls [`syscall_lib::fork`] verbatim. Returns the child
//!   pid (>0) in the parent, 0 in the child, or a negative errno on
//!   failure. `PtyHost::open_and_spawn` interprets the negative case
//!   as a fork failure and rolls back the open PTY pair.
//! - `exec_shell` is the production child path. It dup2's the
//!   secondary fd onto stdin / stdout / stderr (fds 0 / 1 / 2),
//!   closes the original secondary fd (it has been duplicated
//!   wherever it needs to live), and `execve`s the production shell
//!   at `/bin/ion` — the same default `login` exec's after reading
//!   `/etc/passwd`. On exec failure we fall through to `/bin/sh0`
//!   (the in-tree minimal shell), mirroring `login`'s
//!   "ion-first, sh0-fallback" shape so a broken / missing ion does
//!   not leave the user staring at a blank surface. On both paths
//!   failing the function `syscall_lib::exit`s the child with a
//!   distinct code so the supervisor's restart path records a
//!   clean failure.
//! - `close` wraps [`syscall_lib::close`] and returns its raw errno.
//! - `try_wait` calls [`syscall_lib::waitpid`] with `WNOHANG`. The
//!   raw status is decoded into the exit code using the standard
//!   POSIX `wait` macros: `WIFEXITED` checks the low byte, `WEXITSTATUS`
//!   shifts the high byte. Phase 57 does not yet care about
//!   signal-killed children; if `WIFEXITED` is false the function
//!   returns the raw status as-is so the caller can log and treat it
//!   as an abnormal exit.

use crate::pty::{PtyOps, decode_wait_status};
use syscall_lib::{STDOUT_FILENO, WNOHANG};

/// Production default shell. Matches the `/etc/passwd` `:/bin/ion`
/// entries and the path `login` exec's after authenticating. Spelled
/// as a null-terminated byte string so it can travel through
/// `execve` without any per-call allocation.
const SHELL_PATH_ION: &[u8] = b"/bin/ion\0";

/// Fallback shell — minimal in-tree shell that ships unconditionally.
/// Matches `login`'s "ion-first, sh0-fallback" recovery shape so a
/// broken or missing ion does not leave the user staring at a blank
/// surface.
const SHELL_PATH_SH0: &[u8] = b"/bin/sh0\0";

/// Distinct exit codes for the child path's failure modes. The
/// supervisor uses these to distinguish "shell binary missing" from
/// "dup2 failed" in the boot transcript without parsing free-form text.
const CHILD_EXIT_DUP2: i32 = 110;
const CHILD_EXIT_EXECVE: i32 = 111;

/// Production `PtyOps`: thin wrapper over `syscall_lib` that feeds
/// the same `PtyHost` lifecycle the host tests exercise against
/// `MockPtyOps`.
pub struct SyscallPtyOps;

impl SyscallPtyOps {
    pub const fn new() -> Self {
        Self
    }
}

impl Default for SyscallPtyOps {
    fn default() -> Self {
        Self::new()
    }
}

impl PtyOps for SyscallPtyOps {
    fn openpty(&mut self) -> Result<(i32, i32), i32> {
        syscall_lib::openpty()
    }

    fn fork(&mut self) -> i32 {
        let pid = syscall_lib::fork();
        // The kernel's SYS_FORK returns isize; clamp into i32 because
        // that's the PID width the rest of the lifecycle uses.
        if pid > i32::MAX as isize {
            return i32::MAX;
        }
        if pid < i32::MIN as isize {
            return i32::MIN;
        }
        pid as i32
    }

    fn exec_shell(&mut self, secondary_fd: i32) -> ! {
        // Wire the secondary side of the PTY onto stdin, stdout, and
        // stderr. dup2 returns the new fd on success, or a negative
        // errno on failure — abort the child on any negative result
        // so the supervisor records a clean failure.
        for target in 0..=2 {
            if syscall_lib::dup2(secondary_fd, target) < 0 {
                syscall_lib::write_str(STDOUT_FILENO, "term: dup2 failed in child\n");
                syscall_lib::exit(CHILD_EXIT_DUP2)
            }
        }
        // The duplicate has taken ownership of the secondary fd's
        // file table slot at 0/1/2; close the original handle so the
        // child sees only the canonical stdio fds.
        let _ = syscall_lib::close(secondary_fd);
        // Production env. Mirrors the `login` baseline so an
        // interactive shell sees a familiar PATH / TERM / EDITOR /
        // HOME triple. `term` does not yet thread per-user
        // settings; HOME is hard-coded to `/root` because Phase 57
        // term inherits init's uid (root) — the graphical-login
        // story is a future-phase concern.
        let env_path: &[u8] = b"PATH=/bin:/sbin:/usr/bin\0";
        let env_term: &[u8] = b"TERM=m3os\0";
        let env_editor: &[u8] = b"EDITOR=/bin/edit\0";
        let env_home: &[u8] = b"HOME=/root\0";
        let envp: [*const u8; 5] = [
            env_path.as_ptr(),
            env_term.as_ptr(),
            env_editor.as_ptr(),
            env_home.as_ptr(),
            core::ptr::null(),
        ];
        // Try ion (the production default). argv is just the program
        // name — Phase 57 does not yet thread through user-supplied
        // argv.
        let argv_ion: [*const u8; 2] = [SHELL_PATH_ION.as_ptr(), core::ptr::null()];
        let _rc = syscall_lib::execve(SHELL_PATH_ION, &argv_ion, &envp);
        // execve only returns on failure. Fall back to sh0, mirroring
        // `login`'s recovery shape.
        syscall_lib::write_str(
            STDOUT_FILENO,
            "term: execve(/bin/ion) failed; falling back to /bin/sh0\n",
        );
        let argv_sh0: [*const u8; 2] = [SHELL_PATH_SH0.as_ptr(), core::ptr::null()];
        let _rc = syscall_lib::execve(SHELL_PATH_SH0, &argv_sh0, &envp);
        syscall_lib::write_str(STDOUT_FILENO, "term: execve(/bin/sh0) failed\n");
        syscall_lib::exit(CHILD_EXIT_EXECVE)
    }

    fn close(&mut self, fd: i32) -> i32 {
        syscall_lib::close(fd) as i32
    }

    fn try_wait(&mut self, pid: i32) -> Result<Option<i32>, i32> {
        let mut status: i32 = 0;
        let rc = syscall_lib::waitpid(pid, &mut status, WNOHANG);
        decode_wait_status(rc, status)
    }
}
