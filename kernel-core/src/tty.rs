//! TTY data structures matching the Linux x86_64 ABI.
//!
//! These types live in kernel-core so they can be unit-tested on the host
//! (`cargo test -p kernel-core`) without needing QEMU.

/// Number of control characters in a `Termios` struct.
pub const NCCS: usize = 19;

// ---------------------------------------------------------------------------
// Termios struct — Linux x86_64 layout (60 bytes)
// ---------------------------------------------------------------------------

/// Terminal I/O settings, binary-compatible with Linux `struct termios`.
///
/// Layout: c_iflag(4) + c_oflag(4) + c_cflag(4) + c_lflag(4) + c_line(1)
///         + c_cc(19) + padding(24 implicit from repr(C)) = 60 bytes total.
///
/// We use `repr(C)` so field order and alignment match the C ABI.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Termios {
    /// Input mode flags.
    pub c_iflag: u32,
    /// Output mode flags.
    pub c_oflag: u32,
    /// Control mode flags.
    pub c_cflag: u32,
    /// Local mode flags.
    pub c_lflag: u32,
    /// Line discipline (unused, always 0).
    pub c_line: u8,
    /// Control characters.
    pub c_cc: [u8; NCCS],
}

// Linux `struct termios` on x86_64 is exactly 60 bytes:
//   4 (c_iflag) + 4 (c_oflag) + 4 (c_cflag) + 4 (c_lflag) + 1 (c_line)
//   + 19 (c_cc) + 0 padding before next field ... but there is no next
//   field, and the overall struct size is rounded to alignment of the
//   largest member (u32 = 4 bytes), so 4+4+4+4+1+19 = 36, rounded up
//   to 36 (already aligned).  However, the Linux kernel header yields 60
//   bytes because the *kernel* `struct termios` includes c_ispeed and
//   c_ospeed (each u32) plus padding.  musl's `struct termios` is 60
//   bytes: it stores `__c_cc` as `cc_t[32]` (32 bytes) giving
//   4+4+4+4+1+pad(3)+32+4+4 = 60.
//
// We match musl's layout for binary compatibility:
//   The actual on-the-wire format that musl passes through ioctl(TCGETS)
//   is the *kernel* `struct termios` which is 36 bytes on x86_64.
//   But musl's TCGETS ioctl number is 0x5401 which maps to the 36-byte
//   kernel struct.  So we use 36 bytes for the ioctl copy.
//
// After testing, we'll assert the correct size.  For now, keep it simple:
// the ioctl handlers copy exactly `TERMIOS_SIZE` bytes.

/// Size of the termios struct as seen by the kernel ioctl interface.
/// On Linux x86_64, `ioctl(fd, TCGETS, &t)` copies 36 bytes.
pub const TERMIOS_SIZE: usize = 36;

// ---------------------------------------------------------------------------
// Winsize struct — Linux layout (8 bytes)
// ---------------------------------------------------------------------------

/// Terminal window size, binary-compatible with Linux `struct winsize`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Winsize {
    pub ws_row: u16,
    pub ws_col: u16,
    pub ws_xpixel: u16,
    pub ws_ypixel: u16,
}

/// Size of the winsize struct for ioctl copy.
pub const WINSIZE_SIZE: usize = 8;

// ---------------------------------------------------------------------------
// c_lflag constants
// ---------------------------------------------------------------------------

pub const ISIG: u32 = 0o000001;
pub const ICANON: u32 = 0o000002;
pub const ECHO: u32 = 0o000010;
pub const ECHOE: u32 = 0o000020;
pub const ECHOK: u32 = 0o000040;
pub const ECHONL: u32 = 0o000100;
pub const IEXTEN: u32 = 0o100000;

// ---------------------------------------------------------------------------
// c_iflag constants
// ---------------------------------------------------------------------------

pub const ICRNL: u32 = 0o000400;
pub const INLCR: u32 = 0o000100;
pub const IGNCR: u32 = 0o000200;

// ---------------------------------------------------------------------------
// c_oflag constants
// ---------------------------------------------------------------------------

