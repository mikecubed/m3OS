extern crate alloc;

use alloc::vec::Vec;

/// Fixed-size byte ring that retains the most recent bytes written.
///
/// In addition to the most-recent-bytes window, the ring tracks a monotonic
/// `total` byte count so callers can stream from it like a log device:
/// every byte ever pushed has a stable absolute sequence number in
/// `0..total`, and [`LogRing::read_from`] copies bytes newer than a caller's
/// cursor. Bytes that have scrolled out of the `N`-byte window are no longer
/// readable; a cursor that falls behind is fast-forwarded to the oldest
/// resident byte (see [`LogRing::read_from`]).
pub struct LogRing<const N: usize> {
    buf: [u8; N],
    start: usize,
    len: usize,
    /// Total number of bytes ever pushed. Monotonic; the absolute sequence
    /// number of the oldest resident byte is `total - len`. At realistic log
    /// rates a `u64` never wraps (it would take centuries at GB/s).
    total: u64,
}

impl<const N: usize> LogRing<N> {
    pub const fn new() -> Self {
        Self {
            buf: [0; N],
            start: 0,
            len: 0,
            total: 0,
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
            self.total += 1;
        }
    }

    /// Total number of bytes ever pushed (the absolute sequence number one
    /// past the newest resident byte).
    pub fn total_written(&self) -> u64 {
        self.total
    }

    /// Absolute sequence number of the oldest byte still resident in the ring.
    /// Bytes with a smaller sequence number have scrolled out and are gone.
    pub fn oldest_seq(&self) -> u64 {
        self.total - self.len as u64
    }

    /// Copy bytes whose absolute sequence number is `>= cursor` into `out`,
    /// up to `out.len()`, and return `(bytes_copied, new_cursor)`.
    ///
    /// `cursor` is clamped into the resident window `[oldest_seq, total]`:
    /// a cursor that has fallen behind `oldest_seq` (because the ring
    /// overwrote unread bytes) is silently fast-forwarded to `oldest_seq`,
    /// so the returned `new_cursor` may jump past dropped bytes. When the
    /// caller is already caught up (`cursor >= total`) this returns
    /// `(0, total)`.
    pub fn read_from(&self, cursor: u64, out: &mut [u8]) -> (usize, u64) {
        let oldest = self.oldest_seq();
        let start_seq = cursor.clamp(oldest, self.total);
        let avail = (self.total - start_seq) as usize;
        let n = avail.min(out.len());
        for (i, slot) in out.iter_mut().enumerate().take(n) {
            // Offset of this byte within the resident window, then map to the
            // physical ring index. `N > 0` is guaranteed because `n > 0`
            // implies `len > 0`.
            let off = (start_seq - oldest) as usize + i;
            *slot = self.buf[(self.start + off) % N];
        }
        (n, start_seq + n as u64)
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

    #[test]
    fn read_from_streams_without_wrap() {
        let mut ring = LogRing::<16>::new();
        ring.push_bytes(b"hello");
        assert_eq!(ring.total_written(), 5);
        assert_eq!(ring.oldest_seq(), 0);

        let mut out = [0u8; 8];
        // From the start: copies everything, cursor advances to 5.
        let (n, cur) = ring.read_from(0, &mut out);
        assert_eq!(&out[..n], b"hello");
        assert_eq!(cur, 5);
        // Caught up: nothing more, cursor unchanged.
        let (n, cur) = ring.read_from(cur, &mut out);
        assert_eq!(n, 0);
        assert_eq!(cur, 5);
        // New data appended is streamed from the prior cursor.
        ring.push_bytes(b"!");
        let (n, cur) = ring.read_from(cur, &mut out);
        assert_eq!(&out[..n], b"!");
        assert_eq!(cur, 6);
    }

    #[test]
    fn read_from_fast_forwards_past_dropped_bytes() {
        let mut ring = LogRing::<8>::new();
        ring.push_bytes(b"abcdef"); // total=6, len=6, oldest=0
        ring.push_bytes(b"ghijkl"); // total=12, len=8, oldest=4 → resident "efghijkl"
        assert_eq!(ring.total_written(), 12);
        assert_eq!(ring.oldest_seq(), 4);

        let mut out = [0u8; 16];
        // A stale cursor (0) is fast-forwarded to oldest (4); the 4 dropped
        // bytes are skipped and the returned cursor jumps to 12.
        let (n, cur) = ring.read_from(0, &mut out);
        assert_eq!(&out[..n], b"efghijkl");
        assert_eq!(cur, 12);
        // A cursor ahead of total clamps to total and yields nothing.
        let (n, cur) = ring.read_from(99, &mut out);
        assert_eq!(n, 0);
        assert_eq!(cur, 12);
    }

    #[test]
    fn read_from_honors_small_output_buffer() {
        let mut ring = LogRing::<16>::new();
        ring.push_bytes(b"abcdefgh"); // total=8

        let mut out = [0u8; 3];
        let (n, cur) = ring.read_from(2, &mut out);
        assert_eq!(&out[..n], b"cde");
        assert_eq!(cur, 5);
        let (n, cur) = ring.read_from(cur, &mut out);
        assert_eq!(&out[..n], b"fgh");
        assert_eq!(cur, 8);
        let (n, _) = ring.read_from(cur, &mut out);
        assert_eq!(n, 0);
    }
}
