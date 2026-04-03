//! xargs — build and execute command lines from standard input.
//!
//! Usage: xargs [-0] [-I REPLSTR] command [args...]
//!
//! Reads items from stdin (newline-delimited by default, NUL-delimited with -0).
//! Without -I: appends all items to command and runs once.
//! With -I REPLSTR: replaces REPLSTR in each arg, runs once per item.
#![no_std]
#![no_main]
// static_mut_refs: taking &mut/& to a static mut is denied in Rust 2024 by
// default, but we use it intentionally in single-threaded fork/exec code.
#![allow(static_mut_refs)]

#[path = "common.rs"]
mod common;

use common::{build_nul_path, eprintln};
use syscall_lib::{O_RDONLY, STDIN_FILENO, close, execve, fork, open, read, waitpid};

syscall_lib::entry_point!(main);

const MAX_ITEMS: usize = 512;
const ITEM_DATA_CAP: usize = 131072;

fn wifexited(status: i32) -> bool {
    (status & 0x7f) == 0
}

fn wexitstatus(status: i32) -> i32 {
    (status >> 8) & 0xff
}

/// Read all of stdin into `item_data`, returning the number of bytes read.
fn read_all_stdin(item_data: &mut [u8; ITEM_DATA_CAP]) -> usize {
    let mut total = 0usize;
    loop {
        if total >= ITEM_DATA_CAP {
            break;
        }
        let n = read(STDIN_FILENO, &mut item_data[total..]);
        if n <= 0 {
            break;
        }
        total += n as usize;
    }
    total
}

/// Parse items in `item_data[..data_len]`, delimited by `delimiter`.
/// Replaces each delimiter with NUL and records item start offsets in
/// `item_offsets`. Returns the number of items found.
fn parse_items(
    item_data: &mut [u8; ITEM_DATA_CAP],
    item_offsets: &mut [u32; MAX_ITEMS],
    data_len: usize,
    delimiter: u8,
) -> usize {
    let mut count = 0usize;
    let mut start = 0usize;
    let mut i = 0;
    while i < data_len {
        if item_data[i] == delimiter {
            item_data[i] = 0;
            if i > start && count < MAX_ITEMS {
                item_offsets[count] = start as u32;
                count += 1;
            }
            start = i + 1;
        }
        i += 1;
    }
    // Handle last item with no trailing delimiter.
    if start < data_len && count < MAX_ITEMS {
        item_offsets[count] = start as u32;
        count += 1;
        if data_len < ITEM_DATA_CAP {
            item_data[data_len] = 0;
        }
    }
    count
}

/// Try to exec `cmd` (NUL-terminated) by searching PATH from /proc/self/environ.
/// If `cmd` contains '/', exec it directly. On failure (all attempts return),
/// this function returns without executing anything.
fn exec_with_path_search(cmd: &[u8], argv: &[*const u8], envp: &[*const u8]) {
    if cmd.contains(&b'/') {
        execve(cmd, argv, envp);
        return;
    }
    // Strip trailing NUL to get just the command name.
    let cmd_name = if cmd.last() == Some(&0) {
        &cmd[..cmd.len().saturating_sub(1)]
    } else {
        cmd
    };

    // Read PATH from /proc/self/environ (NUL-separated KEY=VALUE entries).
    let mut env_buf = [0u8; 4096];
    let mut path_off = 0usize;
    let mut path_len = 0usize;
    let env_path = b"/proc/self/environ\0";
    let fd = open(env_path, O_RDONLY, 0);
    if fd >= 0 {
        let n = read(fd as i32, &mut env_buf);
        close(fd as i32);
        if n > 0 {
            let data = &env_buf[..n as usize];
            let mut i = 0;
            while i < data.len() {
                let end = data[i..]
                    .iter()
                    .position(|&b| b == 0)
                    .map(|p| i + p)
                    .unwrap_or(data.len());
                let entry = &data[i..end];
                if entry.starts_with(b"PATH=") {
                    path_off = i + 5;
                    path_len = end.saturating_sub(i + 5);
                    break;
                }
                i = end + 1;
            }
        }
    }

    if path_len == 0 {
        let fallback: [&[u8]; 2] = [b"/bin", b"/usr/bin"];
        for &dir in &fallback {
            let mut path_buf = [0u8; 512];
            if let Some(plen) = build_nul_path(&mut path_buf, &[dir, cmd_name]) {
                execve(&path_buf[..plen], argv, envp);
            }
        }
        return;
    }

    let path_val = &env_buf[path_off..path_off + path_len];
    let mut seg_start = 0;
    loop {
        let seg_end = path_val[seg_start..]
            .iter()
            .position(|&b| b == b':')
            .map(|p| seg_start + p)
            .unwrap_or(path_val.len());
        let dir = &path_val[seg_start..seg_end];
        if !dir.is_empty() {
            let mut path_buf = [0u8; 512];
            if let Some(plen) = build_nul_path(&mut path_buf, &[dir, cmd_name]) {
                execve(&path_buf[..plen], argv, envp);
            }
        }
        if seg_end >= path_val.len() {
            break;
        }
        seg_start = seg_end + 1;
    }
}

