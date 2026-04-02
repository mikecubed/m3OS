//! Kernel stdin buffer (Phase 20, updated Phase 22).
//!
//! Circular byte buffer backing `read(0, ...)` for userspace.
//! The line discipline in `stdin_feeder_task` decides when bytes
//! are delivered here (immediately in raw mode, on newline in cooked).

use core::sync::atomic::{AtomicBool, Ordering};
use spin::Mutex;

use crate::task::wait_queue::WaitQueue;

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

    fn push_byte(&mut self, c: u8) {
        if self.count < STDIN_BUF_SIZE {
            let write_pos = (self.read_pos + self.count) % STDIN_BUF_SIZE;
            self.buf[write_pos] = c;
            self.count += 1;
        }
    }

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

/// EOF flag: when set, has_data() returns true but read() returns 0.
static EOF_PENDING: AtomicBool = AtomicBool::new(false);

/// Wait queue for tasks polling stdin for read readiness (Phase 37).
pub static STDIN_WAITQUEUE: WaitQueue = WaitQueue::new();

/// Push a byte into stdin (immediately readable by userspace).
pub fn push_char(c: u8) {
    STDIN.lock().push_byte(c);
    STDIN_WAITQUEUE.wake_all();
}

/// Read from stdin. Returns 0 if no data available (or EOF).
pub fn read(dst: &mut [u8]) -> usize {
    // Check EOF flag first: if set, consume it and return 0.
    if EOF_PENDING
        .compare_exchange(true, false, Ordering::Acquire, Ordering::Relaxed)
        .is_ok()
    {
        return 0;
    }
    STDIN.lock().read(dst)
}

/// Check if stdin has data ready to read (or EOF is pending).
pub fn has_data() -> bool {
    if EOF_PENDING.load(Ordering::Relaxed) {
        return true;
    }
    !STDIN.lock().is_empty()
}

/// Signal EOF: next read() will return 0.
pub fn signal_eof() {
    EOF_PENDING.store(true, Ordering::Release);
    STDIN_WAITQUEUE.wake_all();
}

/// Flush (discard) all pending stdin data.
pub fn flush() {
    let mut s = STDIN.lock();
    s.read_pos = 0;
    s.count = 0;
    EOF_PENDING.store(false, Ordering::Release);
}
