//! Phase 100 Track E.1 — cheap render fingerprint for bare-metal validation.
//!
//! Computes a cheap fingerprint of the compositor's composed output so that
//! "the screen shows the greeter" can be falsified on bare metal — where
//! there is no QMP/PPM screendump path. This is the on-device analog of the
//! `less-render-probe`/`claude_tui_render_arm` PPM band-diff.
//!
//! ## What is measured
//!
//! - **`rows_nonblank`**: how many scanlines contain at least one
//!   non-background pixel (sampled at [`SAMPLE_STEP`] column intervals).
//!   After `fill_background` this is 0; after the greeter renders its
//!   login dialog this is a large positive number.
//! - **`rows_changed`**: how many scanlines differ from the previous
//!   composed frame (per-row FNV-1a hash comparison). Zero on the first
//!   frame (no previous); non-zero on any frame with new content.
//! - **`hash`**: FNV-1a over sampled pixels from all non-blank scanlines.
//!   Stable when the greeter is static; changes with any animated content.
//!
//! ## Threshold (distinguishing "rendered" from "blank")
//!
//! The background colour is `BG_PIXEL = 0x002B_5A4B` (deep teal), which
//! is what `fill_background` writes. Any pixel != `BG_PIXEL` makes its
//! scanline non-blank.
//!
//! | Scenario                                 | `rows_nonblank`       |
//! |------------------------------------------|-----------------------|
//! | All-background (blank) frame             | `= 0`                 |
//! | Something rendered (conservative)        | `>= 50`               |
//! | Greeter dialog visible on 1080p panel    | `>= 200`              |
//! | Full-screen app covering the whole panel | `≈ height` (e.g. 1080)|
//!
//! A truly black (all-zero) framebuffer yields `rows_nonblank = height`
//! (every zero pixel ≠ teal), which indicates the compositor has not yet
//! run `fill_background` at all — a distinct diagnostic signal.
//!
//! ## Sentinel format (greppable, stable)
//!
//! ```text
//! RENDER_FP frame=<n> rows_nonblank=<R> rows_changed=<C> hash=0x<8hex>
//! ```
//!
//! Track C reuses this format for cursor-motion render deltas: a cursor
//! move produces a small `rows_changed` value and a slightly different
//! `hash` even when `rows_nonblank` stays constant.
//!
//! ## Cost
//!
//! At 1920×1080 with `SAMPLE_STEP = 16`: 1080 rows × 120 samples/row =
//! 129 600 pixel reads per frame. The back buffer is cache-warm after the
//! compose pass, so this is a few microseconds — well under one frame
//! budget.
//!
//! ## Host tests
//!
//! The pure functions below are testable on the host:
//!
//! ```text
//! cargo test -p kernel-core --target x86_64-unknown-linux-gnu
//! ```

/// Background pixel value (BGRA8888 teal) — same constant as
/// `display_server::main::BG_PIXEL`. Duplicated here so the fingerprint
/// module is self-contained and host-testable without depending on the
/// binary crate.
pub const BG_PIXEL: u32 = 0x002B_5A4B;

/// Column sampling interval for the fingerprint. Every `SAMPLE_STEP`-th
/// pixel per row is inspected. At 1920 px wide this yields 120 samples
/// per row — enough to detect any text, logo, or dialog element wider
/// than 16 pixels.
pub const SAMPLE_STEP: u32 = 16;

/// Render fingerprint for one composed frame.
///
/// Emitted via the `RENDER_FP` sentinel line (see module doc).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RenderFingerprint {
    /// Monotonic compose-frame counter. Bumped by `display_server::main`
    /// on each compose-with-writes frame; useful for confirming the
    /// compositor is advancing across a CI boot log.
    pub frame: u64,
    /// Scanlines with at least one non-background pixel (sampled).
    /// Zero on a background-only (blank) framebuffer.
    pub rows_nonblank: u32,
    /// Scanlines that differ (via per-row hash) from the previous frame.
    /// Zero when there is no previous frame to compare against.
    pub rows_changed: u32,
    /// FNV-1a fold over sampled pixels from all non-blank scanlines.
    /// Stable while the greeter is static; changes when any pixel moves.
    pub hash: u32,
}

/// FNV-1a 32-bit offset basis.
const FNV_OFFSET: u32 = 2_166_136_261;
/// FNV-1a 32-bit prime.
const FNV_PRIME: u32 = 16_777_619;

