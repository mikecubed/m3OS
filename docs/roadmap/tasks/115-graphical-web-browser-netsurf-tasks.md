# Phase 115 — Graphical Web Browser (NetSurf): Task List

**Status:** Planned
**Source Ref:** phase-115
**Depends on:** Phase 114 (LibreSSL) ⏳, Phase 45 (Ports) ✅, Phase 86c (libcurl) ✅, Phase 105 (compositor client model + `imagefmt`) ✅, Phase 68/72/73 (compositor) ✅, Phase 47/70 (DOOM SHM-client precedent) ✅, Phase 113 (SNTP) ⏳, Phase 87 (VFS bulk-I/O) ✅
**Goal:** Render real HTML+CSS graphically by porting the NetSurf library stack (~10 libraries + libnsfb) and its framebuffer frontend, bound to `display_server` as an SHM client the way DOOM/`imgview` are, fetching over libcurl/LibreSSL — a multi-library arc split into 115a (libraries) and 115b (frontend).

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A (115a) | NetSurf library stack + libpng/libjpeg ports | — | Planned |
| B (115b) | NetSurf core + framebuffer frontend as a `display_server` client | A | Planned |
| C | DEPS chain wiring + `netsurf-smoke` QMP/PPM render gate | A, B | Planned |

Expect A and B to land as separate PRs (115a / 115b) mirroring the 92a–92e / 111a-b
precedent. Every `build_*` MUST route through the shared musl toolchain
(`musl_toolchain()`, `musl_extra_ldflags_joined()`, `--host=x86_64-linux-musl`).

---

## Track A (115a) — NetSurf library stack

### A.1 — Base libraries (no NetSurf deps)

**Files:** `ports/lib/libwapcaplet/`, `ports/lib/libparserutils/` (Portfiles), `xtask/src/port_build.rs` (`build_libwapcaplet`, `build_libparserutils`)
**Symbols:** `musl_toolchain` (`port_build.rs:111`), `build_recipe_id` (`port_build.rs:339`), `port_deps` (`port_build.rs:935`); model `build_ncurses` (`port_build.rs:3238`)
**Why it matters:** These are the DAG roots; everything else links them.

**Acceptance:**
- [ ] Both build as static archives via the shared toolchain; registered in the four points (dispatch `match name` at `port_build.rs:1958`, `port_deps`, `build_recipe_id`, bundle list `main.rs:34655`).

### A.2 — Parser/DOM/CSS libraries

**Files:** `ports/lib/libhubbub/`, `ports/lib/libcss/`, `ports/lib/libdom/` + `build_*`
**Symbol:** `build_libhubbub` / `build_libcss` / `build_libdom`
**Why it matters:** HTML parse → DOM → CSS cascade — the engine's middle layer.

**Acceptance:**
- [ ] `libhubbub` (DEPS libparserutils), `libcss` (DEPS libwapcaplet libparserutils), `libdom` (DEPS libhubbub libwapcaplet libparserutils) build static; DEPS declared so the solver orders them correctly.

### A.3 — Leaf libraries + raster decoders

**Files:** `ports/lib/{libnsgif,libnsbmp,libnsutils,libnslog,libnspsl,libutf8proc,libnsfb}/` + `ports/lib/{libpng,libjpeg}/` + `build_*`
**Symbol:** the leaf `build_*` functions; `libnsfb` is the frontend's plot surface
**Why it matters:** Image decode + utility libs + the framebuffer surface the frontend renders through.

**Acceptance:**
- [ ] Each builds static via the shared toolchain; `libpng` DEPS zlib; all registered in the four points.

---

## Track B (115b) — Framebuffer frontend as a compositor client

### B.1 — `libnsfb` → `display_server` SHM surface backend

**Files:** `ports/lib/libnsfb/` (a backend patch/adaptation), reference `userspace/doom` + `userspace/imgview` (the SHM-client path), `userspace/lib/surface_buffer`, `userspace/lib/desktop_client`
**Symbols:** the compositor client contract (`"display"` service lookup, SHM surface allocate + submit-damage, `ServerMessage::Key`/`Pointer` focus dispatch)
**Why it matters:** NetSurf must draw into a compositor window and receive input — the exact seam DOOM/`imgview` already use; no compositor change.

