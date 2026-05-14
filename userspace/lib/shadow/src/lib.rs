//! `shadow` — atomic shadow-file write helper shared between `passwd` and
//! `adduser` (Phase 66 Track B).
//!
//! Production callers go through [`shadow_write_atomic`]. The implementation
//! is generic over a [`ShadowFs`] trait so host tests can drive the full
//! state machine against a fake filesystem.
//!
//! ## Crash semantics
//!
//! On any failure between `open` and `rename`, the helper unlinks the
//! temporary path so a torn write never overwrites the live shadow file.
//! The original file is only replaced once the temp file is fully written
//! and `fsync`'d.
#![no_std]

/// Maximum length of either the destination path or the temp path
/// (including the trailing NUL byte syscall_lib requires).
const MAX_PATH: usize = 256;

const TEMP_SUFFIX: &[u8] = b".new";

/// Errors returned by [`shadow_write_atomic`]. The numeric variants carry
/// the raw syscall error code so callers can log a precise reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShadowError {
    /// `path` + `.new` + trailing NUL exceeds `MAX_PATH`.
    PathTooLong,
    /// `open(path.new, O_WRONLY | O_CREAT | O_TRUNC | O_CLOEXEC)` failed.
    OpenFailed(isize),
    /// `write` returned a negative errno before completing.
    WriteFailed(isize),
    /// `write` returned fewer bytes than the requested content length.
    ShortWrite { wrote: usize, expected: usize },
    /// `fsync` returned a negative errno.
    FsyncFailed(isize),
    /// `rename(path.new, path)` returned a negative errno.
    RenameFailed(isize),
}

/// Filesystem operations used by [`shadow_write_atomic`]. The production
/// backend implements this with syscall_lib; host tests substitute a fake.
///
/// All paths handed to the trait are NUL-terminated, as syscall_lib
/// expects, so implementations can pass them straight through.
pub trait ShadowFs {
    /// Open `path` (NUL-terminated) for writing with `O_WRONLY | O_CREAT |
    /// O_TRUNC | O_CLOEXEC`. Returns a positive fd or a negative errno.
    fn open_write_cloexec(&mut self, path: &[u8]) -> isize;
    /// Write `buf` to `fd`. Returns bytes written (non-negative) or a
    /// negative errno.
    fn write(&mut self, fd: i32, buf: &[u8]) -> isize;
    /// `fsync(fd)`. Returns 0 or a negative errno.
    fn fsync(&mut self, fd: i32) -> isize;
    /// `close(fd)`. Return value is ignored.
    fn close(&mut self, fd: i32);
    /// `rename(from, to)` where both paths are NUL-terminated. Returns 0
    /// or a negative errno.
    fn rename(&mut self, from: &[u8], to: &[u8]) -> isize;
    /// `unlink(path)` where `path` is NUL-terminated. Return value is
    /// ignored — the temp file may not exist on every cleanup path.
    fn unlink(&mut self, path: &[u8]);
}

/// Atomic shadow-file write driven by the supplied filesystem backend.
///
/// `path` must be a plain UTF-8 path (no trailing NUL). The helper builds
/// `{path}\0` and `{path}.new\0` internally.
pub fn shadow_write_atomic_with<F: ShadowFs>(
    fs: &mut F,
    path: &str,
    content: &[u8],
) -> Result<(), ShadowError> {
    let mut final_buf = [0u8; MAX_PATH];
    let mut temp_buf = [0u8; MAX_PATH];
    let final_len = build_final_path(path.as_bytes(), &mut final_buf)?;
    let temp_len = build_temp_path(path.as_bytes(), &mut temp_buf)?;
    let final_path = &final_buf[..final_len];
    let temp_path = &temp_buf[..temp_len];

    let fd = fs.open_write_cloexec(temp_path);
    if fd < 0 {
        return Err(ShadowError::OpenFailed(fd));
    }
    let fd = fd as i32;

    // Drive write loop — accept partial writes but bail on any negative.
    let mut written = 0usize;
    while written < content.len() {
        let n = fs.write(fd, &content[written..]);
        if n < 0 {
            fs.close(fd);
            fs.unlink(temp_path);
            return Err(ShadowError::WriteFailed(n));
        }
        if n == 0 {
            fs.close(fd);
            fs.unlink(temp_path);
            return Err(ShadowError::ShortWrite {
                wrote: written,
                expected: content.len(),
            });
        }
        written += n as usize;
    }

    let synced = fs.fsync(fd);
    if synced < 0 {
        fs.close(fd);
        fs.unlink(temp_path);
        return Err(ShadowError::FsyncFailed(synced));
    }
    fs.close(fd);

    let renamed = fs.rename(temp_path, final_path);
    if renamed < 0 {
        fs.unlink(temp_path);
        return Err(ShadowError::RenameFailed(renamed));
    }
    Ok(())
}

