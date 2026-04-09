//! Userspace stdin feeder for m3OS (Phase 52, Track E).
//!
//! Obtains scancodes from the `kbd_server` service via IPC (`KBD_READ`),
//! translates them to characters using a US-QWERTY lookup table, implements
//! a line discipline (canonical mode editing, signal characters, echo), and
//! pushes processed bytes into the kernel stdin buffer via `stdin_push`.
//!
//! This is the ring-3 replacement for the kernel-resident `stdin_feeder_task`
//! in `kernel/src/main.rs`.
#![no_std]
#![no_main]

use syscall_lib::{SIGINT, SIGQUIT, SIGTSTP, STDOUT_FILENO, TermiosFlags};

// ---------------------------------------------------------------------------
// Termios flag constants (mirrored from kernel-core/src/tty.rs)
// ---------------------------------------------------------------------------

const ISIG: u32 = 0o000001;
const ICANON: u32 = 0o000002;
const ECHO: u32 = 0o000010;
const ECHOE: u32 = 0o000020;
const ECHOK: u32 = 0o000040;
const ECHONL: u32 = 0o000100;

const ICRNL: u32 = 0o000400;

const ONLCR: u32 = 0o000004;

// c_cc indices
const VINTR: usize = 0;
const VQUIT: usize = 1;
const VERASE: usize = 2;
const VKILL: usize = 3;
const VEOF: usize = 4;
const VSUSP: usize = 10;
const VWERASE: usize = 14;

// ---------------------------------------------------------------------------
// Scancode translation (US-QWERTY, ported from kernel/src/main.rs)
// ---------------------------------------------------------------------------

/// Translate a PS/2 scancode (make code, < 0x80) to an ASCII character.
///
/// Returns `None` for non-printable or unmapped scancodes.
fn scancode_to_char(sc: u8, shift: bool) -> Option<char> {
    let (lo, hi): (Option<char>, Option<char>) = match sc {
        0x02 => (Some('1'), Some('!')),
        0x03 => (Some('2'), Some('@')),
        0x04 => (Some('3'), Some('#')),
        0x05 => (Some('4'), Some('$')),
        0x06 => (Some('5'), Some('%')),
        0x07 => (Some('6'), Some('^')),
        0x08 => (Some('7'), Some('&')),
        0x09 => (Some('8'), Some('*')),
        0x0A => (Some('9'), Some('(')),
        0x0B => (Some('0'), Some(')')),
        0x0C => (Some('-'), Some('_')),
        0x0D => (Some('='), Some('+')),
        0x10 => (Some('q'), Some('Q')),
        0x11 => (Some('w'), Some('W')),
        0x12 => (Some('e'), Some('E')),
        0x13 => (Some('r'), Some('R')),
        0x14 => (Some('t'), Some('T')),
        0x15 => (Some('y'), Some('Y')),
        0x16 => (Some('u'), Some('U')),
        0x17 => (Some('i'), Some('I')),
        0x18 => (Some('o'), Some('O')),
        0x19 => (Some('p'), Some('P')),
        0x1A => (Some('['), Some('{')),
        0x1B => (Some(']'), Some('}')),
        0x1E => (Some('a'), Some('A')),
        0x1F => (Some('s'), Some('S')),
        0x20 => (Some('d'), Some('D')),
        0x21 => (Some('f'), Some('F')),
        0x22 => (Some('g'), Some('G')),
        0x23 => (Some('h'), Some('H')),
        0x24 => (Some('j'), Some('J')),
        0x25 => (Some('k'), Some('K')),
        0x26 => (Some('l'), Some('L')),
        0x27 => (Some(';'), Some(':')),
        0x28 => (Some('\''), Some('"')),
        0x29 => (Some('`'), Some('~')),
        0x2B => (Some('\\'), Some('|')),
        0x2C => (Some('z'), Some('Z')),
        0x2D => (Some('x'), Some('X')),
        0x2E => (Some('c'), Some('C')),
        0x2F => (Some('v'), Some('V')),
        0x30 => (Some('b'), Some('B')),
        0x31 => (Some('n'), Some('N')),
        0x32 => (Some('m'), Some('M')),
        0x33 => (Some(','), Some('<')),
        0x34 => (Some('.'), Some('>')),
        0x35 => (Some('/'), Some('?')),
        0x39 => (Some(' '), Some(' ')),
        _ => (None, None),
    };
    if shift { hi } else { lo }
}

