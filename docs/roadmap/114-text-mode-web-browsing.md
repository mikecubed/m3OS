# Phase 114 - Text-Mode Web Browsing (TLS Library + w3m)

**Status:** Planned
**Source Ref:** phase-114
**Depends on:** Phase 45 (Ports System) ✅, Phase 69d (ncurses port + `tui-app-smoke`) ✅, Phase 86a (CA-trust + resolver + wall-clock floor) ✅, Phase 86c (mbedTLS + curl HTTPS pattern) ✅, Phase 113 (SNTP — a correct clock for cert validity) ⏳, Phase 87 (VFS bulk-I/O — install throughput) ✅
**Builds on:** The cross-compiled ports substrate (`ports/`, `xtask/src/port_build.rs`, the shared musl toolchain), the ncurses port + `tui-app-smoke` gate (Phase 69d), the `ca-certificates` trust bundle + the curl/mbedTLS static-HTTPS pattern (Phase 86c), and the in-kernel TCP/IP stack + musl resolver the browser fetches over.
**Primary Components:** new `ports/lib/libressl`, new `ports/lib/libgc`, new `ports/util/w3m`, `xtask/src/port_build.rs`, `xtask/src/main.rs` (image bundling + gate)

## Milestone Goal

m3OS gets a real web browser — text-mode, but genuinely usable: it fetches and renders HTML
over **HTTPS**, follows links, and displays documentation, wikis, and package changelogs
inside `term`. The headline deliverable is `w3m` running in-OS against a freshly-ported
TLS library, validating certificates against the existing CA bundle. This is the cheap,
honest path to "the OS can browse the web" — one new TLS library port unlocks the whole class
of OpenSSL-linking network tools.

## Why This Phase Exists

The OS has networking, TLS, DNS, and a capable terminal — but **no browser**, and the one
TLS library it has cannot drive most browsers:

- The **only** TLS/crypto library port is **mbedTLS 3.6.2** (`ports/lib/mbedtls/`), and its
  only consumers are `curl` and `git` (`build_curl` `--with-mbedtls`,
  `xtask/src/port_build.rs:5083`; `build_git` via static libcurl). There is **no** OpenSSL,
  LibreSSL, GnuTLS, or wolfSSL port — confirmed by enumerating `ports/`.
- Every mainstream text browser (`w3m`, `lynx`, `links`) links **OpenSSL** (or GnuTLS), not
  mbedTLS. So "add a browser" is really "add an OpenSSL-API-compatible TLS library, then the
  browser on top" — exactly the shape of the curl/mbedTLS work in Phase 86c, but with a
  library that exposes `libssl`/`libcrypto`.
- Everything else the browser needs already exists: the ports toolchain
  (`musl_toolchain()`, `find_musl_cc()`, the `target/musl-stub-libs/` compat archives), the
  ncurses port (Phase 69d), zlib, the `ca-certificates` bundle mounted at
  `/etc/ssl/certs/ca-certificates.crt`, a static-musl C runtime with `getaddrinfo`/`connect`
  over the kernel socket syscalls, and `/etc/resolv.conf` → the SLIRP virtual DNS. The
  `tui-app-smoke` harness already validates ncurses TUIs rendering in `term`.

