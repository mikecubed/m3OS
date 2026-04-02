extern crate alloc;

use alloc::vec::Vec;

/// Fixed-size byte ring that retains the most recent bytes written.
pub struct LogRing<const N: usize> {
    buf: [u8; N],
    start: usize,
    len: usize,
}

impl<const N: usize> LogRing<N> {
    pub const fn new() -> Self {
        Self {
            buf: [0; N],
            start: 0,
            len: 0,
        }
    }

    pub fn push_bytes(&mut self, bytes: &[u8]) {
        if N == 0 {
            return;
        }
        for &byte in bytes {
            let write_pos = (self.start + self.len) % N;
            self.buf[write_pos] = byte;
            if self.len == N {
                self.start = (self.start + 1) % N;
            } else {
                self.len += 1;
            }
        }
    }

    pub fn snapshot(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.len);
        self.snapshot_into(&mut out);
        out
    }

    pub fn snapshot_into(&self, out: &mut Vec<u8>) {
        out.clear();
        for i in 0..self.len {
            out.push(self.buf[(self.start + i) % N]);
        }
    }
}

impl<const N: usize> Default for LogRing<N> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::LogRing;

    #[test]
    fn preserves_bytes_without_wrap() {
        let mut ring = LogRing::<16>::new();
        ring.push_bytes(b"hello");
        assert_eq!(ring.snapshot(), b"hello");
    }

    #[test]
    fn keeps_latest_bytes_on_overflow() {
        let mut ring = LogRing::<8>::new();
        ring.push_bytes(b"abcdef");
        ring.push_bytes(b"ghijkl");
        assert_eq!(ring.snapshot(), b"efghijkl");
    }

    #[test]
    fn handles_multiple_wraps() {
        let mut ring = LogRing::<4>::new();
        ring.push_bytes(b"ab");
        ring.push_bytes(b"cdef");
        ring.push_bytes(b"gh");
        assert_eq!(ring.snapshot(), b"efgh");
    }
}
