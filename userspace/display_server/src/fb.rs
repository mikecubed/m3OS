//! Phase 56 Track C.2 — kernel-backed `FramebufferOwner` impl.
//!
//! This module is the userspace-side wiring between the
//! `kernel-core::display::fb_owner::FramebufferOwner` trait and the kernel
//! framebuffer syscalls (`SYS_FRAMEBUFFER_INFO`, `SYS_FRAMEBUFFER_MMAP`,
//! `SYS_FRAMEBUFFER_RELEASE`). The pure-logic clipping rules and contract
//! tests live in `kernel-core`; this file just connects the trait to the
//! real MMIO mapping.
//!
//! ## Phase 73 — double buffering
//!
//! `write_pixels` lands in a userspace-side back buffer; `present()`
//! is the only path that copies pixels into the kernel-mapped front
//! buffer. Every compose pass that previously wrote partial state
//! directly to the visible framebuffer (clear-to-background, then
//! per-surface blits, then cursor blit) now lands in the back buffer
//! and reaches the screen as one atomic memcpy at frame end. Eliminates
//! the "background flashes between clear and surface blit" tearing
//! the single-buffered path exhibited on every animation tick,
//! arrangement change, focus transition, and destroy.
//!
//! The owner lives for the duration of `display_server`'s ownership of
//! the framebuffer. On drop it best-effort releases the FB; explicit
//! shutdown paths should call `release()` first so the result is checked.

extern crate alloc;

use alloc::vec::Vec;
use core::ptr;

use kernel_core::display::fb_owner::{FbError, FbMetadata, FramebufferOwner, bytes_per_pixel};
use kernel_core::display::protocol::Rect;

use crate::pixel_format_from_kernel_tag;

/// Reasons `KernelFramebufferOwner::acquire` may fail. Caller decides
/// whether each is recoverable (FbBusy → backoff, others → exit).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AcquireError {
    /// Another process currently owns the framebuffer (kernel reported
    /// EBUSY on `framebuffer_mmap`).
    FbBusy,
    /// `framebuffer_info` returned a kernel error or a buffer too small.
    FbInfoFailed,
    /// `framebuffer_mmap` returned a non-EBUSY error.
    FbMmapFailed,
    /// Kernel reported a pixel format outside the Phase 56 supported set.
    UnsupportedPixelFormat,
}

/// Kernel-backed [`FramebufferOwner`]. Holds the userspace virtual address
/// of the mapped framebuffer plus its geometry so writes can be issued
/// without re-querying the kernel.
///
/// Phase 73 — `back_buffer` is a userspace-allocated mirror sized
/// identically to the kernel-mapped front. `write_pixels` lands here
/// (cheap CPU-cache-friendly copy); `present()` is the only path that
/// pushes the back into the visible front. Until `present()` runs the
/// screen shows whatever it last presented, so intermediate states
/// (clear, then blit, then cursor) are never observable to the user.
pub struct KernelFramebufferOwner {
    base: *mut u8,
    back_buffer: Vec<u8>,
    metadata: FbMetadata,
    /// Total mapped byte length — used as a defensive bound on clipped
    /// writes in addition to width/height.
    byte_len: usize,
    released: bool,
    /// `true` iff `back_buffer` differs from the kernel-mapped front.
    /// Cleared by `present()` so consecutive `present()` calls with no
    /// intervening writes are no-ops. Set by every `write_pixels` call
    /// that actually touched bytes.
    dirty: bool,
    /// Row-range bounding box of dirty bytes in `back_buffer`. Tracked
    /// per `write_pixels` so `present()` can memcpy just the touched
    /// rows rather than the whole framebuffer. `(usize::MAX, 0)` is
    /// the "no dirt" sentinel; `usize::MAX` keeps the inclusive-min
    /// branch sound when no writes have happened. Phase 73 — at 1080p
    /// the full-FB memcpy was 8 MiB per present, which TCG choked on;
    /// row-bounded copies bring typical frames back into the
    /// few-hundred-KB range.
    dirty_row_min: usize,
    dirty_row_max_exclusive: usize,
}

// SAFETY: the FB virtual address is only mutated through methods on this
// type, which take `&mut self`. The kernel guarantees the mapping is
// per-process.
unsafe impl Send for KernelFramebufferOwner {}

