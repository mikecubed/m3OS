//! Phase 69c Track C.1 — bounded LRU glyph atlas.
//!
//! `Atlas` is a per-`term`-instance cache keyed by Unicode codepoint.
//! On a miss it walks the font (`Font::glyph_index` →
//! `Font::glyph_outline` → `Rasterizer::rasterize_glyph`) and inserts
//! the new bitmap; on a hit it returns the cached bitmap. The cache
//! is bounded to a configurable number of entries (default 1024) so
//! an adversarial codepoint stream cannot OOM `term`.
//!
//! The LRU policy keeps a doubly-linked list of slot indices: each
//! cache hit moves the entry to the front; every miss evicts from
//! the back. The codepoint-to-slot map is a small linear scan
//! because the typical hot set fits in CPU cache lines and a hash
//! map would pull `hashbrown` into `kernel-core`.

use alloc::vec::Vec;

use super::parser::Font;
use super::raster::{CellMetrics, RasterBitmap, Rasterizer};

/// Default per-`term` atlas capacity (cached glyph count, not a
/// memory ceiling). Packed-bitmap pixel data scales with the
/// configured cell size — the bitmap layout is
/// `ceil(cell_w / 8) * cell_h` bytes per glyph (see
/// [`RasterBitmap::blank`](crate::font::raster::RasterBitmap::blank)).
/// `term`'s current runtime cell is 16 × 32, so packed pixel data is
/// 64 bytes per glyph (~64 KiB at 1024 entries). Real per-`term`
/// heap footprint is higher: each cached entry also carries a `Slot`
/// (codepoint + prev/next link fields + the `RasterBitmap`'s
/// `Vec<u8>` header) plus a separately allocated bitmap heap block.
/// Smaller cell sizes (e.g. an 8 × 16 host-test fixture, which packs
/// to 16 bytes per glyph) shrink the pixel-data line-item
/// proportionally.
pub const DEFAULT_ATLAS_CAPACITY: usize = 1024;

/// Errors observable from the atlas API.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum AtlasError {
    /// Capacity is zero; the cache would never hold anything.
    CapacityTooSmall,
    /// Cell dimensions are below the minimum the fallback-dot
    /// renderer can safely paint. The centred-dot is a 2 × 2 stamp
    /// whose top-left coordinate is `(cell/2) - 1`; that underflows
    /// for `cell_w < 2` or `cell_h < 2`.
    CellTooSmall,
    /// The font byte buffer handed to [`Atlas::new`] could not be
    /// parsed (`Font::open` failed because the bytes are malformed,
    /// truncated, or missing required tables). Per-glyph outline
    /// failures (a covered codepoint whose outline cannot be
    /// reconstructed) are not surfaced through `AtlasError` — they
    /// are absorbed by [`Atlas::resolve`] and routed to the shared
    /// fallback bitmap so callers do not have to branch on a
    /// per-codepoint error type.
    Malformed,
}

/// Bounded LRU glyph atlas. Owns the loaded font's bytes (the
/// caller hands a `Vec<u8>` in) so the atlas's lifetime governs the
/// font.
pub struct Atlas {
    bytes: Vec<u8>,
    metrics: CellMetrics,
    capacity: usize,
    fallback: RasterBitmap,
    blank: RasterBitmap,
    entries: Vec<Slot>,
    /// Most-recently-used slot index (or `usize::MAX` when empty).
    head: usize,
    /// Least-recently-used slot index (or `usize::MAX` when empty).
    tail: usize,
}

#[derive(Clone, Debug)]
struct Slot {
    codepoint: u32,
    bitmap: RasterBitmap,
    prev: usize,
    next: usize,
}

const NIL: usize = usize::MAX;

