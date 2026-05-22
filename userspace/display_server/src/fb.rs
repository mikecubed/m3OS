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
//! Two backends are wired here, picked at acquire time:
//!
//! * **Memcpy back-buffer** (default fallback). `write_pixels` lands
//!   in a userspace `Vec<u8>` sized identically to the visible
//!   framebuffer; `present()` row-bounds the dirty range and memcpys
//!   it into the kernel-mapped MMIO front. Intermediate compose state
//!   (clear, then surface blits, then cursor) is never observable to
//!   the user because every intermediate write lands in heap memory.
//!   The visible-frame memcpy itself can still race the scanout
//!   cursor on the framebuffer hardware, producing residual tearing.
//!
//! * **VBE page-flip** (when `framebuffer_pageflip_query()` returns a
//!   non-zero back-buffer Y offset at acquire time). The kernel-mmap
//!   spans both halves of a virtual framebuffer that is `2 × height`
//!   rows tall; `write_pixels` writes directly into the half that is
//!   *not* currently being scanned out (the "back" half), and
//!   `present()` calls a single `framebuffer_pageflip(new_offset)`
//!   syscall which writes the VBE `Y_OFFSET` register. The display
//!   device applies the new offset on the next scanout cycle, so the
//!   compositor never has to memcpy 8 MiB per frame and the screen
//!   only ever shows a complete frame. Roles swap after each flip:
//!   what was "back" becomes "front" and vice versa.
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
/// Phase 73 — see the module doc for the two backend modes
/// (`Backend::Memcpy` and `Backend::Flip`).
pub struct KernelFramebufferOwner {
    base: *mut u8,
    metadata: FbMetadata,
    /// Total mapped byte length of one visible-half worth of pixels.
    /// In flip mode the kernel mapping is `2 × byte_len_visible`
    /// (front half + back half); in memcpy mode it equals the kernel
    /// mapping size.
    byte_len_visible: usize,
    released: bool,
    backend: Backend,
}

enum Backend {
    /// Single kernel-mapped front + userspace memcpy back. Used on
    /// every non-Bochs framebuffer and on Bochs boots where VBE
    /// double-buffering could not be enabled (e.g. insufficient
    /// `vgamem`).
    Memcpy {
        back_buffer: Vec<u8>,
        /// `true` iff `back_buffer` differs from the kernel-mapped
        /// front. Cleared by `present()` so consecutive `present()`
        /// calls with no intervening writes are no-ops.
        dirty: bool,
        /// Row-range bounding box of dirty bytes in `back_buffer`.
        /// Tracked per `write_pixels` so `present()` can memcpy just
        /// the touched rows rather than the whole framebuffer.
        /// `(usize::MAX, 0)` is the "no dirt" sentinel; `usize::MAX`
        /// keeps the inclusive-min branch sound when no writes have
        /// happened. At 1080p the full-FB memcpy was 8 MiB per
        /// present, which TCG choked on; row-bounded copies bring
        /// typical frames back into the few-hundred-KB range.
        dirty_row_min: usize,
        dirty_row_max_exclusive: usize,
    },
    /// VBE page-flip mode. The kernel mapping spans both halves of a
    /// virtual framebuffer; we render into whichever half is *not*
    /// currently being scanned out (`back_y_offset_pixels`) and ask
    /// the kernel to swap roles via [`syscall_lib::framebuffer_pageflip`]
    /// on `present()`.
    Flip {
        /// Visible height in pixels. The non-zero `Y_OFFSET` value
        /// the kernel will accept for a flip; doubles as the byte
        /// offset between the two halves in the kernel mapping
        /// (multiplied by `stride_bytes`).
        visible_height: u32,
        /// Y offset of the half currently being scanned out.
        front_y_offset: u32,
        /// `true` iff the back half was touched since the last
        /// `present()`. Cleared by `present()`.
        dirty: bool,
    },
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
        // Visible-half bytes: stride_bytes * height. Used as the
        // defensive bound on clipped writes and as the offset between
        // front/back halves when flip mode is active.
        let byte_len_visible =
            (metadata.stride_bytes as usize).saturating_mul(metadata.height as usize);

