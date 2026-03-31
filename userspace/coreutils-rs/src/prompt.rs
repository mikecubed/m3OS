//! PROMPT — ion shell prompt generator.
//!
//! Output: `\x1b[94m<user>\x1b[0m@\x1b[96mm3os\x1b[0m:<cwd># ` (root)
//!     or: `\x1b[94m<user>\x1b[0m@\x1b[96mm3os\x1b[0m:<cwd>$ ` (non-root)
#![no_std]
#![no_main]

use syscall_lib::{STDOUT_FILENO, getcwd, getuid, write};

syscall_lib::entry_point_with_env!(main);

fn main(_args: &[&str], env: &[&str]) -> i32 {
    let mut prompt = [0u8; 512];
    let mut len = 0;

    // Find $USER in environment.
    let mut user: Option<&str> = None;
    for e in env {
        if let Some(val) = e.strip_prefix("USER=") {
            user = Some(val);
            break;
        }
    }

    // Light blue username.
    len += copy(&mut prompt[len..], b"\x1b[94m");
    if let Some(u) = user {
        let ub = u.as_bytes();
        let ul = ub.len().min(32);
        len += copy(&mut prompt[len..], &ub[..ul]);
    } else {
        // Fallback: show uid as decimal.
        len += write_uint_to(&mut prompt[len..], getuid() as u64);
    }
    len += copy(&mut prompt[len..], b"\x1b[0m");

    // @hostname in cyan.
    prompt[len] = b'@';
    len += 1;
    len += copy(&mut prompt[len..], b"\x1b[96m");
    len += copy(&mut prompt[len..], b"m3os");
    len += copy(&mut prompt[len..], b"\x1b[0m");

    // :cwd
    prompt[len] = b':';
    len += 1;
    let mut cwd = [0u8; 256];
    let ret = getcwd(&mut cwd);
    if ret >= 0 {
        let cwd_len = cwd.iter().position(|&b| b == 0).unwrap_or(ret as usize);
        let cl = cwd_len.min(128);
        len += copy(&mut prompt[len..], &cwd[..cl]);
    }

    // # for root, $ for others.
    if getuid() == 0 {
        len += copy(&mut prompt[len..], b"# ");
    } else {
        len += copy(&mut prompt[len..], b"$ ");
    }

    let _ = write(STDOUT_FILENO, &prompt[..len]);
    0
}

fn copy(dst: &mut [u8], src: &[u8]) -> usize {
    let n = src.len().min(dst.len());
    dst[..n].copy_from_slice(&src[..n]);
    n
}

fn write_uint_to(dst: &mut [u8], mut v: u64) -> usize {
    if v == 0 {
        if !dst.is_empty() {
            dst[0] = b'0';
        }
        return 1;
    }
    let mut tmp = [0u8; 20];
    let mut pos = 20;
    while v > 0 {
        pos -= 1;
        tmp[pos] = b'0' + (v % 10) as u8;
        v /= 10;
    }
    let digits = &tmp[pos..];
    let n = digits.len().min(dst.len());
    dst[..n].copy_from_slice(&digits[..n]);
    n
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::exit(101)
}
