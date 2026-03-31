//! PTY (pseudo-terminal) pair data structures.
//!
//! Each PTY pair has two ring buffers (master-to-slave and slave-to-master),
//! per-PTY termios settings, window size, an edit buffer for canonical mode,
//! and foreground process group tracking.
//!
//! These types live in kernel-core so they can be unit-tested on the host.

use crate::tty::{EditBuffer, Termios, Winsize};

/// Size of each PTY ring buffer.
pub const PTY_BUF_SIZE: usize = 4096;

/// Maximum number of simultaneous PTY pairs.
pub const MAX_PTYS: usize = 16;

// ---------------------------------------------------------------------------
// PTY ring buffer
// ---------------------------------------------------------------------------

/// A ring buffer for PTY I/O. Same design as `Pipe` but without refcounts
/// (PTY lifecycle is managed separately via master_open/slave_open flags).
pub struct PtyRingBuffer {
    buf: [u8; PTY_BUF_SIZE],
    read_pos: usize,
    count: usize,
}

impl Default for PtyRingBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl PtyRingBuffer {
    /// Create a new empty ring buffer.
    pub const fn new() -> Self {
        PtyRingBuffer {
            buf: [0u8; PTY_BUF_SIZE],
            read_pos: 0,
            count: 0,
        }
    }

    /// Read up to `dst.len()` bytes. Returns number of bytes read.
    pub fn read(&mut self, dst: &mut [u8]) -> usize {
        let to_read = dst.len().min(self.count);
        for (i, byte) in dst.iter_mut().enumerate().take(to_read) {
            *byte = self.buf[(self.read_pos + i) % PTY_BUF_SIZE];
        }
        self.read_pos = (self.read_pos + to_read) % PTY_BUF_SIZE;
        self.count -= to_read;
        to_read
    }

    /// Write up to `src.len()` bytes. Returns number of bytes written.
    pub fn write(&mut self, src: &[u8]) -> usize {
        let space = PTY_BUF_SIZE - self.count;
        let to_write = src.len().min(space);
        let write_pos = (self.read_pos + self.count) % PTY_BUF_SIZE;
        for (i, &byte) in src.iter().enumerate().take(to_write) {
            self.buf[(write_pos + i) % PTY_BUF_SIZE] = byte;
        }
        self.count += to_write;
        to_write
    }

    /// Returns true if the buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Returns true if the buffer is full.
    pub fn is_full(&self) -> bool {
        self.count == PTY_BUF_SIZE
    }

    /// Number of bytes available for reading.
    pub fn available(&self) -> usize {
        self.count
    }

    /// Number of bytes that can be written before the buffer is full.
    pub fn space(&self) -> usize {
        PTY_BUF_SIZE - self.count
    }

    /// Discard all buffered data.
    pub fn clear(&mut self) {
        self.read_pos = 0;
        self.count = 0;
    }
}

// ---------------------------------------------------------------------------
// PTY pair state
// ---------------------------------------------------------------------------

/// State for a single PTY master/slave pair.
pub struct PtyPairState {
    /// Master-to-slave buffer: master writes → slave reads.
    pub m2s: PtyRingBuffer,
    /// Slave-to-master buffer: slave writes → master reads.
    pub s2m: PtyRingBuffer,
    /// Slave-side terminal settings (cooked mode by default).
    pub termios: Termios,
    /// Terminal window size.
    pub winsize: Winsize,
    /// Slave-side line discipline edit buffer.
    pub edit_buf: EditBuffer,
    /// Foreground process group on the slave side.
    pub slave_fg_pgid: u32,
    /// Number of open FD references to the master side.
    pub master_refcount: u32,
    /// Number of open FD references to the slave side.
    pub slave_refcount: u32,
    /// True when ^D was pressed on an empty edit buffer (EOF pending).
    pub eof_pending: bool,
    /// PTY lock — slave cannot be opened until unlocked via TIOCSPTLCK(0).
    pub locked: bool,
    /// True once slave_refcount has been > 0 at least once.
    /// Used to distinguish "slave never opened" from "slave closed".
    pub slave_opened: bool,
}

