//! Phase 57 / 56-followup — chunked pixel transport for surfaces
//! larger than the IPC bulk cap.
//!
//! The Phase 56 `LABEL_PIXELS` wire ships a whole BGRA8888 surface in
//! one IPC bulk call. The kernel caps a single bulk at
//! `MAX_BULK_LEN = 4096` bytes (see `kernel/src/ipc/mod.rs`), which is
//! enough for `gfx-demo`'s 16×16 reference surface (1 KB pixels) but
//! not for a graphical-terminal-sized 80×25-cell @ 8×16 px = 640×400
//! BGRA surface (1 MB pixels). This module defines the chunked-bulk
//! wire format the `term` graphical client uses to upload that 1 MB
//! buffer in ~256 sub-4-KB IPC frames, plus a host-testable
//! [`ChunkAccumulator`] that the server uses to reassemble it.
//!
//! ## Wire format (`LABEL_PIXELS_CHUNK`)
//!
//! Every chunk's IPC bulk payload is a fixed 24-byte header followed
//! by `chunk_len` bytes of pixel data:
//!
//! ```text
//!   offset 0..4    : buffer_id   (u32 LE)
//!   offset 4..8    : width       (u32 LE) — surface pixel width
//!   offset 8..12   : height      (u32 LE) — surface pixel height
//!   offset 12..16  : total_bytes (u32 LE) — full surface size in bytes
//!   offset 16..20  : offset      (u32 LE) — where chunk starts in the
//!                                            full surface buffer
//!   offset 20..24  : chunk_len   (u32 LE) — number of pixel bytes that
//!                                            follow the header
//!   offset 24..    : chunk_len bytes of BGRA8888 pixel data
//! ```
//!
//! The IPC `data0` slot also carries `buffer_id` so the server can
//! key its [`ChunkAccumulator`] without parsing the bulk in the
//! "queue full" reject path.
//!
//! ## Reassembly contract
//!
//! - Every chunk for a given `buffer_id` MUST agree on `(width,
//!   height, total_bytes)`. The first chunk fixes the geometry; later
//!   chunks that disagree are rejected with [`ChunkError::GeometryMismatch`].
//! - `total_bytes` MUST equal `width * height * 4` (BGRA8888). A
//!   client-supplied mismatch is rejected with
//!   [`ChunkError::GeometryMismatch`].
//! - Chunks may arrive in any order; `offset` is authoritative.
//!   Overlapping chunks overwrite (last-writer-wins) — clients SHOULD
//!   send each byte exactly once, but the server does not enforce
//!   non-overlap to keep the accumulator simple.
//! - When `accumulated_bytes >= total_bytes` the buffer is complete;
//!   [`ChunkAccumulator::add_chunk`] returns
//!   [`AddChunkOutcome::Complete`] with the assembled pixel vector.
//!   The server then moves the completed buffer into its existing
//!   `pending_bulk` slot (matched by `AttachBuffer { buffer_id }`).
//!
//! ## Resource bounds
//!
//! - Per-buffer: bounded by the client-supplied `total_bytes`, capped
//!   at [`MAX_TOTAL_BYTES`] so a hostile client cannot allocate
//!   arbitrarily large reassembly buffers.
//! - Per-server: callers are expected to track open accumulator count
//!   themselves (a `BTreeMap<BufferId, ChunkAccumulator>`); this module
//!   only owns the per-buffer state.

extern crate alloc;

use alloc::vec::Vec;

/// Fixed-size chunked-pixel header preceding the pixel payload in
/// every `LABEL_PIXELS_CHUNK` IPC bulk.
pub const CHUNK_HEADER_LEN: usize = 24;

/// Upper bound on the total reassembled buffer size accepted by
/// [`ChunkAccumulator`]. Today's term surface is 80 × 25 cells × 8 × 16
/// pixels × 4 bytes = 1,024,000 bytes; the cap is rounded up to 4 MiB
/// to leave headroom for resize without inviting unbounded allocation
/// from a hostile client. Anything beyond is a hard reject.
pub const MAX_TOTAL_BYTES: u32 = 4 * 1024 * 1024;

