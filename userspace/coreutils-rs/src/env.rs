//! env — run a program in a modified environment, or print the environment.
//!
//! Usage: env [-] [-i] [NAME=VALUE]... [COMMAND [ARG]...]
//!
//! With a COMMAND, env PATH-searches it, applies any leading `NAME=VALUE`
//! assignments to the environment (each one OVERRIDES an inherited variable of
//! the same name), and execs it — this is the path the `#!/usr/bin/env <interp>`
//! shebang rides (e.g. npm's `#!/usr/bin/env node`). `-`/`-i` starts from an
//! empty environment (only the assignments survive). A `PATH=` assignment also
//! redirects the command lookup itself (POSIX). With no COMMAND, env prints the
//! environment (one `NAME=VALUE` per line).
#![no_std]
#![no_main]
// Raw pointers into fixed stack buffers are handed to execve; valid until the
// exec replaces the (single-threaded) process image.
#![allow(static_mut_refs)]

#[path = "common.rs"]
mod common;

use common::{eprintln, exec_with_path_search, print};

syscall_lib::entry_point_with_env!(main);

/// The NAME part of a `NAME=VALUE` entry (bytes before the first `=`); the whole
/// string if there is no `=`.
fn key_of(entry: &[u8]) -> &[u8] {
    match entry.iter().position(|&b| b == b'=') {
        Some(i) => &entry[..i],
        None => entry,
    }
}

fn main(args: &[&str], env: &[&str]) -> i32 {
    // args[0] is the program name (for a shebang, the literal `/usr/bin/env`).
    let rest: &[&str] = if args.len() > 1 {
        &args[1..]
    } else {
        &[] as &[&str]
    };

    // `-`/`-i` clears the inherited environment (only the assignments survive);
    // then collect leading `NAME=VALUE` assignments — the first remaining token
    // is the COMMAND.
    let mut clear_env = false;
    let mut idx = 0usize;
    while idx < rest.len() {
        let a = rest[idx];
        if idx == 0 && (a == "-" || a == "-i") {
            clear_env = true;
            idx += 1;
            continue;
        }
        if !a.starts_with('-') && a.as_bytes().contains(&b'=') {
            idx += 1;
            continue;
        }
        break;
    }

    let assignments = &rest[..idx];

    if idx >= rest.len() {
        // No COMMAND: print the environment (cleared by -i) plus any standalone
        // assignments. An assignment OVERRIDES (replaces) an inherited var of the
        // same NAME, so suppress the stale inherited line — mirroring the exec
        // path below, and matching the documented semantics / GNU `env`.
        if !clear_env {
            for e in env {
                let eb = e.as_bytes();
                if assignments
                    .iter()
                    .any(|a| a.as_bytes().contains(&b'=') && key_of(a.as_bytes()) == key_of(eb))
                {
                    continue;
                }
                print(e);
                print("\n");
            }
        }
        for a in assignments {
            if a.as_bytes().contains(&b'=') {
                print(a);
                print("\n");
            }
        }
        return 0;
    }

    let cmd_args = &rest[idx..]; // [COMMAND, ARG...]
    let cmd = cmd_args[0];

    // --- argv: pack cmd_args as NUL-terminated C strings, record pointers ---
    // Fail fast on overflow rather than execing a silently-truncated argv (which
    // would run the wrong command line).
    let mut argv_buf = [0u8; 4096];
    let mut argv_ptrs = [core::ptr::null::<u8>(); 98];
    let mut bpos = 0usize;
    let mut argc = 0usize;
    for &a in cmd_args {
        let b = a.as_bytes();
        if argc >= 97 || bpos + b.len() + 1 > argv_buf.len() {
            eprintln("env: argument list too long");
            return 127;
        }
        argv_ptrs[argc] = argv_buf[bpos..].as_ptr();
        argv_buf[bpos..bpos + b.len()].copy_from_slice(b);
        bpos += b.len();
        argv_buf[bpos] = 0;
        bpos += 1;
        argc += 1;
    }

    // --- envp: the inherited environment (the `env` param — unless -i), with any
    // reassigned NAME dropped so the assignment WINS (musl getenv is first-match),
    // then the assignments appended. ---
    let mut envp_buf = [0u8; 8192];
    let mut epos = 0usize;
    {
        // Scoped so the `append` closure's borrow of envp_buf/epos ends here,
        // freeing both for the pointer scan below.
        let mut append = |entry: &[u8]| {
            if epos + entry.len() + 1 > envp_buf.len() {
                return;
            }
            envp_buf[epos..epos + entry.len()].copy_from_slice(entry);
            epos += entry.len();
            envp_buf[epos] = 0;
            epos += 1;
        };
        if !clear_env {
            for &e in env {
                let eb = e.as_bytes();
                // Skip an inherited var that an assignment redefines (override).
                if assignments
                    .iter()
                    .any(|a| a.as_bytes().contains(&b'=') && key_of(a.as_bytes()) == key_of(eb))
                {
                    continue;
                }
                append(eb);
            }
        }
        for &a in assignments {
            let b = a.as_bytes();
            if b.contains(&b'=') {
                append(b);
            }
        }
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

    // Command lookup PATH: a `PATH=` assignment (last one wins) overrides the
    // search path — POSIX requires the command to be looked up in the MODIFIED
    // environment. `-i` with no PATH= clears it (→ default /bin:/usr/bin via the
    // empty-PATH sentinel). Otherwise inherit (None → exec_with_path_search reads
    // /proc/self/environ, the path npm's `#!/usr/bin/env node` rides).
    let path_assign: Option<&[u8]> = assignments
        .iter()
        .rev()
        .find(|a| a.as_bytes().starts_with(b"PATH="))
        .map(|a| &a.as_bytes()[5..]);
    let path_override: Option<&[u8]> = match path_assign {
        Some(p) => Some(p),
        None if clear_env => Some(b""), // empty = no PATH → default fallback only
        None => None,                   // inherit PATH from the environment
    };

    // --- COMMAND as a NUL-terminated path ---
    // Fail fast rather than execing a silently-truncated (wrong) command name,
    // mirroring the argv overflow handling above.
    let mut cmd_nul = [0u8; 512];
    let cb = cmd.as_bytes();
    if cb.len() >= cmd_nul.len() {
        eprintln("env: command name too long");
        return 127;
    }
    let clen = cb.len();
    cmd_nul[..clen].copy_from_slice(cb);
    cmd_nul[clen] = 0;

    exec_with_path_search(
        &cmd_nul[..clen + 1],
        &argv_ptrs[..argc + 1],
        &envp_ptrs[..envc + 1],
        path_override,
    );

    // Only reached if every exec attempt failed (command not found).
    eprintln("env: command not found");
    127
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::exit(101)
}
