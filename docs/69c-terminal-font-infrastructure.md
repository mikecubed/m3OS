# TTF Font Loader and Nerd Font Asset Embedding

**Aligned Roadmap Phase:** Phase 69c
**Status:** Complete
**Source Ref:** phase-69c
**Supersedes Legacy Doc:** new

## Overview

Phase 69b shipped a static-table glyph resolver in
`kernel-core::session::glyph_tables`: ASCII (`U+0020..=U+007F`),
Latin-1 supplement (`U+0080..=U+00FF`), Unicode box-drawing
(`U+2500..=U+257F`), and a centred-dot fallback for anything else.
That is enough for `mc`'s blue panel art, `htop`'s graph bars, and
accented Western European text — but every modern developer TUI
(lazygit, lf, fzf, starship, btop) emits Nerd Font private-use-area
icons in the range `U+E000..=U+F8FF` and `U+F0000..=U+FFFFD`, and a
static bitmap table for that range is on the order of megabytes.

Phase 69c replaces the static path with a runtime TTF rasterizer
plus a bounded LRU atlas. After this phase a single ~2 MB Nerd Font
file (`/usr/share/fonts/m3os/term.ttf`) covers every developer-TUI
glyph the user will ever encounter; the static tables remain as the
load-failure fallback so a corrupt or missing font does not brick
`term`.

The phase splits cleanly into three pieces:

1. **Parse** — `kernel-core::font::parser` wraps the vendored
   `ttf-parser` crate as a thin `Font` façade exposing `open`,
   `glyph_index`, and `glyph_outline`. The outline is returned as a
   `Vec<OutlineSegment>` in em-units along with the font's tight
   bounding box.
2. **Rasterize** — `kernel-core::font::raster` walks the outline,
   flattens quadratic and cubic Bezier curves into 12-segment
   polylines, and runs a non-zero-winding scanline fill with
   pixel-centre coverage to produce a 1-bit `RasterBitmap`. Glyphs
   centre horizontally inside the cell and align to a baseline
   computed from the font's ascender/descender band.
3. **Cache** — `kernel-core::font::atlas` is a bounded LRU keyed by
   codepoint with a default capacity of 1024. Cache hits return the
   stored bitmap; misses rasterize on demand and evict the
   least-recently-used entry when full. Codepoints the font does not
   cover return a shared centred-dot fallback bitmap so adversarial
   streams of uncovered codepoints do not consume slots.

`term` opens the staged font at boot, builds the atlas, and
upgrades the renderer's `GlyphSource` from `Static` to `Atlas`. On
a file-missing, parse-error, or oversized-file failure the
renderer keeps using Phase 69b's static path; the boot log records
the outcome. The current `tui-smoke fonts missing-font` gate
exercises only the in-process static-resolver path; the
complementary stripped-disk boot variant that pins the
`term: font load failed; using static fallback` boot-log line is
tracked as a deferred follow-up (see the Track F section below).

## What This Doc Covers

This doc is the canonical learning companion to the Phase 69c
roadmap design at
[docs/roadmap/69c-terminal-font-infrastructure.md](./roadmap/69c-terminal-font-infrastructure.md).
The roadmap doc is the milestone shape and acceptance contract; this
file walks through how the pieces fit together and why each lives
where it does.

It deliberately does **not** restate the Phase 69b bitmap-glyph
infrastructure (that's covered in
[docs/69b-terminal-utf8-and-glyphs.md](./69b-terminal-utf8-and-glyphs.md)),
nor the Phase 69 terminal contract foundations.

## Core Implementation

### Track A — TTF parser (`kernel-core/src/font/parser.rs`)

The parser is a thin `Font<'a>` wrapper around `ttf_parser::Face<'a>`.
The public surface is intentionally small:

- `Font::open(bytes) -> Result<Font, FontError>` — validates the
  magic + required tables. Returns `Malformed` on a parse error or
  `MissingMetrics` when `units_per_em == 0`.
- `Font::glyph_index(codepoint) -> Option<GlyphId>` — walks the
  font's `cmap` to find the glyph slot for a codepoint, or
  `None` for codepoints the font does not cover.
- `Font::glyph_outline(glyph) -> Result<Outline, FontError>` —
  drives `ttf-parser`'s `OutlineBuilder` callbacks and collects the
  segments into an owned `Outline` (segments + bounding box in
  em-units). Empty glyphs (`.notdef`, space, control characters)
  return an empty outline rather than an error so the caller does
  not need to branch.

The `OutlineSegment` enum mirrors `ttf-parser`'s callback shape:
`MoveTo`, `LineTo`, `QuadTo` (quadratic Bezier), `CurveTo` (cubic
Bezier), `Close`. Coordinates are signed `f32`; the rasterizer
maps em-space onto cell pixels by linear interpolation against the
font's units-per-em and ascender/descender.

