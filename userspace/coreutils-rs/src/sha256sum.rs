//! sha256sum — compute SHA-256 hashes of files.
#![no_std]
#![no_main]

use crypto_lib::hash::Sha256Hasher;
use syscall_lib::{O_RDONLY, STDERR_FILENO, STDIN_FILENO, STDOUT_FILENO, close, open, read, write};

syscall_lib::entry_point!(main);

const BUF_SIZE: usize = 4096;
const HEX: &[u8; 16] = b"0123456789abcdef";

/// Hash an open file descriptor. Returns `Ok(hash)` or `Err(())` on read error.
fn hash_fd(fd: i32) -> Result<[u8; 32], ()> {
    let mut hasher = Sha256Hasher::new();
    let mut buf = [0u8; BUF_SIZE];
    loop {
        let n = read(fd, &mut buf);
        if n < 0 {
            return Err(());
        }
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n as usize]);
    }
    Ok(hasher.finalize())
}

fn print_hash(hash: &[u8; 32]) {
    let mut hex = [0u8; 64];
    for (i, &b) in hash.iter().enumerate() {
        hex[i * 2] = HEX[(b >> 4) as usize];
        hex[i * 2 + 1] = HEX[(b & 0x0f) as usize];
    }
    let _ = write(STDOUT_FILENO, &hex);
}

fn main(args: &[&str]) -> i32 {
    if args.len() <= 1 {
        // Read from stdin.
        match hash_fd(STDIN_FILENO) {
            Ok(hash) => {
                print_hash(&hash);
                let _ = write(STDOUT_FILENO, b"  -\n");
                return 0;
            }
            Err(()) => {
                let _ = write(STDERR_FILENO, b"sha256sum: read error\n");
                return 1;
            }
        }
    }

    let mut ret = 0;
    for &filename in &args[1..] {
        let mut path_buf = [0u8; 256];

        if filename.len() >= path_buf.len() {
            let _ = write(STDERR_FILENO, b"sha256sum: ");
            let _ = write(STDERR_FILENO, filename.as_bytes());
            let _ = write(STDERR_FILENO, b": File name too long\n");
            ret = 1;
            continue;
        }

        let path_len = filename.len();
        path_buf[..path_len].copy_from_slice(filename.as_bytes());
        path_buf[path_len] = 0;

        let fd = open(&path_buf[..=path_len], O_RDONLY, 0);
        if fd < 0 {
            let _ = write(STDERR_FILENO, b"sha256sum: ");
            let _ = write(STDERR_FILENO, filename.as_bytes());
            let _ = write(STDERR_FILENO, b": No such file or directory\n");
            ret = 1;
            continue;
        }

        match hash_fd(fd as i32) {
            Ok(hash) => {
                close(fd as i32);
                print_hash(&hash);
                let _ = write(STDOUT_FILENO, b"  ");
                let _ = write(STDOUT_FILENO, filename.as_bytes());
                let _ = write(STDOUT_FILENO, b"\n");
            }
            Err(()) => {
                close(fd as i32);
                let _ = write(STDERR_FILENO, b"sha256sum: ");
                let _ = write(STDERR_FILENO, filename.as_bytes());
                let _ = write(STDERR_FILENO, b": read error\n");
                ret = 1;
            }
        }
    }
    ret
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::exit(101)
}