pub const OPOST: u32 = 0o000001;
pub const ONLCR: u32 = 0o000004;

// ---------------------------------------------------------------------------
// c_cflag constants (minimal set for defaults)
// ---------------------------------------------------------------------------

/// Baud rate mask — not meaningful for virtual consoles but needed for
/// a valid default.
pub const B38400: u32 = 0o000017;
/// Character size mask.
pub const CS8: u32 = 0o000060;
/// Enable receiver.
pub const CREAD: u32 = 0o000200;
/// Hang up on last close.
pub const HUPCL: u32 = 0o002000;

// ---------------------------------------------------------------------------
// c_cc index constants (Linux x86_64 values)
// ---------------------------------------------------------------------------

pub const VINTR: usize = 0;
pub const VQUIT: usize = 1;
pub const VERASE: usize = 2;
pub const VKILL: usize = 3;
pub const VEOF: usize = 4;
pub const VTIME: usize = 5;
pub const VMIN: usize = 6;
pub const VSTART: usize = 8;
pub const VSTOP: usize = 9;
pub const VSUSP: usize = 10;
pub const VEOL: usize = 11;
pub const VWERASE: usize = 14;
pub const VLNEXT: usize = 15;

// ---------------------------------------------------------------------------
// Default termios constructor
// ---------------------------------------------------------------------------

impl Termios {
    /// Create a termios with sensible cooked-mode defaults matching Linux.
    pub const fn default_cooked() -> Self {
        let mut c_cc = [0u8; NCCS];
        c_cc[VINTR] = 0x03; // ^C
        c_cc[VQUIT] = 0x1C; // ^\
        c_cc[VERASE] = 0x7F; // DEL
        c_cc[VKILL] = 0x15; // ^U
        c_cc[VEOF] = 0x04; // ^D
        c_cc[VTIME] = 0;
        c_cc[VMIN] = 1;
        c_cc[VSTART] = 0x11; // ^Q (XON)
        c_cc[VSTOP] = 0x13; // ^S (XOFF)
        c_cc[VSUSP] = 0x1A; // ^Z
        c_cc[VEOL] = 0;
        c_cc[VWERASE] = 0x17; // ^W
        c_cc[VLNEXT] = 0x16; // ^V

        Termios {
            c_iflag: ICRNL,
            c_oflag: OPOST | ONLCR,
            c_cflag: B38400 | CS8 | CREAD | HUPCL,
            c_lflag: ICANON | ECHO | ECHOE | ISIG | IEXTEN,
            c_line: 0,
            c_cc,
        }
    }

    /// Returns true if ICANON is set (cooked / canonical mode).
    pub const fn is_canonical(&self) -> bool {
        self.c_lflag & ICANON != 0
    }

    /// Returns true if ECHO is set.
    pub const fn is_echo(&self) -> bool {
        self.c_lflag & ECHO != 0
    }

    /// Returns true if ISIG is set (signal characters enabled).
    pub const fn is_isig(&self) -> bool {
        self.c_lflag & ISIG != 0
    }
}

