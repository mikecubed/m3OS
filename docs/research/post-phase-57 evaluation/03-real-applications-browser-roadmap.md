# Real Applications and Browser Roadmap

**Date:** 2026-04-29
**Baseline:** `main` at `4c72e34` (`Phase 57a: scheduler block/wake protocol rewrite`, merged 2026-04-29)
**Question:** What would it take to run real applications, including browsers, on top of the post-Phase 57 system?

## Short answer

Real applications arrive in layers:

1. Native m3OS apps using the existing display protocol.
2. TUI apps inside `term`.
3. Simple software-rendered Wayland clients through a future `wl_shm` adapter.
4. Large desktop apps and browsers after a much larger Linux-compatible runtime, graphics, and packaging stack exists.

A real browser is not close to Phase 57. A browser is less a single application than a stress test for the whole OS: process model, shared memory, TLS/DNS, fonts, file watching, threads, timers, mmap, dynamic linking, C++ runtime, rendering, sandboxing, accessibility, IPC, GPU or software GL, and a mature toolkit surface.

## Application classes

| Class | Examples | Near-term path | Blocking gaps |
|---|---|---|---|
| Native m3OS GUI app | launcher, bar, settings, file viewer, image viewer | Build against the Phase 56 native protocol. | Need small toolkit, event loop, widgets, file dialogs, clipboard. |
| Native game/demo | DOOM-like, framebuffer toys | Existing graphics proof plus display client surface. | Input, audio, session integration, surface lifecycle. |
| TUI app | `nvim`, `tmux`, `fzf`, `btop`, `lazygit` | Run in `term` after terminal compatibility work and host cross-build staging. | terminfo, raw mode, resize, UTF-8, signals, pipes, filesystem/runtime libs. |
| Simple Wayland app | `foot`, `fuzzel`, `mako`, `waybar` in software mode | Add a `wl_shm` Wayland server adapter to the native compositor. | libwayland, xkbcommon, pixman, dynamic linking or static port discipline, shared memory and FD passing. |
| Toolkit GUI app | GTK/Qt/Electron apps | Requires Wayland/X11 plus toolkit stack. | Huge POSIX/Linux ABI and graphics/runtime gaps. |
| Browser | Chromium, Firefox | Long-term multi-phase program. | Toolchain, runtime, graphics, security, networking, fonts, process model, sandbox, packaging. |

## Browser requirements by upstream baseline

Chromium's Linux build docs currently require an x86-64 machine, at least 8 GB RAM with more than 16 GB highly recommended, at least 100 GB disk, Git, Python 3.9+, `depot_tools`, Clang, libc++, GN/Siso/Ninja-style build infrastructure, and a broad Linux desktop dependency set. The Arch dependency list includes GTK3, NSS/NSPR, ALSA, GLib, Cairo, D-Bus, freetype, Xvfb tooling, and more.

Firefox's Linux build docs currently require 64-bit Linux, 4 GB RAM minimum with 8 GB recommended, at least 30 GB disk, Python 3.9+, Git, bootstrap tooling, and `./mach build`. The actual runtime also pulls in the modern browser stack: Rust/C++ code, WebRender/graphics, networking, process isolation, fonts, media, and toolkit integration.

Those upstream expectations are useful because they show the shape of the missing substrate. m3OS does not need to match every Linux detail, but it needs equivalent capabilities.

## Browser substrate gaps

### Runtime and ABI

Required before a serious browser port:

- Dynamic linker and shared library loading, or an explicitly supported all-static strategy for a very large dependency graph.
- C++ runtime (`libc++` or `libstdc++`), exceptions/RTTI policy, TLS, constructors/destructors.
- POSIX-ish process model details browsers assume: fork/exec/env, signals, pipes, socketpair, mmap, shared memory, file locks, robust timers, monotonic clocks.
- Threading and synchronization compatible with browser runtimes.
- `epoll`, `eventfd`, `timerfd`, `signalfd`, `inotify` or substitutes.
- `/dev`, `/proc`, `/sys`, `/run`, `/tmp`, user runtime dirs, fonts/config/data directories.
- Large virtual address and memory pressure behavior.

### Graphics and input

Required before unmodified modern browser UI:

- Wayland or X11 client protocol, or a native browser backend. Native backend is technically possible but not realistic as a first browser path.
- Shared-memory buffers at minimum; DMA-BUF/DRM/KMS/GBM/EGL/GLES for mainstream GPU/toolkit paths.
- libxkbcommon-equivalent key mapping, text input, clipboard, cursor, pointer, touch/scroll handling.
- Fontconfig/freetype/harfbuzz/pango-class text stack or equivalents.
- Hardware acceleration eventually; software rendering can bring up an early path but will be slow.

### Networking and security

Required before a browser is useful:

- DNS resolver with config.
- TLS trust store and certificate validation.
- Strong entropy path.
- TCP stability under high connection count.
- HTTP/2/3 dependencies if using browser-native stacks.
- Credential storage policy.
- Browser sandbox policy or a documented no-sandbox development mode.

### Build and packaging

Required before this is reproducible:

