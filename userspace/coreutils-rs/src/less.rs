//! less — interactive pager.
#![no_std]
#![no_main]

#[path = "common.rs"]
mod common;

use common::{contains, write_all};
use syscall_lib::{
    ECHO, ICANON, O_RDONLY, O_RDWR, STDERR_FILENO, STDOUT_FILENO, Termios, close, open, read,
    tcgetattr, tcsetattr, write_str,
};

syscall_lib::entry_point!(main);

const MAX_CONTENT: usize = 65536;
const MAX_LINES: usize = 2048;

// VMIN index = 6, VTIME index = 5 in c_cc
const VMIN_IDX: usize = 6;
const VTIME_IDX: usize = 5;

// Synthetic key codes for arrow/page keys
const KEY_UP: i32 = 1001;
const KEY_DOWN: i32 = 1002;
const KEY_PGUP: i32 = 1003;
const KEY_PGDN: i32 = 1004;

struct RawMode {
    fd: i32,
    saved: Termios,
}

impl RawMode {
    fn enter(fd: i32) -> Result<Self, isize> {
        let saved = tcgetattr(fd)?;
        let mut raw = saved;
        raw.c_lflag &= !(ICANON | ECHO);
        raw.c_cc[VMIN_IDX] = 1;
        raw.c_cc[VTIME_IDX] = 0;
        tcsetattr(fd, &raw)?;
        Ok(RawMode { fd, saved })
    }

    fn restore(&self) {
        let _ = tcsetattr(self.fd, &self.saved);
    }
}

fn read_key(fd: i32) -> i32 {
    let mut ch = [0u8; 1];
    if read(fd, &mut ch) != 1 {
        return -1;
    }
    if ch[0] != 27 {
        return ch[0] as i32;
    }
    let mut seq = [0u8; 3];
    if read(fd, &mut seq[..1]) != 1 {
        return 27;
    }
    if seq[0] != b'[' {
        return 27;
    }
    if read(fd, &mut seq[1..2]) != 1 {
        return 27;
    }
    match seq[1] {
        b'A' => KEY_UP,
        b'B' => KEY_DOWN,
        b'5' => {
            let mut tilde = [0u8; 1];
            let _ = read(fd, &mut tilde);
            KEY_PGUP
        }
        b'6' => {
            let mut tilde = [0u8; 1];
            let _ = read(fd, &mut tilde);
            KEY_PGDN
        }
        _ => 27,
    }
}

fn write_u32_dec(fd: i32, mut v: u32) {
    if v == 0 {
        write_all(fd, b"0");
        return;
    }
    let mut buf = [0u8; 10];
    let mut i = 10usize;
    while v > 0 {
        i -= 1;
        buf[i] = b'0' + (v % 10) as u8;
        v /= 10;
    }
    write_all(fd, &buf[i..]);
}

fn draw(
    content: &[u8],
    line_starts: &[u32],
    total_lines: usize,
    top: usize,
    rows: usize,
    filename: &[u8],
) {
    // Clear screen, cursor home
    write_all(STDOUT_FILENO, b"\x1b[2J\x1b[H");

    let display_rows = if rows > 1 { rows - 1 } else { 1 };
    let end = if top + display_rows < total_lines {
        top + display_rows
    } else {
        total_lines
    };

    for i in top..end {
        let start = line_starts[i] as usize;
        let line_end = if i + 1 < total_lines {
            line_starts[i + 1] as usize
        } else {
            content.len()
        };
        write_all(STDOUT_FILENO, &content[start..line_end]);
        // Ensure newline if not present
        if line_end > start && content[line_end - 1] != b'\n' {
            write_all(STDOUT_FILENO, b"\n");
        }
    }

    // Status bar: reverse video
    write_all(STDOUT_FILENO, b"\x1b[7m");
    write_all(STDOUT_FILENO, filename);
    write_all(STDOUT_FILENO, b"  ");
    write_u32_dec(STDOUT_FILENO, (top + 1) as u32);
    write_all(STDOUT_FILENO, b"/");
    write_u32_dec(STDOUT_FILENO, total_lines as u32);
    write_all(
        STDOUT_FILENO,
        b"  (q quit  j/k scroll  space/b page  / search)",
    );
    write_all(STDOUT_FILENO, b"\x1b[K\x1b[0m");
}

fn search_forward(
    content: &[u8],
    line_starts: &[u32],
    total_lines: usize,
    from: usize,
    needle: &[u8],
) -> Option<usize> {
    for i in from..total_lines {
        let start = line_starts[i] as usize;
        let end = if i + 1 < total_lines {
            line_starts[i + 1] as usize
        } else {
            content.len()
        };
        if contains(&content[start..end], needle) {
            return Some(i);
        }
    }
    None
}

fn read_file_into(fd: i32, buf: &mut [u8]) -> Option<usize> {
    let mut fill = 0usize;
    loop {
        let space = buf.len() - fill;
        if space == 0 {
            return None; // file too large
        }
        let n = read(fd, &mut buf[fill..]);
        if n == 0 {
            break;
        }
        if n < 0 {
            return None;
        }
        fill += n as usize;
    }
    Some(fill)
}

fn collect_lines(buf: &[u8], fill: usize, starts: &mut [u32; MAX_LINES]) -> usize {
    if fill == 0 {
        return 0;
    }
    let mut count = 0usize;
    if count < MAX_LINES {
        starts[count] = 0;
        count += 1;
    }
    for (i, &byte) in buf[..fill].iter().enumerate() {
        if byte == b'\n' {
            let next = i + 1;
            if next < fill && count < MAX_LINES {
                starts[count] = next as u32;
                count += 1;
            }
        }
    }
    count
}

