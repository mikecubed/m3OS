# GUI Login Manager (Phase 71)

**Aligned Roadmap Phase:** Phase 71
**Status:** Complete
**Source Ref:** phase-71
**Supersedes Legacy Doc:** new

## Overview

Phase 71 closes the gap that Phase 57's local-session story left wide
open. Before Phase 71, the graphical boot sequence ended with `term`
running as root, and the only authentication on the m3OS desktop was
the serial-console autologin path. Reaching the GUI was the same as
reaching a root shell — anyone at the keyboard could read every file
under `/etc/shadow`, write to every user's home directory, and `kill`
any process. That was acceptable for the Phase 57 milestone (which
explicitly framed itself as "the minimal local-session proof") but
unacceptable to call m3OS a multi-user system in the graphical path.

After Phase 71, the boot sequence is:

```
display_server → kbd_server → mouse_server → audio_server → greeter → term
```

The new step is `greeter`, a small `display_server` client at
`userspace/greeter/` that paints a centred login panel over a
configurable background image, reads username and password from the
`kbd_server` event stream, verifies them against the existing
`/etc/passwd` + `/etc/shadow` files via the Phase 27 / Phase 48
`syscall_lib::sha256::verify_password` path, and on success
`setgid`s + `setuid`s to the authenticated user and `execve`s
`/bin/term` in-process. Because the `execve` is in the same PID
slot, `term` inherits the dropped credentials and every shell command
under it runs as the logged-in user. `id` and `whoami` report the
correct UID.

Failed authentications re-prompt without exiting. After three
consecutive failures the greeter renders "Too many attempts. Waiting
5 seconds…" and sleeps for five seconds before unlocking the form
again — the Phase 48 trust-floor brute-force defence, now visible to
the GUI user. Headless boots (no `display_server` in the service set)
keep their serial autologin behaviour by default so the existing
smoke regression continues to work; sites that want a "no serial root
access" deployment opt in by writing the marker file
`/etc/m3os-graphical-only`.

## What This Doc Covers

- The `greeter` binary scaffold — `display_server` client, surface
  creation, background image decoder and blitter, on-screen field
  renderer
- The auth loop — `passwd_lib` integration, the 3-failure / 5 s
  backoff state machine, the session-descriptor stdout marker
- UID propagation via `setgid` + `setuid` + `execve(/bin/term)` —
  why the greeter is the gate that turns a root process into a user
  session without needing to fork
- Headless autologin preservation — the `/etc/m3os-graphical-only`
  marker gate in `userspace/init/src/main.rs`
- BMP and PNG decoding in pure Rust — no FFI to libpng, baseline
  deflate inflate implementation good enough for a background image

## Core Implementation

### Why the greeter is a `display_server` client, not a kernel service

The Phase 56 display server is the central authority on every pixel
shown on the framebuffer and every keystroke routed to a surface. The
greeter must read keystrokes and paint a UI; both capabilities are
client-level interactions with the Phase 56 protocol. There is no
kernel-side reason to special-case the greeter — it speaks the same
`ClientMessage::Hello` + `CreateSurface` + `SetSurfaceRole(Toplevel)`
+ `AttachSharedBuffer` + `DamageSurface` + `CommitSurface` verbs that
`term`, `gfx-demo`, and (after Phase 70) DOOM all speak.

This decision is deliberate, not incidental: putting the greeter in
userspace means the kernel still has no notion of "users", "login",
or "session". Authentication policy, brute-force defence, background
image format choice, and the look of the login panel are all
userspace concerns governed by `/etc/greeter.conf` and the
`greeter` binary's own logic. The kernel's only responsibility is
to keep `display_server` running and to enforce the `setuid` syscall
contract that `greeter` calls before `execve`.

### The auth loop, the descriptor, and the in-process hand-off