/// Errors observable on the chunked-pixel public surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ChunkError {
    /// Bulk slice is shorter than [`CHUNK_HEADER_LEN`] or the body is
    /// shorter than the header's `chunk_len`.
    HeaderTooShort,
    /// Header's `(width, height, total_bytes)` triple does not match
    /// `width * height * 4`, or disagrees with a previous chunk for
    /// the same `buffer_id`.
    GeometryMismatch,
    /// Header's `total_bytes` exceeds [`MAX_TOTAL_BYTES`].
    TotalTooLarge,
    /// Header's `offset + chunk_len` exceeds the buffer's
    /// `total_bytes` — the chunk would write past the end.
    ChunkOutOfRange,
}

/// Decoded chunk header. Pure data; no side effects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PixelChunkHeader {
    pub buffer_id: u32,
    pub width: u32,
    pub height: u32,
    pub total_bytes: u32,
    pub offset: u32,
    pub chunk_len: u32,
}

impl PixelChunkHeader {
    /// Parse the leading [`CHUNK_HEADER_LEN`] bytes of `bulk` into a
    /// header. Does not validate `(w, h, total_bytes)` against
    /// `chunk_len`'s relationship with the body — call
    /// [`ChunkAccumulator::add_chunk`] for full validation.
    pub fn decode(bulk: &[u8]) -> Result<Self, ChunkError> {
        if bulk.len() < CHUNK_HEADER_LEN {
            return Err(ChunkError::HeaderTooShort);
        }
        let read_u32 = |start: usize| -> u32 {
            let mut b = [0u8; 4];
            b.copy_from_slice(&bulk[start..start + 4]);
            u32::from_le_bytes(b)
        };
        Ok(Self {
            buffer_id: read_u32(0),
            width: read_u32(4),
            height: read_u32(8),
            total_bytes: read_u32(12),
            offset: read_u32(16),
            chunk_len: read_u32(20),
        })
    }

    /// Encode this header into the leading [`CHUNK_HEADER_LEN`] bytes
    /// of `buf`. Returns the byte count written or an error if `buf`
    /// is too small. The complementary inverse of [`Self::decode`].
    pub fn encode(&self, buf: &mut [u8]) -> Result<usize, ChunkError> {
        if buf.len() < CHUNK_HEADER_LEN {
            return Err(ChunkError::HeaderTooShort);
        }
        buf[0..4].copy_from_slice(&self.buffer_id.to_le_bytes());
        buf[4..8].copy_from_slice(&self.width.to_le_bytes());
        buf[8..12].copy_from_slice(&self.height.to_le_bytes());
        buf[12..16].copy_from_slice(&self.total_bytes.to_le_bytes());
        buf[16..20].copy_from_slice(&self.offset.to_le_bytes());
        buf[20..24].copy_from_slice(&self.chunk_len.to_le_bytes());
        Ok(CHUNK_HEADER_LEN)
    }
}

/// Outcome of one [`ChunkAccumulator::add_chunk`] call.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum AddChunkOutcome {
    /// Chunk accepted; buffer is not yet complete. Caller should keep
    /// the accumulator alive and wait for more chunks for the same
    /// `buffer_id`.
    Pending,
    /// Chunk accepted and buffer is now complete. Caller takes
    /// ownership of `pixels` (`width * height * 4` bytes long) and
    /// MUST drop the accumulator.
    Complete {
        width: u32,
        height: u32,
        pixels: Vec<u8>,
    },
}

/// Per-`BufferId` reassembly state. The server keeps one of these per
/// in-flight chunked buffer (typically in a `BTreeMap` keyed by
/// `BufferId`).
#[derive(Debug)]
pub struct ChunkAccumulator {
    width: u32,
    height: u32,
    total_bytes: u32,
    /// Reassembly target. Sized once, on the first chunk, so writes
    /// at any offset land in-place without `Vec::resize` churn.
    pixels: Vec<u8>,
    /// Tracks how many bytes have been written. Saturating with
    /// `total_bytes` so an over-counted accumulator (from overlap)
    /// completes once.
    bytes_written: u32,
}

impl ChunkAccumulator {
    /// Construct a new empty accumulator. The first
    /// [`Self::add_chunk`] call fixes the geometry and sizes the
    /// reassembly buffer.
    pub fn new() -> Self {
        Self {
            width: 0,
            height: 0,
            total_bytes: 0,
            pixels: Vec::new(),
            bytes_written: 0,
        }
    }

    /// Total pixel-buffer size, in bytes, agreed by all chunks
    /// observed so far. Returns `0` until the first chunk lands.
    pub fn total_bytes(&self) -> u32 {
        self.total_bytes
    }

