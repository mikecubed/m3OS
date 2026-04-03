//! sed — stream editor: substitution (`s/old/new/[g]`), print-range (`N[,M]p`),
//! and delete-range (`N[,M]d`) commands.
#![no_std]
#![no_main]

#[path = "common.rs"]
mod common;

use common::{eprintln, find_newline, write_all};
use syscall_lib::{O_RDONLY, STDIN_FILENO, STDOUT_FILENO, close, open, read};

syscall_lib::entry_point!(main);

enum CmdKind {
    Substitute,
    Print,
    Delete,
}

struct SedCmd {
    kind: CmdKind,
    old_buf: [u8; 256],
    old_len: usize,
    new_buf: [u8; 256],
    new_len: usize,
    global: bool,
    start_line: u64,
    end_line: u64,
}

fn parse_script(script: &[u8]) -> Option<SedCmd> {
    if script.is_empty() {
        return None;
    }
    match script[0] {
        b's' => {
            if script.len() < 4 {
                return None;
            }
            let delim = script[1];
            let rest = &script[2..];
            let old_end = rest.iter().position(|&b| b == delim)?;
            let old_slice = &rest[..old_end];
            let after_old = &rest[old_end + 1..];
            let new_end = after_old.iter().position(|&b| b == delim)?;
            let new_slice = &after_old[..new_end];
            let flags = &after_old[new_end + 1..];
            if old_slice.len() > 255 || new_slice.len() > 255 {
                return None;
            }
            let mut cmd = SedCmd {
                kind: CmdKind::Substitute,
                old_buf: [0u8; 256],
                old_len: old_slice.len(),
                new_buf: [0u8; 256],
                new_len: new_slice.len(),
                global: false,
                start_line: 0,
                end_line: 0,
            };
            cmd.old_buf[..old_slice.len()].copy_from_slice(old_slice);
            cmd.new_buf[..new_slice.len()].copy_from_slice(new_slice);
            cmd.global = flags.contains(&b'g');
            Some(cmd)
        }
        _ => {
            // Parse N or N,M followed by p or d.
            let mut pos = 0;
            let mut start = 0u64;
            while pos < script.len() && script[pos].is_ascii_digit() {
                start = start * 10 + (script[pos] - b'0') as u64;
                pos += 1;
            }
            if pos == 0 {
                return None;
            }
            let mut end = start;
            if pos < script.len() && script[pos] == b',' {
                pos += 1;
                let range_pos = pos;
                let mut tmp = 0u64;
                while pos < script.len() && script[pos].is_ascii_digit() {
                    tmp = tmp * 10 + (script[pos] - b'0') as u64;
                    pos += 1;
                }
                if pos == range_pos {
                    return None;
                }
                end = tmp;
            }
            if pos >= script.len() {
                return None;
            }
            let kind = match script[pos] {
                b'p' => CmdKind::Print,
                b'd' => CmdKind::Delete,
                _ => return None,
            };
            Some(SedCmd {
                kind,
                old_buf: [0u8; 256],
                old_len: 0,
                new_buf: [0u8; 256],
                new_len: 0,
                global: false,
                start_line: start,
                end_line: end,
            })
        }
    }
}

fn apply_substitute(
    line: &[u8],
    old: &[u8],
    new_text: &[u8],
    global: bool,
    out: &mut [u8; 8192],
) -> usize {
    if old.is_empty() {
        let len = line.len().min(out.len());
        out[..len].copy_from_slice(&line[..len]);
        return len;
    }
    let mut pos = 0;
    let mut out_pos = 0;
    let mut replaced = false;
    while pos < line.len() {
        let can_replace = global || !replaced;
        if can_replace && pos + old.len() <= line.len() && line[pos..pos + old.len()] == *old {
            if out_pos + new_text.len() <= out.len() {
                out[out_pos..out_pos + new_text.len()].copy_from_slice(new_text);
                out_pos += new_text.len();
            }
            pos += old.len();
            replaced = true;
        } else {
            if out_pos < out.len() {
                out[out_pos] = line[pos];
                out_pos += 1;
            }
            pos += 1;
        }
    }
    out_pos
}

