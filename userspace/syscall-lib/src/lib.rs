//! Syscall wrappers for m3OS userspace programs.
//!
//! Syscall ABI (see kernel/src/arch/x86_64/syscall.rs):
//!   rax = number
//!   rdi, rsi, rdx, r10, r8, r9 = args 0-5
//!   return value in rax
//!   rcx and r11 are clobbered by syscall instruction
#![no_std]

use core::arch::asm;

#[cfg(feature = "alloc")]
pub mod heap;

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
pub const SYS_RT_SIGACTION: u64 = 13;
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
pub const SYS_MOUNT: u64 = 165;

/// Custom kernel debug-print syscall.
pub const SYS_DEBUG_PRINT: u64 = 0x1000;

// ===========================================================================
// Socket syscall numbers (Phase 23)
// ===========================================================================

pub const SYS_SOCKET: u64 = 41;
pub const SYS_CONNECT: u64 = 42;
pub const SYS_ACCEPT: u64 = 43;
pub const SYS_SENDTO: u64 = 44;
pub const SYS_RECVFROM: u64 = 45;
pub const SYS_SHUTDOWN: u64 = 48;
pub const SYS_BIND: u64 = 49;
pub const SYS_LISTEN: u64 = 50;
pub const SYS_GETSOCKNAME: u64 = 51;
pub const SYS_GETPEERNAME: u64 = 52;
pub const SYS_SETSOCKOPT: u64 = 54;
pub const SYS_GETSOCKOPT: u64 = 55;
pub const SYS_CLOCK_GETTIME: u64 = 228;

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
// Socket constants (Phase 23)
// ===========================================================================

pub const AF_INET: u64 = 2;
pub const SOCK_STREAM: u64 = 1;
pub const SOCK_DGRAM: u64 = 2;

pub const IPPROTO_TCP: u64 = 6;
pub const IPPROTO_UDP: u64 = 17;
pub const IPPROTO_ICMP: u64 = 1;

// Socket options
pub const SOL_SOCKET: u64 = 1;
pub const SO_REUSEADDR: u64 = 2;
pub const SO_KEEPALIVE: u64 = 9;
pub const SO_RCVBUF: u64 = 8;
pub const SO_SNDBUF: u64 = 7;
pub const TCP_NODELAY: u64 = 1;

// Shutdown modes
// Clock IDs
pub const CLOCK_MONOTONIC: u64 = 1;

pub const SHUT_RD: i32 = 0;
pub const SHUT_WR: i32 = 1;
pub const SHUT_RDWR: i32 = 2;

// Poll events
pub const POLLIN: i16 = 0x001;
pub const POLLOUT: i16 = 0x004;
pub const POLLERR: i16 = 0x008;
pub const POLLHUP: i16 = 0x010;

/// IPv4 socket address, matching Linux `struct sockaddr_in` layout.
/// `sin_port` is stored in network byte order (big-endian).
/// `sin_addr` is stored so that in-memory bytes match the IP octets
/// (e.g., 10.0.2.15 → bytes [10, 0, 2, 15] at the field offset).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SockaddrIn {
    pub sin_family: u16,
    /// Port in **network byte order** (big-endian).
    pub sin_port: u16,
    /// IPv4 address — in-memory bytes are the IP octets in network order.
    pub sin_addr: u32,
    pub sin_zero: [u8; 8],
}

impl SockaddrIn {
    /// Create a new `SockaddrIn` for the given IP and port.
    /// `ip` is in host byte order (e.g., [10, 0, 2, 2]).
    /// `port` is in host byte order.
    pub fn new(ip: [u8; 4], port: u16) -> Self {
        Self {
            sin_family: AF_INET as u16,
            sin_port: port.to_be(),
            sin_addr: u32::from_ne_bytes(ip),
            sin_zero: [0; 8],
        }
    }

    /// Return the port in host byte order.
    pub fn port(&self) -> u16 {
        u16::from_be(self.sin_port)
    }

    /// Return the IP address as a 4-byte array in host order.
    pub fn ip(&self) -> [u8; 4] {
        self.sin_addr.to_ne_bytes()
    }
}

// ===========================================================================
// Wait flags
// ===========================================================================

pub const WNOHANG: i32 = 1;

// ===========================================================================
// Lseek whence constants
// ===========================================================================

pub const SEEK_SET: usize = 0;
pub const SEEK_CUR: usize = 1;
pub const SEEK_END: usize = 2;

// ===========================================================================
// Signal numbers
// ===========================================================================

pub const SIGINT: i32 = 2;
pub const SIGCHLD: i32 = 17;
pub const SIGCONT: i32 = 18;
pub const SIGTSTP: i32 = 20;
pub const SIGWINCH: i32 = 28;

// ===========================================================================
// Signal action constants
// ===========================================================================

pub const SA_RESTORER: u64 = 0x0400_0000;

// ===========================================================================
// Ioctl request numbers
// ===========================================================================

