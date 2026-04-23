---
current-task: "Debug framebuffer login keyboard regression in normal mode"
current-phase: "fix-complete"
next-action: "done"
workspace: "feat/phase-55c-ring3-driver-closure"
last-updated: "2026-04-23T16:18:18Z"
---

## Decisions

- Symptom: keyboard input never works for the normal framebuffer/login/shell path in QEMU GUI, while the serial console still accepts input and DOOM receives keys normally once launched from the serial console.
- Confirmed architecture split: DOOM uses the raw framebuffer + `sys_read_scancode()` path directly, while normal login/shell input depends on the userspace `kbd_server` → `stdin_feeder` → `push_raw_input()` bridge.
- Root cause: `userspace/stdin_feeder` retried `ipc_lookup_service("kbd")` with `syscall_lib::nanosleep(20_000_000)` under the assumption that the argument was nanoseconds, but `syscall_lib::nanosleep()` interprets its argument as whole seconds. When `stdin_feeder` raced ahead of `kbd_server` registration on boot, it slept for ~20 million seconds after the first miss, leaving the normal framebuffer input path effectively dead.
- Fix applied: added `syscall_lib::nanosleep_for(seconds, nanoseconds)` and switched `stdin_feeder`'s startup retry loop to `nanosleep_for(0, 20_000_000)` so the intended 20 ms backoff is real.
- Validation outcome: `cargo xtask check` passed after the change. A follow-up `cargo xtask smoke-test --timeout 60` did not validate guest behaviour because the host-side run aborted before boot with the pre-existing ext2 tool error `Bad option(s) specified: revision`.

## Files Touched

- .agent/SESSION.md
- userspace/stdin_feeder/src/main.rs
- userspace/syscall-lib/src/lib.rs

## Open Questions

- Interactive confirmation in QEMU GUI is still needed to prove the framebuffer login path now receives keys end-to-end.

## Blockers

- Host-side smoke boot in this environment is currently blocked by the ext2 image tooling error `Bad option(s) specified: revision`, so it cannot be used here to validate the GUI keyboard fix.

## Failed Hypotheses

- **DO-NOT-RETRY:** "The remaining regression is specifically a post-DOOM framebuffer ownership handoff bug." Evidence: the clarified user report says normal framebuffer login input never works even before DOOM starts; DOOM only demonstrates that the raw scancode path still works.
- **DO-NOT-RETRY:** "Foreground process-group (`FG_PGID`) routing is the main cause." Evidence: the shell currently does not implement job-control handoff (`setpgid`/`tcsetpgrp`) for DOOM launches, and that theory does not explain why framebuffer login input is dead before any shell command runs.
- **DO-NOT-RETRY:** "The raw/TTY scancode router itself is latching the wrong sink after DOOM exits." Evidence: `kernel_core::input::ScancodeRouter` resets correctly and DOOM already proves raw keyboard delivery works; the actual always-broken path is the userspace TTY bridge.