fn process_line(
    line: &[u8],
    line_num: u64,
    cmd: &SedCmd,
    suppress: bool,
    out_buf: &mut [u8; 8192],
) {
    match &cmd.kind {
        CmdKind::Substitute => {
            let old = &cmd.old_buf[..cmd.old_len];
            let new_text = &cmd.new_buf[..cmd.new_len];
            let out_len = apply_substitute(line, old, new_text, cmd.global, out_buf);
            if !suppress {
                write_all(STDOUT_FILENO, &out_buf[..out_len]);
                write_all(STDOUT_FILENO, b"\n");
            }
        }
        CmdKind::Print => {
            let in_range = line_num >= cmd.start_line && line_num <= cmd.end_line;
            if !suppress {
                write_all(STDOUT_FILENO, line);
                write_all(STDOUT_FILENO, b"\n");
            }
            if in_range {
                write_all(STDOUT_FILENO, line);
                write_all(STDOUT_FILENO, b"\n");
            }
        }
        CmdKind::Delete => {
            let in_range = line_num >= cmd.start_line && line_num <= cmd.end_line;
            if !suppress && !in_range {
                write_all(STDOUT_FILENO, line);
                write_all(STDOUT_FILENO, b"\n");
            }
        }
    }
}

fn process_fd(fd: i32, cmd: &SedCmd, suppress: bool, line_num: &mut u64) {
    let mut buf = [0u8; 4096];
    let mut data_len = 0usize;
    let mut out_buf = [0u8; 8192];
    loop {
        let n = read(fd, &mut buf[data_len..]);
        if n < 0 {
            break;
        }
        let eof = n == 0;
        if !eof {
            data_len += n as usize;
        }
        let mut pos = 0;
        while let Some(nl) = find_newline(&buf[..data_len], pos) {
            *line_num += 1;
            process_line(&buf[pos..nl], *line_num, cmd, suppress, &mut out_buf);
            pos = nl + 1;
        }
        if pos > 0 {
            buf.copy_within(pos..data_len, 0);
            data_len -= pos;
        }
        if eof {
            if data_len > 0 {
                *line_num += 1;
                process_line(&buf[..data_len], *line_num, cmd, suppress, &mut out_buf);
            }
            break;
        }
        if data_len == buf.len() {
            *line_num += 1;
            process_line(&buf[..data_len], *line_num, cmd, suppress, &mut out_buf);
            data_len = 0;
        }
    }
}

fn main(args: &[&str]) -> i32 {
    let mut idx = 1;
    let mut suppress = false;
    if idx < args.len() && args[idx] == "-n" {
        suppress = true;
        idx += 1;
    }
    if idx >= args.len() {
        eprintln("usage: sed [-n] SCRIPT [file...]");
        return 1;
    }
    let script = args[idx].as_bytes();
    idx += 1;
    let cmd = match parse_script(script) {
        Some(c) => c,
        None => {
            eprintln("sed: invalid script");
            return 1;
        }
    };
    let mut line_num = 0u64;
    if idx >= args.len() {
        process_fd(STDIN_FILENO, &cmd, suppress, &mut line_num);
    } else {
        for &file in &args[idx..] {
            let bytes = file.as_bytes();
            if bytes.len() > 255 {
                eprintln("sed: path too long");
                continue;
            }
            let mut path = [0u8; 256];
            path[..bytes.len()].copy_from_slice(bytes);
            path[bytes.len()] = 0;
            let fd = open(&path[..=bytes.len()], O_RDONLY, 0);
            if fd < 0 {
                eprintln("sed: cannot open file");
                continue;
            }
            process_fd(fd as i32, &cmd, suppress, &mut line_num);
            close(fd as i32);
        }
    }
    0
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::exit(101)
}
