# TUI and Neovim Roadmap

**Date:** 2026-04-29
**Baseline:** `main` at `449fc05165868a22e756038b50ccc55981291fcd`
**Question:** What would it take to support Omarchy-like TUI workflows and Neovim?

## Short answer

TUI applications are the best near-term route to a useful developer workstation. They need far less graphical compatibility than browsers, and they align with what m3OS already has: PTYs, shell, ANSI parsing, userspace services, ports, networking, and a graphical terminal.

Neovim is still not "easy." It requires a mature terminal contract, a staged C toolchain/dependency story, libuv, Lua/LuaJIT or a supported alternative, tree-sitter/runtime files if enabled, and enough POSIX behavior for the event loop. But it is much more plausible than Chromium or Firefox.

## Current m3OS baseline

| Component | Status on `main` | TUI relevance |
|---|---|---|
| PTY subsystem | Present from Phase 29 and used by `term`. | Required for shells, tmux, nvim, job control. |
| ANSI parser | Existing Phase 22b work reused by `term`. | Required for screen applications. |
| Graphical terminal | `userspace/term` exists with PTY, screen, render, input, bell modules. | First path for local TUI apps. |
| Shell/coreutils | Built-in shell, coreutils, ports baseline. | Enough for simple workflows. |
| Signals | Present from earlier phases. | Required for Ctrl-C, job control, resize, child lifecycle. |
| Ports system | BSD-style source ports exist. | Good model, but current ports are small and TCC-oriented. |
| Large toolchains | Planned in Phase 59. | Needed for Neovim-class software. |

## Phase 57a impact

The active/planned Phase 57a scheduler rewrite is directly relevant to TUI readiness. Neovim, tmux, shells, fuzzy finders, and log viewers spend most of their time blocked in PTY reads, timers, poll/select/epoll, child waits, and IPC. The planned v2 block/wake path should make those waits reliable by replacing the old lost-wake machinery with `block_current_until`, a rewritten `wake_task`, and full call-site migration.

If 57a lands as planned, this roadmap can assume terminal apps are debugging terminal semantics and POSIX gaps, not an underlying scheduler liveness bug. If it does not, Neovim-class work should stay behind a "demo only" label because editor event loops can appear flaky for reasons unrelated to Neovim or `term`.

The expected timeout-unit fixes also matter: TUI event loops rely on short sleeps and poll timeouts for cursor blink, redraw coalescing, process monitoring, and responsiveness. Getting those units right is part of making `term` feel usable.

## Terminal compatibility gaps

Before `nvim`, `tmux`, `btop`, or `lazygit` feel good, `term` needs a stronger contract:

| Feature | Why it matters |
|---|---|
| Raw/cbreak termios modes | Editors and shells need byte-accurate input without cooked-line surprises. |
| Alternate screen buffer | Full-screen TUIs should not destroy shell scrollback. |
| Cursor modes and full SGR set | Editors rely on cursor style, visibility, colors, attributes. |
| 256-color and truecolor | Omarchy-like themes require more than basic ANSI colors. |
| UTF-8 decoding and font coverage | Modern CLIs assume Unicode, box drawing, icons, and Nerd Font glyphs. |
| Resize handling and `SIGWINCH` | Editors and tmux must react to window size changes. |
| Bracketed paste | Prevents pasted text from being interpreted as typed commands. |
| Mouse reporting | Needed by many terminal UIs and optional Neovim workflows. |
| Scrollback, selection, copy/paste | Required for daily terminal use. |
| Terminfo entry | Apps need to know what escape sequences `term` supports. |
| Keyboard protocol clarity | Function keys, arrows with modifiers, Alt, Ctrl, and Super translations must be stable. |
| Latency and idle behavior | TUI apps are extremely sensitive to input/render lag. |

The most important deliverable is a published `TERM=m3os` terminfo entry plus tests that assert the terminal actually implements it.

## Neovim bring-up

The official Neovim docs describe a CMake/Ninja-based build and note that bundled dependencies include libuv, LuaJIT, utf8proc, tree-sitter parsers, and related runtime pieces. They also document a static Linux build path using musl.

That suggests the right m3OS plan:

### Stage 1: Host-cross static Neovim

- Build Neovim on the host for the m3OS userspace target.
- Prefer static linking at first.
- Use bundled dependencies where possible.
- Disable optional providers and integrations:
  - Python provider
  - Ruby provider
  - Node provider
  - clipboard integration until compositor clipboard exists
  - external LSP until networking and process/runtime behavior mature
