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
| `nano` | 8.7 | `nano-editor.org/dist/v8/nano-8.7.tar.xz` | `afd287aa672c48b8` | `/usr/local/bin/nano` |
| `nnn` | 5.2 | `github.com/jarun/nnn/archive/refs/tags/v5.2.tar.gz` | `f166eda5093ac8dc` | `/usr/local/bin/nnn` |
| `bsdtar` | 3.8.8 | `github.com/libarchive/.../libarchive-3.8.8.tar.gz` | `038918ea315cdd44` | `/usr/local/bin/bsdtar` |

Full SHA-256 lives in each port's `Portfile`. The host-side
`cargo xtask port build <name>` driver re-verifies the SHA on every
fetch and refuses to extract a tarball whose hash doesn't match.

## Capability coverage per app

| App | ncurses variant | terminfo capabilities exercised | termios flags used | UTF-8 blocks consumed | Nerd Font glyphs |
|---|---|---|---|---|---|
| `less` | narrow (`libncurses.a` + `libtinfo.a`) | `smcup`/`rmcup` (?1049), `clear`, `el`, `civis`/`cnorm`, `setaf`/`setab`, `cup`, `cuf`/`cub`/`cuu`/`cud` | `ICANON` off, `ECHO` off, `VMIN`=1, `VTIME`=0 | ASCII-only on the /etc/passwd smoke; UTF-8 viewing path exists for `less -R` but is not exercised in 69d | none |
| `htop` | wide (`libncursesw.a` + `libtinfow.a`) | `cup`, `setaf`/`setab` 256-color, `civis`/`cnorm`, `smcup`/`rmcup`, box-drawing | `ICANON` off, `ECHO` off, `VMIN`=1, `VTIME`=1, `SIGWINCH` handler | Box-drawing characters (U+2500..=U+257F) for CPU/mem gauges | optional theme glyphs (not exercised) |
| `tmux` | wide | full `setaf`/`setab` truecolor, `csr` (scrolling region), `cup`, mouse encoding via `XM`/`xm`, bracketed paste `BE`/`BD` | nested PTY raw mode, `IUTF8`, `IXON` per-pane, `SIGWINCH` per-pane | Box-drawing for pane dividers; UTF-8 status line if themes use it | optional Nerd Font theme glyphs |
| `nano` | wide (`libncursesw.a` + `libtinfow.a`) | `smcup`/`rmcup`, `cup`, `el`, `setaf`/`setab`, `civis`/`cnorm`, function-key sequences (`kf1`..) | `ICANON` off, `ECHO` off, `VMIN`=1, `SIGWINCH` handler | UTF-8 buffer editing (`--enable-utf8`); ASCII-only in the smoke | none |
| `nnn` | wide | `cup`, `setaf`/`setab`, `smcup`/`rmcup`, `civis`/`cnorm`, box-drawing | `ICANON` off, `ECHO` off, `VMIN`=1, `SIGWINCH` handler | Box-drawing + wide-char filename cells; ASCII-only in the smoke | optional (`O_NERD` off) |

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

### nano (Phase 105 Track E)
`--enable-utf8 --disable-nls --disable-libmagic`. Uses the same
`-idirafter` Linux-UAPI injection as htop — `nano.c` includes
`<sys/vt.h>` (console VT detection), and musl's `sys/vt.h` is a shim
over `<linux/vt.h>`. The wide-curses pair is pinned via nano's
documented pkg-config overrides `NCURSESW_CFLAGS`/`NCURSESW_LIBS`
(`-lncursesw -ltinfow`) — same narrow-tinfo hazard as htop below.

### nnn (Phase 105 Track E)
Plain Makefile (no autotools): command-line make variables override the
`?=` pkg-config probes, so `CFLAGS_CURSES`/`LDLIBS_CURSES` pin
`-lncursesw -ltinfow` directly. Knobs: `O_NORL=1` (no readline port in
the tree; also sidesteps `O_STATIC`'s `-lgpm` requirement — `-static`
rides `LDFLAGS` instead), `O_NOX11=1`, `O_NOFIFO=1` (FIFO previewer
wants `mkfifo`). `ports/util/nnn/patches/0001-inotify-optional.patch`
makes startup survive missing kernel inotify support (m3OS has none):
upstream exits if `inotify_init1` fails; the patch degrades to
no-directory-watching, which is safe because every downstream inotify
use is guarded by `inotify_wd >= 0`.

### bsdtar (Phase 105 Track E)
libarchive autotools with static `bsdtar` only:
`--enable-bsdtar=static --disable-bsdcat --disable-bsdcpio
--disable-bsdunzip --enable-static --disable-shared`. zlib is the sole
compression backend (`--with-zlib` against the in-tree zlib stage;
bz2/lzma/lz4/zstd have no ports → `--without-*`), every crypto/XML
backend is off (openssl/mbedtls/nettle/cng/xml2/expat/iconv), and
`--disable-acl --disable-xattr` because m3OS has no acl/xattr syscalls.
BLAKE2 falls back to libarchive's bundled copy (`libb2/bundled` in
`bsdtar --version`) — no extra dep. Not a curses app: no terminfo /
capability-table entry; the smoke asserts a gzip'd create → extract →
`cat` payload round-trip instead of rendered chrome.

