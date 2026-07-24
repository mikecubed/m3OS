# Phase 114 — Text-Mode Web Browsing (TLS Library + w3m): Task List

**Status:** Planned
**Source Ref:** phase-114
**Depends on:** Phase 45 (Ports System) ✅, Phase 69d (ncurses + `tui-app-smoke`) ✅, Phase 86a (CA-trust + resolver) ✅, Phase 86c (mbedTLS/curl HTTPS pattern) ✅, Phase 113 (SNTP — cert-valid clock) ⏳, Phase 87 (VFS bulk-I/O) ✅
**Goal:** Add the OS's second TLS library (LibreSSL — OpenSSL-API `libssl`/`libcrypto`, the class of library every text browser links) plus a Boehm-GC port, then port `w3m` on top and gate it — fetching + rendering HTTPS pages in `term`, verified against the existing CA bundle.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | LibreSSL port (static `libssl`/`libcrypto`/`libtls`) | — | Planned |
| B | Boehm GC port (`libgc.a`) | — | Planned |
| C | w3m port + `browser-smoke` gate | A, B, ncurses, zlib, ca-certificates | Planned |

A and B are independent library ports; C consumes both. Every `build_*` MUST route through
the shared musl toolchain (`musl_toolchain()`, `musl_extra_ldflags_joined()`,
`--host=x86_64-linux-musl`) or fail the `-static -ldl -lpthread -lrt` link probe (exit 77).

---

## Track A — LibreSSL port

### A.1 — `ports/lib/libressl/Portfile` + `build_libressl`

**Files:** `ports/lib/libressl/Portfile` (new), `xtask/src/port_build.rs` (`build_libressl`, new)
**Symbols:** `musl_toolchain` (`port_build.rs:111`), `musl_extra_ldflags_joined` (`port_build.rs:105`), `find_musl_cc` (`main.rs:5324`), `ensure_musl_stub_libs` (`main.rs:5473`); model `build_ncurses` (`port_build.rs:3238`), `build_curl` (`port_build.rs:5083`)
**Why it matters:** LibreSSL exposes the OpenSSL API (`libssl`/`libcrypto`) that w3m/lynx/links link; mbedTLS (the only existing TLS port) does not.

**Acceptance:**
- [ ] Portfile: `NAME=libressl`, `VERSION=<portable release>`, `CATEGORY=lib`, `DEPS=`, `URL`/`SHA256`.
- [ ] `build_libressl` resolves `(cc, ar, ranlib)` via `musl_toolchain()`, composes `LDFLAGS = -static … {musl_extra_ldflags_joined()}`, passes `--host=x86_64-linux-musl --enable-static --disable-shared --with-openssldir=/etc/ssl`, and produces static `libssl.a` + `libcrypto.a` (+ `libtls.a`) under the staged prefix.
- [ ] Cert verification defaults to the existing bundle `/etc/ssl/certs/ca-certificates.crt` (no new trust material).

### A.2 — Register the port (4 points)

**Files:** `xtask/src/port_build.rs` (`port_build` dispatch `match name` at `:1958`, `port_deps` at `:935`, `build_recipe_id` at `:339`), `xtask/src/main.rs` (`populate_phase_69d_ports` bundle list, `BUNDLE_ONLY_PORTS` at `:34655`)
**Symbol:** `build_recipe_id` (the real per-flag cache key; `BUILD_FLAGS=` is never parsed)
**Why it matters:** A new port that misses any of these isn't dispatched, isn't cache-keyed, or isn't bundled into the image.

**Acceptance:**
- [ ] Dispatch arm `"libressl" => build_libressl(...)`; `port_deps("libressl") => &[]`; a stable `build_recipe_id("libressl")` encoding the configure flags; added to the image bundle list.
- [ ] `pkg install libressl` resolves and installs on-device; a build-time TLS link probe (a trivial `libssl`-using C program links) passes.

---

## Track B — Boehm GC port

### B.1 — `ports/lib/libgc/Portfile` + `build_libgc`

