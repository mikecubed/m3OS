//! m3OS dynamic linker (`ld-musl-x86_64.so.1`) — Phase 76 scaffolding.
//!
//! This crate produces a `no_std` PIE ELF that the kernel maps when a
//! binary carries a `PT_INTERP` segment. In Phase 76 the linker is
//! intentionally a **transfer-only stub**: it walks the SysV-ABI
//! initial stack for `AT_ENTRY`, prints a single observability line
//! to serial, and `jmp`s to the main binary's entry. Real
//! `DT_NEEDED` resolution, relocation application, constructor
//! running, and `dlopen`/`dlsym`/`dlclose` ship in Phases 76b–76d.
//!
//! ## Why a stub is the right shape for Phase 76
//!
//! Cramming the kernel `PT_INTERP` branch and a real bring-up linker
//! into one PR would push the diff past reviewable size and force the
//! smoke gate to wait until the entire stack is bottom-up correct.
//! Phase 76's stub validates the kernel → ld.so → main binary handoff
//! in isolation: the auxv layout matches what musl `_dlstart` would
//! expect, the interpreter loads at a sane bias, and control reaches
//! `AT_ENTRY` exactly once. Subsequent subphases grow the linker on
//! top of this proven foundation.
//!
//! ## Stack shape on entry
//!
//! When the kernel transfers to `_dlstart`, `rsp` points at the SysV
//! AMD64 ABI initial stack:
//!
//! ```text
//! [rsp + 0]                  argc                       u64
//! [rsp + 8 .. 8+8*argc]      argv[0..argc]              *const u8
//! [rsp + 8+8*argc]           NULL terminator            *const u8
//! [...]                      envp[..]                   *const u8
//! [...]                      NULL terminator            *const u8
//! [...]                      auxv[..] (16-byte slots)   AuxEntry
//! [...]                      AT_NULL sentinel {0, 0}    AuxEntry
//! [...]                      string region              raw bytes
//! ```
//!
//! `rsp` is 16-byte aligned at this point (SysV-ABI requirement for
//! `_dlstart`).

#![no_std]
#![no_main]
#![deny(unsafe_op_in_unsafe_fn)]

use core::arch::naked_asm;
use core::panic::PanicInfo;

// ---------------------------------------------------------------------------
// AT_* constants — kept private so this file is self-contained. Phase 76b
// will route through `kernel_core::elf::auxv` for sharing.
// ---------------------------------------------------------------------------

const AT_NULL: u64 = 0;
const AT_ENTRY: u64 = 9;
const AT_BASE: u64 = 7;

// ---------------------------------------------------------------------------
// Raw syscalls — ld.so cannot link `syscall_lib` (it uses `BrkAllocator`,
// which would touch the heap before the main binary has had a chance to
// initialize). Phase 76b will introduce a no-alloc subset of `syscall_lib`
// that the linker can share.
// ---------------------------------------------------------------------------

const SYS_WRITE: u64 = 1;

/// `write(fd, buf, len)` — returns bytes written or negative errno.
unsafe fn sys_write(fd: i32, buf: *const u8, len: usize) -> i64 {
    let ret: i64;
    unsafe {
        core::arch::asm!(
            "syscall",
            in("rax") SYS_WRITE,
            in("rdi") fd as i64,
            in("rsi") buf,
            in("rdx") len,
            out("rcx") _,
            out("r11") _,
            lateout("rax") ret,
            options(nostack, preserves_flags),
        );
    }
    ret
}

/// Write a fixed string to fd 2 (stderr / serial console).
fn serial(msg: &[u8]) {
    // Best-effort — if the syscall fails there is no recovery path
    // available to us at this stage.
    unsafe {
        let _ = sys_write(2, msg.as_ptr(), msg.len());
    }
}

