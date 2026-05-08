# Phase 71 - GUI Login Manager

**Status:** Planned
**Source Ref:** phase-71
**Depends on:** Phase 27 (User Accounts) ✅, Phase 48 (Password Store / Trust Floor) ✅, Phase 56 (Display and Input Architecture) ✅, Phase 57 (Audio and Local Session) ✅
**Builds on:** Replaces the autologin-as-root path that `session_manager` inherited from Phase 57 with a `display_server`-client greeter that authenticates against the Phase 27 / Phase 48 password store; extends the Phase 57 session boot sequence; extends Phase 56 surface roles for full-output coverage before login; extends Phase 27 `passwd_lib` UID lookup
**Primary Components:** userspace/greeter, userspace/session_manager, userspace/init, kernel-core/passwd, docs/appendix/phase-57-session-entry.md

## Milestone Goal

Booting m3OS reaches a graphical login screen instead of a root shell. The screen
shows a configurable background image and centered username/password fields. A
successful login drops to a `term` session running as the authenticated user, with
`id` and `whoami` reporting the correct UID. Failed authentication re-prompts with a
back-off after three failures. Headless (serial-only) boots retain the existing
autologin path for administration.

Following TDD, the greeter input loop and `passwd_lib` integration are host-tested before QEMU integration — the backoff state machine is pure logic that proptest covers well. Applying DI (SOLID's Dependency Inversion), greeter calls through an `auth_backend` trait so the test harness substitutes a mock verifier; the production path binds `passwd_lib::verify`. Applying SRP, greeter owns authentication and emits a session descriptor, while `session_manager` owns the spawn-under-UID step — neither knows the internals of the other.

## Why This Phase Exists

The Phase 57 session boot sequence was deliberately minimal: `session_manager`
brought up `display_server` → `kbd_server` → `mouse_server` → `audio_server` →
`term`, and `term` opened as root because there was no authentication gate. This was
acceptable for the Phase 57 local-session proof but leaves m3OS in a state where
any user who reaches the boot console has root access to the graphical session.

Phase 27 established the passwd/shadow store and `passwd_lib` for hash verification.
Phase 48 defined the trust-floor (brute-force backoff, attempt counting). This phase
connects those existing building blocks to the graphical session entry point — the
last step needed to treat m3OS as a minimally multi-user system in the graphical
path.

## Learning Goals

- Understand how a display manager ("greeter") fits into a graphical session
  architecture and why it is a regular compositor client rather than a kernel service.
- Learn how UID-dropping (setuid before exec) propagates the authenticated identity
  to all descendant processes.
- See how image decoding (PNG/BMP) integrates with the compositor's surface-buffer
  protocol.
- Understand the trust-floor concept — backoff after repeated auth failures — and
  why it belongs in userspace policy, not in the kernel.
- Learn how a boot sequence branches between graphical and headless modes.

## Feature Scope

### Greeter binary (Track A)

`userspace/greeter/` is a new Rust crate. It is a regular `display_server` client
that creates a `Toplevel` surface covering the full output. It renders a background
image (Track B) and centered username + password fields using the Phase 56
surface-buffer protocol. It reads username (echoed) and password (not echoed, each
keystroke received via `SurfaceInputEvent::Key`) via the Phase 56 input path. On
each authentication attempt it calls into `passwd_lib` for hash verification.

On success: greeter prints a compact session descriptor to stdout (`uid=N gid=N
home=/home/user shell=/bin/sh`) and exits with code 0. `session_manager` reads the
descriptor and spawns `term` under the authenticated UID/GID.

On failure: re-prompts with an error message. After three consecutive failures, a
5 s backoff is enforced (Phase 48 trust-floor). The greeter never exits to a root
shell; it cycles back to the login prompt after backoff.

### Background image rendering (Track B)

The greeter loads `/etc/greeter/background.png` (or `.bmp` if `.png` is absent).
PNG decoding uses either a vendored minimal decoder or a port of `minipng`/`upng`;
BMP decoding uses a small baseline BMP reader (BITMAPINFOHEADER, 24/32-bit, no
compression). The decoded image is scaled to the output resolution (scale-to-fit,
letterboxed) and blitted into the surface buffer behind the login fields. If no
background file is found, a solid dark background is used.

### `session_manager` integration (Track C)

The Phase 57 boot sequence is extended: `display_server → kbd_server → mouse_server
→ audio_server → greeter`. `session_manager` spawns `greeter` as the last step
instead of `term`. It waits for `greeter` to exit with code 0 and reads the session
descriptor from `greeter`'s stdout. It then spawns `term` under the authenticated
UID/GID from the descriptor.

If `greeter` exits with a non-zero code (unexpected failure, not an auth failure),
`session_manager` applies the existing `restart=on-failure` policy and respawns the
greeter. The `text-fallback` escalation path from Phase 57 is preserved for repeated
greeter crashes.

### Per-user session UID propagation (Track D)

`session_manager` reads `uid` and `gid` from the greeter's session descriptor. It
calls `setuid(uid)` and `setgid(gid)` before exec'ing `term` (and any other
per-user session programs). `passwd_lib`'s UID lookup (Phase 27) is reused to
validate that the UID exists in `/etc/passwd`. After `setuid`, the process and all
its children run as the authenticated user.

