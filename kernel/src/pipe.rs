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

pub use kernel_core::pipe::Pipe;

/// Global pipe table.
static PIPE_TABLE: Mutex<Vec<Option<Pipe>>> = Mutex::new(Vec::new());

/// Allocate a new pipe and return its ID.
///
/// The pipe is created with `reader_count=0, writer_count=0`.
/// Callers must explicitly call `pipe_add_reader`/`pipe_add_writer`
/// for each FD they create that references the pipe.
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

/// Free a pipe slot directly, without adjusting refcounts.
/// Used when pipe creation fails before any FDs reference it.
pub fn free_pipe(pipe_id: usize) {
    let mut table = PIPE_TABLE.lock();
    if pipe_id < table.len() {
        table[pipe_id] = None;
    }
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