The decision to vendor `ttf-parser` rather than hand-roll a `cmap`
+ `glyf` + `loca` subset is recorded in the roadmap doc's
"Decision (Track A.1)" section. The short version: a Nerd Font's
`cmap` uses format 4 + format 12 + format 14 (variation selectors),
and the parser side alone is ~5 KLOC if hand-rolled correctly.
`ttf-parser` is `no_std`-friendly, zero-allocation, MIT/Apache,
and covers every format we'll ever encounter.

### Track B — Rasterizer (`kernel-core/src/font/raster.rs`)

The rasterizer turns an outline into a 1-bit `RasterBitmap`. The
algorithm:

1. **Compute pixel scale.** `scale_y = (cell_h - 2) / em_height`
   reserves one pixel of top and bottom padding so caps and
   descenders don't touch the cell edges. `scale_x = scale_y`
   because Nerd Font is monospace.
2. **Centre horizontally.** The glyph bbox in pixel space is
   `(x_min * scale, x_max * scale)`; the horizontal translation
   places the glyph's midpoint at `cell_w / 2`.
3. **Flatten Bezier curves.** Each `QuadTo` / `CurveTo` becomes 12
   polyline points evaluated at evenly-spaced parameter values.
   Twelve segments is overkill for an 8 × 16 cell but it costs
   nothing at compile-time and removes the curve fidelity dial
   from the design entirely.
4. **Map em-space to pixel-space.** TTF y-axis is up-positive; the
   pixel grid is down-positive. Flipping by subtracting from the
   baseline lands the glyph upright.
5. **Build an edge table.** Horizontal edges (`dy == 0`) are
   skipped. Each edge carries `(y_min, y_max, x_at_ymin, slope,
   winding)` where winding is `+1` for downward-going edges and
   `-1` for upward-going. The winding direction encodes contour
   orientation for the non-zero fill rule.
6. **Scan-line fill.** For each pixel row, the scanline samples at
   `y + 0.5` (centre coverage). Crossings get sorted by x and
   walked left-to-right, accumulating winding. A run of pixels is
   filled when `prev_winding != 0` — i.e., we just crossed back to
   the outside of a non-zero region.
7. **Centre coverage.** A pixel is inked when its centre
   (`px + 0.5`) sits inside the span; this is the FreeType / Skia
   convention. The boundary-based rule (`lo = ceil(span_start),
   hi = floor(span_end)`) silently drops sub-pixel vertical bars,
   which exactly an 8 × 16 'H' rendering at body-text size
   becomes.

The rasterizer hand-rolls `f32::ceil` and `f32::abs` so the crate
builds under `x86_64-unknown-none` without pulling `libm` into
`kernel-core`'s direct dependency surface.

### Track C — Atlas (`kernel-core/src/font/atlas.rs`)

The atlas is a bounded LRU keyed by codepoint. It owns the font
bytes (`Vec<u8>`) and the cell metrics; on construction it parses
the font once to extract `units_per_em` / `ascender` / `descender`
and stores them as `CellMetrics`. The actual `ttf_parser::Face`
borrows from the byte buffer, so re-parsing per resolve is the
simplest way to avoid a self-referential `Atlas<'a>` — the parse
cost is negligible compared with rasterization.

LRU policy: a doubly-linked list of slot indices stored as
`prev` / `next` fields on each `Slot`. Each cache hit moves the
entry to the head; each miss inserts at the head and evicts the
tail when the cache is full. The codepoint-to-slot map is a small
linear scan because the hot set fits in CPU cache lines and a
hash map would pull `hashbrown` into `kernel-core`.

Special-cased codepoints:

- **Blank codepoints** (`U+0000..=U+001F`, `U+007F`,
  `U+0080..=U+009F`, `U+00A0` NBSP) return a shared blank bitmap
  without cache pollution. Control characters never paint pixels.
- **Uncovered codepoints** (font `cmap` returns `None`) return the
  shared centred-dot fallback bitmap. This is the same shape as
  Phase 69b's `FALLBACK_DOT_GLYPH`, so the rendered cell looks
  identical between static and atlas paths.

Capacity defaults to 1024 entries. An 8 × 16 bitmap is 16 bytes;
1024 × 16 = 16 KiB worst-case for `term`'s atlas — well below the
ext2 disk's per-process heap budget.

### Track D — Asset staging (`xtask/src/main.rs`)

`cargo xtask fetch-fonts` downloads JetBrainsMono Nerd Font Mono
Regular from the upstream `ryanoasis/nerd-fonts` release v3.2.1
into `xtask/assets/fonts/term.ttf` and verifies the SHA-256
against `xtask/assets/fonts/term.ttf.sha256` (committed). The
font is gitignored — only the checksum lives in the repo. The
downloader is idempotent: if the on-disk file matches the
expected hash, it skips the download.