// ---------------------------------------------------------------------------
// Line editing buffer (simplified EditBuffer for userspace)
// ---------------------------------------------------------------------------

struct EditBuffer {
    buf: [u8; 4096],
    len: usize,
}

impl EditBuffer {
    const fn new() -> Self {
        Self {
            buf: [0u8; 4096],
            len: 0,
        }
    }

    fn push(&mut self, b: u8) -> bool {
        if self.len < self.buf.len() {
            self.buf[self.len] = b;
            self.len += 1;
            true
        } else {
            false
        }
    }

    fn erase_char(&mut self) -> Option<u8> {
        if self.len > 0 {
            self.len -= 1;
            Some(self.buf[self.len])
        } else {
            None
        }
    }

    fn kill_line(&mut self) -> usize {
        let n = self.len;
        self.len = 0;
        n
    }

    fn word_erase(&mut self) -> usize {
        let orig = self.len;
        // Skip trailing spaces.
        while self.len > 0 && self.buf[self.len - 1] == b' ' {
            self.len -= 1;
        }
        // Erase non-space characters.
        while self.len > 0 && self.buf[self.len - 1] != b' ' {
            self.len -= 1;
        }
        orig - self.len
    }

    fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn clear(&mut self) {
        self.len = 0;
    }

    fn as_slice(&self) -> &[u8] {
        &self.buf[..self.len]
    }
}

// ---------------------------------------------------------------------------
// Echo helper
// ---------------------------------------------------------------------------

/// Write a string to stdout (fd 1). The kernel's sys_write routes this
/// through the console for display output.
fn echo(s: &str) {
    syscall_lib::write_str(STDOUT_FILENO, s);
}

