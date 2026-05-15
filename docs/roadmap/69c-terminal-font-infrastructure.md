# Phase 69c - TTF Font Loader and Nerd Font Asset Embedding

**Status:** Complete
**Source Ref:** phase-69c
**Depends on:** Phase 57 (Audio and Local Session) ✅, Phase 69 (Terminal Contract Foundations), Phase 69a (Termios Raw Mode), Phase 69b (UTF-8 + Bitmap Glyphs)
**Builds on:** Replaces the static bitmap glyph tables in `kernel-core::fb` with a TTF/OTF rasterizer + glyph atlas cache, and embeds a Nerd Font asset on the ext2 data disk so modern developer TUIs (lazygit, lf, fzf, starship glyphs, btop's gauges) render at full fidelity. Phase 69b's `resolve_glyph` accessor is the seam — its body changes from a static-table dispatch to an atlas lookup with a TTF-rasterized fallback.
**Primary Components:** kernel-core/src/font (new module), userspace/term/src/render.rs, xtask (font asset staging), `ports/lang/font` (optional vendor crate wrapping `ttf-parser` + `ab_glyph`)

## Milestone Goal

Phase 69c gives `term` a real font path. After this phase:

- a Nerd Font (e.g. `JetBrainsMono Nerd Font`) is embedded on the data disk at a fixed path;
- `term` opens the font at boot, builds an in-memory glyph atlas, and resolves any Unicode codepoint (including Nerd Font private-use-area icons U+E000–U+F8FF and U+F0000–U+FFFFD) to a rendered bitmap;
- the atlas is bounded (LRU eviction) so an adversarial codepoint stream cannot exhaust memory;
- the static bitmap tables from Phase 69b remain as a startup fallback (used until the atlas is hot or if font loading fails).

This is the last piece of "glyph contract" — after 69c, anything a TUI emits, `term` renders correctly.

## Why This Phase Exists

Modern developer TUIs ship with Nerd Font glyphs baked into their themes: lazygit's diff markers, lf's file-type icons, fzf's prompt arrow, starship's branch/git glyphs. These all live in the Nerd Font private-use area (U+E000+). Phase 69b's static-table approach does not scale to that range — Nerd Font alone is ~10,000 glyphs, and a 10K × 256-byte static table is 2.5 MB. A TTF rasterizer is both smaller (the font itself is ~200 KB) and correct for any codepoint the user might want.

Phase 69c is deliberately scoped to one font at one size. Configurable font size and per-region font fallback are deferred — getting one font working end-to-end is the bar.

## Learning Goals

- Understand TTF/OTF file format basics: tables (`cmap`, `glyf`, `loca`, `head`), the codepoint-to-glyph-index mapping, and the relationship between em-units and pixel size.
- Learn how a glyph rasterizer turns Bezier curves into a coverage mask, and how that mask is composited onto a cell-grid framebuffer.
- See how a bounded LRU atlas cache keeps a rasterizer cheap: rasterize-once, blit-many.
- Understand why monospace terminal fonts are required (each cell is a fixed width).
- See how Nerd Font's private-use area maps icon names to codepoints, and how that maps to the editor/shell config that emits those codepoints.

## Feature Scope

### TTF/OTF parser (Track A)

A `kernel-core/src/font/parser.rs` module wraps `ttf-parser` (a `no_std`-friendly upstream crate) — or, if `ttf-parser` is unsuitable for the m3OS toolchain, a hand-rolled subset covering `cmap` format 4 + 12 + `glyf` + `loca` + `head` + `maxp`. The parser is host-testable; given a font and a codepoint it returns the glyph index and the outline contours.

### Glyph rasterizer (Track B)

A `kernel-core/src/font/raster.rs` module turns a glyph outline into a coverage bitmap at a fixed cell size (the bitmap shape from Phase 69b is preserved so the renderer is unchanged). Uses a simple scanline rasterizer (no anti-aliasing in v1 — m3OS' framebuffer is 1-bit-per-pixel for glyphs; AA lands later if/when sub-pixel rendering arrives). Host-testable; rasterize a known glyph and assert the bitmap.

### Glyph atlas cache (Track C)

A bounded LRU cache keyed by codepoint. `Atlas::resolve(codepoint)` hits cache or rasterizes on miss; capacity defaults to 1024 glyphs. Eviction is LRU. The cache lives in `term`'s process memory; there is one atlas per `term` instance.

### Font asset staging (Track D)

The Nerd Font binary (TTF) is staged on the ext2 data disk at `/usr/share/fonts/m3os/term.ttf`. The xtask image build downloads the font once into `xtask/assets/fonts/` (with a checksum) and copies it during `populate_ext2_files`. Font choice: `JetBrainsMono Nerd Font Mono Regular` (Apache 2.0; ~200 KB; widely used in dev TUIs).

### Atlas startup + fallback (Track E)

`term` opens `/usr/share/fonts/m3os/term.ttf` on boot, builds the atlas, and replaces `kernel-core::fb::resolve_glyph` with `Atlas::resolve` for the runtime path. If font loading fails (file missing, parse error, OOM), `term` falls back to Phase 69b's static tables and logs a single warning — the terminal remains usable for ASCII + Latin-1 + box-drawing.

### Validation (Track F)

A new `tui-smoke fonts` subcommand:

- assert the atlas is populated after boot;
- rasterize and write a Nerd Font icon (U+E0A0 ` `, the standard branch icon), verify the rendered cell contains non-blank pixels in the expected glyph shape;
- write an emoji codepoint (U+1F600 `😀`); verify either it renders (if the font includes it) or falls back to the dot glyph without crashing;
- exhaust the atlas (write 2048 distinct codepoints), verify no OOM and the LRU evicts oldest entries.

## Important Components and How They Work

### `kernel-core/src/font/mod.rs` (new)

Re-exports `parser`, `raster`, `atlas`. The public API is:

```rust
pub fn open(font_bytes: &[u8]) -> Result<Font, FontError>;
impl Font {
    pub fn glyph_index(&self, codepoint: u32) -> Option<GlyphId>;
    pub fn rasterize(&self, glyph: GlyphId, cell_w: u16, cell_h: u16) -> RasterBitmap;
}
```

### `kernel-core/src/font/atlas.rs` (new)

Bounded LRU atlas keyed by codepoint. Stores rasterized bitmaps + a tiny LRU linked list. `Atlas::resolve(codepoint) -> &RasterBitmap` is the hot path the renderer calls.

### `userspace/term/src/render.rs` — atlas-backed glyph path

After 69c, `Renderer::glyph_pixels(codepoint)` calls `Atlas::resolve` instead of `kernel-core::fb::resolve_glyph`. The static tables remain as the startup-fallback path inside the atlas constructor.

### `userspace/term/src/main.rs` — font open at boot

After `Renderer::new(display)`, but before the first compose, `term` reads `/usr/share/fonts/m3os/term.ttf` and constructs the atlas. Failure logs and continues with the static fallback.

### `xtask/src/main.rs` — font staging

`populate_ext2_files` copies `xtask/assets/fonts/term.ttf` to `/usr/share/fonts/m3os/term.ttf` on the data disk. A make-target-style `cargo xtask fetch-fonts` downloads + verifies the asset into `xtask/assets/fonts/` (gated by a checksum) so the asset is not checked in.

## How This Builds on Earlier Phases

- Replaces the static glyph tables from Phase 69b with a TTF-rasterized atlas; preserves them as the startup-fallback path.
- Builds on Phase 69b's `resolve_glyph` accessor (extending it to be atlas-backed).
- Builds on the Phase 45 ports system if `ttf-parser` and the rasterizer are vendored through the ports tree; otherwise they sit as `no_std` Cargo dependencies of `kernel-core` (implementer's call — Track A.1 covers both).

## Implementation Outline

### Decision (Track A.1): vendor `ttf-parser`

Vendoring `ttf-parser` v0.25 with `default-features = false` (no_std, zero-allocation, MIT OR Apache-2.0) is the chosen path. `ttf-parser` covers the full set of `cmap` formats m3OS could encounter from a Nerd Font asset (formats 0 / 4 / 12 / 13 / 14), handles compound `glyf` glyphs, and exposes the outline-builder trait the rasterizer needs. Hand-rolling a subset would deliver less coverage at higher maintenance cost, while still requiring the same outline-walker machinery. The rasterizer (`kernel-core/src/font/raster.rs`) and the LRU atlas (`kernel-core/src/font/atlas.rs`) are hand-rolled because their behaviour is small and m3OS-specific (1-bit coverage, centred-in-cell baseline alignment, capacity-bounded eviction).

1. Decide between vendoring `ttf-parser` + `ab_glyph` vs hand-rolling the subset; document the call. **Decided: vendor `ttf-parser`. See "Decision (Track A.1)" above.**
2. Build `kernel-core/src/font/parser.rs` (host tests on a public-domain TTF).
3. Build `kernel-core/src/font/raster.rs` (host tests: rasterize known glyphs, assert bitmap).
4. Build `kernel-core/src/font/atlas.rs` (host tests: cache hit, miss → rasterize, LRU eviction).
5. Add the font-fetch step to xtask (`cargo xtask fetch-fonts`); pick + commit the checksum.
6. Stage the font on the data disk via `populate_ext2_files`.
7. Wire `term` to open the font at boot and switch `Renderer` to the atlas path; preserve the static-table fallback.
8. Extend `tui-smoke` with the `fonts` subcommand.
9. Cross-ref Phase 69b doc; extend the appendix with a "Font infrastructure" section.
10. Create the aligned legacy learning doc at `docs/69c-terminal-font-infrastructure.md`.
11. Kernel patch bump to 0.69.3.

## Acceptance Criteria

- The Nerd Font asset is present at `/usr/share/fonts/m3os/term.ttf` on a fresh boot.
- `term` builds the atlas without panic; the boot log records the atlas size at startup.
- `tui-smoke fonts` writes a Nerd Font branch icon and asserts the rendered cell contains non-blank pixels in the expected outline.
- Writing 2048 distinct codepoints does not OOM `term`; the atlas evicts oldest entries.
- If the font file is deleted from the data disk, `term` still boots and renders ASCII/Latin-1/box-drawing using the static fallback (with a single warning log line).
- `cargo xtask tui-smoke` continues to pass; `cargo xtask check` and `cargo xtask test` pass.

## Companion Task List

- [Phase 69c Task List](./tasks/69c-terminal-font-infrastructure-tasks.md)

## How Real OS Implementations Differ

- Linux uses `freetype` + `fontconfig` for runtime font discovery and rasterization; m3OS uses a single hard-coded font path and a static rasterizer.
- Wayland compositors expose a font-server protocol or rely on per-client font handling; m3OS keeps font handling fully inside `term`.
- macOS' Core Text supports complex shaping (ligatures, kerning, OpenType features); m3OS performs none of these — one codepoint produces one glyph.

## Deferred Until Later

- Multiple font sizes / dynamic resize.
- Per-region font fallback (e.g. CJK font + Latin font + emoji font composition).
- OpenType feature support (ligatures, kerning, contextual alternates).
- Sub-pixel anti-aliased rendering.
- Variable fonts.
- Font hot-reload.
- Configurable font path / user-overridable font.
