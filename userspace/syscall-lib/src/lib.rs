//! Minimal syscall wrappers for Phase 11 userspace programs.
//!
//! Syscall ABI (see kernel/src/arch/x86_64/syscall.rs):
//!   rax = number
//!   rdi, rsi, rdx = args 0-2
//!   return value in rax
#![no_std]

use core::arch::asm;

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

// Syscall numbers (kernel/src/arch/x86_64/syscall.rs)
pub const SYS_EXIT: u64 = 60;
pub const SYS_FORK: u64 = 57;
pub const SYS_WAITPID: u64 = 61;
pub const SYS_GETPID: u64 = 39;
pub const SYS_GETPPID: u64 = 110;
pub const SYS_EXECVE: u64 = 59;
pub const SYS_DEBUG_PRINT: u64 = 12;

/// Write a UTF-8 string to the kernel serial log.
pub fn serial_print(s: &str) {
    unsafe {
        syscall2(SYS_DEBUG_PRINT, s.as_ptr() as u64, s.len() as u64);
    }
}

/// Terminate the current process with the given exit code.
pub fn exit(code: i32) -> ! {
    unsafe {
        syscall1(SYS_EXIT, code as u64);
    }
    loop {}
}
