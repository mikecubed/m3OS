//! Echo-args: reads argc/argv from the SysV ABI stack and prints them.
//!
//! Validation: P11-T020 — program reads argc/argv via ABI, writes to serial.
//!
//! The SysV AMD64 ABI at process entry:
//!   [rsp]     = argc
//!   [rsp+8]   = argv[0] pointer
//!   ...
//!   [rsp+8*(1+argc)] = NULL
#![no_std]
#![no_main]

use syscall_lib::{exit, serial_print};

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    // Read argc and argv from the initial stack frame.
    // On entry, rsp points to argc.  We capture it via inline asm.
    let stack_ptr: *const u64;
    unsafe {
        core::arch::asm!(
            "mov {}, rsp",
            out(reg) stack_ptr,
            options(nomem, nostack, preserves_flags),
        );
    }

    let argc = unsafe { stack_ptr.read() } as usize;
    let argv_base = unsafe { stack_ptr.add(1) }; // argv[0] is right after argc

    serial_print("echo-args: argc=");
    print_usize(argc);
    serial_print("\n");

    for i in 0..argc {
        let arg_ptr = unsafe { argv_base.add(i).read() } as *const u8;
        if arg_ptr.is_null() {
            break;
        }
        // Find null terminator; cap at 4096 bytes to avoid faulting on
        // unterminated strings. Bound is checked BEFORE reading.
        let mut len = 0usize;
        while len < 4096 && unsafe { arg_ptr.add(len).read() } != 0 {
            len += 1;
        }
        serial_print("echo-args: argv[");
        print_usize(i);
        serial_print("]=");
        let s = unsafe { core::slice::from_raw_parts(arg_ptr, len) };
        if let Ok(str_val) = core::str::from_utf8(s) {
            serial_print(str_val);
        }
        serial_print("\n");
    }

    exit(0)
}

fn print_usize(n: usize) {
    let mut buf = [0u8; 20];
    let mut pos = 20usize;
    if n == 0 {
        serial_print("0");
        return;
    }
    let mut v = n;
    while v > 0 {
        pos -= 1;
        buf[pos] = b'0' + (v % 10) as u8;
        v /= 10;
    }
    if let Ok(s) = core::str::from_utf8(&buf[pos..]) {
        serial_print(s);
    }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    exit(100)
}
