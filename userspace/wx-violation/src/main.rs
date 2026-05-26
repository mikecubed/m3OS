//! Phase 75 Track G.1 — `wx-violation`.
//!
//! Userspace regression for the Phase 75 W^X enforcement work:
//!
//! 1. Anonymous `mmap(PROT_READ | PROT_WRITE)` succeeds and produces a
//!    writable, non-executable page.
//! 2. `mprotect(PROT_WRITE | PROT_EXEC)` on that page is rejected with
//!    `EINVAL` (the new `sys_mprotect` guard added in
//!    `kernel/src/arch/x86_64/syscall/mod.rs`).
//! 3. `mprotect(PROT_READ | PROT_EXEC)` succeeds — the supported JIT
//!    pattern (allocate RW-, write code, then flip to R-X).
//!
//! On success prints `WX_VIOLATION:smoke:ok` and exits 0. An assertion
//! failure prints `WX_VIOLATION:fail <reason>` and exits 2. A panic
//! prints `WX_VIOLATION:fail panic` and exits 101 (distinct sentinel so
//! a panic and a normal assertion failure are distinguishable in serial
//! output).
//!
//! Wired into `cargo xtask smoke-test` via the `smoke-runner`
//! `wx-violation` stage; `smoke-runner` execs `/bin/wx-violation` and
//! asserts the `WX_VIOLATION:smoke:ok` marker is present.

#![no_std]
#![no_main]

use core::arch::asm;
use syscall_lib::{STDOUT_FILENO, exit, write};

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    let _ = write(STDOUT_FILENO, b"WX_VIOLATION:fail panic\n");
    exit(101)
}

fn fail(reason: &[u8]) -> ! {
    let _ = write(STDOUT_FILENO, b"WX_VIOLATION:fail ");
    let _ = write(STDOUT_FILENO, reason);
    let _ = write(STDOUT_FILENO, b"\n");
    exit(2)
}

// ---------------------------------------------------------------------------
// Local syscall numbers / constants. Mirrors
// `kernel/src/arch/x86_64/syscall/mod.rs` and standard POSIX.
// ---------------------------------------------------------------------------

const SYS_MMAP: u64 = 9;
const SYS_MPROTECT: u64 = 10;
const SYS_MUNMAP: u64 = 11;

const PROT_READ: u64 = 0x1;
const PROT_WRITE: u64 = 0x2;
const PROT_EXEC: u64 = 0x4;

const MAP_PRIVATE: u64 = 0x02;
const MAP_ANONYMOUS: u64 = 0x20;

const EINVAL_NEG: i64 = -22;

const PAGE: u64 = 4096;

/// Raw `mmap` — the kernel reads `flags`/`fd`/`offset` from r10/r8/r9
/// (per `sys_linux_mmap`'s `per_core_syscall_arg3` / r8 / r9 reads).
unsafe fn raw_mmap(addr_hint: u64, len: u64, prot: u64, flags: u64, fd: u64, offset: u64) -> u64 {
    unsafe {
        let mut rax = SYS_MMAP;
        asm!(
            "syscall",
            inlateout("rax") rax,
            in("rdi") addr_hint,
            in("rsi") len,
            in("rdx") prot,
            in("r10") flags,
            in("r8") fd,
            in("r9") offset,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
        rax
    }
}

unsafe fn raw_mprotect(addr: u64, len: u64, prot: u64) -> u64 {
    unsafe {
        let mut rax = SYS_MPROTECT;
        asm!(
            "syscall",
            inlateout("rax") rax,
            in("rdi") addr,
            in("rsi") len,
            in("rdx") prot,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
        rax
    }
}

unsafe fn raw_munmap(addr: u64, len: u64) -> u64 {
    unsafe {
        let mut rax = SYS_MUNMAP;
        asm!(
            "syscall",
            inlateout("rax") rax,
            in("rdi") addr,
            in("rsi") len,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
        rax
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    let _ = write(STDOUT_FILENO, b"WX_VIOLATION:smoke:begin\n");

    // 1. Allocate one anonymous RW page.
    let base = unsafe {
        raw_mmap(
            0,
            PAGE,
            PROT_READ | PROT_WRITE,
            MAP_PRIVATE | MAP_ANONYMOUS,
            u64::MAX, // fd ignored for MAP_ANONYMOUS, but pass an obviously-invalid value
            0,
        )
    };
    // Errors are returned in [-4095, -1]; success returns a userspace vaddr.
    if (base as i64) < 0 {
        fail(b"mmap-rw-failed");
    }

    // 2. Touch the page so the demand-fault path runs while the VMA is
    //    still PROT_READ|PROT_WRITE — proves the W^X-rejected mprotect
    //    below does not happen because the page hasn't been faulted in.
    unsafe {
        (base as *mut u8).write_volatile(0xAB);
    }

    // 3. Negative case — `mprotect(PROT_WRITE | PROT_EXEC)` must be
    //    rejected with EINVAL by the Phase 75 guard.
    let rc_wx = unsafe { raw_mprotect(base, PAGE, PROT_WRITE | PROT_EXEC) } as i64;
    if rc_wx != EINVAL_NEG {
        fail(b"mprotect-rwx-not-einval");
    }

    // 4. After the rejection the page must still be readable + writable
    //    (the guard runs before any PTE mutation, so no permission
    //    downgrade happened).
    let after_byte = unsafe { (base as *const u8).read_volatile() };
    if after_byte != 0xAB {
        fail(b"page-readback-after-reject");
    }
    unsafe {
        (base as *mut u8).write_volatile(0xCD);
    }

    // 5. Positive case — the JIT pattern: flip to R-X. Must succeed
    //    (rax = 0).
    let rc_rx = unsafe { raw_mprotect(base, PAGE, PROT_READ | PROT_EXEC) };
    if rc_rx != 0 {
        fail(b"mprotect-rx-failed");
    }

    // 6. Cleanup — best effort; failure does not invalidate the test.
    let _ = unsafe { raw_munmap(base, PAGE) };

    let _ = write(STDOUT_FILENO, b"WX_VIOLATION:smoke:ok\n");
    exit(0)
}
