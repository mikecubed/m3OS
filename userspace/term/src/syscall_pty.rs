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
use syscall_lib::{O_RDONLY, STDOUT_FILENO, WNOHANG};

/// Phase 72b — fill `home_buf` with `HOME=<path>\0` and `user_buf`
/// with `USER=<name>\0` derived from `/etc/passwd` for the current
/// process's `getuid()`. Returns the two `*const u8` pointers the
/// `execve` envp array needs.
///
/// On any failure (couldn't open `/etc/passwd`, UID not found, oversize
/// fields), falls back to `HOME=/root` and `USER=root` so the shell
/// still launches with a usable envp.
///
/// Pure helper: no allocations, no panics. The buffers are caller-owned
/// (live on the caller's stack frame) so the returned pointers stay
/// valid for the lifetime of the `execve` call.
fn build_user_env_from_passwd(home_buf: &mut [u8], user_buf: &mut [u8]) -> (*const u8, *const u8) {
    fn write_prefixed(prefix: &[u8], value: &[u8], out: &mut [u8]) -> bool {
        // Need prefix + value + NUL bytes. Truncating would yield an
        // unterminated string — fail to the caller's fallback.
        if prefix.len() + value.len() + 1 > out.len() {
            return false;
        }
        out[..prefix.len()].copy_from_slice(prefix);
        out[prefix.len()..prefix.len() + value.len()].copy_from_slice(value);
        out[prefix.len() + value.len()] = 0;
        true
    }

    let uid_u32 = syscall_lib::getuid();
    let mut read_buf = [0u8; 4096];
    let n = read_passwd(&mut read_buf);
    let mut user_set = false;
    let mut home_set = false;
    if n > 0 {
        let bytes = &read_buf[..n];
        if let Some(username) = passwd::find_username_by_uid(bytes, uid_u32) {
            user_set = write_prefixed(b"USER=", username, user_buf);
            if let Some(home) = find_home_by_uid(bytes, uid_u32) {
                home_set = write_prefixed(b"HOME=", home, home_buf);
            }
        }
    }
    if !user_set {
        // Couldn't resolve — fall back to root identity rather than
        // launching the shell with no USER (some prompts blank, others
        // print "0"). Truncation here would already mean the static
        // strings are too long for the buffer; that's a caller bug.
        let _ = write_prefixed(b"USER=", b"root", user_buf);
    }
    if !home_set {
        let _ = write_prefixed(b"HOME=", b"/root", home_buf);
    }
    (home_buf.as_ptr(), user_buf.as_ptr())
}

/// Read `/etc/passwd` into `out`. Returns the byte count read, or 0
/// on any failure. Best-effort — used only to look up the current
/// process's name + home; failure falls back to root.
fn read_passwd(out: &mut [u8]) -> usize {
    let fd = syscall_lib::open(b"/etc/passwd\0", O_RDONLY, 0);
    if fd < 0 {
        return 0;
    }
    let fd_i32 = fd as i32;
    let mut total = 0usize;
    let mut chunk = [0u8; 1024];
    loop {
        let n = syscall_lib::read(fd_i32, &mut chunk);
        if n <= 0 {
            break;
        }
        let n_usize = n as usize;
        let copy = n_usize.min(out.len().saturating_sub(total));
        out[total..total + copy].copy_from_slice(&chunk[..copy]);
        total += copy;
        if total == out.len() {
            break;
        }
    }
    let _ = syscall_lib::close(fd_i32);
    total
}

/// Walk `/etc/passwd` looking for the row matching `target_uid` and
/// return its `home` field (field index 5). Mirrors
/// `passwd::find_username_by_uid` but returns the home column —
/// `passwd-lib` currently only exposes the name lookup.
fn find_home_by_uid(passwd: &[u8], target_uid: u32) -> Option<&[u8]> {
    for line in passwd.split(|&b| b == b'\n') {
        if line.is_empty() {
            continue;
        }
        let mut fields = [&[] as &[u8]; 7];
        let mut start = 0;
        let mut field = 0;
        for (i, &b) in line.iter().enumerate() {
            if b == b':' && field < 7 {
                fields[field] = &line[start..i];
                field += 1;
                start = i + 1;
            }
        }
        if field == 6 {
            fields[6] = &line[start..];
            let uid_bytes = fields[2];
            let mut uid: u32 = 0;
            let mut ok = !uid_bytes.is_empty();
            for &b in uid_bytes {
                if !b.is_ascii_digit() {
                    ok = false;
                    break;
                }
                uid = uid.wrapping_mul(10).wrapping_add((b - b'0') as u32);
            }
            if ok && uid == target_uid {
                return Some(fields[5]);
            }
        }
    }
    None
}

/// Production default shell. Matches the `/etc/passwd` `:/bin/ion`
/// entries and the path `login` exec's after authenticating. Spelled
/// as a null-terminated byte string so it can travel through
/// `execve` without any per-call allocation.
const SHELL_PATH_ION: &[u8] = b"/bin/ion\0";
const SHELL_ARG_INTERACTIVE: &[u8] = b"-i\0";

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

fn ion_argv() -> [*const u8; 3] {
    [
        SHELL_PATH_ION.as_ptr(),
        SHELL_ARG_INTERACTIVE.as_ptr(),
        core::ptr::null(),
    ]
}

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
        // Phase 72b — look up the current process's username and home
        // from `/etc/passwd` so the shell prompt shows `user@m3os`
        // (and `whoami` / `$USER` resolve correctly) instead of the
        // raw UID. Pre-72b hardcoded `HOME=/root` and skipped `USER=`
        // entirely; with the K.1 redesign letting term run under the
        // authenticated user's UID, the inherited envp is no longer
        // a safe assumption either — we need self-sufficient lookup.
        // Falls back to root / empty username on any read failure so
        // a missing `/etc/passwd` doesn't lose us a shell.
        let env_path: &[u8] = b"PATH=/usr/local/bin:/bin:/sbin:/usr/bin\0";
        let env_term: &[u8] = b"TERM=m3os-term\0";
        let env_editor: &[u8] = b"EDITOR=/bin/edit\0";
        let mut env_home_buf = [0u8; 128];
        let mut env_user_buf = [0u8; 64];
        let (env_home_ptr, env_user_ptr) =
            build_user_env_from_passwd(&mut env_home_buf, &mut env_user_buf);
        let envp: [*const u8; 6] = [
            env_path.as_ptr(),
            env_term.as_ptr(),
            env_editor.as_ptr(),
            env_home_ptr,
            env_user_ptr,
            core::ptr::null(),
        ];
        // Try ion (the production default). Force interactive mode because
        // m3OS PTY detection can race early boot; term always wants a prompt.
        let argv_ion = ion_argv();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ion_argv_forces_interactive_mode() {
        let argv = ion_argv();
        assert_eq!(argv[0], SHELL_PATH_ION.as_ptr());
        assert_eq!(argv[1], SHELL_ARG_INTERACTIVE.as_ptr());
        assert!(argv[2].is_null());
    }
}
