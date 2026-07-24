# Phase 115 - Graphical Web Browser (NetSurf)

**Status:** Planned
**Source Ref:** phase-115
**Depends on:** Phase 114 (TLS library — LibreSSL) ⏳, Phase 45 (Ports System) ✅, Phase 86c (curl / libcurl fetch) ✅, Phase 105 (m3ui toolkit, `imagefmt`, the `display_server` client model) ✅, Phase 68/72/73 (compositor: surfaces, tiling, polish) ✅, Phase 47/70 (DOOM as a fullscreen framebuffer client — the SHM-into-compositor precedent) ✅, Phase 87 (VFS bulk-I/O — heavy install) ✅
**Builds on:** The compositor's SHM-surface client model (`display_server` + `desktop_client`/`surface_buffer`, as used by DOOM and `imgview`), the LibreSSL + curl/libcurl fetch stack from Phase 114/86c, the `imagefmt` decoders, and the cross-compiled ports substrate. This is a **multi-library arc**, split into sub-phases 115a (the library stack) and 115b (the framebuffer frontend).
**Primary Components:** ~10 new `ports/lib/libns*` + `ports/lib/libcss`/`libdom`/`libhubbub`/`libnsfb` ports, new `ports/util/netsurf`, a `netsurf` framebuffer frontend wired as a `display_server` client, `xtask/src/port_build.rs`, `xtask/src/main.rs`

## Milestone Goal

m3OS renders **real HTML and CSS graphically** — a genuine web page with laid-out text,
images, and links drawn into a compositor window, navigable with keyboard and mouse. The
deliverable is **NetSurf** running on its framebuffer frontend as a `display_server` client,
fetching over HTTPS and painting a rendered page. This is the ceiling of the usability arc:
the first time the OS shows a recognizable web page rather than a terminal approximation.

## Why This Phase Exists

Phase 114 gives text-mode browsing; a graphical browser is a categorically larger, and more
motivating, capstone — but the modern engine options (Blink/WebKit/Gecko) are millions of
lines of C++ with GPU, JIT-JS, and multiprocess assumptions m3OS cannot host. **NetSurf** is
the realistic target for a hobby OS:

- It is portable C with a **modest, well-factored** dependency set (its own ~10 small
  "libns*"/libcss/libdom/libhubbub libraries plus libcurl, zlib, libpng/libjpeg), and a
  **framebuffer frontend** (`libnsfb`) explicitly designed for platforms without X11 —
  exactly m3OS's situation.
- The compositor already hosts fullscreen/windowed **framebuffer clients**: DOOM (Phase
  47/70) and `imgview` (Phase 105 D.1) draw pixels into a `display_server` SHM surface and
  submit damage. NetSurf's `libnsfb` surface maps onto the same seam — the frontend renders
  into an SHM buffer and submits, receiving keyboard/pointer events back through the
  focus-aware dispatcher, precisely as DOOM does.
- The fetch + TLS stack is already (being) built: libcurl (Phase 86c curl port) over LibreSSL
  (Phase 114) validates against the CA bundle; zlib decompresses; `imagefmt` (Phase 105)
  already decodes PNG/JPEG/BMP in-tree (informing, if not directly linking, the image path).

What does **not** exist is any of the NetSurf library stack, a `libnsfb` port, or a frontend
bound to `display_server`. That is real porting work — a dozen ports and a new compositor
client — which is why this is its own multi-track, sub-phased arc rather than a single port.

## Learning Goals

- The layered architecture of a browser engine: fetch → HTML parse (libhubbub) → DOM
  (libdom) → CSS parse + cascade/selection (libcss) → box/layout → paint, and how NetSurf
  keeps each as a separable library.
- Driving a **framebuffer frontend**: `libnsfb`'s surface/plotter abstraction, and mapping it
  onto a compositor SHM client (allocate SHM → render → submit damage → handle input) — the
  same pattern DOOM and `imgview` use.
- Font rendering for graphical layout (NetSurf's internal bitmap font vs. an external
  freetype), and why text metrics drive layout.
- Cross-compiling a **dependency graph of a dozen interdependent C libraries** with the
  shared musl toolchain and a correct topological install order.
- The hard limits: no (or minimal, Duktape-only) JavaScript, no GPU, no modern CSS grid/flex
  edge cases — and why that still renders a large, useful slice of the documentation web.