    /// Bytes written so far. Useful for "queue depth"-style logging.
    pub fn bytes_written(&self) -> u32 {
        self.bytes_written
    }

    /// Apply one chunk's header + body. Returns
    /// [`AddChunkOutcome::Pending`] if the buffer is still
    /// incomplete, [`AddChunkOutcome::Complete`] once
    /// `bytes_written >= total_bytes`.
    pub fn add_chunk(
        &mut self,
        header: PixelChunkHeader,
        body: &[u8],
    ) -> Result<AddChunkOutcome, ChunkError> {
        if body.len() < header.chunk_len as usize {
            return Err(ChunkError::HeaderTooShort);
        }
        if header.total_bytes > MAX_TOTAL_BYTES {
            return Err(ChunkError::TotalTooLarge);
        }
        // Geometry must be self-consistent: total = w * h * 4 (BGRA).
        let expected = (header.width as u64)
            .checked_mul(header.height as u64)
            .and_then(|wh| wh.checked_mul(4));
        if expected != Some(header.total_bytes as u64) {
            return Err(ChunkError::GeometryMismatch);
        }
        // First chunk fixes the geometry; later chunks must agree.
        if self.total_bytes == 0 {
            self.width = header.width;
            self.height = header.height;
            self.total_bytes = header.total_bytes;
            self.pixels = alloc::vec![0u8; header.total_bytes as usize];
        } else if self.width != header.width
            || self.height != header.height
            || self.total_bytes != header.total_bytes
        {
            return Err(ChunkError::GeometryMismatch);
        }
        // Bounds check the chunk's window in the buffer.
        let end = match header.offset.checked_add(header.chunk_len) {
            Some(v) => v,
            None => return Err(ChunkError::ChunkOutOfRange),
        };
        if end > self.total_bytes {
            return Err(ChunkError::ChunkOutOfRange);
        }
        let dst = &mut self.pixels[header.offset as usize..end as usize];
        dst.copy_from_slice(&body[..header.chunk_len as usize]);
        self.bytes_written = self
            .bytes_written
            .saturating_add(header.chunk_len)
            .min(self.total_bytes);
        if self.bytes_written >= self.total_bytes {
            let mut taken = Vec::new();
            core::mem::swap(&mut taken, &mut self.pixels);
            return Ok(AddChunkOutcome::Complete {
                width: self.width,
                height: self.height,
                pixels: taken,
            });
        }
        Ok(AddChunkOutcome::Pending)
    }
}