**Static-link gotcha:** libtool silently EATS a plain `-static` from
`LDFLAGS` at link time (it reserves that spelling for libtool-library
semantics), so the first build produced a *dynamically linked* bsdtar
(`PT_INTERP=/lib/ld-musl-x86_64.so.1`) despite `-static` in the
configure LDFLAGS — and it even ran in-OS via the Phase 93 loader,
masking the regression. The fix is libtool's `-all-static` spelling,
passed on the **make** line (`make LDFLAGS="-all-static …"`), NOT to
configure — plain gcc rejects `-all-static`, so it would break every
configure link probe. `build_bsdtar` also greps the produced binary for
the `ld-musl` interp string and fails the build if it reappears.

**Serial-shell smoke gotchas (all hit while landing this arm — the
rules below apply to ANY smoke step typed at the sh0 serial console):**

1. **sh0 has NO `&&` (or `;`) chaining.** `execute_line` tokenizes the
   whole line into one argv — `cmd1 && cmd2` runs `cmd1` with `&&`,
   `cmd2`… as literal arguments. Worse, a chained sentinel like
   `cmd && echo SENTINEL` *appears* to work because the `Wait` matches
   the PTY **keystroke echo** of the typed line, not executed output —
   a silent false pass. (Several pre-existing steps in this gate use
   the `test -x … && echo …` shape and pass only by this mechanism;
   their real assertions are carried by later output-matching steps.)
   One command per line.
2. **Never type while a long-running command runs** — the keystrokes
   can vanish (the extract line typed during a multi-second bsdtar
   create was never executed). Each long stage must be
   completion-proven before the next Send; the best oracle is the
   stage's OWN output: the bsdtar steps use `-v` (`a payload.txt` on
   create, `x payload.txt` on extract — neither is a substring of any
   typed line).
3. **Execution-proof sentinels via whitespace collapse**: type
   `echo NNN  SEEDED` (double space) and wait for the single-space
   `NNN SEEDED` — argv rejoining collapses the run, so the pattern can
   only match executed output, never the keystroke echo. This is how
   the nano/nnn/bsdtar arms prove "app exited, shell is back" at every
   boundary.
4. **Keep lines short and consider `SmokeStep::SendPaced`** (5 ms/byte)
   for anything beyond ~60 chars: a ~150-char burst `write_all`
   vanished wholesale without even a keystroke echo (never diagnosed
   to the byte — shortening + pacing + the rules above made it moot).
5. `printf` and `touch` are not part of the in-OS command set — seed
   files with the proven `echo text > file` idiom.

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
- TERMTYPE / TERMTYPE2 layout mismatch from mixed wide/narrow tinfo
  linkage: with `--with-termlib` on ncurses, autoconf detects both
  `libtinfo.a` (narrow) and `libtinfow.a` (wide). htop's default
  configure search lands on `-lncursesw -ltinfo` — pulling narrow
  `setupterm` (which populates `cur_term->type`) against wide
  `termattrs_sp` (which dereferences `cur_term->type2.Strings`).
  type2.Strings stays NULL → SIGSEGV at the first `termattrs()` call,
  reading `Strings[25]` (`enter_alt_charset_mode`) at offset 0xc8 of
  a NULL pointer.
  Fix: pass `CURSES_CFLAGS` + `CURSES_LIBS` to htop's configure so
  autoconf takes the explicit `-lncursesw -ltinfow` pair and never
  probes for a narrow tinfo. With the fix in place htop renders the
  full chrome (`Tasks:` header, CPU/Mem bars, F1–F10 strip), quits
  on `q`, and emits the `:ok` sentinel — Phase 69d Track C.2 in full.

### tmux
- yacc dependency: configure invokes `yacc` for `cmd-parse.y`; the host
  was missing bison/byacc. Bootstrapped byacc 20240109 from upstream
  into `target/host-bin/` as a one-time setup step.
- Same TERMTYPE/TERMTYPE2 layout-mismatch hazard as htop. Fixed the
  same way via `LIBTINFO_LIBS=-ltinfow` + `LIBNCURSES_LIBS=-lncursesw
  -ltinfow` so tmux's autoconf takes the wide tinfo unambiguously.
- Missing client/server syscalls: tmux's client/server protocol uses
  `sendmsg`/`recvmsg` over a Unix socket plus `flock` for the socket
  lock. m3OS' syscall table does not currently dispatch numbers 46
  (sendmsg), 47 (recvmsg), or 73 (flock); the kernel logs
  `[WARN] unhandled syscall 46 …` and returns -ENOSYS. tmux therefore
  cannot start its server today. Binary-integrity probe (`tmux -V`)
  succeeds; the full session lifecycle is gated behind a follow-up
  phase that adds scatter-gather Unix-socket message I/O.

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