### Configuration (Track E)

`/etc/greeter.conf` is an optional flat key-value file with keys:
- `background=<path>` — path to background image (default `/etc/greeter/background.png`)
- `prompt-color=<rrggbb>` — hex color for prompt text (default `ffffff`)
- `accent-color=<rrggbb>` — hex color for field highlight (default `4488cc`)
- `welcome=<text>` — welcome message above the username field (default `m3OS Login`)

Unrecognized keys are ignored; missing keys use built-in defaults.

### Init manifest update (Track F)

The autologin-as-root path in `userspace/init/src/main.rs` is disabled for
graphical sessions. For serial-console (headless) boots — detected by the absence
of `display_server` in the service registry — autologin as root continues to work
for administration access. The dispatch is on a `GRAPHICAL_SESSION=1` environment
variable set by `session_manager`; init checks this before choosing the autologin
path.

### Validation (Track G)

Boot reaches the greeter screen. Login as a Phase 27 user (`mikecubed` or similar)
succeeds and opens `term` as that user; `id` and `whoami` report the correct UID.
Incorrect password shows an error and re-prompts. After three failures, 5 s backoff
fires before re-prompting. Background image renders at 1024×768 and at mismatched
output sizes (scale-to-fit verified). Headless boot still autologins as root.

### Documentation updates (Track H)

Phase 27, Phase 48, and Phase 57 design docs updated: Phase 27 notes that
`passwd_lib` UID lookup is now used by the graphical session entry path; Phase 48
notes that the trust-floor back-off is implemented in the greeter; Phase 57 notes
that the autologin-as-root path is now serial-only.

## Important Components and How They Work

### `userspace/greeter/src/main.rs`

Entry point. Connects to display-server socket, creates a full-output `Toplevel`
surface, loads configuration, loads and blits the background image, then enters the
auth loop: render prompt, read username, render password prompt, read password,
call `passwd_lib::verify(username, password)`, handle result. On success, writes the
session descriptor to stdout and exits 0.

### `userspace/greeter/src/image.rs`

Image decoder for PNG and BMP. Calls a minimal PNG library or a bespoke BMP reader;
produces a `Vec<u32>` of BGRA8888 pixels at the decoded dimensions. The scale-to-fit
blitter computes letterbox offsets and copies the scaled image into the surface
buffer at the correct position.

### `userspace/greeter/src/input.rs`

Reads `SurfaceInputEvent::Key` messages from the display-server surface endpoint.
Maintains echo state (username field: echo; password field: no echo). Translates
keycodes to characters using the Phase 56 key-event encoding. Backspace removes the
last character from the input buffer. Enter submits the field.

### `userspace/session_manager/src/boot.rs`