impl Default for ChunkAccumulator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(buffer_id: u32, w: u32, h: u32, offset: u32, chunk_len: u32) -> PixelChunkHeader {
        PixelChunkHeader {
            buffer_id,
            width: w,
            height: h,
            total_bytes: w * h * 4,
            offset,
            chunk_len,
        }
    }

    #[test]
    fn header_round_trip() {
        let h = header(7, 16, 16, 32, 64);
        let mut buf = [0u8; CHUNK_HEADER_LEN];
        h.encode(&mut buf).expect("encode");
        let back = PixelChunkHeader::decode(&buf).expect("decode");
        assert_eq!(back, h);
    }

    #[test]
    fn header_decode_short_bulk_errors() {
        let buf = [0u8; CHUNK_HEADER_LEN - 1];
        assert_eq!(
            PixelChunkHeader::decode(&buf),
            Err(ChunkError::HeaderTooShort)
        );
    }

    #[test]
    fn single_chunk_completes_immediately() {
        let mut acc = ChunkAccumulator::new();
        let h = header(1, 4, 4, 0, 4 * 4 * 4);
        let body = alloc::vec![0xAB; (4 * 4 * 4) as usize];
        match acc.add_chunk(h, &body).expect("ok") {
            AddChunkOutcome::Complete {
                width,
                height,
                pixels,
            } => {
                assert_eq!(width, 4);
                assert_eq!(height, 4);
                assert_eq!(pixels.len(), 64);
                assert!(pixels.iter().all(|&b| b == 0xAB));
            }
            AddChunkOutcome::Pending => panic!("single chunk should complete"),
        }
    }

    #[test]
    fn chunks_reassemble_in_offset_order() {
        let mut acc = ChunkAccumulator::new();
        // 2x2 pixels = 16 bytes, sent as 4 chunks of 4 bytes each.
        let total = 16;
        let want: alloc::vec::Vec<u8> = (0u8..16).collect();
        for offset in (0..total).step_by(4) {
            let h = PixelChunkHeader {
                buffer_id: 1,
                width: 2,
                height: 2,
                total_bytes: total,
                offset,
                chunk_len: 4,
            };
            let body = &want[offset as usize..offset as usize + 4];
            let outcome = acc.add_chunk(h, body).expect("ok");
            if offset + 4 == total {
                match outcome {
                    AddChunkOutcome::Complete { pixels, .. } => assert_eq!(pixels, want),
                    AddChunkOutcome::Pending => panic!("last chunk must complete"),
                }
            } else {
                assert!(matches!(outcome, AddChunkOutcome::Pending));
            }
        }
    }

    #[test]
    fn chunks_can_arrive_out_of_order() {
        let mut acc = ChunkAccumulator::new();
        let total = 16u32;
        let want: alloc::vec::Vec<u8> = (0u8..16).collect();
        // Send in reverse order; the final chunk (offset 0) completes
        // the buffer because each chunk increments `bytes_written` and
        // the count reaches `total_bytes` regardless of arrival order.
        let offsets = [12u32, 8, 4, 0];
        for (idx, &offset) in offsets.iter().enumerate() {
            let h = PixelChunkHeader {
                buffer_id: 1,
                width: 2,
                height: 2,
                total_bytes: total,
                offset,
                chunk_len: 4,
            };
            let body = &want[offset as usize..offset as usize + 4];
            let outcome = acc.add_chunk(h, body).expect("ok");
            if idx + 1 == offsets.len() {
                match outcome {
                    AddChunkOutcome::Complete { pixels, .. } => assert_eq!(pixels, want),
                    AddChunkOutcome::Pending => panic!("last chunk must complete"),
                }
            } else {
                assert!(matches!(outcome, AddChunkOutcome::Pending));
            }
        }
    }

    #[test]
    fn geometry_mismatch_total_vs_wh_rejects() {
        let mut acc = ChunkAccumulator::new();
        let h = PixelChunkHeader {
            buffer_id: 1,
            width: 4,
            height: 4,
            total_bytes: 999, // != 4*4*4
            offset: 0,
            chunk_len: 4,
        };
        let body = [0u8; 4];
        assert_eq!(acc.add_chunk(h, &body), Err(ChunkError::GeometryMismatch));
    }

    #[test]
    fn second_chunk_with_different_geometry_rejects() {
        let mut acc = ChunkAccumulator::new();
        let h1 = header(1, 4, 4, 0, 8);
        acc.add_chunk(h1, &[0u8; 8]).expect("ok");
        let h2 = header(1, 8, 8, 8, 8); // disagrees on geometry
        assert_eq!(
            acc.add_chunk(h2, &[0u8; 8]),
            Err(ChunkError::GeometryMismatch)
        );
    }

    #[test]
    fn chunk_out_of_range_rejects() {
        let mut acc = ChunkAccumulator::new();
        let h = PixelChunkHeader {
            buffer_id: 1,
            width: 4,
            height: 4,
            total_bytes: 64,
            offset: 60,
            chunk_len: 8, // 60 + 8 = 68 > 64
        };
        let body = [0u8; 8];
        assert_eq!(acc.add_chunk(h, &body), Err(ChunkError::ChunkOutOfRange));
    }

    #[test]
    fn over_max_total_bytes_rejects() {
        let mut acc = ChunkAccumulator::new();
        let huge = MAX_TOTAL_BYTES + 4;
        // Pick w/h so that w*h*4 == huge. huge/4 = MAX_TOTAL_BYTES/4 + 1.
        let pixels = huge / 4;
        let h = PixelChunkHeader {
            buffer_id: 1,
            width: pixels,
            height: 1,
            total_bytes: huge,
            offset: 0,
            chunk_len: 4,
        };
        let body = [0u8; 4];
        assert_eq!(acc.add_chunk(h, &body), Err(ChunkError::TotalTooLarge));
    }

    #[test]
    fn body_shorter_than_chunk_len_rejects() {
        let mut acc = ChunkAccumulator::new();
        let h = header(1, 4, 4, 0, 16);
        let body = [0u8; 8]; // body shorter than chunk_len
        assert_eq!(acc.add_chunk(h, &body), Err(ChunkError::HeaderTooShort));
    }
}
