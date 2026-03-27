/// Size of each pipe's ring buffer.
pub const PIPE_BUF_SIZE: usize = 4096;

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
    pub fn new() -> Self {
        Pipe {
            buf: [0u8; PIPE_BUF_SIZE],
            read_pos: 0,
            count: 0,
            reader_count: 0,
            writer_count: 0,
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

impl Default for Pipe {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_write_basic() {
        let mut pipe = Pipe::new();
        let written = pipe.write(b"hello");
        assert_eq!(written, 5);
        assert!(!pipe.is_empty());

        let mut buf = [0u8; 16];
        let read = pipe.read(&mut buf);
        assert_eq!(read, 5);
        assert_eq!(&buf[..5], b"hello");
        assert!(pipe.is_empty());
    }

    #[test]
    fn wraparound() {
        let mut pipe = Pipe::new();
        // Fill most of the buffer
        let data = [0xAA; PIPE_BUF_SIZE - 10];
        pipe.write(&data);
        // Read it all back
        let mut sink = [0u8; PIPE_BUF_SIZE];
        pipe.read(&mut sink);
        assert!(pipe.is_empty());

        // Now write across the wrap boundary
        let wrap_data = [0xBB; 20];
        let written = pipe.write(&wrap_data);
        assert_eq!(written, 20);

        let mut out = [0u8; 20];
        let read = pipe.read(&mut out);
        assert_eq!(read, 20);
        assert_eq!(out, [0xBB; 20]);
    }

    #[test]
    fn empty_and_full() {
        let mut pipe = Pipe::new();
        assert!(pipe.is_empty());
        assert!(!pipe.is_full());

        let data = [0u8; PIPE_BUF_SIZE];
        let written = pipe.write(&data);
        assert_eq!(written, PIPE_BUF_SIZE);
        assert!(pipe.is_full());
        assert!(!pipe.is_empty());

        // Writing to a full pipe returns 0
        let extra = pipe.write(b"x");
        assert_eq!(extra, 0);
    }

    #[test]
    fn partial_read() {
        let mut pipe = Pipe::new();
        pipe.write(b"abcdefgh");

        let mut small = [0u8; 3];
        let read = pipe.read(&mut small);
        assert_eq!(read, 3);
        assert_eq!(&small, b"abc");

        let mut rest = [0u8; 16];
        let read = pipe.read(&mut rest);
        assert_eq!(read, 5);
        assert_eq!(&rest[..5], b"defgh");
    }

    #[test]
    fn partial_write() {
        let mut pipe = Pipe::new();
        // Fill all but 5 bytes
        let data = [0u8; PIPE_BUF_SIZE - 5];
        pipe.write(&data);

        let written = pipe.write(b"abcdefghij");
        assert_eq!(written, 5); // only 5 bytes of space
    }

    #[test]
    fn zero_length_ops() {
        let mut pipe = Pipe::new();
        let written = pipe.write(b"");
        assert_eq!(written, 0);
        assert!(pipe.is_empty());

        pipe.write(b"data");
        let mut empty = [0u8; 0];
        let read = pipe.read(&mut empty);
        assert_eq!(read, 0);
    }

    #[test]
    fn refcounts_start_at_zero() {
        let pipe = Pipe::new();
        assert_eq!(pipe.reader_count, 0);
        assert_eq!(pipe.writer_count, 0);
        assert!(!pipe.has_reader());
        assert!(!pipe.has_writer());
    }
}
