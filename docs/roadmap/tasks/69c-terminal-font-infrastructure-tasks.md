# Phase 69c — TTF Font Loader and Nerd Font Asset Embedding: Task List

**Status:** Planned
**Source Ref:** phase-69c
**Depends on:** Phase 57 (Audio and Local Session) ✅, Phase 69 (Terminal Contract Foundations), Phase 69a (Termios Raw Mode), Phase 69b (UTF-8 + Bitmap Glyphs)
**Goal:** Land TTF font parsing, glyph rasterization, a bounded LRU atlas, and a Nerd Font asset on the data disk so `term` resolves arbitrary Unicode codepoints — including Nerd Font private-use-area icons — at full fidelity. Phase 69b's static bitmap tables remain as the startup-fallback path.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | TTF/OTF parser in `kernel-core/src/font/parser.rs` (vendor vs hand-roll decision documented) | None | Planned |
| B | Glyph rasterizer in `kernel-core/src/font/raster.rs` | A | Planned |
| C | Bounded LRU atlas in `kernel-core/src/font/atlas.rs` | A, B | Planned |
| D | Font asset staging: `cargo xtask fetch-fonts`, ext2 copy in `populate_ext2_files` | None | Planned |
| E | `term` boot wiring + atlas-backed `Renderer::glyph_pixels`; static-table fallback preserved | C, D | Planned |
| F | Validation: `tui-smoke fonts` subcommand | E | Planned |
| G | Documentation: Phase 69b cross-ref; appendix update; kernel patch bump to 0.69.3 | F | Planned |

---

## Track A — TTF/OTF Parser

### A.1 — Decide vendor vs hand-roll

**File:** `docs/roadmap/69c-terminal-font-infrastructure.md` (or a short ADR)
**Symbol:** N/A
**Why it matters:** The TTF spec is large; deciding upfront whether to pull `ttf-parser` (no_std-friendly, MIT) vs writing a subset shapes the rest of Track A.

**Acceptance:**
- [ ] A 1-paragraph decision recorded in the design doc or an ADR file.
- [ ] If vendoring `ttf-parser`: the crate is added to `kernel-core`'s dependency list with `default-features = false` and the version pinned.
- [ ] If hand-rolling: scope is `cmap` format 4 + 12, `glyf`, `loca`, `head`, `maxp` — explicitly documented as the subset.

### A.2 — Font parser implementation

**File:** `kernel-core/src/font/parser.rs` (new)
**Symbol:** `Font`, `Font::open`, `Font::glyph_index`, `Font::glyph_outline`
**Why it matters:** Without parsing, no rasterizer.

**Acceptance:**
- [ ] `Font::open(bytes: &[u8]) -> Result<Font, FontError>` validates the magic + required tables.
- [ ] `glyph_index(codepoint: u32) -> Option<GlyphId>` returns the glyph for the codepoint (or None if absent).
- [ ] `glyph_outline(glyph: GlyphId) -> Outline` returns the Bezier curve set in em-units.
- [ ] Host tests use a public-domain TTF (committed to `kernel-core/tests/fonts/` or fetched on demand): assert known codepoint → glyph mappings; assert outline contour count for a representative glyph.

---

## Track B — Glyph Rasterizer

### B.1 — Scanline rasterizer

**File:** `kernel-core/src/font/raster.rs` (new)
**Symbol:** `Rasterizer::rasterize_glyph`
**Why it matters:** Outlines are useless without pixels.

**Acceptance:**
- [ ] `rasterize_glyph(outline: &Outline, cell_w: u16, cell_h: u16, em_size: u16) -> RasterBitmap` produces a coverage bitmap matching the Phase 69b shape.
- [ ] Rasterizer uses scanline + edge-table fill (no SSE / AVX; m3OS disables SIMD).
- [ ] Glyphs are centred horizontally and baseline-aligned in the cell.
- [ ] Coverage is 1-bit (no AA in v1) — pixel is set if its centre is inside the outline.
- [ ] Host tests: rasterize the letter `H` from the test font, assert the bitmap has exactly two vertical bars + one horizontal crossbar; rasterize `o`, assert a closed loop.

---

## Track C — Atlas Cache

### C.1 — Bounded LRU atlas

**File:** `kernel-core/src/font/atlas.rs` (new)
**Symbol:** `Atlas`, `Atlas::new`, `Atlas::resolve`
**Why it matters:** Without bounded eviction, an adversarial codepoint stream OOMs `term`.

**Acceptance:**
- [ ] `Atlas::new(font: Font, capacity: usize)` constructs with default capacity 1024.
- [ ] `resolve(codepoint: u32) -> &RasterBitmap` hits the cache or rasterizes + inserts.
- [ ] Cache uses LRU eviction (host test: fill to capacity + 1, oldest entry is evicted).
- [ ] `Atlas` is `!Send` and lives inside `term`'s process; no cross-process atlas sharing in 69c.
- [ ] Host tests cover: cache miss → hit transition; LRU eviction order; codepoint with no glyph in the font → fallback dot.

---

## Track D — Font Asset Staging

### D.1 — `cargo xtask fetch-fonts`

**File:** `xtask/src/main.rs`
**Symbol:** `fetch_fonts` subcommand
**Why it matters:** TTF files are binary blobs; committing them inflates the repo, but they need to be reproducible.

**Acceptance:**
- [ ] `cargo xtask fetch-fonts` downloads `JetBrainsMono Nerd Font Mono Regular.ttf` from a pinned URL (Nerd Fonts GitHub release) into `xtask/assets/fonts/term.ttf`.
- [ ] SHA-256 is verified against a checksum committed in `xtask/assets/fonts/term.ttf.sha256`.
- [ ] Mismatch aborts with a clear error.
- [ ] Subsequent runs skip the download if the file matches the checksum.