pub const TCGETS: usize = 0x5401;
pub const TCSETS: usize = 0x5402;
pub const TCSETSW: usize = 0x5403;
pub const TCSETSF: usize = 0x5404;
pub const TIOCGWINSZ: usize = 0x5413;
pub const TIOCSWINSZ: usize = 0x5414;

// ===========================================================================
// Termios types and constants (matching kernel-core layout)
// ===========================================================================

pub const NCCS: usize = 19;

// c_lflag constants
pub const ISIG: u32 = 0o000001;
pub const ICANON: u32 = 0o000002;
pub const ECHO: u32 = 0o000010;
pub const ECHOE: u32 = 0o000020;
pub const IEXTEN: u32 = 0o100000;

// c_iflag constants
pub const ICRNL: u32 = 0o000400;
pub const IXON: u32 = 0o002000;
pub const BRKINT: u32 = 0o000002;
pub const INPCK: u32 = 0o000020;
pub const ISTRIP: u32 = 0o000040;

// c_oflag constants
pub const OPOST: u32 = 0o000001;

// c_cflag constants
pub const CS8: u32 = 0o000060;

/// Terminal I/O settings, binary-compatible with Linux `struct termios`.
/// Size: 36 bytes (matching kernel ioctl copy size).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Termios {
    pub c_iflag: u32,
    pub c_oflag: u32,
    pub c_cflag: u32,
    pub c_lflag: u32,
    pub c_line: u8,
    pub c_cc: [u8; NCCS],
}

/// Terminal window size, binary-compatible with Linux `struct winsize`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Winsize {
    pub ws_row: u16,
    pub ws_col: u16,
    pub ws_xpixel: u16,
    pub ws_ypixel: u16,
}

/// Signal action struct for rt_sigaction, matching Linux layout.
/// Layout: sa_handler(8) + sa_flags(8) + sa_restorer(8) + sa_mask(8) = 32 bytes.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SigAction {
    pub sa_handler: u64,
    pub sa_flags: u64,
    pub sa_restorer: u64,
    pub sa_mask: u64,
}

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
// High-level wrappers — ioctl, lseek, termios, signals
// ===========================================================================

/// Perform an ioctl operation on a file descriptor.
pub fn ioctl(fd: usize, request: usize, arg: usize) -> isize {
    unsafe { syscall3(SYS_IOCTL, fd as u64, request as u64, arg as u64) as isize }
}

/// Seek to a position in a file descriptor.
pub fn lseek(fd: usize, offset: i64, whence: usize) -> isize {
    unsafe { syscall3(SYS_LSEEK, fd as u64, offset as u64, whence as u64) as isize }
}

/// Get terminal attributes.
pub fn tcgetattr(fd: usize) -> Result<Termios, isize> {
    let mut t = Termios {
        c_iflag: 0,
        c_oflag: 0,
        c_cflag: 0,
        c_lflag: 0,
        c_line: 0,
        c_cc: [0; NCCS],
    };
    let ret = ioctl(fd, TCGETS, &mut t as *mut Termios as usize);
    if ret < 0 {
        Err(ret)
    } else {
        Ok(t)
    }
}

/// Set terminal attributes.
pub fn tcsetattr(fd: usize, termios: &Termios) -> Result<(), isize> {
    let ret = ioctl(fd, TCSETS, termios as *const Termios as usize);
    if ret < 0 {
        Err(ret)
    } else {
        Ok(())
    }
}

/// Get terminal window size (rows, cols).
pub fn get_window_size(fd: usize) -> Result<(u16, u16), isize> {
    let mut ws = Winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let ret = ioctl(fd, TIOCGWINSZ, &mut ws as *mut Winsize as usize);
    if ret < 0 {
        Err(ret)
    } else {
        Ok((ws.ws_row, ws.ws_col))
    }
}

/// Install a signal handler via rt_sigaction (syscall 13).
pub fn rt_sigaction(signum: usize, act: *const SigAction, oldact: *mut SigAction) -> isize {
    unsafe { syscall3(SYS_RT_SIGACTION, signum as u64, act as u64, oldact as u64) as isize }
}

/// brk syscall — set or query the program break.
pub fn brk(addr: u64) -> u64 {
    unsafe { syscall1(SYS_BRK, addr) }
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
    let ts: [i64; 2] = [seconds as i64, 0];
    unsafe { syscall2(SYS_NANOSLEEP, ts.as_ptr() as u64, 0) as isize }
}

// ===========================================================================
// High-level wrappers — Sockets (Phase 23)
// ===========================================================================

// ===========================================================================
// Phase 24: mount syscall
// ===========================================================================

/// Mount a filesystem. Returns 0 on success, negative errno on error.
pub fn mount(source: *const u8, target: *const u8, fstype: *const u8) -> isize {
    unsafe { syscall3(SYS_MOUNT, source as u64, target as u64, fstype as u64) as isize }
}

/// Create a socket. Returns fd on success, negative errno on error.
pub fn socket(domain: i32, socktype: i32, protocol: i32) -> isize {
    unsafe { syscall3(SYS_SOCKET, domain as u64, socktype as u64, protocol as u64) as isize }
}