#[cfg(feature = "guest-bin")]
mod syscall_backend {
    use super::{ShadowError, ShadowFs, shadow_write_atomic_with};

    const O_WRONLY_FLAGS: u64 =
        syscall_lib::O_WRONLY | syscall_lib::O_CREAT | syscall_lib::O_TRUNC | O_CLOEXEC;
    /// `O_CLOEXEC` value (0x80000). Not yet exposed by syscall_lib;
    /// inlined here so passwd/adduser get the bit on every open.
    const O_CLOEXEC: u64 = 0o2000000;

    /// Production [`ShadowFs`] backend that calls into syscall_lib.
    pub struct SyscallShadowFs;

    impl ShadowFs for SyscallShadowFs {
        fn open_write_cloexec(&mut self, path: &[u8]) -> isize {
            syscall_lib::open(path, O_WRONLY_FLAGS, 0o600)
        }
        fn write(&mut self, fd: i32, buf: &[u8]) -> isize {
            syscall_lib::write(fd, buf)
        }
        fn fsync(&mut self, fd: i32) -> isize {
            syscall_lib::fsync(fd)
        }
        fn close(&mut self, fd: i32) {
            let _ = syscall_lib::close(fd);
        }
        fn rename(&mut self, from: &[u8], to: &[u8]) -> isize {
            syscall_lib::rename(from, to)
        }
        fn unlink(&mut self, path: &[u8]) {
            let _ = syscall_lib::unlink(path);
        }
    }

    /// Atomic shadow-file write using the production syscall backend.
    pub fn shadow_write_atomic(path: &str, content: &[u8]) -> Result<(), ShadowError> {
        let mut fs = SyscallShadowFs;
        shadow_write_atomic_with(&mut fs, path, content)
    }
}

#[cfg(feature = "guest-bin")]
pub use syscall_backend::{SyscallShadowFs, shadow_write_atomic};

/// Build `{path}\0` into `out` and return the length (including the
/// trailing NUL).
fn build_final_path(path: &[u8], out: &mut [u8; MAX_PATH]) -> Result<usize, ShadowError> {
    let total = path.len() + 1;
    if total > out.len() {
        return Err(ShadowError::PathTooLong);
    }
    out[..path.len()].copy_from_slice(path);
    out[path.len()] = 0;
    Ok(total)
}

/// Build `{path}.new\0` into `out` and return the length (including the
/// trailing NUL).
fn build_temp_path(path: &[u8], out: &mut [u8; MAX_PATH]) -> Result<usize, ShadowError> {
    let total = path.len() + TEMP_SUFFIX.len() + 1;
    if total > out.len() {
        return Err(ShadowError::PathTooLong);
    }
    out[..path.len()].copy_from_slice(path);
    out[path.len()..path.len() + TEMP_SUFFIX.len()].copy_from_slice(TEMP_SUFFIX);
    out[path.len() + TEMP_SUFFIX.len()] = 0;
    Ok(total)
}