impl PtyPairState {
    /// Create a new PTY pair with default settings.
    pub fn new(_id: u32) -> Self {
        PtyPairState {
            m2s: PtyRingBuffer::new(),
            s2m: PtyRingBuffer::new(),
            termios: Termios::default_cooked(),
            winsize: Winsize::default_console(),
            edit_buf: EditBuffer::new(),
            slave_fg_pgid: 0,
            master_refcount: 1,
            slave_refcount: 0,
            eof_pending: false,
            locked: true,
            slave_opened: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- PtyRingBuffer tests --

    #[test]
    fn ring_buffer_read_write() {
        let mut rb = PtyRingBuffer::new();
        assert!(rb.is_empty());
        assert!(!rb.is_full());
        assert_eq!(rb.available(), 0);
        assert_eq!(rb.space(), PTY_BUF_SIZE);

        let written = rb.write(b"hello");
        assert_eq!(written, 5);
        assert_eq!(rb.available(), 5);
        assert!(!rb.is_empty());

        let mut buf = [0u8; 16];
        let read = rb.read(&mut buf);
        assert_eq!(read, 5);
        assert_eq!(&buf[..5], b"hello");
        assert!(rb.is_empty());
    }

    #[test]
    fn ring_buffer_wraparound() {
        let mut rb = PtyRingBuffer::new();
        // Fill most of the buffer
        let data = [0xAA; PTY_BUF_SIZE - 10];
        rb.write(&data);
        // Read it all
        let mut sink = [0u8; PTY_BUF_SIZE];
        rb.read(&mut sink);
        assert!(rb.is_empty());

        // Write across the wrap boundary
        let wrap_data = [0xBB; 20];
        let written = rb.write(&wrap_data);
        assert_eq!(written, 20);

        let mut out = [0u8; 20];
        let read = rb.read(&mut out);
        assert_eq!(read, 20);
        assert_eq!(out, [0xBB; 20]);
    }

    #[test]
    fn ring_buffer_full() {
        let mut rb = PtyRingBuffer::new();
        let data = [0u8; PTY_BUF_SIZE];
        let written = rb.write(&data);
        assert_eq!(written, PTY_BUF_SIZE);
        assert!(rb.is_full());
        assert_eq!(rb.space(), 0);

        // Writing to full buffer returns 0
        assert_eq!(rb.write(b"x"), 0);
    }

    #[test]
    fn ring_buffer_partial_read() {
        let mut rb = PtyRingBuffer::new();
        rb.write(b"abcdefgh");

        let mut small = [0u8; 3];
        let read = rb.read(&mut small);
        assert_eq!(read, 3);
        assert_eq!(&small, b"abc");

        let mut rest = [0u8; 16];
        let read = rb.read(&mut rest);
        assert_eq!(read, 5);
        assert_eq!(&rest[..5], b"defgh");
    }

    #[test]
    fn ring_buffer_partial_write() {
        let mut rb = PtyRingBuffer::new();
        let data = [0u8; PTY_BUF_SIZE - 5];
        rb.write(&data);

        let written = rb.write(b"abcdefghij");
        assert_eq!(written, 5);
    }

    #[test]
    fn ring_buffer_zero_length() {
        let mut rb = PtyRingBuffer::new();
        assert_eq!(rb.write(b""), 0);
        assert!(rb.is_empty());

        rb.write(b"data");
        let mut empty = [0u8; 0];
        assert_eq!(rb.read(&mut empty), 0);
    }

    // -- PtyPairState tests --

    #[test]
    fn pair_state_defaults() {
        let pair = PtyPairState::new(0);
        assert_eq!(pair.master_refcount, 1);
        assert_eq!(pair.slave_refcount, 0);
        assert!(pair.locked);
        assert!(pair.m2s.is_empty());
        assert!(pair.s2m.is_empty());
        assert_eq!(pair.slave_fg_pgid, 0);
        assert!(pair.termios.is_canonical());
        assert!(pair.termios.is_echo());
        assert_eq!(pair.winsize.ws_row, 24);
        assert_eq!(pair.winsize.ws_col, 80);
    }
}
