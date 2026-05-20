# Phase 71 — GUI Login Manager: Task List

**Status:** Complete
**Source Ref:** phase-71
**Depends on:** Phase 27 (User Accounts) ✅, Phase 48 (Security Foundation / Trust Floor) ✅, Phase 56 (Display and Input Architecture) ✅, Phase 57 (Audio and Local Session) ✅
**Goal:** Replace the autologin-as-root session entry path with a graphical greeter that authenticates against the Phase 27 / Phase 48 password store, renders a configurable background image, and propagates the authenticated UID to a per-user `term` session; retain autologin on serial-only (headless) boots.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | `userspace/greeter/` crate scaffold: workspace, xtask bins, surface creation, solid-color background | None | Complete |
| B | Background image: PNG + BMP decoder, scale-to-fit blitter | A | Complete |
| C | Input handling: username (echoed) + password (silent) field input loop | A | Complete |
| D | Authentication loop: `passwd_lib::verify`, 3-failure / 5 s backoff, session descriptor stdout | C | Complete |
| E | Configuration: `/etc/greeter.conf` parser, built-in defaults | A | Complete |
| F | `session_manager` integration: greeter spawn, stdout capture, UID propagation, `term` exec | D | Complete |
| G | Init manifest update: headless autologin preserved; graphical path routes to greeter | F | Complete |
| H | Documentation: Phase 27, 48, 57 cross-refs; `phase-57-session-entry.md` update | F, G | Complete |
| I | Documentation and Release: aligned legacy learning doc, kernel version bump to 0.71.0 | H | Complete |

---

## Track A — Greeter Crate Scaffold

### A.1 — Add `userspace/greeter/` to workspace and xtask build

**Files:**
- `Cargo.toml` (workspace members)
- `xtask/src/main.rs` (`build_userspace` bins array)
- `userspace/greeter/Cargo.toml`
- `userspace/greeter/src/main.rs`

**Symbol:** `build_userspace` (xtask), `main` (greeter)
**Why it matters:** The greeter must be built by the xtask pipeline and embedded in the initrd before it can run.

**Acceptance:**
- [x] `userspace/greeter/` is a `no_std` Rust crate with `syscall-lib` (alloc feature) and `kernel-core` dependencies.
- [x] `Cargo.toml` workspace `members` includes `userspace/greeter`.
- [x] xtask `bins` array includes `{ name: "greeter", needs_alloc: true }`.
- [x] `kernel/src/fs/ramdisk.rs` includes a `BIN_ENTRIES` entry for `greeter`.
- [x] `cargo xtask check` passes after the scaffold.

### A.2 — Surface creation and solid-color background

**File:** `userspace/greeter/src/main.rs`
**Symbol:** `init_surface`
**Why it matters:** Establishes the Phase 56 surface-buffer protocol connection before any image decode or input handling is added.

**Acceptance:**
- [x] Greeter connects to the display-server socket via the Phase 56 service-lookup path.
- [x] Sends `SurfaceCreate { role: Toplevel, title: "m3OS Login" }` and stores the surface id.
- [x] Sends `BufferCreate { width: display_w, height: display_h, format: BGRA8888 }`.
- [x] Fills the buffer with a solid dark color and sends `Commit` + `DamageBuffer`.
- [x] Greeter process is visible as a full-output window in the compositor.

---

## Track B — Background Image

### B.1 — BMP decoder

**File:** `userspace/greeter/src/image.rs`
**Symbol:** `decode_bmp`
**Why it matters:** BMP is the simpler format and the most likely fallback; implementing it first gives a working image path before PNG decoding is complete.

**Acceptance:**
- [x] `decode_bmp(data: &[u8]) -> Result<(u32, u32, Vec<u32>), ImageError>` decodes BITMAPINFOHEADER BMPs with 24-bit (RGB) and 32-bit (BGRA) color depth.
- [x] No external crate dependency; pure Rust, `no_std` compatible.
- [x] Unit tests cover: a 4×4 24-bit BMP, a 4×4 32-bit BMP, and a truncated BMP that returns `ImageError::Truncated`.
- [x] Output pixel format is BGRA8888 (matches surface buffer).

### B.2 — PNG decoder integration

**File:** `userspace/greeter/src/image.rs`
**Symbol:** `decode_png`
**Why it matters:** PNG is the primary background image format; the user has a background image in PNG they wish to use.

**Acceptance:**
- [x] `decode_png(data: &[u8]) -> Result<(u32, u32, Vec<u32>), ImageError>` decodes baseline PNG (RGBA8 and RGB8 color types; deflate compression).
- [x] Uses a vendored minimal PNG library (e.g., `upng` or a purpose-built decoder) rather than `libpng` FFI, to stay `no_std`.
- [x] Output pixel format is BGRA8888.
- [x] A known-good 32×32 PNG test vector is embedded in the unit tests as `include_bytes!`.

