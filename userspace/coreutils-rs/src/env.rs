//! env — run a program in a modified environment, or print the environment.
//!
//! Usage: env [-] [-i] [NAME=VALUE]... [COMMAND [ARG]...]
//!
//! With a COMMAND, env PATH-searches it, applies any leading `NAME=VALUE`
//! assignments to the inherited environment, and execs it — this is the path the
//! `#!/usr/bin/env <interp>` shebang rides (e.g. npm's `#!/usr/bin/env node`).
//! With no COMMAND, env prints the environment (one `NAME=VALUE` per line).
#![no_std]
#![no_main]
// Raw pointers into fixed stack buffers are handed to execve; valid until the
// exec replaces the (single-threaded) process image.
#![allow(static_mut_refs)]

#[path = "common.rs"]
mod common;

use common::{eprintln, exec_with_path_search, print};
use syscall_lib::{O_RDONLY, close, open, read};

syscall_lib::entry_point_with_env!(main);

fn main(args: &[&str], env: &[&str]) -> i32 {
    // args[0] is the program name (for a shebang, the literal `/usr/bin/env`).
    let rest: &[&str] = if args.len() > 1 {
        &args[1..]
    } else {
        &[] as &[&str]
    };

    // Skip a leading `-`/`-i` (env-clearing not implemented) and collect leading
    // `NAME=VALUE` assignments; the first remaining token is the COMMAND.
    let mut idx = 0usize;
    while idx < rest.len() {
        let a = rest[idx];
        if idx == 0 && (a == "-" || a == "-i") {
            idx += 1;
            continue;
        }
        if !a.starts_with('-') && a.as_bytes().contains(&b'=') {
            idx += 1;
            continue;
        }
        break;
    }

    if idx >= rest.len() {
        // No COMMAND: print the environment (plus any standalone assignments).
        for e in env {
            print(e);
            print("\n");
        }
        for a in &rest[..idx] {
            if a.as_bytes().contains(&b'=') {
                print(a);
                print("\n");
            }
        }
        return 0;
    }

    let assignments = &rest[..idx];
    let cmd_args = &rest[idx..]; // [COMMAND, ARG...]
    let cmd = cmd_args[0];

    // --- argv: pack cmd_args as NUL-terminated C strings, record pointers ---
    let mut argv_buf = [0u8; 4096];
    let mut argv_ptrs = [core::ptr::null::<u8>(); 98];
    let mut bpos = 0usize;
    let mut argc = 0usize;
    for &a in cmd_args {
        if argc >= 97 {
            break;
        }
        let b = a.as_bytes();
        if bpos + b.len() + 1 > argv_buf.len() {
            break;
        }
        argv_ptrs[argc] = argv_buf[bpos..].as_ptr();
        argv_buf[bpos..bpos + b.len()].copy_from_slice(b);
        bpos += b.len();
        argv_buf[bpos] = 0;
        bpos += 1;
        argc += 1;
    }

    // --- envp: inherited /proc/self/environ, then appended assignments ---
    let mut envp_buf = [0u8; 8192];
    let mut epos = 0usize;
    let fd = open(b"/proc/self/environ\0", O_RDONLY, 0);
    if fd >= 0 {
        let n = read(fd as i32, &mut envp_buf);
        close(fd as i32);
        if n > 0 {
            epos = (n as usize).min(envp_buf.len());
        }
    }
    for &a in assignments {
        let b = a.as_bytes();
        if !b.contains(&b'=') || epos + b.len() + 1 > envp_buf.len() {
            continue;
        }
        envp_buf[epos..epos + b.len()].copy_from_slice(b);
        epos += b.len();
        envp_buf[epos] = 0;
        epos += 1;
    }
    let mut envp_ptrs = [core::ptr::null::<u8>(); 256];
    let mut envc = 0usize;
    let mut i = 0usize;
    while i < epos && envc < 255 {
        let start = i;
        while i < epos && envp_buf[i] != 0 {
            i += 1;
        }
        if i > start {
            envp_ptrs[envc] = envp_buf[start..].as_ptr();
            envc += 1;
        }
        i += 1; // skip the NUL
    }

    // --- COMMAND as a NUL-terminated path ---
    let mut cmd_nul = [0u8; 512];
    let cb = cmd.as_bytes();
    let clen = cb.len().min(511);
    cmd_nul[..clen].copy_from_slice(&cb[..clen]);
    cmd_nul[clen] = 0;

    exec_with_path_search(
        &cmd_nul[..clen + 1],
        &argv_ptrs[..argc + 1],
        &envp_ptrs[..envc + 1],
    );

    // Only reached if every exec attempt failed (command not found).
    eprintln("env: command not found");
    127
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::exit(101)
}
