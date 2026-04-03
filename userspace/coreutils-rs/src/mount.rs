//! mount — mount a filesystem or show current mounts.
#![no_std]
#![no_main]

use syscall_lib::{
    O_RDONLY, STDERR_FILENO, STDOUT_FILENO, close, mount, open, read, write, write_str,
};

syscall_lib::entry_point!(main);

fn main(args: &[&str]) -> i32 {
    // No arguments: print /proc/mounts.
    if args.len() == 1 {
        return show_mounts();
    }

    // Parse: mount -t TYPE SOURCE TARGET
    let mut argi = 1usize;
    let mut fstype: Option<&str> = None;

    if argi < args.len() && args[argi] == "-t" {
        argi += 1;
        if argi >= args.len() {
            write_str(STDERR_FILENO, "usage: mount -t TYPE SOURCE TARGET\n");
            return 1;
        }
        fstype = Some(args[argi]);
        argi += 1;
    }

    if args.len() - argi != 2 || fstype.is_none() {
        write_str(STDERR_FILENO, "usage: mount -t TYPE SOURCE TARGET\n");
        return 1;
    }

    let source_str = args[argi];
    let target_str = args[argi + 1];
    let fstype_str = fstype.unwrap();

    let src_bytes = source_str.as_bytes();
    let tgt_bytes = target_str.as_bytes();
    let fst_bytes = fstype_str.as_bytes();

    if src_bytes.len() > 255 || tgt_bytes.len() > 255 || fst_bytes.len() > 63 {
        write_str(STDERR_FILENO, "mount: argument too long\n");
        return 1;
    }

    let mut source = [0u8; 256];
    let mut target = [0u8; 256];
    let mut fstype_buf = [0u8; 64];

    source[..src_bytes.len()].copy_from_slice(src_bytes);
    source[src_bytes.len()] = 0;
    target[..tgt_bytes.len()].copy_from_slice(tgt_bytes);
    target[tgt_bytes.len()] = 0;
    fstype_buf[..fst_bytes.len()].copy_from_slice(fst_bytes);
    fstype_buf[fst_bytes.len()] = 0;

    if mount(source.as_ptr(), target.as_ptr(), fstype_buf.as_ptr()) != 0 {
        write_str(STDERR_FILENO, "mount: failed\n");
        return 1;
    }
    0
}

fn show_mounts() -> i32 {
    let path = b"/proc/mounts\0";
    let fd = open(path, O_RDONLY, 0);
    if fd < 0 {
        write_str(STDERR_FILENO, "mount: cannot open /proc/mounts\n");
        return 1;
    }
    let fd = fd as i32;
    let mut buf = [0u8; 512];
    loop {
        let n = read(fd, &mut buf);
        if n == 0 {
            break;
        }
        if n < 0 {
            write_str(STDERR_FILENO, "mount: read error\n");
            close(fd);
            return 1;
        }
        let mut off = 0usize;
        let n = n as usize;
        while off < n {
            let w = write(STDOUT_FILENO, &buf[off..n]);
            if w <= 0 {
                close(fd);
                return 1;
            }
            off += w as usize;
        }
    }
    close(fd);
    0
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::exit(101)
}