So the phase is mostly a **porting** exercise on a proven substrate: bring up **LibreSSL**
(OpenSSL-API `libssl`/`libcrypto`, a clean autotools static build), a small **Boehm GC**
(`libgc`, w3m's allocator dependency), and **w3m** itself, then wire the dependency chain and
a render gate. `lynx`/`links` are lower-dependency fallbacks if w3m's build proves fussy.

## Learning Goals

- The anatomy of an HTTP(S) client at the library level: TCP connect → TLS handshake →
  certificate chain + hostname verification against a CA bundle → HTTP request/response →
  content decode (gzip via zlib).
- Why a wrong clock breaks HTTPS (cert `notBefore`/`notAfter`) — the concrete tie-in to
  Phase 113 SNTP and the Phase 86a build-date floor.
- Porting an OpenSSL-API library (LibreSSL) with the shared musl cross-toolchain: the
  `--host` triple, the `-static` + stub-archive LDFLAGS, and `build_recipe_id` cache
  invalidation.
- How a **conservative garbage collector** (Boehm GC) is packaged as a static archive and
  why w3m depends on one.
- HTML rendering as a **layout-to-cells** problem: turning a parsed document into a terminal
  grid (tables, links, forms) — and the limits of text-mode rendering (no CSS layout, no JS).

## Feature Scope

### Track A — LibreSSL port (the OpenSSL-API TLS library)

Port **LibreSSL** (portable release, autotools) to produce static `libssl.a` + `libcrypto.a`
(+ `libtls.a`) under the staged prefix, following the mbedTLS/curl pattern:

- Resolve the toolchain via `musl_toolchain()`; compose LDFLAGS with
  `musl_extra_ldflags_joined()` (so the `-static -ldl -lpthread -lrt` configure probe finds
  the `target/musl-stub-libs/` archives); pass `--host=x86_64-linux-musl`; use the
  `(cc, ar, ranlib)` tuple. Configure `--enable-static --disable-shared --with-openssldir=/etc/ssl`.
- Point cert verification at the existing `ca-certificates` bundle
  (`/etc/ssl/certs/ca-certificates.crt`) so no new trust material is introduced.
- Register the port in all four places: the `port_build` dispatch `match name`
  (`port_build.rs:1958`), `port_deps` (`port_build.rs:935`), `build_recipe_id`
  (`port_build.rs:339`), and the image-builder bundle list (`populate_phase_69d_ports`,
  `main.rs:34655`).

### Track B — Boehm GC port (w3m's allocator)

Port **Boehm-Demers-Weiser GC** (`libgc`) as a static `libgc.a` — a small, self-contained
autotools build with no further ports dependencies. (This track is skipped if the browser
target chosen in Track C is `lynx`/`links`, which do not need a GC.)

### Track C — w3m port + browser gate

Port **w3m** linked against LibreSSL + libgc + ncurses + zlib, using the `ca-certificates`
bundle for verification:

- `ports/util/w3m/Portfile` with `DEPS=zlib ncurses libgc libressl ca-certificates`; a
  `build_w3m` that guards each staged archive, sets `CFLAGS`/`LDFLAGS`/`LIBS` for the four
  libraries, `--host=x86_64-linux-musl`, and produces `/usr/local/bin/w3m`.
- Declare the transitive dependency chain so the `pkg` solver
  (`topo_install_order`, `userspace/pkg/src/lib.rs:355`) installs
  `zlib → ncurses → libgc → libressl → ca-certificates → w3m` in order.
- A `browser-smoke` gate on the `tui-app-smoke` pattern: launch `w3m` on a **local** HTML
  fixture inside `term` and assert known rendered text appears on the PTY (always-on); plus
  an **opt-in live HTTPS** arm (`w3m https://…`) that fetches a real page and asserts a
  cert-verified 200, mirroring `git-https-smoke`'s live/opt-in split.

## Important Components and How They Work

### The ports substrate this rides (Track A/B/C)

Every port is a **fully static musl binary/archive** — m3OS ships no general `libc.so` for
ports. The build flows through `port_build(name)` (`port_build.rs:1710`), which resolves the
cross toolchain (`musl_toolchain()` at `port_build.rs:111` → `find_musl_cc()` at
`main.rs:5324`) and dispatches on `match name` (`port_build.rs:1958`). Configure flags are
pinned per-port in `build_recipe_id` (`port_build.rs:339`) — the real cache key (the
`BUILD_FLAGS=` Portfile field is never parsed). The `-static -ldl -lpthread -lrt` probe every
autotools port runs needs the empty compat archives materialized by
`ensure_musl_stub_libs()` (`main.rs:5473`) and threaded in via `musl_extra_ldflags_joined()`
— the exact plumbing AGENTS.md mandates. LibreSSL, libgc, and w3m each follow the model that
`build_ncurses` (`port_build.rs:3238`) and `build_curl` (`port_build.rs:5083`) already
establish.

### TLS verification and the clock dependency (Track A/C)

w3m-over-LibreSSL does the same chain as curl-over-mbedTLS: connect via the kernel socket
syscalls, TLS-handshake, then verify the server certificate chain + hostname against
`/etc/ssl/certs/ca-certificates.crt` (the Phase 86a bundle curl already pins with
`--with-ca-bundle`). Verification checks cert validity **dates**, which is exactly why Phase
113 (SNTP) is a listed dependency: on a machine whose clock is wrong, every HTTPS fetch fails
with a confusing "certificate not yet valid / expired" error. The Phase 86a build-date floor
guarantees the clock is at least as new as the image, and SNTP keeps it correct thereafter.

### DNS + TCP for a ported C app (Track C)

A static-musl browser gets DNS + TCP for free: musl `getaddrinfo` reads `/etc/resolv.conf`
(`nameserver 10.0.2.3`, the SLIRP virtual DNS, staged at `main.rs:32018`) and issues UDP
queries, then `connect()`s over the kernel socket syscalls (`SOCKET`/`CONNECT`/… dispatched
in `kernel/src/arch/x86_64/syscall/mod.rs`) into the in-kernel TCP/IP stack. This is the same
path `dns-smoke` (a musl-linked C binary) and curl already exercise; the browser adds no new
network-layer code.

## How This Builds on Earlier Phases

- **Extends Phase 86c** by adding the *second* TLS library (LibreSSL, OpenSSL-API) alongside
  mbedTLS — unlocking the OpenSSL-linking tool class curl's mbedTLS backend cannot serve.
- **Reuses the Phase 45/69d ports substrate** (toolchain, ncurses, `tui-app-smoke`)
  unchanged; the browser is three new Portfiles + `build_*` functions.
- **Reuses the Phase 86a** CA bundle + resolver and the Phase 16 TCP stack for the fetch path.
- **Depends on Phase 113 (SNTP)** for a correct clock so cert validity checks pass on a
  machine that has been powered off for a while.

## Implementation Outline

1. **Track A:** add `ports/lib/libressl/Portfile` + `build_libressl` (static `libssl`/
   `libcrypto`/`libtls`); register in the four points; verify `openssl version` / a static
   link probe on-device.
2. **Track B:** add `ports/lib/libgc/Portfile` + `build_libgc` (static `libgc.a`).
3. **Track C:** add `ports/util/w3m/Portfile` + `build_w3m` linking A+B+ncurses+zlib; declare
   the DEPS chain; add the `browser-smoke` gate (local render arm always-on, live HTTPS arm
   opt-in). Document the gate.

## Acceptance Criteria

- **Track A:** LibreSSL builds static `libssl.a`/`libcrypto.a`/`libtls.a`; a build-time link
   probe confirms a trivial TLS-using C program links; `pkg install libressl` resolves and
   installs on-device.
- **Track C:** `pkg install w3m` resolves the full chain (`zlib → ncurses → libgc →
   libressl → ca-certificates → w3m`) and installs; `w3m` renders a local HTML fixture in
   `term` with the expected text on the PTY (`browser-smoke`, `M3OS_BROWSER_REGRESSION=1`);
   the opt-in `M3OS_BROWSER_LIVE` arm fetches a real HTTPS page (cert-verified, HTTP 200) and
   renders it — skip-with-reason in CI like `git-https-smoke`.
- A rejected-bad-cert arm (a self-signed / expired host is refused) mirrors the Phase 86c
   mandatory bad-cert rejection.

## Companion Task List

- [Phase 114 Task List](./tasks/114-text-mode-web-browsing-tasks.md)

## How Real OS Implementations Differ

- A desktop **browser engine** (Blink/WebKit/Gecko) does CSS layout, JavaScript, media, and
  GPU compositing; a text browser does none of that — `w3m` renders a flowed
  tables-and-links approximation into character cells. Large parts of the modern web
  (JS-only single-page apps) simply do not work.
- Production distros ship **OpenSSL** as a first-class system library many packages share;
  m3OS ports **LibreSSL** as a static archive per-consumer (no shared `.so`), so each TLS
  tool carries its own copy.
- Mainstream browsers manage their own **trust store** with revocation (CRL/OCSP) and
  pinning; m3OS reuses the single static CA bundle with no revocation.
- `w3m` uses a conservative **Boehm GC**; production language runtimes use precise collectors
  — the GC here is an implementation detail of the port, not a system service.

## Deferred Until Later

- `lynx` / `links` as additional/alternative text browsers (lower dependency footprint — no
  libgc; land if w3m's GC dependency proves troublesome).
- A `vi`/`vim` port (same ncurses/autotools class as the browser tooling; a natural
  companion that needs no TLS) — a cheap follow-on, tracked here as a note rather than a
  track.
- OpenSSL proper (vs. LibreSSL) if a consumer needs an API LibreSSL does not provide.
- Certificate revocation (CRL/OCSP), key pinning, and a per-user trust store.
- The graphical browser (NetSurf) — its own arc, [Phase 115](./115-graphical-web-browser-netsurf.md).
- w3m's inline-image (sixel/framebuffer) mode.