impl Atlas {
    /// Construct a fresh atlas from already-loaded font bytes plus
    /// cell metrics. The metrics typically come from a constant —
    /// `term` currently passes 16 × 32 (see
    /// `userspace/term/src/display.rs`'s `CELL_WIDTH` / `CELL_HEIGHT`),
    /// and the host-side tests use 8 × 16 — combined with the font's
    /// `units_per_em` / `ascender` / `descender`.
    ///
    /// `cell_w` / `cell_h` are `u8` because [`RasterBitmap`] stores
    /// its dimensions as `u8` and the rasterizer would otherwise
    /// silently truncate a wider value when allocating the bitmap.
    /// Callers that want cells larger than 255 px need a different
    /// API.
    pub fn new(
        bytes: Vec<u8>,
        cell_w: u8,
        cell_h: u8,
        capacity: usize,
    ) -> Result<Self, AtlasError> {
        if capacity == 0 {
            return Err(AtlasError::CapacityTooSmall);
        }
        if cell_w < 2 || cell_h < 2 {
            return Err(AtlasError::CellTooSmall);
        }
        // Parse once to extract metrics; we re-parse on each resolve
        // because `ttf-parser::Face` borrows from the byte buffer
        // and we don't want to thread a self-referential struct
        // through `Atlas`.
        let metrics = {
            let font = Font::open(&bytes).map_err(|_| AtlasError::Malformed)?;
            CellMetrics {
                cell_w,
                cell_h,
                units_per_em: font.units_per_em(),
                ascender: font.ascender(),
                descender: font.descender(),
            }
        };
        let fallback = build_fallback(cell_w, cell_h);
        let blank = RasterBitmap::blank(cell_w, cell_h);
        Ok(Self {
            bytes,
            metrics,
            capacity,
            fallback,
            blank,
            entries: Vec::with_capacity(capacity),
            head: NIL,
            tail: NIL,
        })
    }

    /// Current number of cached glyphs.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True when the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Capacity ceiling.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Bitmap dimensions handed back from [`Atlas::resolve`].
    pub fn cell_size(&self) -> (u8, u8) {
        (self.metrics.cell_w, self.metrics.cell_h)
    }

    /// True when `codepoint` currently occupies a cache slot. Does
    /// not update LRU order. Exposed so callers — notably the
    /// adversarial smoke test — can assert eviction policy without
    /// having to inspect private slot state.
    pub fn contains(&self, codepoint: u32) -> bool {
        self.find_slot(codepoint).is_some()
    }

    /// Look up the bitmap for `codepoint`. Returns the cached entry
    /// on a hit; rasterizes + inserts on a miss; falls back to the
    /// centred-dot bitmap when the font does not cover the
    /// codepoint or rasterization fails.
    pub fn resolve(&mut self, codepoint: u32) -> &RasterBitmap {
        if is_blank_codepoint(codepoint) {
            return &self.blank;
        }
        if let Some(idx) = self.find_slot(codepoint) {
            self.move_to_front(idx);
            return &self.entries[idx].bitmap;
        }
        // Rasterize. Borrow the bytes through a fresh `Font` so the
        // atlas's `entries` borrow chain isn't pulled into the
        // parser's lifetime.
        let bitmap = self.rasterize(codepoint);
        let bitmap = match bitmap {
            Some(b) => b,
            None => return &self.fallback,
        };
        self.insert(codepoint, bitmap);
        // `insert` always promotes the new slot to head, so head is
        // the slot we just inserted.
        let idx = self.head;
        &self.entries[idx].bitmap
    }

    fn rasterize(&self, codepoint: u32) -> Option<RasterBitmap> {
        let font = Font::open(&self.bytes).ok()?;
        let id = font.glyph_index(codepoint)?;
        match font.glyph_outline(id) {
            Ok(outline) if !outline.segments.is_empty() => {
                Some(Rasterizer.rasterize_glyph(&outline, self.metrics))
            }
            // Empty-outline cases — either ttf-parser returned
            // `Some(empty bbox)` for a glyph with no contours, or
            // (via `Err`) the glyph has no outline data at all
            // (bitmap-only / colour-only / reconstruction failure).
            // We can't tell from the outline alone whether the
            // codepoint is "intentionally blank" (ASCII space) or
            // "supposed to render but unrasterizable" (bitmap-only
            // glyph), so we use the codepoint to decide:
            Ok(_) | Err(_) if renders_blank_when_unrasterizable(codepoint) => Some(
                RasterBitmap::blank(self.metrics.cell_w, self.metrics.cell_h),
            ),
            // For every other codepoint, an absent or empty outline
            // means "we can't draw this" — return None so
            // [`Atlas::resolve`] hands back the visible centred-dot
            // fallback rather than an invisible blank cell.
            Ok(_) | Err(_) => None,
        }
    }

    fn find_slot(&self, codepoint: u32) -> Option<usize> {
        self.entries
            .iter()
            .position(|slot| slot.codepoint == codepoint)
    }

    fn insert(&mut self, codepoint: u32, bitmap: RasterBitmap) {
        if self.entries.len() == self.capacity {
            self.evict_oldest();
        }
        let idx = self.entries.len();
        self.entries.push(Slot {
            codepoint,
            bitmap,
            prev: NIL,
            next: self.head,
        });
        if self.head != NIL {
            self.entries[self.head].prev = idx;
        }
        self.head = idx;
        if self.tail == NIL {
            self.tail = idx;
        }
    }