/// Bind a socket to an address.
pub fn bind(fd: i32, addr: &SockaddrIn) -> isize {
    unsafe {
        syscall3(
            SYS_BIND,
            fd as u64,
            addr as *const SockaddrIn as u64,
            core::mem::size_of::<SockaddrIn>() as u64,
        ) as isize
    }
}

/// Connect a socket to a remote address.
pub fn connect(fd: i32, addr: &SockaddrIn) -> isize {
    unsafe {
        syscall3(
            SYS_CONNECT,
            fd as u64,
            addr as *const SockaddrIn as u64,
            core::mem::size_of::<SockaddrIn>() as u64,
        ) as isize
    }
}

/// Listen for incoming connections on a socket.
pub fn listen(fd: i32, backlog: i32) -> isize {
    unsafe { syscall2(SYS_LISTEN, fd as u64, backlog as u64) as isize }
}

/// Accept an incoming connection. Returns new fd on success.
pub fn accept(fd: i32, addr: Option<&mut SockaddrIn>) -> isize {
    let mut len: u32 = core::mem::size_of::<SockaddrIn>() as u32;
    let (addr_ptr, len_ptr) = match addr {
        Some(a) => (a as *mut SockaddrIn as u64, &mut len as *mut u32 as u64),
        None => (0u64, 0u64),
    };
    unsafe { syscall3(SYS_ACCEPT, fd as u64, addr_ptr, len_ptr) as isize }
}

/// Send data on a connected socket.
pub fn send(fd: i32, buf: &[u8], flags: i32) -> isize {
    unsafe {
        syscall6(
            SYS_SENDTO,
            fd as u64,
            buf.as_ptr() as u64,
            buf.len() as u64,
            flags as u64,
            0,
            0,
        ) as isize
    }
}

/// Send data to a specific address.
pub fn sendto(fd: i32, buf: &[u8], flags: i32, addr: &SockaddrIn) -> isize {
    unsafe {
        syscall6(
            SYS_SENDTO,
            fd as u64,
            buf.as_ptr() as u64,
            buf.len() as u64,
            flags as u64,
            addr as *const SockaddrIn as u64,
            core::mem::size_of::<SockaddrIn>() as u64,
        ) as isize
    }
}

/// Receive data from a connected socket.
pub fn recv(fd: i32, buf: &mut [u8], flags: i32) -> isize {
    unsafe {
        syscall6(
            SYS_RECVFROM,
            fd as u64,
            buf.as_mut_ptr() as u64,
            buf.len() as u64,
            flags as u64,
            0,
            0,
        ) as isize
    }
}

/// Receive data and sender address.
pub fn recvfrom(fd: i32, buf: &mut [u8], flags: i32, addr: &mut SockaddrIn) -> isize {
    let mut len: u32 = core::mem::size_of::<SockaddrIn>() as u32;
    unsafe {
        syscall6(
            SYS_RECVFROM,
            fd as u64,
            buf.as_mut_ptr() as u64,
            buf.len() as u64,
            flags as u64,
            addr as *mut SockaddrIn as u64,
            &mut len as *mut u32 as u64,
        ) as isize
    }
}

/// Shut down part of a full-duplex connection.
pub fn shutdown(fd: i32, how: i32) -> isize {
    unsafe { syscall2(SYS_SHUTDOWN, fd as u64, how as u64) as isize }
}

/// Get the local address of a socket.
pub fn getsockname(fd: i32, addr: &mut SockaddrIn) -> isize {
    let mut len: u32 = core::mem::size_of::<SockaddrIn>() as u32;
    unsafe {
        syscall3(
            SYS_GETSOCKNAME,
            fd as u64,
            addr as *mut SockaddrIn as u64,
            &mut len as *mut u32 as u64,
        ) as isize
    }
}

/// Get the remote address of a connected socket.
pub fn getpeername(fd: i32, addr: &mut SockaddrIn) -> isize {
    let mut len: u32 = core::mem::size_of::<SockaddrIn>() as u32;
    unsafe {
        syscall3(
            SYS_GETPEERNAME,
            fd as u64,
            addr as *mut SockaddrIn as u64,
            &mut len as *mut u32 as u64,
        ) as isize
    }
}

/// Set a socket option.
pub fn setsockopt(fd: i32, level: i32, optname: i32, optval: &[u8]) -> isize {
    unsafe {
        syscall5(
            SYS_SETSOCKOPT,
            fd as u64,
            level as u64,
            optname as u64,
            optval.as_ptr() as u64,
            optval.len() as u64,
        ) as isize
    }
}

/// Get a socket option.
pub fn getsockopt(fd: i32, level: i32, optname: i32, optval: &mut [u8]) -> isize {
    let mut len: u32 = optval.len() as u32;
    unsafe {
        syscall5(
            SYS_GETSOCKOPT,
            fd as u64,
            level as u64,
            optname as u64,
            optval.as_mut_ptr() as u64,
            &mut len as *mut u32 as u64,
        ) as isize
    }
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
