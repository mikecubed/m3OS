//! Phase 56 Track B.4 — userspace surface-buffer helper.
//!
//! Lives in its own crate so binaries that don't need pixel buffers (e.g.
//! `echo-args`, `init`) don't have to provide a global allocator just to
//! satisfy unrelated feature unification on `syscall-lib`. Crates that do
//! want client-owned pixel buffers (`display_server`, `gfx-demo`) take a
//! direct `path` dependency on this crate.
//!
//! Provides a typed handle to a client-owned, refcounted pixel buffer
//! suitable for the Phase 56 client → `display_server` surface protocol. The
//! refcount lifecycle (attach → commit → release → destroy) is enforced by
//! [`kernel_core::display::buffer::BufferLifecycle`] and is consumed by both
//! the client (via this helper) and the server (in `display_server::client`).
//!
//! # Transport posture
//!
//! True zero-copy via page-grant capabilities is the long-term target (see
//! `docs/appendix/gui/wayland-gap-analysis.md` § 1 on `wl_shm` semantics).
//! Phase 56 ships the **structural** seam — the [`SurfaceBuffer`] type and
//! the lifecycle state machine — and pairs it with the existing kernel
//! bulk-IPC primitive `ipc_call_buf` for transport. Because `MAX_BULK_LEN`
//! in the kernel is 4 KiB today, Phase 56 demo surfaces are capped at
//! 32 × 32 BGRA8888 pixels (= 4 KiB exact). Larger surfaces are a
//! straightforward follow-up: bump `MAX_BULK_LEN` and/or land the
//! page-grant capability transfer that the Phase 56 wrap-up notes call out.
//!
//! # Pixel format
//!
//! All Phase 56 buffers are BGRA8888 little-endian (one `u32` per pixel,
//! native to the framebuffer format the kernel boots with on QEMU/UEFI).
//! Other formats are deferred — recorded in the learning doc — and would
//! land alongside multi-format compositor support.
#![no_std]

extern crate alloc;

use alloc::vec;
use alloc::vec::Vec;

/// Pixel format for Phase 56 surface buffers. Single-variant enum gives us
/// a forward-compatible seam without committing to format-aware composition
/// in this phase.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PixelFormat {
    /// 32 bits per pixel, byte order B,G,R,A (native UEFI framebuffer).
    Bgra8888,
}

impl PixelFormat {
    pub const fn bytes_per_pixel(self) -> u32 {
        match self {
            PixelFormat::Bgra8888 => 4,
        }
    }
}

/// Maximum bytes carried by a single bulk-IPC message (matches the kernel's
/// `MAX_BULK_LEN`). Userspace callers can use this constant to size their
/// surfaces; allocations exceeding this fail with [`SurfaceBufferError::TooLarge`].
pub const MAX_BUFFER_BYTES: usize = 4096;

/// Errors returned by [`SurfaceBuffer`] constructors and accessors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SurfaceBufferError {
    /// `width * height * bytes_per_pixel` overflowed `usize`.
    GeometryOverflow,
    /// `width` or `height` was zero — no pixels allocated.
    ZeroDimension,
    /// Total byte size exceeds [`MAX_BUFFER_BYTES`].
    TooLarge { requested: usize, limit: usize },
}

/// One client-owned pixel buffer. Stable across attach → commit → release;
/// content is mutable only while the buffer is **not** in flight (see the
/// Phase 56 learning doc § Buffer lifecycle for the full rules).
///
/// The buffer carries a stable [`SurfaceBufferId`] so the server can match
/// `BufferReleased` events back to the correct client buffer.
#[derive(Debug)]
pub struct SurfaceBuffer {
    id: SurfaceBufferId,
    width: u32,
    height: u32,
    format: PixelFormat,
    pixels: Vec<u8>,
}

/// Stable identifier for a [`SurfaceBuffer`]. Mirrors the wire-level
/// `BufferId` in `kernel_core::display::protocol`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SurfaceBufferId(pub u32);

impl SurfaceBuffer {
    /// Allocate a new buffer of `width × height` BGRA8888 pixels.
    ///
    /// The buffer is zero-filled. Call [`Self::fill`] or write directly via
    /// [`Self::pixels_mut`] to populate before the first attach/commit.
    pub fn new(
        id: SurfaceBufferId,
        width: u32,
        height: u32,
        format: PixelFormat,
    ) -> Result<Self, SurfaceBufferError> {
        if width == 0 || height == 0 {
            return Err(SurfaceBufferError::ZeroDimension);
        }
        let bpp = format.bytes_per_pixel() as usize;
        let total = (width as usize)
            .checked_mul(height as usize)
            .and_then(|wh| wh.checked_mul(bpp))
            .ok_or(SurfaceBufferError::GeometryOverflow)?;
        if total > MAX_BUFFER_BYTES {
            return Err(SurfaceBufferError::TooLarge {
                requested: total,
                limit: MAX_BUFFER_BYTES,
            });
        }
        Ok(Self {
            id,
            width,
            height,
            format,
            pixels: vec![0u8; total],
        })
    }

