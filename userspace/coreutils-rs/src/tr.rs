//! tr — translate or delete characters.
#![no_std]
#![no_main]

use syscall_lib::{STDERR_FILENO, STDIN_FILENO, STDOUT_FILENO, read, write, write_str};

syscall_lib::entry_point!(main);

fn parse_escape(c: u8) -> u8 {
    match c {
        b'n' => b'\n',
        b'r' => b'\r',
        b't' => b'\t',
        b'\\' => b'\\',
        _ => c,
    }
}

fn expand_set(spec: &[u8], out: &mut [u8; 256]) -> usize {
    let mut len = 0usize;
    let mut i = 0usize;

    while i < spec.len() && len < 256 {
        let first = if spec[i] == b'\\' && i + 1 < spec.len() {
            let c = parse_escape(spec[i + 1]);
            i += 2;
            c
        } else {
            let c = spec[i];
            i += 1;
            c
        };

        if i < spec.len() && spec[i] == b'-' && i + 1 < spec.len() {
            i += 1;
            let last = if spec[i] == b'\\' && i + 1 < spec.len() {
                let c = parse_escape(spec[i + 1]);
                i += 2;
                c
            } else {
                let c = spec[i];
                i += 1;
                c
            };
            if first <= last {
                let mut ch = first;
                loop {
                    if len >= 256 {
                        break;
                    }
                    out[len] = ch;
                    len += 1;
                    if ch == last {
                        break;
                    }
                    ch += 1;
                }
            } else {
                let mut ch = first;
                loop {
                    if len >= 256 {
                        break;
                    }
                    out[len] = ch;
                    len += 1;
                    if ch == last {
                        break;
                    }
                    ch -= 1;
                }
            }
        } else {
            out[len] = first;
            len += 1;
        }
    }
    len
}

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
    let mut argi = 1usize;
    let mut delete_mode = false;

    if argi < args.len() && args[argi] == "-d" {
        delete_mode = true;
        argi += 1;
    }

    let remaining = args.len() - argi;
    let expected = if delete_mode { 1 } else { 2 };
    if remaining != expected {
        write_str(STDERR_FILENO, "usage: tr [-d] SET1 [SET2]\n");
        return 1;
    }

    let mut set1 = [0u8; 256];
    let set1_len = expand_set(args[argi].as_bytes(), &mut set1);
    if set1_len == 0 {
        write_str(STDERR_FILENO, "usage: tr [-d] SET1 [SET2]\n");
        return 1;
    }

    let mut map = [0i16; 256];
    for (i, slot) in map.iter_mut().enumerate() {
        *slot = i as i16;
    }

    if delete_mode {
        for i in 0..set1_len {
            map[set1[i] as usize] = -1;
        }
    } else {
        let mut set2 = [0u8; 256];
        let set2_len = expand_set(args[argi + 1].as_bytes(), &mut set2);
        if set2_len == 0 {
            write_str(STDERR_FILENO, "usage: tr [-d] SET1 [SET2]\n");
            return 1;
        }
        for i in 0..set1_len {
            let rep = set2[if i < set2_len { i } else { set2_len - 1 }];
            map[set1[i] as usize] = rep as i16;
        }
    }

    let mut in_buf = [0u8; 4096];
    let mut out_buf = [0u8; 4096];
    loop {
        let n = read(STDIN_FILENO, &mut in_buf);
        if n == 0 {
            break;
        }
        if n < 0 {
            return 1;
        }
        let mut out_len = 0usize;
        for &b in &in_buf[..n as usize] {
            let mapped = map[b as usize];
            if mapped >= 0 {
                out_buf[out_len] = mapped as u8;
                out_len += 1;
                if out_len == out_buf.len() {
                    if !write_all(STDOUT_FILENO, &out_buf[..out_len]) {
                        return 1;
                    }
                    out_len = 0;
                }
            }
        }
        if out_len > 0 && !write_all(STDOUT_FILENO, &out_buf[..out_len]) {
            return 1;
        }
    }
    0
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::exit(101)
}
