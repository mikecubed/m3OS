//! Common `_start` entry point for `no_std` Rust userspace binaries.
//!
//! The SysV AMD64 ABI places `[argc, argv[0], ..., NULL, envp[0], ..., NULL]`
//! at the process entry stack pointer.  A normal (non-naked) `_start` has a
//! compiler-generated prologue that moves `rsp` before user code runs, making
//! it impossible to read `argc`/`argv` reliably with inline asm.
//!
//! This module provides a `#[unsafe(naked)]` `_start` that captures the
//! pristine `rsp` and passes it to a Rust function that parses it into
//! `argc` and `argv`.
//!
//! # Usage
//!
//! ```rust,ignore
//! syscall_lib::entry_point!(my_main);
//!
//! fn my_main(args: &[&str]) -> i32 {
//!     // args[0] is the program name, args[1..] are arguments.
//!     0 // exit code
//! }
//! ```

/// Maximum number of command-line arguments supported.
const MAX_ARGS: usize = 32;

/// Maximum length of a single argument (bytes).
const MAX_ARG_LEN: usize = 4096;

/// Parse the initial stack into argc/argv, build `&[&str]`, and call `main_fn`.
///
/// # Safety
///
/// `stack_ptr` must point to the original process entry stack (argc at `*stack_ptr`).
pub unsafe fn run_main(stack_ptr: *const u64, main_fn: fn(&[&str]) -> i32) -> ! {
    let argc = unsafe { *stack_ptr } as usize;
    let argv_base = unsafe { stack_ptr.add(1) } as *const *const u8;

    let mut arg_strs: [&str; MAX_ARGS] = [""; MAX_ARGS];
    let count = argc.min(MAX_ARGS);

    for i in 0..count {
        let ptr = unsafe { *argv_base.add(i) };
        if ptr.is_null() {
            break;
        }
        let mut len = 0;
        while len < MAX_ARG_LEN && unsafe { *ptr.add(len) } != 0 {
            len += 1;
        }
        let bytes = unsafe { core::slice::from_raw_parts(ptr, len) };
        if let Ok(s) = core::str::from_utf8(bytes) {
            arg_strs[i] = s;
        }
    }

    let code = main_fn(&arg_strs[..count]);
    crate::exit(code)
}

/// Declares a `_start` entry point that parses argc/argv and calls your main function.
///
/// The main function receives `&[&str]` (argv as string slices) and returns an `i32` exit code.
///
/// # Example
///
/// ```rust,ignore
/// syscall_lib::entry_point!(my_main);
///
/// fn my_main(args: &[&str]) -> i32 {
///     0
/// }
/// ```
#[macro_export]
macro_rules! entry_point {
    ($main_fn:path) => {
        #[unsafe(naked)]
        #[unsafe(no_mangle)]
        pub extern "C" fn _start() -> ! {
            core::arch::naked_asm!(
                "mov rdi, rsp",
                "call {entry}",
                entry = sym _m3os_entry,
            );
        }

        fn _m3os_entry(stack_ptr: *const u64) -> ! {
            unsafe { $crate::start::run_main(stack_ptr, $main_fn) }
        }
    };
}
