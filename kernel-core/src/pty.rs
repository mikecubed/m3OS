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
    /// Phase 69a Track C: IXON output suspension state.  Set when VSTOP
    /// arrives in the master→slave stream and IXON is on; cleared on
    /// VSTART.  Slave reads/writes consult this flag if they want to honour
    /// software flow control.
    pub ldisc_output_suspended: bool,
    /// Phase 69a Track F: VTIME deadline (absolute kernel ticks).  `None`
    /// when the timer is not armed.  Set when the slave read path arms the
    /// VMIN/VTIME timer; cleared when the buffer drains.
    pub ldisc_deadline_ticks: Option<u64>,
}

impl PtyPairState {
    /// Readiness predicate for a **slave-side** `write(2)` that stalled with a
    /// full `s2m` ring — see the `FdBackend::PtySlave` write arm.
    ///
    /// `need` is the space the *stalling* byte requires: 1 for an ordinary
    /// byte, 2 for a `\n` that OPOST+ONLCR must expand to `\r\n` atomically.
    /// Blocking on `space() >= need` rather than on the coarser
    /// `!s2m.is_full()` is what keeps the write path from live-spinning: with
    /// exactly one free slot and a pending newline, "not full" is true but the
    /// retry still cannot make progress.
    ///
    /// `master_refcount == 0` is part of the predicate so a master that closes
    /// while a slave writer sleeps releases it (the write arm then returns
    /// `-EIO`) instead of stranding it forever.
    pub fn slave_write_ready(&self, need: usize) -> bool {
        self.s2m.space() >= need || self.master_refcount == 0
    }

    /// Readiness predicate for a **master-side** `write(2)` that stalled — see
    /// the `FdBackend::PtyMaster` write arm.
    ///
    /// Which buffer the master fills depends on the line discipline: canonical
    /// input accumulates in `edit_buf` until a `\n` completes the line, raw
    /// input goes straight into the `m2s` ring. The mode is re-read here rather
    /// than captured at stall time so a concurrent `tcsetattr` cannot leave the
    /// sleeper waiting on the buffer the retry will no longer touch.
    ///
    /// The `slave_refcount == 0 && !locked` term mirrors the arm's `-EIO`
    /// hangup check: once the slave side is gone for good the writer must wake
    /// and fail rather than wait for a drain that can never happen. A still
    /// `locked` pair has simply not been opened yet, so it is not a hangup.
    pub fn master_write_ready(&self) -> bool {
        // `EditBuffer::push` succeeds exactly while `len < buf.len()`.
        let has_room = if self.termios.is_canonical() {
            self.edit_buf.len < self.edit_buf.buf.len()
        } else {
            !self.m2s.is_full()
        };
        has_room || (self.slave_refcount == 0 && !self.locked)
    }

    /// Create a new PTY pair with default settings.
    pub fn new(_id: u32) -> Self {
        PtyPairState {
            m2s: PtyRingBuffer::new(),
            s2m: PtyRingBuffer::new(),
            termios: Termios::cooked_default(),
            winsize: Winsize::default_console(),
            edit_buf: EditBuffer::new(),
            slave_fg_pgid: 0,
            master_refcount: 1,
            slave_refcount: 0,
            eof_pending: false,
            locked: true,
            slave_opened: false,
            ldisc_output_suspended: false,
            ldisc_deadline_ticks: None,
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

    // -- write-readiness predicate tests (Phase 112 PTY blocking-write fix) --

    #[test]
    fn slave_write_ready_tracks_required_space() {
        let mut pair = PtyPairState::new(0);
        assert!(pair.slave_write_ready(1));
        assert!(pair.slave_write_ready(2));

        // Fill s2m to exactly one free slot: enough for a plain byte, not for
        // an ONLCR-expanded newline.
        let filler = [b'x'; PTY_BUF_SIZE - 1];
        assert_eq!(pair.s2m.write(&filler), PTY_BUF_SIZE - 1);
        assert_eq!(pair.s2m.space(), 1);
        assert!(pair.slave_write_ready(1));
        assert!(!pair.slave_write_ready(2));

        // Completely full: no write can progress.
        assert_eq!(pair.s2m.write(b"y"), 1);
        assert!(pair.s2m.is_full());
        assert!(!pair.slave_write_ready(1));
        assert!(!pair.slave_write_ready(2));

        // Draining one byte re-arms a single-byte writer only.
        let mut sink = [0u8; 1];
        assert_eq!(pair.s2m.read(&mut sink), 1);
        assert!(pair.slave_write_ready(1));
        assert!(!pair.slave_write_ready(2));
    }

    #[test]
    fn slave_write_ready_on_master_hangup() {
        let mut pair = PtyPairState::new(0);
        let filler = [b'x'; PTY_BUF_SIZE];
        assert_eq!(pair.s2m.write(&filler), PTY_BUF_SIZE);
        assert!(!pair.slave_write_ready(1));

        // A departing master must release the blocked writer (it then sees
        // -EIO) rather than leave it parked on a ring nobody will drain.
        pair.master_refcount = 0;
        assert!(pair.slave_write_ready(1));
        assert!(pair.slave_write_ready(2));
    }

    #[test]
    fn master_write_ready_follows_line_discipline() {
        let mut pair = PtyPairState::new(0);
        pair.slave_refcount = 1;
        pair.locked = false;
        assert!(pair.termios.is_canonical());
        assert!(pair.master_write_ready());

        // Canonical mode fills edit_buf; a full m2s is irrelevant there.
        let filler = [b'x'; PTY_BUF_SIZE];
        assert_eq!(pair.m2s.write(&filler), PTY_BUF_SIZE);
        assert!(pair.m2s.is_full());
        assert!(pair.master_write_ready());

        while pair.edit_buf.push(b'a') {}
        assert!(!pair.master_write_ready());

        // Switching to raw mode re-points the predicate at m2s, which is full.
        pair.termios.c_lflag &= !crate::tty::ICANON;
        assert!(!pair.termios.is_canonical());
        assert!(!pair.master_write_ready());

        // Draining one byte of m2s re-arms the raw-mode writer, while the
        // still-full edit buffer keeps the canonical one parked.
        let mut sink = [0u8; 1];
        assert_eq!(pair.m2s.read(&mut sink), 1);
        assert!(pair.master_write_ready());
        pair.termios.c_lflag |= crate::tty::ICANON;
        assert!(!pair.master_write_ready());
    }

    #[test]
    fn master_write_ready_on_slave_hangup() {
        let mut pair = PtyPairState::new(0);
        pair.slave_refcount = 1;
        pair.locked = false;
        pair.termios.c_lflag &= !crate::tty::ICANON;
        let filler = [b'x'; PTY_BUF_SIZE];
        assert_eq!(pair.m2s.write(&filler), PTY_BUF_SIZE);
        assert!(!pair.master_write_ready());

        // A pair that was never unlocked is not a hangup — the slave simply
        // has not opened yet, so the writer keeps waiting.
        pair.slave_refcount = 0;
        pair.locked = true;
        assert!(!pair.master_write_ready());

        // Slave closed after having been opened → wake and fail with -EIO.
        pair.locked = false;
        assert!(pair.master_write_ready());
    }

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
