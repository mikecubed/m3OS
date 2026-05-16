# TUI App Port Notes — Phase 69d

**Status:** Living document
**Source Ref:** phase-69d Track F.2
**Last touched:** 2026-05-16 (kernel 0.69.4)

Reference document for the Phase 69d ports. Catalogs the upstream version,
configure flags, and per-app terminal-contract coverage so future port
authors can match a new app against the known-good recipes — and so a
regression in any one capability has a single place to record what
broke.

## Port matrix

| Port | Version | URL | SHA-256 (first 16) | Stages to |
|---|---|---|---|---|
| `ncurses` | 6.5 | `ftp.gnu.org/.../ncurses-6.5.tar.gz` | `136d91bc269a9a57` | `usr/local/{bin,lib,include}` + `usr/share/terminfo` |
| `libevent` | 2.1.12-stable | `github.com/libevent/.../libevent-2.1.12-stable.tar.gz` | `92e6de1be9ec1764` | `usr/local/{lib,include}` |
| `less` | 668 | `greenwoodsoftware.com/less/less-668.tar.gz` | `2819f55564d86d54` | `/usr/local/bin/less` |
| `htop` | 3.4.0 | `github.com/htop-dev/.../htop-3.4.0.tar.xz` | `feaabd2d31ca27c0` | `/usr/local/bin/htop` |
| `tmux` | 3.5a | `github.com/tmux/.../tmux-3.5a.tar.gz` | `16216bd087717ddf` | `/usr/local/bin/tmux` |

Full SHA-256 lives in each port's `Portfile`. The host-side
`cargo xtask port build <name>` driver re-verifies the SHA on every
fetch and refuses to extract a tarball whose hash doesn't match.

## Capability coverage per app

| App | ncurses variant | terminfo capabilities exercised | termios flags used | UTF-8 blocks consumed | Nerd Font glyphs |
|---|---|---|---|---|---|
| `less` | narrow (`libncurses.a` + `libtinfo.a`) | `smcup`/`rmcup` (?1049), `clear`, `el`, `civis`/`cnorm`, `setaf`/`setab`, `cup`, `cuf`/`cub`/`cuu`/`cud` | `ICANON` off, `ECHO` off, `VMIN`=1, `VTIME`=0 | ASCII-only on the /etc/passwd smoke; UTF-8 viewing path exists for `less -R` but is not exercised in 69d | none |
| `htop` | wide (`libncursesw.a` + `libtinfow.a`) | `cup`, `setaf`/`setab` 256-color, `civis`/`cnorm`, `smcup`/`rmcup`, box-drawing | `ICANON` off, `ECHO` off, `VMIN`=1, `VTIME`=1, `SIGWINCH` handler | Box-drawing characters (U+2500..=U+257F) for CPU/mem gauges | optional theme glyphs (not exercised) |
| `tmux` | wide | full `setaf`/`setab` truecolor, `csr` (scrolling region), `cup`, mouse encoding via `XM`/`xm`, bracketed paste `BE`/`BD` | nested PTY raw mode, `IUTF8`, `IXON` per-pane, `SIGWINCH` per-pane | Box-drawing for pane dividers; UTF-8 status line if themes use it | optional Nerd Font theme glyphs |

## Build flags worth knowing

### ncurses
`--without-shared --with-normal --with-termlib --enable-overwrite
--enable-widec` (wide pass) / `--disable-widec` (narrow pass);
`--datadir=/usr/share` so the runtime terminfo lookup matches the on-target
file system; `make install DESTDIR=$STAGE` to keep the host root clean.

### libevent
`--disable-shared --disable-openssl --disable-samples
--disable-debug-mode --disable-libevent-regress`. The OpenSSL knob is
critical: m3OS has no system OpenSSL today and pulling it in would
balloon the static binary.

### less
`--with-regex=posix` (musl ships POSIX regex.h). Default everything else.

