//! Kernel pipe implementation (Phase 14, Track B).
//!
//! Each pipe is a 4 KiB ring buffer shared between a read end and a write end.
//! Pipes are identified by a global `pipe_id` index.  The read and write ends
//! are tracked via reference counts so that fork/dup2 can create multiple
//! references to the same pipe end:
//!   - `read()` returns EOF (0) when the writer ref-count reaches 0
//!   - `write()` returns EPIPE when the reader ref-count reaches 0
//!   - The pipe is freed when both counts reach 0

extern crate alloc;

use alloc::vec::Vec;
use spin::Mutex;

/// Size of each pipe's ring buffer.
const PIPE_BUF_SIZE: usize = 4096;

/// A kernel pipe: ring buffer with ref-counted reader/writer ends.
pub struct Pipe {
    buf: [u8; PIPE_BUF_SIZE],
    /// Read position in the ring buffer.
    read_pos: usize,
    /// Number of valid bytes in the buffer (0 = empty, PIPE_BUF_SIZE = full).
    count: usize,
    /// Number of open read-end references (FDs pointing to PipeRead for this pipe).
    pub reader_count: u32,
    /// Number of open write-end references (FDs pointing to PipeWrite for this pipe).
    pub writer_count: u32,
}

impl Pipe {
    fn new() -> Self {
        Pipe {
            buf: [0u8; PIPE_BUF_SIZE],
            read_pos: 0,
            count: 0,
            reader_count: 1,
            writer_count: 1,
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

    /// Check if any writer is still open.
    pub fn has_writer(&self) -> bool {
        self.writer_count > 0
    }

    /// Check if any reader is still open.
    pub fn has_reader(&self) -> bool {
        self.reader_count > 0
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

/// Increment the reader ref-count (called by fork/dup2 when cloning a PipeRead FD).
pub fn pipe_add_reader(pipe_id: usize) {
    let mut table = PIPE_TABLE.lock();
    if let Some(Some(pipe)) = table.get_mut(pipe_id) {
        pipe.reader_count += 1;
    }
}

/// Increment the writer ref-count (called by fork/dup2 when cloning a PipeWrite FD).
pub fn pipe_add_writer(pipe_id: usize) {
    let mut table = PIPE_TABLE.lock();
    if let Some(Some(pipe)) = table.get_mut(pipe_id) {
        pipe.writer_count += 1;
    }
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
        if !pipe.has_writer() {
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

    if !pipe.has_reader() {
        return Err(false); // EPIPE
    }

    if pipe.is_full() {
        return Err(true); // would block
    }

    Ok(pipe.write(src))
}

/// Decrement reader ref-count. Frees the pipe when both counts reach 0.
pub fn pipe_close_reader(pipe_id: usize) {
    let mut table = PIPE_TABLE.lock();
    if let Some(Some(pipe)) = table.get_mut(pipe_id) {
        pipe.reader_count = pipe.reader_count.saturating_sub(1);
        if pipe.reader_count == 0 && pipe.writer_count == 0 {
            table[pipe_id] = None;
        }
    }
}

/// Decrement writer ref-count. Frees the pipe when both counts reach 0.
pub fn pipe_close_writer(pipe_id: usize) {
    let mut table = PIPE_TABLE.lock();
    if let Some(Some(pipe)) = table.get_mut(pipe_id) {
        pipe.writer_count = pipe.writer_count.saturating_sub(1);
        if pipe.reader_count == 0 && pipe.writer_count == 0 {
            table[pipe_id] = None;
        }
    }
}