**Acceptance:**
- [ ] A `libnsfb` surface backend whose claim/update/release map to allocate-SHM / submit-damage / free against `display_server`.
- [ ] An event pump translating `display_server` keyboard/pointer events into NetSurf's frontend input (scroll, click-to-follow-link, URL entry).

### B.2 — `ports/util/netsurf` core + framebuffer frontend

**Files:** `ports/util/netsurf/Portfile`, `xtask/src/port_build.rs` (`build_netsurf`)
**Symbols:** links the Track A stack + libcurl (Phase 86c) + LibreSSL (Phase 114) + zlib; internal bitmap font
**Why it matters:** The user-facing deliverable — the browser binary + frontend.

**Acceptance:**
- [ ] `build_netsurf` builds the `framebuffer` frontend against `libnsfb` + the full library stack, with the internal bitmap font (no freetype in the first cut); produces `/usr/local/bin/netsurf`.
- [ ] Fetch via libcurl over LibreSSL, verifying `/etc/ssl/certs/ca-certificates.crt`.
- [ ] `Portfile` DEPS enumerates the full transitive chain.

---

## Track C — Wiring + render gate

### C.1 — Transitive DEPS chain + bundle registration

**Files:** each Portfile's `DEPS=`, `xtask/src/port_build.rs` (`port_deps`), `xtask/src/main.rs` (bundle list)
**Symbol:** `topo_install_order` (`userspace/pkg/src/lib.rs:355`)
**Why it matters:** The solver must install a dozen interdependent libraries dependency-first.

**Acceptance:**
- [ ] `pkg install netsurf` resolves and installs the full chain (base libs → parser/DOM/CSS → leaf libs + libpng/libjpeg → libcurl/libressl/zlib → netsurf) in topological order.

### C.2 — `netsurf-smoke` QMP/PPM render probe

**Files:** `xtask/src/main.rs` (new `cmd_netsurf_smoke`, model `imgview-smoke` / `claude_tui_render_arm` / `htop-render-probe`), `xtask/src/qmp.rs` + `xtask/src/ppm.rs`, `.githooks/pre-push` (`M3OS_NETSURF_REGRESSION`), `AGENTS.md` + `docs/appendix/regression-gates.md`
**Symbols:** `QmpClient::{connect, send_key, screendump}`, PPM pixel-occupancy assertion
**Why it matters:** A serial `Wait` proves the process ran; only a framebuffer dump proves a page rendered (a blank window ≈ 0 changed scanlines).

**Acceptance:**
- [ ] Always-on local arm: launch NetSurf on a local HTML+CSS fixture in a compositor window, screendump, assert the window contains laid-out non-blank content (pixel-occupancy over threshold); inject a scroll key and assert the frame changes.
- [ ] Opt-in `M3OS_NETSURF_LIVE` arm: fetch + render a real HTTPS page (cert-verified); skip-with-reason in CI (like `git-https-smoke`).
- [ ] Gate row added to `AGENTS.md` + `regression-gates.md`; the JS/engine ceiling documented in the phase doc.

---

## Documentation Notes

- This is the deepest dependency graph the ports system builds; call out the DAG order and
  the `topo_install_order` solver.
- NetSurf is a **client** of the existing compositor (SHM surface + submit-damage + focus
  input) — the same contract as DOOM (Phase 47/70) and `imgview` (Phase 105); no compositor
  change. Reference those as the precedent.
- State the ceiling plainly: HTML+CSS, little/no JavaScript, CPU-plotted — a
  documentation/static-site browser, not a modern web-app browser.
- Expect 115a/115b to ship as separate PRs; note the split when it firms up.
- Prefer exact symbols; `port_build.rs`/`main.rs` line numbers drift — reference
  `build_ncurses`/`build_curl`/`imgview`/`topo_install_order`.
