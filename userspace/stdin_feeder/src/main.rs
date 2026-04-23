//! Userspace stdin feeder for m3OS (Phase 52d, Track C).
//!
//! Obtains scancodes from the `kbd_server` service via IPC (`KBD_READ`),
//! translates them to raw bytes using a US-QWERTY lookup table, and forwards
//! each byte to the kernel via `push_raw_input`.
//!
//! All terminal policy (canonical editing, echo, signal generation, ICRNL)
//! is handled by the kernel-side `LineDiscipline` in `push_raw_input`.
//! This binary is a pure scancode-to-byte bridge.
#![no_std]
#![no_main]

use syscall_lib::STDOUT_FILENO;

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
// Entry point
// ---------------------------------------------------------------------------

syscall_lib::entry_point!(program_main);

/// IPC operation label: read one scancode from kbd_server.
const KBD_READ: u64 = 1;

fn lookup_kbd_service() -> u32 {
    loop {
        let handle = syscall_lib::ipc_lookup_service("kbd");
        if handle != u64::MAX {
            return handle as u32;
        }
        let _ = syscall_lib::nanosleep_for(0, 20_000_000); // 20 ms
    }
}

fn program_main(_args: &[&str]) -> i32 {
    syscall_lib::write_str(STDOUT_FILENO, "stdin_feeder: starting\n");

    // Look up the "kbd" service to obtain an endpoint capability.
    // Retry indefinitely because service state is "running" as soon as init
    // forks the task, which still races service-registry publication.
    let mut kbd_handle = lookup_kbd_service();

    syscall_lib::write_str(STDOUT_FILENO, "stdin_feeder: ready\n");

    let mut shift = false;
    let mut ctrl = false;

    loop {
        // Request one scancode from kbd_server via IPC.  This blocks until
        // the keyboard service has a scancode ready (it blocks on IRQ1
        // internally).  The scancode is returned as the reply label.
        let sc_rc = syscall_lib::ipc_call(kbd_handle, KBD_READ, 0);
        if sc_rc == u64::MAX {
            kbd_handle = lookup_kbd_service();
            continue;
        }
        let sc = sc_rc as u8;

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

        // VT100 escape sequences for special keys — forward each byte
        // through the kernel line discipline.
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
            for &b in seq {
                syscall_lib::push_raw_input(b);
            }
            continue;
        }

        // Convert scancode to a raw byte.
        let byte = if sc == 0x1C {
            b'\r' // Enter key produces CR; kernel ICRNL translates to LF
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

        // Forward raw byte to the kernel line discipline.
        syscall_lib::push_raw_input(byte);
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
