//! Syscall wrappers for m3OS userspace programs.
//!
//! Syscall ABI (see kernel/src/arch/x86_64/syscall.rs):
//!   rax = number
//!   rdi, rsi, rdx, r10, r8, r9 = args 0-5
//!   return value in rax
//!   rcx and r11 are clobbered by syscall instruction
#![no_std]

use core::arch::asm;

// ===========================================================================
// Raw syscall wrappers
// ===========================================================================

/// Raw zero-argument syscall.
#[inline(always)]
pub unsafe fn syscall0(num: u64) -> u64 {
    let mut rax = num;
    asm!(
        "syscall",
        inlateout("rax") rax,
        lateout("rcx") _,
        lateout("r11") _,
        options(nostack),
    );
    rax
}

/// Raw one-argument syscall.
#[inline(always)]
pub unsafe fn syscall1(num: u64, a0: u64) -> u64 {
    let mut rax = num;
    asm!(
        "syscall",
        inlateout("rax") rax,
        in("rdi") a0,
        lateout("rcx") _,
        lateout("r11") _,
        options(nostack),
    );
    rax
}

/// Raw two-argument syscall.
#[inline(always)]
pub unsafe fn syscall2(num: u64, a0: u64, a1: u64) -> u64 {
    let mut rax = num;
    asm!(
        "syscall",
        inlateout("rax") rax,
        in("rdi") a0,
        in("rsi") a1,
        lateout("rcx") _,
        lateout("r11") _,
        options(nostack),
    );
    rax
}

/// Raw three-argument syscall.
#[inline(always)]
pub unsafe fn syscall3(num: u64, a0: u64, a1: u64, a2: u64) -> u64 {
    let mut rax = num;
    asm!(
        "syscall",
        inlateout("rax") rax,
        in("rdi") a0,
        in("rsi") a1,
        in("rdx") a2,
        lateout("rcx") _,
        lateout("r11") _,
        options(nostack),
    );
    rax
}

/// Raw four-argument syscall. Note: arg4 uses r10 (not rcx, which is clobbered).
#[inline(always)]
pub unsafe fn syscall4(num: u64, a0: u64, a1: u64, a2: u64, a3: u64) -> u64 {
    let mut rax = num;
    asm!(
        "syscall",
        inlateout("rax") rax,
        in("rdi") a0,
        in("rsi") a1,
        in("rdx") a2,
        in("r10") a3,
        lateout("rcx") _,
        lateout("r11") _,
        options(nostack),
    );
    rax
}

/// Raw five-argument syscall.
#[inline(always)]
pub unsafe fn syscall5(num: u64, a0: u64, a1: u64, a2: u64, a3: u64, a4: u64) -> u64 {
    let mut rax = num;
    asm!(
        "syscall",
        inlateout("rax") rax,
        in("rdi") a0,
        in("rsi") a1,
        in("rdx") a2,
        in("r10") a3,
        in("r8") a4,
        lateout("rcx") _,
        lateout("r11") _,
        options(nostack),
    );
    rax
}

/// Raw six-argument syscall.
#[inline(always)]
pub unsafe fn syscall6(num: u64, a0: u64, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64) -> u64 {
    let mut rax = num;
    asm!(
        "syscall",
        inlateout("rax") rax,
        in("rdi") a0,
        in("rsi") a1,
        in("rdx") a2,
        in("r10") a3,
        in("r8") a4,
        in("r9") a5,
        lateout("rcx") _,
        lateout("r11") _,
        options(nostack),
    );
    rax
}

// ===========================================================================
// Syscall numbers
// ===========================================================================

pub const SYS_READ: u64 = 0;
pub const SYS_WRITE: u64 = 1;
pub const SYS_OPEN: u64 = 2;
pub const SYS_CLOSE: u64 = 3;
pub const SYS_FSTAT: u64 = 5;
pub const SYS_LSEEK: u64 = 8;
pub const SYS_MMAP: u64 = 9;
pub const SYS_BRK: u64 = 12;
pub const SYS_IOCTL: u64 = 16;
pub const SYS_PIPE: u64 = 22;
pub const SYS_DUP2: u64 = 33;
pub const SYS_NANOSLEEP: u64 = 35;
pub const SYS_FORK: u64 = 57;
pub const SYS_EXECVE: u64 = 59;
pub const SYS_EXIT: u64 = 60;
pub const SYS_WAITPID: u64 = 61;
pub const SYS_KILL: u64 = 62;
pub const SYS_GETCWD: u64 = 79;
pub const SYS_CHDIR: u64 = 80;
pub const SYS_MKDIR: u64 = 83;
pub const SYS_GETPID: u64 = 39;
pub const SYS_GETPPID: u64 = 110;
pub const SYS_SETPGID: u64 = 109;
pub const SYS_GETPGID: u64 = 121;

/// Custom kernel debug-print syscall.
pub const SYS_DEBUG_PRINT: u64 = 0x1000;

// ===========================================================================
// File flags and constants
// ===========================================================================

pub const O_RDONLY: u64 = 0;
pub const O_WRONLY: u64 = 1;
pub const O_RDWR: u64 = 2;
pub const O_CREAT: u64 = 0x40;
pub const O_TRUNC: u64 = 0x200;
pub const O_APPEND: u64 = 0x400;

pub const STDIN_FILENO: i32 = 0;
pub const STDOUT_FILENO: i32 = 1;
pub const STDERR_FILENO: i32 = 2;

// ===========================================================================
// Wait flags
// ===========================================================================

pub const WNOHANG: i32 = 1;

// ===========================================================================
// Signal numbers
// ===========================================================================

