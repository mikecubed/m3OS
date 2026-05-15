# Phase 69d — ncurses Port and First Quality TUI Apps: Task List

**Status:** Planned
**Source Ref:** phase-69d
**Depends on:** Phase 31 (Compiler Bootstrap) ✅, Phase 44 (Rust Cross-Compilation) ✅, Phase 45 (Ports System) ✅, Phase 69 (Terminal Contract Foundations), Phase 69a (Termios Raw Mode), Phase 69b (UTF-8 + Bitmap Glyphs), Phase 69c (TTF Font Infrastructure)
**Goal:** Port ncurses and three quality TUI apps (`less`, `htop`, `tmux`) through the Phase 45 ports tree; drive each through a scripted smoke that asserts observable terminal-contract behaviour; prove the Phase 69 / 69a / 69b / 69c stack is ready for arbitrary C TUI software.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | ncurses port (narrow + wide variants); link against m3OS termios + terminfo | None | Planned |
| B | `less` port + smoke | A | Planned |
| C | `htop` port + smoke | A | Planned |
| D | `libevent` port + `tmux` port + smoke | A | Planned |
| E | Validation: `cargo xtask tui-app-smoke` gate | B, C, D | Planned |
| F | Documentation: post-57 eval closeout; ports appendix; kernel patch bump to 0.69.4 | E | Planned |

---

## Track A — ncurses Port

### A.1 — Write `ports/lib/ncurses/Portfile`

**File:** `ports/lib/ncurses/Portfile`
**Symbol:** N/A
**Why it matters:** Without ncurses, none of the three target apps build.

