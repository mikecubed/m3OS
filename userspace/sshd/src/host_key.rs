//! Host key generation and storage (Track B).
//!
//! Generates Ed25519 host keys on first boot and persists them as raw 32-byte seeds.

use syscall_lib::{O_CREAT, O_RDONLY, O_TRUNC, O_WRONLY, STDOUT_FILENO, close, open, write_str};

use sunset::{KeyType, SignKey};

const HOST_KEY_PATH: &[u8] = b"/etc/ssh/ssh_host_ed25519_key\0";
const HOST_KEY_PUB_PATH: &[u8] = b"/etc/ssh/ssh_host_ed25519_key.pub\0";

/// A wrapper around sunset's SignKey for the host key.
pub struct HostKey {
    pub key: SignKey,
}

/// Load an existing host key, or generate one if none exists.
pub fn load_or_generate() -> Result<HostKey, ()> {
    match load_host_key() {
        Ok(key) => {
            write_str(STDOUT_FILENO, "sshd: loaded existing host key\n");
            Ok(key)
        }
        Err(()) => {
            write_str(STDOUT_FILENO, "sshd: generating new host key\n");
            generate_host_key()
        }
    }
}

/// B.3: Load existing host key from /etc/ssh/ssh_host_ed25519_key.
fn load_host_key() -> Result<HostKey, ()> {
    let fd = open(HOST_KEY_PATH, O_RDONLY, 0);
    if fd < 0 {
        return Err(());
    }
    let fd = fd as i32;

    let mut seed = [0u8; 32];
    let n = syscall_lib::read(fd, &mut seed);
    close(fd);

    if n != 32 {
        return Err(());
    }

    // Reconstruct the dalek SigningKey from the seed.
    let signing_key = ed25519_dalek::SigningKey::from_bytes(&seed);
    Ok(HostKey {
        key: SignKey::Ed25519(signing_key),
    })
}

/// B.2: Generate a new Ed25519 host key and save to disk.
fn generate_host_key() -> Result<HostKey, ()> {
    let key = SignKey::generate(KeyType::Ed25519, None).map_err(|_| ())?;

    // Extract the 32-byte seed from the key.
    let seed = match &key {
        SignKey::Ed25519(k) => k.to_bytes(),
        _ => return Err(()),
    };

    // Write private key seed (mode 0600).
    let fd = open(HOST_KEY_PATH, O_WRONLY | O_CREAT | O_TRUNC, 0o600);
    if fd < 0 {
        write_str(STDOUT_FILENO, "sshd: cannot write host key\n");
        return Err(());
    }
    let n = syscall_lib::write(fd as i32, &seed);
    close(fd as i32);
    if n != 32 {
        write_str(STDOUT_FILENO, "sshd: short write on host key\n");
        syscall_lib::unlink(HOST_KEY_PATH);
        return Err(());
    }

    // Write public key (mode 0644).
    let pubkey_bytes = match &key {
        SignKey::Ed25519(k) => k.verifying_key().to_bytes(),
        _ => return Err(()),
    };
    let fd = open(HOST_KEY_PUB_PATH, O_WRONLY | O_CREAT | O_TRUNC, 0o644);
    if fd >= 0 {
        let n = syscall_lib::write(fd as i32, &pubkey_bytes);
        close(fd as i32);
        if n != 32 {
            syscall_lib::unlink(HOST_KEY_PUB_PATH);
        }
    }

    // Print fingerprint (SHA-256 of public key) to log.
    print_fingerprint(&pubkey_bytes);

    Ok(HostKey { key })
}

/// Print the SHA-256 fingerprint of the public key to serial.
fn print_fingerprint(pubkey: &[u8; 32]) {
    let hash = crypto_lib::hash::sha256(pubkey);
    write_str(STDOUT_FILENO, "sshd: host key fingerprint SHA256:");
    let mut hex = [0u8; 64];
    for (i, &b) in hash.iter().enumerate() {
        hex[i * 2] = to_hex_char(b >> 4);
        hex[i * 2 + 1] = to_hex_char(b & 0xf);
    }
    syscall_lib::write(STDOUT_FILENO, &hex);
    syscall_lib::write(STDOUT_FILENO, b"\n");
}

fn to_hex_char(nibble: u8) -> u8 {
    if nibble < 10 {
        b'0' + nibble
    } else {
        b'a' + nibble - 10
    }
}
