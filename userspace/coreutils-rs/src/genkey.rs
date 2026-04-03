//! genkey — generate an Ed25519 keypair and write to files.
#![no_std]
#![no_main]

use crypto_lib::asymmetric::{
    ed25519_keygen, ed25519_signing_key_to_bytes, ed25519_verifying_key_to_bytes,
};
use crypto_lib::random::csprng_init;
use syscall_lib::{O_CREAT, O_TRUNC, O_WRONLY, STDERR_FILENO, STDOUT_FILENO, close, open, write};

syscall_lib::entry_point!(main);

const HEX: &[u8; 16] = b"0123456789abcdef";

/// Write all bytes to a file descriptor, retrying on partial writes.
fn write_all(fd: i32, mut data: &[u8]) -> bool {
    while !data.is_empty() {
        let n = write(fd, data);
        if n <= 0 {
            return false;
        }
        data = &data[n as usize..];
    }
    true
}

fn write_key_file(dir: &[u8], name: &[u8], data: &[u8]) -> bool {
    let mut path = [0u8; 256];
    let dir_len = dir.len();
    if dir_len + 1 + name.len() + 1 > path.len() {
        return false;
    }
    path[..dir_len].copy_from_slice(dir);
    if dir_len > 0 && dir[dir_len - 1] != b'/' {
        path[dir_len] = b'/';
        path[dir_len + 1..dir_len + 1 + name.len()].copy_from_slice(name);
        path[dir_len + 1 + name.len()] = 0;
    } else {
        path[dir_len..dir_len + name.len()].copy_from_slice(name);
        path[dir_len + name.len()] = 0;
    }

    let total_len = if dir_len > 0 && dir[dir_len - 1] != b'/' {
        dir_len + 1 + name.len() + 1
    } else {
        dir_len + name.len() + 1
    };

    let fd = open(&path[..total_len], O_WRONLY | O_CREAT | O_TRUNC, 0o600);
    if fd < 0 {
        let _ = write(STDERR_FILENO, b"genkey: failed to create ");
        let _ = write(STDERR_FILENO, &path[..total_len - 1]);
        let _ = write(STDERR_FILENO, b"\n");
        return false;
    }
    let ok = write_all(fd as i32, data);
    close(fd as i32);
    if !ok {
        let _ = write(STDERR_FILENO, b"genkey: write error for ");
        let _ = write(STDERR_FILENO, &path[..total_len - 1]);
        let _ = write(STDERR_FILENO, b"\n");
    }
    ok
}

fn main(args: &[&str]) -> i32 {
    // Parse optional -o flag.
    let mut output_dir: &[u8] = b".";
    let mut i = 1;
    while i < args.len() {
        if args[i] == "-o" {
            if i + 1 >= args.len() {
                let _ = write(STDERR_FILENO, b"genkey: -o requires an argument\n");
                return 1;
            }
            output_dir = args[i + 1].as_bytes();
            i += 2;
        } else {
            let _ = write(STDERR_FILENO, b"Usage: genkey [-o <dir>]\n");
            return 1;
        }
    }

    // Initialize CSPRNG.
    let mut rng = match csprng_init() {
        Ok(r) => r,
        Err(_) => {
            let _ = write(STDERR_FILENO, b"genkey: failed to initialize CSPRNG\n");
            return 1;
        }
    };

    // Generate Ed25519 keypair.
    let (signing_key, verifying_key) = ed25519_keygen(&mut rng);
    let sk_bytes = ed25519_signing_key_to_bytes(&signing_key);
    let vk_bytes = ed25519_verifying_key_to_bytes(&verifying_key);

    // Write private key.
    if !write_key_file(output_dir, b"id_ed25519", &sk_bytes) {
        return 1;
    }

    // Write public key.
    if !write_key_file(output_dir, b"id_ed25519.pub", &vk_bytes) {
        return 1;
    }

    // Print public key in hex to stdout.
    let mut hex = [0u8; 64];
    for (j, &b) in vk_bytes.iter().enumerate() {
        hex[j * 2] = HEX[(b >> 4) as usize];
        hex[j * 2 + 1] = HEX[(b & 0x0f) as usize];
    }
    let _ = write(STDOUT_FILENO, &hex);
    let _ = write(STDOUT_FILENO, b"\n");

    0
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::exit(101)
}
