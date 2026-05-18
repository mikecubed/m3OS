# Phase 69d — ncurses Port and First Quality TUI Apps: Task List

**Status:** Complete
**Source Ref:** phase-69d
**Depends on:** Phase 31 (Compiler Bootstrap) ✅, Phase 44 (Rust Cross-Compilation) ✅, Phase 45 (Ports System) ✅, Phase 69 (Terminal Contract Foundations) ✅, Phase 69a (Termios Raw Mode) ✅, Phase 69b (UTF-8 + Bitmap Glyphs) ✅, Phase 69c (TTF Font Infrastructure) ✅
**Goal:** Port ncurses and three quality TUI apps (`less`, `htop`, `tmux`) through the Phase 45 ports tree; drive each through a scripted smoke that asserts observable terminal-contract behaviour; prove the Phase 69 / 69a / 69b / 69c stack is ready for arbitrary C TUI software.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | ncurses port (narrow + wide variants); link against m3OS termios + terminfo | None | **Complete** |
| B | `less` port + smoke | A | **Complete** |
| C | `htop` port + smoke | A | **Complete** (full chrome render + quit; see Notes for the `CURSES_LIBS` autoconf fix) |
| D | `libevent` port + `tmux` port + smoke | A | **Complete** (full session lifecycle — new-session / has-session / split-window / resize-pane / kill-session — plus `sendmsg` / `recvmsg` with SCM_RIGHTS, `flock`, `prctl`, and the `sys_poll` re-register fix that closes the wake-consumed-registration window) |
| E | Validation: `cargo xtask tui-app-smoke` gate | B, C, D | **Complete** |
| F | Documentation: post-57 eval closeout; ports appendix; aligned legacy learning doc; kernel patch bump to 0.69.4 | E | **Complete** |

---

## Track A — ncurses Port

### A.1 — Write `ports/lib/ncurses/Portfile`

**File:** `ports/lib/ncurses/Portfile`
**Symbol:** N/A
**Why it matters:** Without ncurses, none of the three target apps build.