    fn evict_oldest(&mut self) {
        if self.tail == NIL {
            return;
        }
        let victim = self.tail;
        let prev = self.entries[victim].prev;
        if prev != NIL {
            self.entries[prev].next = NIL;
        } else {
            // Cache shrinks to empty.
            self.head = NIL;
        }
        self.tail = prev;

        // Swap-remove the victim. If the moved slot is referenced
        // by head / tail / neighbours, retarget them.
        let last = self.entries.len() - 1;
        if victim != last {
            let moved_old_idx = last;
            let moved_new_idx = victim;
            // Update head/tail
            if self.head == moved_old_idx {
                self.head = moved_new_idx;
            }
            if self.tail == moved_old_idx {
                self.tail = moved_new_idx;
            }
            // Retarget neighbours
            let prev = self.entries[moved_old_idx].prev;
            let next = self.entries[moved_old_idx].next;
            if prev != NIL {
                self.entries[prev].next = moved_new_idx;
            }
            if next != NIL {
                self.entries[next].prev = moved_new_idx;
            }
        }
        self.entries.swap_remove(victim);
    }

    fn move_to_front(&mut self, idx: usize) {
        if self.head == idx {
            return;
        }
        // Unlink idx from its current position.
        let prev = self.entries[idx].prev;
        let next = self.entries[idx].next;
        if prev != NIL {
            self.entries[prev].next = next;
        }
        if next != NIL {
            self.entries[next].prev = prev;
        }
        if self.tail == idx {
            self.tail = prev;
        }
        // Insert idx at head.
        self.entries[idx].prev = NIL;
        self.entries[idx].next = self.head;
        if self.head != NIL {
            self.entries[self.head].prev = idx;
        }
        self.head = idx;
    }

    /// Iterator over (codepoint, bitmap) in LRU order from MRU to
    /// LRU. Test helper only.
    #[cfg(test)]
    fn lru_order(&self) -> Vec<u32> {
        let mut out = Vec::new();
        let mut cur = self.head;
        while cur != NIL {
            out.push(self.entries[cur].codepoint);
            cur = self.entries[cur].next;
        }
        out
    }
}

/// Codepoints that always render as a blank cell — ASCII control
/// (0x00–0x1F, 0x7F), the C1 control range (0x80–0x9F), and the
/// no-break space (0xA0). Mirrors Phase 69b's
/// `glyph_tables::resolve_glyph` blank classification.
fn is_blank_codepoint(codepoint: u32) -> bool {
    codepoint <= 0x1F
        || codepoint == 0x7F
        || (0x80..=0x9F).contains(&codepoint)
        || codepoint == 0xA0
}

/// Codepoints that should render as a blank cell when the font
/// supplies no rasterizable outline. ASCII space (`U+0020`) is the
/// canonical printable example: some fonts (e.g. JetBrainsMono Nerd
/// Font Mono) record space as a cmap entry with no `glyf` data, so
/// `glyph_outline` returns `Err`. The visual expectation is still
/// "blank cell", not the visible centred-dot fallback that
/// uncovered-yet-supposed-to-render codepoints get. Codepoints that
/// always render blank regardless of font (control codes, NBSP) are
/// already filtered by [`is_blank_codepoint`] before this check
/// runs.
fn renders_blank_when_unrasterizable(codepoint: u32) -> bool {
    codepoint == 0x20
}