The Phase 71 task list describes one shape — greeter emits a
session-descriptor line (`uid=N gid=N home=... shell=...`) on stdout
and `session_manager` fork+pipe+parses it, then `setuid`s + execs
`term` itself. The implementation in this repository takes an
observably equivalent simpler shape: the greeter does the descriptor
emit (for log + debug), then drops credentials and `execve`s `term`
in-process. Three reasons:

1. The simpler shape needs no new fork+pipe+wait machinery in
   `session_manager` — every existing F.4 lifecycle invariant
   (`MAX_RETRIES_PER_STEP`, the text-fallback rollback) still
   applies, because `session_manager` continues to observe each step
   via the IPC service registry rather than via direct process
   parenthood.
2. The `setuid`/`setgid` + `execve` chain is identical to what
   `userspace/login/src/main.rs` already does on the serial path —
   we are deliberately reusing the same shape so the security
   contract is single-sourced. Anyone auditing "how does m3OS drop
   privileges before handing the user a shell?" finds the same two
   lines in `login` and `greeter`.
3. The session-descriptor line still goes out on stdout so a future
   per-user observer (or a richer `session_manager` follow-up that
   does want fork+pipe orchestration) can consume it without
   protocol changes.

### The trust-floor backoff

`greeter::auth::AuthLoopState` is a pure-logic state machine —
`record_attempt(result)` returns one of three outcomes:
`Success(SessionDescriptor)`, `Failed(AuthError)`, or
`Backoff { wait_secs, reason }`. Three consecutive failures trigger
`Backoff`; the counter resets to zero after the backoff fires so the
next attempt starts fresh. The pure-logic shape means the contract
is host-testable via `cargo test -p greeter --target
x86_64-unknown-linux-gnu`, and the binary just supplies the wall
clock (a `syscall_lib::nanosleep_for(1, 0)` once per second so the
countdown actually counts down on screen).

### Headless autologin preservation

The serial autologin path in `userspace/init/src/main.rs` is gated
by a new `graphical_only_enabled()` predicate that checks for
`/etc/m3os-graphical-only`. Default boots leave the marker file out
so the existing smoke regression — which logs in over the serial
console — keeps working. The marker is a deliberate opt-in: it
matches the existing pattern set by Phase 56 F.2's
`display_server.debug-crash` marker and Phase 56 close-out G.1's
readback marker. The acceptance bullet in the Phase 71 task list
phrased the gate as `GRAPHICAL_SESSION=1` in init's environment, but
PID 1's env has no convenient hook for a later daemon to mutate; the
marker file is observably equivalent and consistent with every other
"opt in to a non-default boot mode" gate in this repository.

## Key Files

| File | Role |
|---|---|
| `userspace/greeter/src/main.rs` | Binary entry point. Connects to `display_server`, paints the background + form, runs the auth loop, and on success `setuid`s + `execve`s `/bin/term` |
| `userspace/greeter/src/auth.rs` | Pure-logic `AuthLoopState` state machine. 3-failure / 5 s backoff per Phase 48. Host-tested via `cargo test -p greeter` |
| `userspace/greeter/src/image.rs` | BMP and PNG decoders + the scale-to-fit blitter that centres the background image with letterbox bars |
| `userspace/greeter/src/main.rs` (`read_field` + `handle_key`) | Drains `ServerMessage::Key` events from the display-server input channel, distinguishes echo (username) from silent (password), translates printable characters via the keymap, handles Enter / Backspace / Esc / Ctrl-C / Tab |
| `userspace/greeter/src/render.rs` | `render_login_ui` — paints the centred login panel, welcome banner, active-field highlight, and error / backoff message line over whatever the background image (or solid fallback) already drew |
| `userspace/greeter/src/config.rs` | `/etc/greeter.conf` parser. `key=value` lines, `#` comments, four recognised keys (`background`, `prompt-color`, `accent-color`, `welcome`) plus typed `ConfigParseEvent` reports for unknown keys / invalid colours |
| `userspace/greeter/src/session_desc.rs` | Encoder + decoder for the `uid=N gid=N home=P shell=P` stdout line |
| `userspace/session_manager/src/boot.rs` | Bumped `SESSION_STEP_COUNT` from 5 to 6 to thread the new `greeter` step through the F.1 sequencer |
| `userspace/init/src/main.rs` | Owns the autologin dispatch (`graphical_only_enabled()` gate) and the `KNOWN_CONFIGS` fallback list that now includes `/etc/services.d/greeter.conf` |
| `kernel-core/src/passwd/mod.rs` | Unchanged in Phase 71; Phase 71 is a consumer of the existing `passwd_lib::verify` path |
| `kernel-core/src/session_supervisor.rs` | `DECLARED_SESSION_STEP_NAMES` extended to insert `greeter` between `audio_server` and `term` |

