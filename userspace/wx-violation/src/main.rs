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
//! 4. (Phase 90a C.1) `mmap(PROT_READ | PROT_WRITE | PROT_EXEC)` is
//!    rejected with `EINVAL` at mmap entry — `mmap` carries no pkey, so a
//!    W+X mmap must fail closed rather than demand-fault into a live
//!    unguarded W+X PTE. Guards the C.1 mmap(W+X) bypass.
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
//!
//! Syscalls route through `syscall_lib::{syscall2, syscall3, syscall6}`
//! and the centrally-defined `SYS_MMAP` / `SYS_MPROTECT` / `SYS_MUNMAP`
//! constants — no local inline-asm wrappers or duplicate syscall
//! numbers.

#![no_std]
#![no_main]

use syscall_lib::{
    STDOUT_FILENO, SYS_MMAP, SYS_MPROTECT, SYS_MUNMAP, exit, syscall2, syscall3, syscall6, write,
};

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

const PROT_READ: u64 = 0x1;
const PROT_WRITE: u64 = 0x2;
const PROT_EXEC: u64 = 0x4;

const MAP_PRIVATE: u64 = 0x02;
const MAP_ANONYMOUS: u64 = 0x20;

const EINVAL_NEG: i64 = -22;

const PAGE: u64 = 4096;

// Phase 86f FIX 2: naked _start trampoline.  This binary ignores argv/envp.
#[unsafe(naked)]
#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    core::arch::naked_asm!(
        "xor rbp, rbp",
        "call {f}",
        f = sym wx_violation_main,
    );
}

fn wx_violation_main() -> ! {
    let _ = write(STDOUT_FILENO, b"WX_VIOLATION:smoke:begin\n");

    // 1. Allocate one anonymous RW page. fd is ignored for MAP_ANONYMOUS,
    //    but pass an obviously-invalid value.
    let base = unsafe {
        syscall6(
            SYS_MMAP,
            0,
            PAGE,
            PROT_READ | PROT_WRITE,
            MAP_PRIVATE | MAP_ANONYMOUS,
            u64::MAX,
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
    let rc_wx = unsafe { syscall3(SYS_MPROTECT, base, PAGE, PROT_WRITE | PROT_EXEC) } as i64;
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
    let rc_rx = unsafe { syscall3(SYS_MPROTECT, base, PAGE, PROT_READ | PROT_EXEC) };
    if rc_rx != 0 {
        fail(b"mprotect-rx-failed");
    }

    // 6. Negative case — `mmap(PROT_READ | PROT_WRITE | PROT_EXEC)` must be
    //    rejected with EINVAL by the Phase 90a C.1 guard (contract clause 1).
    //    `mmap` carries no pkey argument, so it can only express the unguarded
    //    key-0 case; a W+X mmap must fail closed at entry rather than
    //    demand-fault into a live unguarded W+X PTE. Guards the C.1 mmap(W+X)
    //    bypass against regression.
    let rc_mmap_wx = unsafe {
        syscall6(
            SYS_MMAP,
            0,
            PAGE,
            PROT_READ | PROT_WRITE | PROT_EXEC,
            MAP_PRIVATE | MAP_ANONYMOUS,
            u64::MAX,
            0,
        )
    } as i64;
    // The kernel returns the same errno as the mprotect W+X reject (EINVAL).
    // Accept any negative (MAP_FAILED-class) return, but require it be an
    // error — a non-negative vaddr means the W+X mapping was honored (bypass).
    if rc_mmap_wx >= 0 {
        // Best-effort unmap so a failed assertion does not leak the bad page.
        let _ = unsafe { syscall2(SYS_MUNMAP, rc_mmap_wx as u64, PAGE) };
        fail(b"mmap-wx-not-rejected");
    }
    if rc_mmap_wx != EINVAL_NEG {
        fail(b"mmap-wx-not-einval");
    }

    // 7. Cleanup — best effort; failure does not invalidate the test.
    let _ = unsafe { syscall2(SYS_MUNMAP, base, PAGE) };

    let _ = write(STDOUT_FILENO, b"WX_VIOLATION:smoke:ok\n");
    exit(0)
}
