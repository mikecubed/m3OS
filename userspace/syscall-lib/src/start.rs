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

/// Maximum number of environment variables supported.
const MAX_ENVS: usize = 64;

/// Parse a null-terminated C string pointer into a `&str`, up to `max_len` bytes.
///
/// Returns `None` if the pointer is null or the bytes are not valid UTF-8.
unsafe fn parse_cstr<'a>(ptr: *const u8, max_len: usize) -> Option<&'a str> {
    if ptr.is_null() {
        return None;
    }
    let mut len = 0;
    while len < max_len && unsafe { *ptr.add(len) } != 0 {
        len += 1;
    }
    let bytes = unsafe { core::slice::from_raw_parts(ptr, len) };
    core::str::from_utf8(bytes).ok()
}

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

    let mut parsed = 0;
    for i in 0..count {
        let ptr = unsafe { *argv_base.add(i) };
        if ptr.is_null() {
            break;
        }
        if let Some(s) = unsafe { parse_cstr(ptr, MAX_ARG_LEN) } {
            arg_strs[parsed] = s;
            parsed += 1;
        }
    }

    let code = main_fn(&arg_strs[..parsed]);
    crate::exit(code)
}

/// Parse the initial stack into argc/argv/envp, and call `main_fn`.
///
/// # Safety
///
/// `stack_ptr` must point to the original process entry stack (argc at `*stack_ptr`).
pub unsafe fn run_main_with_env(stack_ptr: *const u64, main_fn: fn(&[&str], &[&str]) -> i32) -> ! {
    let argc = unsafe { *stack_ptr } as usize;
    let argv_base = unsafe { stack_ptr.add(1) } as *const *const u8;

    // Parse argv.
    let mut arg_strs: [&str; MAX_ARGS] = [""; MAX_ARGS];
    let count = argc.min(MAX_ARGS);
    let mut parsed_args = 0;
    for i in 0..count {
        let ptr = unsafe { *argv_base.add(i) };
        if ptr.is_null() {
            break;
        }
        if let Some(s) = unsafe { parse_cstr(ptr, MAX_ARG_LEN) } {
            arg_strs[parsed_args] = s;
            parsed_args += 1;
        }
    }

    // envp starts after argv's NULL terminator: stack_ptr + 1 + argc + 1
    let envp_base = unsafe { stack_ptr.add(1 + argc + 1) } as *const *const u8;
    let mut env_strs: [&str; MAX_ENVS] = [""; MAX_ENVS];
    let mut parsed_envs = 0;
    for i in 0..MAX_ENVS {
        let ptr = unsafe { *envp_base.add(i) };
        if ptr.is_null() {
            break;
        }
        if let Some(s) = unsafe { parse_cstr(ptr, MAX_ARG_LEN) } {
            env_strs[parsed_envs] = s;
            parsed_envs += 1;
        }
    }

    let code = main_fn(&arg_strs[..parsed_args], &env_strs[..parsed_envs]);
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

/// Declares a `_start` entry point that parses argc/argv/envp and calls your main function.
///
/// The main function receives `(&[&str], &[&str])` — (argv, envp) — and returns an `i32` exit code.
/// Each envp entry is a `"KEY=value"` string.
///
/// # Example
///
/// ```rust,ignore
/// syscall_lib::entry_point_with_env!(my_main);
///
/// fn my_main(args: &[&str], env: &[&str]) -> i32 {
///     0
/// }
/// ```
#[macro_export]
macro_rules! entry_point_with_env {
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
            unsafe { $crate::start::run_main_with_env(stack_ptr, $main_fn) }
        }
    };
}