/// Return `true` when scanline `y` contains at least one pixel that
/// differs from `bg` in the sampled columns.
///
/// Assumes 4 bytes per pixel (BGRA8888 or RGBA8888). Rows outside the
/// `pixels` slice are treated as blank.
#[inline]
pub fn row_is_nonblank(pixels: &[u8], y: u32, width: u32, stride_bytes: u32, bg: u32) -> bool {
    let row_off = (y as usize).saturating_mul(stride_bytes as usize);
    let mut x: u32 = 0;
    while x < width {
        let px_off = row_off + (x as usize).saturating_mul(4);
        if px_off + 4 > pixels.len() {
            break;
        }
        let px = u32::from_le_bytes([
            pixels[px_off],
            pixels[px_off + 1],
            pixels[px_off + 2],
            pixels[px_off + 3],
        ]);
        if px != bg {
            return true;
        }
        x += SAMPLE_STEP;
    }
    false
}

/// Compute a FNV-1a hash of the sampled pixels on scanline `y`.
///
/// Samples every `SAMPLE_STEP` columns. Rows outside the `pixels` slice
/// produce the bare FNV offset basis (`FNV_OFFSET`), which is distinct
/// from a row of all-zero pixels.
#[inline]
pub fn row_sample_hash(pixels: &[u8], y: u32, width: u32, stride_bytes: u32) -> u32 {
    let row_off = (y as usize).saturating_mul(stride_bytes as usize);
    let mut h: u32 = FNV_OFFSET;
    let mut x: u32 = 0;
    while x < width {
        let px_off = row_off + (x as usize).saturating_mul(4);
        if px_off + 4 > pixels.len() {
            break;
        }
        // Fold all four bytes of the pixel into the hash.
        h ^= pixels[px_off] as u32;
        h = h.wrapping_mul(FNV_PRIME);
        h ^= pixels[px_off + 1] as u32;
        h = h.wrapping_mul(FNV_PRIME);
        h ^= pixels[px_off + 2] as u32;
        h = h.wrapping_mul(FNV_PRIME);
        h ^= pixels[px_off + 3] as u32;
        h = h.wrapping_mul(FNV_PRIME);
        x += SAMPLE_STEP;
    }
    h
}

/// Compute the render fingerprint for one composed frame.
///
/// # Arguments
///
/// - `pixels`: flat BGRA8888 back-buffer bytes (4 bpp, packed or with
///   stride padding). Must be at least `height × stride_bytes` bytes.
/// - `width`, `height`, `stride_bytes`: framebuffer geometry.
/// - `bg`: background pixel value (use `BG_PIXEL`).
/// - `frame`: caller-supplied monotonic frame counter.
/// - `prev_row_hashes`: per-row hash array from the previous call.
///   Pass an empty slice on the first frame; `rows_changed` will be 0.
///
/// # Returns
///
/// A `(RenderFingerprint, Vec<u32>)` pair. The `Vec<u32>` is the current
/// frame's per-row hashes; pass it back as `prev_row_hashes` on the next
/// call to get a valid `rows_changed`.
///
/// This convenience form allocates a fresh `Vec` per call. The compositor
/// hot loop — which composes once per damage frame (cursor motion,
/// animations) — should instead use [`compute_fingerprint_into`] with two
/// caller-owned buffers it swaps each frame, so steady-state composes are
/// allocation-free.
pub fn compute_fingerprint(
    pixels: &[u8],
    width: u32,
    height: u32,
    stride_bytes: u32,
    bg: u32,
    frame: u64,
    prev_row_hashes: &[u32],
) -> (RenderFingerprint, alloc::vec::Vec<u32>) {
    let mut row_hashes: alloc::vec::Vec<u32> = alloc::vec::Vec::with_capacity(height as usize);
    let fp = compute_fingerprint_into(
        pixels,
        width,
        height,
        stride_bytes,
        bg,
        frame,
        prev_row_hashes,
        &mut row_hashes,
    );
    (fp, row_hashes)
}