**Files:** `ports/lib/libgc/Portfile` (new), `xtask/src/port_build.rs` (`build_libgc`, new)
**Symbol:** `build_libgc`
**Why it matters:** w3m allocates through the Boehm conservative GC; `libgc.a` is a prerequisite archive. (Skip this track if Track C targets `lynx`/`links`, which need no GC.)

**Acceptance:**
- [ ] Portfile `NAME=libgc`, `CATEGORY=lib`, `DEPS=`; `build_libgc` produces static `libgc.a` via the shared toolchain + `--host=x86_64-linux-musl --enable-static --disable-shared`.
- [ ] Registered in the four points (`port_build` dispatch, `port_deps`, `build_recipe_id`, bundle list).

---

## Track C — w3m port + gate

### C.1 — `ports/util/w3m/Portfile` + `build_w3m`

**Files:** `ports/util/w3m/Portfile` (new), `xtask/src/port_build.rs` (`build_w3m`, new)
**Symbols:** model `build_less` (`port_build.rs:3759`) / `build_htop` (`port_build.rs:3819`) for the ncurses-linking + `--host` pattern; final binary at `stage/usr/local/bin/w3m`
**Why it matters:** w3m is the user-facing deliverable; it links four staged libraries (LibreSSL, libgc, ncurses, zlib) + the CA bundle.

**Acceptance:**
- [ ] Portfile `NAME=w3m`, `CATEGORY=util`, `DEPS=zlib ncurses libgc libressl ca-certificates`.
- [ ] `build_w3m` guards each staged archive, sets `CFLAGS`/`LDFLAGS`/`LIBS` for all four libs + `--host=x86_64-linux-musl`, and produces `/usr/local/bin/w3m` (existence-checked).
- [ ] `port_deps("w3m")` + `build_recipe_id("w3m")` + bundle registration so the `pkg` solver installs `zlib → ncurses → libgc → libressl → ca-certificates → w3m` in order (`topo_install_order`, `userspace/pkg/src/lib.rs:355`).

### C.2 — `browser-smoke` gate

**Files:** `xtask/src/main.rs` (new `cmd_browser_smoke`, model `tui_app_smoke_steps` at `:20454`), `.githooks/pre-push` (`M3OS_BROWSER_REGRESSION`), `AGENTS.md` + `docs/appendix/regression-gates.md`
**Symbols:** `SmokeStep::{Send, Wait, WaitPassOrFail}`, a `BROWSER_SMOKE:w3m:ok` sentinel; the local HTML fixture (staged under `/usr/share/w3m/`)
**Why it matters:** A serial `Wait` proves w3m ran; the render arm must assert it produced the expected text; the live arm proves the real HTTPS + cert path.

**Acceptance:**
- [ ] Always-on local arm: `TERM=m3os-term … w3m /usr/share/w3m/test.html` in `term`, wait for a known string from the fixture, `q` to quit, echo `BROWSER_SMOKE:w3m:ok` (the `tui-app-smoke` shape).
- [ ] Opt-in `M3OS_BROWSER_LIVE` arm: `w3m https://<host>` fetches a real page (cert-verified, HTTP 200) and renders expected text; skip-with-reason in CI (like `git-https-smoke`).
- [ ] Bad-cert arm: a self-signed/expired host is refused (mirrors the Phase 86c mandatory reject).
- [ ] Gate row added to `AGENTS.md` + `regression-gates.md`.

---

## Documentation Notes

- Frame Track A as adding the OS's **second** TLS library — LibreSSL (OpenSSL API) alongside
  mbedTLS — and explain why mbedTLS could not serve a browser (no `libssl`/`libcrypto`).
- Every `build_*` must use the shared musl plumbing; cite the exit-77 link-probe failure as
  the symptom of skipping it (AGENTS.md's ports contract).
- Note the Phase 113 clock dependency: HTTPS cert validity fails on a wrong clock — the
  concrete reason SNTP precedes the browser.
- Record `lynx`/`links` (no libgc) and a `vim` port as cheap follow-ons if scope allows.
- Prefer exact symbols; `port_build.rs`/`main.rs` line numbers drift heavily (both files are
  tens of thousands of lines) — reference `build_curl`/`build_ncurses`/`tui_app_smoke_steps`.
