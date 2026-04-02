//! ln — create hard and symbolic links.
#![no_std]
#![no_main]

use syscall_lib::{STDERR_FILENO, link, symlink, write_str};

syscall_lib::entry_point!(main);

fn main(args: &[&str]) -> i32 {
    let mut symbolic = false;
    let mut positional = [&""; 2];
    let mut positional_count = 0usize;

    for arg in &args[1..] {
        if *arg == "-s" {
            symbolic = true;
        } else if positional_count < positional.len() {
            positional[positional_count] = arg;
            positional_count += 1;
        }
    }

    if positional_count != 2 {
        write_str(STDERR_FILENO, "usage: ln [-s] <target> <link>\n");
        return 1;
    }

    let mut target_buf = [0u8; 256];
    let Some(target) = to_cstr(positional[0].as_bytes(), &mut target_buf) else {
        write_str(STDERR_FILENO, "ln: target path too long\n");
        return 1;
    };
    let mut link_buf = [0u8; 256];
    let Some(linkpath) = to_cstr(positional[1].as_bytes(), &mut link_buf) else {
        write_str(STDERR_FILENO, "ln: link path too long\n");
        return 1;
    };

    let rc = if symbolic {
        symlink(target, linkpath)
    } else {
        link(target, linkpath)
    };
    if rc < 0 {
        write_str(STDERR_FILENO, "ln: link creation failed\n");
        return 1;
    }
    0
}

fn to_cstr<'a>(bytes: &[u8], buf: &'a mut [u8; 256]) -> Option<&'a [u8]> {
    if bytes.len() > 255 {
        return None;
    }
    buf.fill(0);
    buf[..bytes.len()].copy_from_slice(bytes);
    Some(&buf[..=bytes.len()])
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::exit(101)
}