fn main(args: &[&str]) -> i32 {
    // Determine input source
    let (input_fd, filename_bytes) = if args.len() >= 2 {
        let arg = args[1].as_bytes();
        if arg.len() > 254 {
            write_str(STDERR_FILENO, "less: path too long\n");
            return 1;
        }
        let mut path_buf = [0u8; 256];
        path_buf[..arg.len()].copy_from_slice(arg);
        path_buf[arg.len()] = 0;
        let fd = open(&path_buf[..=arg.len()], O_RDONLY, 0);
        if fd < 0 {
            write_str(STDERR_FILENO, "less: cannot open file\n");
            return 1;
        }
        (fd as i32, arg)
    } else {
        (0i32, b"stdin" as &[u8])
    };

    // Read content
    let mut content = [0u8; MAX_CONTENT];
    let fill = match read_file_into(input_fd, &mut content) {
        Some(n) => n,
        None => {
            write_str(STDERR_FILENO, "less: file too large or read error\n");
            if input_fd != 0 {
                close(input_fd);
            }
            return 1;
        }
    };
    if input_fd != 0 {
        close(input_fd);
    }

    // Collect line starts
    let mut line_starts = [0u32; MAX_LINES];
    let total_lines = collect_lines(&content, fill, &mut line_starts);

    if total_lines == 0 {
        return 0;
    }

    // Open tty for interactive input
    let tty_fd = open(b"/dev/tty\0", O_RDWR, 0);
    let input_fd = if tty_fd >= 0 { tty_fd as i32 } else { 0i32 };

    // Enter raw mode
    let raw = match RawMode::enter(input_fd) {
        Ok(r) => r,
        Err(_) => {
            // Not a terminal — just dump content and exit
            write_all(STDOUT_FILENO, &content[..fill]);
            if tty_fd >= 0 {
                close(tty_fd as i32);
            }
            return 0;
        }
    };

    // Get window size
    let (rows, _cols) = match syscall_lib::get_window_size(STDOUT_FILENO) {
        Ok(sz) => (sz.0 as usize, sz.1 as usize),
        Err(_) => (24usize, 80usize),
    };
    let page_size = if rows > 1 { rows - 1 } else { 1 };

    let mut top: usize = 0;
    let mut search_buf = [0u8; 128];
    let mut search_len: usize = 0;
    let mut searching = false;

    draw(
        &content,
        &line_starts,
        total_lines,
        top,
        rows,
        filename_bytes,
    );

    loop {
        if searching {
            // Read search text character by character
            let k = read_key(input_fd);
            if k == b'\n' as i32 || k == b'\r' as i32 {
                // Execute search
                searching = false;
                if search_len > 0 {
                    let needle = &search_buf[..search_len];
                    if let Some(found) =
                        search_forward(&content, &line_starts, total_lines, top + 1, needle)
                    {
                        top = found;
                    }
                }
                draw(
                    &content,
                    &line_starts,
                    total_lines,
                    top,
                    rows,
                    filename_bytes,
                );
            } else if k == 27 {
                // Cancel search
                searching = false;
                draw(
                    &content,
                    &line_starts,
                    total_lines,
                    top,
                    rows,
                    filename_bytes,
                );
            } else if k == 127 || k == 8 {
                // Backspace
                search_len = search_len.saturating_sub(1);
                // Redraw status with partial search
                write_all(STDOUT_FILENO, b"\x1b[7m/");
                write_all(STDOUT_FILENO, &search_buf[..search_len]);
                write_all(STDOUT_FILENO, b"\x1b[K\x1b[0m");
            } else if k > 0 && k < 128 {
                if search_len < search_buf.len() {
                    search_buf[search_len] = k as u8;
                    search_len += 1;
                }
                write_all(STDOUT_FILENO, b"\x1b[7m/");
                write_all(STDOUT_FILENO, &search_buf[..search_len]);
                write_all(STDOUT_FILENO, b"\x1b[K\x1b[0m");
            }
            continue;
        }

        let k = read_key(input_fd);
        match k {
            -1 => break,
            k if k == b'q' as i32 || k == b'Q' as i32 => break,
            k if k == b'j' as i32 || k == b'J' as i32 || k == KEY_DOWN => {
                if top + 1 < total_lines {
                    top += 1;
                }
                draw(
                    &content,
                    &line_starts,
                    total_lines,
                    top,
                    rows,
                    filename_bytes,
                );
            }
            k if k == b'k' as i32 || k == b'K' as i32 || k == KEY_UP => {
                top = top.saturating_sub(1);
                draw(
                    &content,
                    &line_starts,
                    total_lines,
                    top,
                    rows,
                    filename_bytes,
                );
            }
            k if k == b' ' as i32 || k == KEY_PGDN => {
                top = (top + page_size).min(total_lines.saturating_sub(1));
                draw(
                    &content,
                    &line_starts,
                    total_lines,
                    top,
                    rows,
                    filename_bytes,
                );
            }
            k if k == b'b' as i32 || k == KEY_PGUP => {
                top = top.saturating_sub(page_size);
                draw(
                    &content,
                    &line_starts,
                    total_lines,
                    top,
                    rows,
                    filename_bytes,
                );
            }
            k if k == b'/' as i32 => {
                searching = true;
                search_len = 0;
                // Show search prompt in status bar
                write_all(STDOUT_FILENO, b"\x1b[7m/\x1b[K\x1b[0m");
            }
            _ => {}
        }
    }

    // Restore terminal and clear screen
    raw.restore();
    write_all(STDOUT_FILENO, b"\x1b[2J\x1b[H");

    if tty_fd >= 0 {
        close(tty_fd as i32);
    }
    0
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::exit(101)
}
