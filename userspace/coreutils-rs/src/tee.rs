//! tee — copy stdin to stdout and to each file argument.
#![no_std]
#![no_main]

use syscall_lib::{
    O_APPEND, O_CREAT, O_TRUNC, O_WRONLY, STDERR_FILENO, STDIN_FILENO, STDOUT_FILENO, close, open,
    read, write, write_str,
};

syscall_lib::entry_point!(main);

const MAX_FILES: usize = 8;

fn write_all(fd: i32, data: &[u8]) -> bool {
    let mut off = 0;
    while off < data.len() {
        let w = write(fd, &data[off..]);
        if w <= 0 {
            return false;
        }
        off += w as usize;
    }
    true
}

fn main(args: &[&str]) -> i32 {
    let mut append = false;
    let mut argi = 1usize;
    let mut status = 0i32;

    // Parse flags.
    while argi < args.len() {
        if args[argi] == "--" {
            argi += 1;
            break;
        }
        if args[argi] == "-a" {
            append = true;
            argi += 1;
            continue;
        }
        if args[argi].starts_with('-') && args[argi].len() > 1 {
            write_str(STDERR_FILENO, "usage: tee [-a] [file...]\n");
            return 1;
        }
        break;
    }

    // Open output files (up to MAX_FILES).
    let file_args = &args[argi..];
    let num_files = if file_args.len() > MAX_FILES {
        MAX_FILES
    } else {
        file_args.len()
    };
    let mut fds = [-1i32; MAX_FILES];

    let flags = if append {
        O_WRONLY | O_CREAT | O_APPEND
    } else {
        O_WRONLY | O_CREAT | O_TRUNC
    };

    for i in 0..num_files {
        let bytes = file_args[i].as_bytes();
        if bytes.len() > 255 {
            write_str(STDERR_FILENO, "tee: path too long\n");
            status = 1;
            continue;
        }
        let mut path = [0u8; 256];
        path[..bytes.len()].copy_from_slice(bytes);
        path[bytes.len()] = 0;
        let fd = open(&path[..=bytes.len()], flags, 0o666);
        if fd < 0 {
            write_str(STDERR_FILENO, "tee: cannot open '");
            write_str(STDERR_FILENO, file_args[i]);
            write_str(STDERR_FILENO, "'\n");
            status = 1;
        } else {
            fds[i] = fd as i32;
        }
    }

    // Copy loop: stdin → stdout + each open file.
    let mut buf = [0u8; 4096];
    loop {
        let n = read(STDIN_FILENO, &mut buf);
        if n == 0 {
            break;
        }
        if n < 0 {
            write_str(STDERR_FILENO, "tee: read error\n");
            status = 1;
            break;
        }
        let data = &buf[..n as usize];
        if !write_all(STDOUT_FILENO, data) {
            write_str(STDERR_FILENO, "tee: write error\n");
            status = 1;
        }
        for fd in &fds[..num_files] {
            if *fd >= 0 && !write_all(*fd, data) {
                write_str(STDERR_FILENO, "tee: write error\n");
                status = 1;
            }
        }
    }

    for fd in &fds[..num_files] {
        if *fd >= 0 {
            close(*fd);
        }
    }
    status
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::exit(101)
}