        // Probe the kernel for VBE double-buffer support. A non-zero
        // value is the visible height in pixels (and therefore the
        // back-half Y offset); zero means "no hardware support, fall
        // back to memcpy". The kernel's mmap already gave us
        // `2 × byte_len_visible` of mapped pages when the probe is
        // non-zero, so the second half is addressable without any
        // additional syscalls.
        let pageflip_back_y = syscall_lib::framebuffer_pageflip_query();
        let backend = if pageflip_back_y == metadata.height {
            Backend::Flip {
                visible_height: pageflip_back_y,
                front_y_offset: 0,
                dirty: false,
            }
        } else {
            // Allocate the userspace back buffer for the memcpy path.
            // Initial contents are zero; the first compose pass
            // overwrites every byte via `fill_background` + surface
            // blits before its terminating `present()`, so the
            // initial zero state is never observable.
            let back_buffer = alloc::vec![0u8; byte_len_visible];
            Backend::Memcpy {
                back_buffer,
                dirty: false,
                dirty_row_min: usize::MAX,
                dirty_row_max_exclusive: 0,
            }
        };

        Ok(Self {
            base,
            metadata,
            byte_len_visible,
            released: false,
            backend,
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

        // Defensive: never write past one visible-half bounds. The
        // bytes-per-row math is identical for both backends; flip mode
        // just shifts the destination by `back_y_offset × stride`.
        let dest_stride = self.metadata.stride_bytes as usize;
        let dest_x_bytes = (clipped.x as usize).saturating_mul(bpp);
        let last_row_end = (clipped.y as usize)
            .saturating_add(clipped.h as usize)
            .saturating_sub(1)
            .saturating_mul(dest_stride)
            .saturating_add(dest_x_bytes)
            .saturating_add(clipped_w_bytes);
        if last_row_end > self.byte_len_visible {
            return Err(FbError::OutOfBounds);
        }

        match &mut self.backend {
            Backend::Memcpy {
                back_buffer,
                dirty,
                dirty_row_min,
                dirty_row_max_exclusive,
            } => {
                for row in 0..clipped.h as usize {
                    let src_row_off = (src_offset_y_rows + row) * stride + src_offset_x_bytes;
                    let dest_row_off = (clipped.y as usize + row) * dest_stride + dest_x_bytes;
                    back_buffer[dest_row_off..dest_row_off + clipped_w_bytes]
                        .copy_from_slice(&src[src_row_off..src_row_off + clipped_w_bytes]);
                }
                *dirty = true;
                let row_start = clipped.y as usize;
                let row_end = row_start + clipped.h as usize;
                if row_start < *dirty_row_min {
                    *dirty_row_min = row_start;
                }
                if row_end > *dirty_row_max_exclusive {
                    *dirty_row_max_exclusive = row_end;
                }
            }
            Backend::Flip {
                visible_height,
                front_y_offset,
                dirty,
            } => {
                // The back half is whichever one is not currently
                // being scanned out. With only two halves, that's
                // `0` XOR `visible_height`.
                let back_y_offset = if *front_y_offset == 0 {
                    *visible_height as usize
                } else {
                    0
                };
                let back_byte_offset = back_y_offset * dest_stride;
                // SAFETY: the kernel mapped `2 × byte_len_visible`
                // writable bytes at `self.base`. We bounded the
                // visible-half destination above; adding the back
                // offset keeps us inside the second half because the
                // halves are the same size.
                unsafe {
                    let dest_base = self.base.add(back_byte_offset);
                    for row in 0..clipped.h as usize {
                        let src_row_off = (src_offset_y_rows + row) * stride + src_offset_x_bytes;
                        let dest_row_off = (clipped.y as usize + row) * dest_stride + dest_x_bytes;
                        ptr::copy_nonoverlapping(
                            src.as_ptr().add(src_row_off),
                            dest_base.add(dest_row_off),
                            clipped_w_bytes,
                        );
                    }
                }
                *dirty = true;
            }
        }
        Ok(())
    }

    fn present(&mut self) -> Result<(), FbError> {
        match &mut self.backend {
            Backend::Memcpy {
                back_buffer,
                dirty,
                dirty_row_min,
                dirty_row_max_exclusive,
            } => {
                if !*dirty {
                    return Ok(());
                }
                let dest_stride = self.metadata.stride_bytes as usize;
                // Clamp the dirty row range to the FB extents.
                let row_min = (*dirty_row_min).min(self.metadata.height as usize);
                let row_max = (*dirty_row_max_exclusive).min(self.metadata.height as usize);
                if row_max > row_min {
                    let byte_off = row_min.saturating_mul(dest_stride);
                    let byte_len = (row_max - row_min).saturating_mul(dest_stride);
                    // SAFETY: see acquire() — the kernel mapped
                    // `byte_len_visible` writable bytes (or more in
                    // flip mode, but flip mode never takes this
                    // branch). `byte_off + byte_len` ≤
                    // `byte_len_visible` because `row_max ≤
                    // metadata.height`. Source and destination are
                    // disjoint (kernel-mapped MMIO vs heap).
                    unsafe {
                        ptr::copy_nonoverlapping(
                            back_buffer.as_ptr().add(byte_off),
                            self.base.add(byte_off),
                            byte_len,
                        );
                    }
                }
                *dirty = false;
                *dirty_row_min = usize::MAX;
                *dirty_row_max_exclusive = 0;
                Ok(())
            }
            Backend::Flip {
                visible_height,
                front_y_offset,
                dirty,
            } => {
                if !*dirty {
                    return Ok(());
                }
                let new_offset = if *front_y_offset == 0 {
                    *visible_height
                } else {
                    0
                };
                let rc = syscall_lib::framebuffer_pageflip(new_offset);
                if rc < 0 {
                    return Err(FbError::Unsupported);
                }
                *front_y_offset = new_offset;
                *dirty = false;
                Ok(())
            }
        }
    }

    fn needs_full_repaint_per_frame(&self) -> bool {
        // Flip mode swaps to a half that only contains pixels written
        // since the previous present — partial-damage frames would
        // leave stale content from two frames ago in any region the
        // compositor didn't touch. Forcing a full repaint per frame
        // keeps the visible half always-correct in exchange for ~8
        // MiB of pixel writes per frame (cheap relative to the memcpy
        // we just removed). Memcpy mode keeps the cursor-only fast
        // path because its back buffer is a true mirror of the front.
        matches!(self.backend, Backend::Flip { .. })
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
        if off.saturating_add(4) > self.byte_len_visible {
            return Err(FbError::OutOfBounds);
        }
        match &self.backend {
            Backend::Memcpy { back_buffer, .. } => {
                // Read from the userspace back buffer — it is the
                // source of truth between presents, so the test-only
                // `ReadBackPixel` verb observes pixels the compositor
                // staged before the next memcpy reaches the front.
                Ok(u32::from_le_bytes([
                    back_buffer[off],
                    back_buffer[off + 1],
                    back_buffer[off + 2],
                    back_buffer[off + 3],
                ]))
            }
            Backend::Flip {
                visible_height,
                front_y_offset,
                ..
            } => {
                // Read from whichever half is currently being scanned
                // out (the visible front). The back half holds
                // in-flight, partially-rendered pixels which would
                // mislead the test-only verb. The byte offset is
                // (front_y_offset × stride) plus the row offset.
                let half_byte_off = (*front_y_offset as usize) * dest_stride;
                let full_off = half_byte_off + off;
                // SAFETY: front_y_offset is 0 or visible_height; both
                // map to addresses inside the `2 × byte_len_visible`
                // kernel mapping. We bounded `off` against
                // `byte_len_visible` above, so `full_off + 4` ≤
                // `2 × byte_len_visible`.
                let bytes = unsafe {
                    let p = self.base.add(full_off);
                    [
                        core::ptr::read_volatile(p),
                        core::ptr::read_volatile(p.add(1)),
                        core::ptr::read_volatile(p.add(2)),
                        core::ptr::read_volatile(p.add(3)),
                    ]
                };
                let _ = visible_height; // keep field documented + reachable
                Ok(u32::from_le_bytes(bytes))
            }
        }
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