**Acceptance:**
- [ ] Upstream version pinned (current ncurses 6.x).
- [ ] SHA-256 of source tarball pinned.
- [ ] Build flags select both narrow + wide builds (`--enable-widec`), shared off (`--without-shared` until Phase 76's dynamic linker), `--with-termlib`, terminfo path `/usr/share/terminfo`.
- [ ] `tic`, `infocmp`, `tput`, `clear` build but are optional (`tic` is already a build-host requirement from Phase 69 Track A.2).
- [ ] Build target depends on no other ports.

### A.2 — Patches (if any)

**Files:** `ports/lib/ncurses/patches/*.patch`
**Symbol:** N/A
**Why it matters:** musl + m3OS termios may need small patches; record the surface area for future maintainers.

**Acceptance:**
- [ ] Any required patches are minimal and individually justified in their commit message.
- [ ] If no patches are required, this task ships an empty patches directory with a README explaining "no patches needed."

### A.3 — Verify install staging

**File:** `ports/lib/ncurses/Portfile`
**Symbol:** Install stage
**Why it matters:** Headers + static libs must end up where the consuming ports expect them.

**Acceptance:**
- [ ] `cargo xtask port build ncurses` succeeds.
- [ ] Install stage contains `libncurses.a`, `libncursesw.a`, headers under `include/`, and `infocmp` binary.
- [ ] `infocmp m3os-term` (run host-side against the install stage) prints the entry without error.

---

## Track B — `less` Port + Smoke

### B.1 — Write `ports/util/less/Portfile`

**File:** `ports/util/less/Portfile`
**Symbol:** N/A
**Why it matters:** `less` is the lightest pager; first proof that ncurses works end-to-end.

**Acceptance:**
- [ ] Upstream `less` version pinned + SHA-256 verified.
- [ ] Depends on `ports/lib/ncurses`.
- [ ] Build flags: `--with-regex=posix` (musl), default everything else.

### B.2 — Smoke: open/page/quit

**Files:**
- `xtask/src/main.rs` (smoke step)
- `userspace/tui-smoke/src/main.rs` (or a new `tui-app-smoke` shim)

**Symbol:** `cmd_less_smoke`
**Why it matters:** Proves the alt-screen contract from Phase 69 Track B works end-to-end with a real app.

**Acceptance:**
- [ ] Script: shell types `less /etc/passwd\n`, the smoke harness asserts the alt-screen is entered (Screen::active_grid_id changes), pages with `j` × 5, asserts the bottom-of-page status line appears, searches `/root\n`, asserts a highlight is emitted, quits with `q`, asserts the primary screen is restored bit-identical to the pre-launch state.
- [ ] No kernel panic, no `term` panic.

---

## Track C — `htop` Port + Smoke

### C.1 — Write `ports/util/htop/Portfile`

**File:** `ports/util/htop/Portfile`
**Symbol:** N/A
**Why it matters:** htop exercises 256-colour SGR, UTF-8 box-drawing, and SIGWINCH reflow in one app.

**Acceptance:**
- [ ] Upstream `htop` version pinned + SHA-256 verified.
- [ ] Depends on `ports/lib/ncurses` (wide variant).
- [ ] Build flags: `--disable-hwloc`, `--disable-unicode` set to false (htop's `--enable-unicode` requires ncursesw), `--enable-affinity` off (no SMP affinity API yet in m3OS).
- [ ] /proc-equivalent path is whatever m3OS provides today; if htop's process discovery is empty, the app must still render the chrome (CPU/mem bars, header) — that is acceptable scope.

### C.2 — Smoke: render + resize

**File:** `userspace/tui-smoke/src/main.rs` or `xtask`
**Symbol:** `cmd_htop_smoke`
**Why it matters:** Resize-while-running is the SIGWINCH gate.

**Acceptance:**
- [ ] Shell types `htop\n`, the smoke harness asserts the first frame is composed (cell grid has the htop header glyphs).
- [ ] Synthesizes a `SurfaceResized` to a smaller geometry; asserts the second frame's cell grid reflects the new dimensions (header truncated or wrapped per htop's policy).
- [ ] Sends `q` to quit; asserts return to shell.

---

## Track D — `tmux` Port + Smoke

### D.1 — `libevent` port

**File:** `ports/lib/libevent/Portfile`
**Symbol:** N/A
**Why it matters:** tmux depends on libevent; without it, tmux does not build.

**Acceptance:**
- [ ] Upstream `libevent` version pinned + SHA-256 verified.
- [ ] Build flags: `--disable-openssl`, `--disable-samples`, `--disable-debug-mode`.
- [ ] Depends on no other ports.
- [ ] Install stage produces `libevent.a` + headers.

### D.2 — Write `ports/util/tmux/Portfile`

**File:** `ports/util/tmux/Portfile`
**Symbol:** N/A
**Why it matters:** Multiplexer = the harshest test of the kernel TTY/PTY stack.

**Acceptance:**
- [ ] Upstream `tmux` version pinned + SHA-256 verified.
- [ ] Depends on `ports/lib/ncurses` (wide) and `ports/lib/libevent`.
- [ ] Build flags: `--enable-utempter=no`, `--enable-systemd=no`.

### D.3 — Smoke: session lifecycle

**File:** `xtask/src/main.rs` (`tui_app_smoke`)
**Symbol:** `cmd_tmux_smoke`
**Why it matters:** Nested PTYs + control sequences + alt-screen + mouse — if any prior phase missed something, this is where it shows up.

**Acceptance:**
- [ ] Shell types `tmux new-session -d -s smoke 'sleep 60'\n`; harness asserts the session exists (probe via `tmux list-sessions`).
- [ ] `tmux split-window -h -t smoke\n`; harness asserts the cell grid shows a vertical divider.
- [ ] `tmux resize-pane -t smoke -R 5\n`; harness asserts the divider column shifted.
- [ ] `tmux detach -s smoke\n` (or `tmux kill-session -t smoke`); harness asserts return to plain shell prompt.

---

## Track E — Validation Gate

### E.1 — `cargo xtask tui-app-smoke`

**File:** `xtask/src/main.rs`
**Symbol:** `tui_app_smoke` subcommand
**Why it matters:** Single command runs all three app smokes — the load-bearing acceptance for the phase.

**Acceptance:**
- [ ] `cargo xtask tui-app-smoke` boots, runs less / htop / tmux smokes in sequence, reports per-app `:ok` / `:fail`, exits non-zero if any failed.
- [ ] Total runtime under 5 min on a developer laptop.
- [ ] Wired into the pre-push hook behind `M3OS_TUI_APP_REGRESSION=1`.

---

## Track F — Documentation and Release

### F.1 — Post-Phase-57 evaluation closeout

**File:** `docs/research/post-phase-57 evaluation/04-tui-and-neovim-roadmap.md`
**Symbol:** N/A
**Why it matters:** The eval doc that scoped Phase 69 must record what landed where.

**Acceptance:**
- [ ] Doc gains a "Closeout" section enumerating which gaps landed in 69 / 69a / 69b / 69c / 69d.
- [ ] Neovim, btop, lazygit, fzf, starship, mc, ranger, lf, vim each have a one-line forward-pointer to their respective deferral phase.

### F.2 — Ports + apps appendix

**File:** `docs/appendix/tui-app-port-notes.md` (new)
**Symbol:** N/A
**Why it matters:** Future port authors need a worked-example of "what Phase 69 features each app uses" so they know which `term` capability to verify first.

**Acceptance:**
- [ ] Table: app | ncurses variant | terminfo capabilities exercised | termios flags used | UTF-8 blocks consumed | Nerd Font glyphs (if any).
- [ ] One paragraph per app summarising what proved tricky during the port.

### F.3 — Kernel patch bump to 0.69.4

**Files:**
- `kernel/Cargo.toml`
- `Cargo.lock`
- `AGENTS.md`
- `docs/roadmap/README.md`

**Symbol:** `version`
**Why it matters:** Patch bump per phase; even though 69d is userspace+ports, the project convention still bumps.

**Acceptance:**
- [ ] `kernel/Cargo.toml` `version = "0.69.4"`.
- [ ] `Cargo.lock` regenerated.
- [ ] `AGENTS.md` version cursor updated.
- [ ] `cargo xtask check` passes.

---

## Documentation Notes

- 69d is intentionally narrow: three apps, one library. The wider TUI universe (nvim, btop, lazygit, …) lives in dedicated post-69d phases, each with its own toolchain or runtime dependency.
- If an app smoke fails because of a Phase 69 / 69a / 69b / 69c bug, the fix lands as a back-port to the originating phase (with a comment naming the discovery). 69d itself does not change `term` architecture — it ports and validates.
- htop's process-discovery surface is constrained by what m3OS exposes today (`/proc` is partial); a fully-populated process list is not a 69d acceptance criterion. Rendering the chrome correctly is.
- tmux is the harshest test in the set. If it surfaces a kernel TTY/PTY bug under nested-PTY load, the bug fixes route to Phase 22 / 29 follow-ups rather than to 69d.