impl KernelFramebufferOwner {
    /// Acquire the framebuffer. Combines `framebuffer_info` and
    /// `framebuffer_mmap` (which atomically claims ownership) into one
    /// step. Errors are typed; callers handle `FbBusy` with a backoff
    /// loop and treat the rest as fatal.
    pub fn acquire() -> Result<Self, AcquireError> {
        let mut info_buf = [0u8; 20];
        let info_ret = syscall_lib::framebuffer_info(&mut info_buf);
        if info_ret < 0 {
            return Err(AcquireError::FbInfoFailed);
        }

        let width = u32::from_le_bytes([info_buf[0], info_buf[1], info_buf[2], info_buf[3]]);
        let height = u32::from_le_bytes([info_buf[4], info_buf[5], info_buf[6], info_buf[7]]);
        let stride_pixels =
            u32::from_le_bytes([info_buf[8], info_buf[9], info_buf[10], info_buf[11]]);
        let bpp = u32::from_le_bytes([info_buf[12], info_buf[13], info_buf[14], info_buf[15]]);
        let pf_tag = u32::from_le_bytes([info_buf[16], info_buf[17], info_buf[18], info_buf[19]]);

        let pixel_format = match pixel_format_from_kernel_tag(pf_tag) {
            Some(f) => f,
            None => return Err(AcquireError::UnsupportedPixelFormat),
        };
        if bpp != bytes_per_pixel(pixel_format) {
            return Err(AcquireError::UnsupportedPixelFormat);
        }

        let metadata = FbMetadata {
            width,
            height,
            stride_bytes: stride_pixels.saturating_mul(bpp),
            pixel_format,
        };

        let mmap_ret = syscall_lib::framebuffer_mmap();
        // Kernel error convention: any value above (u64::MAX - 4096) is a
        // negative errno encoded as u64. EBUSY is the recoverable case.
        if mmap_ret > u64::MAX - 4096 {
            let errno = -(mmap_ret as i64);
            return Err(if errno == 16 {
                AcquireError::FbBusy
            } else {
                AcquireError::FbMmapFailed
            });
        }

        let base = mmap_ret as *mut u8;
        // Total bytes the kernel actually mapped: stride_bytes * height
        // (the kernel maps full-row units).
        let byte_len = (metadata.stride_bytes as usize).saturating_mul(metadata.height as usize);

        // Allocate the back buffer at the same size. Initial contents
        // are zero; the first compose pass overwrites every byte via
        // `fill_background` + surface blits before its terminating
        // `present()`, so the initial zero state is never observable.
        let back_buffer = alloc::vec![0u8; byte_len];

        Ok(Self {
            base,
            back_buffer,
            metadata,
            byte_len,
            released: false,
            dirty: false,
            dirty_row_min: usize::MAX,
            dirty_row_max_exclusive: 0,
        })
    }

    /// Explicit release path. Returns the kernel's syscall result so the
    /// caller can react (negative errno on failure).
    #[allow(dead_code)]
    pub fn release(mut self) -> isize {
        if self.released {
            return 0;
        }
        self.released = true;
        syscall_lib::framebuffer_release()
    }
}

impl Drop for KernelFramebufferOwner {
    fn drop(&mut self) {
        if !self.released {
            // Best-effort release; we cannot surface the result here.
            let _ = syscall_lib::framebuffer_release();
        }
    }
}

impl FramebufferOwner for KernelFramebufferOwner {
    fn metadata(&self) -> FbMetadata {
        self.metadata
    }

    fn write_pixels(&mut self, rect: Rect, src: &[u8], src_stride: u32) -> Result<(), FbError> {
        // Pure-logic clipping mirrors `RecordingFramebufferOwner` so the
        // contract suite passes against this impl too. We compute the
        // clipped rect in i64 to avoid overflow on pathological inputs.
        let bpp = bytes_per_pixel(self.metadata.pixel_format) as usize;
        let clipped = match clip_rect(rect, self.metadata.width, self.metadata.height) {
            Some(c) => c,
            None => return Ok(()), // zero-area or fully off-screen
        };

        let clipped_w_bytes = (clipped.w as usize).saturating_mul(bpp);
        let stride = src_stride as usize;
        if stride < clipped_w_bytes {
            return Err(FbError::InvalidStride);
        }

        let src_offset_x_bytes =
            ((clipped.x as i64 - rect.x as i64).max(0) as usize).saturating_mul(bpp);
        let src_offset_y_rows = (clipped.y as i64 - rect.y as i64).max(0) as usize;
        let required_src = src_offset_y_rows
            .saturating_mul(stride)
            .saturating_add(src_offset_x_bytes)
            .saturating_add(stride.saturating_mul(clipped.h as usize - 1))
            .saturating_add(clipped_w_bytes);
        if src.len() < required_src {
            return Err(FbError::Truncated);
        }

        // Defensive: never write past the back-buffer bounds (which are
        // identical to the kernel-mapped front-buffer bounds).
        let dest_stride = self.metadata.stride_bytes as usize;
        let dest_x_bytes = (clipped.x as usize).saturating_mul(bpp);
        let last_row_end = (clipped.y as usize)
            .saturating_add(clipped.h as usize)
            .saturating_sub(1)
            .saturating_mul(dest_stride)
            .saturating_add(dest_x_bytes)
            .saturating_add(clipped_w_bytes);
        if last_row_end > self.byte_len {
            return Err(FbError::OutOfBounds);
        }

        // Land the write in the userspace back buffer. `present()` is
        // the only path that pushes pixels into the visible front, so
        // intermediate compose state (clear-then-blit-then-cursor) is
        // not observable.
        for row in 0..clipped.h as usize {
            let src_row_off = (src_offset_y_rows + row) * stride + src_offset_x_bytes;
            let dest_row_off = (clipped.y as usize + row) * dest_stride + dest_x_bytes;
            self.back_buffer[dest_row_off..dest_row_off + clipped_w_bytes]
                .copy_from_slice(&src[src_row_off..src_row_off + clipped_w_bytes]);
        }
        self.dirty = true;
        // Track the row-range of every write so `present()` can copy
        // just those rows instead of the entire framebuffer. At 1080p
        // the FB is 8 MiB; copying it all on every present pegged the
        // CPU under TCG. A typical compose touches a few thousand
        // rows at most, so row-bounded copies stay cheap.
        let row_start = clipped.y as usize;
        let row_end = row_start + clipped.h as usize;
        if row_start < self.dirty_row_min {
            self.dirty_row_min = row_start;
        }
        if row_end > self.dirty_row_max_exclusive {
            self.dirty_row_max_exclusive = row_end;
        }
        Ok(())
    }