### D.2 — Stage the font on the ext2 data disk

**File:** `xtask/src/main.rs`
**Symbol:** `populate_ext2_files`
**Why it matters:** The font must be readable from `term`'s userspace path at runtime.

**Acceptance:**
- [ ] `populate_ext2_files` creates `/usr/share/fonts/m3os/` and copies `xtask/assets/fonts/term.ttf` to `/usr/share/fonts/m3os/term.ttf`.
- [ ] If the source file is missing, `xtask image` aborts with `error: run "cargo xtask fetch-fonts" first`.
- [ ] `cargo xtask check` continues to pass.

---

## Track E — Term Wiring

### E.1 — Atlas construction at boot

**File:** `userspace/term/src/main.rs`
**Symbol:** `build_atlas`
**Why it matters:** This is where the static-table → atlas switch flips for the runtime path.

**Acceptance:**
- [ ] After `Renderer::new(display)`, `term` opens `/usr/share/fonts/m3os/term.ttf` via `syscall_lib::open` + `read`.
- [ ] On success: constructs an `Atlas` with capacity 1024 and stashes it on the renderer.
- [ ] On any failure (file missing, parse error, OOM): logs `term: font load failed; using static fallback` and proceeds with Phase 69b's static-table path.
- [ ] Boot log includes `term: atlas loaded N glyphs` on success.

### E.2 — Renderer atlas-backed glyph path

**File:** `userspace/term/src/render.rs`
**Symbol:** `Renderer::glyph_pixels`, `GlyphSource`
**Why it matters:** This is the seam Phase 69b's `resolve_glyph` accessor was built for.

**Acceptance:**
- [ ] `Renderer` carries a `GlyphSource` enum: `Static` (Phase 69b tables) or `Atlas(Atlas)`.
- [ ] `glyph_pixels(codepoint)` dispatches: `Atlas` → `atlas.resolve(codepoint)`; `Static` → `kernel_core::fb::resolve_glyph(codepoint)`.
- [ ] No allocation per glyph blit (both paths return `&RasterBitmap`).
- [ ] When the atlas misses (codepoint not in the font), the resolver falls back to the static centred-dot glyph — same behaviour Phase 69b promised.

---

## Track F — Validation

### F.1 — `tui-smoke fonts` subcommand

**File:** `userspace/tui-smoke/src/main.rs`
**Symbol:** `cmd_fonts`
**Why it matters:** Phase 69c's acceptance is "the atlas works for real glyphs"; this is the gate.

**Acceptance:**
- [ ] `tui-smoke fonts startup` asserts the boot log contains `term: atlas loaded N glyphs` with N > 100.
- [ ] `tui-smoke fonts branch-icon` writes U+E0A0 to the screen, asserts `Screen::cell(0, 0).codepoint == 0xE0A0` and the renderer's painted pixels are not all blank (atlas rasterizer produced output).
- [ ] `tui-smoke fonts emoji` writes U+1F600; passes whether the font covers it (assert non-blank pixels) or not (assert centred-dot fallback) — both are acceptable, neither must crash.
- [ ] `tui-smoke fonts adversarial` writes 2048 distinct codepoints in sequence; asserts no OOM, atlas size stays at 1024, eviction order is LRU.
- [ ] `tui-smoke fonts missing-font` (driven by an xtask harness that omits the font from the data disk) asserts `term: font load failed; using static fallback` and asserts ASCII / Latin-1 / box-drawing still render.

### F.2 — Wire into `cargo xtask tui-smoke`

**File:** `xtask/src/main.rs`
**Symbol:** `tui_smoke` subcommand
**Why it matters:** One gate covers all of 69 / 69a / 69b / 69c.

**Acceptance:**
- [ ] `cargo xtask tui-smoke` invokes the five new `fonts` checks.
- [ ] Total runtime under 120 s.

---

## Track G — Documentation and Release

### G.1 — Cross-reference Phase 69b

**File:** `docs/roadmap/69b-terminal-utf8-and-glyphs.md`
**Symbol:** N/A
**Why it matters:** Phase 69b's `Deferred Until Later` line for TTF/Nerd Font is closed by 69c.

**Acceptance:**
- [ ] Phase 69b doc updated to mark TTF/Nerd Font as `(closed in Phase 69c)`.

### G.2 — Extend `docs/appendix/term-escape-sequences.md`

**File:** `docs/appendix/term-escape-sequences.md`
**Symbol:** N/A
**Why it matters:** Document the runtime font resolver and the static fallback.

**Acceptance:**
- [ ] New "Font infrastructure" section explains the atlas → static-fallback dispatch.
- [ ] Documents the font path `/usr/share/fonts/m3os/term.ttf` and the asset's provenance.

### G.3 — Kernel patch bump to 0.69.3

**Files:**
- `kernel/Cargo.toml`
- `Cargo.lock`
- `AGENTS.md`
- `docs/roadmap/README.md`

**Symbol:** `version`
**Why it matters:** Patch bump per phase.

**Acceptance:**
- [ ] `kernel/Cargo.toml` `version = "0.69.3"`.
- [ ] `Cargo.lock` regenerated.
- [ ] `AGENTS.md` version cursor updated.
- [ ] `cargo xtask check` passes.

---

## Documentation Notes

- The font asset is fetched, not committed, to keep the repo small. The checksum is committed; the binary is not.
- One font, one size in 69c. Multi-font fallback and size configuration are explicitly deferred.
- The static-table fallback is load-bearing — a deleted/corrupt font file must not brick `term`. The fallback path is tested via `tui-smoke fonts missing-font`.
- Phase 69c lands the *infrastructure*; the first quality TUI apps that consume it (lazygit, lf, fzf) land alongside or after Phase 69d's ncurses port.