- Host cross-build pipeline for C/C++/Rust projects at browser scale.
- Python, Clang/LLD, GN/Siso or CMake/Ninja as needed.
- Package staging for thousands of runtime files.
- Debug symbol stripping and split artifacts.
- Disk images much larger than the current toy-OS baseline.
- A repeatable update strategy.

## Practical path to real apps

### Stage 1: Native first-party GUI apps

Build small native apps using the m3OS display protocol:

- Launcher
- Bar
- Notifications
- Settings
- File browser
- Text/image viewer
- Package manager frontend

This proves the compositor, toolkit, event loop, and app lifecycle without dragging in Linux compatibility.

### Stage 2: TUI application ecosystem

Bring up terminal-first tools:

- `nvim`
- `tmux`
- `fzf`
- `ripgrep`
- `less`
- `btop`
- `lazygit` after git and terminal compatibility mature

This gives an Omarchy-like developer workflow much sooner than a browser.

### Stage 3: `wl_shm` compatibility adapter

Add a Wayland server backend that supports a constrained software path:

- `wl_compositor`
- `wl_shm`
- `xdg-shell`
- `wlr-layer-shell` equivalent if targeting Waybar/Mako-like clients
- keyboard/pointer events via m3OS input
- shared-memory buffer import into the native compositor

This can unlock simple Wayland clients without Mesa or GPU work. It still needs libwayland, xkbcommon, pixman, and runtime compatibility.

### Stage 4: Toolkit compatibility

Port enough GTK/Qt dependencies for software-rendered apps:

- GLib/GObject
- Cairo/pixman
- Pango/harfbuzz/fontconfig/freetype
- D-Bus or stubs
- icon/theme/data lookup paths
- file dialog and portal story

This is the point where "normal desktop apps" start becoming possible, but it is still not browser-grade.

### Stage 5: Browser-specific program

Choose one browser target and write a dedicated phase plan:

- Firefox may be more tractable than Chromium if the project wants Rust/C++ plus `mach`, but it still expects a mature Linux-like runtime.
- Chromium is likely the better compatibility target for Omarchy parity, but its build and runtime stack is larger.
- A text browser (`links`, `lynx`, `w3m`) is a much better early internet milestone than Chromium/Firefox.
- A remote-browser client or VNC/RDP-style viewer could provide "browser access" earlier, but it would not mean m3OS runs a browser locally.

## What "browser on m3OS" should mean

Define the target before implementation:

| Target | Meaning | Cost |
|---|---|---|
| Text browser | Runs in `term`, basic HTTP/HTML. | Low-medium after networking/TUI work. |
| Remote browser client | m3OS shows a remote session/browser. | Medium, network/display integration; not local app support. |
| Minimal local webview | Small native renderer for simple pages. | High if JS/CSS matter; limited utility if they do not. |
| Software Wayland browser | Firefox/Chromium render through CPU paths. | Very high; requires runtime/toolkit/Wayland stack. |
| Full desktop browser | Modern browser with GPU and sandbox. | Multi-year, post-GPU/DRM/Mesa-level work. |

## Dependencies on existing roadmap

| Roadmap area | Relevance |
|---|---|
| Phase 57a | Merged. It stabilizes blocked waits by replacing the v1 lost-wake protocol, deleting the lost-wake machinery, fixing timeout units, adding watchdog/trace diagnostics, and improving `audio_server`, `serial_stdin_feeder`, and `syslogd` behavior. It is necessary but not sufficient for reliable apps: the real-hardware GUI gate still fails from cooperative-scheduling starvation. |
| Phase 57b / 57c | Planned in `docs/appendix/preemptive-multitasking.md`. Needed before GUI apps or browser-class workloads can assume that CPU-bound user code, logging bursts, or kernel busy-waits will not monopolize a core and starve event loops. |
| Phase 59 | Brings git, Python, Clang/LLD, and larger staged toolchains. This is a prerequisite for serious third-party ports. |
| Phase 60 | DNS, HTTPS trust, git remote, GitHub CLI. Browser networking builds on the same trust and resolver story. |
| Phase 61 | Node.js and npm. Useful for Electron/JS ecosystem understanding, but Electron itself is browser-stack sized. |
| Wayland gap work | Needed for unmodified graphical apps unless m3OS builds a native toolkit ecosystem only. |
| Dynamic linking | Not currently owned by a phase but blocks most modern desktop stacks. |

## Sources

- Chromium Linux build instructions: <https://chromium.googlesource.com/chromium/src/+/main/docs/linux/build_instructions.md>
- Firefox Linux build instructions: <https://firefox-source-docs.mozilla.org/setup/linux_build.html>
- Omarchy package list: <https://raw.githubusercontent.com/basecamp/omarchy/dev/install/omarchy-base.packages>
- Existing local analysis: `docs/appendix/gui/wayland-gap-analysis.md`
- Existing local analysis: `docs/appendix/gui/tiling-compositor-path.md`
- Existing roadmap: `docs/roadmap/59-cross-compiled-toolchains.md`
- Existing roadmap: `docs/roadmap/60-networking-and-github.md`
- Existing roadmap: `docs/roadmap/61-nodejs.md`
