//! Kernel pipe implementation (Phase 14, Track B).
//!
//! Each pipe is a 4 KiB ring buffer shared between a read end and a write end.
//! Pipes are identified by a global `pipe_id` index.  The read and write ends
//! are tracked via reference counts (reader_open, writer_open) so that:
//!   - `read()` returns EOF (0) when the writer has closed
//!   - `write()` returns EPIPE when the reader has closed
//!   - The pipe is freed when both ends are closed

extern crate alloc;

use alloc::vec::Vec;
use spin::Mutex;

/// Size of each pipe's ring buffer.
const PIPE_BUF_SIZE: usize = 4096;

/// A kernel pipe: ring buffer with reader/writer state.
pub struct Pipe {
    buf: [u8; PIPE_BUF_SIZE],
    /// Read position in the ring buffer.
    read_pos: usize,
    /// Number of valid bytes in the buffer (0 = empty, PIPE_BUF_SIZE = full).
    count: usize,
    /// True while the read end is open.
    pub reader_open: bool,
    /// True while the write end is open.
    pub writer_open: bool,
}

impl Pipe {
    fn new() -> Self {
        Pipe {
            buf: [0u8; PIPE_BUF_SIZE],
            read_pos: 0,
            count: 0,
            reader_open: true,
            writer_open: true,
        }
    }

    /// Read up to `dst.len()` bytes from the pipe. Returns number of bytes read.
    pub fn read(&mut self, dst: &mut [u8]) -> usize {
        let to_read = dst.len().min(self.count);
        for (i, byte) in dst.iter_mut().enumerate().take(to_read) {
            *byte = self.buf[(self.read_pos + i) % PIPE_BUF_SIZE];
        }
        self.read_pos = (self.read_pos + to_read) % PIPE_BUF_SIZE;
        self.count -= to_read;
        to_read
    }

    /// Write up to `src.len()` bytes into the pipe. Returns number of bytes written.
    pub fn write(&mut self, src: &[u8]) -> usize {
        let space = PIPE_BUF_SIZE - self.count;
        let to_write = src.len().min(space);
        let write_pos = (self.read_pos + self.count) % PIPE_BUF_SIZE;
        for (i, &byte) in src.iter().enumerate().take(to_write) {
            self.buf[(write_pos + i) % PIPE_BUF_SIZE] = byte;
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
        self.count == PIPE_BUF_SIZE
    }
}

/// Global pipe table.
static PIPE_TABLE: Mutex<Vec<Option<Pipe>>> = Mutex::new(Vec::new());

/// Allocate a new pipe and return its ID.
pub fn create_pipe() -> usize {
    let mut table = PIPE_TABLE.lock();
    // Reuse a freed slot if available.
    for (i, slot) in table.iter_mut().enumerate() {
        if slot.is_none() {
            *slot = Some(Pipe::new());
            return i;
        }
    }
    let id = table.len();
    table.push(Some(Pipe::new()));
    id
}

/// Read from a pipe. Returns:
///   - `Ok(n)` — n bytes read (0 = EOF if writer closed)
///   - `Err(true)` — buffer empty but writer still open (would block)
pub fn pipe_read(pipe_id: usize, dst: &mut [u8]) -> Result<usize, bool> {
    let mut table = PIPE_TABLE.lock();
    let pipe = match table.get_mut(pipe_id).and_then(|p| p.as_mut()) {
        Some(p) => p,
        None => return Ok(0),
    };

    if pipe.is_empty() {
        if !pipe.writer_open {
            return Ok(0); // EOF
        }
        return Err(true); // would block
    }

    Ok(pipe.read(dst))
}

/// Write to a pipe. Returns:
///   - `Ok(n)` — n bytes written
///   - `Err(false)` — reader closed (EPIPE)
///   - `Err(true)` — buffer full but reader still open (would block)
pub fn pipe_write(pipe_id: usize, src: &[u8]) -> Result<usize, bool> {
    let mut table = PIPE_TABLE.lock();
    let pipe = match table.get_mut(pipe_id).and_then(|p| p.as_mut()) {
        Some(p) => p,
        None => return Err(false),
    };

    if !pipe.reader_open {
        return Err(false); // EPIPE
    }

    if pipe.is_full() {
        return Err(true); // would block
    }

    Ok(pipe.write(src))
}

/// Close one end of a pipe. Frees the pipe when both ends are closed.
pub fn pipe_close_reader(pipe_id: usize) {
    let mut table = PIPE_TABLE.lock();
    if let Some(Some(pipe)) = table.get_mut(pipe_id) {
        pipe.reader_open = false;
        if !pipe.writer_open {
            table[pipe_id] = None;
        }
    }
}

/// Close the write end of a pipe.
pub fn pipe_close_writer(pipe_id: usize) {
    let mut table = PIPE_TABLE.lock();
    if let Some(Some(pipe)) = table.get_mut(pipe_id) {
        pipe.writer_open = false;
        if !pipe.reader_open {
            table[pipe_id] = None;
        }
    }
}