    pub fn id(&self) -> SurfaceBufferId {
        self.id
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn format(&self) -> PixelFormat {
        self.format
    }

    /// Length in bytes (`width * height * bytes_per_pixel`).
    pub fn byte_len(&self) -> usize {
        self.pixels.len()
    }

    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// Mutable view of the pixel bytes. Only safe to call when the buffer
    /// is not in flight (see the lifecycle rules in the learning doc).
    pub fn pixels_mut(&mut self) -> &mut [u8] {
        &mut self.pixels
    }

    /// Fill the buffer with a single BGRA8888 pixel value (little-endian).
    pub fn fill(&mut self, bgra: u32) {
        let bytes = bgra.to_le_bytes();
        match self.format {
            PixelFormat::Bgra8888 => {
                for chunk in self.pixels.chunks_exact_mut(4) {
                    chunk.copy_from_slice(&bytes);
                }
            }
        }
    }

    /// Write one BGRA8888 pixel at `(x, y)`. Out-of-range coordinates are
    /// silently ignored — the demo client never trusts caller arithmetic on
    /// the hot path.
    pub fn put_pixel(&mut self, x: u32, y: u32, bgra: u32) {
        if x >= self.width || y >= self.height {
            return;
        }
        let stride = (self.width as usize) * (self.format.bytes_per_pixel() as usize);
        let off = (y as usize) * stride + (x as usize) * (self.format.bytes_per_pixel() as usize);
        let bytes = bgra.to_le_bytes();
        if off + 4 <= self.pixels.len() {
            self.pixels[off..off + 4].copy_from_slice(&bytes);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocation_fits_geometry() {
        let buf = SurfaceBuffer::new(SurfaceBufferId(7), 16, 16, PixelFormat::Bgra8888).unwrap();
        assert_eq!(buf.id(), SurfaceBufferId(7));
        assert_eq!(buf.byte_len(), 16 * 16 * 4);
        assert_eq!(buf.pixels().iter().copied().sum::<u8>(), 0);
    }

    #[test]
    fn zero_dimension_is_typed_error() {
        let err = SurfaceBuffer::new(SurfaceBufferId(0), 0, 16, PixelFormat::Bgra8888).unwrap_err();
        assert_eq!(err, SurfaceBufferError::ZeroDimension);
    }

    #[test]
    fn too_large_is_typed_error() {
        // 64 × 64 = 4096 pixels = 16384 bytes > MAX_BUFFER_BYTES.
        let err =
            SurfaceBuffer::new(SurfaceBufferId(0), 64, 64, PixelFormat::Bgra8888).unwrap_err();
        match err {
            SurfaceBufferError::TooLarge { requested, limit } => {
                assert_eq!(requested, 64 * 64 * 4);
                assert_eq!(limit, MAX_BUFFER_BYTES);
            }
            _ => panic!("expected TooLarge, got {err:?}"),
        }
    }

    #[test]
    fn fill_sets_every_pixel() {
        let mut buf = SurfaceBuffer::new(SurfaceBufferId(0), 4, 4, PixelFormat::Bgra8888).unwrap();
        buf.fill(0xAABBCCDD);
        for chunk in buf.pixels().chunks_exact(4) {
            assert_eq!(chunk, &[0xDD, 0xCC, 0xBB, 0xAA]);
        }
    }

    #[test]
    fn put_pixel_in_range_writes() {
        let mut buf = SurfaceBuffer::new(SurfaceBufferId(0), 4, 4, PixelFormat::Bgra8888).unwrap();
        buf.put_pixel(2, 1, 0x11223344);
        // (2, 1) at stride=16 → offset 1*16 + 2*4 = 24
        assert_eq!(buf.pixels()[24..28], [0x44, 0x33, 0x22, 0x11]);
    }

    #[test]
    fn put_pixel_out_of_range_no_panic() {
        let mut buf = SurfaceBuffer::new(SurfaceBufferId(0), 4, 4, PixelFormat::Bgra8888).unwrap();
        buf.put_pixel(99, 99, 0x11223344);
        // No write should have happened; entire buffer remains zero.
        assert!(buf.pixels().iter().all(|&b| b == 0));
    }

    #[test]
    fn max_buffer_size_is_exactly_4kb() {
        // 32 × 32 BGRA = 4096 bytes (the IPC bulk limit). Largest Phase 56 demo.
        let buf = SurfaceBuffer::new(SurfaceBufferId(0), 32, 32, PixelFormat::Bgra8888).unwrap();
        assert_eq!(buf.byte_len(), 4096);
        assert_eq!(buf.byte_len(), MAX_BUFFER_BYTES);
    }
}