`populate_ext2_files` stages the asset on the ext2 data disk at
`/usr/share/fonts/m3os/term.ttf` with `mode 0644 uid 0 gid 0`. A
missing source file aborts the image build with the actionable
"run `cargo xtask fetch-fonts` first" hint so a developer cannot
silently ship without the font.

### Track E — `term` wiring (`userspace/term`)

`Renderer` carries a `GlyphSource` enum (`Static` / `Atlas`). The
default is `Static` so host tests and early-boot code paths work
unchanged. `Renderer::set_atlas(atlas)` upgrades a live renderer
to the atlas path; `term::main` calls this after `build_atlas`
returns successfully.

`Renderer::compose()` resolves each `PutGlyph` op through
`GlyphSource` before invoking `FramebufferOwner::put_glyph`. The
trait method takes both the codepoint (for diagnostics) and a
borrowed `GlyphView<'_>` — the common shape both static `Glyph`
and atlas `RasterBitmap` flatten to via `as_view()`. The
framebuffer owner no longer branches on resolution policy; it just
blits the pre-resolved bitmap.

`build_atlas` reads `/usr/share/fonts/m3os/term.ttf` via
`syscall_lib::open` + `read` (chunked in 4 KiB blocks) into an
`alloc::Vec<u8>`, constructs an `Atlas` with the default 1024
capacity, pre-warms the printable-ASCII range so the boot log can
report a glyph count, and calls `Renderer::set_atlas`. On any
failure (file missing, parse error, atlas construction error) the
function logs `term: font load failed; using static fallback` and
returns without changing the renderer's `GlyphSource`.

### Track F — `tui-smoke fonts` (`userspace/tui-smoke`)

Five subcommand leaves, each emitting
`TUI_SMOKE:fonts-<leaf>:ok` on success:

- **`startup`** — opens the staged font, builds a fresh atlas,
  pre-warms `U+0020..=U+007F`, asserts ≥ 64 of those produce
  non-blank pixels and the atlas length is ≥ 64.
- **`branch-icon`** — confirms the font's cmap covers `U+E0A0`
  (`Font::glyph_index` is `Some`), resolves the codepoint, and
  asserts the bitmap is non-blank *and* has more ink than the
  4-pixel fallback dot — so a stripped non-Nerd-Font asset cannot
  silently pass. Also feeds the UTF-8 bytes of `U+E0A0` through a
  `Screen` instance and asserts `Cell::codepoint == 0xE0A0`.
- **`emoji`** — resolves `U+1F600` and asserts the bitmap is not
  blank. Either real ink (font covered it) or the centred-dot
  fallback shape (font did not) is acceptable; the assertion
  rejects a silent regression that returns a blank cell.
- **`adversarial`** — writes 2 × CAP distinct codepoints into a
  CAP-sized atlas and asserts `atlas.len() == CAP`, the
  first-inserted codepoint has been evicted, and the
  most-recently-inserted one is still cached.
- **`missing-font`** — asserts the Phase 69b static-table resolver
  still produces ink for ASCII / Latin-1 / box-drawing. **Deferred**:
  the complementary "boot with the font omitted, watch for the
  fallback log line" half of this gate is not yet wired — the
  current xtask harness always stages the font on the data disk.
  A dedicated stripped-disk boot is tracked as a follow-up.

## Key Files

| File | Purpose |
|---|---|
| `kernel-core/Cargo.toml` | New dependency: `ttf-parser` v0.25 with `default-features = false` + `no-std-float`. |
| `kernel-core/src/font/mod.rs` | New — module entry; re-exports the public surface and the unified `GlyphView<'a>` borrow used by both static and atlas paths. |
| `kernel-core/src/font/parser.rs` | New — `Font::open` / `glyph_index` / `glyph_outline`; the `OutlineSegment` enum mirrors `ttf-parser`'s callback shape. |
| `kernel-core/src/font/raster.rs` | New — `Rasterizer::rasterize_glyph`; non-zero-winding scanline fill, hand-rolled `f32` math for `no_std`. |
| `kernel-core/src/font/atlas.rs` | New — bounded LRU keyed by codepoint; control-codepoint shortcut and shared fallback bitmap. |
| `xtask/src/main.rs` | Extended — `cmd_fetch_fonts` downloads + checksum-verifies the asset; `populate_ext2_files` stages it on the ext2 data disk; the `tui-smoke` step list grows the five new `fonts-*` leaves. |
| `xtask/assets/fonts/term.ttf.sha256` | New — committed SHA-256 of JetBrainsMono Nerd Font Mono Regular v3.2.1. |
| `xtask/assets/fonts/.gitignore` | New — excludes the binary asset from the repository. |
| `userspace/term/src/render.rs` | Extended — `GlyphSource` enum, `Renderer::set_atlas`, `Renderer::glyph_pixels`; `compose()` resolves before calling `FramebufferOwner::put_glyph`. |
| `userspace/term/src/display.rs` | Extended — `FramebufferOwner::put_glyph` now takes `&GlyphView`; `DisplayClient::put_glyph` blits the pre-resolved bitmap through `blit_glyph_view`. |
| `userspace/term/src/main.rs` | Extended — `build_atlas` opens the font and upgrades the renderer; `format_atlas_msg` builds the boot-log line `term: atlas loaded N glyphs`. |
| `userspace/tui-smoke/src/main.rs` | Extended — five new `fonts <leaf>` subcommand leaves driving the in-process atlas. |
| `docs/appendix/term-escape-sequences.md` | Extended — new "Font infrastructure (Phase 69c)" section documenting the dispatch and asset path. |
| `docs/roadmap/69b-terminal-utf8-and-glyphs.md` | Extended — the "TTF/OTF font loader + Nerd Font asset embedding" deferral is marked `(closed in Phase 69c)`. |

