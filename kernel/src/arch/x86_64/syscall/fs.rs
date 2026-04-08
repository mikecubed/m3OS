//! File I/O and filesystem syscall handlers.

/// Handle filesystem-related syscalls.
///
/// Returns `Some(result)` if the syscall number belongs to this subsystem,
/// `None` otherwise.
#[inline(always)]
pub(super) fn handle_fs_syscall(number: u64, arg0: u64, arg1: u64, arg2: u64) -> Option<u64> {
    let result = match number {
        // read
        0 => super::sys_linux_read(arg0, arg1, arg2),
        // write
        1 => super::sys_linux_write(arg0, arg1, arg2),
        // open
        2 => super::sys_linux_open(arg0, arg1, arg2),
        // close
        3 => super::sys_linux_close(arg0),
        // stat (follows symlinks)
        4 => super::sys_linux_fstatat(super::AT_FDCWD, arg0, arg1, 0),
        // fstat
        5 => super::sys_linux_fstat(arg0, arg1),
        // lstat (no follow)
        6 => super::sys_linux_fstatat(super::AT_FDCWD, arg0, arg1, super::AT_SYMLINK_NOFOLLOW),
        // lseek
        8 => super::sys_linux_lseek(arg0, arg1, arg2),
        // readv
        19 => super::sys_linux_readv(arg0, arg1, arg2),
        // writev
        20 => super::sys_linux_writev(arg0, arg1, arg2),
        // access
        21 => super::sys_access(arg0),
        // dup
        32 => super::sys_dup(arg0),
        // dup2
        33 => super::sys_dup2(arg0, arg1),
        // fcntl
        72 => super::sys_fcntl(arg0, arg1, arg2),
        // fsync
        74 => super::sys_linux_fsync(arg0),
        // truncate
        76 => super::sys_linux_truncate(arg0, arg1),
        // ftruncate
        77 => super::sys_linux_ftruncate(arg0, arg1),
        // getcwd
        79 => super::sys_linux_getcwd(arg0, arg1),
        // chdir
        80 => super::sys_linux_chdir(arg0),
        // rename
        82 => super::sys_linux_rename(arg0, arg1),
        // mkdir
        83 => super::sys_linux_mkdir(arg0, arg1),
        // rmdir
        84 => super::sys_linux_rmdir(arg0),
        // link
        86 => super::sys_link(arg0, arg1),
        // unlink
        87 => super::sys_linux_unlink(arg0),
        // symlink
        88 => super::sys_symlink(arg0, arg1),
        // readlink
        89 => super::sys_readlink(arg0, arg1, arg2),
        // chmod
        90 => super::sys_linux_chmod(arg0, arg1),
        // fchmod
        91 => super::sys_linux_fchmod(arg0, arg1),
        // chown
        92 => super::sys_linux_chown(arg0, arg1, arg2),
        // fchown
        93 => super::sys_linux_fchown(arg0, arg1, arg2),
        // statfs
        137 => super::sys_statfs(arg0, arg1),
        // fstatfs
        138 => super::sys_fstatfs(arg0, arg1),
        // mount
        165 => super::sys_linux_mount(arg0, arg1, arg2),
        // umount2
        166 => super::sys_linux_umount2(arg0, arg1),
        // getdents64
        217 => super::sys_linux_getdents64(arg0, arg1, arg2),
        // openat
        257 => super::sys_linux_openat(arg0, arg1, arg2),
        // fstatat
        262 => super::sys_linux_fstatat(arg0, arg1, arg2, super::per_core_syscall_arg3()),
        // linkat
        265 => super::sys_linkat(
            arg0,
            arg1,
            arg2,
            super::per_core_syscall_arg3(),
            crate::smp::per_core().syscall_user_r8,
        ),
        // symlinkat
        266 => super::sys_symlinkat(arg0, arg1, arg2),
        // readlinkat
        267 => super::sys_readlinkat(arg0, arg1, arg2, super::per_core_syscall_arg3()),
        // utimensat
        280 => {
            let flags = super::per_core_syscall_arg3();
            super::sys_utimensat(arg0, arg1, arg2, flags)
        }
        // dup3
        292 => super::sys_dup2(arg0, arg1),
        _ => return None,
    };
    Some(result)
}