/// Write a `u64` in hex (no `0x` prefix, no padding).
fn serial_hex(mut value: u64) {
    let mut buf = [0u8; 16];
    let mut i = buf.len();
    if value == 0 {
        serial(b"0");
        return;
    }
    while value > 0 {
        let nibble = (value & 0xF) as u8;
        i -= 1;
        buf[i] = if nibble < 10 {
            b'0' + nibble
        } else {
            b'a' + (nibble - 10)
        };
        value >>= 4;
    }
    serial(&buf[i..]);
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// SysV-ABI entry point. The kernel transfers here with `rsp` pointing
/// at `argc` and 16-byte alignment. We pass `rsp` to `dlstart_rust`
/// which walks the auxv for `AT_ENTRY` and returns its value; we then
/// `jmp` to it with the stack untouched so the main binary's `_start`
/// sees exactly what it would have seen if loaded directly.
///
/// `rbp` is zeroed per SysV-ABI convention (the outermost frame).
/// SysV entry point. Named `_start` so rust-lld defaults to it as the
/// ELF `e_entry`. musl calls the conceptually-equivalent symbol
/// `_dlstart` — preserved here as an alias for cross-reference, see
/// `musl/arch/x86_64/crt_arch.h`.
#[unsafe(no_mangle)]
#[unsafe(naked)]
pub extern "C" fn _start() -> ! {
    naked_asm!(
        // Outermost frame: zero rbp.
        "xor rbp, rbp",
        // Pass rsp to Rust handler.
        "mov rdi, rsp",
        "call {dlstart_rust}",
        // dlstart_rust returns AT_ENTRY in rax. Jump there leaving
        // the stack unchanged.
        "jmp rax",
        dlstart_rust = sym dlstart_rust,
    );
}

/// musl-style alias for `_start`. Phase 76b's bring-up linker may
/// keep this name as the public entry; for now it just forwards.
#[unsafe(no_mangle)]
#[unsafe(naked)]
pub extern "C" fn _dlstart() -> ! {
    naked_asm!("jmp _start");
}

/// Walk the SysV-ABI initial stack for `AT_ENTRY` and return its
/// value. Called from `_dlstart` with `stack` pointing at `argc`.
///
/// # Safety
/// `stack` must be the genuine kernel-built SysV stack. We trust the
/// kernel's `setup_abi_stack_with_envp` to terminate every list
/// (argv, envp, auxv) correctly.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dlstart_rust(stack: *const u64) -> u64 {
    serial(b"ldso: _dlstart entry=");
    // SAFETY: stack came from the kernel-built SysV layout; the
    // caller (`_dlstart`) hands us the exact pointer the kernel set
    // as the initial `rsp`.
    let entry = unsafe { find_at_entry(stack) };
    serial_hex(entry);
    serial(b"\n");
    entry
}

/// Walk argc + argv + envp + auxv to find `AT_ENTRY`. Returns 0 if
/// not found (which would indicate a kernel bug — Phase 76's auxv
/// always emits `AT_ENTRY` when `PT_INTERP` was honored).
///
/// # Safety
/// `stack` must point at a SysV-ABI initial stack laid out by the
/// kernel `setup_abi_stack_with_envp` path. The argv and envp lists
/// must be `NULL`-terminated and the auxv must be `AT_NULL`-terminated.
unsafe fn find_at_entry(stack: *const u64) -> u64 {
    unsafe {
        // [rsp + 0]: argc
        let argc = *stack as usize;
        // Skip argc.
        let mut p = stack.add(1);
        // Skip argv[0..argc] then the NULL terminator.
        p = p.add(argc).add(1);
        // Skip envp[..] until NULL.
        while *p != 0 {
            p = p.add(1);
        }
        // Skip envp NULL terminator.
        p = p.add(1);
        // auxv entries: 16 bytes each (a_type, a_val). AT_NULL ends.
        loop {
            let a_type = *p;
            let a_val = *p.add(1);
            if a_type == AT_NULL {
                serial(b"ldso: WARN AT_ENTRY missing\n");
                return 0;
            }
            if a_type == AT_ENTRY {
                return a_val;
            }
            // AT_BASE is harmless to see; mention it once for
            // observability so the smoke log shows the linker
            // recognized its own load bias.
            if a_type == AT_BASE {
                serial(b"ldso: AT_BASE=");
                serial_hex(a_val);
                serial(b"\n");
            }
            p = p.add(2);
        }
    }
}

// ---------------------------------------------------------------------------
// Panic handler — there is nothing the linker can do at this stage;
// crash hard so the kernel sees a SIGSEGV and reports it.
// ---------------------------------------------------------------------------

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    serial(b"ldso: PANIC\n");
    let _ = info;
    // SAFETY: `ud2` deliberately raises #UD so the kernel observes
    // an illegal instruction and terminates the process. `noreturn`
    // tells rustc this asm never returns control.
    unsafe {
        core::arch::asm!("ud2", options(noreturn));
    }
}
