//! Kernel stdin buffer (Phase 20).
//!
//! Provides a raw (character-at-a-time) stdin for userspace processes.
//! Each byte pushed via `push_byte` is immediately available to
//! `read(0, ...)` — there is no kernel-side line buffering.  The
//! userspace shell handles its own echo, backspace, and line editing.

use spin::Mutex;

/// Maximum size of the read-ready buffer.
const STDIN_BUF_SIZE: usize = 4096;

/// The global stdin state — a simple circular byte buffer.
struct StdinState {
    buf: [u8; STDIN_BUF_SIZE],
    read_pos: usize,
    count: usize,
}

impl StdinState {
    const fn new() -> Self {
        StdinState {
            buf: [0u8; STDIN_BUF_SIZE],
            read_pos: 0,
            count: 0,
        }
    }

    /// Push a single byte into the read-ready buffer (immediately readable).
    fn push_byte(&mut self, c: u8) {
        if self.count < STDIN_BUF_SIZE {
            let write_pos = (self.read_pos + self.count) % STDIN_BUF_SIZE;
            self.buf[write_pos] = c;
            self.count += 1;
        }
    }

    /// Read up to `dst.len()` bytes from the buffer.
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

/// Push a byte into stdin (immediately readable by userspace).
pub fn push_char(c: u8) {
    STDIN.lock().push_byte(c);
}

/// Clear line — no-op in raw mode (retained for Ctrl-C/Z handler compat).
pub fn clear_line() {}

/// Read from stdin. Returns 0 if no data available.
pub fn read(dst: &mut [u8]) -> usize {
    STDIN.lock().read(dst)
}

/// Check if stdin has data ready to read.
pub fn has_data() -> bool {
    !STDIN.lock().is_empty()
}