## How This Phase Differs From Later Font Work

Phase 69c deliberately ships infrastructure, not coverage. The
following are explicit non-goals:

- **Multiple font sizes / dynamic resize.** The atlas is built at
  one cell size (8 × 16). A future phase that wants larger cells
  would need a per-size atlas or a re-rasterize-on-resize policy.
- **Per-region font fallback.** Phase 69c uses one font. A future
  phase that wants CJK + Latin + emoji composition would need
  font-fallback logic in the atlas's `resolve` path.
- **OpenType features.** No ligatures, no kerning, no contextual
  alternates. One codepoint produces one glyph.
- **Sub-pixel anti-aliased rendering.** Coverage is 1-bit; the
  framebuffer is BGRA8888 but glyph pixels are fg or bg with no
  blend. AA lands later if/when sub-pixel rendering arrives.
- **Variable fonts.** No axis support.
- **Hot reload.** The font is read once at boot. Editing the file
  while `term` is running has no effect.
- **Configurable font path.** The path is hard-coded to
  `/usr/share/fonts/m3os/term.ttf`. A future phase that wants
  user-overridable fonts would need a config-file plumb.

## Closure of Related Phases

- **Phase 57 — Audio and Local Session.** `term`'s graphical
  terminal emulator is the integration point Phase 57 reserved
  for "real font infrastructure". Phase 69c is the closure: the
  `GlyphSource::Atlas` path replaces the static-table-only model
  Phase 57 booted with.
- **Phase 69 — Terminal Contract Foundations.** The `?1049h`
  alternate-screen buffer, the SGR colour resolver, and the
  Bracketed-Paste / Mouse / Resize plumbing all keep working
  unchanged under the atlas path — the renderer's `compose()`
  resolves codepoints before paint, so the screen state machine
  is decoupled from glyph storage.
- **Phase 69a — Terminal Termios.** No interaction. The atlas
  ships glyphs; termios ships line-discipline behaviour. They
  share `term` as their integration host but the code paths are
  disjoint.
- **Phase 69b — UTF-8 + Bitmap Glyphs.** The `resolve_glyph`
  accessor Phase 69b built as the unified static-path entry is
  preserved as the load-failure fallback. The atlas takes over
  the hot path; the static tables stay around as the safety net.
  The Phase 69b deferral "TTF/OTF font loader + Nerd Font asset
  embedding" is now closed.

## Related Roadmap Docs

- [Phase 69c roadmap doc](./roadmap/69c-terminal-font-infrastructure.md)
- [Phase 69c task doc](./roadmap/tasks/69c-terminal-font-infrastructure-tasks.md)

## Deferred or Later-Phase Topics

- **Anti-aliased rendering.** The 1-bit coverage works at 8 × 16
  but coarse rasterization at larger cell sizes will benefit from
  4-bit or 8-bit coverage with sub-pixel sampling. The rasterizer
  is structured to accept a different coverage shape — the fill
  loop just needs a different write path.
- **Composite glyphs and OpenType layout features.** `ttf-parser`
  surfaces the data; Phase 69c does not consume it. A future
  phase that wants ligatures (e.g. `==`, `!=`, `->` in
  JetBrainsMono) would extend `Renderer::glyph_pixels` to take a
  small look-ahead window.
- **Multi-size atlas.** A pixel-doubling renderer for the
  framebuffer console could share the same atlas if the atlas
  carries multiple bitmaps per codepoint indexed by cell size.
- **Per-region fallback.** A future phase with both a Western
  monospace font and a CJK font would need to consult a fallback
  chain in the atlas's miss path, plus per-region cell sizing.
- **Configurable font path / user override.** Wiring this through
  the session config requires a service-config schema change; the
  hard-coded path in `term::build_atlas` is the minimal seam.