- Consider PUC Lua or a no-JIT LuaJIT mode if executable memory/JIT behavior is not ready.
- Stage `nvim` plus runtime files into the image under a documented prefix.

### Stage 2: Minimal runtime contract

Acceptance should be small and concrete:

- `nvim /tmp/test.txt` opens inside `term`.
- Insert text, save, quit.
- Ctrl-C and escape sequences behave correctly.
- Alternate screen restores shell state.
- Resize event updates layout.
- Syntax highlighting works for one file type.
- Crash/exit returns control to the shell without breaking the PTY.

### Stage 3: Developer workflow

Add:

- `ripgrep`
- `fd`
- `fzf`
- `git` after Phase 59/60
- `tree-sitter` parsers if stable
- theme files matching the GUI theme
- optional LSP after language runtimes exist

## TUI application tiers

### Tier 0: m3OS-native terminal apps

Build or improve native apps first:

- shell history/completion
- `edit`
- `top`/process viewer
- service status viewer
- log viewer
- package/ports viewer

These are controlled targets and good tests for the terminal.

### Tier 1: small portable Unix TUIs

Port:

- `less`
- `nano` or `micro` as an easier editor target
- `fzf`
- `ripgrep`
- `fd`
- `htop`-like or `btop`-like process monitor, possibly native first

Needs:

- termios
- libc and curses/terminfo story
- filesystem traversal
- pipes
- signals

### Tier 2: multiplexing and git workflow

Port:

- `tmux`
- `lazygit`
- `tig`

Needs:

- robust PTY behavior
- pseudo-terminal nesting
- sockets
- signals/job control
- resize propagation
- git from Phase 59/60

### Tier 3: Omarchy-like terminal menus

Build native menu clients that can launch either GUI or TUI flows:

- app launcher
- package install/remove UI
- theme picker
- audio/network controls
- keybinding browser
- system/session menu

This gives the Omarchy feel without depending on Arch package tools or Wayland UI utilities.

## Porting strategy

| Strategy | Use when | Notes |
|---|---|---|
| Native Rust app | Behavior is small and OS-specific. | Best for service/log/package/status tools. |
| Host cross-build static binary | Third-party app has manageable C/Rust deps. | Best initial path for Neovim and common TUIs. |
| Ports system build in guest | App is small enough for TCC or future Clang. | Good validation, but slow and resource-limited. |
| Binary package staging | App is large but stable. | Future package system direction. |

## Work items before Neovim

1. Publish terminal compatibility spec:
   - `TERM=m3os`
   - supported escape sequences
   - color depth
   - key encodings
   - resize and mouse modes

2. Harden `term`:
   - alternate screen
   - scrollback
   - 256-color/truecolor
   - UTF-8
   - bracketed paste
   - mouse reporting
   - resize propagation

3. Strengthen PTY/TTY:
   - raw mode correctness
   - `SIGWINCH`
   - process group/job control audit
   - nested PTY behavior for tmux

4. Build dependencies:
   - CMake/Ninja host-side for staging
   - libuv
   - Lua/LuaJIT choice
   - utf8proc
   - tree-sitter optional
   - terminfo/unibilium choice

5. Stage runtime files:
   - `/usr/bin/nvim`
   - `/usr/share/nvim/runtime`
   - parser directory if tree-sitter enabled
   - config/theme location

## TUI regression suite

Add smokes for:

- `term` shows shell prompt and echoes a typed command.
- Ctrl-C interrupts `sleep`.
- Alternate screen app enters/exits cleanly.
- Resize event changes reported rows/cols.
- 256-color palette sample renders with expected cells.
- UTF-8 box drawing renders stable glyph cells.
- Bracketed paste round trip.
- `nvim` open/write/quit once staged.
- `tmux` nested PTY attach/detach once staged.

## Sources

- Neovim build docs: <https://neovim.io/doc/build/>
- Omarchy manual: <https://learn.omacom.io/2/the-omarchy-manual>
- Omarchy package list: <https://raw.githubusercontent.com/basecamp/omarchy/dev/install/omarchy-base.packages>
- Existing local docs: `docs/29-pty-subsystem.md`
- Existing local docs: `docs/22b-ansi-escape.md`
- Existing local docs: `docs/57-audio-and-local-session.md`
