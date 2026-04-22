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

pub mod sha256;

pub mod start;

// ===========================================================================
// Raw syscall wrappers
// ===========================================================================

/// Raw zero-argument syscall.
///
/// # Safety
///
/// Caller must pass a valid syscall number. Side effects depend on the syscall.
#[inline(always)]
pub unsafe fn syscall0(num: u64) -> u64 {
    unsafe {
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
}

/// Raw one-argument syscall.
///
/// # Safety
///
/// Caller must pass a valid syscall number and argument. Pointer arguments must be valid.
#[inline(always)]
pub unsafe fn syscall1(num: u64, a0: u64) -> u64 {
    unsafe {
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
}

/// Raw two-argument syscall.
///
/// # Safety
///
/// Caller must pass a valid syscall number and arguments. Pointer arguments must be valid.
#[inline(always)]
pub unsafe fn syscall2(num: u64, a0: u64, a1: u64) -> u64 {
    unsafe {
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
}

/// Raw three-argument syscall.
///
/// # Safety
///
/// Caller must pass a valid syscall number and arguments. Pointer arguments must be valid.
#[inline(always)]
pub unsafe fn syscall3(num: u64, a0: u64, a1: u64, a2: u64) -> u64 {
    unsafe {
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
}

/// Raw four-argument syscall. Note: arg4 uses r10 (not rcx, which is clobbered).
///
/// # Safety
///
/// Caller must pass a valid syscall number and arguments. Pointer arguments must be valid.
#[inline(always)]
pub unsafe fn syscall4(num: u64, a0: u64, a1: u64, a2: u64, a3: u64) -> u64 {
    unsafe {
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
}

/// Raw five-argument syscall.
///
/// # Safety
///
/// Caller must pass a valid syscall number and arguments. Pointer arguments must be valid.
#[inline(always)]
pub unsafe fn syscall5(num: u64, a0: u64, a1: u64, a2: u64, a3: u64, a4: u64) -> u64 {
    unsafe {
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
}

/// Raw six-argument syscall.
///
/// # Safety
///
/// Caller must pass a valid syscall number and arguments. Pointer arguments must be valid.
#[inline(always)]
pub unsafe fn syscall6(num: u64, a0: u64, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64) -> u64 {
    unsafe {
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
pub const SYS_RENAME: u64 = 82;
pub const SYS_MKDIR: u64 = 83;
pub const SYS_RMDIR: u64 = 84;
pub const SYS_LINK: u64 = 86;
pub const SYS_UNLINK: u64 = 87;
pub const SYS_SYMLINK: u64 = 88;
pub const SYS_READLINK: u64 = 89;
pub const SYS_GETPID: u64 = 39;
pub const SYS_GETPPID: u64 = 110;
pub const SYS_SETPGID: u64 = 109;
pub const SYS_GETPGID: u64 = 121;
pub const SYS_MOUNT: u64 = 165;
pub const SYS_UMOUNT2: u64 = 166;

// Phase 27: User identity and file permission syscalls
pub const SYS_CHMOD: u64 = 90;
pub const SYS_FCHMOD: u64 = 91;
pub const SYS_CHOWN: u64 = 92;
pub const SYS_FCHOWN: u64 = 93;
pub const SYS_UMASK: u64 = 95;
pub const SYS_GETUID: u64 = 102;
pub const SYS_GETGID: u64 = 104;
pub const SYS_SETUID: u64 = 105;
pub const SYS_SETGID: u64 = 106;
pub const SYS_GETEUID: u64 = 107;
pub const SYS_GETEGID: u64 = 108;
pub const SYS_SETREUID: u64 = 113;
pub const SYS_SETREGID: u64 = 114;

// Directory listing and stat
pub const SYS_GETDENTS64: u64 = 217;
pub const SYS_NEWFSTATAT: u64 = 262;
pub const SYS_LINKAT: u64 = 265;
pub const SYS_SYMLINKAT: u64 = 266;
pub const SYS_READLINKAT: u64 = 267;

/// Custom kernel debug-print syscall.
pub const SYS_DEBUG_PRINT: u64 = 0x1000;

/// Custom kernel meminfo syscall (Phase 33).
pub const SYS_MEMINFO: u64 = 0x1001;

/// Custom kernel trace ring read syscall (Phase 43b).
pub const SYS_KTRACE: u64 = 0x1002;

/// Phase 47: framebuffer info syscall.
pub const SYS_FRAMEBUFFER_INFO: u64 = 0x1005;

/// Phase 47: framebuffer mmap syscall.
pub const SYS_FRAMEBUFFER_MMAP: u64 = 0x1006;

/// Phase 47: raw scancode read syscall.
pub const SYS_READ_SCANCODE: u64 = 0x1007;

/// Phase 52: push bytes into kernel stdin from userspace.
pub const SYS_STDIN_PUSH: u64 = 0x1008;

/// Phase 52: signal a foreground process group from userspace.
pub const SYS_SIGNAL_PROCESS_GROUP: u64 = 0x1009;

/// Phase 52: read one scancode from the TTY keyboard buffer (for kbd service).
pub const SYS_READ_KBD_SCANCODE: u64 = 0x100A;

// ===========================================================================
// IPC syscall numbers (Phase 50)
// ===========================================================================

/// IPC recv: block until a message arrives on the endpoint.
pub const SYS_IPC_RECV: u64 = 0x1100;

/// IPC send: send a message to an endpoint (no reply expected).
pub const SYS_IPC_SEND: u64 = 0x1101;

/// IPC call: send a message and block for a reply.
pub const SYS_IPC_CALL: u64 = 0x1102;

/// IPC reply: send a reply to a caller.
pub const SYS_IPC_REPLY: u64 = 0x1103;

/// IPC reply_recv: reply to a caller and immediately wait for the next message.
pub const SYS_IPC_REPLY_RECV: u64 = 0x1104;

/// Grant a capability to another task.
pub const SYS_CAP_GRANT: u64 = 0x1105;

/// Wait on a notification capability.
pub const SYS_NOTIFY_WAIT: u64 = 0x1106;

/// Signal a notification capability.
pub const SYS_NOTIFY_SIGNAL: u64 = 0x1107;

/// Register a named service in the IPC registry.
pub const SYS_IPC_REGISTER_SERVICE: u64 = 0x1108;

/// Look up a named service in the IPC registry.
pub const SYS_IPC_LOOKUP_SERVICE: u64 = 0x1109;

/// Create a notification for a hardware IRQ and return a cap handle.
pub const SYS_CREATE_IRQ_NOTIFICATION: u64 = 0x110A;

/// Create a new IPC endpoint and return a cap handle.
pub const SYS_CREATE_ENDPOINT: u64 = 0x110B;

/// Send a message with an attached bulk-data buffer (fire-and-forget).
pub const SYS_IPC_SEND_BUF: u64 = 0x110C;

/// Send a message with an attached bulk-data buffer and wait for reply.
pub const SYS_IPC_CALL_BUF: u64 = 0x110D;

/// Receive a message with full data words and optional bulk payload.
pub const SYS_IPC_RECV_MSG: u64 = 0x110E;

/// Reply then receive with full data words and optional bulk payload.
pub const SYS_IPC_REPLY_RECV_MSG: u64 = 0x110F;

/// Store bulk data to be sent with the next IPC reply (Phase 54).
pub const SYS_IPC_STORE_REPLY_BULK: u64 = 0x1110;

/// Bind a notification capability to an endpoint for the bound-recv path (Phase 55c Track B).
pub const SYS_NOTIF_BIND: u64 = 0x1111;

/// Read raw disk sectors from userspace (Phase 54).
pub const SYS_BLOCK_READ: u64 = 0x1011;

/// Write raw disk sectors from userspace (Phase 55b Track F.3d-2).
pub const SYS_BLOCK_WRITE: u64 = 0x1012;

// ===========================================================================
// IPC wrappers (Phase 52)
// ===========================================================================

/// IPC message data: 4 u64 words.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct IpcMessage {
    pub label: u64,
    pub data: [u64; 4],
}

impl IpcMessage {
    pub const fn new(label: u64) -> Self {
        Self {
            label,
            data: [0; 4],
        }
    }
}

/// Receive a message on an endpoint capability.
///
/// `ep_cap_handle` is the handle of an Endpoint capability in the caller's
/// capability table. Blocks until a message is available. Returns the
/// message label on success, or `u64::MAX` on error.
pub fn ipc_recv(ep_cap_handle: u32) -> u64 {
    unsafe { syscall1(SYS_IPC_RECV, ep_cap_handle as u64) }
}

/// Send a message to an endpoint (fire-and-forget, no reply).
///
/// Returns 0 on success, `u64::MAX` on error.
pub fn ipc_send(ep_cap_handle: u32, label: u64, data0: u64) -> u64 {
    unsafe { syscall3(SYS_IPC_SEND, ep_cap_handle as u64, label, data0) }
}

/// Send a message and block waiting for a reply (RPC-style call).
///
/// Returns the reply label on success, `u64::MAX` on error.
pub fn ipc_call(ep_cap_handle: u32, label: u64, data0: u64) -> u64 {
    unsafe { syscall3(SYS_IPC_CALL, ep_cap_handle as u64, label, data0) }
}

/// Reply to a caller using a one-shot reply capability.
///
/// Returns 0 on success, `u64::MAX` on error.
pub fn ipc_reply(reply_cap_handle: u32, label: u64, data0: u64) -> u64 {
    unsafe { syscall3(SYS_IPC_REPLY, reply_cap_handle as u64, label, data0) }
}

/// Reply to a caller and immediately wait for the next message on an endpoint.
///
/// Returns the next message label on success, `u64::MAX` on error.
pub fn ipc_reply_recv(reply_cap_handle: u32, label: u64, ep_cap_handle: u32) -> u64 {
    unsafe {
        syscall3(
            SYS_IPC_REPLY_RECV,
            reply_cap_handle as u64,
            label,
            ep_cap_handle as u64,
        )
    }
}

/// Register a named service endpoint in the IPC registry.
///
/// `ep_cap_handle` is the handle of the Endpoint capability to register.
/// Returns 0 on success, `u64::MAX` on error.
pub fn ipc_register_service(ep_cap_handle: u32, name: &str) -> u64 {
    unsafe {
        syscall3(
            SYS_IPC_REGISTER_SERVICE,
            ep_cap_handle as u64,
            name.as_ptr() as u64,
            name.len() as u64,
        )
    }
}

/// Look up a named service in the IPC registry.
///
/// On success, inserts an Endpoint capability into the caller's capability
/// table and returns the new handle. Returns `u64::MAX` on error.
pub fn ipc_lookup_service(name: &str) -> u64 {
    unsafe {
        syscall2(
            SYS_IPC_LOOKUP_SERVICE,
            name.as_ptr() as u64,
            name.len() as u64,
        )
    }
}

/// Wait on a notification capability. Blocks until at least one bit is set.
///
/// Returns the pending-bit word on success, or 0 on error.
pub fn notify_wait(notif_cap_handle: u32) -> u64 {
    unsafe { syscall1(SYS_NOTIFY_WAIT, notif_cap_handle as u64) }
}

/// Signal a notification capability with the given bits.
///
/// Returns 0 on success, `u64::MAX` on error.
pub fn notify_signal(notif_cap_handle: u32, bits: u64) -> u64 {
    unsafe { syscall2(SYS_NOTIFY_SIGNAL, notif_cap_handle as u64, bits) }
}

/// Create a new IPC endpoint.
///
/// Returns the new cap handle on success, `u64::MAX` on error.
pub fn create_endpoint() -> u64 {
    unsafe { syscall0(SYS_CREATE_ENDPOINT) }
}

/// Create a notification registered for a hardware IRQ and return a cap handle.
///
/// Only IRQ 1 (keyboard) is currently supported.
/// Returns the new cap handle on success, `u64::MAX` on error.
pub fn create_irq_notification(irq: u32) -> u64 {
    unsafe { syscall1(SYS_CREATE_IRQ_NOTIFICATION, irq as u64) }
}

// ---------------------------------------------------------------------------
// Bulk-data IPC wrappers (Phase 52)
// ---------------------------------------------------------------------------

/// Send a message with an attached bulk-data buffer (fire-and-forget).
///
/// The kernel copies `buf` from this process's address space, attaches it
/// to the IPC message, and delivers both to the endpoint receiver.
/// Returns 0 on success, `u64::MAX` on error.
pub fn ipc_send_buf(ep_cap_handle: u32, label: u64, data0: u64, buf: &[u8]) -> u64 {
    unsafe {
        syscall6(
            SYS_IPC_SEND_BUF,
            ep_cap_handle as u64,
            label,
            data0,
            buf.as_ptr() as u64,
            buf.len() as u64,
            0,
        )
    }
}

/// Send a message with an attached bulk-data buffer and wait for a reply.
///
/// Like [`ipc_call`] but additionally copies `buf` from this process's
/// address space and delivers it alongside the message.
/// Returns the reply label on success, `u64::MAX` on error.
pub fn ipc_call_buf(ep_cap_handle: u32, label: u64, data0: u64, buf: &[u8]) -> u64 {
    unsafe {
        syscall6(
            SYS_IPC_CALL_BUF,
            ep_cap_handle as u64,
            label,
            data0,
            buf.as_ptr() as u64,
            buf.len() as u64,
            0,
        )
    }
}

/// Receive a message with full data words and optional bulk payload.
///
/// Blocks until a message arrives on the endpoint.  The kernel writes
/// the [`IpcMessage`] header (label + data[0..4]) to `msg` and copies
/// any attached bulk data into `buf`.  Returns the label on success,
/// `u64::MAX` on error.
pub fn ipc_recv_msg(ep_cap_handle: u32, msg: &mut IpcMessage, buf: &mut [u8]) -> u64 {
    unsafe {
        syscall6(
            SYS_IPC_RECV_MSG,
            ep_cap_handle as u64,
            msg as *mut IpcMessage as u64,
            buf.as_mut_ptr() as u64,
            buf.len() as u64,
            0,
            0,
        )
    }
}

/// Reply to a caller and immediately receive the next message with bulk data.
///
/// Combines reply + recv_msg in one syscall.  The reply carries only a
/// label (no bulk data in the reply direction).  The received message
/// header is written to `msg` and any bulk payload to `buf`.
/// Returns the next message label on success, `u64::MAX` on error.
pub fn ipc_reply_recv_msg(
    reply_cap_handle: u32,
    reply_label: u64,
    ep_cap_handle: u32,
    msg: &mut IpcMessage,
    buf: &mut [u8],
) -> u64 {
    unsafe {
        syscall6(
            SYS_IPC_REPLY_RECV_MSG,
            reply_cap_handle as u64,
            reply_label,
            ep_cap_handle as u64,
            msg as *mut IpcMessage as u64,
            buf.as_mut_ptr() as u64,
            buf.len() as u64,
        )
    }
}

/// Store bulk data to be attached to the next IPC reply (Phase 54).
///
/// The kernel copies `buf` from this process's address space into the
/// task's `pending_bulk` slot.  When the server next calls [`ipc_reply`]
/// or [`ipc_reply_recv_msg`], the pending bulk is transferred to the
/// caller alongside the reply message.
///
/// Returns 0 on success, `u64::MAX` on error.
pub fn ipc_store_reply_bulk(buf: &[u8]) -> u64 {
    unsafe {
        syscall2(
            SYS_IPC_STORE_REPLY_BULK,
            buf.as_ptr() as u64,
            buf.len() as u64,
        )
    }
}

/// Bind a notification capability to an endpoint for the bound-recv path (Phase 55c Track B).
///
/// After a successful bind, `ipc_recv_msg` on `ep_cap_handle` will return
/// `RECV_KIND_NOTIFICATION` (= 1) when the bound notification's pending bits
/// are non-zero, delivering the drained bit mask in `IpcMessage.data[0]`.
///
/// Returns 0 on success, or a negative errno:
/// - `-9` (EBADF): invalid capability handle or wrong type.
/// - `-16` (EBUSY): notification already bound to a different task.
pub fn sys_notif_bind(notif_cap_handle: u32, ep_cap_handle: u32) -> u64 {
    unsafe {
        syscall2(
            SYS_NOTIF_BIND,
            notif_cap_handle as u64,
            ep_cap_handle as u64,
        )
    }
}

/// Read raw disk sectors into a userspace buffer (Phase 54).
///
/// Reads `count` 512-byte sectors starting at `start_sector` (absolute LBA)
/// into `buf`.  `buf` must be at least `count * 512` bytes.
///
/// Returns 0 on success, negative errno on error.
pub fn block_read(start_sector: u64, count: usize, buf: &mut [u8]) -> i64 {
    unsafe {
        syscall6(
            SYS_BLOCK_READ,
            start_sector,
            count as u64,
            buf.as_mut_ptr() as u64,
            buf.len() as u64,
            0,
            0,
        ) as i64
    }
}

/// Write raw disk sectors from a userspace buffer (Phase 55b Track F.3d-2).
///
/// Writes `count` 512-byte sectors starting at `start_sector` (absolute LBA)
/// from `buf`.  `buf` must be at least `count * 512` bytes.
///
/// Returns 0 on success, negative errno on error (EAGAIN = -11 when driver
/// is mid-restart; EIO = -5 for hard failures; EPERM = -1 if caller lacks
/// storage-service credentials).
pub fn block_write(start_sector: u64, count: usize, buf: &[u8]) -> i64 {
    unsafe {
        syscall6(
            SYS_BLOCK_WRITE,
            start_sector,
            count as u64,
            buf.as_ptr() as u64,
            buf.len() as u64,
            0,
            0,
        ) as i64
    }
}

/// Push bytes from a userspace buffer into the kernel stdin buffer.
///
/// Returns 0 on success, negative errno on error.
pub fn stdin_push(buf: &[u8]) -> isize {
    unsafe { syscall2(SYS_STDIN_PUSH, buf.as_ptr() as u64, buf.len() as u64) as isize }
}

/// Read one scancode from the TTY keyboard buffer (non-blocking).
///
/// Returns the scancode, or 0 if the buffer is empty.
pub fn read_kbd_scancode() -> u8 {
    unsafe { syscall0(SYS_READ_KBD_SCANCODE) as u8 }
}

/// Signal the foreground process group with the given signal number.
///
/// Returns 0 on success.
pub fn signal_process_group(sig: u32) -> isize {
    unsafe { syscall2(SYS_SIGNAL_PROCESS_GROUP, sig as u64, 0) as isize }
}

/// Phase 52: get termios c_lflag, c_iflag, c_oflag, and c_cc from TTY0.
pub const SYS_GET_TERMIOS_FLAGS: u64 = 0x100B;

/// Phase 52: signal EOF on kernel stdin.
pub const SYS_STDIN_SIGNAL_EOF: u64 = 0x100C;

/// Termios flags returned by `get_termios_flags`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct TermiosFlags {
    pub c_lflag: u32,
    pub c_iflag: u32,
    pub c_oflag: u32,
    pub c_cc: [u8; 19],
    /// Padding to match the kernel's 32-byte `sys_get_termios_flags` layout.
    pub _pad: u8,
}

impl TermiosFlags {
    pub const fn zeroed() -> Self {
        Self {
            c_lflag: 0,
            c_iflag: 0,
            c_oflag: 0,
            c_cc: [0; 19],
            _pad: 0,
        }
    }
}

/// Query termios flags from the kernel's TTY0.
///
/// Returns 0 on success and fills the provided struct.
///
/// Uses a compiler fence after the syscall to force a reload of the struct
/// from memory. Without this, the optimizer may cache the pre-zeroed values
/// because the syscall asm block passes the buffer as a raw integer, severing
/// the pointer provenance that would tell the compiler the buffer is modified.
pub fn get_termios_flags(out: &mut TermiosFlags) -> isize {
    let buf = out as *mut TermiosFlags as *mut u8;
    let ret = unsafe {
        syscall2(
            SYS_GET_TERMIOS_FLAGS,
            buf as u64,
            core::mem::size_of::<TermiosFlags>() as u64,
        ) as isize
    };
    // Force the compiler to reload `out` from memory after the syscall.
    core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
    ret
}

/// Signal EOF on the kernel stdin buffer.
///
/// Causes the next `read(0, ...)` to return 0.
pub fn stdin_signal_eof() -> isize {
    unsafe { syscall0(SYS_STDIN_SIGNAL_EOF) as isize }
}

/// Syscall numbers for direct termios field reads (return value in rax).
///
/// **Temporary compatibility only** — introduced as a workaround for the
/// `copy_to_user` intermittent reliability bug (see
/// `docs/appendix/copy-to-user-reliability-bug.md`).  No in-tree binary
/// depends on these after Phase 52d Track C converged keyboard input on
/// `push_raw_input`.  Retained only for out-of-tree or diagnostic use.
/// Prefer `tcgetattr` or kernel-side line discipline for new code.
pub const SYS_GET_TERMIOS_LFLAG: u64 = 0x100D;
pub const SYS_GET_TERMIOS_IFLAG: u64 = 0x100E;
pub const SYS_GET_TERMIOS_OFLAG: u64 = 0x100F;

/// Get TTY0 c_lflag directly as a return value.
///
/// **Deprecated** — temporary `copy_to_user` workaround.
/// No in-tree binary calls this after Phase 52d Track C.
#[deprecated(note = "temporary copy_to_user workaround; use tcgetattr or push_raw_input")]
pub fn get_termios_lflag() -> u32 {
    unsafe { syscall0(SYS_GET_TERMIOS_LFLAG) as u32 }
}

/// Get TTY0 c_iflag directly as a return value.
///
/// **Deprecated** — temporary `copy_to_user` workaround.
/// No in-tree binary calls this after Phase 52d Track C.
#[deprecated(note = "temporary copy_to_user workaround; use tcgetattr or push_raw_input")]
pub fn get_termios_iflag() -> u32 {
    unsafe { syscall0(SYS_GET_TERMIOS_IFLAG) as u32 }
}

/// Get TTY0 c_oflag directly as a return value.
///
/// **Deprecated** — temporary `copy_to_user` workaround.
/// No in-tree binary calls this after Phase 52d Track C.
#[deprecated(note = "temporary copy_to_user workaround; use tcgetattr or push_raw_input")]
pub fn get_termios_oflag() -> u32 {
    unsafe { syscall0(SYS_GET_TERMIOS_OFLAG) as u32 }
}

/// Phase 52c: push raw input byte through kernel line discipline.
pub const SYS_PUSH_RAW_INPUT: u64 = 0x1010;

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
pub const SYS_SOCKETPAIR: u64 = 53;
pub const SYS_SETSOCKOPT: u64 = 54;
pub const SYS_GETSOCKOPT: u64 = 55;
pub const SYS_CLOCK_GETTIME: u64 = 228;

// Phase 32: File timestamp syscall
pub const SYS_UTIMENSAT: u64 = 280;

// Phase 21: getrandom (kernel syscall 318)
pub const SYS_GETRANDOM: u64 = 318;

// ===========================================================================
// File flags and constants
// ===========================================================================

pub const O_RDONLY: u64 = 0;
pub const O_WRONLY: u64 = 1;
pub const O_RDWR: u64 = 2;
pub const O_CREAT: u64 = 0x40;
pub const O_TRUNC: u64 = 0x200;
pub const O_APPEND: u64 = 0x400;
pub const O_DIRECTORY: u64 = 0o200000;

pub const STDIN_FILENO: i32 = 0;
pub const STDOUT_FILENO: i32 = 1;
pub const STDERR_FILENO: i32 = 2;

pub const NEG_EEXIST: isize = -17;

// ===========================================================================
// Socket constants (Phase 23)
// ===========================================================================

pub const AF_UNIX: u64 = 1;
pub const AF_INET: u64 = 2;
pub const SOCK_STREAM: u64 = 1;
pub const SOCK_DGRAM: u64 = 2;
pub const SOCK_CLOEXEC: u64 = 0x80000;

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

// Clock IDs
pub const CLOCK_REALTIME: u64 = 0;
pub const CLOCK_MONOTONIC: u64 = 1;

// Syscall numbers for time
pub const SYS_GETTIMEOFDAY: u64 = 96;

// Shutdown modes
pub const SHUT_RD: i32 = 0;
pub const SHUT_WR: i32 = 1;
pub const SHUT_RDWR: i32 = 2;

// Poll syscall number
pub const SYS_POLL: u64 = 7;

// Poll events
pub const POLLIN: i16 = 0x001;
pub const POLLOUT: i16 = 0x004;
pub const POLLERR: i16 = 0x008;
pub const POLLHUP: i16 = 0x010;
pub const POLLNVAL: i16 = 0x020;

/// Poll file descriptor entry, matching Linux `struct pollfd` layout.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct PollFd {
    pub fd: i32,
    pub events: i16,
    pub revents: i16,
}

/// Wrapper for the `poll()` syscall.
/// Returns the number of ready file descriptors, 0 on timeout, or negative errno.
pub fn poll(fds: &mut [PollFd], timeout_ms: i32) -> isize {
    unsafe {
        syscall3(
            SYS_POLL,
            fds.as_mut_ptr() as u64,
            fds.len() as u64,
            timeout_ms as u64,
        ) as isize
    }
}

// fcntl constants
pub const SYS_FCNTL: u64 = 72;
pub const SYS_FSYNC: u64 = 74;
pub const F_GETFL: u64 = 3;
pub const F_SETFL: u64 = 4;
pub const O_NONBLOCK: u64 = 0x800;

/// Wrapper for the `fcntl()` syscall.
pub fn fcntl(fd: i32, cmd: u64, arg: u64) -> isize {
    unsafe { syscall3(SYS_FCNTL, fd as u64, cmd, arg) as isize }
}

/// Set the `O_NONBLOCK` flag on a file descriptor.
/// Returns 0 on success, or negative errno on failure.
pub fn set_nonblocking(fd: i32) -> isize {
    let flags = fcntl(fd, F_GETFL, 0);
    if flags < 0 {
        return flags;
    }
    fcntl(fd, F_SETFL, (flags as u64) | O_NONBLOCK)
}

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

/// Unix domain socket address, matching Linux `struct sockaddr_un` layout.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SockaddrUn {
    pub sun_family: u16,
    pub sun_path: [u8; 108],
}

impl SockaddrUn {
    /// Create a new `SockaddrUn` for the given path.
    pub fn new(path: &str) -> Self {
        let mut addr = Self {
            sun_family: AF_UNIX as u16,
            sun_path: [0; 108],
        };
        let bytes = path.as_bytes();
        let n = bytes.len().min(107);
        addr.sun_path[..n].copy_from_slice(&bytes[..n]);
        addr
    }

    /// Return the length (family + path + NUL), clamped to struct size.
    pub fn len(&self) -> usize {
        let path_len = self.sun_path.iter().position(|&b| b == 0).unwrap_or(108);
        // Clamp to struct size (110 = 2 + 108).
        (2 + path_len + 1).min(core::mem::size_of::<Self>())
    }

    pub fn is_empty(&self) -> bool {
        self.sun_path[0] == 0
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

pub const SIGHUP: i32 = 1;
pub const SIGINT: i32 = 2;
pub const SIGQUIT: i32 = 3;
pub const SIGKILL: i32 = 9;
pub const SIGTERM: i32 = 15;
pub const SIGCHLD: i32 = 17;
pub const SIGCONT: i32 = 18;
pub const SIGTSTP: i32 = 20;
pub const SIGUSR1: i32 = 10;
pub const SIGUSR2: i32 = 12;
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
pub const ONLCR: u32 = 0o000004;

// c_cflag constants
pub const CS8: u32 = 0o000060;
pub const CREAD: u32 = 0o000200;
pub const HUPCL: u32 = 0o000400;
pub const B38400: u32 = 0o060000;

/// Terminal I/O settings, matching the Linux *kernel* `termios` layout
/// used by the TCGETS/TCSETS ioctls (36 bytes).
///
/// This is the kernel ioctl copy format, **not** the libc/musl userland
/// `struct termios` layout (which is 60 bytes).
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

/// Flush file data to storage.
pub fn fsync(fd: i32) -> isize {
    unsafe { syscall1(SYS_FSYNC, fd as u64) as isize }
}

// ===========================================================================
// High-level wrappers — ioctl, lseek, termios, signals
// ===========================================================================

/// Perform an ioctl operation on a file descriptor.
pub fn ioctl(fd: i32, request: usize, arg: usize) -> isize {
    unsafe { syscall3(SYS_IOCTL, fd as u64, request as u64, arg as u64) as isize }
}

/// Seek to a position in a file descriptor.
pub fn lseek(fd: i32, offset: i64, whence: usize) -> isize {
    unsafe { syscall3(SYS_LSEEK, fd as u64, offset as u64, whence as u64) as isize }
}

/// Get terminal attributes.
pub fn tcgetattr(fd: i32) -> Result<Termios, isize> {
    let mut t = Termios {
        c_iflag: 0,
        c_oflag: 0,
        c_cflag: 0,
        c_lflag: 0,
        c_line: 0,
        c_cc: [0; NCCS],
    };
    let ret = ioctl(fd, TCGETS, &mut t as *mut Termios as usize);
    if ret < 0 { Err(ret) } else { Ok(t) }
}

/// Set terminal attributes.
pub fn tcsetattr(fd: i32, termios: &Termios) -> Result<(), isize> {
    let ret = ioctl(fd, TCSETS, termios as *const Termios as usize);
    if ret < 0 { Err(ret) } else { Ok(()) }
}

/// Set terminal attributes, flushing pending input first (TCSETSF).
///
/// Like [`tcsetattr`], but uses the TCSETSF ioctl which flushes the stdin
/// buffer and TTY edit buffer before applying the new settings.
pub fn tcsetattr_flush(fd: i32, termios: &Termios) -> Result<(), isize> {
    let ret = ioctl(fd, TCSETSF, termios as *const Termios as usize);
    if ret < 0 { Err(ret) } else { Ok(()) }
}

/// Get terminal window size (rows, cols).
pub fn get_window_size(fd: i32) -> Result<(u16, u16), isize> {
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

// Shared sigaction restorer trampoline. Lives in syscall-lib's .text so every
// userspace binary can point `sa_restorer` at it without re-declaring the stub.
// When a signal handler returns, control transfers here, which invokes
// rt_sigreturn (syscall 15) to restore the pre-signal register/stack context.
// Without this, `rt_sigaction` registrations default to restorer=0 and the
// process will fault the first time the handler actually fires.
core::arch::global_asm!(
    ".global __syscall_lib_sigrestorer",
    "__syscall_lib_sigrestorer:",
    "mov rax, 15",
    "syscall",
);

unsafe extern "C" {
    pub fn __syscall_lib_sigrestorer();
}

/// Install a signal handler with the default restorer trampoline wired up.
/// Prefer this over the raw `rt_sigaction` for ordinary `fn(i32)` handlers —
/// it guarantees the `SA_RESTORER` flag and a valid restorer pointer so the
/// handler can actually return without faulting.
pub fn rt_sigaction_simple(signum: usize, handler: extern "C" fn(i32)) -> isize {
    let act = SigAction {
        sa_handler: handler as *const () as u64,
        sa_flags: SA_RESTORER,
        sa_restorer: __syscall_lib_sigrestorer as *const () as u64,
        sa_mask: 0,
    };
    rt_sigaction(signum, &act as *const SigAction, core::ptr::null_mut())
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
    // The kernel terminates the process on SYS_EXIT; this is unreachable.
    #[allow(clippy::empty_loop)]
    loop {}
}

// ===========================================================================
// High-level wrappers — User identity (Phase 27)
// ===========================================================================

/// Get the real user ID of the calling process.
pub fn getuid() -> u32 {
    unsafe { syscall0(SYS_GETUID) as u32 }
}

/// Get the real group ID of the calling process.
pub fn getgid() -> u32 {
    unsafe { syscall0(SYS_GETGID) as u32 }
}

/// Get the effective user ID of the calling process.
pub fn geteuid() -> u32 {
    unsafe { syscall0(SYS_GETEUID) as u32 }
}

/// Get the effective group ID of the calling process.
pub fn getegid() -> u32 {
    unsafe { syscall0(SYS_GETEGID) as u32 }
}

/// Set the user ID of the calling process.
pub fn setuid(uid: u32) -> isize {
    unsafe { syscall1(SYS_SETUID, uid as u64) as isize }
}

/// Set the group ID of the calling process.
pub fn setgid(gid: u32) -> isize {
    unsafe { syscall1(SYS_SETGID, gid as u64) as isize }
}

/// Change file mode bits. `path` must be null-terminated.
pub fn chmod(path: &[u8], mode: u16) -> isize {
    unsafe { syscall2(SYS_CHMOD, path.as_ptr() as u64, mode as u64) as isize }
}

/// Change file ownership. `path` must be null-terminated.
pub fn chown(path: &[u8], uid: u32, gid: u32) -> isize {
    unsafe { syscall3(SYS_CHOWN, path.as_ptr() as u64, uid as u64, gid as u64) as isize }
}

/// Change file mode bits by fd.
pub fn fchmod(fd: i32, mode: u16) -> isize {
    unsafe { syscall2(SYS_FCHMOD, fd as u64, mode as u64) as isize }
}

/// Change file ownership by fd.
pub fn fchown(fd: i32, uid: u32, gid: u32) -> isize {
    unsafe { syscall3(SYS_FCHOWN, fd as u64, uid as u64, gid as u64) as isize }
}

/// Set real and effective user IDs.
/// Pass `None` for an ID you do not want to change (translated to -1 for the kernel).
pub fn setreuid(ruid: Option<u32>, euid: Option<u32>) -> isize {
    let r = ruid.map(|id| id as u64).unwrap_or(u32::MAX as u64);
    let e = euid.map(|id| id as u64).unwrap_or(u32::MAX as u64);
    unsafe { syscall2(SYS_SETREUID, r, e) as isize }
}

/// Set real and effective group IDs.
/// Pass `None` for an ID you do not want to change (translated to -1 for the kernel).
pub fn setregid(rgid: Option<u32>, egid: Option<u32>) -> isize {
    let r = rgid.map(|id| id as u64).unwrap_or(u32::MAX as u64);
    let e = egid.map(|id| id as u64).unwrap_or(u32::MAX as u64);
    unsafe { syscall2(SYS_SETREGID, r, e) as isize }
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

/// Create a directory. `path` must be a null-terminated byte string.
pub fn mkdir(path: &[u8], mode: u64) -> isize {
    unsafe { syscall2(SYS_MKDIR, path.as_ptr() as u64, mode) as isize }
}

/// Remove an empty directory. `path` must be a null-terminated byte string.
pub fn rmdir(path: &[u8]) -> isize {
    unsafe { syscall1(SYS_RMDIR, path.as_ptr() as u64) as isize }
}

/// Remove (unlink) a file. `path` must be a null-terminated byte string.
pub fn unlink(path: &[u8]) -> isize {
    unsafe { syscall1(SYS_UNLINK, path.as_ptr() as u64) as isize }
}

/// Create a hard link. Both paths must be null-terminated byte strings.
pub fn link(oldpath: &[u8], newpath: &[u8]) -> isize {
    unsafe { syscall2(SYS_LINK, oldpath.as_ptr() as u64, newpath.as_ptr() as u64) as isize }
}

/// Rename a file or directory. Both paths must be null-terminated byte strings.
pub fn rename(old: &[u8], new: &[u8]) -> isize {
    unsafe { syscall2(SYS_RENAME, old.as_ptr() as u64, new.as_ptr() as u64) as isize }
}

/// Read directory entries. Returns bytes read, 0 at end, or negative on error.
pub fn getdents64(fd: i32, buf: &mut [u8]) -> isize {
    unsafe {
        syscall3(
            SYS_GETDENTS64,
            fd as u64,
            buf.as_mut_ptr() as u64,
            buf.len() as u64,
        ) as isize
    }
}

/// Stat a file by path (newfstatat syscall 262).
///
/// `path` must be null-terminated. `stat_buf` must be 144 bytes.
/// Uses `AT_FDCWD` (-100) as dirfd and `flags = 0`.
pub fn newfstatat(path: &[u8], stat_buf: &mut [u8; 144]) -> isize {
    newfstatat_flags(path, stat_buf, 0)
}

/// Stat a file by path with explicit flags (newfstatat syscall 262).
///
/// `path` must be null-terminated. `stat_buf` must be 144 bytes.
/// Uses `AT_FDCWD` (-100) as dirfd.
///
/// Supported flags currently include `AT_SYMLINK_NOFOLLOW` for `lstat`-style
/// metadata lookups. Other Linux `newfstatat` flags are not implemented yet.
pub fn newfstatat_flags(path: &[u8], stat_buf: &mut [u8; 144], flags: u64) -> isize {
    unsafe {
        syscall4(
            SYS_NEWFSTATAT,
            (-100i64) as u64,
            path.as_ptr() as u64,
            stat_buf.as_mut_ptr() as u64,
            flags,
        ) as isize
    }
}

/// `lstat(path, buf)` — get metadata without following the final symlink.
pub fn lstat(path: &[u8], stat_buf: &mut [u8; 144]) -> isize {
    const AT_SYMLINK_NOFOLLOW: u64 = 0x100;
    newfstatat_flags(path, stat_buf, AT_SYMLINK_NOFOLLOW)
}

/// `lstat(path, buf)` — get typed metadata without following the final symlink.
pub fn lstat_stat(path: &[u8], buf: &mut Stat) -> isize {
    const AT_SYMLINK_NOFOLLOW: u64 = 0x100;
    unsafe {
        syscall4(
            SYS_NEWFSTATAT,
            (-100i64) as u64,
            path.as_ptr() as u64,
            buf as *mut Stat as u64,
            AT_SYMLINK_NOFOLLOW,
        ) as isize
    }
}

/// `symlink(target, linkpath)` — create a symbolic link.
pub fn symlink(target: &[u8], linkpath: &[u8]) -> isize {
    unsafe {
        syscall2(
            SYS_SYMLINK,
            target.as_ptr() as u64,
            linkpath.as_ptr() as u64,
        ) as isize
    }
}

/// `readlink(path, buf)` — read a symbolic link target without NUL termination.
pub fn readlink(path: &[u8], buf: &mut [u8]) -> isize {
    unsafe {
        syscall3(
            SYS_READLINK,
            path.as_ptr() as u64,
            buf.as_mut_ptr() as u64,
            buf.len() as u64,
        ) as isize
    }
}

/// `symlinkat(target, dirfd, linkpath)` — create a symbolic link relative to a directory fd.
pub fn symlinkat(target: &[u8], dirfd: i32, linkpath: &[u8]) -> isize {
    unsafe {
        syscall3(
            SYS_SYMLINKAT,
            target.as_ptr() as u64,
            dirfd as u64,
            linkpath.as_ptr() as u64,
        ) as isize
    }
}

/// `readlinkat(dirfd, path, buf)` — read a symbolic link target relative to a directory fd.
pub fn readlinkat(dirfd: i32, path: &[u8], buf: &mut [u8]) -> isize {
    unsafe {
        syscall4(
            SYS_READLINKAT,
            dirfd as u64,
            path.as_ptr() as u64,
            buf.as_mut_ptr() as u64,
            buf.len() as u64,
        ) as isize
    }
}

/// `linkat(olddirfd, oldpath, newdirfd, newpath, flags)` — create a hard link relative to directory fds.
pub fn linkat(olddirfd: i32, oldpath: &[u8], newdirfd: i32, newpath: &[u8], flags: i32) -> isize {
    unsafe {
        syscall5(
            SYS_LINKAT,
            olddirfd as u64,
            oldpath.as_ptr() as u64,
            newdirfd as u64,
            newpath.as_ptr() as u64,
            flags as u64,
        ) as isize
    }
}

/// `umask(mask)` — set file creation mask and return the previous mask.
pub fn umask(mask: u32) -> isize {
    unsafe { syscall1(SYS_UMASK, mask as u64) as isize }
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
// High-level wrappers — Reboot (Phase 46)
// ===========================================================================

pub const SYS_REBOOT: u64 = 169;

/// Reboot command: halt / power-off the system.
pub const REBOOT_CMD_HALT: u64 = 0xCDEF0123;
/// Reboot command: restart the system.
pub const REBOOT_CMD_RESTART: u64 = 0x01234567;
/// Reboot command: power off the system.
pub const REBOOT_CMD_POWER_OFF: u64 = 0x4321FEDC;

/// Request system reboot or halt. Only UID 0 may call this.
pub fn reboot(cmd: u64) -> isize {
    unsafe { syscall1(SYS_REBOOT, cmd) as isize }
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

/// Unmount a filesystem. `target` must be null-terminated.
pub fn umount(target: &[u8]) -> isize {
    unsafe { syscall2(SYS_UMOUNT2, target.as_ptr() as u64, 0) as isize }
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
// Phase 39: Unix domain socket helpers
// ===========================================================================

/// Create a pair of connected Unix domain sockets.
/// Returns 0 on success (fds written to `sv`), negative errno on error.
pub fn socketpair(domain: i32, socktype: i32, protocol: i32, sv: &mut [i32; 2]) -> isize {
    unsafe {
        syscall4(
            SYS_SOCKETPAIR,
            domain as u64,
            socktype as u64,
            protocol as u64,
            sv.as_mut_ptr() as u64,
        ) as isize
    }
}

/// Bind a Unix domain socket to a path.
pub fn bind_unix(fd: i32, addr: &SockaddrUn) -> isize {
    unsafe {
        syscall3(
            SYS_BIND,
            fd as u64,
            addr as *const SockaddrUn as u64,
            addr.len() as u64,
        ) as isize
    }
}

/// Connect a Unix domain socket to a path.
pub fn connect_unix(fd: i32, addr: &SockaddrUn) -> isize {
    unsafe {
        syscall3(
            SYS_CONNECT,
            fd as u64,
            addr as *const SockaddrUn as u64,
            addr.len() as u64,
        ) as isize
    }
}

/// Send a datagram to a Unix domain socket address.
pub fn sendto_unix(fd: i32, buf: &[u8], flags: i32, addr: &SockaddrUn) -> isize {
    unsafe {
        syscall6(
            SYS_SENDTO,
            fd as u64,
            buf.as_ptr() as u64,
            buf.len() as u64,
            flags as u64,
            addr as *const SockaddrUn as u64,
            addr.len() as u64,
        ) as isize
    }
}

/// Receive a datagram from a Unix domain socket (with sender address).
pub fn recvfrom_unix(fd: i32, buf: &mut [u8], flags: i32, addr: &mut SockaddrUn) -> isize {
    let mut len: u32 = core::mem::size_of::<SockaddrUn>() as u32;
    unsafe {
        syscall6(
            SYS_RECVFROM,
            fd as u64,
            buf.as_mut_ptr() as u64,
            buf.len() as u64,
            flags as u64,
            addr as *mut SockaddrUn as u64,
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

/// Query kernel memory statistics.
///
/// Writes a text summary into `buf` and returns the number of bytes written.
pub fn meminfo(buf: &mut [u8]) -> usize {
    unsafe { syscall2(SYS_MEMINFO, buf.as_mut_ptr() as u64, buf.len() as u64) as usize }
}

/// Read trace ring entries from a specific core into a byte buffer.
///
/// Returns the number of `TraceEntry`-sized records written on success.
/// Returns `u64::MAX` on invalid core ID or null buffer pointer.
/// Returns `0` when `buf` is zero-length.
///
/// If the kernel is built without the `trace` feature, syscall `0x1002` is
/// not registered and this returns the raw `-ENOSYS` value as `u64`.
pub fn ktrace(core_id: u32, buf: &mut [u8]) -> u64 {
    unsafe {
        syscall3(
            SYS_KTRACE,
            core_id as u64,
            buf.as_mut_ptr() as u64,
            buf.len() as u64,
        )
    }
}

/// Retrieves framebuffer metadata into `buf` (must be ≥ 20 bytes).
/// Returns 0 on success, negative errno on failure.
pub fn framebuffer_info(buf: &mut [u8]) -> isize {
    unsafe {
        syscall2(
            SYS_FRAMEBUFFER_INFO,
            buf.as_mut_ptr() as u64,
            buf.len() as u64,
        ) as isize
    }
}

/// Maps the framebuffer into the calling process's address space.
///
/// Returns the userspace virtual address of the mapped framebuffer on success.
/// On error, returns a large `u64` encoding a negative errno (e.g.
/// `(-22_i64) as u64` for EINVAL, `(-16_i64) as u64` for EBUSY).
/// Callers should check `ret > u64::MAX - 4096` to detect error values.
pub fn framebuffer_mmap() -> u64 {
    unsafe { syscall0(SYS_FRAMEBUFFER_MMAP) }
}

/// Reads one raw PS/2 scancode from the keyboard ring buffer.
/// Returns the scancode (0x01–0xFF), or 0 if no key is pending.
pub fn read_scancode() -> u64 {
    unsafe { syscall0(SYS_READ_SCANCODE) }
}

/// Push a single raw input byte through the kernel's line discipline.
///
/// The kernel processes the byte through TTY0's LineDiscipline (iflag
/// transforms, signal generation, canonical editing, echo) and delivers
/// the result to stdin.
pub fn push_raw_input(byte: u8) -> i64 {
    unsafe { syscall1(SYS_PUSH_RAW_INPUT, byte as u64) as i64 }
}

/// Write a string slice to a file descriptor.
pub fn write_str(fd: i32, s: &str) -> isize {
    write(fd, s.as_bytes())
}

/// `setsid()` — create a new session (syscall 112).
pub fn setsid() -> i64 {
    unsafe { syscall0(112) as i64 }
}

/// `getsid(pid)` — get session ID (syscall 124).
pub fn getsid(pid: u32) -> i64 {
    unsafe { syscall1(124, pid as u64) as i64 }
}

/// Open a PTY pair. Returns `Ok((master_fd, slave_fd))` or `Err(negative_errno)`.
/// Error values are negative (e.g., -5 for EIO), matching raw syscall convention.
pub fn openpty() -> Result<(i32, i32), i32> {
    // Open /dev/ptmx to allocate a new PTY pair.
    let master_fd = open(b"/dev/ptmx\0", 2, 0); // O_RDWR
    if master_fd < 0 {
        return Err(master_fd as i32);
    }
    let mfd = master_fd as i32;

    // Unlock the slave side.
    let zero: i32 = 0;
    let ret = ioctl(mfd, 0x40045431, &zero as *const _ as usize); // TIOCSPTLCK
    if ret < 0 {
        close(mfd);
        return Err(ret as i32);
    }

    // Get the PTY number.
    let mut pty_num: u32 = 0;
    let ret = ioctl(mfd, 0x80045430, &mut pty_num as *mut _ as usize); // TIOCGPTN
    if ret < 0 {
        close(mfd);
        return Err(ret as i32);
    }

    // Construct /dev/pts/N path.
    let mut path = [0u8; 32];
    let prefix = b"/dev/pts/";
    path[..prefix.len()].copy_from_slice(prefix);
    let mut pos = prefix.len();
    if pty_num == 0 {
        path[pos] = b'0';
        pos += 1;
    } else {
        let mut digits = [0u8; 10];
        let mut dpos = digits.len();
        let mut n = pty_num;
        while n > 0 {
            dpos -= 1;
            digits[dpos] = b'0' + (n % 10) as u8;
            n /= 10;
        }
        let len = digits.len() - dpos;
        path[pos..pos + len].copy_from_slice(&digits[dpos..]);
        pos += len;
    }
    path[pos] = 0; // null terminator

    let slave_fd = open(&path, 2, 0); // O_RDWR
    if slave_fd < 0 {
        close(mfd);
        return Err(slave_fd as i32);
    }

    Ok((mfd, slave_fd as i32))
}

// ===========================================================================
// Phase 32: stat and utimensat wrappers
// ===========================================================================

/// x86_64 Linux stat struct (144 bytes).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Stat {
    pub st_dev: u64,
    pub st_ino: u64,
    pub st_nlink: u64,
    pub st_mode: u32,
    pub st_uid: u32,
    pub st_gid: u32,
    pub __pad0: u32,
    pub st_rdev: u64,
    pub st_size: i64,
    pub st_blksize: i64,
    pub st_blocks: i64,
    pub st_atime: i64,
    pub st_atime_nsec: i64,
    pub st_mtime: i64,
    pub st_mtime_nsec: i64,
    pub st_ctime: i64,
    pub st_ctime_nsec: i64,
    pub __reserved: [i64; 3],
}

impl Stat {
    pub const fn zeroed() -> Self {
        Stat {
            st_dev: 0,
            st_ino: 0,
            st_nlink: 0,
            st_mode: 0,
            st_uid: 0,
            st_gid: 0,
            __pad0: 0,
            st_rdev: 0,
            st_size: 0,
            st_blksize: 0,
            st_blocks: 0,
            st_atime: 0,
            st_atime_nsec: 0,
            st_mtime: 0,
            st_mtime_nsec: 0,
            st_ctime: 0,
            st_ctime_nsec: 0,
            __reserved: [0; 3],
        }
    }
}

/// `stat(path, buf)` — get file metadata by path.
pub fn stat(path: &[u8], buf: &mut Stat) -> isize {
    // syscall 4 = stat (path, statbuf)
    unsafe { syscall2(4, path.as_ptr() as u64, buf as *mut Stat as u64) as isize }
}

/// `fstat(fd, buf)` — get file metadata by file descriptor.
pub fn fstat(fd: i32, buf: &mut Stat) -> isize {
    unsafe { syscall2(SYS_FSTAT, fd as u64, buf as *mut Stat as u64) as isize }
}

/// `utimensat(dirfd, path, times, flags)` — update file timestamps.
/// Sets atime to `atime_sec` and mtime to `mtime_sec` (seconds since epoch).
pub fn utimensat(path: &[u8], atime_sec: i64, mtime_sec: i64) -> isize {
    // struct timespec { tv_sec: i64, tv_nsec: i64 } × 2 = 32 bytes
    let times: [i64; 4] = [atime_sec, 0, mtime_sec, 0];
    unsafe {
        syscall4(
            SYS_UTIMENSAT,
            (-100_i64) as u64, // AT_FDCWD
            path.as_ptr() as u64,
            times.as_ptr() as u64,
            0,
        ) as isize
    }
}

/// `utimensat` with NULL times — set both to current time.
pub fn utimensat_now(path: &[u8]) -> isize {
    unsafe {
        syscall4(
            SYS_UTIMENSAT,
            (-100_i64) as u64, // AT_FDCWD
            path.as_ptr() as u64,
            0, // NULL times = set to now
            0,
        ) as isize
    }
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

// ===========================================================================
// Time functions (Phase 34)
// ===========================================================================

/// Call clock_gettime(clk_id). Returns (tv_sec, tv_nsec) or (-1, 0) on error.
pub fn clock_gettime(clk_id: u64) -> (i64, i64) {
    let mut ts = [0u8; 16];
    let ret = unsafe { syscall2(SYS_CLOCK_GETTIME, clk_id, ts.as_mut_ptr() as u64) } as i64;
    if ret < 0 {
        return (-1, 0);
    }
    let sec = i64::from_ne_bytes(ts[0..8].try_into().unwrap());
    let nsec = i64::from_ne_bytes(ts[8..16].try_into().unwrap());
    (sec, nsec)
}

/// Call gettimeofday(). Returns (tv_sec, tv_usec) or (-1, 0) on error.
pub fn gettimeofday() -> (i64, i64) {
    let mut tv = [0u8; 16];
    let ret = unsafe { syscall1(SYS_GETTIMEOFDAY, tv.as_mut_ptr() as u64) } as i64;
    if ret < 0 {
        return (-1, 0);
    }
    let sec = i64::from_ne_bytes(tv[0..8].try_into().unwrap());
    let usec = i64::from_ne_bytes(tv[8..16].try_into().unwrap());
    (sec, usec)
}

/// Broken-down date/time (UTC).
pub struct DateTime {
    pub year: u32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
    pub weekday: u32,
}

/// Convert Unix epoch seconds to broken-down UTC date/time.
pub fn gmtime(epoch_secs: u64) -> DateTime {
    let total_days = epoch_secs / 86400;
    let remaining = epoch_secs % 86400;
    let weekday = ((total_days + 4) % 7) as u32;
    let hour = (remaining / 3600) as u32;
    let minute = ((remaining % 3600) / 60) as u32;
    let second = (remaining % 60) as u32;

    let mut year = 1970u32;
    let mut days_left = total_days;
    loop {
        let dy: u64 =
            if (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400) {
                366
            } else {
                365
            };
        if days_left < dy {
            break;
        }
        days_left -= dy;
        year += 1;
    }

    let days_in_month = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let is_leap = (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400);
    let mut month = 1u32;
    for (i, &dm) in days_in_month.iter().enumerate() {
        let d = if i == 1 && is_leap { 29u64 } else { dm };
        if days_left < d {
            break;
        }
        days_left -= d;
        month += 1;
    }
    let day = days_left as u32 + 1;

    DateTime {
        year,
        month,
        day,
        hour,
        minute,
        second,
        weekday,
    }
}

const WEEKDAYS: [&[u8]; 7] = [b"Sun", b"Mon", b"Tue", b"Wed", b"Thu", b"Fri", b"Sat"];
const MONTHS: [&[u8]; 12] = [
    b"Jan", b"Feb", b"Mar", b"Apr", b"May", b"Jun", b"Jul", b"Aug", b"Sep", b"Oct", b"Nov", b"Dec",
];

/// Format a DateTime as "Wed Apr  1 12:30:00 UTC 2026" into the provided buffer.
/// Returns the number of bytes written.
pub fn format_datetime(dt: &DateTime, buf: &mut [u8]) -> usize {
    let mut pos = 0;

    let append = |buf: &mut [u8], pos: &mut usize, s: &[u8]| {
        for &b in s {
            if *pos < buf.len() {
                buf[*pos] = b;
                *pos += 1;
            }
        }
    };
    let append_u32_pad2 = |buf: &mut [u8], pos: &mut usize, v: u32| {
        if *pos + 1 < buf.len() {
            buf[*pos] = b'0' + (v / 10) as u8;
            buf[*pos + 1] = b'0' + (v % 10) as u8;
            *pos += 2;
        }
    };

    // "Wed "
    let wd = WEEKDAYS[dt.weekday as usize % 7];
    append(buf, &mut pos, wd);
    append(buf, &mut pos, b" ");

    // "Apr "
    let mn = MONTHS[(dt.month.wrapping_sub(1)) as usize % 12];
    append(buf, &mut pos, mn);
    append(buf, &mut pos, b" ");

    // " 1 " or "12 "
    if dt.day < 10 {
        append(buf, &mut pos, b" ");
    }
    // day as decimal
    if dt.day >= 10 && pos < buf.len() {
        buf[pos] = b'0' + (dt.day / 10) as u8;
        pos += 1;
    }
    if pos < buf.len() {
        buf[pos] = b'0' + (dt.day % 10) as u8;
        pos += 1;
    }
    append(buf, &mut pos, b" ");

    // "12:30:00"
    append_u32_pad2(buf, &mut pos, dt.hour);
    append(buf, &mut pos, b":");
    append_u32_pad2(buf, &mut pos, dt.minute);
    append(buf, &mut pos, b":");
    append_u32_pad2(buf, &mut pos, dt.second);

    // " UTC "
    append(buf, &mut pos, b" UTC ");

    // year — write up to 4 digits
    let y = dt.year;
    if y >= 1000 && pos < buf.len() {
        buf[pos] = b'0' + (y / 1000) as u8;
        pos += 1;
    }
    if y >= 100 && pos < buf.len() {
        buf[pos] = b'0' + ((y / 100) % 10) as u8;
        pos += 1;
    }
    if y >= 10 && pos < buf.len() {
        buf[pos] = b'0' + ((y / 10) % 10) as u8;
        pos += 1;
    }
    if pos < buf.len() {
        buf[pos] = b'0' + (y % 10) as u8;
        pos += 1;
    }

    append(buf, &mut pos, b"\n");
    pos
}

/// Fill a buffer with random bytes from the kernel's getrandom syscall.
/// Loops internally to handle partial reads (the kernel may cap per-call output).
/// Returns the number of bytes actually written (may be less than `buf.len()` if
/// the kernel returns 0), or a negative errno on failure.
pub fn getrandom(buf: &mut [u8]) -> isize {
    let mut filled = 0usize;
    while filled < buf.len() {
        let ret = unsafe {
            syscall3(
                SYS_GETRANDOM,
                buf[filled..].as_mut_ptr() as u64,
                (buf.len() - filled) as u64,
                0,
            ) as isize
        };
        if ret < 0 {
            return ret;
        }
        let written = ret as usize;
        if written == 0 {
            break;
        }
        filled += written;
    }
    filled as isize
}
