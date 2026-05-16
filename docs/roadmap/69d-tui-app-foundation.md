# Phase 69d - ncurses Port and First Quality TUI Apps

**Status:** Planned
**Source Ref:** phase-69d
**Depends on:** Phase 31 (Compiler Bootstrap) ✅, Phase 44 (Rust Cross-Compilation) ✅, Phase 45 (Ports System) ✅, Phase 69 (Terminal Contract Foundations), Phase 69a (Termios Raw Mode), Phase 69b (UTF-8 + Bitmap Glyphs), Phase 69c (TTF Font Infrastructure)
**Builds on:** Consumes everything Phase 69 / 69a / 69b / 69c built — terminfo entry, termios, UTF-8 codepoint rendering, Nerd Font glyphs — and turns it into the first **real-application** validation: an ncurses port and three TUI apps (`less`, `htop`, `tmux`) running natively inside `term`. This is where the terminal contract is proven end-to-end.
**Primary Components:** ports/lib/ncurses, ports/util/less, ports/util/htop, ports/util/tmux, `userspace/term`, `xtask` (port build + smoke gate)

## Milestone Goal

Phase 69d closes the loop: `less /etc/passwd` pages cleanly inside `term`, `htop` renders a full-colour process list and reflows on resize, `tmux new-session && split-window && resize-pane && detach` produces a visible split that reflows correctly. These three apps cover the pager / process-monitor / multiplexer archetypes; passing them is the load-bearing signal that the 69-series terminal stack is ready for arbitrary C TUI apps.

Neovim is **not** in 69d — it has its own dependency chain (libuv + Lua/LuaJIT or a supported alternative + tree-sitter) and gets a dedicated phase after 69d. `btop` is **not** in 69d — it is C++ and depends on the cross-compiled toolchain landing in Phase 78. `lazygit`, `fzf`, `starship` are Go binaries deferred to a Go-toolchain phase. 69d is the C-with-ncurses gateway; everything else inherits from it.

## Why This Phase Exists

ncurses is the standard library every C TUI app links against. Once ncurses is ported and `term` speaks `m3os-term`, the same source tarball that builds `less` on Linux builds it on m3OS with a stock `./configure && make install` after a port-system Portfile is written. `htop` is the same story; so is `tmux` (with libevent already on the m3OS port wishlist as a transitive dependency).

This phase is also where Phase 69's bracketed-paste, Phase 69a's raw mode, Phase 69b's UTF-8 box-drawing, and Phase 69c's Nerd Font atlas are exercised against real-world byte streams. If any of those phases left a bug, this phase will find it.

## Learning Goals

- Understand the ncurses architecture: termcap/terminfo abstraction, panel/menu/form layers, the wide-character `ncursesw` variant.
- Learn how to write a Portfile (m3OS-flavoured BSD ports) for a real upstream tarball: `./configure` flag selection, patch application, install staging.
- See how an editor sets up its terminal: `setupterm()` → `tcgetattr` → flag manipulation → `tcsetattr` → main loop reads byte-by-byte.
- Understand why pagers, process monitors, and multiplexers each stress different parts of the terminal contract (pagers: alt-screen + scrollback; htop: 256-colour SGR + resize + Unicode bars; tmux: nested PTYs + control sequences + mouse).

## Feature Scope

### ncurses port (Track A)

A full `ports/lib/ncurses/` port: upstream tarball, Portfile, patches (if any), build through the existing Phase 45 ports infrastructure. The build links against m3OS' termios (Phase 69a) and reads `/usr/share/terminfo/m/m3os-term` (Phase 69 Track A.2). Both narrow (`libncurses`) and wide (`libncursesw`) variants are built so UTF-8-aware apps work.

### `less` port (Track B)

`ports/util/less/` builds upstream `less` against the new ncurses. The boot smoke `less /etc/passwd` opens, pages with j/k, searches with `/`, and quits with `q`. Alt-screen behaviour is verified — quitting `less` restores the shell scrollback.

### `htop` port (Track C)

`ports/util/htop/` builds upstream `htop` against the new ncurses + ncursesw. Smoke: `htop` renders a full-colour process list, the F-key bindings work, and the resize behaviour (verified via `tui-smoke` synthesising a `SurfaceResized` while `htop` is running) shows the layout reflowing. Process discovery uses the existing `/proc`-equivalent path — limited in m3OS today; htop may show only the m3OS-supervised process set. That is acceptable and documented.

### `tmux` port (Track D)

`ports/util/tmux/` builds upstream tmux, pulling in `libevent` as a transitive dependency port. Smoke covers `tmux new-session`, `split-window`, `resize-pane`, and `detach`. tmux's nested PTY behaviour is the harshest test of the kernel TTY layer; passing this proves the Phase 29 + Phase 69a contract holds under load.

### Validation gate (Track E)

A new `cargo xtask tui-app-smoke` boots, drives each of the three apps through a scripted command sequence, asserts observable state (cell-grid snapshot at known points, exit status, no kernel panic), and reports per-app `:ok` / `:fail`. This gate is the load-bearing acceptance for 69d.

### Documentation (Track F)

The post-Phase-57 evaluation gets a closeout entry; a new appendix doc enumerates the ports added and the apps validated; `docs/appendix/term-escape-sequences.md` notes which features each app exercises.

