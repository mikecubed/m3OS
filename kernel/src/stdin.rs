//! Kernel stdin buffer (Phase 14, Track E).
//!
//! Provides a line-buffered stdin for userspace processes.  Characters are
//! accumulated in a line buffer; on Enter the completed line (including '\n')
//! is flushed to the read buffer where `read(0, ...)` can consume it.
//!
//! The stdin feeder task reads scancodes from the keyboard IRQ ring buffer,
//! decodes them to characters, echoes them to the console, handles backspace,
//! and flushes on Enter.

extern crate alloc;

use alloc::vec::Vec;
use spin::Mutex;

/// Maximum size of the read-ready buffer.
const STDIN_BUF_SIZE: usize = 4096;

/// The global stdin state.
struct StdinState {
    /// Line buffer: characters typed but not yet committed (Enter not pressed).
    line: Vec<u8>,
    /// Read-ready buffer: completed lines available for `read(0, ...)`.
    buf: [u8; STDIN_BUF_SIZE],
    /// Read position in buf.
    read_pos: usize,
    /// Number of valid bytes in buf.
    count: usize,
}

impl StdinState {
    const fn new() -> Self {
        StdinState {
            line: Vec::new(),
            buf: [0u8; STDIN_BUF_SIZE],
            read_pos: 0,
            count: 0,
        }
    }

    /// Append a character to the line buffer.
    fn push_char(&mut self, c: u8) {
        if self.line.len() < 1024 {
            self.line.push(c);
        }
    }

    /// Remove the last character from the line buffer (backspace).
    fn backspace(&mut self) -> bool {
        self.line.pop().is_some()
    }

    /// Flush the line buffer + '\n' into the read-ready buffer.
    ///
    /// If the buffer is full, retains unflushed bytes in the line buffer
    /// so they can be retried later (prevents silent data loss).
    fn flush_line(&mut self) {
        let mut flushed = 0;
        for &b in &self.line {
            if self.count >= STDIN_BUF_SIZE {
                break;
            }
            let write_pos = (self.read_pos + self.count) % STDIN_BUF_SIZE;
            self.buf[write_pos] = b;
            self.count += 1;
            flushed += 1;
        }
        // Add newline if there's space.
        if self.count < STDIN_BUF_SIZE {
            let write_pos = (self.read_pos + self.count) % STDIN_BUF_SIZE;
            self.buf[write_pos] = b'\n';
            self.count += 1;
            // Newline was appended; the line is complete, so discard the buffer.
            self.line.clear();
        } else {
            // Couldn't even fit the newline; retain unflushed portion so it can be retried.
            self.line.drain(..flushed);
        }
    }

    /// Read up to `n` bytes from the read-ready buffer.
    fn read(&mut self, dst: &mut [u8]) -> usize {
        let to_read = dst.len().min(self.count);
        for (i, byte) in dst.iter_mut().enumerate().take(to_read) {
            *byte = self.buf[(self.read_pos + i) % STDIN_BUF_SIZE];
        }
        self.read_pos = (self.read_pos + to_read) % STDIN_BUF_SIZE;
        self.count -= to_read;
        to_read
    }

    fn is_empty(&self) -> bool {
        self.count == 0
    }
}

static STDIN: Mutex<StdinState> = Mutex::new(StdinState::new());

/// Push a decoded character into the stdin line buffer.
pub fn push_char(c: u8) {
    STDIN.lock().push_char(c);
}

/// Handle backspace in the line buffer.
pub fn backspace() -> bool {
    STDIN.lock().backspace()
}

/// Flush the current line (on Enter).
pub fn flush_line() {
    STDIN.lock().flush_line();
}

/// Discard the current line buffer (e.g., on Ctrl-C/Ctrl-Z).
pub fn clear_line() {
    STDIN.lock().line.clear();
}

/// Read from stdin. Returns 0 if no data available (non-blocking check).
pub fn read(dst: &mut [u8]) -> usize {
    STDIN.lock().read(dst)
}

/// Check if stdin has data ready to read.
pub fn has_data() -> bool {
    !STDIN.lock().is_empty()
}
