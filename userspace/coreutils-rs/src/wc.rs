//! wc — count lines, words, and bytes.
#![no_std]
#![no_main]

use syscall_lib::{
    O_RDONLY, STDERR_FILENO, STDOUT_FILENO, close, open, read, write_str, write_u64,
};

syscall_lib::entry_point!(main);

fn wc_fd(fd: i32) -> (u64, u64, u64) {
    let mut buf = [0u8; 4096];
    let mut lines: u64 = 0;
    let mut words: u64 = 0;
    let mut bytes: u64 = 0;
    let mut in_word = false;
    loop {
        let n = read(fd, &mut buf);
        if n <= 0 {
            break;
        }
        let n = n as usize;
        bytes += n as u64;
        for &b in &buf[..n] {
            if b == b'\n' {
                lines += 1;
            }
            if b == b' ' || b == b'\t' || b == b'\n' || b == b'\r' {
                in_word = false;
            } else if !in_word {
                in_word = true;
                words += 1;
            }
        }
    }
    (lines, words, bytes)
}

fn main(args: &[&str]) -> i32 {
    let mut show_lines = false;
    let mut show_words = false;
    let mut show_bytes = false;
    let mut first_file = 1;

    // Parse flags.
    for (i, arg) in args.iter().enumerate().skip(1) {
        if arg.starts_with('-') && arg.len() > 1 {
            for b in arg.as_bytes()[1..].iter() {
                match b {
                    b'l' => show_lines = true,
                    b'w' => show_words = true,
                    b'c' => show_bytes = true,
                    _ => {}
                }
            }
            first_file = i + 1;
        } else {
            break;
        }
    }

    if !show_lines && !show_words && !show_bytes {
        show_lines = true;
        show_words = true;
        show_bytes = true;
    }

    let files = &args[first_file..];
    let mut total_l: u64 = 0;
    let mut total_w: u64 = 0;
    let mut total_c: u64 = 0;

    if files.is_empty() {
        let (l, w, c) = wc_fd(0);
        if show_lines {
            write_u64(STDOUT_FILENO, l);
            write_str(STDOUT_FILENO, " ");
        }
        if show_words {
            write_u64(STDOUT_FILENO, w);
            write_str(STDOUT_FILENO, " ");
        }
        if show_bytes {
            write_u64(STDOUT_FILENO, c);
        }
        write_str(STDOUT_FILENO, "\n");
        return 0;
    }

    for file in files {
        let bytes = file.as_bytes();
        if bytes.len() > 254 {
            write_str(STDERR_FILENO, "wc: path too long\n");
            continue;
        }
        let mut path = [0u8; 256];
        path[..bytes.len()].copy_from_slice(bytes);
        path[bytes.len()] = 0;

        let fd = open(&path[..=bytes.len()], O_RDONLY, 0);
        if fd < 0 {
            write_str(STDERR_FILENO, "wc: cannot open: ");
            write_str(STDERR_FILENO, file);
            write_str(STDERR_FILENO, "\n");
            continue;
        }
        let (l, w, c) = wc_fd(fd as i32);
        close(fd as i32);
        total_l += l;
        total_w += w;
        total_c += c;

        if show_lines {
            write_u64(STDOUT_FILENO, l);
            write_str(STDOUT_FILENO, " ");
        }
        if show_words {
            write_u64(STDOUT_FILENO, w);
            write_str(STDOUT_FILENO, " ");
        }
        if show_bytes {
            write_u64(STDOUT_FILENO, c);
            write_str(STDOUT_FILENO, " ");
        }
        write_str(STDOUT_FILENO, file);
        write_str(STDOUT_FILENO, "\n");
    }

    if files.len() > 1 {
        if show_lines {
            write_u64(STDOUT_FILENO, total_l);
            write_str(STDOUT_FILENO, " ");
        }
        if show_words {
            write_u64(STDOUT_FILENO, total_w);
            write_str(STDOUT_FILENO, " ");
        }
        if show_bytes {
            write_u64(STDOUT_FILENO, total_c);
            write_str(STDOUT_FILENO, " ");
        }
        write_str(STDOUT_FILENO, "total\n");
    }
    0
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::exit(101)
}
