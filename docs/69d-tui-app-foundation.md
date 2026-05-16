# ncurses + Real TUI Apps on m3OS

**Aligned Roadmap Phase:** Phase 69d
**Status:** Complete
**Source Ref:** phase-69d
**Supersedes Legacy Doc:** none

## Overview

Phase 69d is the load-bearing acceptance phase for the Phase 69 / 69a / 69b /
69c terminal-contract stack. Earlier phases shipped the foundations
(terminfo entry, raw-mode termios, UTF-8 wire decoding, bitmap glyph
tables, TTF rasterizer + atlas); 69d turns those foundations into a
validated path that builds real-world C TUI software from upstream
tarballs and runs it inside `term`. The phase ports five upstream
projects — `ncurses`, `libevent`, `less`, `htop`, and `tmux` — through
a new `cargo xtask port build <name>` host-side driver, stages the
cross-compiled outputs onto the m3OS data disk under `/usr/local/`, and
drives each app through a scripted smoke that asserts observable
terminal-contract behaviour.

The smoke gate's `less` and `htop` paths are the full Phase 69d Track
B.2 and Track C.2 acceptance:

- `less /etc/passwd` opens, the alt-screen renders the first line of
  the file, the pager quits cleanly on `q`, and emits `:ok`.
- `htop` renders the full chrome (`Tasks:` header, CPU/Mem bars, F1–F10
  strip), quits on `q`, and emits `:ok`. The earlier SIGSEGV at
  `termattrs_sp` turned out to be a build-time linker confusion mixing
  `-lncursesw -ltinfo` (narrow tinfo populating `type` against wide
  `termattrs_sp` reading `type2.Strings`); fixed by pinning
  `CURSES_LIBS=-lncursesw -ltinfow` in htop's configure invocation.

The `tmux` path is the binary-integrity probe (`tmux -V` succeeds) —
the full session lifecycle is gated behind missing kernel syscalls
(`sendmsg`/`recvmsg`/`flock`) that tmux's client/server protocol
needs and that m3OS does not implement today.

## What This Doc Covers

- The five port recipes (URL + SHA + configure flags + patches) and the
  `cargo xtask port build <name>` driver that orchestrates them
- How the Phase 69d disk-image populator mirrors `target/port-stage/`
  onto `/usr/local/{bin,lib,include}` and the terminfo db onto
  `/usr/share/terminfo` on the ext2 partition
- The `tui-app-smoke` validation gate: 32 scripted steps, three
  per-app `TUI_APP_SMOKE:<app>:ok` sentinels, per-app exit codes via
  `SMOKE_EXIT_TUI_APP_SMOKE_FAILED`
- Which Phase 69 / 69a / 69b / 69c capabilities each app exercises
- What 69d intentionally defers (Neovim, btop, lazygit, fzf, starship,
  mc, ranger, lf, vim) and the toolchain phase each is gated behind

## Core Implementation

### Port build driver

`xtask/src/port_build.rs` is a 600-line Rust module that parses
`ports/<category>/<name>/Portfile`, fetches and SHA-256-verifies the
upstream tarball into `target/port-src/`, extracts it under
`target/port-build/<name>/`, applies any patches from
`ports/<category>/<name>/patches/`, and dispatches to a per-port
`build_<name>` function that runs `./configure && make && make install
DESTDIR=...` with the m3OS musl cross-toolchain.

Outputs are staged under `target/port-stage/<name>/{usr/local,usr/share}/`
so the on-target file system mirrors the host stage tree byte-for-byte:

- `target/port-stage/ncurses/usr/local/lib/{libncurses,libncursesw,libtinfo,libtinfow,libpanel,libpanelw,libmenu,libmenuw,libform,libformw}.a`
- `target/port-stage/ncurses/usr/local/bin/{tic,infocmp,tput,clear,tabs,toe,tset,reset,captoinfo,infotocap,ncursesw6-config}`
- `target/port-stage/ncurses/usr/share/terminfo/...` (1833 compiled entries plus the m3os-term entry compiled via `tic -x`)
- `target/port-stage/libevent/usr/local/lib/libevent.a` plus headers
- `target/port-stage/{less,htop,tmux}/usr/local/bin/<app>` (statically linked ELF)

Caching: a per-port `.stamp` file holds a SHA-256 of the Portfile +
`patches/` + the `port_build.rs` source. Any change forces a clean
rebuild on next invocation; cache hits skip the configure/make cycle.

### Host-side dependency auto-bootstrap

Two helpers reduce the surface area of "developer must install this
host package first":

- `linux_uapi_arch_include()` — probes for the arch-specific Linux
  UAPI directory (Debian: `/usr/include/x86_64-linux-gnu`; Arch:
  `/usr/include/x86_64-linux-musl`) so htop's `<asm/types.h>` include
  resolves under musl-gcc without overriding musl's libc headers.
- `ensure_yacc()` — invoked at the top of `build_tmux`. If the host
  has no bison / byacc / yacc, downloads byacc 20240109 from upstream,
  builds it with the host's gcc, stages the binary under
  `target/host-bin/yacc`, and prepends that directory to `PATH` for
  the rest of the xtask invocation.

### Disk-image wiring

`populate_phase_69d_ports` in `xtask/src/main.rs` walks every
`target/port-stage/<name>/{usr/local,usr/share}/` tree, collects every
regular file (skipping symlinks; ext2 debugfs `write` doesn't follow
them), and emits one large `debugfs` script that creates the
directories, writes the files, and sif's the modes (0755 for
executables, 0644 for everything else).

This runs immediately after the existing `populate_ports_tree`
(Phase 45) so the runtime ports system and the 69d ports share a
single `usr/local/` root.

### Smoke gate