## Feature Scope

### Track A (115a) — The NetSurf library stack

Port NetSurf's own libraries as static archives, in dependency order, each through the shared
musl toolchain and registered in the four port points (`port_build` dispatch, `port_deps`,
`build_recipe_id`, image bundle):

- **libwapcaplet** (interned strings), **libparserutils** (input/charset) — no NetSurf deps.
- **libhubbub** (HTML5 parser; deps: libparserutils), **libcss** (CSS; deps: libwapcaplet,
  libparserutils), **libdom** (DOM; deps: libhubbub, libwapcaplet, libparserutils).
- **libnsgif**, **libnsbmp** (GIF/BMP decoders), **libnsutils**, **libnslog**, **libnspsl**
  (public-suffix list), **libutf8proc** — small leaf libraries.
- **libnsfb** — the framebuffer surface/plotter library the frontend renders through.

External deps already available: **libcurl** (Phase 86c), **LibreSSL** (Phase 114),
**zlib** (ported), plus small **libpng**/**libjpeg** ports for raster images.

### Track B (115b) — NetSurf framebuffer frontend as a compositor client

Port NetSurf core + its **framebuffer frontend**, and bind that frontend to `display_server`:

- Build `netsurf` with the `framebuffer` frontend against `libnsfb` + the Track A stack,
  using an **internal bitmap font** (avoids a freetype/fontconfig port in the first cut).
- Replace/adapt `libnsfb`'s surface backend so it allocates a compositor SHM surface (the
  DOOM/`imgview` path via `surface_buffer`/`desktop_client`), renders the page into it, and
  submits damage; route `display_server` keyboard/pointer events into NetSurf's input
  handlers (scroll, click-to-follow-link, URL entry).
- Fetch through libcurl over LibreSSL, verifying against `/etc/ssl/certs/ca-certificates.crt`;
  a correct clock (Phase 113 SNTP) keeps cert-validity checks passing.

### Track C — Bundle wiring + render gate

- Declare the full transitive DEPS chain so `pkg install netsurf` installs the dozen
  libraries + curl/libressl/zlib/libpng/libjpeg + netsurf in topological order.
- A **QMP/PPM render probe** (`netsurf-smoke`): launch NetSurf on a **local** HTML fixture in
  a compositor window, screendump, and assert the window contains rendered non-blank content
  (laid-out text/box regions — the `imgview-smoke` / `claude_tui_render_arm` /
  `htop-render-probe` pattern, which asserts real pixels, not just that the process ran).
  Opt-in live HTTPS arm fetches a real page.

## Important Components and How They Work

### `libnsfb` → `display_server` SHM client (Track B)

The compositor client contract is already exercised by DOOM and `imgview`: a client looks up
the `"display"` service, allocates an SHM-backed surface via the `surface_buffer` /
`desktop_client` path, draws pixels, and submits damage; the focus-aware dispatcher returns
`ServerMessage::Key`/`Pointer` events to the focused surface (the same events `term`
consumes). NetSurf's `libnsfb` already abstracts "a surface you plot into"; the port's work is
a new `libnsfb` surface backend whose `claim`/`update`/`release` map to allocate-SHM /
submit-damage / free on the compositor, and an event pump that translates `display_server`
input into NetSurf's `fbtk`/browser event calls. No compositor change is required — NetSurf
becomes one more SHM client.

### The dependency graph and topological install (Track A/C)

NetSurf's libraries form a DAG (libwapcaplet/libparserutils at the base, libcss/libhubbub/
libdom above, the frontend + core at the top). Each Portfile declares its `DEPS=`, mirrored
into the `.meta` sidecar and resolved dependency-first by `topo_install_order`
(`userspace/pkg/src/lib.rs:355`) — the same solver curl's `zlib → mbedtls → ca-certificates`
chain uses, just deeper. Every `build_*` routes through `musl_toolchain()` +
`musl_extra_ldflags_joined()` + `--host=x86_64-linux-musl`, and pins its flags in
`build_recipe_id` (`port_build.rs:339`) for cache invalidation.

### Fetch, TLS, and the clock (Track B)

NetSurf fetches through **libcurl** (the Phase 86c curl port already builds a static libcurl)
over **LibreSSL** (Phase 114), verifying the certificate chain against the Phase 86a CA bundle
— the identical trust path the text browser and git use. Cert validity depends on a correct
clock, so Phase 113 (SNTP) is a transitive dependency of a browser that works on a machine
that has been powered off.

## How This Builds on Earlier Phases

- **Reuses the Phase 47/70 + 105 compositor-client model** (SHM surface + submit damage +
  focus-aware input) unchanged — NetSurf is a client like DOOM/`imgview`, not a compositor
  change.
- **Builds on Phase 114** (LibreSSL) + **Phase 86c** (libcurl) for fetch/TLS, and **Phase
  113** (SNTP) for cert-valid time.
- **Reuses the Phase 45 ports substrate** at scale — the deepest single dependency graph the
  ports system will have built.
- **Complements Phase 105** (`imagefmt`): the OS already decodes PNG/JPEG/BMP in-tree;
  NetSurf brings its own decoders, but the image-in-a-window experience is the graphical
  extension of `imgview`.

## Implementation Outline

1. **115a Track A:** port the NetSurf library stack in DAG order (libwapcaplet →
   libparserutils → libhubbub/libcss/libdom → the leaf libns* → libnsfb) + libpng/libjpeg;
   register each in the four port points; verify each links.
2. **115b Track B:** port netsurf core + the framebuffer frontend against `libnsfb`;
   implement the `libnsfb`→`display_server` SHM surface backend + input pump; fetch via
   libcurl/LibreSSL; internal bitmap font.
3. **Track C:** declare the full DEPS chain; add the `netsurf-smoke` QMP/PPM render probe
   (local arm always-on, live HTTPS opt-in); document the gate + the JS/engine ceiling.

## Acceptance Criteria

- **115a:** each NetSurf library + libpng/libjpeg builds as a static archive via the shared
   toolchain and passes a link probe; `pkg install` resolves the sub-graph.
- **115b:** `netsurf` launches as a `display_server` client and renders a **local** HTML+CSS
   fixture — a QMP/PPM screendump of its window shows laid-out non-blank content (text/box
   regions), asserted by pixel occupancy the way `imgview-smoke` / `htop-render-probe` do (a
   blank window ≈ 0 changed scanlines → fail). Keyboard scroll + click-to-follow-link work.
- **Track C:** `pkg install netsurf` resolves the full transitive chain; `netsurf-smoke`
   (`M3OS_NETSURF_REGRESSION=1`) passes the local render arm; the opt-in `M3OS_NETSURF_LIVE`
   arm fetches + renders a real HTTPS page (cert-verified), skip-with-reason in CI.
- The JavaScript/engine ceiling is documented (below) so the milestone is not mistaken for a
   modern-web browser.

## Companion Task List

- [Phase 115 Task List](./tasks/115-graphical-web-browser-netsurf-tasks.md)

## How Real OS Implementations Differ

- **Chromium/WebKit/Gecko** implement full modern CSS, a JIT JavaScript engine, multiprocess
  site isolation, GPU compositing, and media pipelines — millions of lines. NetSurf renders
  HTML + a large CSS subset with **little or no JavaScript** (optional Duktape); JS-driven
  single-page apps and most web apps will not work. This is a documentation/wiki/static-site
  browser, not a Chrome replacement.
- Production browsers composite on the **GPU**; NetSurf plots into a CPU framebuffer (here, a
  compositor SHM surface).
- Real browsers manage sandboxing, extensions, sync, and an auto-updating trust store with
  revocation; m3OS's NetSurf reuses the single static CA bundle with none of that.
- Font rendering in production is subpixel-AA freetype/HarfBuzz shaping; the first cut uses
  NetSurf's internal bitmap font (freetype is a deferred enhancement).

## Deferred Until Later

- JavaScript (even NetSurf's optional Duktape engine) — the first cut is HTML+CSS only.
- freetype/fontconfig for scalable, anti-aliased, shaped text (start with the internal font).
- Tabbed browsing, history/bookmarks persistence, downloads UI, and cookies beyond the
  session.
- Video/audio media elements and web fonts.
- A native (non-ported, m3ui-based) browser chrome — the first cut uses NetSurf's own
  framebuffer UI (`fbtk`).
- Splitting the effort formally into published 115a/115b PRs (mirroring the 92a–92e and
  111a/111b precedent) is expected during implementation as scope firms up.