/// Build the centred-dot fallback bitmap. Matches Phase 69b's
/// `FALLBACK_DOT_GLYPH` shape so a font miss is visually
/// indistinguishable from the static-table fallback.
fn build_fallback(width: u8, height: u8) -> RasterBitmap {
    let mut bm = RasterBitmap::blank(width, height);
    let cx = (width as usize) / 2;
    let cy = (height as usize) / 2;
    // 2 × 2 dot centred on (cx, cy).
    for dy in 0..2 {
        for dx in 0..2 {
            let x = cx + dx - 1;
            let y = cy + dy - 1;
            if x < width as usize && y < height as usize {
                let bytes_per_row = (width as usize).div_ceil(8);
                let byte_idx = y * bytes_per_row + x / 8;
                let bit_idx = 7 - (x % 8);
                bm.bitmap[byte_idx] |= 1u8 << bit_idx;
            }
        }
    }
    bm
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::font::test_fixtures::load_test_font_bytes as load_bytes;

    #[test]
    fn capacity_zero_rejected() {
        let bytes = vec![0u8; 4];
        let err = Atlas::new(bytes, 8, 16, 0).err().expect("expected error");
        assert_eq!(err, AtlasError::CapacityTooSmall);
    }

    #[test]
    fn cell_below_two_rejected() {
        // cell_w = 0 / 1 or cell_h = 0 / 1 would underflow
        // `build_fallback`'s centred-dot calculation. The constructor
        // must reject these dimensions up front rather than relying
        // on every caller to guard.
        for (w, h) in [(0u8, 16u8), (1, 16), (8, 0), (8, 1), (1, 1)] {
            let bytes = vec![0u8; 4];
            let err = Atlas::new(bytes, w, h, 16).err().expect("expected error");
            assert_eq!(err, AtlasError::CellTooSmall, "w={w} h={h}");
        }
    }

    #[test]
    fn malformed_bytes_rejected() {
        let bytes = vec![0u8; 4];
        let err = Atlas::new(bytes, 8, 16, 16).err().expect("expected error");
        assert_eq!(err, AtlasError::Malformed);
    }

    #[test]
    fn miss_then_hit_returns_same_bitmap() {
        let Some(bytes) = load_bytes() else {
            return;
        };
        let mut atlas = Atlas::new(bytes, 8, 16, 16).expect("build atlas");
        let first_ink = atlas.resolve(b'A' as u32).ink_count();
        let second_ink = atlas.resolve(b'A' as u32).ink_count();
        assert_eq!(first_ink, second_ink);
        assert_eq!(atlas.len(), 1);
    }

    #[test]
    fn lru_eviction_drops_oldest() {
        let Some(bytes) = load_bytes() else {
            return;
        };
        let mut atlas = Atlas::new(bytes, 8, 16, 3).expect("build atlas");
        atlas.resolve(b'A' as u32);
        atlas.resolve(b'B' as u32);
        atlas.resolve(b'C' as u32);
        assert_eq!(atlas.len(), 3);
        // Insert one more — 'A' (oldest) must be evicted.
        atlas.resolve(b'D' as u32);
        assert_eq!(atlas.len(), 3);
        assert!(atlas.find_slot(b'A' as u32).is_none(), "A must be evicted");
        assert!(atlas.find_slot(b'D' as u32).is_some(), "D must be cached");
    }

    #[test]
    fn access_promotes_to_front() {
        let Some(bytes) = load_bytes() else {
            return;
        };
        let mut atlas = Atlas::new(bytes, 8, 16, 3).expect("build atlas");
        atlas.resolve(b'A' as u32);
        atlas.resolve(b'B' as u32);
        atlas.resolve(b'C' as u32);
        // Touch 'A' so it becomes MRU; 'B' is now LRU.
        atlas.resolve(b'A' as u32);
        assert_eq!(
            atlas.lru_order(),
            vec![b'A' as u32, b'C' as u32, b'B' as u32]
        );
        // Inserting 'D' evicts 'B'.
        atlas.resolve(b'D' as u32);
        assert!(atlas.find_slot(b'B' as u32).is_none());
        assert!(atlas.find_slot(b'A' as u32).is_some());
    }

    #[test]
    fn missing_codepoint_returns_fallback() {
        let Some(bytes) = load_bytes() else {
            return;
        };
        let mut atlas = Atlas::new(bytes, 8, 16, 4).expect("build atlas");
        // A codepoint outside any normal font's cmap.
        let bm = atlas.resolve(0xFFFFE);
        // Fallback dot has at least one set pixel and is not blank.
        assert!(!bm.is_blank(), "fallback must be visible");
        // Not stored in the cache — fallback is shared.
        assert_eq!(atlas.len(), 0);
    }

    #[test]
    fn control_codepoint_renders_blank() {
        let Some(bytes) = load_bytes() else {
            return;
        };
        let mut atlas = Atlas::new(bytes, 8, 16, 4).expect("build atlas");
        for cp in [0x00u32, 0x07, 0x1F, 0x7F, 0x80, 0x9F, 0xA0] {
            let bm = atlas.resolve(cp);
            assert!(bm.is_blank(), "codepoint 0x{cp:X} must render blank");
        }
    }

    #[test]
    fn adversarial_stream_stays_bounded() {
        let Some(bytes) = load_bytes() else {
            return;
        };
        let mut atlas = Atlas::new(bytes, 8, 16, 64).expect("build atlas");
        for cp in 0x20u32..(0x20 + 512) {
            atlas.resolve(cp);
        }
        assert!(atlas.len() <= 64, "atlas must respect capacity");
    }
}