/// Replace all occurrences of `replstr` in `template` with `item`, writing the
/// result into `out[..511]`. Returns the number of bytes written (without NUL).
fn replace_str(template: &[u8], replstr: &[u8], item: &[u8], out: &mut [u8; 512]) -> usize {
    if replstr.is_empty() {
        let len = template.len().min(511);
        out[..len].copy_from_slice(&template[..len]);
        return len;
    }
    let mut pos = 0;
    let mut out_pos = 0;
    while pos < template.len() {
        if pos + replstr.len() <= template.len() && template[pos..pos + replstr.len()] == *replstr {
            if out_pos + item.len() <= 511 {
                out[out_pos..out_pos + item.len()].copy_from_slice(item);
                out_pos += item.len();
            }
            pos += replstr.len();
        } else {
            if out_pos < 511 {
                out[out_pos] = template[pos];
                out_pos += 1;
            }
            pos += 1;
        }
    }
    out_pos
}

/// Fork, build argv from fixed_args + all items, and exec `cmd_bytes`.
/// Returns the child's exit code (or 1 on error).
fn run_once(
    cmd_bytes: &[u8],
    fixed_args: &[&str],
    item_data: &[u8; ITEM_DATA_CAP],
    item_offsets: &[u32; MAX_ITEMS],
    item_count: usize,
) -> i32 {
    let pid = fork();
    if pid < 0 {
        eprintln("xargs: fork failed");
        return 1;
    }
    if pid == 0 {
        // Child: build argv and exec.
        let mut cmd_nul = [0u8; 256];
        let clen = cmd_bytes.len().min(255);
        cmd_nul[..clen].copy_from_slice(&cmd_bytes[..clen]);

        let null_ptr: *const u8 = core::ptr::null();
        let mut argv_ptrs: [*const u8; 66] = [null_ptr; 66];
        let mut argc = 0usize;

        argv_ptrs[0] = cmd_nul.as_ptr();
        argc += 1;

        // NUL-terminated copies of fixed args (skipping the command itself).
        let mut fixed_nul: [[u8; 256]; 32] = [[0u8; 256]; 32];
        for (i, &arg) in fixed_args.iter().skip(1).enumerate() {
            if i >= 32 || argc >= 65 {
                break;
            }
            let b = arg.as_bytes();
            let alen = b.len().min(255);
            fixed_nul[i][..alen].copy_from_slice(&b[..alen]);
            argv_ptrs[argc] = fixed_nul[i].as_ptr();
            argc += 1;
        }

        // Append each item (already NUL-terminated in item_data).
        for &offset_u32 in item_offsets.iter().take(item_count) {
            if argc >= 65 {
                break;
            }
            let offset = offset_u32 as usize;
            argv_ptrs[argc] = item_data[offset..].as_ptr();
            argc += 1;
        }

        // argv_ptrs[argc] is the null terminator (initialized to null_ptr).
        let envp: [*const u8; 1] = [null_ptr];
        exec_with_path_search(&cmd_nul[..clen + 1], &argv_ptrs[..argc + 1], &envp);
        syscall_lib::exit(127);
    }
    let mut status = 0i32;
    waitpid(pid as i32, &mut status, 0);
    if wifexited(status) {
        wexitstatus(status)
    } else {
        1
    }
}