impl Winsize {
    /// Default 24x80 terminal.
    pub const fn default_console() -> Self {
        Winsize {
            ws_row: 24,
            ws_col: 80,
            ws_xpixel: 0,
            ws_ypixel: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Line discipline — pure logic (no I/O, testable on host)
// ---------------------------------------------------------------------------

/// Edit buffer for canonical mode line editing.
pub struct EditBuffer {
    pub buf: [u8; 4096],
    pub len: usize,
}

impl Default for EditBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl EditBuffer {
    pub const fn new() -> Self {
        EditBuffer {
            buf: [0u8; 4096],
            len: 0,
        }
    }

    /// Push a byte into the edit buffer. Returns false if full.
    pub fn push(&mut self, b: u8) -> bool {
        if self.len < self.buf.len() {
            self.buf[self.len] = b;
            self.len += 1;
            true
        } else {
            false
        }
    }

    /// Erase the last character. Returns the erased byte or None.
    pub fn erase_char(&mut self) -> Option<u8> {
        if self.len > 0 {
            self.len -= 1;
            Some(self.buf[self.len])
        } else {
            None
        }
    }

    /// Kill (erase) the entire line. Returns the number of characters erased.
    pub fn kill_line(&mut self) -> usize {
        let n = self.len;
        self.len = 0;
        n
    }

    /// Word erase: erase back to previous whitespace boundary.
    /// Returns the number of characters erased.
    pub fn word_erase(&mut self) -> usize {
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

    /// Get the current contents as a slice.
    pub fn as_slice(&self) -> &[u8] {
        &self.buf[..self.len]
    }

    /// Clear the buffer.
    pub fn clear(&mut self) {
        self.len = 0;
    }

    /// Check if the buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Remove the first `n` bytes from the buffer, shifting remaining bytes.
    pub fn drain(&mut self, n: usize) {
        let n = n.min(self.len);
        if n < self.len {
            self.buf.copy_within(n..self.len, 0);
        }
        self.len -= n;
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem;

    #[test]
    fn termios_size() {
        // The repr(C) struct should be 36 bytes (matching Linux kernel termios).
        assert_eq!(mem::size_of::<Termios>(), TERMIOS_SIZE);
    }

    #[test]
    fn winsize_size() {
        assert_eq!(mem::size_of::<Winsize>(), WINSIZE_SIZE);
    }

    #[test]
    fn termios_field_offsets() {
        // Verify field offsets match Linux ABI.
        let t = Termios::default_cooked();
        let base = &t as *const _ as usize;
        assert_eq!(&t.c_iflag as *const _ as usize - base, 0);
        assert_eq!(&t.c_oflag as *const _ as usize - base, 4);
        assert_eq!(&t.c_cflag as *const _ as usize - base, 8);
        assert_eq!(&t.c_lflag as *const _ as usize - base, 12);
        assert_eq!(&t.c_line as *const _ as usize - base, 16);
        // c_cc starts at offset 17
        assert_eq!(&t.c_cc as *const _ as usize - base, 17);
    }

    #[test]
    fn default_cooked_flags() {
        let t = Termios::default_cooked();
        assert!(t.is_canonical());
        assert!(t.is_echo());
        assert!(t.is_isig());
        assert_eq!(t.c_cc[VINTR], 0x03);
        assert_eq!(t.c_cc[VEOF], 0x04);
        assert_eq!(t.c_cc[VERASE], 0x7F);
        assert_eq!(t.c_cc[VKILL], 0x15);
        assert_eq!(t.c_cc[VSUSP], 0x1A);
        assert_eq!(t.c_cc[VWERASE], 0x17);
    }

    #[test]
    fn edit_buffer_push_and_erase() {
        let mut eb = EditBuffer::new();
        assert!(eb.is_empty());
        eb.push(b'h');
        eb.push(b'e');
        eb.push(b'l');
        assert_eq!(eb.as_slice(), b"hel");

        assert_eq!(eb.erase_char(), Some(b'l'));
        assert_eq!(eb.as_slice(), b"he");
    }

    #[test]
    fn edit_buffer_kill_line() {
        let mut eb = EditBuffer::new();
        eb.push(b'a');
        eb.push(b'b');
        eb.push(b'c');
        assert_eq!(eb.kill_line(), 3);
        assert!(eb.is_empty());
    }

    #[test]
    fn edit_buffer_word_erase() {
        let mut eb = EditBuffer::new();
        for &b in b"hello world" {
            eb.push(b);
        }
        // Should erase "world" (5 chars).
        assert_eq!(eb.word_erase(), 5);
        assert_eq!(eb.as_slice(), b"hello ");

        // Should erase " " then "hello" (6 chars).
        assert_eq!(eb.word_erase(), 6);
        assert!(eb.is_empty());
    }

    #[test]
    fn edit_buffer_erase_empty() {
        let mut eb = EditBuffer::new();
        assert_eq!(eb.erase_char(), None);
        assert_eq!(eb.kill_line(), 0);
        assert_eq!(eb.word_erase(), 0);
    }
}