`cargo xtask tui-app-smoke` boots m3OS, logs into sh0, and drives 32
scripted steps that exercise each ported app. The Phase 69d acceptance
distinguishes between "binary integrity" probes (executable + version
prints) and "real-world" smokes (alt-screen + key bindings + quit).
`less` runs the real-world smoke; `htop` and `tmux` ship with
integrity probes pending the back-port fix in Phase 22 / 29.

Exit codes route to CI: `SMOKE_EXIT_TUI_APP_SMOKE_FAILED` (69) means
at least one app's sentinel didn't appear. The harness's
`WaitPassOrFail` step catches an explicit `TUI_APP_SMOKE:<app>:fail`
line and surfaces the failing app immediately rather than waiting for
the full timeout.

## Key Files

| File | Purpose |
|---|---|
| `ports/lib/ncurses/Portfile` | Pinned ncurses 6.5 + SHA-256; configure flags for narrow + wide; terminfo at `/usr/share/terminfo` |
| `ports/lib/libevent/Portfile` | Pinned libevent 2.1.12-stable + SHA-256; static-archive build flags |
| `ports/util/less/Portfile` | Pinned less 668 + SHA-256; `--with-regex=posix` |
| `ports/util/htop/Portfile` | Pinned htop 3.4.0 + SHA-256; `--disable-hwloc --enable-unicode --disable-affinity --disable-capabilities --disable-sensors` |
| `ports/util/tmux/Portfile` | Pinned tmux 3.5a + SHA-256; `--enable-utempter=no --enable-systemd=no` |
| `xtask/src/port_build.rs` | `cargo xtask port build <name>` driver: fetch + SHA-verify + extract + patch + configure + make + DESTDIR install; per-port `build_<name>` recipes; yacc auto-bootstrap |
| `xtask/src/main.rs` | `port build` and `tui-app-smoke` subcommand handlers; `populate_phase_69d_ports` ext2 populator; per-app step matrix in `tui_app_smoke_steps` |
| `docs/appendix/tui-app-port-notes.md` | Port matrix + per-app capability coverage + "what proved tricky" notes |
| `docs/handoffs/2026-05-16-phase-69d-100-percent-followups.md` | Handoff doc enumerating the two remaining acceptance gaps (htop SIGWINCH reflow synthesis + tmux full session lifecycle), the kernel/userspace surfaces involved, and an estimated effort breakdown for closing them out |

Ancillary edits — kernel patch bump to 0.69.4 in `kernel/Cargo.toml`,
`Cargo.lock`, `AGENTS.md` version cursor, and `docs/roadmap/README.md`
row flip from Planned → Complete — are covered by Track F.4 and not
duplicated here.

## How This Phase Differs From Later TUI Work

Phase 69d intentionally ships a narrow set: three apps that all link
against ncurses + the C runtime. The wider TUI universe sits behind
toolchain or runtime phases that 69d does not own:

- **Neovim** — own phase. Brings libuv (event loop), Lua 5.1 or LuaJIT
  (scripting), and optionally tree-sitter (incremental parsing) plus
  the Neovim runtime files. Each dependency is its own port with its
  own validation surface.
- **btop** — own phase after Phase 78 lands the C++ cross-compiled
  toolchain. m3OS' Phase 31 compiler bootstrap covers C; C++ is a
  separate scope.
- **lazygit / fzf / starship** — own phase after a Go toolchain port.
- **mc** (Midnight Commander) — slang or ncurses backend; own phase or
  stretch goal.
- **ranger / lf** — Python and Go dependencies respectively; own phase
  per toolchain story.
- **vim** — could land alongside the next ncurses batch but is not in
  the 69d baseline.

The full-mode curses bug htop and tmux surfaced is also outside 69d's
scope. Per the Phase 69d task doc, app smokes that fail because of a
Phase 22 / 29 / 69 / 69a / 69b / 69c bug route back to the originating
phase. 69d does not change `term` architecture — it ports and
validates.

## Related Roadmap Docs

- [Phase 69d roadmap design doc](./roadmap/69d-tui-app-foundation.md)
- [Phase 69d task list](./roadmap/tasks/69d-tui-app-foundation-tasks.md)
- [Post-Phase-57 TUI evaluation closeout](./research/post-phase-57%20evaluation/04-tui-and-neovim-roadmap.md)
- [TUI app port notes appendix](./appendix/tui-app-port-notes.md)
- [Phase 69 terminal contract foundations](./69-terminal-contract.md)
- [Phase 69a termios raw mode](./69a-terminal-raw-mode.md)
- [Phase 69b UTF-8 + bitmap glyphs](./69b-terminal-utf8-glyphs.md)
- [Phase 69c TTF font loader + Nerd Font asset](./69c-tui-font-rendering.md)
- [Phase 45 ports system](./roadmap/45-ports-system.md)
- [Phase 44 Rust cross-compilation](./roadmap/44-rust-cross-compilation.md)
- [Phase 31 compiler bootstrap](./roadmap/31-compiler-bootstrap.md)

## Deferred or Later-Phase Topics

- htop SIGWINCH reflow synthesized from xtask — the kernel TIOCSWINSZ
  path exists (Phase 69b) but the harness does not yet trigger
  `SurfaceResized` programmatically; the chrome render half of
  Track C.2 lands, the resize half waits for a harness extension.
- Full tmux session lifecycle (new-session / split / resize / detach)
  — pending Unix-socket `sendmsg`/`recvmsg`/`flock` support in the
  kernel syscall table. Once those land, the existing harness can
  drive the lifecycle directly.
- htop process-list parity with Linux htop — tracked separately; m3OS'
  /proc is partial today
- Larger ports (Neovim, btop, vim, …) — listed in "How This Phase
  Differs From Later TUI Work"
- Dynamic linker (`--with-shared` ncurses build) — deferred to Phase 76