### htop
`--disable-hwloc --enable-unicode --disable-affinity
--disable-capabilities --disable-sensors --enable-static-link`. We also
inject `-idirafter /usr/include -idirafter /usr/include/x86_64-linux-gnu`
into the CFLAGS so musl-gcc finds the host Linux UAPI headers
(`linux/capability.h`, `asm/types.h`) without overriding musl's libc
headers. The `-idirafter` ordering is the load-bearing trick: glibc
headers in `/usr/include/sys/...` get shadowed by musl's own copies, but
the kernel UAPI under `/usr/include/linux/` and the arch-specific
`/usr/include/x86_64-linux-gnu/asm/` remain reachable.

### tmux
`--enable-utempter=no --enable-systemd=no --disable-utf8proc`. tmux's
`configure` shells out to `yacc` for `cmd-parse.y`; if the host lacks
bison/yacc, the xtask `ensure_yacc()` helper downloads byacc 20240109
into `target/host-bin/yacc` and prepends it to PATH before tmux's
configure runs.

## What proved tricky during the port

### ncurses
- Two-pass build for narrow + wide variants. `make distclean` doesn't
  fully reset all generated headers between passes; the port driver
  runs `make distclean` defensively before each pass.
- Terminfo install path: configure bakes `/usr/share/terminfo` as the
  runtime lookup directory; `make install DESTDIR=$STAGE` lands the
  compiled entries at `$STAGE/usr/share/terminfo` so the on-target
  filesystem mirrors the path the binary will query.
- m3os-term entry: compiled with `tic -x` (extended capabilities) into
  the staged terminfo db so the `BE`/`BD`/`XM`/`Ss`/`Se` extensions
  Phase 69 added land alongside the standard capability set.

### less
- Drop-in. The musl-cross-compile produced a runnable binary on the
  first attempt and the Phase 69d Track B.2 smoke passed end-to-end
  without any workarounds.

### htop
- Linux UAPI dependency: `LinuxProcessTable.c` includes
  `<linux/capability.h>` unconditionally even with
  `--disable-capabilities`, because htop calls `capget()` directly via
  the syscall ABI. Fixed by `-idirafter /usr/include` (see above).
- Runtime crash: `htop` SIGSEGVs in `initscr()` on m3OS 0.69.4. The
  binary itself is healthy (`htop --help` runs to completion) but the
  curses init path forks into a screen-management subprocess that
  trips an existing m3OS-side bug. Per the Phase 69d task doc, that
  fix lands as a back-port to whichever phase owns the offending
  contract (likely Phase 22 ANSI parser or Phase 29 PTY layer) rather
  than as a 69d change. The smoke gate accepts a binary-integrity
  probe as proof-of-port until that follow-up phase lands.

### tmux
- yacc dependency: configure invokes `yacc` for `cmd-parse.y`; the host
  was missing bison/byacc. Bootstrapped byacc 20240109 from upstream
  into `target/host-bin/` as a one-time setup step.
- Same `initscr()`-style crash as htop on the full-session lifecycle.
  Binary integrity probe (`tmux -V`) succeeds; the full
  `new-session/split-window/resize-pane/detach` flow is gated behind
  the same Phase 22/29 follow-up.

## Re-running the port build

```bash
cargo xtask port build ncurses    # narrow + wide, ~2 min cold
cargo xtask port build libevent   # ~30 s cold
cargo xtask port build less       # ~30 s cold, depends on ncurses
cargo xtask port build htop       # ~1 min cold, depends on ncurses
cargo xtask port build tmux       # ~2 min cold, depends on ncurses + libevent
```

The `cargo xtask tui-app-smoke` gate also calls
`port_build::build_phase_69d_ports()` as a precondition, so a clean
`target/` tree builds every port the first time the smoke runs.

Cache invalidation: the per-port stamp file at
`target/port-stage/<name>/.stamp` is a SHA-256 of the Portfile +
`patches/` + the `xtask/src/port_build.rs` source. Any change to any of
those forces the next `cargo xtask port build` invocation to re-run
from scratch.