### B.3 — Scale-to-fit blitter

**File:** `userspace/greeter/src/image.rs`
**Symbol:** `blit_scale_to_fit`
**Why it matters:** The background image may not match the display resolution; scale-to-fit with letterboxing ensures the image is always fully visible without stretching.

**Acceptance:**
- [x] `blit_scale_to_fit(src: &[u32], src_w: u32, src_h: u32, dst: &mut [u32], dst_w: u32, dst_h: u32)` scales the source image uniformly (preserving aspect ratio) and centers it in the destination buffer with black letterbox bars.
- [x] Scaling uses nearest-neighbor interpolation (sufficient for a login background).
- [x] Unit test: 320×200 image → 1024×768 buffer; verify letterbox region is zero-filled and scaled image occupies the correct centered rect.

### B.4 — Load and render background from `/etc/greeter/background.{png,bmp}`

**File:** `userspace/greeter/src/main.rs`
**Symbol:** `load_background`
**Why it matters:** Connects the image decoder and blitter to the surface buffer in the greeter's init path.

**Acceptance:**
- [x] `load_background` tries `/etc/greeter/background.png` first, then `/etc/greeter/background.bmp`.
- [x] If neither file is found, fills with a solid dark background (no error, just a fallback).
- [x] On decode error, falls back to solid background and emits a `log::warn!` naming the file and error.
- [x] Decoded image is blitted into the surface buffer via `blit_scale_to_fit` before the first `Commit`.

---

## Track C — Input Handling

### C.1 — Username and password field input loop

**File:** `userspace/greeter/src/input.rs`
**Symbol:** `read_field`
**Why it matters:** The greeter must collect typed username and password from the Phase 56 key-event stream, not from a PTY, because there is no shell running yet.

**Acceptance:**
- [x] `read_field(surface_endpoint, echo: bool) -> String` blocks on `SurfaceInputEvent::Key` messages; appends printable characters to the input buffer; Backspace removes the last character; Enter returns the buffer.
- [x] When `echo = true`, typed characters are rendered in the active text field on screen.
- [x] When `echo = false` (password), nothing is rendered for typed characters (no stars, no indicators).
- [x] Ctrl-C aborts the current field and restarts the auth loop from the username prompt.

### C.2 — On-screen field rendering

**File:** `userspace/greeter/src/render.rs`
**Symbol:** `render_login_ui`
**Why it matters:** The user must see where they are in the login flow (username vs. password field, welcome text, error messages).

**Acceptance:**
- [x] Renders a centered panel over the background with: welcome text (from config), a "Username:" label and echoed input, a "Password:" label (no echo), and an optional error message line.
- [x] Uses the Phase 57 / `kernel-core` bitmap font for glyph rendering.
- [x] Active field is highlighted with the configured `accent-color`.
- [x] Error message (wrong password, backoff countdown) is rendered in a distinct color.
- [x] Surface `Commit` + `DamageBuffer` is called after each keystroke that changes the visual state.

---

## Track D — Authentication Loop

### D.1 — `passwd_lib::verify` integration

**File:** `userspace/greeter/src/auth.rs`
**Symbol:** `attempt_login`
**Why it matters:** The entire purpose of the greeter is to gate session entry on successful authentication; this is the call site.

**Acceptance:**
- [x] `attempt_login(username: &str, password: &str) -> Result<SessionDescriptor, AuthError>` calls `passwd_lib::verify(username, password)`.
- [x] On success, returns `SessionDescriptor { uid, gid, home, shell }` populated from `passwd_lib::lookup_uid(username)`.
- [x] On failure, returns `AuthError::BadPassword`.
- [x] No plaintext password stored or logged at any point.

### D.2 — 3-failure / 5 s backoff (Phase 48 trust-floor)

**File:** `userspace/greeter/src/auth.rs`
**Symbol:** `auth_loop`
**Why it matters:** Without a backoff, brute-force guessing is trivially feasible from the login screen.

**Acceptance:**
- [x] Failure counter resets to 0 on each successful login (if the greeter is reused for fast-user-switch in a later phase).
- [x] After 3 consecutive failures, the greeter renders "Too many attempts. Waiting 5 seconds..." and sleeps 5 s via `sys_nanosleep` before re-prompting.
- [x] Backoff counter resets after each successful login or after the greeter restarts.
- [x] Unit test: `auth_loop` with a mock `verify` that always fails returns backoff error after 3 calls.

### D.3 — Session descriptor stdout and exit 0

