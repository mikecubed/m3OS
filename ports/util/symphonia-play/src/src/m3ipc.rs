//! Raw m3OS IPC syscalls from a musl-`std` binary.
//!
//! The kernel's syscall dispatch is ONE flat table with no personality
//! gate — Linux-numbered calls (read=0, write=1, …) and the m3OS-native
//! kernel-extension range (0x1000+) dispatch identically for every
//! process (`kernel/src/arch/x86_64/syscall/mod.rs`, flat `match number`).
//! The register convention is byte-identical to Linux x86_64. These
//! wrappers therefore mirror `userspace/syscall-lib/src/lib.rs`
//! (`syscall2`/`syscall6` + the three IPC entry points) — syscall-lib
//! itself is a `x86_64-unknown-none` workspace crate a musl crate cannot
//! link, so the numbers are re-declared here with provenance:
//!
//! - `SYS_IPC_LOOKUP_SERVICE   = 0x1109` (syscall-lib:374)
//! - `SYS_IPC_CALL_BUF         = 0x110D` (syscall-lib:386)
//! - `SYS_IPC_TAKE_PENDING_BULK= 0x1112` (syscall-lib:403)

use std::arch::asm;

const SYS_IPC_LOOKUP_SERVICE: u64 = 0x1109;
const SYS_IPC_CALL_BUF: u64 = 0x110D;
const SYS_IPC_TAKE_PENDING_BULK: u64 = 0x1112;
/// Linux `nanosleep` — implemented by m3OS (syscall-lib:205). NOTE:
/// `std::thread::sleep` cannot be used in this binary: Rust std sleeps
/// via `clock_nanosleep` (Linux 230), which m3OS does NOT implement —
/// std `assert!`s on the ENOSYS return and panics (found live by
/// symphonia-smoke). All retry backoffs must go through
/// [`nanosleep_ms`].
const SYS_NANOSLEEP: u64 = 35;

#[inline]
unsafe fn syscall2(num: u64, a0: u64, a1: u64) -> u64 {
    let mut rax = num;
    unsafe {
        asm!(
            "syscall",
            inlateout("rax") rax,
            in("rdi") a0,
            in("rsi") a1,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }
    rax
}

#[inline]
#[allow(clippy::too_many_arguments)]
unsafe fn syscall6(num: u64, a0: u64, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64) -> u64 {
    let mut rax = num;
    unsafe {
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
    }
    rax
}

/// Look up a registered IPC service; returns an endpoint cap handle or
/// `u64::MAX` on failure (mirrors `syscall_lib::ipc_lookup_service`).
pub fn ipc_lookup_service(name: &str) -> u64 {
    unsafe {
        syscall2(
            SYS_IPC_LOOKUP_SERVICE,
            name.as_ptr() as u64,
            name.len() as u64,
        )
    }
}

/// Synchronous IPC call carrying `buf` as the bulk payload (mirrors
/// `syscall_lib::ipc_call_buf`). Returns the reply label, or `u64::MAX`
/// on failure.
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

/// Drain the staged reply bulk into `buf` (mirrors
/// `syscall_lib::ipc_take_pending_bulk`). Returns bytes written, or
/// `u64::MAX` on failure.
pub fn ipc_take_pending_bulk(buf: &mut [u8]) -> u64 {
    unsafe {
        syscall2(
            SYS_IPC_TAKE_PENDING_BULK,
            buf.as_mut_ptr() as u64,
            buf.len() as u64,
        )
    }
}

/// Sleep via the kernel's `nanosleep` (timespec pointer, rem ignored) —
/// the m3OS-safe replacement for `std::thread::sleep` (see the
/// `SYS_NANOSLEEP` note above; mirrors `syscall_lib::nanosleep_for`).
pub fn nanosleep_ms(ms: u64) {
    let ts: [i64; 2] = [(ms / 1000) as i64, ((ms % 1000) * 1_000_000) as i64];
    unsafe {
        syscall2(SYS_NANOSLEEP, ts.as_ptr() as u64, 0);
    }
}