#[cfg(any(test, feature = "host-tests"))]
pub mod test_support {
    //! Mock filesystem backend for host-side tests.
    //!
    //! Records every syscall in order so tests can assert the exact
    //! sequence (`open → write → fsync → close → rename`) and inject
    //! failures at any step.
    extern crate alloc;
    use alloc::collections::BTreeMap;
    use alloc::vec::Vec;

    use super::ShadowFs;

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum Op {
        Open(Vec<u8>),
        Write(i32, Vec<u8>),
        Fsync(i32),
        Close(i32),
        Rename(Vec<u8>, Vec<u8>),
        Unlink(Vec<u8>),
    }

    pub struct MockFs {
        pub files: BTreeMap<Vec<u8>, Vec<u8>>,
        pub ops: Vec<Op>,
        pub fail_open: Option<isize>,
        pub fail_write: Option<isize>,
        pub fail_fsync: Option<isize>,
        pub fail_rename: Option<isize>,
        next_fd: i32,
        open_paths: BTreeMap<i32, Vec<u8>>,
    }

    impl MockFs {
        pub fn new() -> Self {
            Self {
                files: BTreeMap::new(),
                ops: Vec::new(),
                fail_open: None,
                fail_write: None,
                fail_fsync: None,
                fail_rename: None,
                next_fd: 3,
                open_paths: BTreeMap::new(),
            }
        }

        pub fn with_file(mut self, path: &[u8], content: &[u8]) -> Self {
            self.files
                .insert(strip_nul(path).to_vec(), content.to_vec());
            self
        }
    }

    impl Default for MockFs {
        fn default() -> Self {
            Self::new()
        }
    }

    impl ShadowFs for MockFs {
        fn open_write_cloexec(&mut self, path: &[u8]) -> isize {
            self.ops.push(Op::Open(path.to_vec()));
            if let Some(err) = self.fail_open {
                return err;
            }
            let fd = self.next_fd;
            self.next_fd += 1;
            self.open_paths.insert(fd, strip_nul(path).to_vec());
            self.files.insert(strip_nul(path).to_vec(), Vec::new());
            fd as isize
        }
        fn write(&mut self, fd: i32, buf: &[u8]) -> isize {
            self.ops.push(Op::Write(fd, buf.to_vec()));
            if let Some(err) = self.fail_write {
                return err;
            }
            if let Some(path) = self.open_paths.get(&fd).cloned() {
                let file = self.files.entry(path).or_default();
                file.extend_from_slice(buf);
            }
            buf.len() as isize
        }
        fn fsync(&mut self, fd: i32) -> isize {
            self.ops.push(Op::Fsync(fd));
            self.fail_fsync.unwrap_or(0)
        }
        fn close(&mut self, fd: i32) {
            self.ops.push(Op::Close(fd));
            self.open_paths.remove(&fd);
        }
        fn rename(&mut self, from: &[u8], to: &[u8]) -> isize {
            self.ops.push(Op::Rename(from.to_vec(), to.to_vec()));
            if let Some(err) = self.fail_rename {
                return err;
            }
            let from_k = strip_nul(from).to_vec();
            let to_k = strip_nul(to).to_vec();
            if let Some(data) = self.files.remove(&from_k) {
                self.files.insert(to_k, data);
            }
            0
        }
        fn unlink(&mut self, path: &[u8]) {
            self.ops.push(Op::Unlink(path.to_vec()));
            self.files.remove(strip_nul(path));
        }
    }