pub const SIGINT: i32 = 2;
pub const SIGCHLD: i32 = 17;
pub const SIGCONT: i32 = 18;
pub const SIGTSTP: i32 = 20;

// ===========================================================================
// High-level wrappers — File I/O
// ===========================================================================

/// Read up to `buf.len()` bytes from file descriptor `fd`.
pub fn read(fd: i32, buf: &mut [u8]) -> isize {
    unsafe {
        syscall3(
            SYS_READ,
            fd as u64,
            buf.as_mut_ptr() as u64,
            buf.len() as u64,
        ) as isize
    }
}

/// Write `buf` to file descriptor `fd`.
pub fn write(fd: i32, buf: &[u8]) -> isize {
    unsafe { syscall3(SYS_WRITE, fd as u64, buf.as_ptr() as u64, buf.len() as u64) as isize }
}

/// Open a file. `path` must be a null-terminated byte string.
pub fn open(path: &[u8], flags: u64, mode: u64) -> isize {
    unsafe { syscall3(SYS_OPEN, path.as_ptr() as u64, flags, mode) as isize }
}

/// Close a file descriptor.
pub fn close(fd: i32) -> isize {
    unsafe { syscall1(SYS_CLOSE, fd as u64) as isize }
}

// ===========================================================================
// High-level wrappers — Process lifecycle
// ===========================================================================

/// Fork the current process. Returns child PID in parent, 0 in child.
pub fn fork() -> isize {
    unsafe { syscall0(SYS_FORK) as isize }
}

/// Execute a program. `path`, `argv` entries, and `envp` entries must be null-terminated.
/// `argv` and `envp` arrays must be null-pointer terminated.
pub fn execve(path: &[u8], argv: &[*const u8], envp: &[*const u8]) -> isize {
    unsafe {
        syscall3(
            SYS_EXECVE,
            path.as_ptr() as u64,
            argv.as_ptr() as u64,
            envp.as_ptr() as u64,
        ) as isize
    }
}

/// Wait for a child process. Returns the PID of the child that changed state.
pub fn waitpid(pid: i32, status: &mut i32, options: i32) -> isize {
    unsafe {
        syscall3(
            SYS_WAITPID,
            pid as u64,
            status as *mut i32 as u64,
            options as u64,
        ) as isize
    }
}

/// Get the current process ID.
pub fn getpid() -> isize {
    unsafe { syscall0(SYS_GETPID) as isize }
}

/// Get the parent process ID.
pub fn getppid() -> isize {
    unsafe { syscall0(SYS_GETPPID) as isize }
}

/// Terminate the current process with the given exit code.
pub fn exit(code: i32) -> ! {
    unsafe {
        syscall1(SYS_EXIT, code as u64);
    }
    loop {}
}

// ===========================================================================
// High-level wrappers — Pipes and redirection
// ===========================================================================

/// Create a pipe. On success, `fds[0]` is the read end and `fds[1]` is the write end.
pub fn pipe(fds: &mut [i32; 2]) -> isize {
    unsafe { syscall1(SYS_PIPE, fds.as_mut_ptr() as u64) as isize }
}

/// Duplicate `oldfd` onto `newfd`, closing `newfd` first if open.
pub fn dup2(oldfd: i32, newfd: i32) -> isize {
    unsafe { syscall2(SYS_DUP2, oldfd as u64, newfd as u64) as isize }
}

// ===========================================================================
// High-level wrappers — Directory and path
// ===========================================================================

/// Change working directory. `path` must be null-terminated.
pub fn chdir(path: &[u8]) -> isize {
    unsafe { syscall1(SYS_CHDIR, path.as_ptr() as u64) as isize }
}

/// Get current working directory into `buf`. Returns bytes written on success.
pub fn getcwd(buf: &mut [u8]) -> isize {
    unsafe { syscall2(SYS_GETCWD, buf.as_mut_ptr() as u64, buf.len() as u64) as isize }
}

// ===========================================================================
// High-level wrappers — Signals and process control
// ===========================================================================

/// Send a signal to a process.
pub fn kill(pid: i32, sig: i32) -> isize {
    unsafe { syscall2(SYS_KILL, pid as u64, sig as u64) as isize }
}

/// Set the process group ID of process `pid` to `pgid`.
pub fn setpgid(pid: i32, pgid: i32) -> isize {
    unsafe { syscall2(SYS_SETPGID, pid as u64, pgid as u64) as isize }
}

/// Sleep for `seconds` seconds.
pub fn nanosleep(seconds: u64) -> isize {
    // The kernel's nanosleep reads a timespec struct from a user pointer:
    //   bytes 0..8: tv_sec (i64)
    //   bytes 8..16: tv_nsec (i64)
    let ts: [u64; 2] = [seconds, 0];
    unsafe { syscall2(SYS_NANOSLEEP, ts.as_ptr() as u64, 0) as isize }
}

// ===========================================================================
// Convenience helpers
// ===========================================================================

/// Write a string to the kernel serial log (debug channel).
pub fn serial_print(s: &str) {
    unsafe {
        syscall2(SYS_DEBUG_PRINT, s.as_ptr() as u64, s.len() as u64);
    }
}

/// Write a string slice to a file descriptor.
pub fn write_str(fd: i32, s: &str) -> isize {
    write(fd, s.as_bytes())
}

/// Write a u64 as decimal text to a file descriptor (no alloc needed).
pub fn write_u64(fd: i32, mut n: u64) {
    if n == 0 {
        let _ = write(fd, b"0");
        return;
    }
    let mut buf = [0u8; 20]; // max digits for u64
    let mut pos = buf.len();
    while n > 0 {
        pos -= 1;
        buf[pos] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    let _ = write(fd, &buf[pos..]);
}