## Related Roadmap Docs

- [Phase 71 Design Doc](roadmap/71-gui-login-manager.md)
- [Phase 71 Task List](roadmap/tasks/71-gui-login-manager-tasks.md)
- [Phase 27 (User Accounts)](roadmap/27-user-accounts.md) — the
  `/etc/passwd` + `/etc/shadow` store and `syscall_lib::sha256`
  verification path Phase 71 consumes
- [Phase 48 (Security Foundation)](roadmap/48-security-foundation.md)
  — the trust-floor brute-force-defence requirement Phase 71
  implements in the greeter's auth loop
- [Phase 56 (Display and Input Architecture)](roadmap/56-display-and-input-architecture.md)
  — the surface-buffer + input-event protocol Phase 71 consumes as a
  client
- [Phase 57 (Audio and Local Session)](roadmap/57-audio-and-local-session.md)
  — the boot-sequence orchestrator that Phase 71 extends with the
  greeter step
- [`docs/appendix/phase-57-session-entry.md`](appendix/phase-57-session-entry.md)
  — the ordered startup-step table that Phase 71 added two rows to

## Learning Notes

Three things a future reader should notice about this phase:

1. **The greeter is just a regular Phase 56 client.** No special
   compositor support is needed; no new IPC verb is needed; no
   kernel changes are needed. The only kernel surface this phase
   uses that the existing serial `login` doesn't already use is the
   `setgid` + `setuid` syscall pair, both of which have been there
   since Phase 27. The whole graphical-login feature is roughly 1500
   lines of new userspace code plus a handful of test + scaffolding
   edits, because everything underneath was already correct.
2. **`execve` is a UID-propagation primitive, not just an
   abstraction over "load this binary".** The same line of code
   replaces the greeter's address space with `term`'s — and the
   `setuid` call immediately before survives the `execve` because
   credentials live in the process control block, not in the
   address space. This is the same trick `login` has used since
   Unix V7; it works just as well in m3OS in 2026.
3. **Trust-floor defence belongs in userspace policy, not in the
   kernel.** The 3-failure / 5 s backoff is enforced entirely by the
   `AuthLoopState` state machine running in the greeter's process
   address space. The kernel knows nothing about authentication
   attempts — it just sees a process making `setuid` syscalls. This
   means a malicious binary running in some other process can't
   bypass the backoff by talking directly to `passwd_lib`; the
   `passwd_lib` verify path doesn't enforce backoff itself because
   it doesn't need to. The gate is the binary that owns the login
   form, not the verification primitive.

## How to extend Phase 71

The deliberate deferrals are listed in
`docs/roadmap/71-gui-login-manager.md` § "Deferred Until Later".
Two of them are particularly worth flagging:

- **Lock-screen (re-authentication after idle).** The trust-floor
  state machine in `greeter::auth` is already a `fn record_attempt`
  + reset interface; a lock screen would be the same state machine
  driven from a screensaver process that requested re-authentication
  after an idle timer fired. The protocol-side work is small.
- **Fast user switching.** This is harder. It would require the
  current `term` session to persist (or hibernate) while a second
  greeter cycle ran for a different user. The `setuid` + `execve`
  hand-off used today destroys the previous process; supporting
  multiple concurrent user sessions would require a session manager
  that fork+execs each term and tracks them as siblings.