## Important Components and How They Work

### `ports/lib/ncurses/`

Standard ports layout: `Portfile`, `patches/` (likely empty or near-empty given m3OS' musl + termios fidelity), `src/` (upstream tarball staged by the port build). Build outputs `libncurses.a`, `libncursesw.a`, headers, and the four small tools (`tic`, `infocmp`, `tput`, `clear`) — although `tic` is already a build-host requirement from Phase 69 Track A.2; the in-tree `tic` is an optional addition.

### `ports/util/less/`, `ports/util/htop/`, `ports/util/tmux/`

Same layout. tmux pulls in `ports/lib/libevent/` as a dependency.

### `userspace/term` — no changes expected

By 69d, `term`'s feature set should be complete. If any app surfaces a missing escape sequence, the fix lands as a back-port to Phase 69 (with a comment naming the discovery) — `term` itself does not gain new architecture in 69d. This is a port-and-validate phase, not a kernel-or-userspace-change phase.

### `xtask/src/main.rs` — `tui-app-smoke` gate

New subcommand boots, executes a scripted keystroke sequence through `term`, asserts the cell-grid state at known checkpoints. Re-uses the existing `smoke-test` PTY-driver shape.

## How This Builds on Earlier Phases

- Consumes Phase 69's terminfo entry, alt-screen, SGR, mouse, DECSCUSR, bracketed paste, SIGWINCH wiring.
- Consumes Phase 69a's termios contract for every `tcgetattr` / `tcsetattr` call each app makes.
- Consumes Phase 69b's UTF-8 + box-drawing for htop's gauges and mc-style panels (if mc is added).
- Consumes Phase 69c's Nerd Font atlas if any of the three apps uses Nerd Font glyphs (htop's themes optionally do; tmux status-line themes optionally do).
- Builds on Phase 31's compiler, Phase 44's cross-compilation story, and Phase 45's ports system. If those phases left any gaps that the three apps surface, they fail this phase rather than 69d's terminal-contract proofs.

## Implementation Outline

1. Port `ncurses`: write Portfile, run through `cargo xtask port build ncurses`, install staging produces `libncurses.a` + `libncursesw.a` + headers.
2. Smoke `infocmp m3os-term` against the installed library; assert the entry matches Phase 69's terminfo.
3. Port `less`: write Portfile, build, smoke open/page/quit against `/etc/passwd`.
4. Port `htop`: write Portfile, build, smoke render-process-list + resize-reflow.
5. Port `libevent` (tmux dependency): write Portfile, build, expose headers + static lib.
6. Port `tmux`: write Portfile, build, smoke new-session + split + resize + detach.
7. Build `cargo xtask tui-app-smoke`; drive all three apps through scripted sequences.
8. Author `docs/appendix/tui-app-port-notes.md` cataloguing the ports + each app's terminal-contract coverage.
9. Update post-Phase-57 evaluation doc with a closeout note.
10. Author `docs/69d-tui-app-foundation.md` — the aligned legacy learning doc — cross-referencing 69 / 69a / 69b / 69c and enumerating the deferred TUI apps.
11. Kernel patch bump to 0.69.4 (userspace + ports only; no kernel changes expected).

## Acceptance Criteria

- `cargo xtask port build ncurses` succeeds; `libncurses.a`, `libncursesw.a`, and `infocmp` are present in the install stage.
- `infocmp m3os-term` on m3OS prints the installed terminfo without error.
- `less /etc/passwd` opens, pages, searches, quits with primary-screen restored.
- `htop` renders a 256-colour process list (or graceful degradation if 256-colour is degraded under the current font); resize reflows the layout.
- `tmux new-session` creates a session; `split-window -h` produces a visible vertical pane split; `resize-pane -R 5` shifts the divider; `detach` returns to the shell.
- `cargo xtask tui-app-smoke` reports `:ok` for all three apps.
- No kernel panic across the full smoke run.
- `docs/69d-tui-app-foundation.md` exists as the aligned legacy learning doc, cross-referencing Phases 31 / 44 / 45 / 69 / 69a / 69b / 69c.
- Kernel version bumped to `0.69.4`; `docs/roadmap/README.md` Phase 69d row flipped from Planned → Complete.

## Companion Task List

- [Phase 69d Task List](./tasks/69d-tui-app-foundation-tasks.md)

## How Real OS Implementations Differ

- Linux distributions ship pre-built binary packages of ncurses + less + htop + tmux; m3OS builds from source through the ports tree.
- BSDs ship `bsdcurses` and `more` in base; m3OS chose `ncurses` + `less` for upstream compatibility with editor expectations.
- macOS bundles `less` with a custom build; m3OS uses the upstream `less` build straight from the tarball.

## Deferred Until Later

- **Neovim** — own phase (libuv + Lua/LuaJIT + tree-sitter).
- **btop** — own phase after Phase 78 cross-compiled toolchains (C++).
- **lazygit, fzf, starship** — own phase after a Go toolchain port.
- **mc** (Midnight Commander) — own phase or stretch goal; depends on the slang library or ncurses (mc upstream supports both).
- **ranger / lf** — Python or Go dependencies push these to their own phase.
- **vim** (the original, not nvim) — could land here if scope allows; not in baseline 69d acceptance.