**File:** `userspace/greeter/src/main.rs`
**Symbol:** `emit_session_descriptor`
**Why it matters:** `session_manager` reads this line to learn which UID to use when spawning `term`; the format must be machine-parseable.

**Acceptance:**
- [x] On successful auth, greeter writes exactly one line to stdout: `uid=<N> gid=<N> home=<path> shell=<path>\n`.
- [x] Greeter then calls `exit(0)`.
- [x] Unit test: `emit_session_descriptor` with a known `SessionDescriptor` produces the expected line.

---

## Track E — Configuration

### E.1 — `/etc/greeter.conf` parser

**File:** `userspace/greeter/src/config.rs`
**Symbol:** `GreeterConfig`, `load_config`
**Why it matters:** Allows the user to set a background image path and color scheme without recompiling the greeter.

**Acceptance:**
- [x] `load_config() -> GreeterConfig` reads `/etc/greeter.conf` if present; missing file returns built-in defaults silently.
- [x] Parses `key=value` lines; ignores blank lines and lines starting with `#`.
- [x] Recognized keys: `background`, `prompt-color`, `accent-color`, `welcome`.
- [x] Unrecognized keys emit `log::warn!` and are ignored.
- [x] Unit test: parse a config string with all four keys; parse a config with an unrecognized key.

### E.2 — Add `greeter.conf` to xtask ext2 and init KNOWN_CONFIGS

**Files:**
- `xtask/src/main.rs` (`populate_ext2_files`)
- `userspace/init/src/main.rs` (`KNOWN_CONFIGS`)

**Symbol:** `populate_ext2_files`, `KNOWN_CONFIGS`
**Why it matters:** Following the pattern from AGENTS.md: service configs must be registered in both the xtask ext2 builder and the init KNOWN_CONFIGS fallback list.

**Acceptance:**
- [x] `populate_ext2_files` writes a default `greeter.conf` to `/etc/greeter.conf` on the ext2 disk.
- [x] `KNOWN_CONFIGS` in `userspace/init/src/main.rs` includes `"greeter.conf"`.
- [x] `cargo xtask clean && cargo xtask run-gui` produces a disk with `/etc/greeter.conf` present.

---

## Track F — `session_manager` Integration

### F.1 — Boot sequence extended to spawn greeter

**File:** `userspace/session_manager/src/boot.rs`
**Symbol:** `run_boot_sequence`
**Why it matters:** `session_manager` must launch greeter as the last boot step and wait for it to authenticate before spawning `term`.

**Acceptance:**
- [x] Boot sequence: `display_server → kbd_server → mouse_server → audio_server → greeter`.
- [x] `greeter` is spawned with a captured stdout pipe.
- [x] `session_manager` waits (blocking) on greeter's exit code.
- [x] On exit code 0, reads and parses the session descriptor line from the pipe.

### F.2 — UID/GID propagation and per-user `term` exec

**File:** `userspace/session_manager/src/boot.rs`
**Symbol:** `spawn_user_session`
**Why it matters:** Without setuid before exec, `term` and all descendant processes run as root despite the greeter having authenticated a different user.

**Acceptance:**
- [x] After parsing the session descriptor, `session_manager` calls `sys_setuid(uid)` and `sys_setgid(gid)` before exec'ing `term`.
- [x] `term` and its child shell process report the correct UID via `id` and `whoami`.
- [x] `passwd_lib::lookup_uid` validates that the UID from the session descriptor exists in `/etc/passwd` before setuid is called; mismatch returns an error that escalates to `text-fallback`.

### F.3 — Greeter restart policy

**File:** `userspace/session_manager/src/boot.rs`
**Symbol:** `handle_greeter_exit`
**Why it matters:** An unexpected greeter crash (non-zero, non-auth exit) must be handled by the existing `restart=on-failure` supervisor policy, not silently ignored.

**Acceptance:**
- [x] Exit code 0 → parse descriptor, spawn user session.
- [x] Exit code 1 (unexpected failure) → respawn greeter up to 3 times per minute; after 3 failures, escalate to `text-fallback`.
- [x] `session_manager` logs the exit code and restart decision on each non-zero exit.

---

## Track G — Init Manifest Update

### G.1 — Disable autologin-as-root for graphical sessions

**File:** `userspace/init/src/main.rs`
**Symbol:** `start_autologin` (or equivalent)
**Why it matters:** The autologin path must not fire when a display server is running; only headless boots should skip the greeter.

**Acceptance:**
- [x] Init checks for `GRAPHICAL_SESSION=1` in the environment (set by `session_manager` when it starts).
- [x] If `GRAPHICAL_SESSION=1`, the autologin-as-root path is skipped; a log line notes this.
- [x] If `GRAPHICAL_SESSION` is absent (headless boot), autologin-as-root proceeds as before.
- [x] A headless `cargo xtask run` (no `run-gui`) still autologins as root on the serial console.