    fn present(&mut self) -> Result<(), FbError> {
        if !self.dirty {
            return Ok(());
        }
        let dest_stride = self.metadata.stride_bytes as usize;
        // Clamp the dirty row range to the FB extents; `write_pixels`
        // already bounded each row write against `byte_len`, but
        // bounding here too keeps the unsafe path defensive against a
        // future regression that forgets to.
        let row_min = self.dirty_row_min.min(self.metadata.height as usize);
        let row_max = self
            .dirty_row_max_exclusive
            .min(self.metadata.height as usize);
        if row_max > row_min {
            let byte_off = row_min.saturating_mul(dest_stride);
            let byte_len = (row_max - row_min).saturating_mul(dest_stride);
            // SAFETY: the kernel mapped `self.byte_len` writable bytes
            // at `self.base`; `byte_off + byte_len <= self.byte_len`
            // because `row_max <= metadata.height` and the back
            // buffer is sized identically. The two regions are
            // disjoint (kernel-mapped MMIO vs heap), so the copy is
            // non-overlapping.
            unsafe {
                ptr::copy_nonoverlapping(
                    self.back_buffer.as_ptr().add(byte_off),
                    self.base.add(byte_off),
                    byte_len,
                );
            }
        }
        self.dirty = false;
        self.dirty_row_min = usize::MAX;
        self.dirty_row_max_exclusive = 0;
        Ok(())
    }

    /// Phase 56 close-out (G.1) — read one BGRA8888 pixel from the
    /// mapped framebuffer at `(x, y)`. Returns
    /// [`FbError::OutOfBounds`] if the coordinate falls outside the
    /// reported `(width, height)`, or [`FbError::Unsupported`] if the
    /// active pixel format is not 4 bytes per pixel (Phase 56 ships
    /// only BGRA8888 / RGBA8888). Used only by the test-only
    /// `ReadBackPixel` control verb.
    fn read_pixel(&self, x: u32, y: u32) -> Result<u32, FbError> {
        if x >= self.metadata.width || y >= self.metadata.height {
            return Err(FbError::OutOfBounds);
        }
        let bpp = bytes_per_pixel(self.metadata.pixel_format) as usize;
        // Phase 56 ships only 4-bpp formats (BGRA8888 / RGBA8888); read
        // exactly 4 bytes and pack as a `u32`. A non-4-bpp format is a
        // backend capability gap, not a bounds error — surface it as
        // `Unsupported` so callers can distinguish it from a
        // coordinate fault.
        if bpp != 4 {
            return Err(FbError::Unsupported);
        }
        let dest_stride = self.metadata.stride_bytes as usize;
        let dest_x_bytes = (x as usize).saturating_mul(bpp);
        let off = (y as usize)
            .saturating_mul(dest_stride)
            .saturating_add(dest_x_bytes);
        if off.saturating_add(4) > self.byte_len {
            return Err(FbError::OutOfBounds);
        }
        // Read from the back buffer. With double buffering the back
        // is the source of truth between presents — readback observes
        // pixels the compositor staged, even before the next
        // `present()` pushes them to the kernel-mapped front.
        let value = u32::from_le_bytes([
            self.back_buffer[off],
            self.back_buffer[off + 1],
            self.back_buffer[off + 2],
            self.back_buffer[off + 3],
        ]);
        Ok(value)
    }
}

/// Clip a rectangle to `[0, width) × [0, height)`. Returns `None` if the
/// clipped rect has zero area. Math is in i64 to defend against
/// adversarial inputs near `i32::MAX`.
fn clip_rect(rect: Rect, width: u32, height: u32) -> Option<Rect> {
    let left = rect.x as i64;
    let top = rect.y as i64;
    let right = left + rect.w as i64;
    let bottom = top + rect.h as i64;

    let cl = left.max(0);
    let ct = top.max(0);
    let cr = right.min(width as i64);
    let cb = bottom.min(height as i64);

    if cr <= cl || cb <= ct {
        return None;
    }
    Some(Rect {
        x: cl as i32,
        y: ct as i32,
        w: (cr - cl) as u32,
        h: (cb - ct) as u32,
    })
}