/// Fork, build argv from fixed_args with REPLSTR replaced by `item`, and exec.
/// Returns the child's exit code (or 1 on error).
fn run_replace(cmd_bytes: &[u8], fixed_args: &[&str], item: &[u8], replstr: &[u8]) -> i32 {
    let pid = fork();
    if pid < 0 {
        eprintln("xargs: fork failed");
        return 1;
    }
    if pid == 0 {
        let mut cmd_nul = [0u8; 256];
        let clen = cmd_bytes.len().min(255);
        cmd_nul[..clen].copy_from_slice(&cmd_bytes[..clen]);

        let null_ptr: *const u8 = core::ptr::null();
        let mut argv_ptrs: [*const u8; 66] = [null_ptr; 66];
        let mut argc = 0usize;

        argv_ptrs[0] = cmd_nul.as_ptr();
        argc += 1;

        // Build each fixed arg with REPLSTR replaced by the item.
        let mut replaced_args: [[u8; 512]; 32] = [[0u8; 512]; 32];
        for (i, &arg) in fixed_args.iter().skip(1).enumerate() {
            if i >= 32 || argc >= 65 {
                break;
            }
            let rlen = replace_str(arg.as_bytes(), replstr, item, &mut replaced_args[i]);
            replaced_args[i][rlen] = 0;
            argv_ptrs[argc] = replaced_args[i].as_ptr();
            argc += 1;
        }

        let envp: [*const u8; 1] = [null_ptr];
        exec_with_path_search(&cmd_nul[..clen + 1], &argv_ptrs[..argc + 1], &envp);
        syscall_lib::exit(127);
    }
    let mut status = 0i32;
    waitpid(pid as i32, &mut status, 0);
    if wifexited(status) {
        wexitstatus(status)
    } else {
        1
    }
}

fn main(args: &[&str]) -> i32 {
    let mut idx = 1;
    let mut use_null = false;
    let mut replstr: &[u8] = &[];

    while idx < args.len() {
        match args[idx] {
            "-0" => {
                use_null = true;
                idx += 1;
            }
            "-I" => {
                idx += 1;
                if idx >= args.len() {
                    eprintln("xargs: -I requires argument");
                    return 1;
                }
                replstr = args[idx].as_bytes();
                idx += 1;
            }
            _ => break,
        }
    }

    if idx >= args.len() {
        eprintln("usage: xargs [-0] [-I REPLSTR] command [args...]");
        return 1;
    }

    let cmd_bytes = args[idx].as_bytes();
    let fixed_args = &args[idx..];

    static mut ITEM_DATA: [u8; ITEM_DATA_CAP] = [0u8; ITEM_DATA_CAP];
    static mut ITEM_OFFSETS: [u32; MAX_ITEMS] = [0u32; MAX_ITEMS];

    let delimiter = if use_null { 0u8 } else { b'\n' };
    let data_len = unsafe { read_all_stdin(&mut ITEM_DATA) };
    let item_count = unsafe { parse_items(&mut ITEM_DATA, &mut ITEM_OFFSETS, data_len, delimiter) };

    if replstr.is_empty() {
        unsafe { run_once(cmd_bytes, fixed_args, &ITEM_DATA, &ITEM_OFFSETS, item_count) }
    } else {
        let mut exit_code = 0i32;
        for &offset_u32 in unsafe { &ITEM_OFFSETS }.iter().take(item_count) {
            let offset = offset_u32 as usize;
            let item_end = unsafe {
                ITEM_DATA[offset..]
                    .iter()
                    .position(|&b| b == 0)
                    .map(|p| offset + p)
                    .unwrap_or(offset)
            };
            let item = unsafe { &ITEM_DATA[offset..item_end] };
            let code = run_replace(cmd_bytes, fixed_args, item, replstr);
            if code != 0 {
                exit_code = code;
            }
        }
        exit_code
    }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::exit(101)
}