    fn strip_nul(p: &[u8]) -> &[u8] {
        if let Some((&0, rest)) = p.split_last() {
            rest
        } else {
            p
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate alloc;
    use alloc::vec;

    use super::test_support::{MockFs, Op};
    use super::*;

    fn nul(s: &str) -> alloc::vec::Vec<u8> {
        let mut v = s.as_bytes().to_vec();
        v.push(0);
        v
    }

    #[test]
    fn success_path_commits_rename() {
        let mut fs = MockFs::new().with_file(b"/etc/shadow", b"old\n");
        let result = shadow_write_atomic_with(&mut fs, "/etc/shadow", b"new\n");
        assert_eq!(result, Ok(()));
        assert_eq!(
            fs.files.get(&b"/etc/shadow".to_vec()),
            Some(&b"new\n".to_vec())
        );
        assert!(fs.files.get(&b"/etc/shadow.new".to_vec()).is_none());
        // Sequence must be: open → write → fsync → close → rename
        assert_eq!(fs.ops[0], Op::Open(nul("/etc/shadow.new")));
        assert!(matches!(fs.ops[1], Op::Write(_, _)));
        assert!(matches!(fs.ops[2], Op::Fsync(_)));
        assert!(matches!(fs.ops[3], Op::Close(_)));
        assert_eq!(
            fs.ops[4],
            Op::Rename(nul("/etc/shadow.new"), nul("/etc/shadow"))
        );
    }

    #[test]
    fn write_failure_leaves_original_unchanged_and_unlinks_temp() {
        let mut fs = MockFs::new().with_file(b"/etc/shadow", b"old\n");
        fs.fail_write = Some(-5);
        let result = shadow_write_atomic_with(&mut fs, "/etc/shadow", b"new\n");
        assert_eq!(result, Err(ShadowError::WriteFailed(-5)));
        assert_eq!(
            fs.files.get(&b"/etc/shadow".to_vec()),
            Some(&b"old\n".to_vec())
        );
        assert!(fs.files.get(&b"/etc/shadow.new".to_vec()).is_none());
        // Cleanup must include unlink of the temp.
        assert!(
            fs.ops
                .iter()
                .any(|op| matches!(op, Op::Unlink(p) if p == &nul("/etc/shadow.new")))
        );
    }

    #[test]
    fn open_failure_returns_open_error() {
        let mut fs = MockFs::new().with_file(b"/etc/shadow", b"old\n");
        fs.fail_open = Some(-13);
        let result = shadow_write_atomic_with(&mut fs, "/etc/shadow", b"new\n");
        assert_eq!(result, Err(ShadowError::OpenFailed(-13)));
        // Original file intact.
        assert_eq!(
            fs.files.get(&b"/etc/shadow".to_vec()),
            Some(&b"old\n".to_vec())
        );
    }

    #[test]
    fn rename_failure_leaves_original_intact_and_unlinks_temp() {
        let mut fs = MockFs::new().with_file(b"/etc/shadow", b"old\n");
        fs.fail_rename = Some(-30);
        let result = shadow_write_atomic_with(&mut fs, "/etc/shadow", b"new\n");
        assert_eq!(result, Err(ShadowError::RenameFailed(-30)));
        assert_eq!(
            fs.files.get(&b"/etc/shadow".to_vec()),
            Some(&b"old\n".to_vec())
        );
        assert!(fs.files.get(&b"/etc/shadow.new".to_vec()).is_none());
    }

    #[test]
    fn path_too_long_rejected() {
        let mut fs = MockFs::new();
        let long = "x".repeat(MAX_PATH);
        let result = shadow_write_atomic_with(&mut fs, &long, b"data");
        assert_eq!(result, Err(ShadowError::PathTooLong));
        // No syscalls issued.
        assert!(fs.ops.is_empty());
    }

    #[test]
    fn fsync_failure_leaves_original_unchanged() {
        let mut fs = MockFs::new().with_file(b"/etc/shadow", b"old\n");
        fs.fail_fsync = Some(-5);
        let result = shadow_write_atomic_with(&mut fs, "/etc/shadow", b"new\n");
        assert_eq!(result, Err(ShadowError::FsyncFailed(-5)));
        assert_eq!(
            fs.files.get(&b"/etc/shadow".to_vec()),
            Some(&b"old\n".to_vec())
        );
        assert!(fs.files.get(&b"/etc/shadow.new".to_vec()).is_none());
    }
}