/// Allocation-free variant of [`compute_fingerprint`] that writes the
/// current frame's per-row hashes into a caller-owned buffer.
///
/// `out_row_hashes` is [`Vec::clear`]ed and refilled in place, so once its
/// capacity has grown to `height` (after the first frame) no further heap
/// allocation occurs. Intended usage in a render loop: keep two buffers and
/// swap them each frame —
///
/// ```text
/// let fp = compute_fingerprint_into(.., &prev, &mut curr);
/// core::mem::swap(&mut prev, &mut curr); // `prev` now holds this frame
/// ```
///
/// The borrow checker guarantees `prev_row_hashes` and `out_row_hashes`
/// cannot be the same `Vec` (shared + exclusive borrow), so the clear can
/// never clobber the previous-frame data being compared against.
// One arg over clippy's default: the geometry tuple (pixels/width/height/
// stride/bg) mirrors the sibling `compute_fingerprint`, and bundling it into
// a struct would churn every call site/test for no readability gain here.
#[allow(clippy::too_many_arguments)]
pub fn compute_fingerprint_into(
    pixels: &[u8],
    width: u32,
    height: u32,
    stride_bytes: u32,
    bg: u32,
    frame: u64,
    prev_row_hashes: &[u32],
    out_row_hashes: &mut alloc::vec::Vec<u32>,
) -> RenderFingerprint {
    let mut rows_nonblank: u32 = 0;
    let mut rows_changed: u32 = 0;
    // Frame hash folds non-blank row hashes only so a background-only
    // frame yields a stable all-background value regardless of height.
    let mut frame_hash: u32 = FNV_OFFSET;

    out_row_hashes.clear();
    out_row_hashes.reserve(height as usize);

    for y in 0..height {
        let rh = row_sample_hash(pixels, y, width, stride_bytes);
        out_row_hashes.push(rh);

        if row_is_nonblank(pixels, y, width, stride_bytes, bg) {
            rows_nonblank += 1;
            // Fold the row hash into the frame hash (non-blank rows only).
            frame_hash ^= rh;
            frame_hash = frame_hash.wrapping_mul(FNV_PRIME);
        }

        if let Some(&prev_h) = prev_row_hashes.get(y as usize)
            && rh != prev_h
        {
            rows_changed += 1;
        }
    }

    RenderFingerprint {
        frame,
        rows_nonblank,
        rows_changed,
        hash: frame_hash,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    // Build a flat BGRA8888 frame of `width × height` pixels all set to
    // `fill`. Stride == width × 4 (packed, no row padding).
    fn make_frame(width: u32, height: u32, fill: u32) -> alloc::vec::Vec<u8> {
        let n = (width as usize) * (height as usize) * 4;
        let bytes = fill.to_le_bytes();
        let mut v = vec![0u8; n];
        for chunk in v.chunks_exact_mut(4) {
            chunk.copy_from_slice(&bytes);
        }
        v
    }

    // Paint rows `y_start..y_end` with `colour`.
    fn paint_rows(buf: &mut [u8], width: u32, y_start: u32, y_end: u32, stride: u32, colour: u32) {
        let bytes = colour.to_le_bytes();
        for y in y_start..y_end {
            let row_off = (y as usize) * (stride as usize);
            for x in 0..width as usize {
                let off = row_off + x * 4;
                buf[off..off + 4].copy_from_slice(&bytes);
            }
        }
    }

    /// An all-background frame has zero non-blank rows and zero changed
    /// rows (no previous frame to compare against on the first call).
    #[test]
    fn all_background_yields_zero_nonblank() {
        let (w, h) = (64u32, 48u32);
        let stride = w * 4;
        let pixels = make_frame(w, h, BG_PIXEL);

        // row_is_nonblank must return false for every row.
        for y in 0..h {
            assert!(
                !row_is_nonblank(&pixels, y, w, stride, BG_PIXEL),
                "row {y} should be blank (all background)"
            );
        }

        let (fp, hashes) = compute_fingerprint(&pixels, w, h, stride, BG_PIXEL, 0, &[]);
        assert_eq!(fp.rows_nonblank, 0, "all-background → rows_nonblank = 0");
        assert_eq!(fp.rows_changed, 0, "no previous frame → rows_changed = 0");
        assert_eq!(fp.frame, 0);
        assert_eq!(hashes.len(), h as usize);
    }

    /// Known non-background rows are counted correctly.
    #[test]
    fn nonblank_rows_counted_correctly() {
        let (w, h) = (64u32, 16u32);
        let stride = w * 4;
        let mut pixels = make_frame(w, h, BG_PIXEL);
        // Paint rows 4..8 red — 4 non-blank rows.
        paint_rows(&mut pixels, w, 4, 8, stride, 0x00FF_0000);

        let (fp, _) = compute_fingerprint(&pixels, w, h, stride, BG_PIXEL, 1, &[]);
        assert_eq!(
            fp.rows_nonblank, 4,
            "rows 4..8 must be counted as non-blank"
        );
        assert_eq!(fp.frame, 1);
    }

    /// A frame with exactly one changed row yields `rows_changed = 1`.
    #[test]
    fn rows_changed_counts_one_differing_row() {
        let (w, h) = (32u32, 8u32);
        let stride = w * 4;
        let frame0 = make_frame(w, h, BG_PIXEL);
        let (_, prev) = compute_fingerprint(&frame0, w, h, stride, BG_PIXEL, 0, &[]);

        // Change one pixel in row 3 so its hash differs.
        let mut frame1 = frame0.clone();
        let off = 3 * stride as usize; // first pixel of row 3
        frame1[off..off + 4].copy_from_slice(&0x00FF_FF00u32.to_le_bytes());

        let (fp, _) = compute_fingerprint(&frame1, w, h, stride, BG_PIXEL, 1, &prev);
        assert_eq!(fp.rows_changed, 1, "exactly one row was modified");
        // Row 3 is now non-blank (was background before).
        assert!(fp.rows_nonblank >= 1, "the changed row should be non-blank");
    }

    /// Two identical frames produce `rows_changed = 0`.
    #[test]
    fn identical_frames_yield_zero_changed() {
        let (w, h) = (32u32, 8u32);
        let stride = w * 4;
        // Use a non-background colour so rows_nonblank > 0.
        let frame = make_frame(w, h, 0x00AB_CDEF);
        let (_, prev) = compute_fingerprint(&frame, w, h, stride, BG_PIXEL, 0, &[]);
        let (fp, _) = compute_fingerprint(&frame, w, h, stride, BG_PIXEL, 1, &prev);
        assert_eq!(fp.rows_changed, 0, "identical frames → rows_changed = 0");
    }

    /// A background-only frame followed by a frame with painted rows reports
    /// the painted rows as changed.
    #[test]
    fn background_then_painted_frame_reports_changed() {
        let (w, h) = (64u32, 32u32);
        let stride = w * 4;
        let frame0 = make_frame(w, h, BG_PIXEL);
        let (_, prev) = compute_fingerprint(&frame0, w, h, stride, BG_PIXEL, 0, &[]);

        let mut frame1 = make_frame(w, h, BG_PIXEL);
        // Paint rows 10..20 with a non-background colour.
        paint_rows(&mut frame1, w, 10, 20, stride, 0x0055_AAFF);

        let (fp, _) = compute_fingerprint(&frame1, w, h, stride, BG_PIXEL, 1, &prev);
        assert_eq!(fp.rows_nonblank, 10, "10 rows painted non-blank");
        assert_eq!(fp.rows_changed, 10, "those 10 rows changed vs. previous");
    }

    /// The allocation-free `_into` variant produces identical fingerprints and
    /// per-row hashes to the allocating `compute_fingerprint`.
    #[test]
    fn into_variant_matches_allocating() {
        let (w, h) = (64u32, 24u32);
        let stride = w * 4;
        let mut pixels = make_frame(w, h, BG_PIXEL);
        paint_rows(&mut pixels, w, 5, 9, stride, 0x0012_3456);

        let (fp_alloc, hashes_alloc) = compute_fingerprint(&pixels, w, h, stride, BG_PIXEL, 7, &[]);

        let mut out = alloc::vec::Vec::new();
        let fp_into = compute_fingerprint_into(&pixels, w, h, stride, BG_PIXEL, 7, &[], &mut out);

        assert_eq!(fp_into, fp_alloc, "fingerprints must match");
        assert_eq!(out, hashes_alloc, "per-row hashes must match");
    }

    /// A two-buffer swap loop reuses its buffers: after the first frame grows
    /// capacity to `height`, subsequent frames allocate nothing (capacity is
    /// retained across `clear()`), and `rows_changed` is still computed
    /// correctly against the previous frame.
    #[test]
    fn into_variant_swap_loop_is_capacity_stable() {
        let (w, h) = (32u32, 16u32);
        let stride = w * 4;
        let mut prev = alloc::vec::Vec::new();
        let mut curr = alloc::vec::Vec::new();

        // Frame 0: all background → establishes baseline hashes.
        let f0 = make_frame(w, h, BG_PIXEL);
        let _ = compute_fingerprint_into(&f0, w, h, stride, BG_PIXEL, 0, &prev, &mut curr);
        core::mem::swap(&mut prev, &mut curr);
        let cap_after_first = prev.capacity();
        assert!(cap_after_first >= h as usize);

        // Frame 1: paint 3 rows → exactly 3 rows changed vs. frame 0.
        let mut f1 = make_frame(w, h, BG_PIXEL);
        paint_rows(&mut f1, w, 2, 5, stride, 0x00AA_55FF);
        let fp1 = compute_fingerprint_into(&f1, w, h, stride, BG_PIXEL, 1, &prev, &mut curr);
        core::mem::swap(&mut prev, &mut curr);
        assert_eq!(
            fp1.rows_changed, 3,
            "3 rows differ from the background frame"
        );
        // The buffer now reused for `prev` must not have reallocated.
        assert_eq!(
            prev.capacity(),
            cap_after_first,
            "swap loop must not reallocate after the first frame"
        );

        // Frame 2: identical to frame 1 → 0 rows changed.
        let fp2 = compute_fingerprint_into(&f1, w, h, stride, BG_PIXEL, 2, &prev, &mut curr);
        assert_eq!(fp2.rows_changed, 0, "identical frame → rows_changed = 0");
    }
}