Extended boot sequence. After `audio_server` is confirmed running, spawns `greeter`
with its stdout captured on a pipe. On greeter exit code 0, reads and parses the
session descriptor line from the pipe. Calls `setuid(uid)` / `setgid(gid)` and
exec's `term`.

### `kernel-core/src/passwd/mod.rs`

`passwd_lib::verify(username: &str, password: &str) -> Result<Uid, AuthError>`
is the existing Phase 27 / Phase 48 interface. Phase 71 is a consumer, not a
modifier; the verify function is unchanged.

## How This Builds on Earlier Phases

- Extends the Phase 57 session boot sequence by inserting `greeter` between
  `audio_server` and `term`.
- Consumes Phase 27 `passwd_lib` for user lookup and Phase 48's trust-floor logic
  for the 3-failure / 5 s backoff, without modifying either.
- Reuses Phase 56 `SurfaceCreate`, `BufferCreate`, `Commit`, `DamageBuffer`, and
  `SurfaceInputEvent::Key` — greeter is a standard Phase 56 client.
- Extends Phase 57 `session_manager` with a UID-propagation path (setuid before
  exec) and the session-descriptor protocol between greeter and session_manager.

## Implementation Outline

1. Scaffold `userspace/greeter/` Rust crate; add to workspace and xtask bins list.
2. Implement greeter surface creation and background blit (solid color first, then
   image decode).
3. Implement input handling: username field (echoed), password field (silent).
4. Integrate `passwd_lib::verify`; implement the 3-failure / 5 s backoff loop.
5. Implement session descriptor stdout write and exit-0 path.
6. Extend `session_manager` boot sequence to spawn greeter, capture its stdout,
   parse the session descriptor, and exec `term` under the authenticated UID/GID.
7. Implement PNG and BMP image decoders in `greeter/src/image.rs`.
8. Implement scale-to-fit blitter.
9. Add `/etc/greeter.conf` parsing with defaults.
10. Update `userspace/init/` autologin dispatch for headless vs. graphical mode.
11. Add `greeter.conf` to xtask ext2 population and `init` KNOWN_CONFIGS fallback.
12. Validation: boot → greeter → auth success → `term` as user; auth failure loop;
    headless autologin.
13. Update Phase 27, Phase 48, Phase 57 docs.

## Acceptance Criteria

- Boot reaches the greeter screen with the configured background image rendered.
- Login as `mikecubed` (Phase 27 passwd) drops to a `term` session; `id` reports the
  correct UID and `whoami` reports `mikecubed`.
- Incorrect password shows an error message and re-prompts; the password field is
  always silent (no echo of typed characters).
- After three consecutive failures, a 5 s backoff delay is enforced before
  re-prompting.
- Background image renders correctly at 1024×768; scale-to-fit does not stretch or
  crop at mismatched resolutions.
- A headless boot (no display_server) still autologins as root on the serial console.

## Companion Task List

- [Phase 71 Task List](./tasks/71-gui-login-manager-tasks.md)

## How Real OS Implementations Differ

- Linux uses PAM (Pluggable Authentication Modules) for authentication; m3OS calls
  `passwd_lib` directly because PAM's dynamic library loading requires a full libc
  and dlfcn infrastructure.
- GNOME/KDE display managers (GDM, SDDM) support session type selection (Wayland,
  X11, custom); m3OS supports a single session type.
- Real display managers implement auto-lock, idle timeout, and switch-user; Phase 71
  ships login only and defers lock-screen to a later phase.
- Real greetors implement accessibility features (screen reader, high-contrast
  mode); Phase 71 defers those.
- Linux's `logind` manages seat state and VT switching; m3OS handles session
  transition directly in `session_manager`.

## Deferred Until Later

- Lock-screen (re-authentication after idle timeout) — explicitly post-Phase 71
- Session type selection (graphical vs. alternate shell)
- Fast user switching between concurrent sessions
- PAM-equivalent pluggable authentication
- Accessibility features (high-contrast, screen reader)
- Greeter animation or theme packs
- Kiosk / single-application session mode launched directly from greeter