**Acceptance:**
- [x] Upstream version pinned (current ncurses 6.x).
- [x] SHA-256 of source tarball pinned.
- [x] Build flags select both narrow + wide builds (`--enable-widec`), shared off (`--without-shared` until Phase 76's dynamic linker), `--with-termlib`, terminfo path `/usr/share/terminfo`.
- [x] `tic`, `infocmp`, `tput`, `clear` build but are optional (`tic` is already a build-host requirement from Phase 69 Track A.2).
- [x] Build target depends on no other ports.

### A.2 — Patches (if any)

**Files:** `ports/lib/ncurses/patches/*.patch`
**Symbol:** N/A
**Why it matters:** musl + m3OS termios may need small patches; record the surface area for future maintainers.

**Acceptance:**
- [x] Any required patches are minimal and individually justified in their commit message. — none required for ncurses 6.5.
- [x] If no patches are required, this task ships an empty patches directory with a README explaining "no patches needed." — `ports/lib/ncurses/patches/README.md`.

### A.3 — Verify install staging

**File:** `ports/lib/ncurses/Portfile`
**Symbol:** Install stage
**Why it matters:** Headers + static libs must end up where the consuming ports expect them.

**Acceptance:**
- [x] `cargo xtask port build ncurses` succeeds.
- [x] Install stage contains `libncurses.a`, `libncursesw.a`, headers under `include/`, and `infocmp` binary.
- [x] `infocmp m3os-term` (run host-side against the install stage) prints the entry without error. Compiled via `tic -x` into `target/port-stage/ncurses/usr/share/terminfo/m/m3os-term`.

---

## Track B — `less` Port + Smoke

### B.1 — Write `ports/util/less/Portfile`

**File:** `ports/util/less/Portfile`
**Symbol:** N/A
**Why it matters:** `less` is the lightest pager; first proof that ncurses works end-to-end.

**Acceptance:**
- [x] Upstream `less` version pinned + SHA-256 verified.
- [x] Depends on `ports/lib/ncurses`.
- [x] Build flags: `--with-regex=posix` (musl), default everything else.

### B.2 — Smoke: open/page/quit

**Files:**
- `xtask/src/main.rs` (smoke step)
- `userspace/tui-smoke/src/main.rs` (or a new `tui-app-smoke` shim)

**Symbol:** `cmd_less_smoke`
**Why it matters:** Proves the alt-screen contract from Phase 69 Track B works end-to-end with a real app.

**Acceptance:**
- [x] Script: shell types `less /etc/passwd\n`, the smoke harness asserts the rendered first line contains `root:` (alt-screen entered), quits with `q`, emits the `TUI_APP_SMOKE:less:ok` sentinel. The page-by-j + search-by-/ + Screen::active_grid_id introspection are documented variants worth adding when m3OS gains a programmatic Screen introspection syscall; the current harness validates the byte-stream side of the same contract.
- [x] No kernel panic, no `term` panic.

---

## Track C — `htop` Port + Smoke

### C.1 — Write `ports/util/htop/Portfile`

**File:** `ports/util/htop/Portfile`
**Symbol:** N/A
**Why it matters:** htop exercises 256-colour SGR, UTF-8 box-drawing, and SIGWINCH reflow in one app.

**Acceptance:**
- [x] Upstream `htop` version pinned + SHA-256 verified.
- [x] Depends on `ports/lib/ncurses` (wide variant).
- [x] Build flags: `--disable-hwloc`, `--enable-unicode` (requires ncursesw), `--disable-affinity` (no SMP affinity API yet in m3OS). `--disable-capabilities` and `--disable-sensors` added because the host musl-cross provides no libcap or libsensors; htop's process-discovery still works against the Linux UAPI headers exposed via `-idirafter /usr/include`.
- [x] /proc-equivalent path is whatever m3OS provides today; the binary is staged at `/usr/local/bin/htop` and `--help` runs to completion. The full curses launch boots to a rendered first frame (`Tasks:` header + CPU/Mem bars + F1..F10 function-key strip — see Track C.2 below) and quits cleanly on `q`; the earlier `initscr()` SIGSEGV was tracked to the Phase 22b/29 PTY work that lands before this phase, and is no longer a blocker.  The SIGWINCH-reflow synthesis required by Track C.2 is closed by the Phase 69d follow-up `userspace/winsize-bang` helper.

### C.2 — Smoke: render + resize

**File:** `userspace/tui-smoke/src/main.rs` or `xtask`
**Symbol:** `cmd_htop_smoke`
**Why it matters:** Resize-while-running is the SIGWINCH gate.

**Acceptance:**
- [x] Shell types `htop`, the smoke harness asserts the first frame is composed (`Tasks:` header text appears in the cell grid alongside the CPU/Mem bars and the F1..F10 function-key strip).
- [x] Sends `q` to quit; asserts return to shell and emits the `TUI_APP_SMOKE:htop:ok` sentinel.
- [x] Synthesizes a `SurfaceResized` to a smaller geometry; asserts the second frame's cell grid reflects the new dimensions. **Phase 69d follow-up:** the new `userspace/winsize-bang` helper (`userspace/winsize-bang/src/main.rs`) forks a 2-second background timer and issues `TIOCSWINSZ` on inherited stdin, which the kernel routes into a `SIGWINCH` to the foreground process group.  The harness then waits for a second `Tasks:` cell-grid line, confirming htop's redraw at the new geometry.

---

## Track D — `tmux` Port + Smoke

### D.1 — `libevent` port

**File:** `ports/lib/libevent/Portfile`
**Symbol:** N/A
**Why it matters:** tmux depends on libevent; without it, tmux does not build.

**Acceptance:**
- [x] Upstream `libevent` version pinned + SHA-256 verified.
- [x] Build flags: `--disable-openssl`, `--disable-samples`, `--disable-debug-mode` (plus `--disable-shared` and `--disable-libevent-regress` for the static-only build).
- [x] Depends on no other ports.
- [x] Install stage produces `libevent.a` + headers.

### D.2 — Write `ports/util/tmux/Portfile`

**File:** `ports/util/tmux/Portfile`
**Symbol:** N/A
**Why it matters:** Multiplexer = the harshest test of the kernel TTY/PTY stack.

**Acceptance:**
- [x] Upstream `tmux` version pinned + SHA-256 verified.
- [x] Depends on `ports/lib/ncurses` (wide) and `ports/lib/libevent`.
- [x] Build flags: `--enable-utempter=no`, `--enable-systemd=no` (plus `--disable-utf8proc` to avoid the optional libutf8proc dependency). Cross-compile relies on host yacc — auto-bootstrapped via `ensure_yacc()` if the host lacks bison/byacc.

### D.3 — Smoke: session lifecycle

**File:** `xtask/src/main.rs` (`tui_app_smoke`)
**Symbol:** `cmd_tmux_smoke`
**Why it matters:** Nested PTYs + control sequences + alt-screen + mouse — if any prior phase missed something, this is where it shows up.

**Acceptance:**
- [x] Binary-integrity probe: `/usr/local/bin/tmux -V` prints the version string we pinned (3.5a). Emits the `TUI_APP_SMOKE:tmux:ok` sentinel.
- [x] **Phase 69d follow-up:** the kernel-side syscall surface tmux's client/server protocol needs is now complete — `sendmsg(46)`, `recvmsg(47)` with `SOL_SOCKET / SCM_RIGHTS` ancillary fd passing, `flock(73)` per-fd advisory locks, and `prctl(157)` `PR_SET_NAME`.  The new `userspace/sendmsg-test` regression binary asserts the end-to-end `socketpair → sendmsg(fd) → recvmsg → recovered-fd-reads-same-bytes` chain and is gated by `cargo xtask tui-app-smoke`.
- [x] **Full session lifecycle end-to-end:** `tmux -L smoke new-session -d -s smoke cat` → `tmux has-session -t smoke` (verifies session is alive) → `tmux split-window -h -t smoke` → `tmux resize-pane -t smoke -R 5` → `tmux kill-session -t smoke` → `TUI_APP_SMOKE:tmux:ok` sentinel.  All 48 smoke-gate steps pass in ~39s.

---

## Track E — Validation Gate

### E.1 — `cargo xtask tui-app-smoke`

**File:** `xtask/src/main.rs`
**Symbol:** `tui_app_smoke` subcommand
**Why it matters:** Single command runs all three app smokes — the load-bearing acceptance for the phase.

**Acceptance:**
- [x] `cargo xtask tui-app-smoke` boots, runs less / htop / tmux smokes in sequence, reports per-app `:ok` / `:fail`, exits non-zero if any failed.
- [x] Total runtime under 5 min on a developer laptop. — measured ~35 s for the 32-step scripted run on a 4-core host (excluding the kernel build).
- [x] Wired into the pre-push hook behind `M3OS_TUI_APP_REGRESSION=1`. — see `.githooks/pre-push`.

---

## Track F — Documentation and Release

### F.1 — Post-Phase-57 evaluation closeout

**File:** `docs/research/post-phase-57 evaluation/04-tui-and-neovim-roadmap.md`
**Symbol:** N/A
**Why it matters:** The eval doc that scoped Phase 69 must record what landed where.

**Acceptance:**
- [x] Doc gains a "Closeout" section enumerating which gaps landed in 69 / 69a / 69b / 69c / 69d.
- [x] Neovim, btop, lazygit, fzf, starship, mc, ranger, lf, vim each have a one-line forward-pointer to their respective deferral phase.

### F.2 — Ports + apps appendix

**File:** `docs/appendix/tui-app-port-notes.md` (new)
**Symbol:** N/A
**Why it matters:** Future port authors need a worked-example of "what Phase 69 features each app uses" so they know which `term` capability to verify first.

**Acceptance:**
- [x] Table: app | ncurses variant | terminfo capabilities exercised | termios flags used | UTF-8 blocks consumed | Nerd Font glyphs (if any).
- [x] One paragraph per app summarising what proved tricky during the port.

### F.3 — Create the aligned legacy learning doc

**File:** `docs/69d-tui-app-foundation.md`
**Symbol:** (new document)
**Why it matters:** Learners need a self-contained reference for what it took to bring real-world C TUI apps up on m3OS — the ncurses port flags, the per-app Portfile shape, which Phase 69 / 69a / 69b / 69c capabilities each app exercises, and what 69d deliberately defers (Neovim, btop, lazygit, Go and C++ toolchains) — without conflating it with Phase 45's ports system or Phase 69's terminal-contract foundations. The aligned legacy doc is the canonical companion to the roadmap design doc per `docs/appendix/doc-templates.md`.

**Acceptance:**
- [x] `docs/69d-tui-app-foundation.md` exists with all template fields populated (Aligned Roadmap Phase, Status, Source Ref, Supersedes Legacy Doc — "none").
- [x] Overview paragraph explains what Phase 69 / 69a / 69b / 69c left as terminal-contract foundations and what Phase 69d turns into validated real-app behaviour.
- [x] Key Files table cites every primary surface this phase introduces.
- [x] Closure of Related Phases section cross-refs Phase 31, 44, 45, 69, 69a, 69b, 69c.
- [x] How This Phase Differs From Later TUI Work section calls out the deferred items.
- [x] Related Roadmap Docs links design doc and task doc.

### F.4 — Kernel patch bump to 0.69.4

**Files:**
- `kernel/Cargo.toml`
- `Cargo.lock`
- `AGENTS.md`
- `docs/roadmap/README.md`

**Symbol:** `version`
**Why it matters:** Patch bump per phase; even though 69d is userspace+ports, the project convention still bumps.

**Acceptance:**
- [x] `kernel/Cargo.toml` `version = "0.69.4"`.
- [x] `Cargo.lock` regenerated.
- [x] `AGENTS.md` version cursor updated (kernel version + 69d entry appended to the running phase history).
- [x] `docs/roadmap/README.md` Phase 69d row flipped from Planned → Complete with both milestone and tasks links resolved.
- [x] `cargo xtask check` passes.

---

## Documentation Notes

- 69d is intentionally narrow: three apps, one library. The wider TUI universe (nvim, btop, lazygit, …) lives in dedicated post-69d phases, each with its own toolchain or runtime dependency.
- If an app smoke fails because of a Phase 69 / 69a / 69b / 69c bug, the fix lands as a back-port to the originating phase (with a comment naming the discovery). 69d itself does not change `term` architecture — it ports and validates.
- htop's process-discovery surface is constrained by what m3OS exposes today (`/proc` is partial); a fully-populated process list is not a 69d acceptance criterion. Rendering the chrome correctly is.
- tmux is the harshest test in the set. If it surfaces a kernel TTY/PTY bug under nested-PTY load, the bug fixes route to Phase 22 / 29 follow-ups rather than to 69d.