---

## Track H — Documentation Updates

### H.1 — Update Phase 27, Phase 48, and Phase 57 docs

**Files:**
- `docs/roadmap/27-user-accounts.md`
- `docs/roadmap/48-security-foundation.md`
- `docs/roadmap/57-audio-and-local-session.md`
- `docs/appendix/phase-57-session-entry.md`

**Symbol:** N/A
**Why it matters:** Each of these docs has a section or paragraph that implies root autologin is the current graphical session entry path; they must be updated to reflect Phase 71.

**Acceptance:**
- [x] Phase 27 doc notes that `passwd_lib::verify` and `passwd_lib::lookup_uid` are consumed by the Phase 71 greeter.
- [x] `docs/roadmap/48-security-foundation.md` notes that the trust-floor 3-failure/5 s backoff is implemented in `userspace/greeter/src/auth.rs`.
- [x] Phase 57 doc notes that the autologin-as-root path was superseded by the Phase 71 greeter for graphical sessions; headless autologin is documented as preserved.
- [x] `docs/appendix/phase-57-session-entry.md` updated with the new boot sequence and greeter → session_manager → user-term handoff.

---

## Track I — Documentation and Release

### I.1 — Create the aligned legacy learning doc

**File:** `docs/71-gui-login-manager.md`
**Symbol:** N/A (new learning doc)
**Why it matters:** Learners need a document explaining how the graphical login manager works, why it is a compositor client rather than a kernel service, and how UID propagation connects the greeter's auth decision to the user's `term` session.

**Acceptance:**
- [x] `docs/71-gui-login-manager.md` exists with all template fields populated (`**Aligned Roadmap Phase:** Phase 71`, `**Status:** Planned`, `**Source Ref:** phase-71`, `**Supersedes Legacy Doc:** new`)
- [x] Overview explains in learner-friendly terms why the graphical login manager replaces autologin-as-root and how it fits into the Phase 57 boot sequence
- [x] "What This Doc Covers" list enumerates the greeter binary, background image decoding, auth loop with backoff, session descriptor protocol, UID propagation via setuid, and headless autologin preservation
- [x] "Core Implementation" prose walks through the greeter → session descriptor → session_manager → `term` handoff in plain language, including the trust-floor backoff
- [x] "Key Files" table cites `userspace/greeter/src/main.rs`, `userspace/greeter/src/auth.rs`, `userspace/greeter/src/image.rs`, `userspace/greeter/src/input.rs`, `userspace/session_manager/src/boot.rs`, and `kernel-core/src/passwd/mod.rs`
- [x] "Related Roadmap Docs" links both `docs/roadmap/71-gui-login-manager.md` and `docs/roadmap/tasks/71-gui-login-manager-tasks.md`

### I.2 — Bump kernel version to 0.71.0

**Files:**
- `kernel/Cargo.toml`
- `Cargo.lock`
- `AGENTS.md`
- `docs/roadmap/README.md`

**Symbol:** `version` field in `kernel/Cargo.toml` `[package]`
**Why it matters:** Project convention bumps the kernel minor version by 1 per shipped phase. Phase 70 shipped `v0.70.0`; Phase 71 lifts it to `v0.71.0` so the version cursor in `AGENTS.md` and the roadmap stay accurate.

**Acceptance:**
- [x] `kernel/Cargo.toml` `version = "0.71.0"` (previously `0.70.0`)
- [x] `Cargo.lock` regenerated to reflect the new version
- [x] `AGENTS.md` "Kernel v0.70.0" reference updated to "Kernel v0.71.0"
- [x] `docs/roadmap/README.md` row for Phase 71 updated to reflect Completed status at ship
- [x] `cargo xtask check` passes after the version bump
- [x] Git tag `v0.71.0` recommended at phase merge

---

## Documentation Notes

- The greeter is a `no_std` Rust crate; it must define a `#[global_allocator]` using `syscall_lib::heap::BrkAllocator` and enable the `alloc` feature on `syscall-lib` (per AGENTS.md `needs_alloc` convention).
- PNG and BMP decoding are both implemented in `userspace/greeter/src/image.rs` as `no_std`-compatible pure Rust; no FFI to `libpng` or `libbmp`.
- The session descriptor line format (`uid=N gid=N home=P shell=P`) is machine-parseable by a simple split-on-space parser in `session_manager`; it is not intended as a general-purpose protocol.
- The `GRAPHICAL_SESSION=1` environment variable is set by `session_manager`; init checks it to distinguish headless from graphical boots.
- Lock-screen (re-authentication after idle) is explicitly deferred; Phase 71 delivers login only.