/// Write a single byte to stdout.
fn echo_byte(b: u8) {
    let buf = [b];
    if let Ok(s) = core::str::from_utf8(&buf) {
        echo(s);
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

syscall_lib::entry_point!(program_main);

/// IPC operation label: read one scancode from kbd_server.
const KBD_READ: u64 = 1;

fn program_main(_args: &[&str]) -> i32 {
    syscall_lib::write_str(STDOUT_FILENO, "stdin_feeder: starting\n");

    // Look up the "kbd" service to obtain an endpoint capability.
    let kbd_handle = syscall_lib::ipc_lookup_service("kbd");
    if kbd_handle == u64::MAX {
        syscall_lib::write_str(STDOUT_FILENO, "stdin_feeder: failed to lookup 'kbd'\n");
        return 1;
    }
    let kbd_handle = kbd_handle as u32;

    syscall_lib::write_str(STDOUT_FILENO, "stdin_feeder: ready\n");

    let mut shift = false;
    let mut ctrl = false;
    let mut edit_buf = EditBuffer::new();

    loop {
        // Request one scancode from kbd_server via IPC.  This blocks until
        // the keyboard service has a scancode ready (it blocks on IRQ1
        // internally).  The scancode is returned as the reply label.
        let sc = syscall_lib::ipc_call(kbd_handle, KBD_READ, 0) as u8;

        // Key-release (break) codes: bit 7 set.
        if sc >= 0x80 {
            let make = sc & 0x7F;
            if make == 0x2A || make == 0x36 {
                shift = false;
            }
            if make == 0x1D {
                ctrl = false;
            }
            continue;
        }

        // Modifier make codes.
        if sc == 0x1D {
            ctrl = true;
            continue;
        }
        if sc == 0x2A || sc == 0x36 {
            shift = true;
            continue;
        }

        // Read current termios flags from the kernel.
        let mut tf = TermiosFlags::zeroed();
        syscall_lib::get_termios_flags(&mut tf);

        let canonical = tf.c_lflag & ICANON != 0;

        // VT100 escape sequences for special keys.
        let escape_seq: Option<&[u8]> = match sc {
            0x48 => Some(b"\x1b[A"),  // Arrow Up
            0x50 => Some(b"\x1b[B"),  // Arrow Down
            0x4D => Some(b"\x1b[C"),  // Arrow Right
            0x4B => Some(b"\x1b[D"),  // Arrow Left
            0x47 => Some(b"\x1b[H"),  // Home
            0x4F => Some(b"\x1b[F"),  // End
            0x53 => Some(b"\x1b[3~"), // Delete
            0x49 => Some(b"\x1b[5~"), // Page Up
            0x51 => Some(b"\x1b[6~"), // Page Down
            0x01 => Some(b"\x1b"),    // Escape
            _ => None,
        };

        if let Some(seq) = escape_seq {
            if canonical {
                // In cooked mode, escape sequences are not useful — skip.
                continue;
            }
            syscall_lib::stdin_push(seq);
            continue;
        }

        // Convert scancode to byte.
        let byte = if sc == 0x1C {
            b'\r' // Enter key produces CR; ICRNL translates to LF
        } else if sc == 0x0F {
            b'\t' // Tab
        } else if sc == 0x0E {
            0x7F // DEL / backspace
        } else if ctrl {
            // Ctrl + letter -> control character (0x01-0x1A).
            match scancode_to_char(sc, false) {
                Some(c) if c.is_ascii_alphabetic() => (c.to_ascii_uppercase() as u8) - b'A' + 1,
                _ => continue,
            }
        } else {
            match scancode_to_char(sc, shift) {
                Some(c) => {
                    let mut buf = [0u8; 4];
                    let s = c.encode_utf8(&mut buf);
                    s.as_bytes()[0]
                }
                None => continue,
            }
        };

        let echo_on = tf.c_lflag & ECHO != 0;
        let isig = tf.c_lflag & ISIG != 0;

        // ICRNL: translate CR to NL on input.
        let byte = if (tf.c_iflag & ICRNL != 0) && byte == b'\r' {
            b'\n'
        } else {
            byte
        };

        // ISIG: check signal characters from c_cc.
        if isig {
            let signal = if byte == tf.c_cc[VINTR] {
                Some((SIGINT as u32, "^C"))
            } else if byte == tf.c_cc[VSUSP] {
                Some((SIGTSTP as u32, "^Z"))
            } else if byte == tf.c_cc[VQUIT] {
                Some((SIGQUIT as u32, "^\\"))
            } else {
                None
            };

            if let Some((sig, name)) = signal {
                // Clear edit buffer in canonical mode.
                if canonical {
                    edit_buf.clear();
                }
                echo(name);
                echo("\n");
                syscall_lib::signal_process_group(sig);
                continue;
            }
        }

        if canonical {
            // Cooked mode: buffer in edit_buf, deliver on newline or EOF.

            // VERASE (backspace/DEL)
            if byte == tf.c_cc[VERASE] || byte == 0x7F {
                let erased = edit_buf.erase_char();
                if erased.is_some() && echo_on && (tf.c_lflag & ECHOE != 0) {
                    echo("\x08 \x08");
                }
                continue;
            }

            // VKILL (^U)
            if byte == tf.c_cc[VKILL] {
                let n = edit_buf.kill_line();
                if n > 0 && echo_on && (tf.c_lflag & ECHOK != 0) {
                    for _ in 0..n {
                        echo("\x08 \x08");
                    }
                }
                continue;
            }

            // VWERASE (^W)
            if byte == tf.c_cc[VWERASE] {
                let n = edit_buf.word_erase();
                if n > 0 && echo_on {
                    for _ in 0..n {
                        echo("\x08 \x08");
                    }
                }
                continue;
            }

            // VEOF (^D)
            if byte == tf.c_cc[VEOF] {
                if edit_buf.is_empty() {
                    syscall_lib::stdin_signal_eof();
                } else {
                    // Non-empty: flush buffer without appending newline.
                    let data = edit_buf.as_slice();
                    syscall_lib::stdin_push(data);
                    edit_buf.clear();
                }
                continue;
            }

            // Newline: deliver line.
            if byte == b'\n' {
                let data = edit_buf.as_slice();
                syscall_lib::stdin_push(data);
                edit_buf.clear();
                let nl = [b'\n'];
                syscall_lib::stdin_push(&nl);

                // Echo newline.
                if echo_on || (tf.c_lflag & ECHONL != 0) {
                    if tf.c_oflag & ONLCR != 0 {
                        echo("\r\n");
                    } else {
                        echo("\n");
                    }
                }
                continue;
            }

            // Regular character: buffer it.
            edit_buf.push(byte);

            if echo_on {
                echo_byte(byte);
            }
        } else {
            // Raw / cbreak mode: push byte immediately.
            let buf = [byte];
            syscall_lib::stdin_push(&buf);

            if echo_on {
                if tf.c_oflag & ONLCR != 0 && byte == b'\n' {
                    echo("\r\n");
                } else {
                    echo_byte(byte);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Panic handler
// ---------------------------------------------------------------------------

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "stdin_feeder: PANIC\n");
    syscall_lib::exit(101)
}
